use crate::wire::{Query, WireError, recognize_query, validate_query_reply};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};

pub type ConnId = u64;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseOperation {
    Fresh,
    Resume,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseRole {
    Viewer,
    InputOnly,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultOutcome {
    Granted,
    Resumed,
    Released,
    Refused,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultReason {
    None,
    Busy,
    BadEpoch,
    BadToken,
    BadRole,
    NotHeld,
    Exhausted,
    BadIncarnation,
    BadSequence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseRequest {
    pub operation: LeaseOperation,
    pub role: LeaseRole,
    pub epoch: u32,
    pub incarnation: [u8; 16],
    pub token: [u8; 16],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseResult {
    pub outcome: ResultOutcome,
    pub reason: ResultReason,
    pub role: LeaseRole,
    pub epoch: u32,
    pub token: [u8; 16],
}

impl LeaseRequest {
    pub fn encode_wire(&self) -> Result<[u8; 40], WireError> {
        let mut out = [0; 40];
        out[0] = self.operation as u8;
        out[1] = self.role as u8;
        out[4..8].copy_from_slice(&self.epoch.to_le_bytes());
        out[8..24].copy_from_slice(&self.incarnation);
        out[24..].copy_from_slice(&self.token);
        crate::wire::validate_payload(0x15, &out)?;
        Ok(out)
    }
    pub fn decode_wire(bytes: &[u8]) -> Result<Self, WireError> {
        crate::wire::validate_payload(0x15, bytes)?;
        Ok(Self {
            operation: [LeaseOperation::Fresh, LeaseOperation::Resume][bytes[0] as usize],
            role: [LeaseRole::Viewer, LeaseRole::InputOnly][bytes[1] as usize],
            epoch: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            incarnation: bytes[8..24].try_into().unwrap(),
            token: bytes[24..40].try_into().unwrap(),
        })
    }
}

impl LeaseResult {
    pub fn encode_wire(&self) -> Result<[u8; 24], WireError> {
        let mut out = [0; 24];
        out[0] = self.outcome as u8;
        out[1] = self.reason as u8;
        out[2] = self.role as u8;
        out[4..8].copy_from_slice(&self.epoch.to_le_bytes());
        out[8..].copy_from_slice(&self.token);
        crate::wire::validate_payload(0x16, &out)?;
        Ok(out)
    }
    pub fn decode_wire(bytes: &[u8]) -> Result<Self, WireError> {
        crate::wire::validate_payload(0x16, bytes)?;
        Ok(Self {
            outcome: [
                ResultOutcome::Granted,
                ResultOutcome::Resumed,
                ResultOutcome::Released,
                ResultOutcome::Refused,
            ][bytes[0] as usize],
            reason: [
                ResultReason::None,
                ResultReason::Busy,
                ResultReason::BadEpoch,
                ResultReason::BadToken,
                ResultReason::BadRole,
                ResultReason::NotHeld,
                ResultReason::Exhausted,
                ResultReason::BadIncarnation,
            ][bytes[1] as usize],
            role: [LeaseRole::Viewer, LeaseRole::InputOnly][bytes[2] as usize],
            epoch: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            token: bytes[8..24].try_into().unwrap(),
        })
    }
}

pub trait TokenSource {
    fn token(&mut self) -> Option<[u8; 16]>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedInput {
    pub epoch: u32,
    pub request_id: u64,
    pub exact_payload: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputAdmission {
    Execute,
    Replay(Vec<u8>),
    Refuse(ResultReason),
}

struct Lease {
    conn: ConnId,
    role: LeaseRole,
    epoch: u32,
    token: [u8; 16],
    deadline: u64,
    reserved: bool,
    high: u64,
    cached: Option<(OwnedInput, Vec<u8>)>,
    inflight: Option<OwnedInput>,
}

pub struct LeaseMachine {
    incarnation: [u8; 16],
    allocated: u32,
    lease: Option<Lease>,
}

impl LeaseMachine {
    pub fn new(incarnation: [u8; 16]) -> Self {
        Self::with_allocated(incarnation, 0)
    }
    pub fn with_allocated(incarnation: [u8; 16], allocated: u32) -> Self {
        Self {
            incarnation,
            allocated,
            lease: None,
        }
    }
    fn refusal(&self, role: LeaseRole, reason: ResultReason) -> LeaseResult {
        LeaseResult {
            outcome: ResultOutcome::Refused,
            reason,
            role,
            epoch: self.allocated,
            token: [0; 16],
        }
    }
    pub fn request(
        &mut self,
        conn: ConnId,
        request: &LeaseRequest,
        now: u64,
        tokens: &mut impl TokenSource,
    ) -> LeaseResult {
        self.expire(now);
        match request.operation {
            LeaseOperation::Fresh => {
                if self.lease.is_some() {
                    return self.refusal(request.role, ResultReason::Busy);
                }
                if request.epoch != 0 {
                    return self.refusal(request.role, ResultReason::BadEpoch);
                }
                if request.incarnation != [0; 16] {
                    return self.refusal(request.role, ResultReason::BadIncarnation);
                }
                if request.token != [0; 16] {
                    return self.refusal(request.role, ResultReason::BadToken);
                }
                let Some(epoch) = self.allocated.checked_add(1) else {
                    return self.refusal(request.role, ResultReason::Exhausted);
                };
                let Some(token) = tokens.token().filter(|value| *value != [0; 16]) else {
                    return self.refusal(request.role, ResultReason::Exhausted);
                };
                self.allocated = epoch;
                self.lease = Some(Lease {
                    conn,
                    role: request.role,
                    epoch,
                    token,
                    deadline: now.saturating_add(10_000),
                    reserved: false,
                    high: 0,
                    cached: None,
                    inflight: None,
                });
                LeaseResult {
                    outcome: ResultOutcome::Granted,
                    reason: ResultReason::None,
                    role: request.role,
                    epoch,
                    token,
                }
            }
            LeaseOperation::Resume => {
                let Some(lease) = self.lease.as_ref() else {
                    return self.refusal(request.role, ResultReason::NotHeld);
                };
                let reason = if !lease.reserved {
                    Some(ResultReason::Busy)
                } else if request.incarnation != self.incarnation {
                    Some(ResultReason::BadIncarnation)
                } else if request.epoch != lease.epoch {
                    Some(ResultReason::BadEpoch)
                } else if request.role != lease.role {
                    Some(ResultReason::BadRole)
                } else if request.token != lease.token {
                    Some(ResultReason::BadToken)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    return self.refusal(request.role, reason);
                }
                let Some(token) = tokens.token().filter(|value| *value != [0; 16]) else {
                    return self.refusal(request.role, ResultReason::Exhausted);
                };
                let lease = self.lease.as_mut().unwrap();
                lease.conn = conn;
                lease.token = token;
                lease.reserved = false;
                lease.deadline = now.saturating_add(10_000);
                LeaseResult {
                    outcome: ResultOutcome::Resumed,
                    reason: ResultReason::None,
                    role: lease.role,
                    epoch: lease.epoch,
                    token,
                }
            }
        }
    }
    pub fn release(&mut self, conn: ConnId, epoch: u32, token: [u8; 16]) -> LeaseResult {
        let role = self
            .lease
            .as_ref()
            .map_or(LeaseRole::Viewer, |lease| lease.role);
        let exact = self.lease.as_ref().is_some_and(|lease| {
            !lease.reserved && lease.conn == conn && lease.epoch == epoch && lease.token == token
        });
        if exact {
            self.lease = None;
            LeaseResult {
                outcome: ResultOutcome::Released,
                reason: ResultReason::None,
                role,
                epoch,
                token: [0; 16],
            }
        } else {
            self.refusal(role, ResultReason::NotHeld)
        }
    }
    pub fn keepalive(
        &mut self,
        conn: ConnId,
        epoch: u32,
        token: [u8; 16],
        now: u64,
    ) -> Result<(), ResultReason> {
        self.expire(now);
        let Some(lease) = self.lease.as_mut() else {
            return Err(ResultReason::NotHeld);
        };
        if lease.reserved || lease.conn != conn || lease.epoch != epoch || lease.token != token {
            return Err(ResultReason::NotHeld);
        }
        lease.deadline = now.saturating_add(10_000);
        Ok(())
    }
    pub fn disconnect(&mut self, conn: ConnId) {
        if let Some(lease) = self
            .lease
            .as_mut()
            .filter(|lease| !lease.reserved && lease.conn == conn)
        {
            lease.reserved = true;
        }
    }
    pub fn touch_owner(&mut self, conn: ConnId, now: u64) -> Result<(), ResultReason> {
        self.expire(now);
        let Some(lease) = self
            .lease
            .as_mut()
            .filter(|lease| !lease.reserved && lease.conn == conn)
        else {
            return Err(ResultReason::NotHeld);
        };
        lease.deadline = now.saturating_add(10_000);
        Ok(())
    }
    pub fn expire(&mut self, now: u64) {
        if self
            .lease
            .as_ref()
            .is_some_and(|lease| now >= lease.deadline)
        {
            self.lease = None;
        }
    }
    pub fn admit_input(&mut self, conn: ConnId, input: &OwnedInput) -> InputAdmission {
        let now = self
            .lease
            .as_ref()
            .map_or(0, |lease| lease.deadline.saturating_sub(10_000));
        self.admit_input_at(conn, input, now)
    }
    pub fn admit_input_at(&mut self, conn: ConnId, input: &OwnedInput, now: u64) -> InputAdmission {
        self.expire(now);
        let Some(lease) = self.lease.as_mut() else {
            return InputAdmission::Refuse(ResultReason::NotHeld);
        };
        if lease.reserved || lease.conn != conn || input.epoch != lease.epoch {
            return InputAdmission::Refuse(ResultReason::NotHeld);
        }
        lease.deadline = now.saturating_add(10_000);
        if lease.inflight.is_some() {
            return InputAdmission::Refuse(ResultReason::BadSequence);
        }
        if input.request_id == lease.high && lease.high != 0 {
            return match &lease.cached {
                Some((prior, receipt)) if prior == input => InputAdmission::Replay(receipt.clone()),
                _ => InputAdmission::Refuse(ResultReason::BadSequence),
            };
        }
        match lease.high.checked_add(1) {
            Some(next) if input.request_id == next => {
                lease.inflight = Some(input.clone());
                InputAdmission::Execute
            }
            Some(_) => InputAdmission::Refuse(ResultReason::BadSequence),
            None => InputAdmission::Refuse(ResultReason::Exhausted),
        }
    }
    pub fn finish_input(
        &mut self,
        conn: ConnId,
        input: OwnedInput,
        receipt: Vec<u8>,
    ) -> Result<(), ResultReason> {
        let Some(lease) = self
            .lease
            .as_mut()
            .filter(|lease| !lease.reserved && lease.conn == conn)
        else {
            return Err(ResultReason::NotHeld);
        };
        if lease.inflight.as_ref() != Some(&input) {
            return Err(ResultReason::BadSequence);
        }
        lease.inflight = None;
        lease.high = input.request_id;
        lease.cached = Some((input, receipt));
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Unattached,
    InputOnly,
    Resumed,
    Observer,
    Viewer,
    Closing,
}

pub const fn legal_in_phase(phase: Phase, kind: u8) -> bool {
    match phase {
        Phase::Unattached => matches!(kind, 0x03 | 0x0d | 0x0f | 0x15 | 0x19),
        Phase::InputOnly => matches!(kind, 0x09 | 0x17 | 0x18),
        Phase::Resumed => matches!(kind, 0x03 | 0x17 | 0x18),
        Phase::Observer => matches!(kind, 0x07 | 0x0d | 0x0f | 0x15 | 0x19),
        Phase::Viewer => matches!(
            kind,
            0x07 | 0x09 | 0x0b | 0x0c | 0x0d | 0x0f | 0x17 | 0x18 | 0x19
        ),
        Phase::Closing => false,
    }
}

pub struct ControllerConnection {
    generation: u32,
    identity: Vec<u8>,
    phase: Option<Phase>,
}

impl ControllerConnection {
    pub fn new(generation: u32, identity: Vec<u8>) -> Self {
        Self {
            generation,
            identity,
            phase: None,
        }
    }
    pub fn phase(&self) -> Option<Phase> {
        self.phase
    }
    pub fn hello(&mut self, generation: u32, identity: &[u8]) -> Result<u32, WireError> {
        if self.phase.is_some() {
            return Err(WireError::Malformed);
        }
        if generation != 0 && generation != self.generation {
            return Err(WireError::GenerationMismatch);
        }
        if identity != self.identity {
            return Err(WireError::IdentityMismatch);
        }
        self.phase = Some(Phase::Unattached);
        Ok(self.generation)
    }
    pub fn frame(&self, generation: u32, kind: u8) -> Result<(), WireError> {
        if generation != self.generation {
            return Err(WireError::GenerationMismatch);
        }
        if self.phase.is_some_and(|phase| legal_in_phase(phase, kind)) {
            Ok(())
        } else {
            Err(WireError::Malformed)
        }
    }
    pub fn lease(
        &mut self,
        operation: LeaseOperation,
        role: LeaseRole,
        granted: bool,
    ) -> Result<(), WireError> {
        let phase = self.phase.ok_or(WireError::Malformed)?;
        let next = match (phase, operation, role) {
            (Phase::Unattached, LeaseOperation::Fresh, LeaseRole::InputOnly) => Phase::InputOnly,
            (Phase::Unattached, LeaseOperation::Resume, LeaseRole::InputOnly) => Phase::InputOnly,
            (Phase::Unattached, LeaseOperation::Resume, LeaseRole::Viewer) => Phase::Resumed,
            (Phase::Observer, LeaseOperation::Fresh, LeaseRole::Viewer) => Phase::Viewer,
            _ => return Err(WireError::Malformed),
        };
        if granted {
            self.phase = Some(next);
        }
        Ok(())
    }
    pub fn attach(&mut self, request_lease: bool, owns_lease: bool) -> Result<(), WireError> {
        self.phase = Some(match self.phase {
            Some(Phase::Unattached) if request_lease => {
                if owns_lease {
                    Phase::Viewer
                } else {
                    Phase::Observer
                }
            }
            Some(Phase::Unattached) if !owns_lease => Phase::Observer,
            Some(Phase::Resumed) if !request_lease && owns_lease => Phase::Viewer,
            _ => return Err(WireError::Malformed),
        });
        Ok(())
    }
    pub fn released(&mut self) -> Result<(), WireError> {
        self.phase = Some(match self.phase {
            Some(Phase::Viewer) => Phase::Observer,
            Some(Phase::InputOnly | Phase::Resumed) => Phase::Unattached,
            _ => return Err(WireError::Malformed),
        });
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryContext {
    pub owner: Option<(ConnId, u32)>,
    pub synthetic: Option<Vec<u8>>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryAction {
    Delegate { conn: ConnId, query: Query },
    ChildReply(Vec<u8>),
    Release(Vec<u8>),
    ResourceExhausted { conn: ConnId },
    Disconnect { conn: ConnId },
}
struct PendingQuery {
    conn: ConnId,
    query: Query,
    shape: crate::wire::QueryShape,
    fallback: Option<Vec<u8>>,
    deadline: u64,
}
pub struct QueryMachine {
    next: Option<u64>,
    pending: VecDeque<PendingQuery>,
    exhaustion_reported: bool,
}

impl QueryMachine {
    pub fn new() -> Self {
        Self::with_next(1)
    }
    pub fn with_next(next: u64) -> Self {
        Self {
            next: Some(next).filter(|n| *n != 0),
            pending: VecDeque::new(),
            exhaustion_reported: false,
        }
    }
    fn immediate(raw: &[u8], fallback: Option<Vec<u8>>) -> Vec<QueryAction> {
        let mut out = Vec::new();
        if let Some(reply) = fallback {
            out.push(QueryAction::ChildReply(reply));
        }
        out.push(QueryAction::Release(raw.to_vec()));
        out
    }
    fn cancel(&mut self, out: &mut Vec<QueryAction>) {
        for pending in self.pending.drain(..) {
            if let Some(reply) = pending.fallback {
                out.push(QueryAction::ChildReply(reply));
            }
        }
    }
    pub fn recognize(&mut self, now: u64, raw: &[u8], context: QueryContext) -> Vec<QueryAction> {
        let Some(shape) = recognize_query(raw) else {
            return vec![QueryAction::Release(raw.to_vec())];
        };
        let Some((conn, epoch)) = context.owner else {
            return Self::immediate(raw, context.synthetic);
        };
        if self.exhaustion_reported {
            return Self::immediate(raw, context.synthetic);
        }
        if self.pending.len() == 64 {
            let mut out = vec![QueryAction::Disconnect { conn }];
            self.cancel(&mut out);
            out.extend(Self::immediate(raw, context.synthetic));
            return out;
        }
        if self.next.is_none() {
            self.exhaustion_reported = true;
            let mut out = vec![
                QueryAction::ResourceExhausted { conn },
                QueryAction::Disconnect { conn },
            ];
            self.cancel(&mut out);
            out.extend(Self::immediate(raw, context.synthetic));
            return out;
        }
        let correlation = self.next.unwrap();
        self.next = correlation.checked_add(1);
        let query = Query {
            correlation,
            epoch,
            class: shape.class,
            bytes: raw.to_vec(),
        };
        self.pending.push_back(PendingQuery {
            conn,
            query: query.clone(),
            shape,
            fallback: context.synthetic,
            deadline: now.saturating_add(250),
        });
        vec![
            QueryAction::Delegate { conn, query },
            QueryAction::Release(raw.to_vec()),
        ]
    }
    pub fn reply(&mut self, now: u64, conn: ConnId, reply: &Query) -> Vec<QueryAction> {
        let Some(index) = self
            .pending
            .iter()
            .position(|p| p.conn == conn && p.query.correlation == reply.correlation)
        else {
            return Vec::new();
        };
        let pending = &self.pending[index];
        if pending.query.epoch != reply.epoch
            || pending.query.class != reply.class
            || !validate_query_reply(&pending.shape, &reply.bytes)
        {
            return Vec::new();
        }
        if now >= pending.deadline {
            return self
                .pending
                .remove(index)
                .unwrap()
                .fallback
                .into_iter()
                .map(QueryAction::ChildReply)
                .collect();
        }
        self.pending.remove(index);
        vec![QueryAction::ChildReply(reply.bytes.clone())]
    }
    pub fn poll(&mut self, now: u64) -> Vec<QueryAction> {
        let mut out = Vec::new();
        while self.pending.front().is_some_and(|p| now >= p.deadline) {
            if let Some(reply) = self.pending.pop_front().unwrap().fallback {
                out.push(QueryAction::ChildReply(reply));
            }
        }
        out
    }
    pub fn owner_gone(&mut self, conn: ConnId) -> Vec<QueryAction> {
        let mut out = Vec::new();
        let mut kept = VecDeque::new();
        while let Some(pending) = self.pending.pop_front() {
            if pending.conn == conn {
                if let Some(reply) = pending.fallback {
                    out.push(QueryAction::ChildReply(reply));
                }
            } else {
                kept.push_back(pending);
            }
        }
        self.pending = kept;
        out
    }
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
    pub fn delegation_allocatable(&self) -> bool {
        self.next.is_some() && !self.exhaustion_reported
    }
}

impl Default for QueryMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticMode {
    Edge,
    Stateful,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticEventKind {
    Transition,
    Snapshot,
    ApplicationReceipt,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticAckStatus {
    Accepted,
    Duplicate,
    Refused,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticRefusal {
    CapabilityAbsent,
    StaleToken,
    Generation,
    SourceConflict,
    ResourceExhausted,
    BadSequence,
    EventConflict,
    SnapshotRequired,
    InvalidPayload,
    Superseded,
    ApplicationConflict,
    UnknownApplication,
    SourceUnavailable,
}
pub fn next_semantic_sequence(high: u64) -> Result<u64, SemanticRefusal> {
    high.checked_add(1)
        .ok_or(SemanticRefusal::ResourceExhausted)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticHello {
    pub token: [u8; 16],
    pub producer: [u8; 16],
    pub generation: u32,
    pub mode: SemanticMode,
    pub capabilities: u8,
    pub source: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticHelloAck {
    pub epoch: u32,
    pub snapshot_required: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticEvent {
    pub id: [u8; 16],
    pub sequence: u64,
    pub kind: SemanticEventKind,
    pub exact_payload: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventPosition {
    pub epoch: u32,
    pub sequence: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticAck {
    pub id: [u8; 16],
    pub sequence: u64,
    pub status: SemanticAckStatus,
    pub position: Option<EventPosition>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct CommitTicket(u64);
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticAdmission {
    Append(CommitTicket),
    Immediate(SemanticAck),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationInput {
    pub application_id: [u8; 16],
    pub lease_epoch: u32,
    pub request_id: u64,
    pub source: Vec<u8>,
    pub terminal: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationReceipt {
    pub application_id: [u8; 16],
    pub lease_epoch: u32,
    pub request_id: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputNotice {
    pub application_id: [u8; 16],
    pub lease_epoch: u32,
    pub request_id: u64,
    pub byte_count: u64,
    pub digest: [u8; 32],
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputNoticeAck {
    pub application_id: [u8; 16],
    pub lease_epoch: u32,
    pub request_id: u64,
    pub prepared: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NoticeTicket([u8; 16]);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WritePermit([u8; 16]);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissingReason {
    Deadline,
    SourceLost,
    RetentionExpired,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticEffect {
    pub application_id: [u8; 16],
    pub source: Vec<u8>,
    pub source_epoch: u32,
    pub producer: [u8; 16],
    pub reason: MissingReason,
}

#[derive(Clone)]
struct Entry {
    event: SemanticEvent,
    position: EventPosition,
}
struct Source {
    mode: SemanticMode,
    epoch: u32,
    conn: ConnId,
    producer: [u8; 16],
    capabilities: u8,
    snapshot: bool,
    high: u64,
    entries: VecDeque<Entry>,
    pending: bool,
    lost: bool,
    disconnected: bool,
    last_seen: u64,
}
struct Pending {
    source: Vec<u8>,
    epoch: u32,
    event: SemanticEvent,
    correlation: Option<[u8; 16]>,
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum CorrelationState {
    Prepared,
    Permitted,
    Written,
}
struct Correlation {
    input: ApplicationInput,
    conn: ConnId,
    source_epoch: u32,
    producer: [u8; 16],
    state: CorrelationState,
    notice_deadline: u64,
    deadline: u64,
    expiry: u64,
    deadline_emitted: bool,
    source_emitted: bool,
}

pub struct SemanticMachine {
    token: [u8; 16],
    generation: u32,
    sources: HashMap<Vec<u8>, Source>,
    connections: HashMap<ConnId, Vec<u8>>,
    pending: HashMap<CommitTicket, Pending>,
    next_ticket: u64,
    correlations: HashMap<[u8; 16], Correlation>,
    writable: bool,
}

impl SemanticMachine {
    pub fn new(token: [u8; 16], generation: u32) -> Self {
        Self {
            token,
            generation,
            sources: HashMap::new(),
            connections: HashMap::new(),
            pending: HashMap::new(),
            next_ticket: 1,
            correlations: HashMap::new(),
            writable: true,
        }
    }
    pub fn hello(
        &mut self,
        conn: ConnId,
        hello: &SemanticHello,
    ) -> Result<SemanticHelloAck, SemanticRefusal> {
        self.hello_at(conn, hello, 0)
    }
    pub fn hello_at(
        &mut self,
        conn: ConnId,
        hello: &SemanticHello,
        now: u64,
    ) -> Result<SemanticHelloAck, SemanticRefusal> {
        if self.token == [0; 16] {
            return Err(SemanticRefusal::CapabilityAbsent);
        }
        if !self.writable {
            return Err(SemanticRefusal::ResourceExhausted);
        }
        if hello.token != self.token {
            return Err(SemanticRefusal::StaleToken);
        }
        if hello.generation != self.generation && !(self.generation == 1 && hello.generation == 0) {
            return Err(SemanticRefusal::Generation);
        }
        if hello.capabilities & !7 != 0
            || hello.source.is_empty()
            || hello.source.len() > 128
            || !hello
                .source
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
        {
            return Err(SemanticRefusal::InvalidPayload);
        }
        let epoch;
        if let Some(source) = self.sources.get_mut(&hello.source) {
            if source.mode != hello.mode {
                return Err(SemanticRefusal::SourceConflict);
            }
            epoch = source
                .epoch
                .checked_add(1)
                .ok_or(SemanticRefusal::ResourceExhausted)?;
            source.epoch = epoch;
            source.conn = conn;
            source.producer = hello.producer;
            source.capabilities = hello.capabilities;
            source.snapshot = hello.mode == SemanticMode::Stateful;
            source.high = 0;
            source.entries.clear();
            source.pending = false;
            source.lost = false;
            source.disconnected = false;
            source.last_seen = now;
        } else {
            if self.sources.len() >= 64 {
                return Err(SemanticRefusal::ResourceExhausted);
            }
            epoch = 1;
            self.sources.insert(
                hello.source.clone(),
                Source {
                    mode: hello.mode,
                    epoch,
                    conn,
                    producer: hello.producer,
                    capabilities: hello.capabilities,
                    snapshot: hello.mode == SemanticMode::Stateful,
                    high: 0,
                    entries: VecDeque::new(),
                    pending: false,
                    lost: false,
                    disconnected: false,
                    last_seen: now,
                },
            );
        }
        self.connections.insert(conn, hello.source.clone());
        Ok(SemanticHelloAck {
            epoch,
            snapshot_required: hello.mode == SemanticMode::Stateful,
        })
    }
    pub fn admit(
        &mut self,
        conn: ConnId,
        event: &SemanticEvent,
    ) -> Result<SemanticAdmission, SemanticRefusal> {
        if !self.writable {
            return Err(SemanticRefusal::ResourceExhausted);
        }
        let name = self
            .connections
            .get(&conn)
            .ok_or(SemanticRefusal::Superseded)?
            .clone();
        let source = self.sources.get_mut(&name).unwrap();
        if source.conn != conn || source.disconnected {
            return Err(SemanticRefusal::Superseded);
        }
        if source.mode == SemanticMode::Edge && event.kind != SemanticEventKind::Transition {
            return Err(SemanticRefusal::InvalidPayload);
        }
        if source.snapshot && event.kind != SemanticEventKind::Snapshot {
            return Err(SemanticRefusal::SnapshotRequired);
        }
        if event.exact_payload.len() > 32 * 1024 || event.sequence == 0 {
            return Err(SemanticRefusal::InvalidPayload);
        }
        if source
            .entries
            .iter()
            .any(|entry| entry.event.id == event.id && &entry.event != event)
        {
            return Err(SemanticRefusal::EventConflict);
        }
        if event.sequence <= source.high {
            if let Some(entry) = source
                .entries
                .iter()
                .find(|entry| entry.event.sequence == event.sequence)
            {
                if &entry.event == event {
                    return Ok(SemanticAdmission::Immediate(SemanticAck {
                        id: event.id,
                        sequence: event.sequence,
                        status: SemanticAckStatus::Duplicate,
                        position: Some(entry.position),
                    }));
                }
                return Err(SemanticRefusal::EventConflict);
            }
            return Err(if source.high == u64::MAX {
                SemanticRefusal::ResourceExhausted
            } else {
                SemanticRefusal::BadSequence
            });
        }
        if source.pending || next_semantic_sequence(source.high)? != event.sequence {
            return Err(SemanticRefusal::BadSequence);
        }
        if self.next_ticket == u64::MAX {
            return Err(SemanticRefusal::ResourceExhausted);
        }
        let ticket = CommitTicket(self.next_ticket);
        self.next_ticket += 1;
        source.pending = true;
        self.pending.insert(
            ticket,
            Pending {
                source: name,
                epoch: source.epoch,
                event: event.clone(),
                correlation: None,
            },
        );
        Ok(SemanticAdmission::Append(ticket))
    }
    pub fn admit_epoch(
        &mut self,
        conn: ConnId,
        epoch: u32,
        event: &SemanticEvent,
    ) -> Result<SemanticAdmission, SemanticRefusal> {
        let name = self
            .connections
            .get(&conn)
            .ok_or(SemanticRefusal::Superseded)?;
        if self
            .sources
            .get(name)
            .is_none_or(|source| source.conn != conn || source.epoch != epoch)
        {
            return Err(SemanticRefusal::Superseded);
        }
        self.admit(conn, event)
    }
    pub fn failed(&mut self, ticket: CommitTicket) -> Result<(), SemanticRefusal> {
        let pending = self
            .pending
            .remove(&ticket)
            .ok_or(SemanticRefusal::Superseded)?;
        if let Some(source) = self
            .sources
            .get_mut(&pending.source)
            .filter(|source| source.epoch == pending.epoch)
        {
            source.pending = false;
        }
        Ok(())
    }
    pub fn committed(
        &mut self,
        ticket: CommitTicket,
        position: EventPosition,
    ) -> Result<SemanticAck, SemanticRefusal> {
        let pending = self
            .pending
            .remove(&ticket)
            .ok_or(SemanticRefusal::Superseded)?;
        let source = self
            .sources
            .get_mut(&pending.source)
            .ok_or(SemanticRefusal::Superseded)?;
        if source.epoch != pending.epoch {
            return Err(SemanticRefusal::Superseded);
        }
        source.pending = false;
        source.high = pending.event.sequence;
        if pending.event.kind == SemanticEventKind::Snapshot {
            source.snapshot = false;
            if !source.disconnected {
                source.lost = false;
            }
        }
        source.entries.push_back(Entry {
            event: pending.event.clone(),
            position,
        });
        if source.entries.len() > 512 {
            source.entries.pop_front();
        }
        if let Some(application) = pending.correlation {
            self.correlations.remove(&application);
        }
        Ok(SemanticAck {
            id: pending.event.id,
            sequence: pending.event.sequence,
            status: SemanticAckStatus::Accepted,
            position: Some(position),
        })
    }

    pub fn prepare_input(
        &mut self,
        input: &ApplicationInput,
        now: u64,
    ) -> Result<(NoticeTicket, InputNotice), SemanticRefusal> {
        if self.correlations.contains_key(&input.application_id) {
            return Err(SemanticRefusal::ApplicationConflict);
        }
        if self.correlations.len() >= 512 {
            return Err(SemanticRefusal::ResourceExhausted);
        }
        let source = self
            .sources
            .get(&input.source)
            .ok_or(SemanticRefusal::SourceUnavailable)?;
        if source.mode != SemanticMode::Stateful
            || source.snapshot
            || source.lost
            || source.capabilities & 6 != 6
        {
            return Err(SemanticRefusal::SourceUnavailable);
        }
        let digest: [u8; 32] = Sha256::digest(&input.terminal).into();
        let notice = InputNotice {
            application_id: input.application_id,
            lease_epoch: input.lease_epoch,
            request_id: input.request_id,
            byte_count: input.terminal.len() as u64,
            digest,
        };
        self.correlations.insert(
            input.application_id,
            Correlation {
                input: input.clone(),
                conn: source.conn,
                source_epoch: source.epoch,
                producer: source.producer,
                state: CorrelationState::Prepared,
                notice_deadline: now.saturating_add(2_000),
                deadline: 0,
                expiry: 0,
                deadline_emitted: false,
                source_emitted: false,
            },
        );
        Ok((NoticeTicket(input.application_id), notice))
    }
    pub fn accept_notice(
        &mut self,
        conn: ConnId,
        ticket: NoticeTicket,
        ack: &InputNoticeAck,
        now: u64,
    ) -> Result<WritePermit, SemanticRefusal> {
        if self
            .correlations
            .get(&ticket.0)
            .is_some_and(|correlation| now >= correlation.notice_deadline)
        {
            self.correlations.remove(&ticket.0);
            return Err(SemanticRefusal::SourceUnavailable);
        }
        let Some(correlation) = self.correlations.get(&ticket.0) else {
            return Err(SemanticRefusal::UnknownApplication);
        };
        if correlation.conn != conn
            || correlation.state != CorrelationState::Prepared
            || ack.application_id != correlation.input.application_id
            || ack.lease_epoch != correlation.input.lease_epoch
            || ack.request_id != correlation.input.request_id
            || !self.current_source(correlation)
        {
            return Err(SemanticRefusal::SourceUnavailable);
        }
        if !ack.prepared {
            self.correlations.remove(&ticket.0);
            return Err(SemanticRefusal::SourceUnavailable);
        }
        self.correlations.get_mut(&ticket.0).unwrap().state = CorrelationState::Permitted;
        Ok(WritePermit(ticket.0))
    }
    pub fn input_written(&mut self, permit: WritePermit, now: u64) -> Result<(), SemanticRefusal> {
        let current = self
            .correlations
            .get(&permit.0)
            .is_some_and(|correlation| self.current_source(correlation));
        if !current {
            self.correlations.remove(&permit.0);
            return Err(SemanticRefusal::SourceUnavailable);
        }
        let correlation = self
            .correlations
            .get_mut(&permit.0)
            .ok_or(SemanticRefusal::UnknownApplication)?;
        if correlation.state != CorrelationState::Permitted {
            return Err(SemanticRefusal::SourceUnavailable);
        }
        correlation.state = CorrelationState::Written;
        correlation.deadline = now.saturating_add(60_000);
        correlation.expiry = now.saturating_add(600_000);
        Ok(())
    }
    pub fn input_failed(&mut self, permit: WritePermit) {
        self.correlations.remove(&permit.0);
    }
    fn current_source(&self, correlation: &Correlation) -> bool {
        self.sources
            .get(&correlation.input.source)
            .is_some_and(|source| {
                source.conn == correlation.conn
                    && source.epoch == correlation.source_epoch
                    && source.producer == correlation.producer
                    && !source.lost
            })
    }
    pub fn expire_notices(&mut self, now: u64) -> Vec<NoticeTicket> {
        let expired: Vec<_> = self
            .correlations
            .iter()
            .filter(|(_, correlation)| {
                correlation.state == CorrelationState::Prepared
                    && now >= correlation.notice_deadline
            })
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            self.correlations.remove(id);
        }
        expired.into_iter().map(NoticeTicket).collect()
    }
    pub fn admit_receipt(
        &mut self,
        conn: ConnId,
        event: &SemanticEvent,
        receipt: ApplicationReceipt,
    ) -> Result<SemanticAdmission, SemanticRefusal> {
        let admission = self.admit(conn, event)?;
        if matches!(admission, SemanticAdmission::Immediate(_)) {
            return Ok(admission);
        }
        let SemanticAdmission::Append(ticket) = admission else {
            unreachable!()
        };
        let valid = self
            .correlations
            .get(&receipt.application_id)
            .is_some_and(|c| {
                c.conn == conn
                    && c.state == CorrelationState::Written
                    && c.input.lease_epoch == receipt.lease_epoch
                    && c.input.request_id == receipt.request_id
            });
        if !valid {
            let pending = self.pending.remove(&ticket).unwrap();
            if let Some(source) = self.sources.get_mut(&pending.source) {
                source.pending = false;
            }
            return Err(SemanticRefusal::UnknownApplication);
        }
        self.pending.get_mut(&ticket).unwrap().correlation = Some(receipt.application_id);
        Ok(SemanticAdmission::Append(ticket))
    }
    fn effect(correlation: &Correlation, reason: MissingReason) -> SemanticEffect {
        SemanticEffect {
            application_id: correlation.input.application_id,
            source: correlation.input.source.clone(),
            source_epoch: correlation.source_epoch,
            producer: correlation.producer,
            reason,
        }
    }
    fn mark_lost(&mut self, conn: ConnId, disconnected: bool) -> Vec<SemanticEffect> {
        for source in self.sources.values_mut().filter(|s| s.conn == conn) {
            source.lost = true;
            source.disconnected |= disconnected;
            source.snapshot = source.mode == SemanticMode::Stateful;
        }
        let mut out = Vec::new();
        for correlation in self
            .correlations
            .values_mut()
            .filter(|c| c.conn == conn && c.state == CorrelationState::Written && !c.source_emitted)
        {
            correlation.source_emitted = true;
            out.push(Self::effect(correlation, MissingReason::SourceLost));
        }
        out
    }
    pub fn source_lost(&mut self, conn: ConnId, _now: u64) -> Vec<SemanticEffect> {
        self.mark_lost(conn, true)
    }
    pub fn heartbeat(&mut self, conn: ConnId, now: u64) -> Result<(), SemanticRefusal> {
        let name = self
            .connections
            .get(&conn)
            .ok_or(SemanticRefusal::Superseded)?;
        let source = self
            .sources
            .get_mut(name)
            .filter(|source| source.conn == conn && !source.disconnected)
            .ok_or(SemanticRefusal::Superseded)?;
        source.last_seen = now;
        Ok(())
    }
    pub fn poll(&mut self, now: u64) -> Vec<SemanticEffect> {
        let mut out = Vec::new();
        let lost: Vec<_> = self
            .sources
            .values()
            .filter(|source| {
                source.mode == SemanticMode::Stateful
                    && !source.lost
                    && now >= source.last_seen.saturating_add(15_000)
            })
            .map(|source| source.conn)
            .collect();
        for conn in lost {
            out.extend(self.mark_lost(conn, false));
        }
        let mut expired = Vec::new();
        for (id, correlation) in &mut self.correlations {
            if correlation.state != CorrelationState::Written {
                continue;
            }
            if now >= correlation.deadline && !correlation.deadline_emitted {
                correlation.deadline_emitted = true;
                out.push(Self::effect(correlation, MissingReason::Deadline));
            }
            if now >= correlation.expiry {
                out.push(Self::effect(correlation, MissingReason::RetentionExpired));
                expired.push(*id);
            }
        }
        for id in expired {
            self.correlations.remove(&id);
        }
        out
    }
    pub fn pending_correlations(&self) -> u16 {
        self.correlations
            .values()
            .filter(|correlation| correlation.state == CorrelationState::Written)
            .count() as u16
    }
    pub fn set_writable(&mut self, writable: bool) -> Vec<ConnId> {
        let mut close = if self.writable && !writable {
            self.sources.values().map(|source| source.conn).collect()
        } else {
            Vec::new()
        };
        close.sort_unstable();
        close.dedup();
        self.writable = writable;
        close
    }
    pub fn semantic_flags(&self) -> u8 {
        let exact = self
            .sources
            .values()
            .any(|s| s.mode == SemanticMode::Stateful && !s.snapshot && !s.lost);
        let degraded = self
            .sources
            .values()
            .any(|s| s.mode == SemanticMode::Stateful && s.lost);
        let receipt = self.sources.values().any(|s| {
            s.mode == SemanticMode::Stateful && !s.snapshot && !s.lost && s.capabilities & 6 == 6
        });
        u8::from(exact) | (u8::from(degraded) << 1) | (u8::from(receipt) << 2)
    }
}
