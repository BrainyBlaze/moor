use crate::events::{self, Axis, Cursor, Event, EventKind, EventStream, Json};
use crate::runtime::private::now;
use crate::store::{Commit, Store, StoreError};
use crate::terminal::Observation;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

mod worker;
use worker::{Lane, Work};

const TERMINAL_RESERVATION: usize = 14_210;

schema!(enum pub Purpose [Clone, Copy, Debug, Eq, PartialEq]; Background, Clear(u64, u64), Lifecycle, Semantic(u64, bool), Sources(u64, bool), Final);
schema!(enum pub StorageError [Clone, Copy, Debug, Eq, PartialEq]; Disabled, Busy);
schema!(struct pub Done pub fields; lane: usize, purpose: Purpose, result: Result<(Commit, bool), StoreError>);
schema!(struct pub EventConfig pub fields; store: Store, stream: EventStream, created: u64, session: String, generation: Option<u32>);
schema!(struct Events fields; stream: EventStream, records: String, created: u64, session: String,
    generation: Option<u32>, reserved: usize, snapshots: [Option<Event>; 3], semantic: BTreeMap<(usize, Arc<[u8]>), (Event, usize)>);
schema!(struct pub SessionStorage fields; lanes: [Option<Lane>; 3], log_cap: u64, events: Option<Events>);

impl Events {
    fn body(&self, cursor: Cursor, history: bool, records: &str) -> String {
        let mut body =
            events::canonical_header(self.created, &self.session, self.generation, cursor);
        body.reserve(records.len() + usize::from(history) * self.records.len());
        if history {
            body.push_str(&self.records);
        }
        body.push_str(records);
        body
    }
}

impl SessionStorage {
    pub fn new(
        log: Option<(Store, u64)>,
        events: Option<EventConfig>,
        lifecycle: Store,
        jobs: usize,
        bytes: usize,
    ) -> Self {
        let log_cap = log.as_ref().map_or(0, |(_, cap)| *cap);
        let log = log.map(|(store, _)| Lane::new(store, jobs.min(64), bytes.min(1 << 20)));
        let (event_lane, events) = events
            .map(|config| {
                let reserved = events::canonical_header(
                    config.created,
                    &config.session,
                    config.generation,
                    Cursor(u32::MAX, u64::MAX, u64::MAX, 1),
                )
                .len()
                    + TERMINAL_RESERVATION;
                let state = Events {
                    stream: config.stream,
                    records: String::new(),
                    created: config.created,
                    session: config.session,
                    generation: config.generation,
                    reserved,
                    snapshots: [None, None, None],
                    semantic: Default::default(),
                };
                (
                    Lane::new(config.store, jobs.min(64), bytes.min(512 << 10)),
                    state,
                )
            })
            .unzip();
        Self {
            lanes: [
                log,
                event_lane,
                Some(Lane::new(lifecycle, jobs.min(1), bytes.min(4 << 20))),
            ],
            log_cap,
            events,
        }
    }

    pub fn output(&mut self, bytes: Arc<[u8]>, end: u64) -> Result<(), StorageError> {
        let cap = self.log_cap;
        self.submit(0, Purpose::Background, Work::Append(bytes, cap, end))
    }

    pub fn observe(&mut self, observation: Observation) -> Result<(), StorageError> {
        let Some((transition, slot)) = observed_event(&observation, now()) else {
            return Ok(());
        };
        self.record(Purpose::Background, std::slice::from_ref(&transition), slot)
    }

    pub fn commit(&mut self, purpose: Purpose, events: &[Event]) -> Result<(), StorageError> {
        if events.is_empty() {
            Ok(())
        } else {
            self.record(purpose, events, None)
        }
    }

    fn record(
        &mut self,
        purpose: Purpose,
        transitions: &[Event],
        remember_after: Option<usize>,
    ) -> Result<(), StorageError> {
        let events = self.events.as_mut().ok_or(StorageError::Disabled)?;
        let mut reserved = events.reserved;
        let mut retained = BTreeMap::new();
        for event in transitions {
            let Some((class, source)) = event.retention() else {
                continue;
            };
            let size = reserved_event(class, event)?;
            let key = (class, Arc::clone(source));
            let prior = retained
                .get(&key)
                .or_else(|| events.semantic.get(&key))
                .map_or(0, |entry| entry.1);
            retained.insert(key, (event.clone(), size));
            reserved = reserved - prior + size;
        }
        let changes = !retained.is_empty();
        return_if!(changes && reserved > 256 << 10, Err(StorageError::Busy));
        let mut next = events.stream;
        let (mut records, mut cursor, mut exhausted) = next
            .transact(&[], transitions, false)
            .map_err(|_| StorageError::Disabled)?;
        let mut body = events.body(cursor, true, &records);
        let mut compacted = false;
        if body.len() > 256 << 10 {
            let mut snapshots = events
                .snapshots
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            snapshots.extend(
                events
                    .semantic
                    .iter()
                    .filter(|(key, _)| !retained.contains_key(*key))
                    .map(|(_, event)| event.0.clone()),
            );
            next = events.stream;
            (records, cursor, exhausted) = next
                .transact(&snapshots, transitions, true)
                .map_err(|_| StorageError::Disabled)?;
            compacted = !matches!(exhausted, Some(Axis::Sequence | Axis::Epoch));
            body = events.body(cursor, !compacted, &records);
        }
        return_if!(
            body.len() > 320 << 10 || body.len() > 256 << 10 && changes && exhausted.is_none(),
            Err(StorageError::Busy)
        );
        let Cursor(epoch, next_sequence, first, _) = cursor;
        let accepted = exhausted != Some(Axis::Sequence);
        let purpose = match purpose {
            Purpose::Semantic(tag, _) => Purpose::Semantic(tag, exhausted.is_some()),
            purpose => purpose,
        };
        let body = body.into_bytes();
        let submitted = if accepted {
            purpose
        } else {
            Purpose::Background
        };
        self.lanes[1]
            .as_mut()
            .ok_or(StorageError::Disabled)?
            .submit(submitted, Work::Replace(body, epoch, first, next_sequence))?;
        events.stream = next;
        if compacted {
            events.records = records;
        } else {
            events.records.push_str(&records);
        }
        if accepted {
            if let Some(slot) = remember_after {
                events.snapshots[slot] = transitions.first().cloned();
            }
            events.reserved = reserved;
            events.semantic.extend(retained);
        }
        accepted.then_some(()).ok_or(StorageError::Disabled)
    }

    pub fn clear(&mut self, tag: u64, observed: u64, end: u64) -> Result<(), StorageError> {
        self.submit(0, Purpose::Clear(tag, observed), Work::Clear(observed, end))
    }

    pub fn lifecycle(&mut self, body: Vec<u8>, end: u64) -> Result<(), StorageError> {
        self.submit(2, Purpose::Lifecycle, Work::Replace(body, 1, end, end))
    }

    fn submit(&mut self, lane: usize, purpose: Purpose, work: Work) -> Result<(), StorageError> {
        self.lanes[lane]
            .as_mut()
            .ok_or(StorageError::Disabled)?
            .submit(purpose, work)
    }

    pub fn poll(&mut self) -> smallvec::SmallVec<[Done; 4]> {
        let now = Instant::now();
        let mut out = smallvec::SmallVec::new();
        for (at, lane) in self.lanes.iter_mut().enumerate() {
            let Some(lane) = lane else { continue };
            while let Some(done) = lane.try_complete(now) {
                out.push(Done { lane: at, ..done });
            }
        }
        out
    }

    pub const EVENT_LANE: usize = 1;

    pub fn health(&self) -> u8 {
        self.lanes.iter().enumerate().fold(0, |bits, (at, lane)| {
            bits | (u8::from(
                lane.as_ref().is_some_and(Lane::writable)
                    && (at != 1
                        || self
                            .events
                            .as_ref()
                            .is_some_and(|events| events.stream.writable())),
            ) << at)
        })
    }

    /// The event lane's currently selected commit, which is what a reader
    /// would select. §5 of the schema requires the status descriptor to carry
    /// this rather than uncommitted writer state or a stale launch value.
    /// The event lane's selected commit. Callers must drain completions first,
    /// which `Runtime::send_status` does, so this reflects every commit whose
    /// completion has been observed. A commit issued by a worker that was then
    /// quarantined can validate afterwards without a completion to observe; that
    /// residual case needs a handle-bound refresh rather than a re-open by
    /// pathname, which would reintroduce §11.4's check/use window.
    pub fn event_commit(&self) -> Option<(u8, u64, u64, [u8; 32])> {
        // The worker publishes this frontier after every operation, from a
        // recovery read on its own validated handles, so it covers both the Ok
        // and the reported-error paths. It does NOT cover the interval between
        // run() flushing a commit and the worker publishing it: a holder-side
        // read would, but only once the store uses position-independent I/O,
        // because duplicated handles share a file offset on POSIX and a
        // concurrent read would corrupt the writer's position.
        let selected = self.lanes[Self::EVENT_LANE].as_ref()?.selected();
        Some((
            selected.body,
            selected.index,
            selected.length,
            selected.hash,
        ))
    }

    pub fn log_status(&self) -> Option<(u32, u64, u64, u64)> {
        self.lanes[0].as_ref().map(|lane| {
            let commit = lane.selected();
            (commit.epoch, commit.index, commit.start, commit.end)
        })
    }

    pub fn pending(&self) -> usize {
        self.lanes.iter().flatten().map(Lane::pending).sum()
    }
}

fn reserved_event(class: usize, event: &Event) -> Result<usize, StorageError> {
    let record = events::canonical_event(u32::MAX, u64::MAX, EventKind::Snapshot, event);
    return_if!(class != 0, Ok(record.len()));
    let at = record.find(",\"status\":").ok_or(StorageError::Disabled)?;
    Ok(at + ",\"status\":\"disconnected\",\"reason\":\"transport-closed\"}\n".len())
}

fn observed_event(observation: &Observation, ts: u64) -> Option<(Event, Option<usize>)> {
    macro_rules! record {
        ($slot:expr, $name:literal; $($key:literal => $value:expr),* $(,)?) => {
            Some((events::event($name, ts, &[$(($key, $value)),*]), $slot))
        };
    }
    match observation {
        Observation::Ready => record!(Some(0), "ready";),
        Observation::State(state, title, truncated) => record!(
            Some(1), "state"; "state" => Json::String(state), "title" => Json::String(title),
            "truncated" => Json::Bool(*truncated)
        ),
        Observation::Link(uri, truncated) => record!(
            Some(2), "link"; "uri" => Json::String(uri), "truncated" => Json::Bool(*truncated)
        ),
        Observation::Degraded(scanner, reason) => record!(
            None, "observer-degraded"; "scanner" => Json::String(scanner),
            "reason" => Json::String(reason)
        ),
        Observation::Query(..) => None,
    }
}
