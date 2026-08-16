use crate::events::{self, Axis, Cursor, Event, EventKind, EventStream, Json};
use crate::runtime::private::now;
use crate::store::{Commit, Store, StoreError};
use crate::terminal::Observation;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

mod worker;
use worker::{Frontier, Lane, Work};

const TERMINAL_RESERVATION: usize = 14_210;

schema!(enum pub Purpose [Clone, Copy, Debug, Eq, PartialEq]; Background, Clear(u64, u64), Lifecycle, Semantic(u64, bool), Sources(u64, bool), Final);
schema!(enum pub StorageError [Clone, Copy, Debug, Eq, PartialEq]; Disabled, Busy);
schema!(struct pub Done pub fields; lane: usize, purpose: Purpose, result: Result<(Commit, bool), StoreError>);
schema!(struct pub EventConfig pub fields; store: Store, stream: EventStream, created: u64, session: String, generation: Option<u32>);
schema!(struct Events fields; stream: EventStream, records: String, created: u64, session: String, generation: Option<u32>, reserved: usize, snapshots: [Option<Event>; 3], semantic: BTreeMap<(usize, Arc<[u8]>), (Event, usize)>);
schema!(struct pub SessionStorage fields; lanes: [Option<Lane>; 3], log_cap: u64, events: Option<Events>);
schema!(struct pub(crate) StatusSnapshot derive [Clone, Copy] pub(crate) fields; health: u8, event: Option<Commit>, log: Option<Commit>);
schema!(enum pub(crate) SnapshotState; Ready(StatusSnapshot), Busy, Failed);

impl Events {
    fn header(&self, cursor: Cursor) -> String {
        events::canonical_header(self.created, &self.session, self.generation, cursor)
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
                (
                    Lane::new(config.store, jobs.min(64), bytes.min(512 << 10)),
                    Events {
                        stream: config.stream,
                        records: String::new(),
                        created: config.created,
                        session: config.session,
                        generation: config.generation,
                        reserved,
                        snapshots: [None, None, None],
                        semantic: Default::default(),
                    },
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
        return_if!(events.is_empty(), Ok(()));
        self.record(purpose, events, None)
    }

    fn record(
        &mut self,
        mut purpose: Purpose,
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
        let mut body = events.header(cursor);
        let mut body_len = body.len() + events.records.len() + records.len();
        let mut compacted = false;
        if body_len > 256 << 10 {
            let snapshots =
                events
                    .snapshots
                    .iter()
                    .flatten()
                    .chain(events.semantic.iter().filter_map(|(key, (event, _))| {
                        (!retained.contains_key(key)).then_some(event)
                    }))
                    .cloned()
                    .collect::<Vec<_>>();
            next = events.stream;
            (records, cursor, exhausted) = next
                .transact(&snapshots, transitions, true)
                .map_err(|_| StorageError::Disabled)?;
            compacted = !matches!(exhausted, Some(Axis::Sequence | Axis::Epoch));
            body = events.header(cursor);
            body_len = body.len() + records.len() + usize::from(!compacted) * events.records.len();
        }
        return_if!(
            body_len > 320 << 10 || body_len > 256 << 10 && changes && exhausted.is_none(),
            Err(StorageError::Busy)
        );
        let Cursor(epoch, next_sequence, first, _) = cursor;
        let accepted = exhausted != Some(Axis::Sequence);
        if let Purpose::Semantic(tag, _) = purpose {
            purpose = Purpose::Semantic(tag, exhausted.is_some());
        }
        body.reserve(body_len - body.len());
        if !compacted {
            body.push_str(&events.records);
        }
        body.push_str(&records);
        let body = body.into_bytes();
        if !accepted {
            purpose = Purpose::Background;
        }
        self.lanes[1]
            .as_mut()
            .ok_or(StorageError::Disabled)?
            .submit(purpose, Work::Replace(body, epoch, first, next_sequence))?;
        events.stream = next;
        if compacted {
            events.records.clear();
        }
        events.records.push_str(&records);
        return_if!(!accepted, Err(StorageError::Disabled));
        if let Some(slot) = remember_after {
            events.snapshots[slot] = transitions.first().cloned();
        }
        events.reserved = reserved;
        events.semantic.extend(retained);
        Ok(())
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
        let event = self
            .events
            .as_ref()
            .is_some_and(|events| events.stream.writable());
        self.lanes.iter().enumerate().fold(0, |bits, (at, lane)| {
            bits | (u8::from(lane.as_ref().is_some_and(Lane::writable)) << at)
        }) & (!2 | (u8::from(event) << Self::EVENT_LANE))
    }

    /// Acquire a nonblocking memory-only status linearization point across all
    /// configured lanes. A worker sets its commit phase before the alternate
    /// commit can become selectable and publishes the selected frontier before
    /// clearing it. Holding every idle lane therefore freezes exact event/log
    /// metadata without reading or hashing a store on the holder thread.
    pub(crate) fn try_status_snapshot(&self) -> SnapshotState {
        let mut held = 0u8;
        for (at, lane) in self.lanes.iter().enumerate() {
            let Some(lane) = lane else { continue };
            if !lane.hold() {
                self.release_held(held);
                return SnapshotState::Busy;
            }
            held |= 1 << at;
        }
        let event = self.lanes[Self::EVENT_LANE].as_ref().map(Lane::snapshot);
        let log = self.lanes[0].as_ref().map(Lane::snapshot);
        if matches!(event, Some(Frontier::Busy)) || matches!(log, Some(Frontier::Busy)) {
            self.release_held(held);
            return SnapshotState::Busy;
        }
        if matches!(event, Some(Frontier::Ready(None) | Frontier::Failed))
            || matches!(log, Some(Frontier::Ready(None) | Frontier::Failed))
        {
            self.release_held(held);
            return SnapshotState::Failed;
        }
        let commit = |frontier| match frontier {
            Some(Frontier::Ready(commit)) => commit,
            _ => None,
        };
        SnapshotState::Ready(StatusSnapshot {
            health: self.health(),
            event: commit(event),
            log: commit(log),
        })
    }

    pub(crate) fn release_status_snapshot(&self) {
        self.release_held(u8::MAX);
    }

    fn release_held(&self, held: u8) {
        for (at, lane) in self.lanes.iter().enumerate() {
            if let Some(lane) = lane.as_ref().filter(|_| held & (1 << at) != 0) {
                lane.release();
            }
        }
    }

    pub fn pending(&self) -> usize {
        self.lanes.iter().flatten().map(Lane::pending).sum()
    }

    pub(crate) fn abandon_clear(&mut self, tag: u64) {
        if self.lanes[0].as_ref().is_some_and(|lane| {
            lane.pending_matches(|purpose| matches!(purpose, Purpose::Clear(id, _) if id == tag))
        }) {
            self.quarantine_log();
        }
    }

    pub(crate) fn quarantine_log(&mut self) {
        self.lanes[0].iter_mut().for_each(Lane::close);
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
