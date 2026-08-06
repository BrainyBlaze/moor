use super::{Done, Purpose, StorageError};
use crate::store::{Commit, Store, StoreError, StoreStep};
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::sync::{
    Arc, Mutex, TryLockError,
    atomic::{AtomicU8, Ordering},
    mpsc::{self, Sender},
};
use std::time::{Duration, Instant};

type Outcome = Result<(Commit, bool), StoreError>;
type Job = (Purpose, Instant, usize);
type Submitted = (Work, Instant);
schema!(enum pub(crate) Frontier; Ready(Option<Commit>), Busy, Failed);
const BODY: u8 = 1;
const DIRTY: u8 = 2;
const COMMIT: u8 = 3;
const HELD: u8 = 4;
const PHASE: u8 = 7;
const CLOSED: u8 = 8;
const PUBLISHING: u8 = 16;

struct State {
    bits: AtomicU8,
    frontier: Mutex<Option<Commit>>,
    completed: Mutex<VecDeque<Outcome>>,
}

impl State {
    fn deadline(&self, deadline: Instant) -> Result<(), StoreError> {
        return_if!(Instant::now() < deadline, Ok(()));
        self.close();
        Err(failure(ErrorKind::TimedOut))
    }

    fn begin_body(&self, deadline: Instant) -> Result<(), StoreError> {
        loop {
            self.deadline(deadline)?;
            match self.change(0, BODY) {
                Ok(_) => return self.deadline(deadline),
                Err(HELD) => std::thread::yield_now(),
                Err(state) if state & CLOSED != 0 => {
                    return Err(failure(ErrorKind::WouldBlock));
                }
                Err(_) => unreachable!("one worker owns each lane"),
            }
        }
    }

    fn io(&self, deadline: Instant, step: StoreStep) -> Result<(), StoreError> {
        self.deadline(deadline)?;
        let state = self.bits.load(Ordering::Acquire);
        let (from, to) = match (step, state) {
            (StoreStep::Body, BODY) | (StoreStep::Commit, DIRTY) => return Ok(()),
            (StoreStep::Commit, BODY) => (BODY, DIRTY),
            (StoreStep::Flush, DIRTY) => (DIRTY, COMMIT),
            _ => return Err(failure(ErrorKind::WouldBlock)),
        };
        self.change(from, to)
            .map(|_| ())
            .map_err(|_| failure(ErrorKind::WouldBlock))
    }

    fn change(&self, from: u8, to: u8) -> Result<u8, u8> {
        self.bits
            .compare_exchange(from, to, Ordering::AcqRel, Ordering::Acquire)
    }

    fn claim(&self, blocked: u8, set: u8) -> bool {
        self.bits
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & blocked == 0).then_some(state | set)
            })
            .is_ok()
    }

    fn finish(&self, failed: bool, result: Option<Outcome>) {
        if failed {
            self.close();
        }
        let mut completed = self.completed.lock().expect("completion lock");
        self.bits.fetch_and(CLOSED, Ordering::AcqRel);
        if let Some(result) = result {
            completed.push_back(result);
        }
    }

    fn close(&self) {
        self.bits.fetch_or(CLOSED, Ordering::AcqRel);
    }

    fn closed(&self) -> bool {
        self.bits.load(Ordering::Acquire) & CLOSED != 0
    }
}

fn failure(kind: ErrorKind) -> StoreError {
    StoreError::Io(kind.into())
}
schema!(enum pub(crate) Work; Append(Arc<[u8]>, u64, u64), Replace(Vec<u8>, u32, u64, u64), Clear(u64, u64), #[cfg(test)] Test(TestWork));

impl Work {
    fn size(&self) -> usize {
        match self {
            Self::Append(bytes, ..) => bytes.len(),
            Self::Replace(bytes, ..) => bytes.len(),
            #[cfg(test)]
            Self::Test(work) => work.size(),
            _ => 0,
        }
    }
    fn run(self, store: &mut Store, state: &State, deadline: Instant) -> Outcome {
        #[cfg(test)]
        let operation = self.prepare_test();
        #[cfg(not(test))]
        let operation = self;
        state.begin_body(deadline)?;
        match operation {
            Self::Append(bytes, cap, end) => {
                store.append_capped_with(&bytes, cap, end, |step| state.io(deadline, step))
            }
            Self::Replace(bytes, epoch, start, end) => {
                store.replace_with(&bytes, epoch, start, end, |step| state.io(deadline, step))
            }
            Self::Clear(observed, end) => {
                let selected = *store.selected();
                if selected.index != observed || selected.length == 0 {
                    return Ok((selected, selected.index != observed));
                }
                let epoch = selected.epoch.checked_add(1).ok_or(StoreError::Exhausted)?;
                store.replace_with(&[], epoch, end, end, |step| state.io(deadline, step))
            }
            #[cfg(test)]
            Self::Test(work) => return work.run(store, state, deadline),
        }
        .map(|commit| (*commit, false))
    }
}

schema!(struct pub(crate) Lane fields; submit: Sender<Submitted>, limits: (usize, usize), pending: VecDeque<Job>,
    bytes: usize, failure: Option<ErrorKind>, state: Arc<State>);

impl Lane {
    pub(crate) fn new(mut store: Store, jobs: usize, bytes: usize) -> Self {
        // The worker publishes its selected commit before acknowledging the
        // job, so a descriptor built between the commit and the consumption of
        // its completion still names the commit a reader would select (§5). The
        // alternative — re-opening the store by pathname — would reintroduce
        // §11.4's check/use substitution window on every STATUS.
        let (submit, work) = mpsc::channel::<Submitted>();
        let state = Arc::new(State {
            bits: AtomicU8::new(0),
            frontier: Mutex::new(Some(*store.selected())),
            completed: Mutex::new(VecDeque::new()),
        });
        let worker_state = Arc::clone(&state);
        std::thread::spawn(move || {
            for (operation, deadline) in work {
                if worker_state.closed() {
                    break;
                }
                let result = operation.run(&mut store, &worker_state, deadline);
                // Successful store operations return the selected validated
                // commit, so publishing it directly avoids a full read/hash of
                // the store after every append. Only an ambiguous commit-phase
                // error needs recovery through the already validated handles.
                let phase = worker_state.bits.load(Ordering::Acquire) & PHASE;
                let selected = match (&result, phase) {
                    (Ok((commit, _)), _) => Some(*commit),
                    // None is published as an explicit unknown frontier; the
                    // holder then refuses STATUS/ATTACH instead of silently
                    // replaying stale coordinates.
                    (Err(_), COMMIT) => store.selected_result().ok(),
                    (Err(_), DIRTY) => None,
                    (Err(_), _) => Some(*store.selected()),
                };
                let publishing =
                    worker_state.claim(CLOSED, PUBLISHING) && Instant::now() < deadline;
                let result_failed = result.is_err();
                {
                    let mut frontier = worker_state.frontier.lock().expect("frontier lock");
                    // Unknown is absorbing; otherwise publication is monotonic.
                    *frontier = (*frontier)
                        .zip(selected)
                        .map(|(old, new)| if old.index < new.index { new } else { old });
                }
                let abandoned = worker_state.closed();
                let failed = result_failed || !publishing || abandoned;
                // Make completion delivery a phase barrier: once a controller
                // observes `next()`, the lane is already snapshottable (or
                // terminally closed on failure).
                worker_state.finish(failed, (publishing && !abandoned).then_some(result));
                if failed || worker_state.closed() {
                    break;
                }
            }
        });
        Self {
            submit,
            limits: (jobs, bytes),
            pending: VecDeque::new(),
            bytes: 0,
            failure: None,
            state,
        }
    }

    pub(crate) fn submit(&mut self, purpose: Purpose, operation: Work) -> Result<(), StorageError> {
        return_if!(!self.writable(), Err(StorageError::Disabled));
        let size = operation.size();
        let Some(bytes) = self
            .bytes
            .checked_add(size)
            .filter(|total| *total <= self.limits.1 && self.pending.len() < self.limits.0)
        else {
            if matches!(
                purpose,
                Purpose::Background
                    | Purpose::Lifecycle
                    | Purpose::Final
                    | Purpose::Sources(_, true)
            ) {
                self.close();
                return Err(StorageError::Disabled);
            }
            return Err(StorageError::Busy);
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        if self.submit.send((operation, deadline)).is_err() {
            self.fail(ErrorKind::BrokenPipe);
            return Err(StorageError::Disabled);
        }
        self.bytes = bytes;
        self.pending.push_back((purpose, deadline, size));
        Ok(())
    }

    pub(crate) fn try_complete(&mut self, now: Instant) -> Option<Done> {
        let deadline = self.pending.front()?.1;
        let result = if let Some(kind) = self.failure {
            Err(failure(kind))
        } else {
            let state = Arc::clone(&self.state);
            match state.completed.try_lock() {
                Ok(mut completed) => match completed.pop_front() {
                    Some(result) => result,
                    None if now < deadline || !self.state.claim(PUBLISHING, CLOSED) => return None,
                    None => Err(self.fail(ErrorKind::TimedOut)),
                },
                Err(TryLockError::WouldBlock) => return None,
                Err(TryLockError::Poisoned(_)) => Err(self.fail(ErrorKind::BrokenPipe)),
            }
        };
        if result.is_err() && self.failure.is_none() {
            self.fail(ErrorKind::BrokenPipe);
        }
        let (purpose, _, size) = self.pending.pop_front()?;
        self.bytes = self.bytes.saturating_sub(size);
        Some(Done {
            lane: 0,
            purpose,
            result,
        })
    }

    pub(crate) fn hold(&self) -> bool {
        let state = self.state.bits.load(Ordering::Acquire);
        state & PHASE == 0 && self.state.change(state, state | HELD).is_ok()
    }
    pub(crate) fn release(&self) {
        self.state.bits.fetch_and(!PHASE, Ordering::Release);
    }
    pub(crate) fn snapshot(&self) -> Frontier {
        match self.state.frontier.try_lock() {
            Ok(frontier) => Frontier::Ready(*frontier),
            Err(TryLockError::WouldBlock) => Frontier::Busy,
            Err(TryLockError::Poisoned(_)) => Frontier::Failed,
        }
    }
    pub(crate) fn writable(&self) -> bool {
        self.failure.is_none() && !self.state.closed()
    }
    pub(crate) fn pending(&self) -> usize {
        self.pending.len()
    }
    pub(crate) fn pending_matches(&self, predicate: impl Fn(Purpose) -> bool) -> bool {
        self.pending.iter().any(|job| predicate(job.0))
    }
    pub(crate) fn close(&mut self) {
        self.fail(ErrorKind::WouldBlock);
    }
    fn fail(&mut self, kind: ErrorKind) -> StoreError {
        self.failure.get_or_insert(kind);
        self.state.close();
        failure(kind)
    }
}

#[cfg(test)]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/unit/runtime_worker.rs"
));
