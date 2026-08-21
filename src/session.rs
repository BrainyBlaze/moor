use crate::wire::{
    InputReceipt, Query, QueryShape, ReplayDescriptor, WireError, validate_query_reply,
};
use sha2::{Digest, Sha256};
use smallvec::SmallVec;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::Range;
use std::sync::Arc;

pub type ConnId = u64;

macro_rules! lease_codec {
    ($name:ident[$size:literal]; valid $this:ident => $valid:expr; raw $raw:ident => $raw_valid:expr;
     value $value:expr; fields $($at:expr => $field:expr),+ $(,)?) => {
        impl $name {
            fn valid_wire(&self) -> bool { let $this = self; $valid }
            pub fn encode_wire(&self) -> Result<[u8; $size], WireError> {
                let $this = self;
                crate::wire::require($valid, WireError::Malformed)?;
                crate::wire::fixed_payload(&[$(($at, $field)),+])
            }
            pub fn decode_wire($raw: &[u8]) -> Result<Self, WireError> {
                crate::wire::require($raw.len() == $size && $raw_valid, WireError::Malformed)?;
                let value = $value;
                crate::wire::require(value.valid_wire(), WireError::Malformed)?;
                Ok(value)
            }
        }
    };
}

schema!(enum ordinal pub LeaseOperation; Fresh, Resume);
schema!(enum ordinal pub LeaseRole; Viewer, InputOnly);
schema!(enum ordinal pub ResultOutcome; Granted, Resumed, Released, Refused);
schema!(enum ordinal pub ResultReason; None, Busy, BadEpoch, BadToken, BadRole, NotHeld, Exhausted, BadIncarnation, BadSequence);
schema!(struct default pub LeaseRequest derive [Clone, Debug, Eq, PartialEq] pub fields; operation: LeaseOperation = LeaseOperation::Fresh, role: LeaseRole = LeaseRole::Viewer, epoch: u32 = 0, incarnation: [u8; 16] = [0; 16], token: [u8; 16] = [0; 16]);
schema!(struct pub LeaseResult derive [Clone, Debug, Eq, PartialEq] pub fields; outcome: ResultOutcome, reason: ResultReason, role: LeaseRole, epoch: u32, token: [u8; 16]);
lease_codec!(LeaseRequest[40];
    valid this => { let resume = this.operation == LeaseOperation::Resume;
        (this.epoch != 0) == resume && (this.incarnation != [0; 16]) == resume && (this.token != [0; 16]) == resume };
    raw bytes => bytes[0] <= 1 && bytes[1] <= 1 && bytes[2..4] == [0, 0];
    value Self { operation: LeaseOperation::from_ordinal(bytes[0]), role: LeaseRole::from_ordinal(bytes[1]),
        epoch: u32::from_le_bytes(bytes[4..8].try_into().unwrap()), incarnation: bytes[8..24].try_into().unwrap(),
        token: bytes[24..40].try_into().unwrap() };
    fields 0 => &[this.operation as u8], 1 => &[this.role as u8], 4 => &this.epoch.to_le_bytes(),
        8 => &this.incarnation, 24 => &this.token,
);
impl LeaseRequest {
    pub fn fresh(role: LeaseRole) -> Self {
        Self {
            role,
            ..Self::default()
        }
    }
}
lease_codec!(LeaseResult[24];
    valid this => { let common = this.reason == ResultReason::None && this.epoch != 0;
        match this.outcome {
            ResultOutcome::Granted | ResultOutcome::Resumed => common && this.token != [0; 16],
            ResultOutcome::Released => common && this.token == [0; 16],
            ResultOutcome::Refused => matches!(this.reason as u8, 1..=7) && this.token == [0; 16],
        } };
    raw bytes => bytes[0] <= 3 && bytes[1] <= 8 && bytes[2] <= 1 && bytes[3] == 0;
    value Self { outcome: ResultOutcome::from_ordinal(bytes[0]), reason: ResultReason::from_ordinal(bytes[1]),
        role: LeaseRole::from_ordinal(bytes[2]), epoch: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
        token: bytes[8..24].try_into().unwrap() };
    fields 0 => &[this.outcome as u8], 1 => &[this.reason as u8], 2 => &[this.role as u8],
        4 => &this.epoch.to_le_bytes(), 8 => &this.token,
);
impl LeaseResult {
    fn success(outcome: ResultOutcome, role: LeaseRole, epoch: u32, token: [u8; 16]) -> Self {
        Self {
            outcome,
            reason: ResultReason::None,
            role,
            epoch,
            token,
        }
    }
}

schema!(struct pub OwnedInput derive [Clone, Debug, Eq, PartialEq] pub fields; epoch: u32, request_id: u64, exact_payload: Arc<[u8]>);
impl OwnedInput {
    pub(crate) fn application_id(&self) -> Option<[u8; 16]> {
        (self.exact_payload.get(12) == Some(&1)).then_some(())?;
        self.exact_payload.get(13..29)?.try_into().ok()
    }
}

schema!(struct default Lease fields; owner: Option<ConnId> = None, role: LeaseRole = LeaseRole::Viewer, epoch: u32 = 0, token: [u8; 16] = [0; 16], deadline: u64 = 0, cached: Option<(OwnedInput, [u8; 43])> = None, inflight: Option<OwnedInput> = None);

/// The undo ledger for one in-flight standalone lease reply. A grant or a
/// resume mutates the machine BEFORE its result frame is enqueued; if that
/// enqueue fails, the requester never saw the token, so the mutation must
/// unwind: a fresh grant restores the (empty) prior lease and the prior
/// allocation, a resume restores ownerless state, the old token, and the
/// original deadline while leaving the request cache untouched. Refusals
/// never create an entry — they are nonmutating by construction.
enum LeaseUndo {
    Fresh {
        prior: Lease,
        allocated: u32,
    },
    Resume {
        owner: Option<ConnId>,
        token: [u8; 16],
        deadline: u64,
    },
}

schema!(enum ordinal pub Phase; Unattached, InputOnly, Resumed, Observer, Viewer, Closing);

pub const fn legal_in_phase(phase: Phase, kind: u8) -> bool {
    const MASKS: [u32; 6] = [
        1 << 3 | 1 << 13 | 1 << 15 | 1 << 21 | 1 << 25,
        1 << 9 | 1 << 23 | 1 << 24,
        1 << 3 | 1 << 23 | 1 << 24,
        1 << 7 | 1 << 13 | 1 << 15 | 1 << 21 | 1 << 25,
        1 << 7
            | 1 << 9
            | 1 << 11
            | 1 << 12
            | 1 << 13
            | 1 << 15
            | 1 << 23
            | 1 << 24
            | 1 << 25
            | 1 << 27,
        0,
    ];
    kind < 32 && MASKS[phase as usize] & (1 << kind) != 0
}

pub const fn next_phase(phase: Phase, action: u8, flags: u8) -> Option<Phase> {
    use Phase::*;
    match (phase, action, flags) {
        (Unattached, 3, 3) => Some(Viewer),
        (Unattached, 3, _) => Some(Observer),
        (Resumed, 3, _) => Some(Viewer),
        (Unattached, 0x15, 2 | 3) => Some(InputOnly),
        (Unattached, 0x15, 1) => Some(Resumed),
        (Observer, 0x15, 0) => Some(Viewer),
        (Viewer, 0x17, _) => Some(Observer),
        (InputOnly | Resumed, 0x17, _) => Some(Unattached),
        _ => None,
    }
}

schema!(enum ordinal pub SemanticMode; Edge, Stateful);
schema!(enum ordinal pub SemanticEventKind; Transition, Snapshot, ApplicationReceipt);
schema!(enum ordinal pub SemanticAckStatus; Accepted, Duplicate, Refused);
schema!(enum ordinal pub SemanticRefusal; CapabilityAbsent, StaleToken, Generation, SourceConflict, ResourceExhausted, BadSequence, EventConflict, SnapshotRequired, InvalidPayload, Superseded, ApplicationConflict, UnknownApplication, SourceUnavailable, NotWritten);
type SemResult<T = ()> = Result<T, SemanticRefusal>;
pub fn next_semantic_sequence(high: u64) -> Result<u64, SemanticRefusal> {
    high.checked_add(1)
        .ok_or(SemanticRefusal::ResourceExhausted)
}
schema!(struct pub SemanticHello derive [Clone, Debug, Eq, PartialEq] pub fields; token: [u8; 16], producer: [u8; 16], generation: u32, mode: SemanticMode, capabilities: u8, source: Arc<[u8]>);
schema!(struct pub SemanticEvent derive [Clone, Debug, Eq, PartialEq] pub fields; id: [u8; 16], sequence: u64, kind: SemanticEventKind, exact_payload: Arc<[u8]>);
schema!(struct pub SemanticAck derive [Clone, Debug, Eq, PartialEq] pub fields; id: [u8; 16], sequence: u64, status: SemanticAckStatus, position: Option<EventPosition>);
schema!(struct pub SemanticHelloAck derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; epoch: u32, snapshot_required: bool);
schema!(struct pub EventPosition derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; epoch: u32, sequence: u64);
impl SemanticAck {
    fn at(event: &SemanticEvent, status: SemanticAckStatus, position: EventPosition) -> Self {
        Self {
            id: event.id,
            sequence: event.sequence,
            status,
            position: Some(position),
        }
    }
}
schema!(tuple pub Ticket [Clone, Copy, Debug, Eq, PartialEq, Hash]; fields; u64);
pub type CommitTicket = Ticket;
pub type WriteTicket = Ticket;
impl Ticket {
    pub const fn get(self) -> u64 {
        self.0
    }
    pub(crate) const fn from_raw(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }
}
schema!(struct pub ApplicationInput derive [Clone, Debug, Eq, PartialEq] pub fields; receipt: ApplicationReceipt, source: Range<usize>, terminal_at: usize);
schema!(struct pub InputNotice derive [Clone, Debug, Eq, PartialEq] pub fields; receipt: ApplicationReceipt, byte_count: u64, digest: [u8; 32]);
schema!(struct pub SemanticEffect derive [Clone, Debug, Eq, PartialEq] pub fields; receipt: ApplicationReceipt, source: Arc<[u8]>, source_epoch: u32, producer: [u8; 16], reason: MissingReason);
schema!(struct pub ApplicationReceipt derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; application_id: [u8; 16], lease_epoch: u32, request_id: u64);
schema!(struct pub InputNoticeAck derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; receipt: ApplicationReceipt, prepared: bool);
schema!(enum ordinal pub MissingReason; Deadline, SourceLost, RetentionExpired);
schema!(enum ordinal pub SourceStatus; Connected, Exact, Degraded, Disconnected);
schema!(enum ordinal pub SourceReason; None, HeartbeatTimeout, TransportClosed, Superseded, SessionEnding);
schema!(struct pub SourceEffect derive [Clone, Debug, Eq, PartialEq] pub fields; source: Arc<[u8]>, producer: [u8; 16], source_epoch: u32, status: SourceStatus, reason: SourceReason);
schema!(struct Binding derive [Clone, Copy, Eq, PartialEq] fields; conn: ConnId, epoch: u32, producer: [u8; 16]);
impl Binding {
    fn change(
        self,
        source: &Arc<[u8]>,
        status: SourceStatus,
        reason: SourceReason,
    ) -> SemanticChange {
        SemanticChange::Source(SourceEffect {
            source: Arc::clone(source),
            producer: self.producer,
            source_epoch: self.epoch,
            status,
            reason,
        })
    }
}
schema!(enum pub SemanticChange [Clone, Debug, Eq, PartialEq]; Source(SourceEffect), Missing(SemanticEffect));

schema!(struct Retained fields; id: [u8; 16], sequence: u64, kind: SemanticEventKind, digest: [u8; 32], position: EventPosition);
schema!(struct default Source fields; binding: Binding = Binding { conn: 0, epoch: 0, producer: [0; 16] }, mode: SemanticMode = SemanticMode::Edge, capabilities: u8 = 0, status: SourceStatus = SourceStatus::Connected, entries: VecDeque<Retained> = VecDeque::new(), pending: u8 = 0, last_seen: u64 = 0);
schema!(struct PendingHello fields; name: Arc<[u8]>, source: Source, superseded: Option<ConnId>, missing: Vec<[u8; 16]>);
const COMMIT_PENDING: u8 = 1;
const ACK_PENDING: u8 = 2;
pub const GEOMETRY_LIMIT: u16 = 32_767;
pub const GEOMETRY_CELLS: u32 = 2_000_000;
macro_rules! reject {
    ($($invalid:expr => $error:expr),+ $(,)?) => {
        $(if $invalid { return Err($error); })+
    };
}
impl Retained {
    fn new(event: &SemanticEvent, position: EventPosition) -> Self {
        Self {
            id: event.id,
            sequence: event.sequence,
            kind: event.kind,
            digest: Sha256::digest(&event.exact_payload).into(),
            position,
        }
    }
    fn matches(&self, event: &SemanticEvent) -> bool {
        self.id == event.id
            && self.sequence == event.sequence
            && self.kind == event.kind
            && self.digest == <[u8; 32]>::from(Sha256::digest(&event.exact_payload))
    }
}
impl Source {
    fn stateful(&self) -> bool {
        self.mode == SemanticMode::Stateful
    }
    fn active(&self) -> bool {
        self.status != SourceStatus::Disconnected
    }
    fn exact(&self) -> bool {
        self.stateful() && self.status == SourceStatus::Exact
    }
    fn timed_out(&self, now: u64) -> bool {
        self.stateful()
            && matches!(self.status, SourceStatus::Connected | SourceStatus::Exact)
            && now >= self.last_seen.saturating_add(15_000)
    }
    fn transition(
        &mut self,
        name: &Arc<[u8]>,
        status: SourceStatus,
        reason: SourceReason,
    ) -> Option<SemanticChange> {
        return_if!(self.status == status, None);
        self.status = status;
        self.stateful()
            .then(|| self.binding.change(name, status, reason))
    }

    fn admit(
        &self,
        event: &SemanticEvent,
        receipt: Option<ApplicationReceipt>,
        writable: bool,
        valid_application: impl FnOnce(ApplicationReceipt) -> SemResult,
    ) -> SemResult<Option<SemanticAck>> {
        use SemanticRefusal::*;
        reject! {
            !writable => ResourceExhausted,
            receipt.is_some() != (event.kind == SemanticEventKind::ApplicationReceipt) => InvalidPayload,
            self.capabilities & if receipt.is_some() { 2 } else { 1 } == 0 => CapabilityAbsent,
            !self.stateful() && event.kind != SemanticEventKind::Transition => InvalidPayload,
        }
        if let Some(prior) = self
            .entries
            .iter()
            .find(|prior| prior.id == event.id || prior.sequence == event.sequence)
        {
            reject! { !prior.matches(event) => EventConflict }
            return Ok(Some(SemanticAck::at(
                event,
                SemanticAckStatus::Duplicate,
                prior.position,
            )));
        }
        reject! {
            self.stateful() && !self.exact() && event.kind != SemanticEventKind::Snapshot => SnapshotRequired,
            event.exact_payload.len() > 32 * 1024
                || event.sequence == 0
                || event.kind != SemanticEventKind::ApplicationReceipt
                    && crate::events::json_object(&event.exact_payload, 64, 1024).is_none() => InvalidPayload,
        }
        let high = self.entries.back().map_or(0, |entry| entry.sequence);
        reject! {
            event.sequence <= high => if high == u64::MAX { ResourceExhausted } else { BadSequence },
            self.pending != 0 || next_semantic_sequence(high)? != event.sequence => BadSequence,
        }
        if let Some(receipt) = receipt {
            valid_application(receipt)?;
        }
        Ok(None)
    }
}
schema!(struct InputPending fields; controller: ConnId, input: OwnedInput, expected: u64, terminal_at: usize);
schema!(enum AppState; Notice(InputPending), Writing, Written);
schema!(struct Application fields; receipt: ApplicationReceipt, source: Arc<[u8]>, binding: Binding, state: AppState, deadline: u64, emitted: u8);
pub(crate) fn valid_source_id(source: &[u8]) -> bool {
    !source.is_empty()
        && source.len() <= 128
        && source
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(byte))
}
impl Application {
    fn current(&self, source: Option<&Source>) -> bool {
        source.is_some_and(|source| source.binding == self.binding && source.exact())
    }

    fn written(&self) -> bool {
        matches!(self.state, AppState::Written)
    }

    fn effect(&self, reason: MissingReason) -> SemanticChange {
        SemanticChange::Missing(SemanticEffect {
            receipt: self.receipt,
            source: Arc::clone(&self.source),
            source_epoch: self.binding.epoch,
            producer: self.binding.producer,
            reason,
        })
    }
    fn emit_once(&mut self, bit: u8, reason: MissingReason) -> Option<SemanticChange> {
        return_if!(self.emitted & bit != 0, None);
        self.emitted |= bit;
        Some(self.effect(reason))
    }

    fn status(&self, conn: ConnId, receipt: ApplicationReceipt) -> SemResult {
        reject! {
            (self.binding.conn, self.receipt.lease_epoch, self.receipt.request_id)
                != (conn, receipt.lease_epoch, receipt.request_id) => SemanticRefusal::UnknownApplication,
            !self.written() => SemanticRefusal::NotWritten,
        }
        Ok(())
    }
}

schema!(struct pub ReceiptProjection pub fields; receipt: ApplicationReceipt, status: u8, provider_session: Range<usize>, provider_turn: Range<usize>);
schema!(struct pub PolicyStatus derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; owns_lease: bool, viewers: bool, lease_epoch: u32, semantic_flags: u8, semantic_pending: u16, query_available: bool, replay: ReplayDescriptor);
schema!(struct pub OutputRecord derive [Clone, Debug, Eq, PartialEq] pub fields; sequence: u64, offset: u64, bytes: Arc<[u8]>);
schema!(enum pub Reply [Clone, Debug, Eq, PartialEq]; Lease(LeaseResult), Input(Vec<u8>), Notice(InputNotice), NoticeCancel(ApplicationReceipt), SemanticAck(SemanticAck), SemanticRefused(Option<SemanticEvent>, SemanticRefusal), SemanticHello(SemanticHelloAck), ControllerError(u16, &'static [u8]), Termination(u8, u8, u8, &'static [u8]));
schema!(enum pub Effect; Send(ConnId, Reply), LeaseReply(ConnId, LeaseResult), Attached(ConnId, bool, Option<LeaseResult>, Option<(u16, u16)>), Resize(ConnId, u16, u16), Redraw(ConnId, u16, u16), Write(WriteTicket, Vec<u8>), CommitSources(CommitTicket, Vec<SemanticChange>, bool), CommitSemantic(CommitTicket, Vec<u8>, u32, [u8; 16], SemanticEvent, Option<ReceiptProjection>), QuerySend(ConnId, Query), Output(Option<ConnId>, OutputRecord), Gap(ConnId, u64), OutputExhausted, Terminate(bool), ReportTermination(ConnId), Flush(ConnId, u64), Close(ConnId), Replaced(ConnId));
schema!(enum pub Completion; Write(u64, Option<u16>), Sources(bool), Semantic(Result<EventPosition, SemanticRefusal>));
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayPolicy {
    Retained,
    LiveOnly,
}

schema!(enum pub Request<'a>; Attach(u16, u16, bool, bool, ReplayPolicy, Option<[u8; 16]>), Lease(LeaseRequest, Option<[u8; 16]>), Release(u32, [u8; 16]), Keepalive(u32, [u8; 16]), Resize(u32, u16, u16), Redraw(u32, u16, u16), Input(OwnedInput, Option<ApplicationInput>), NoticeAck(InputNoticeAck), SemanticHello(SemanticHello), SemanticEvent(SemanticEvent, Option<ReceiptProjection>), SemanticHeartbeat, QueryReply(u64, u32, u8, &'a [u8]), OutputAck(u64), Terminate(&'a [u8], u32, [u8; 16], bool));
schema!(enum pub Transition<'a>; Peer(u64, ConnId, Request<'a>), Complete(u64, CommitTicket, Completion), Query(u64, Arc<[u8]>, QueryShape, Option<Vec<u8>>), Output(u64, Vec<u8>), Shutdown(u64, bool), TerminationApplied(u8, bool), ReportTermination(ConnId), Retired(bool, bool), Tick(u64), Disconnect(ConnId), Writable(bool), Ending);
pub type Effects = SmallVec<[Effect; 4]>;

schema!(enum Peer; Controller(bool, bool), Semantic(Arc<[u8]>));
schema!(enum Pending; Input(ConnId, OwnedInput), Application([u8; 16], InputPending), Semantic(Arc<[u8]>, SemanticEvent, Option<[u8; 16]>), Sources, Hello(Box<PendingHello>), Ack(Arc<[u8]>, SemanticAck));
schema!(struct PendingQuery fields; conn: ConnId, correlation: u64, epoch: u32, shape: QueryShape, fallback: Option<Vec<u8>>, deadline: u64);
schema!(struct Termination fields; peer: Option<ConnId>, started: u64, containment: u8, method: u8, expired: bool);
impl Termination {
    fn reply(&self, outcome: u8, message: &'static [u8]) -> Reply {
        Reply::Termination(outcome, self.containment, self.method, message)
    }
}
schema!(enum SourceTrigger [Clone, Copy]; Timeout(u64), Closed(ConnId), Ending);
schema!(struct default pub Machine fields; generation: u32 = 0, incarnation: [u8; 16] = [0; 16], allocated: u32 = 0, lease: Lease = Lease::default(), semantic_token: [u8; 16] = [0; 16], sources: BTreeMap<Arc<[u8]>, Source> = BTreeMap::new(), applications: BTreeMap<[u8; 16], Application> = BTreeMap::new(), writable: bool = true, peers: BTreeMap<ConnId, Peer> = BTreeMap::new(), pending: HashMap<Ticket, Pending> = HashMap::new(), next_ticket: u64 = 1, queries: VecDeque<PendingQuery> = VecDeque::new(), query_next: u64 = 1, query_exhaustion_pending: bool = false, replay: VecDeque<OutputRecord> = VecDeque::new(), replay_limit: u64 = u64::MAX, next_sequence: u64 = 1, next_offset: u64 = 0, lost: u64 = 0, identity: Vec<u8> = Vec::new(), termination: Option<Termination> = None, effects: Effects = Effects::new(), lease_undo: Option<(ConnId, LeaseUndo)> = None);

impl Machine {
    fn send(&mut self, conn: ConnId, reply: Reply) {
        self.effects.push(Effect::Send(conn, reply));
    }

    fn refuse_semantic(
        &mut self,
        conn: ConnId,
        event: Option<SemanticEvent>,
        error: SemanticRefusal,
    ) {
        self.send(conn, Reply::SemanticRefused(event, error));
    }

    fn semantic_storage_failed(&mut self, conn: ConnId) {
        self.refuse_semantic(conn, None, SemanticRefusal::ResourceExhausted);
        self.set_writable(false);
    }

    pub fn new(generation: u32, incarnation: [u8; 16], semantic_token: [u8; 16]) -> Self {
        Self {
            generation,
            incarnation,
            semantic_token,
            ..Self::default()
        }
    }

    pub fn configure(&mut self, identity: Vec<u8>, limit: usize) {
        self.identity = identity;
        self.replay_limit = limit.try_into().unwrap_or(u64::MAX);
    }

    pub fn register_controller(&mut self, conn: ConnId) {
        self.peers.insert(conn, Peer::Controller(false, false));
    }

    pub fn phase(&self, conn: ConnId) -> Option<Phase> {
        let Peer::Controller(attached, _) = self.peers.get(&conn)? else {
            return None;
        };
        Some(
            match (*attached, self.lease.owner == Some(conn), self.lease.role) {
                (true, true, _) => Phase::Viewer,
                (true, false, _) => Phase::Observer,
                (false, true, LeaseRole::InputOnly) => Phase::InputOnly,
                (false, true, LeaseRole::Viewer) => Phase::Resumed,
                _ => Phase::Unattached,
            },
        )
    }

    pub fn attached(&self, conn: ConnId) -> bool {
        matches!(self.peers.get(&conn), Some(Peer::Controller(true, _)))
    }

    pub fn legal(&self, conn: ConnId, kind: u8) -> bool {
        self.phase(conn)
            .is_some_and(|phase| legal_in_phase(phase, kind))
    }

    fn lease_refusal(&self, role: LeaseRole, reason: ResultReason) -> LeaseResult {
        LeaseResult {
            reason,
            ..LeaseResult::success(ResultOutcome::Refused, role, self.allocated, [0; 16])
        }
    }

    fn expire_lease(&mut self, now: u64) {
        return_if!(self.lease.epoch == 0 || now < self.lease.deadline);
        if let Some(conn) = std::mem::take(&mut self.lease).owner {
            self.queries_gone(conn);
        }
    }

    fn request_lease(
        &mut self,
        conn: ConnId,
        request: &LeaseRequest,
        now: u64,
        token: Option<[u8; 16]>,
        transactional: bool,
    ) -> LeaseResult {
        self.expire_lease(now);
        let fresh = request.operation == LeaseOperation::Fresh;
        let (outcome, epoch, reason) = if fresh {
            let reason = (self.lease.epoch != 0)
                .then_some(ResultReason::Busy)
                .or((request.epoch != 0).then_some(ResultReason::BadEpoch))
                .or((request.incarnation != [0; 16]).then_some(ResultReason::BadIncarnation))
                .or((request.token != [0; 16]).then_some(ResultReason::BadToken));
            (
                ResultOutcome::Granted,
                self.allocated.checked_add(1).unwrap_or(0),
                reason,
            )
        } else {
            let lease = &self.lease;
            let reason = (lease.epoch == 0)
                .then_some(ResultReason::NotHeld)
                .or(lease.owner.is_some().then_some(ResultReason::Busy))
                .or((request.incarnation != self.incarnation)
                    .then_some(ResultReason::BadIncarnation))
                .or((request.epoch != lease.epoch).then_some(ResultReason::BadEpoch))
                .or((request.role != lease.role).then_some(ResultReason::BadRole))
                .or((request.token != lease.token).then_some(ResultReason::BadToken));
            (ResultOutcome::Resumed, lease.epoch, reason)
        };
        if let Some(reason) = reason {
            return self.lease_refusal(request.role, reason);
        }
        let Some(token) = token.filter(|value| *value != [0; 16] && epoch != 0) else {
            return self.lease_refusal(request.role, ResultReason::Exhausted);
        };
        if fresh {
            let allocated = self.allocated;
            self.allocated = epoch;
            let prior = std::mem::replace(
                &mut self.lease,
                Lease {
                    owner: Some(conn),
                    role: request.role,
                    epoch,
                    token,
                    deadline: now.saturating_add(10_000),
                    ..Lease::default()
                },
            );
            if transactional {
                self.lease_undo = Some((conn, LeaseUndo::Fresh { prior, allocated }));
            }
        } else {
            if transactional {
                self.lease_undo = Some((
                    conn,
                    LeaseUndo::Resume {
                        owner: self.lease.owner,
                        token: self.lease.token,
                        deadline: self.lease.deadline,
                    },
                ));
            }
            (self.lease.owner, self.lease.token) = (Some(conn), token);
            self.lease.deadline = now.saturating_add(10_000);
        }
        LeaseResult::success(outcome, request.role, epoch, token)
    }

    pub fn lease_reply_delivered(&mut self, conn: ConnId) {
        if self
            .lease_undo
            .as_ref()
            .is_some_and(|(pending, _)| *pending == conn)
        {
            self.lease_undo = None;
        }
    }

    pub fn lease_reply_failed(&mut self, conn: ConnId) {
        let Some((pending, undo)) = self.lease_undo.take_if(|(pending, _)| *pending == conn) else {
            return;
        };
        debug_assert_eq!(pending, conn);
        match undo {
            LeaseUndo::Fresh { prior, allocated } => {
                self.lease = prior;
                self.allocated = allocated;
            }
            LeaseUndo::Resume {
                owner,
                token,
                deadline,
            } => {
                (self.lease.owner, self.lease.token) = (owner, token);
                self.lease.deadline = deadline;
            }
        }
    }

    pub fn rollback_attach(&mut self, epoch: u32) {
        // A fresh grant made inside an attach transaction is PROVISIONAL until
        // the holder has delivered its token in the attach prefix. When any
        // earlier step fails — deadline, native resize, a prefix send — the
        // requester can never present the token, so a surviving reservation
        // would refuse every honest fresh controller until its deadline for a
        // lease nobody can use. The epoch identifies the exact grant: a stale
        // rollback of a superseded epoch changes nothing. The allocation
        // counter is restored too — no frame carrying this epoch ever left
        // the holder (the descriptor is part of the same failed prefix), so
        // the attach/grant transaction is atomic and an uncommitted grant may
        // not consume an epoch: the next fresh controller receives this very
        // number.
        return_if!(self.lease.epoch != epoch);
        if self.allocated == epoch {
            self.allocated = epoch - 1;
        }
        if let Some(conn) = std::mem::take(&mut self.lease).owner {
            self.queries_gone(conn);
        }
    }

    fn release_lease(&mut self, conn: ConnId, epoch: u32, token: [u8; 16]) -> LeaseResult {
        let role = match self.lease.epoch {
            0 => LeaseRole::Viewer,
            _ => self.lease.role,
        };
        if self.lease.owner == Some(conn) && self.lease.epoch == epoch && self.lease.token == token
        {
            self.lease = Lease::default();
            self.queries_gone(conn);
            LeaseResult::success(ResultOutcome::Released, role, epoch, [0; 16])
        } else {
            self.lease_refusal(role, ResultReason::NotHeld)
        }
    }

    fn touch_lease(&mut self, conn: ConnId, epoch: u32, token: Option<[u8; 16]>, now: u64) -> bool {
        let lease = &mut self.lease;
        return_if!(lease.owner != Some(conn) || lease.epoch != epoch, false);
        return_if!(token.is_some_and(|token| lease.token != token), false);
        lease.deadline = now.saturating_add(10_000);
        true
    }

    fn geometry(&mut self, conn: ConnId, columns: u16, rows: u16) -> bool {
        let (code, diagnostic) = if (columns == 0) != (rows == 0) {
            (14, b"geometry was half specified".as_slice())
        } else if columns > GEOMETRY_LIMIT
            || rows > GEOMETRY_LIMIT
            || u32::from(columns) * u32::from(rows) > GEOMETRY_CELLS
        {
            (5, b"geometry exceeded its valid range".as_slice())
        } else {
            return true;
        };
        self.send(conn, Reply::ControllerError(code, diagnostic));
        self.effects.push(Effect::Close(conn));
        false
    }

    pub fn query_owner(&self) -> Option<(ConnId, u32)> {
        return_if!(self.query_next == 0 && !self.query_exhaustion_pending, None);
        let owner = self.lease.owner?;
        matches!(self.peers.get(&owner), Some(Peer::Controller(true, false)))
            .then_some((owner, self.lease.epoch))
    }

    fn update_sources(&mut self, trigger: SourceTrigger, now: Option<u64>) {
        let (status, reason) = match trigger {
            SourceTrigger::Timeout(_) => (SourceStatus::Degraded, SourceReason::HeartbeatTimeout),
            SourceTrigger::Closed(_) => (SourceStatus::Disconnected, SourceReason::TransportClosed),
            SourceTrigger::Ending => (SourceStatus::Disconnected, SourceReason::SessionEnding),
        };
        let mut changes: Vec<_> = self
            .sources
            .iter_mut()
            .filter(|(_, source)| match trigger {
                SourceTrigger::Timeout(at) => source.timed_out(at),
                SourceTrigger::Closed(conn) => source.binding.conn == conn,
                SourceTrigger::Ending => source.stateful() && source.active(),
            })
            .filter_map(|(name, source)| source.transition(name, status, reason))
            .collect();
        self.sweep_applications(now, &mut changes);
        self.persist_sources(changes);
    }

    fn sweep_applications(&mut self, now: Option<u64>, changes: &mut Vec<SemanticChange>) {
        let sources = &self.sources;
        self.applications.retain(|_, value| {
            return_if!(!value.written(), true);
            let current = value.current(sources.get(&value.source));
            if !current && let Some(effect) = value.emit_once(2, MissingReason::SourceLost) {
                changes.push(effect);
            }
            let Some(now) = now else { return true };
            if now >= value.deadline
                && let Some(effect) = value.emit_once(1, MissingReason::Deadline)
            {
                changes.push(effect);
            }
            return_if!(now < value.deadline.saturating_add(540_000), true);
            changes.push(value.effect(MissingReason::RetentionExpired));
            false
        });
    }

    fn semantic_flags(&self) -> u8 {
        self.sources
            .values()
            .filter(|source| source.stateful())
            .fold(0, |flags, source| {
                flags
                    | match (source.exact(), source.status) {
                        (true, _) => 1 | u8::from(source.capabilities & 6 == 6) << 2,
                        (false, SourceStatus::Degraded | SourceStatus::Disconnected) => 2,
                        _ => 0,
                    }
            })
    }

    pub fn status(&self, conn: ConnId) -> PolicyStatus {
        let range = self.replay.front().zip(self.replay.back());
        PolicyStatus {
            owns_lease: self.lease.owner == Some(conn),
            viewers: self
                .peers
                .values()
                .any(|peer| matches!(peer, Peer::Controller(true, _))),
            lease_epoch: self.allocated,
            semantic_flags: self.semantic_flags(),
            semantic_pending: self
                .applications
                .values()
                .filter(|value| value.written())
                .count() as u16,
            query_available: self.query_next != 0,
            replay: ReplayDescriptor {
                first: range.map_or(self.next_sequence, |records| records.0.sequence),
                last: range.map_or(self.lost, |records| records.1.sequence),
                start: range.map_or(self.next_offset, |records| records.0.offset),
                end: self.next_offset,
                complete: self.lost == 0,
                modes_exact: false,
            },
        }
    }

    pub fn output_end(&self) -> u64 {
        self.next_offset
    }

    pub fn termination_expired(&self) -> bool {
        self.termination.as_ref().is_some_and(|state| state.expired)
    }

    pub fn termination_started(&self) -> Option<u64> {
        self.termination.as_ref().map(|state| state.started)
    }

    pub fn termination_forced(&self) -> Option<bool> {
        self.termination.as_ref().map(|state| state.method == 2)
    }

    fn terminate(&mut self, peer: Option<ConnId>, now: u64, force: bool) {
        if self.termination.is_none() {
            self.termination = Some(Termination {
                peer,
                started: now,
                containment: 0,
                method: if force { 2 } else { 1 },
                expired: false,
            });
            self.effects.push(Effect::Terminate(force));
            return;
        }
        let state = self.termination.as_mut().unwrap();
        if let Some((peer, _)) = peer.zip(state.peer).filter(|(left, right)| left != right) {
            let reply = Reply::Termination(4, 0, 0, b"termination is already in progress");
            self.send(peer, reply);
            return;
        }
        state.peer = state.peer.or(peer);
        if force && state.method == 1 {
            state.method = 2;
            state.containment |= 4;
            self.effects.push(Effect::Terminate(true));
        }
    }

    fn exhaust_output(&mut self, now: u64) {
        self.next_sequence = 0;
        self.effects.push(Effect::OutputExhausted);
        self.terminate(None, now, true);
    }

    fn output(&mut self, now: u64, bytes: Vec<u8>) {
        let sequence = self.next_sequence;
        let Some(end) = self.next_offset.checked_add(bytes.len() as u64) else {
            return self.exhaust_output(now);
        };
        return_if!(sequence == 0);
        let record = OutputRecord {
            sequence,
            offset: self.next_offset,
            bytes: bytes.into(),
        };
        self.next_sequence = sequence.wrapping_add(1);
        self.next_offset = end;
        self.replay.push_back(record.clone());
        while self
            .replay
            .front()
            .is_some_and(|first| end - first.offset > self.replay_limit)
        {
            let dropped = self.replay.pop_front().unwrap();
            self.lost = dropped.sequence;
        }
        self.effects.push(Effect::Output(None, record));
        if self.next_sequence == 0 {
            self.exhaust_output(now);
        }
    }

    fn receipt(&self, input: &OwnedInput, written: u64, error: Option<u16>) -> [u8; 43] {
        InputReceipt::outcome(
            input.epoch,
            input.request_id,
            self.generation,
            self.incarnation,
            written,
            error,
        )
        .encode()
        .expect("valid input receipt")
    }

    fn refuse_input(&mut self, conn: ConnId, input: OwnedInput, code: u16) {
        let receipt = self.receipt(&input, 0, Some(code));
        self.finish_input(conn, input, receipt);
    }

    fn reject_input(&mut self, conn: ConnId, input: &OwnedInput, reason: ResultReason) {
        let code = [6, 6, 6, 6, 6, 15, 13, 6, 6][reason as usize];
        let receipt = self.receipt(input, 0, Some(code));
        self.send(conn, Reply::Input(receipt.into()));
    }

    fn finish_input(&mut self, conn: ConnId, input: OwnedInput, receipt: [u8; 43]) {
        let lease = &mut self.lease;
        if lease.inflight.as_ref() != Some(&input) {
            return self.effects.push(Effect::Close(conn));
        }
        lease.inflight = None;
        lease.cached = Some((input, receipt));
        if let Some(owner) = lease.owner {
            self.send(owner, Reply::Input(receipt.into()));
        }
    }

    fn admit(&mut self, pending: Pending) -> Result<Ticket, Pending> {
        let Some(ticket) = Ticket::from_raw(self.next_ticket) else {
            return Err(pending);
        };
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.pending.insert(ticket, pending);
        Ok(ticket)
    }

    fn queue_write(&mut self, pending: Pending, bytes: Vec<u8>) {
        match self.admit(pending) {
            Ok(ticket) => self.effects.push(Effect::Write(ticket, bytes)),
            Err(Pending::Input(conn, input)) => self.refuse_input(conn, input, 13),
            Err(Pending::Application(id, pending)) => {
                self.applications.remove(&id);
                self.refuse_input(pending.controller, pending.input, 13);
            }
            Err(_) => unreachable!(),
        };
    }

    /// `mandatory` marks a holder observation the protocol cannot refuse — a
    /// source degrading or disconnecting really happened, and §8.4.4 reserves
    /// storage for it. Overflowing the queue with one closes the store
    /// (closure §5.7); a rejectable producer request is refused instead.
    fn commit_sources(
        &mut self,
        pending: Pending,
        changes: Vec<SemanticChange>,
        mandatory: bool,
    ) -> Result<(), Pending> {
        let ticket = self.admit(pending)?;
        self.effects
            .push(Effect::CommitSources(ticket, changes, mandatory));
        Ok(())
    }

    fn persist_sources(&mut self, changes: Vec<SemanticChange>) {
        return_if!(changes.is_empty());
        if self
            .commit_sources(Pending::Sources, changes, true)
            .is_err()
        {
            self.set_writable(false);
        }
    }

    fn input(
        &mut self,
        conn: ConnId,
        now: u64,
        input: OwnedInput,
        application: Option<ApplicationInput>,
    ) {
        self.expire_lease(now);
        let lease = &mut self.lease;
        if lease.owner != Some(conn) || lease.epoch != input.epoch {
            return self.reject_input(conn, &input, ResultReason::NotHeld);
        }
        if let Some(prior) = &lease.inflight {
            if prior == &input {
                lease.deadline = now.saturating_add(10_000);
                return;
            }
            return self.reject_input(conn, &input, ResultReason::BadSequence);
        }
        if let Some(receipt) = lease
            .cached
            .as_ref()
            .filter(|(prior, _)| prior.request_id != 0 && prior == &input)
            .map(|(_, receipt)| *receipt)
        {
            lease.deadline = now.saturating_add(10_000);
            return self.send(conn, Reply::Input(receipt.into()));
        }
        let high = lease
            .cached
            .as_ref()
            .map_or(0, |(prior, _)| prior.request_id);
        let Some(next) = high.checked_add(1) else {
            return self.reject_input(conn, &input, ResultReason::Exhausted);
        };
        if input.request_id != next {
            return self.reject_input(conn, &input, ResultReason::BadSequence);
        }
        lease.deadline = now.saturating_add(10_000);
        lease.inflight = Some(input.clone());
        let Some(application) = application else {
            let bytes = input.exact_payload[13..].to_vec();
            return self.queue_write(Pending::Input(conn, input), bytes);
        };
        let receipt = application.receipt;
        if self.lease.cached.as_ref().is_some_and(|(prior, _)| {
            prior != &input && prior.application_id() == Some(receipt.application_id)
        }) {
            return self.refuse_input(conn, input, 18);
        }
        let valid_bounds = application.source.end <= application.terminal_at;
        let source_bytes = input
            .exact_payload
            .get(application.source)
            .filter(|source| valid_bounds && valid_source_id(source));
        let terminal = input.exact_payload.get(application.terminal_at..);
        let Some((source_bytes, terminal)) = source_bytes.zip(terminal) else {
            return self.refuse_input(conn, input, 17);
        };
        let (byte_count, digest) = (terminal.len() as u64, Sha256::digest(terminal).into());
        let Some((source, state)) = self
            .sources
            .get_key_value(source_bytes)
            .filter(|(_, source)| source.exact() && source.capabilities & 6 == 6)
            .filter(|(_, source)| source.pending != ACK_PENDING)
        else {
            return self.refuse_input(conn, input, 17);
        };
        let (source, binding) = (source.clone(), state.binding);
        if self.applications.contains_key(&receipt.application_id) {
            return self.refuse_input(conn, input, 18);
        }
        if self.applications.len() >= 512 {
            return self.refuse_input(conn, input, 13);
        }
        let notice = InputNotice {
            receipt,
            byte_count,
            digest,
        };
        self.applications.insert(
            receipt.application_id,
            Application {
                receipt,
                source,
                binding,
                state: AppState::Notice(InputPending {
                    controller: conn,
                    input,
                    expected: byte_count,
                    terminal_at: application.terminal_at,
                }),
                deadline: now.saturating_add(2_000),
                emitted: 0,
            },
        );
        self.send(binding.conn, Reply::Notice(notice));
    }

    fn notice_ack(&mut self, conn: ConnId, now: u64, ack: InputNoticeAck) {
        let receipt = ack.receipt;
        let application_id = receipt.application_id;
        let Some(value) = self
            .applications
            .get(&application_id)
            .filter(|value| matches!(value.state, AppState::Notice(_)))
        else {
            return self.refuse_semantic(conn, None, SemanticRefusal::UnknownApplication);
        };
        if value.binding.conn != conn || value.receipt != receipt {
            return self.refuse_semantic(conn, None, SemanticRefusal::SourceUnavailable);
        }
        let expired = now >= value.deadline;
        let permitted = !expired && value.current(self.sources.get(&value.source)) && ack.prepared;
        if !permitted {
            let AppState::Notice(pending) =
                self.applications.remove(&application_id).unwrap().state
            else {
                unreachable!()
            };
            let code = if expired { 19 } else { 17 };
            return self.refuse_input(pending.controller, pending.input, code);
        }
        let value = self.applications.get_mut(&application_id).unwrap();
        let AppState::Notice(pending) = std::mem::replace(&mut value.state, AppState::Writing)
        else {
            unreachable!()
        };
        let bytes = pending.input.exact_payload[pending.terminal_at..].to_vec();
        self.queue_write(Pending::Application(application_id, pending), bytes);
    }

    fn cancel_notice(&mut self, code: u16, selected: impl Fn(&Application) -> bool) {
        let removed = self
            .applications
            .extract_if(.., |_, application| {
                matches!(application.state, AppState::Notice(_)) && selected(application)
            })
            .next();
        if let Some((_, Application { state, .. })) = removed
            && let AppState::Notice(pending) = state
        {
            self.refuse_input(pending.controller, pending.input, code);
        }
    }

    fn finish_hello(&mut self, mut pending: PendingHello, now: u64, success: bool) {
        let conn = pending.source.binding.conn;
        if let Some(source) = self.sources.get_mut(&pending.name) {
            source.pending = 0;
        }
        if !success {
            self.peers.remove(&conn);
            self.refuse_semantic(conn, None, SemanticRefusal::ResourceExhausted);
            return self.effects.push(Effect::Close(conn));
        }
        let ack = SemanticHelloAck {
            epoch: pending.source.binding.epoch,
            snapshot_required: pending.source.stateful(),
        };
        pending.source.last_seen = now;
        self.sources.insert(pending.name.clone(), pending.source);
        // Only the ids this committed batch actually carried may be marked
        // reported. A write that completed while the batch was in flight was
        // never part of it, so suppressing it here would drop its source-lost
        // record permanently (§10.3.4).
        for id in &pending.missing {
            if let Some(application) = self.applications.get_mut(id) {
                application.emitted |= 2;
            }
        }
        if let Some(old) = pending.superseded.filter(|old| *old != conn) {
            self.peers.remove(&old);
            self.cancel_notice(17, |application| application.binding.conn == old);
            self.effects.push(Effect::Replaced(old));
        }
        self.peers.insert(conn, Peer::Semantic(pending.name));
        self.send(conn, Reply::SemanticHello(ack));
    }

    fn fallback(&mut self, fallback: Option<Vec<u8>>) {
        self.effects
            .extend(fallback.map(|bytes| Effect::Write(Ticket(0), bytes)));
    }

    fn queries_gone(&mut self, conn: ConnId) {
        return_if!(self.queries.front().is_none_or(|query| query.conn != conn));
        for query in std::mem::take(&mut self.queries) {
            self.fallback(query.fallback);
        }
    }

    fn query(&mut self, now: u64, raw: Arc<[u8]>, shape: QueryShape, fallback: Option<Vec<u8>>) {
        let Some((owner, epoch)) = self.query_owner() else {
            return self.fallback(fallback);
        };
        let correlation = self.query_next;
        let overloaded = self.queries.len() == 64;
        if overloaded || correlation == 0 {
            if !overloaded {
                self.query_exhaustion_pending = false;
                let error = Reply::ControllerError(13, b"query correlation exhausted");
                self.send(owner, error);
            }
            self.effects.push(Effect::Close(owner));
            self.queries_gone(owner);
            return self.fallback(fallback);
        }
        self.query_next = correlation.wrapping_add(1);
        self.query_exhaustion_pending = self.query_next == 0;
        self.queries.push_back(PendingQuery {
            conn: owner,
            correlation,
            epoch,
            shape,
            fallback,
            deadline: now.saturating_add(250),
        });
        self.effects.push(Effect::QuerySend(
            owner,
            Query {
                correlation,
                epoch,
                class: shape.class,
                bytes: raw.to_vec(),
            },
        ));
    }

    fn query_reply(&mut self, conn: ConnId, now: u64, reply: (u64, u32, u8, &[u8])) {
        let Some(index) = self
            .queries
            .iter()
            .position(|query| query.conn == conn && query.correlation == reply.0)
        else {
            return;
        };
        let query = &self.queries[index];
        return_if!(
            query.epoch != reply.1
                || query.shape.class != reply.2
                || !validate_query_reply(&query.shape, reply.3)
        );
        let expired = now >= query.deadline;
        return_if!(!expired && !self.touch_lease(conn, query.epoch, None, now));
        let fallback = self.queries.remove(index).unwrap().fallback;
        self.fallback((!expired).then(|| reply.3.to_vec()).or(fallback));
    }

    fn semantic_hello(&mut self, conn: ConnId, now: u64, hello: SemanticHello) -> SemResult {
        use SemanticRefusal::*;
        reject! {
            self.semantic_token == [0; 16] => CapabilityAbsent,
            !self.writable => ResourceExhausted,
            hello.token != self.semantic_token => StaleToken,
                hello.generation != self.generation
                    && !(self.generation == 1 && hello.generation == 0) => Generation,
            hello.capabilities & !7 != 0 || !valid_source_id(&hello.source) => InvalidPayload,
        }
        let mut pending_sources = 0;
        for pending in self.pending.values() {
            let Pending::Hello(pending) = pending else {
                continue;
            };
            reject! {
                pending.source.binding.conn == conn
                    || pending.name.as_ref() == hello.source.as_ref() => ResourceExhausted,
            }
            pending_sources += usize::from(!self.sources.contains_key(pending.name.as_ref()));
        }
        let prior = self.sources.get(hello.source.as_ref());
        let epoch = match prior {
            Some(source) if source.mode != hello.mode => return Err(SourceConflict),
            Some(source) if source.pending != 0 => return Err(ResourceExhausted),
            Some(source) => source
                .binding
                .epoch
                .checked_add(1)
                .ok_or(ResourceExhausted)?,
            None if self.sources.len() + pending_sources >= 64 => return Err(ResourceExhausted),
            None => 1,
        };
        let binding = Binding {
            conn,
            epoch,
            producer: hello.producer,
        };
        let superseded = prior
            .filter(|source| source.active())
            .map(|source| source.binding.conn);
        let mut changes = Vec::with_capacity(2);
        if let Some(source) = prior.filter(|source| source.stateful() && source.active()) {
            changes.push(source.binding.change(
                &hello.source,
                SourceStatus::Disconnected,
                SourceReason::Superseded,
            ));
        }
        let mut missing = Vec::new();
        for value in self.applications.values() {
            if Some(value.binding.conn) == superseded && value.written() && value.emitted & 2 == 0 {
                changes.push(value.effect(MissingReason::SourceLost));
                missing.push(value.receipt.application_id);
            }
        }
        if hello.mode == SemanticMode::Stateful {
            changes.push(binding.change(
                &hello.source,
                SourceStatus::Connected,
                SourceReason::None,
            ));
        }
        let pending = PendingHello {
            name: hello.source,
            missing,
            source: Source {
                binding,
                mode: hello.mode,
                capabilities: hello.capabilities,
                ..Source::default()
            },
            superseded,
        };
        self.peers
            .insert(conn, Peer::Semantic(pending.name.clone()));
        if changes.is_empty() {
            self.finish_hello(pending, now, true);
            return Ok(());
        }
        if let Some(source) = self.sources.get_mut(&pending.name) {
            source.pending = COMMIT_PENDING;
        }
        if let Err(Pending::Hello(pending)) =
            self.commit_sources(Pending::Hello(Box::new(pending)), changes, false)
        {
            self.finish_hello(*pending, now, false);
            self.set_writable(false);
        }
        Ok(())
    }

    fn semantic_event(
        &mut self,
        conn: ConnId,
        event: SemanticEvent,
        projection: Option<ReceiptProjection>,
    ) {
        use SemanticRefusal::*;
        let Some(Peer::Semantic(name)) = self.peers.get(&conn) else {
            return self.refuse_semantic(conn, Some(event), Superseded);
        };
        let name = name.clone();
        let Some(source) = self
            .sources
            .get(&name)
            .filter(|source| source.binding.conn == conn && source.active())
        else {
            return self.refuse_semantic(conn, Some(event), Superseded);
        };
        if source.pending == ACK_PENDING {
            self.refuse_semantic(conn, Some(event), BadSequence);
            return self.effects.push(Effect::Close(conn));
        }
        let binding = source.binding;
        let receipt = projection.as_ref().map(|value| value.receipt);
        let duplicate = match source.admit(&event, receipt, self.writable, |receipt| {
            self.applications
                .get(&receipt.application_id)
                .ok_or(UnknownApplication)?
                .status(conn, receipt)
        }) {
            Ok(duplicate) => duplicate,
            Err(error) => return self.refuse_semantic(conn, Some(event), error),
        };
        if let Some(ack) = duplicate {
            return self.send(conn, Reply::SemanticAck(ack));
        }
        let pending = Pending::Semantic(
            name.clone(),
            event.clone(),
            receipt.map(|receipt| receipt.application_id),
        );
        let Ok(ticket) = self.admit(pending) else {
            return self.refuse_semantic(conn, Some(event), ResourceExhausted);
        };
        self.sources.get_mut(&name).unwrap().pending = COMMIT_PENDING;
        self.effects.push(Effect::CommitSemantic(
            ticket,
            name.to_vec(),
            binding.epoch,
            binding.producer,
            event,
            projection,
        ));
    }

    fn complete(&mut self, ticket: Ticket, now: u64, completion: Completion) {
        let Some(pending) = self.pending.remove(&ticket) else {
            return;
        };
        match (pending, completion) {
            (Pending::Input(conn, input), Completion::Write(written, error)) => {
                let receipt = self.receipt(&input, written, error);
                self.finish_input(conn, input, receipt);
            }
            (Pending::Application(id, pending), Completion::Write(written, error)) => {
                let mut application = self.applications.remove(&id).expect("pending application");
                debug_assert!(matches!(application.state, AppState::Writing));
                application.state = AppState::Written;
                let failure = (error.is_some() || written != pending.expected).then_some(20);
                if failure.is_none() {
                    application.deadline = now.saturating_add(60_000);
                    self.applications.insert(id, application);
                } else {
                    self.send(
                        application.binding.conn,
                        Reply::NoticeCancel(application.receipt),
                    );
                }
                let receipt = self.receipt(&pending.input, written, failure);
                self.finish_input(pending.controller, pending.input, receipt);
            }
            (Pending::Hello(hello), Completion::Sources(success)) => {
                self.finish_hello(*hello, now, success);
            }
            (Pending::Ack(name, ack), Completion::Sources(success)) => {
                let source = self.sources.get_mut(&name).expect("pending source");
                source.pending = 0;
                let conn = source.binding.conn;
                if success {
                    self.send(conn, Reply::SemanticAck(ack));
                } else {
                    self.semantic_storage_failed(conn);
                }
            }
            (Pending::Sources, Completion::Sources(success)) => {
                if !success {
                    self.set_writable(false);
                }
            }
            (Pending::Semantic(name, event, application), Completion::Semantic(position)) => {
                let source = self.sources.get_mut(&name).expect("pending source");
                let binding = source.binding;
                source.pending = 0;
                let position = match position {
                    Ok(position) => position,
                    Err(error) => {
                        self.refuse_semantic(binding.conn, Some(event), error);
                        return;
                    }
                };
                let change = (event.kind == SemanticEventKind::Snapshot && source.active())
                    .then(|| source.transition(&name, SourceStatus::Exact, SourceReason::None))
                    .flatten();
                source.entries.push_back(Retained::new(&event, position));
                if source.entries.len() > 512 {
                    source.entries.pop_front();
                }
                if let Some(application) = application {
                    self.applications.remove(&application);
                }
                let ack = SemanticAck::at(&event, SemanticAckStatus::Accepted, position);
                if let Some(change) = change {
                    source.pending = ACK_PENDING;
                    if self
                        .commit_sources(Pending::Ack(name.clone(), ack), vec![change], false)
                        .is_err()
                    {
                        self.semantic_storage_failed(binding.conn);
                    }
                } else {
                    self.send(binding.conn, Reply::SemanticAck(ack));
                }
            }
            (pending, _) => {
                self.pending.insert(ticket, pending);
            }
        }
    }

    fn peer_request<'a>(
        &mut self,
        now: u64,
        conn: ConnId,
        request: Request<'a>,
    ) -> Result<(), WireError> {
        match request {
            Request::Attach(columns, rows, lease, non_vt, replay, token) => {
                return_if!(!self.geometry(conn, columns, rows), Ok(()));
                let phase = self.phase(conn).ok_or(WireError::Malformed)?;
                let resumed = phase == Phase::Resumed && !lease && self.lease.owner == Some(conn);
                require_policy(resumed || phase == Phase::Unattached && (columns == 0 || lease))?;
                let result = lease.then(|| {
                    self.request_lease(
                        conn,
                        &LeaseRequest::fresh(LeaseRole::Viewer),
                        now,
                        token,
                        false,
                    )
                });
                let owns = resumed
                    || matches!(&result, Some(value) if value.outcome == ResultOutcome::Granted);
                *self.peers.get_mut(&conn).unwrap() = Peer::Controller(true, non_vt);
                let resize = (columns != 0 && owns).then_some((rows, columns));
                self.effects
                    .push(Effect::Attached(conn, non_vt, result, resize));
                if replay == ReplayPolicy::Retained {
                    if self.lost != 0 {
                        self.effects.push(Effect::Gap(conn, self.lost));
                    }
                    self.effects.extend(
                        self.replay
                            .iter()
                            .cloned()
                            .map(|record| Effect::Output(Some(conn), record)),
                    );
                }
            }
            Request::Lease(request, token) => {
                let phase = self.phase(conn).ok_or(WireError::Malformed)?;
                let flags = request.operation as u8 | (request.role as u8) << 1;
                require_policy(next_phase(phase, 0x15, flags).is_some())?;
                let result = self.request_lease(conn, &request, now, token, true);
                self.effects.push(Effect::LeaseReply(conn, result));
            }
            Request::Release(epoch, token) => {
                self.expire_lease(now);
                let result = self.release_lease(conn, epoch, token);
                self.send(conn, Reply::Lease(result));
            }
            Request::Keepalive(epoch, token) => {
                self.expire_lease(now);
                if !self.touch_lease(conn, epoch, Some(token), now) {
                    self.send(conn, Reply::ControllerError(15, b"lease not held"));
                    self.effects.push(Effect::Close(conn));
                }
            }
            Request::Resize(epoch, columns, rows) => {
                return_if!(!self.geometry(conn, columns, rows), Ok(()));
                self.expire_lease(now);
                require_policy(self.touch_lease(conn, epoch, None, now))?;
                if columns != 0 {
                    self.effects.push(Effect::Resize(conn, rows, columns));
                }
            }
            Request::Redraw(epoch, columns, rows) => {
                return_if!(!self.geometry(conn, columns, rows), Ok(()));
                self.expire_lease(now);
                require_policy(self.touch_lease(conn, epoch, None, now))?;
                if columns != 0 {
                    self.effects.push(Effect::Redraw(conn, rows, columns));
                }
            }
            Request::Input(input, application) => self.input(conn, now, input, application),
            Request::NoticeAck(ack) => self.notice_ack(conn, now, ack),
            Request::SemanticHello(hello) => self
                .semantic_hello(conn, now, hello)
                .unwrap_or_else(|error| self.refuse_semantic(conn, None, error)),
            Request::SemanticEvent(event, projection) => {
                self.semantic_event(conn, event, projection)
            }
            Request::SemanticHeartbeat => {
                let Some(source) = (match self.peers.get(&conn) {
                    Some(Peer::Semantic(name)) => self.sources.get_mut(name),
                    _ => None,
                })
                .filter(|source| source.binding.conn == conn && source.active()) else {
                    self.refuse_semantic(conn, None, SemanticRefusal::Superseded);
                    return Ok(());
                };
                source.last_seen = now;
            }
            Request::QueryReply(correlation, epoch, class, bytes) => {
                self.query_reply(conn, now, (correlation, epoch, class, bytes))
            }
            Request::OutputAck(acknowledged) => {
                let high = self
                    .replay
                    .back()
                    .map_or(self.lost, |record| record.sequence);
                crate::wire::require(acknowledged <= high, WireError::BadSequence)?;
            }
            Request::Terminate(identity, generation, incarnation, force) => {
                if (identity, generation, incarnation)
                    != (&self.identity, self.generation, self.incarnation)
                {
                    let reply = Reply::Termination(2, 0, 0, b"session identity did not match");
                    self.send(conn, reply);
                } else {
                    self.terminate(Some(conn), now, force);
                }
            }
        }
        Ok(())
    }

    fn tick(&mut self, now: u64) {
        self.expire_lease(now);
        if let Some(state) = self.termination.as_mut() {
            if state.method == 1 && now >= state.started.saturating_add(5_000) {
                state.method = 2;
                state.containment |= 4;
                self.effects.push(Effect::Terminate(true));
            }
            if !state.expired && now >= state.started.saturating_add(10_000) {
                state.expired = true;
                self.effects
                    .extend(state.peer.take().map(Effect::ReportTermination));
            }
        }
        self.cancel_notice(19, |application| now >= application.deadline);
        self.update_sources(SourceTrigger::Timeout(now), Some(now));
        while self
            .queries
            .front()
            .is_some_and(|query| now >= query.deadline)
        {
            let query = self.queries.pop_front().unwrap();
            self.fallback(query.fallback);
        }
    }

    fn disconnect(&mut self, conn: ConnId) {
        let Some(peer) = self.peers.remove(&conn) else {
            return;
        };
        let Peer::Semantic(name) = peer else {
            self.lease.owner = self.lease.owner.filter(|owner| *owner != conn);
            return self.queries_gone(conn);
        };
        if self
            .sources
            .get(&name)
            .is_some_and(|source| source.binding.conn == conn)
        {
            self.update_sources(SourceTrigger::Closed(conn), None);
            self.cancel_notice(17, |application| application.binding.conn == conn);
        } else {
            self.pending.retain(
                |_, pending| !matches!(pending, Pending::Hello(hello) if hello.source.binding.conn == conn),
            );
        }
    }

    fn set_writable(&mut self, writable: bool) {
        let losing = std::mem::replace(&mut self.writable, writable) && !writable;
        return_if!(!losing);
        for (&conn, peer) in &self.peers {
            if matches!(peer, Peer::Semantic(..)) {
                self.effects.push(Effect::Send(
                    conn,
                    Reply::SemanticRefused(None, SemanticRefusal::ResourceExhausted),
                ));
                self.effects.push(Effect::Close(conn));
            }
        }
    }

    pub fn transition<'a>(&mut self, transition: Transition<'a>) -> Result<Effects, WireError> {
        debug_assert!(self.effects.is_empty());
        let result = (|| {
            match transition {
                Transition::Peer(now, conn, request) => self.peer_request(now, conn, request)?,
                Transition::Complete(now, ticket, completion) => {
                    self.complete(ticket, now, completion)
                }
                Transition::Query(now, raw, shape, fallback) => {
                    self.query(now, raw, shape, fallback)
                }
                Transition::Output(now, bytes) => self.output(now, bytes),
                Transition::Shutdown(now, force) => self.terminate(None, now, force),
                Transition::TerminationApplied(containment, forced) => {
                    if let Some(state) = self.termination.as_mut() {
                        state.containment |= containment;
                        state.method = if forced { 2 } else { state.method };
                    }
                }
                Transition::ReportTermination(conn) => {
                    if let Some(state) = self.termination.as_ref() {
                        let reply = state.reply(
                            3,
                            b"termination outcome was not established within 10 seconds",
                        );
                        self.send(conn, reply);
                    }
                }
                Transition::Retired(unlinked, survivor) => {
                    if let Some(mut state) = self.termination.take()
                        && !state.expired
                        && let Some(peer) = state.peer
                    {
                        state.containment |= u8::from(survivor) << 3;
                        let (outcome, message) = if unlinked {
                            (0, b"".as_slice())
                        } else {
                            (4, b"session retirement did not complete".as_slice())
                        };
                        self.send(peer, state.reply(outcome, message));
                        self.effects
                            .push(Effect::Flush(peer, state.started.saturating_add(10_000)));
                    }
                }
                Transition::Tick(now) => self.tick(now),
                Transition::Disconnect(conn) => self.disconnect(conn),
                Transition::Writable(writable) => self.set_writable(writable),
                Transition::Ending => self.update_sources(SourceTrigger::Ending, None),
            }
            Ok(())
        })();
        if result.is_err() {
            self.effects.clear();
        }
        result.map(|_| std::mem::take(&mut self.effects))
    }
}

fn require_policy(valid: bool) -> Result<(), WireError> {
    valid.then_some(()).ok_or(WireError::Malformed)
}

#[cfg(test)]
include!("../tests/unit/session.rs");
