use super::{Done, Purpose, StorageError};
use crate::store::{Commit, Reader, Store, StoreError};
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender, TryRecvError},
};
use std::time::{Duration, Instant};

type Outcome = Result<(Commit, bool), StoreError>;
type Job = (Purpose, Instant, usize);
schema!(enum pub(crate) Work; Append(Arc<[u8]>, u64, u64), Replace(Vec<u8>, u32, u64, u64), Clear(u64, u64), #[cfg(test)] Hold(Receiver<()>));

impl Work {
    fn size(&self) -> usize {
        match self {
            Self::Append(bytes, ..) => bytes.len(),
            Self::Replace(bytes, ..) => bytes.len(),
            _ => 0,
        }
    }
    fn run(self, store: &mut Store) -> Outcome {
        match self {
            Self::Append(bytes, cap, end) => store.append_capped(&bytes, cap, end),
            Self::Replace(bytes, epoch, start, end) => store.replace(&bytes, epoch, start, end),
            Self::Clear(observed, end) => {
                let selected = *store.selected();
                if selected.index != observed || selected.length == 0 {
                    return Ok((selected, selected.index != observed));
                }
                let epoch = selected.epoch.checked_add(1).ok_or(StoreError::Exhausted)?;
                store.replace(&[], epoch, end, end)
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

schema!(struct pub(crate) Lane fields; submit: Sender<Work>, done: Receiver<Outcome>, limits: (usize, usize), pending: VecDeque<Job>,
    bytes: usize, failure: Option<ErrorKind>, open: Arc<AtomicBool>, frontier: Arc<Mutex<Commit>>);

impl Lane {
    pub(crate) fn new(mut store: Store, jobs: usize, bytes: usize) -> Self {
        // The worker publishes its selected commit before acknowledging the
        // job, so a descriptor built between the commit and the consumption of
        // its completion still names the commit a reader would select (§5). The
        // alternative — re-opening the store by pathname — would reintroduce
        // §11.4's check/use substitution window on every STATUS.
        let frontier = Arc::new(Mutex::new(*store.selected()));
        let published = Arc::clone(&frontier);
        let (submit, work) = mpsc::channel::<Work>();
        let (finish, done) = mpsc::channel();
        let open = Arc::new(AtomicBool::new(true));
        let worker_open = Arc::clone(&open);
        std::thread::spawn(move || {
            let reader = store.reader().ok();
            for operation in work {
                if !worker_open.load(Ordering::Acquire) {
                    break;
                }
                let result = operation.run(&mut store);
                // Published after BOTH outcomes, from a recovery/select read on
                // the store's own validated handles: a write or flush can report
                // an error after leaving a candidate a reader validates, so
                // publishing only the Ok commit would leave the frontier behind
                // permanently. Never publishes an attempted commit blindly, and
                // never moves backwards.
                if let Some(commit) = reader.as_ref().and_then(Reader::selected) {
                    let mut frontier = published.lock().expect("frontier lock");
                    if commit.index > frontier.index {
                        *frontier = commit;
                    }
                }
                let failed = result.is_err();
                if finish.send(result).is_err() || failed {
                    worker_open.store(false, Ordering::Release);
                    break;
                }
            }
        });
        Self {
            submit,
            done,
            limits: (jobs, bytes),
            pending: VecDeque::new(),
            bytes: 0,
            failure: None,
            open,
            frontier,
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
        if self.submit.send(operation).is_err() {
            self.fail(ErrorKind::BrokenPipe);
            return Err(StorageError::Disabled);
        }
        self.bytes = bytes;
        self.pending
            .push_back((purpose, Instant::now() + Duration::from_secs(2), size));
        Ok(())
    }

    pub(crate) fn try_complete(&mut self, now: Instant) -> Option<Done> {
        let result = if let Some(kind) = self.failure {
            Err(StoreError::Io(kind.into()))
        } else {
            match self.done.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Disconnected) if !self.pending.is_empty() => {
                    Err(self.fail(ErrorKind::BrokenPipe))
                }
                Err(TryRecvError::Empty)
                    if self.pending.front().is_some_and(|job| now >= job.1) =>
                {
                    Err(self.fail(ErrorKind::TimedOut))
                }
                Err(_) => return None,
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

    pub(crate) fn selected(&self) -> Commit {
        *self.frontier.lock().expect("frontier lock")
    }
    pub(crate) fn writable(&self) -> bool {
        self.failure.is_none() && self.open.load(Ordering::Acquire)
    }
    pub(crate) fn pending(&self) -> usize {
        self.pending.len()
    }
    pub(crate) fn close(&mut self) {
        self.fail(ErrorKind::WouldBlock);
    }
    fn fail(&mut self, kind: ErrorKind) -> StoreError {
        self.failure.get_or_insert(kind);
        self.open.store(false, Ordering::Release);
        StoreError::Io(kind.into())
    }
}
