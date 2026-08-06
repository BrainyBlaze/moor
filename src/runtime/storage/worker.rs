use super::{Done, Purpose, StorageError};
use crate::store::{Commit, Store, StoreError, StoreStep};
use std::collections::VecDeque;
use std::io::ErrorKind;
#[cfg(test)]
use std::sync::mpsc::Receiver;
use std::sync::{
    Arc, Mutex, TryLockError,
    atomic::{AtomicU8, Ordering},
    mpsc::{self, Sender},
};
use std::time::{Duration, Instant};

type Outcome = Result<(Commit, bool), StoreError>;
type Completed = (Instant, Outcome);
type Job = (Purpose, Instant, usize);
type Submitted = (Work, Instant);
struct Published {
    frontier: Option<Commit>,
    completed: VecDeque<Completed>,
}
pub(crate) enum Frontier {
    Ready(Option<Commit>),
    Busy,
    Failed,
}
const BODY: u8 = 1;
const DIRTY: u8 = 2;
const COMMIT: u8 = 3;
const HELD: u8 = 4;
const PHASE: u8 = 7;
const CLOSED: u8 = 8;
const PUBLISHING: u8 = 16;

struct State {
    bits: AtomicU8,
    #[cfg(test)]
    publication_gate: Mutex<Option<(Sender<()>, Receiver<()>)>>,
}

impl State {
    fn deadline(&self, deadline: Instant) -> Result<(), StoreError> {
        if Instant::now() < deadline {
            Ok(())
        } else {
            self.close();
            Err(StoreError::Io(ErrorKind::TimedOut.into()))
        }
    }

    fn begin_body(&self, deadline: Instant) -> Result<(), StoreError> {
        loop {
            self.deadline(deadline)?;
            let state = self.bits.load(Ordering::Acquire);
            if state & CLOSED != 0 {
                return Err(StoreError::Io(ErrorKind::WouldBlock.into()));
            }
            match state & PHASE {
                0 => {
                    if self
                        .bits
                        .compare_exchange(state, BODY, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        self.deadline(deadline)?;
                        return Ok(());
                    }
                }
                HELD => std::thread::yield_now(),
                _ => unreachable!("one worker owns each lane"),
            }
        }
    }

    fn io(&self, deadline: Instant, step: StoreStep) -> Result<(), StoreError> {
        self.deadline(deadline)?;
        match step {
            StoreStep::Body => self.require(BODY),
            StoreStep::Commit => {
                let state = self.bits.load(Ordering::Acquire);
                if state & CLOSED != 0 {
                    return Err(StoreError::Io(ErrorKind::WouldBlock.into()));
                }
                match state & PHASE {
                    BODY => self
                        .bits
                        .compare_exchange(state, DIRTY, Ordering::AcqRel, Ordering::Acquire)
                        .map(|_| ())
                        .map_err(|_| StoreError::Io(ErrorKind::WouldBlock.into())),
                    DIRTY => Ok(()),
                    _ => Err(StoreError::Io(ErrorKind::WouldBlock.into())),
                }
            }
            StoreStep::Flush => self
                .bits
                .compare_exchange(DIRTY, COMMIT, Ordering::AcqRel, Ordering::Acquire)
                .map(|_| ())
                .map_err(|_| StoreError::Io(ErrorKind::WouldBlock.into())),
        }
    }

    fn require(&self, phase: u8) -> Result<(), StoreError> {
        let state = self.bits.load(Ordering::Acquire);
        if state & CLOSED == 0 && state & PHASE == phase {
            Ok(())
        } else {
            Err(StoreError::Io(ErrorKind::WouldBlock.into()))
        }
    }

    fn publish(&self) -> bool {
        let mut state = self.bits.load(Ordering::Acquire);
        loop {
            if state & CLOSED != 0 {
                return false;
            }
            match self.bits.compare_exchange(
                state,
                state | PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => state = actual,
            }
        }
    }

    fn expire(&self) -> bool {
        let mut state = self.bits.load(Ordering::Acquire);
        loop {
            if state & PUBLISHING != 0 {
                return false;
            }
            if state & CLOSED != 0 {
                return true;
            }
            match self.bits.compare_exchange(
                state,
                state | CLOSED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => state = actual,
            }
        }
    }

    fn finish(&self, failed: bool) {
        let mut state = self.bits.load(Ordering::Acquire);
        loop {
            let next = (state & CLOSED) | (u8::from(failed) * CLOSED);
            match self
                .bits
                .compare_exchange(state, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(actual) => state = actual,
            }
        }
    }

    fn close(&self) {
        self.bits.fetch_or(CLOSED, Ordering::AcqRel);
    }

    fn closed(&self) -> bool {
        self.bits.load(Ordering::Acquire) & CLOSED != 0
    }

    fn commit_issued(&self) -> bool {
        self.bits.load(Ordering::Acquire) & PHASE == COMMIT
    }

    fn commit_dirty(&self) -> bool {
        self.bits.load(Ordering::Acquire) & PHASE == DIRTY
    }

    fn hold(&self) -> bool {
        let state = self.bits.load(Ordering::Acquire);
        state & PHASE == 0
            && self
                .bits
                .compare_exchange(state, state | HELD, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    fn release(&self) {
        self.bits.fetch_and(!PHASE, Ordering::Release);
    }

    #[cfg(test)]
    fn pause_publication(&self) {
        let gate = self
            .publication_gate
            .lock()
            .expect("publication gate")
            .take();
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            let _ = release.recv();
        }
    }
}
schema!(enum pub(crate) Work; Append(Arc<[u8]>, u64, u64), Replace(Vec<u8>, u32, u64, u64), Clear(u64, u64), #[cfg(test)] #[allow(dead_code)] Hold(Receiver<()>),
    #[cfg(test)] Staged(Vec<u8>, u32, u64, u64, Option<(Sender<()>, Receiver<()>)>, bool),
    #[cfg(test)] #[allow(dead_code)] Phased(Vec<u8>, u32, u64, u64, u8, Sender<()>, Receiver<()>, bool),
    #[cfg(test)] #[allow(dead_code)] AppendPhased(Vec<u8>, u64, u64, u8, Sender<()>, Receiver<()>),
    #[cfg(test)] #[allow(dead_code)] ClearPhased(u64, u64, u8, Sender<()>, Receiver<()>),
    #[cfg(test)] #[allow(dead_code)] Recover(Vec<u8>, u32, u64, u64, Sender<()>, Receiver<()>));

#[cfg(test)]
fn phased_io(
    state: &State,
    deadline: Instant,
    stage: u8,
    step: StoreStep,
    entered: &Sender<()>,
    gate: &Receiver<()>,
    announced: &mut bool,
) -> Result<(), StoreError> {
    if matches!(
        (stage, step),
        (1, StoreStep::Commit) | (3, StoreStep::Flush)
    ) && !*announced
    {
        *announced = true;
        let _ = entered.send(());
        let _ = gate.recv();
    }
    state.io(deadline, step)?;
    if stage == 2 && step == StoreStep::Flush && !*announced {
        *announced = true;
        let _ = entered.send(());
        let _ = gate.recv();
    }
    Ok(())
}

impl Work {
    fn size(&self) -> usize {
        match self {
            Self::Append(bytes, ..) => bytes.len(),
            Self::Replace(bytes, ..) => bytes.len(),
            #[cfg(test)]
            Self::Recover(bytes, ..) | Self::AppendPhased(bytes, ..) => bytes.len(),
            _ => 0,
        }
    }
    fn run(self, store: &mut Store, state: &State, deadline: Instant) -> Outcome {
        #[cfg(test)]
        let operation = match self {
            Self::Staged(bytes, epoch, start, end, Some((entered, gate)), fail) => {
                let _ = entered.send(());
                let _ = gate.recv();
                Self::Staged(bytes, epoch, start, end, None, fail)
            }
            operation => operation,
        };
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
            // Commits, then either blocks past the caller's deadline or reports
            // an error, so the two ambiguous-durability paths become testable:
            // a commit that lands after its lane is quarantined, and a valid
            // committed candidate followed by a reported failure.
            #[cfg(test)]
            Self::Staged(bytes, epoch, start, end, _, fail) => {
                let commit = *store
                    .replace_with(&bytes, epoch, start, end, |step| state.io(deadline, step))?;
                return if fail {
                    Err(StoreError::Corrupt)
                } else {
                    Ok((commit, false))
                };
            }
            #[cfg(test)]
            Self::Phased(bytes, epoch, start, end, stage, entered, gate, fail) => {
                let mut announced = false;
                let commit = *store.replace_with(&bytes, epoch, start, end, |step| {
                    phased_io(
                        state,
                        deadline,
                        stage,
                        step,
                        &entered,
                        &gate,
                        &mut announced,
                    )
                })?;
                return if fail {
                    Err(StoreError::Corrupt)
                } else {
                    Ok((commit, false))
                };
            }
            #[cfg(test)]
            Self::AppendPhased(bytes, cap, end, stage, entered, gate) => {
                let mut announced = false;
                store.append_capped_with(&bytes, cap, end, |step| {
                    phased_io(
                        state,
                        deadline,
                        stage,
                        step,
                        &entered,
                        &gate,
                        &mut announced,
                    )
                })
            }
            #[cfg(test)]
            Self::ClearPhased(observed, end, stage, entered, gate) => {
                let selected = *store.selected();
                if selected.index != observed || selected.length == 0 {
                    return Ok((selected, selected.index != observed));
                }
                let epoch = selected.epoch.checked_add(1).ok_or(StoreError::Exhausted)?;
                let mut announced = false;
                store.replace_with(&[], epoch, end, end, |step| {
                    phased_io(
                        state,
                        deadline,
                        stage,
                        step,
                        &entered,
                        &gate,
                        &mut announced,
                    )
                })
            }
            #[cfg(test)]
            Self::Recover(bytes, epoch, start, end, entered, gate) => {
                store.replace_with(&bytes, epoch, start, end, |step| state.io(deadline, step))?;
                let _ = entered.send(());
                let _ = gate.recv();
                return Err(StoreError::Corrupt);
            }
            #[cfg(test)]
            Self::Hold(wait) => {
                let _ = wait.recv();
                return Ok((*store.selected(), false));
            }
        }
        .map(|commit| (*commit, false))
    }
}

schema!(struct pub(crate) Lane fields; submit: Sender<Submitted>, limits: (usize, usize), pending: VecDeque<Job>,
    bytes: usize, failure: Option<ErrorKind>, state: Arc<State>, published: Arc<Mutex<Published>>);

impl Lane {
    pub(crate) fn new(mut store: Store, jobs: usize, bytes: usize) -> Self {
        // The worker publishes its selected commit before acknowledging the
        // job, so a descriptor built between the commit and the consumption of
        // its completion still names the commit a reader would select (§5). The
        // alternative — re-opening the store by pathname — would reintroduce
        // §11.4's check/use substitution window on every STATUS.
        let published = Arc::new(Mutex::new(Published {
            frontier: Some(*store.selected()),
            completed: VecDeque::new(),
        }));
        let worker_published = Arc::clone(&published);
        let (submit, work) = mpsc::channel::<Submitted>();
        let state = Arc::new(State {
            bits: AtomicU8::new(0),
            #[cfg(test)]
            publication_gate: Mutex::new(None),
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
                let selected = match &result {
                    Ok((commit, _)) => Some(*commit),
                    // None is published as an explicit unknown frontier; the
                    // holder then refuses STATUS/ATTACH instead of silently
                    // replaying stale coordinates.
                    Err(_) if worker_state.commit_issued() => store.selected_result().ok(),
                    Err(_) if worker_state.commit_dirty() => None,
                    Err(_) => Some(*store.selected()),
                };
                let completed = Instant::now();
                let publishing = completed < deadline && worker_state.publish();
                #[cfg(test)]
                if publishing {
                    worker_state.pause_publication();
                }
                let result_failed = result.is_err();
                let mut published = worker_published.lock().expect("published lock");
                match (&mut published.frontier, selected) {
                    (Some(current), Some(commit)) if commit.index > current.index => {
                        *current = commit
                    }
                    (_, None) => published.frontier = None,
                    _ => {}
                }
                let abandoned = worker_state.closed();
                let failed = result_failed || completed >= deadline || abandoned;
                // Make completion delivery a phase barrier: once a controller
                // observes `next()`, the lane is already snapshottable (or
                // terminally closed on failure).
                worker_state.finish(failed);
                if publishing && !abandoned {
                    published.completed.push_back((completed, result));
                }
                drop(published);
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
            published,
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
            Err(StoreError::Io(kind.into()))
        } else {
            let published = Arc::clone(&self.published);
            let observed = match published.try_lock() {
                Ok(mut published) => match published.completed.pop_front() {
                    Some(completed) => Ok(Some(completed)),
                    None if now >= deadline => {
                        if self.state.expire() {
                            Err(ErrorKind::TimedOut)
                        } else {
                            Ok(None)
                        }
                    }
                    None => Ok(None),
                },
                Err(TryLockError::WouldBlock) => return None,
                Err(TryLockError::Poisoned(_)) => Err(ErrorKind::BrokenPipe),
            };
            match observed {
                Ok(Some((completed, _))) if completed >= deadline => {
                    Err(self.fail(ErrorKind::TimedOut))
                }
                Ok(Some((_, result))) => result,
                Ok(None) => return None,
                Err(kind) => Err(self.fail(kind)),
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

    pub(crate) fn selected(&self) -> Option<Commit> {
        self.published.lock().expect("published lock").frontier
    }
    pub(crate) fn hold(&self) -> bool {
        self.state.hold()
    }
    pub(crate) fn release(&self) {
        self.state.release();
    }
    pub(crate) fn snapshot(&self) -> Frontier {
        match self.published.try_lock() {
            Ok(published) => Frontier::Ready(published.frontier),
            Err(TryLockError::WouldBlock) => Frontier::Busy,
            Err(TryLockError::Poisoned(_)) => Frontier::Failed,
        }
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn block_publication(&self, entered: Sender<()>, release: Receiver<()>) {
        let published = Arc::clone(&self.published);
        std::thread::spawn(move || {
            let _guard = published.lock().expect("published lock");
            let _ = entered.send(());
            let _ = release.recv();
        });
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn delay_publication(&self, entered: Sender<()>, release: Receiver<()>) {
        *self
            .state
            .publication_gate
            .lock()
            .expect("publication gate") = Some((entered, release));
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
        StoreError::Io(kind.into())
    }
}
