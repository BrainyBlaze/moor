use moor::session::{
    ApplicationInput, ApplicationReceipt, CommitTicket, Completion, Effect, Effects, EventPosition,
    InputNotice, InputNoticeAck, LeaseRequest, LeaseRole, Machine, MissingReason, OwnedInput,
    ReceiptProjection, Reply, Request, SemanticAck, SemanticAckStatus, SemanticChange,
    SemanticEvent, SemanticEventKind, SemanticHello, SemanticHelloAck, SemanticMode,
    SemanticRefusal, SourceEffect, SourceReason, SourceStatus, Ticket, Transition,
    next_semantic_sequence,
};
use moor::wire::InputReceipt;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

const CONTROLLER: u64 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
enum SemanticAdmission {
    Append(CommitTicket, Vec<u8>, u32, [u8; 16], SemanticEvent),
    Immediate(SemanticAck),
}

#[derive(Clone, Copy, Debug)]
struct NoticeTicket([u8; 16]);

struct TestApplication {
    request: ApplicationInput,
    source: Vec<u8>,
    terminal: Vec<u8>,
}
impl Deref for TestApplication {
    type Target = ApplicationInput;
    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

#[derive(Clone)]
struct WritePermit(Ticket, Vec<u8>);

#[derive(Default)]
struct HelloState {
    commit: Option<CommitTicket>,
    ack: Option<SemanticHelloAck>,
    refusal: Option<SemanticRefusal>,
    replaced: Option<u64>,
}

struct SemanticMachine {
    inner: Machine,
    hellos: HashMap<u64, HelloState>,
    commits: HashMap<CommitTicket, u64>,
    input_sequence: u64,
}

impl SemanticMachine {
    fn new(token: [u8; 16], generation: u32) -> Self {
        let mut inner = Machine::new(generation, id(6), token).allocated(2);
        inner.register_controller(CONTROLLER);
        inner
            .transition(Transition::Peer(
                0,
                CONTROLLER,
                Request::Lease(LeaseRequest::fresh(LeaseRole::InputOnly), Some(id(7))),
            ))
            .unwrap();
        Self {
            inner,
            hellos: HashMap::new(),
            commits: HashMap::new(),
            input_sequence: 1,
        }
    }

    fn request<'a>(&mut self, now: u64, conn: u64, request: Request<'a>) -> Effects {
        self.transition(Transition::Peer(now, conn, request))
    }

    fn transition<'a>(&mut self, event: Transition<'a>) -> Effects {
        self.inner.transition(event).unwrap()
    }

    fn begin_hello(
        &mut self,
        conn: u64,
        hello: SemanticHello,
    ) -> Result<Vec<SemanticChange>, SemanticRefusal> {
        let mut state = HelloState::default();
        let mut changes = Vec::new();
        for effect in self.request(0, conn, Request::SemanticHello(hello)) {
            match effect {
                Effect::CommitSources(ticket, batch) => {
                    state.commit = Some(ticket);
                    changes = batch;
                }
                Effect::Send(id, Reply::SemanticHello(ack)) if id == conn => state.ack = Some(ack),
                Effect::Send(id, Reply::SemanticRefused(_, error)) if id == conn => {
                    state.refusal = Some(error)
                }
                Effect::Replaced(id) => state.replaced = Some(id),
                _ => {}
            }
        }
        if let Some(error) = state.refusal {
            return Err(error);
        }
        self.hellos.insert(conn, state);
        Ok(changes)
    }

    fn complete_hello(&mut self, conn: u64, now: u64, success: bool) {
        let Some(mut state) = self.hellos.remove(&conn) else {
            return;
        };
        if let Some(ticket) = state.commit.take() {
            for effect in self.transition(Transition::Complete(
                now,
                ticket,
                Completion::Sources(success),
            )) {
                match effect {
                    Effect::Send(id, Reply::SemanticHello(ack)) if id == conn => {
                        state.ack = Some(ack)
                    }
                    Effect::Send(id, Reply::SemanticRefused(_, error)) if id == conn => {
                        state.refusal = Some(error)
                    }
                    Effect::Replaced(id) => state.replaced = Some(id),
                    _ => {}
                }
            }
        }
        self.hellos.insert(conn, state);
    }

    fn adopt_hello(&mut self, conn: u64, now: u64) -> Option<u64> {
        self.complete_hello(conn, now, true);
        self.hellos.get(&conn).and_then(|state| state.replaced)
    }

    fn finish_hello(
        &mut self,
        conn: u64,
        success: bool,
    ) -> Option<Result<SemanticHelloAck, SemanticRefusal>> {
        self.complete_hello(conn, 0, success);
        let state = self.hellos.remove(&conn)?;
        Some(
            state
                .refusal
                .map_or_else(|| state.ack.ok_or(SemanticRefusal::Superseded), Err),
        )
    }

    fn admit(
        &mut self,
        conn: u64,
        event: &SemanticEvent,
    ) -> Result<SemanticAdmission, SemanticRefusal> {
        self.admit_with(conn, event, None)
    }

    fn admit_receipt(
        &mut self,
        conn: u64,
        event: &SemanticEvent,
        receipt: ApplicationReceipt,
    ) -> Result<SemanticAdmission, SemanticRefusal> {
        self.admit_with(
            conn,
            event,
            Some(ReceiptProjection {
                receipt,
                status: 0,
                provider_session: 0..0,
                provider_turn: 0..0,
            }),
        )
    }

    fn admit_with(
        &mut self,
        conn: u64,
        event: &SemanticEvent,
        projection: Option<ReceiptProjection>,
    ) -> Result<SemanticAdmission, SemanticRefusal> {
        for effect in self.request(0, conn, Request::SemanticEvent(event.clone(), projection)) {
            match effect {
                Effect::CommitSemantic(ticket, source, epoch, producer, event, _) => {
                    self.commits.insert(ticket, conn);
                    return Ok(SemanticAdmission::Append(
                        ticket, source, epoch, producer, event,
                    ));
                }
                Effect::Send(id, Reply::SemanticAck(ack)) if id == conn => {
                    return Ok(SemanticAdmission::Immediate(ack));
                }
                Effect::Send(id, Reply::SemanticRefused(_, error)) if id == conn => {
                    return Err(error);
                }
                _ => {}
            }
        }
        Err(SemanticRefusal::Superseded)
    }

    fn committed(
        &mut self,
        ticket: CommitTicket,
        position: EventPosition,
    ) -> Result<(u64, SemanticAck, Option<SourceEffect>), SemanticRefusal> {
        let mut ack = None;
        let mut producer = self.commits.remove(&ticket);
        let mut source_effect = None;
        let mut source_commit = None;
        for effect in self.transition(Transition::Complete(
            0,
            ticket,
            Completion::Semantic(Ok(position)),
        )) {
            match effect {
                Effect::Send(conn, Reply::SemanticAck(value)) => {
                    producer = Some(conn);
                    ack = Some(value);
                }
                Effect::CommitSources(ticket, changes) => {
                    source_effect = changes.into_iter().find_map(|change| match change {
                        SemanticChange::Source(effect) => Some(effect),
                        _ => None,
                    });
                    source_commit = Some(ticket);
                }
                Effect::Send(_, Reply::SemanticRefused(_, error)) => return Err(error),
                _ => {}
            }
        }
        if let Some(ticket) = source_commit {
            for effect in
                self.transition(Transition::Complete(0, ticket, Completion::Sources(true)))
            {
                if let Effect::Send(id, Reply::SemanticAck(value)) = effect
                    && Some(id) == producer
                {
                    ack = Some(value);
                }
            }
        }
        Ok((
            producer.ok_or(SemanticRefusal::Superseded)?,
            ack.ok_or(SemanticRefusal::Superseded)?,
            source_effect,
        ))
    }

    fn failed(&mut self, ticket: CommitTicket) -> Result<(), SemanticRefusal> {
        self.commits.remove(&ticket);
        self.transition(Transition::Complete(
            0,
            ticket,
            Completion::Semantic(Err(SemanticRefusal::ResourceExhausted)),
        ));
        Ok(())
    }

    fn changes(effects: Effects) -> Vec<SemanticChange> {
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::CommitSources(_, changes) => Some(changes),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn poll(&mut self, now: u64) -> Vec<SemanticChange> {
        let out = self.transition(Transition::Tick(now));
        Self::changes(out)
    }

    fn source_lost(&mut self, conn: u64) -> Vec<SemanticChange> {
        let out = self.transition(Transition::Disconnect(conn));
        Self::changes(out)
    }

    fn session_ending(&mut self) -> Vec<SemanticChange> {
        Self::changes(self.transition(Transition::Ending))
    }

    fn set_writable(&mut self, writable: bool) -> Vec<u64> {
        let out = self.transition(Transition::Writable(writable));
        out.into_iter()
            .filter_map(|effect| match effect {
                Effect::Close(conn) => Some(conn),
                _ => None,
            })
            .collect()
    }

    fn heartbeat(&mut self, conn: u64, now: u64) -> Result<(), SemanticRefusal> {
        for effect in self.request(now, conn, Request::SemanticHeartbeat) {
            if let Effect::Send(id, Reply::SemanticRefused(_, error)) = effect
                && id == conn
            {
                return Err(error);
            }
        }
        Ok(())
    }

    fn semantic_flags(&self) -> u8 {
        self.inner.status(CONTROLLER).semantic_flags
    }

    fn pending_correlations(&self) -> usize {
        self.inner.status(CONTROLLER).semantic_pending.into()
    }

    fn prepare_input(
        &mut self,
        application: &TestApplication,
        now: u64,
    ) -> Result<(NoticeTicket, InputNotice), SemanticRefusal> {
        let request_id = self.input_sequence;
        self.input_sequence = self
            .input_sequence
            .checked_add(1)
            .ok_or(SemanticRefusal::ResourceExhausted)?;
        let mut payload =
            Vec::with_capacity(29 + application.source.len() + application.terminal.len());
        payload.extend_from_slice(&application.receipt.lease_epoch.to_le_bytes());
        payload.extend_from_slice(&request_id.to_le_bytes());
        payload.push(1);
        payload.extend_from_slice(&application.receipt.application_id);
        payload.extend_from_slice(&application.source);
        payload.extend_from_slice(&application.terminal);
        let input = OwnedInput {
            epoch: application.receipt.lease_epoch,
            request_id,
            exact_payload: payload.into(),
        };
        for effect in self.request(
            now,
            CONTROLLER,
            Request::Input(input, Some(application.request.clone())),
        ) {
            match effect {
                Effect::Send(_, Reply::Notice(notice)) => {
                    return Ok((NoticeTicket(application.receipt.application_id), notice));
                }
                Effect::Send(_, Reply::Input(receipt)) => {
                    let receipt = InputReceipt::decode(&receipt)
                        .map_err(|_| SemanticRefusal::SourceUnavailable)?;
                    return Err(match receipt.result {
                        13 => SemanticRefusal::ResourceExhausted,
                        17 | 18 => SemanticRefusal::ApplicationConflict,
                        _ => SemanticRefusal::SourceUnavailable,
                    });
                }
                _ => {}
            }
        }
        Err(SemanticRefusal::SourceUnavailable)
    }

    fn accept_notice(
        &mut self,
        conn: u64,
        ticket: NoticeTicket,
        ack: &InputNoticeAck,
        now: u64,
    ) -> Result<WritePermit, SemanticRefusal> {
        if ticket.0 != ack.receipt.application_id {
            return Err(SemanticRefusal::SourceUnavailable);
        }
        for effect in self.request(now, conn, Request::NoticeAck(*ack)) {
            match effect {
                Effect::Write(ticket, bytes) => {
                    return Ok(WritePermit(ticket, bytes));
                }
                Effect::Send(_, Reply::SemanticRefused(_, error)) => return Err(error),
                Effect::Send(_, Reply::Input(_)) => {
                    return Err(SemanticRefusal::SourceUnavailable);
                }
                _ => {}
            }
        }
        Err(SemanticRefusal::SourceUnavailable)
    }

    fn input_written(&mut self, permit: WritePermit, now: u64) -> Result<(), SemanticRefusal> {
        let written = permit.1.len() as u64;
        for effect in self.transition(Transition::Complete(
            now,
            permit.0,
            Completion::Write(written, None),
        )) {
            if let Effect::Send(_, Reply::Input(receipt)) = effect {
                let receipt = InputReceipt::decode(&receipt)
                    .map_err(|_| SemanticRefusal::SourceUnavailable)?;
                return if receipt.status == 0 {
                    Ok(())
                } else {
                    Err(SemanticRefusal::SourceUnavailable)
                };
            }
        }
        Err(SemanticRefusal::SourceUnavailable)
    }

    fn expire_notices(&mut self, now: u64) -> Vec<Vec<u8>> {
        self.transition(Transition::Tick(now))
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Send(_, Reply::Input(receipt)) => Some(receipt),
                _ => None,
            })
            .collect()
    }
}

fn id(n: u8) -> [u8; 16] {
    [n; 16]
}
fn hello(mode: SemanticMode) -> SemanticHello {
    SemanticHello {
        token: id(1),
        producer: id(2),
        generation: 7,
        mode,
        capabilities: 7,
        source: b"claude".as_slice().into(),
    }
}
fn notice_ack(input: &ApplicationInput, prepared: bool) -> InputNoticeAck {
    InputNoticeAck {
        receipt: input.receipt,
        prepared,
    }
}

fn application(terminal: &[u8]) -> TestApplication {
    let source = b"claude".to_vec();
    TestApplication {
        request: ApplicationInput {
            receipt: ApplicationReceipt {
                application_id: id(9),
                lease_epoch: 3,
                request_id: 2,
            },
            source: 29..29 + source.len(),
            terminal_at: 29 + source.len(),
        },
        source,
        terminal: terminal.to_vec(),
    }
}

fn connect_at(
    machine: &mut SemanticMachine,
    conn: u64,
    request: &SemanticHello,
    now: u64,
) -> Result<(SemanticHelloAck, Vec<SemanticChange>, Option<u64>), SemanticRefusal> {
    let changes = machine.begin_hello(conn, request.clone())?;
    let superseded = machine.adopt_hello(conn, now);
    let ack = machine.finish_hello(conn, true).unwrap()?;
    Ok((ack, changes, superseded))
}

fn connect(
    machine: &mut SemanticMachine,
    conn: u64,
    request: &SemanticHello,
) -> Result<SemanticHelloAck, SemanticRefusal> {
    connect_at(machine, conn, request, 0).map(|result| result.0)
}

#[test]
fn semantic_ack_exists_only_after_durable_commit_and_duplicate_keeps_position() {
    let mut machine = SemanticMachine::new(id(1), 7);
    let accepted = connect(&mut machine, 10, &hello(SemanticMode::Stateful)).unwrap();
    assert!(accepted.snapshot_required);
    let event = SemanticEvent {
        id: id(3),
        sequence: 1,
        kind: SemanticEventKind::Snapshot,
        exact_payload: br#"{"state":"ready"}"#.to_vec().into(),
    };
    let ticket = match machine.admit(10, &event) {
        Ok(SemanticAdmission::Append(ticket, ..)) => ticket,
        other => panic!("unexpected {other:?}"),
    };
    let (producer, ack, _) = machine
        .committed(
            ticket,
            EventPosition {
                epoch: 0,
                sequence: 0,
            },
        )
        .unwrap();
    assert_eq!(producer, 10);
    assert_eq!(
        (ack.status, ack.position),
        (
            SemanticAckStatus::Accepted,
            Some(EventPosition {
                epoch: 0,
                sequence: 0
            })
        )
    );
    let duplicate = machine.admit(10, &event).unwrap();
    assert!(
        matches!(duplicate, SemanticAdmission::Immediate(ref a) if a.status == SemanticAckStatus::Duplicate && a.position == ack.position)
    );
    let mut conflict = event.clone();
    conflict.exact_payload = br#"{"state":"changed"}"#.to_vec().into();
    assert_eq!(
        machine.admit(10, &conflict),
        Err(SemanticRefusal::EventConflict)
    );
    conflict.exact_payload = event.exact_payload.clone();
    conflict.kind = SemanticEventKind::Transition;
    assert_eq!(
        machine.admit(10, &conflict),
        Err(SemanticRefusal::EventConflict)
    );
}

#[test]
fn semantic_retry_history_is_exactly_the_latest_512_digests() {
    let mut machine = SemanticMachine::new(id(1), 7);
    connect(&mut machine, 10, &hello(SemanticMode::Stateful)).unwrap();
    let make = |sequence: u64| {
        let mut event_id = [0; 16];
        event_id[..8].copy_from_slice(&sequence.to_le_bytes());
        SemanticEvent {
            id: event_id,
            sequence,
            kind: if sequence == 1 {
                SemanticEventKind::Snapshot
            } else {
                SemanticEventKind::Transition
            },
            exact_payload: b"{}".to_vec().into(),
        }
    };
    for sequence in 1..=513 {
        let event = make(sequence);
        let SemanticAdmission::Append(ticket, ..) = machine.admit(10, &event).unwrap() else {
            unreachable!()
        };
        machine
            .committed(ticket, EventPosition { epoch: 1, sequence })
            .unwrap();
    }
    assert_eq!(
        machine.admit(10, &make(1)),
        Err(SemanticRefusal::BadSequence)
    );
    assert!(
        matches!(machine.admit(10, &make(2)), Ok(SemanticAdmission::Immediate(ack))
        if ack.status == SemanticAckStatus::Duplicate && ack.position == Some(EventPosition { epoch: 1, sequence: 2 }))
    );
}

#[test]
fn source_mode_is_fixed_and_stateful_replacement_gets_a_new_epoch() {
    let mut machine = SemanticMachine::new(id(1), 7);
    let first = connect(&mut machine, 10, &hello(SemanticMode::Stateful)).unwrap();
    let (second, _, superseded) =
        connect_at(&mut machine, 11, &hello(SemanticMode::Stateful), 0).unwrap();
    assert_eq!(superseded, Some(10));
    assert_eq!(second.epoch, first.epoch + 1);
    assert!(second.snapshot_required);
    assert_eq!(
        connect(&mut machine, 12, &hello(SemanticMode::Edge)),
        Err(SemanticRefusal::SourceConflict)
    );
}

#[test]
fn replacement_preflights_one_ordered_causal_batch() {
    let mut machine = exact_machine();
    let input = application(b"x");
    let (ticket, _) = machine.prepare_input(&input, 0).unwrap();
    let permit = machine
        .accept_notice(10, ticket, &notice_ack(&input, true), 1)
        .unwrap();
    machine.input_written(permit, 2).unwrap();

    let changes = machine
        .begin_hello(11, hello(SemanticMode::Stateful))
        .unwrap();
    assert!(matches!(changes.as_slice(), [
        SemanticChange::Source(old), SemanticChange::Missing(missing), SemanticChange::Source(new)
    ] if old.status == SourceStatus::Disconnected && old.reason == SourceReason::Superseded
        && missing.reason == MissingReason::SourceLost && missing.receipt.application_id == id(9)
        && new.status == SourceStatus::Connected && new.reason == SourceReason::None));
    assert_eq!(machine.adopt_hello(11, 3), Some(10));
    machine.finish_hello(11, true).unwrap().unwrap();
    assert!(
        machine.poll(3).is_empty(),
        "the committed batch must not be emitted again"
    );
}

#[test]
fn source_replacement_cannot_overtake_an_uncommitted_event() {
    let mut machine = exact_machine();
    let event = SemanticEvent {
        id: id(11),
        sequence: 2,
        kind: SemanticEventKind::Transition,
        exact_payload: b"{}".to_vec().into(),
    };
    let SemanticAdmission::Append(ticket, ..) = machine.admit(10, &event).unwrap() else {
        unreachable!()
    };
    assert_eq!(
        machine.begin_hello(11, hello(SemanticMode::Stateful)),
        Err(SemanticRefusal::ResourceExhausted)
    );
    machine.failed(ticket).unwrap();
    assert!(
        machine
            .begin_hello(11, hello(SemanticMode::Stateful))
            .is_ok()
    );
}

#[test]
fn source_event_cannot_overtake_an_uncommitted_replacement() {
    let mut machine = exact_machine();
    let event = SemanticEvent {
        id: id(11),
        sequence: 2,
        kind: SemanticEventKind::Transition,
        exact_payload: b"{}".to_vec().into(),
    };
    machine
        .begin_hello(11, hello(SemanticMode::Stateful))
        .unwrap();
    assert_eq!(machine.admit(10, &event), Err(SemanticRefusal::BadSequence));
    machine.finish_hello(11, true).unwrap().unwrap();
}

#[test]
fn session_ending_includes_newly_due_missing_receipts() {
    let mut machine = exact_machine();
    let input = application(b"x");
    let (ticket, _) = machine.prepare_input(&input, 0).unwrap();
    let permit = machine
        .accept_notice(10, ticket, &notice_ack(&input, true), 1)
        .unwrap();
    machine.input_written(permit, 2).unwrap();
    let changes = machine.session_ending();
    let [
        SemanticChange::Source(source),
        SemanticChange::Missing(missing),
    ] = changes.as_slice()
    else {
        panic!("unexpected changes: {changes:?}")
    };
    assert!(Arc::ptr_eq(&source.source, &missing.source));
    assert!(
        matches!(changes.as_slice(), [SemanticChange::Source(source), SemanticChange::Missing(missing)]
        if source.status == SourceStatus::Disconnected && source.reason == SourceReason::SessionEnding
            && missing.reason == MissingReason::SourceLost && missing.receipt.application_id == id(9))
    );
}

#[test]
fn snapshot_gate_and_exact_payload_conflicts_fail_closed() {
    let mut machine = SemanticMachine::new(id(1), 7);
    connect(&mut machine, 10, &hello(SemanticMode::Stateful)).unwrap();
    let transition = SemanticEvent {
        id: id(4),
        sequence: 1,
        kind: SemanticEventKind::Transition,
        exact_payload: vec![1].into(),
    };
    assert_eq!(
        machine.admit(10, &transition),
        Err(SemanticRefusal::SnapshotRequired)
    );
}

#[test]
fn assertion_payload_must_be_a_bounded_duplicate_free_json_object() {
    let mut machine = exact_machine();
    let deep = format!("{}0{}", "{\"x\":".repeat(65), "}".repeat(65)).into_bytes();
    let many = format!(
        "{{{}}}",
        (0..1025)
            .map(|n| format!("\"k{n}\":0"))
            .collect::<Vec<_>>()
            .join(",")
    )
    .into_bytes();
    let payloads = [
        b"[]".to_vec(),
        b"{\"x\":1,\"x\":2}".to_vec(),
        b"{\"x\":\xff}".to_vec(),
        deep,
        many,
    ];
    for (n, payload) in payloads.into_iter().enumerate() {
        let event = SemanticEvent {
            id: id(20 + n as u8),
            sequence: 2,
            kind: SemanticEventKind::Transition,
            exact_payload: payload.into(),
        };
        assert_eq!(
            machine.admit(10, &event),
            Err(SemanticRefusal::InvalidPayload)
        );
    }
}

#[test]
fn advertised_capabilities_gate_assertions_and_receipts() {
    let mut request = hello(SemanticMode::Stateful);
    request.capabilities = 0;
    let mut machine = SemanticMachine::new(id(1), 7);
    connect(&mut machine, 10, &request).unwrap();
    let snapshot = SemanticEvent {
        id: id(8),
        sequence: 1,
        kind: SemanticEventKind::Snapshot,
        exact_payload: b"{}".to_vec().into(),
    };
    assert_eq!(
        machine.admit(10, &snapshot),
        Err(SemanticRefusal::CapabilityAbsent)
    );

    let receipt = SemanticEvent {
        id: id(9),
        sequence: 1,
        kind: SemanticEventKind::ApplicationReceipt,
        exact_payload: b"receipt".to_vec().into(),
    };
    assert_eq!(
        machine.admit(10, &receipt),
        Err(SemanticRefusal::InvalidPayload)
    );
}

fn exact_machine() -> SemanticMachine {
    let mut machine = SemanticMachine::new(id(1), 7);
    connect(&mut machine, 10, &hello(SemanticMode::Stateful)).unwrap();
    let snapshot = SemanticEvent {
        id: id(8),
        sequence: 1,
        kind: SemanticEventKind::Snapshot,
        exact_payload: b"{}".to_vec().into(),
    };
    let SemanticAdmission::Append(ticket, ..) = machine.admit(10, &snapshot).unwrap() else {
        unreachable!()
    };
    machine
        .committed(
            ticket,
            EventPosition {
                epoch: 0,
                sequence: 0,
            },
        )
        .unwrap();
    machine
}

#[test]
fn stateful_lifecycle_is_explicit_durable_work_and_edge_sources_are_silent() {
    let mut machine = SemanticMachine::new(id(1), 7);
    let (_, changes, _) = connect_at(&mut machine, 10, &hello(SemanticMode::Stateful), 0).unwrap();
    assert!(
        matches!(changes.as_slice(), [SemanticChange::Source(effect)]
        if effect.status == SourceStatus::Connected && effect.reason == SourceReason::None)
    );
    let snapshot = SemanticEvent {
        id: id(8),
        sequence: 1,
        kind: SemanticEventKind::Snapshot,
        exact_payload: b"{}".to_vec().into(),
    };
    let SemanticAdmission::Append(ticket, ..) = machine.admit(10, &snapshot).unwrap() else {
        unreachable!()
    };
    let (_, _, effect) = machine
        .committed(
            ticket,
            EventPosition {
                epoch: 0,
                sequence: 0,
            },
        )
        .unwrap();
    assert!(matches!(effect, Some(effect)
        if effect.status == SourceStatus::Exact && effect.reason == SourceReason::None));
    let changes = machine.poll(15_000);
    assert!(
        matches!(changes.as_slice(), [SemanticChange::Source(effect)]
        if effect.status == SourceStatus::Degraded && effect.reason == SourceReason::HeartbeatTimeout)
    );
    let changes = machine.source_lost(10);
    assert!(
        matches!(changes.as_slice(), [SemanticChange::Source(effect)]
        if effect.status == SourceStatus::Disconnected && effect.reason == SourceReason::TransportClosed)
    );

    let mut edge = hello(SemanticMode::Edge);
    edge.source = b"edge".as_slice().into();
    assert!(connect_at(&mut machine, 11, &edge, 0).unwrap().1.is_empty());
    assert!(machine.source_lost(11).is_empty());
}

#[test]
fn application_notice_precedes_write_and_correlation_resolves_only_after_commit() {
    let mut machine = exact_machine();
    let input = application(b"hello");
    let (ticket, notice) = machine.prepare_input(&input, 10).unwrap();
    assert_eq!(
        notice.digest,
        [
            0x2c, 0xf2, 0x4d, 0xba, 0x5f, 0xb0, 0xa3, 0x0e, 0x26, 0xe8, 0x3b, 0x2a, 0xc5, 0xb9,
            0xe2, 0x9e, 0x1b, 0x16, 0x1e, 0x5c, 0x1f, 0xa7, 0x42, 0x5e, 0x73, 0x04, 0x33, 0x62,
            0x93, 0x8b, 0x98, 0x24,
        ]
    );
    let permit = machine
        .accept_notice(10, ticket, &notice_ack(&input, true), 20)
        .unwrap();
    assert_eq!(permit.1, b"hello");
    machine.input_written(permit, 30).unwrap();
    assert_eq!(machine.pending_correlations(), 1);

    let receipt_event = SemanticEvent {
        id: id(10),
        sequence: 2,
        kind: SemanticEventKind::ApplicationReceipt,
        exact_payload: b"receipt".to_vec().into(),
    };
    let receipt = ApplicationReceipt {
        application_id: id(9),
        lease_epoch: 3,
        request_id: 2,
    };
    let SemanticAdmission::Append(commit, ..) =
        machine.admit_receipt(10, &receipt_event, receipt).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(machine.pending_correlations(), 1);
    machine
        .committed(
            commit,
            EventPosition {
                epoch: 0,
                sequence: 1,
            },
        )
        .unwrap();
    assert_eq!(machine.pending_correlations(), 0);
}

#[test]
fn malformed_application_payload_bounds_are_refused_before_notice_or_write() {
    let mut machine = exact_machine();
    let mut input = application(b"never written");
    input.request.terminal_at = usize::MAX;
    assert!(matches!(
        machine.prepare_input(&input, 0),
        Err(SemanticRefusal::ApplicationConflict)
    ));
    assert_eq!(machine.pending_correlations(), 0);
}

#[test]
fn correlation_mismatch_source_loss_deadline_and_expiry_emit_once() {
    let mut machine = exact_machine();
    let input = application(b"x");
    let (ticket, _) = machine.prepare_input(&input, 0).unwrap();
    let permit = machine
        .accept_notice(10, ticket, &notice_ack(&input, true), 1)
        .unwrap();
    machine.input_written(permit.clone(), 2).unwrap();
    let bad = ApplicationReceipt {
        application_id: id(9),
        lease_epoch: 3,
        request_id: 99,
    };
    let event = SemanticEvent {
        id: id(10),
        sequence: 2,
        kind: SemanticEventKind::ApplicationReceipt,
        exact_payload: vec![1].into(),
    };
    assert_eq!(
        machine.admit_receipt(10, &event, bad),
        Err(SemanticRefusal::UnknownApplication)
    );
    let next = SemanticEvent {
        id: id(11),
        sequence: 2,
        kind: SemanticEventKind::Transition,
        exact_payload: b"{}".to_vec().into(),
    };
    let SemanticAdmission::Append(ticket, ..) = machine.admit(10, &next).unwrap() else {
        unreachable!()
    };
    assert_eq!(
        ticket.get(),
        permit.0.get() + 1,
        "an invalid receipt must not consume a shared operation ticket"
    );
    machine.failed(ticket).unwrap();
    let lost = machine.source_lost(10);
    assert!(
        matches!(&lost[..], [SemanticChange::Source(_), SemanticChange::Missing(effect)] if effect.reason == MissingReason::SourceLost
        && effect.receipt.lease_epoch == 3 && effect.receipt.request_id == 2)
    );
    assert!(machine.source_lost(10).is_empty());
    assert_eq!(
        machine
            .poll(60_002)
            .iter()
            .filter(|change| matches!(change,
        SemanticChange::Missing(effect) if effect.reason == MissingReason::Deadline))
            .count(),
        1
    );
    assert!(machine.poll(60_003).is_empty());
    assert_eq!(
        machine
            .poll(600_002)
            .iter()
            .filter(|change| matches!(change,
        SemanticChange::Missing(effect) if effect.reason == MissingReason::RetentionExpired))
            .count(),
        1
    );
    assert_eq!(machine.pending_correlations(), 0);
}

#[test]
fn epoch_event_identity_commit_failure_and_stream_health_are_fenced() {
    let mut machine = SemanticMachine::new(id(1), 7);
    connect(&mut machine, 10, &hello(SemanticMode::Stateful)).unwrap();
    assert_eq!(machine.semantic_flags(), 0);
    let snapshot = SemanticEvent {
        id: id(3),
        sequence: 1,
        kind: SemanticEventKind::Snapshot,
        exact_payload: b"{}".to_vec().into(),
    };
    let SemanticAdmission::Append(failed, ..) = machine.admit(10, &snapshot).unwrap() else {
        unreachable!()
    };
    machine.failed(failed).unwrap();
    let SemanticAdmission::Append(commit, ..) = machine.admit(10, &snapshot).unwrap() else {
        unreachable!()
    };
    machine
        .committed(
            commit,
            EventPosition {
                epoch: 0,
                sequence: 0,
            },
        )
        .unwrap();
    assert_eq!(machine.semantic_flags() & 1, 1);

    let reused = SemanticEvent {
        id: id(3),
        sequence: 2,
        kind: SemanticEventKind::Transition,
        exact_payload: b"{}".to_vec().into(),
    };
    assert_eq!(
        machine.admit(10, &reused),
        Err(SemanticRefusal::EventConflict)
    );
    machine.set_writable(false);
    let next = SemanticEvent {
        id: id(4),
        sequence: 2,
        kind: SemanticEventKind::Transition,
        exact_payload: b"{}".to_vec().into(),
    };
    assert_eq!(
        machine.admit(10, &next),
        Err(SemanticRefusal::ResourceExhausted)
    );
}

#[test]
fn unsupervised_zero_generation_and_heartbeat_degradation_are_explicit() {
    let mut machine = SemanticMachine::new(id(1), 1);
    let mut unsupervised = hello(SemanticMode::Stateful);
    unsupervised.generation = 0;
    connect_at(&mut machine, 10, &unsupervised, 100).unwrap();
    machine.heartbeat(10, 1_000).unwrap();
    assert!(machine.poll(15_999).is_empty());
    machine.poll(16_000);
    assert_eq!(machine.semantic_flags() & 2, 2);
}

#[test]
fn simultaneous_source_timeouts_are_processed_in_one_poll() {
    let mut machine = SemanticMachine::new(id(1), 7);
    for (conn, source) in [(10, b"alpha".as_slice()), (11, b"beta".as_slice())] {
        let mut producer = hello(SemanticMode::Stateful);
        producer.source = source.into();
        connect_at(&mut machine, conn, &producer, 0).unwrap();
    }
    let effects = machine.poll(15_000);
    let mut effects = effects
        .into_iter()
        .filter_map(|change| match change {
            SemanticChange::Source(effect) => Some(effect),
            SemanticChange::Missing(_) => None,
        })
        .collect::<Vec<_>>();
    effects.sort_by(|left, right| left.source.cmp(&right.source));
    assert_eq!(
        effects
            .iter()
            .map(|effect| (&effect.source[..], effect.status))
            .collect::<Vec<_>>(),
        [
            (b"alpha".as_slice(), SourceStatus::Degraded),
            (b"beta".as_slice(), SourceStatus::Degraded),
        ]
    );
}

#[test]
fn unwritable_event_storage_refuses_semantic_hello() {
    let mut machine = SemanticMachine::new(id(1), 7);
    machine.set_writable(false);
    assert_eq!(
        connect(&mut machine, 10, &hello(SemanticMode::Stateful)),
        Err(SemanticRefusal::ResourceExhausted)
    );
}

#[test]
fn losing_event_storage_identifies_semantic_connections_to_close() {
    let mut machine = SemanticMachine::new(id(1), 7);
    connect(&mut machine, 10, &hello(SemanticMode::Stateful)).unwrap();
    assert_eq!(format!("{:?}", machine.set_writable(false)), "[10]");
}

#[test]
fn absent_event_storage_has_no_semantic_capability() {
    let mut machine = SemanticMachine::new([0; 16], 7);
    let mut request = hello(SemanticMode::Stateful);
    request.token = [0; 16];
    assert_eq!(
        connect(&mut machine, 10, &request),
        Err(SemanticRefusal::CapabilityAbsent)
    );
}

#[test]
fn degraded_source_recovers_only_after_a_durable_snapshot() {
    let mut machine = exact_machine();
    machine.poll(15_000);
    assert_eq!(machine.semantic_flags() & 3, 2);
    let snapshot = SemanticEvent {
        id: id(11),
        sequence: 2,
        kind: SemanticEventKind::Snapshot,
        exact_payload: b"{}".to_vec().into(),
    };
    let SemanticAdmission::Append(ticket, ..) = machine.admit(10, &snapshot).unwrap() else {
        unreachable!()
    };
    machine
        .committed(
            ticket,
            EventPosition {
                epoch: 0,
                sequence: 1,
            },
        )
        .unwrap();
    assert_eq!(machine.semantic_flags() & 3, 1);
}

#[test]
fn degradation_does_not_hide_a_durable_duplicate_ack() {
    let mut machine = exact_machine();
    let event = SemanticEvent {
        id: id(11),
        sequence: 2,
        kind: SemanticEventKind::Transition,
        exact_payload: b"{}".to_vec().into(),
    };
    let SemanticAdmission::Append(ticket, ..) = machine.admit(10, &event).unwrap() else {
        unreachable!()
    };
    machine
        .committed(
            ticket,
            EventPosition {
                epoch: 3,
                sequence: 9,
            },
        )
        .unwrap();
    machine.poll(15_000);
    assert!(
        matches!(machine.admit(10, &event), Ok(SemanticAdmission::Immediate(ack))
        if ack.status == SemanticAckStatus::Duplicate && ack.position == Some(EventPosition { epoch: 3, sequence: 9 }))
    );
    let next = SemanticEvent {
        id: id(12),
        sequence: 3,
        ..event
    };
    assert_eq!(
        machine.admit(10, &next),
        Err(SemanticRefusal::SnapshotRequired)
    );
}

#[test]
fn disconnected_source_cannot_publish_a_recovery_snapshot() {
    let mut machine = exact_machine();
    machine.source_lost(10);
    let snapshot = SemanticEvent {
        id: id(11),
        sequence: 2,
        kind: SemanticEventKind::Snapshot,
        exact_payload: b"{}".to_vec().into(),
    };
    assert_eq!(
        machine.admit(10, &snapshot),
        Err(SemanticRefusal::Superseded)
    );
}

#[test]
fn input_notice_ack_at_deadline_cannot_authorize_a_write() {
    let mut machine = exact_machine();
    let input = application(b"x");
    let (ticket, _) = machine.prepare_input(&input, 0).unwrap();
    assert!(
        machine
            .accept_notice(10, ticket, &notice_ack(&input, true), 2_000)
            .is_err()
    );
}

#[test]
fn producer_replacement_after_a_completed_write_reports_written_then_source_lost() {
    // Schema §7.1 confines APPLICATION_SOURCE_UNAVAILABLE to replacement
    // *before* the write, where nothing is written. Here the PTY write
    // completed, so §7.2 requires a written receipt describing it, and
    // §10.3.4 requires the correlation to be retained and the lost producer
    // to be reported as application-receipt-missing{source-lost}. Reporting a
    // delivered input as refused is the one direction the spec forbids,
    // because the controller's recovery is to send the bytes again.
    let mut machine = exact_machine();
    let input = application(b"x");
    let (ticket, _) = machine.prepare_input(&input, 0).unwrap();
    let permit = machine
        .accept_notice(10, ticket, &notice_ack(&input, true), 1)
        .unwrap();
    connect_at(&mut machine, 11, &hello(SemanticMode::Stateful), 1).unwrap();
    assert_eq!(machine.input_written(permit, 2), Ok(()));
    let changes = machine.poll(3);
    assert!(
        changes.iter().any(|change| matches!(change,
            SemanticChange::Missing(effect)
                if effect.reason == MissingReason::SourceLost
                    && effect.receipt.application_id == input.receipt.application_id)),
        "{changes:?}"
    );
}

#[test]
fn producer_degradation_after_a_completed_write_reports_written_then_source_lost() {
    let mut machine = exact_machine();
    let input = application(b"x");
    let (ticket, _) = machine.prepare_input(&input, 0).unwrap();
    let permit = machine
        .accept_notice(10, ticket, &notice_ack(&input, true), 1)
        .unwrap();
    // The write must complete inside the 10 s input-lease deadline, and only
    // then may the 15 s heartbeat timeout degrade the source. The previous
    // revision polled at 15 000 before completing, which expired the lease and
    // suppressed the receipt entirely, so it asserted a refusal it never
    // actually exercised.
    assert_eq!(machine.input_written(permit, 2), Ok(()));
    let changes = machine.poll(15_001);
    assert!(
        changes.iter().any(|change| matches!(change,
            SemanticChange::Missing(effect)
                if effect.reason == MissingReason::SourceLost
                    && effect.receipt.application_id == input.receipt.application_id)),
        "{changes:?}"
    );
}

#[test]
fn input_notice_ack_must_echo_the_complete_application_tuple() {
    let mut machine = exact_machine();
    let input = application(b"x");
    let (ticket, _) = machine.prepare_input(&input, 0).unwrap();
    let mut ack = notice_ack(&input, true);
    ack.receipt.request_id += 1;
    assert!(machine.accept_notice(10, ticket, &ack, 1).is_err());
}

#[test]
fn unanswered_input_notices_expire_at_two_seconds() {
    let mut machine = exact_machine();
    let input = application(b"x");
    machine.prepare_input(&input, 0).unwrap();
    assert!(machine.expire_notices(1_999).is_empty());
    let expired = machine.expire_notices(2_000);
    assert_eq!(expired.len(), 1);
    // Schema §11 is a closed set and §10.2.9 requires the receipt to name the
    // reason: a notice that was never acknowledged is APPLICATION_NOTICE_TIMEOUT
    // (19), not APPLICATION_SOURCE_UNAVAILABLE (17).
    let receipt = InputReceipt::decode(&expired[0]).unwrap();
    assert_eq!((receipt.status, receipt.result), (1, 19));
    assert_eq!(machine.pending_correlations(), 0);
}

#[test]
fn a_receipt_naming_an_unwritten_correlation_is_application_not_written() {
    // Schema §14.8 assigns SEM_APPLICATION_NOT_WRITTEN (15) to a provider
    // receipt that arrives before the PTY write completed; folding it into
    // SEM_UNKNOWN_APPLICATION_REQUEST (9) loses the distinction between "I do
    // not know that request" and "that request has not been written yet".
    let mut machine = exact_machine();
    let input = application(b"x");
    let (ticket, _) = machine.prepare_input(&input, 0).unwrap();
    // Accepted notice, so the correlation exists and is bound, but the write
    // has deliberately not been completed.
    let _permit = machine
        .accept_notice(10, ticket, &notice_ack(&input, true), 1)
        .unwrap();
    let event = SemanticEvent {
        id: id(9),
        sequence: 2,
        kind: SemanticEventKind::ApplicationReceipt,
        exact_payload: b"receipt".to_vec().into(),
    };
    assert_eq!(
        machine.admit_receipt(10, &event, input.receipt),
        Err(SemanticRefusal::NotWritten)
    );
}

#[test]
fn a_bound_application_id_reports_conflict_not_capacity_exhaustion() {
    let mut machine = exact_machine();
    let input = application(b"x");
    let (ticket, _) = machine.prepare_input(&input, 0).unwrap();
    let permit = machine
        .accept_notice(10, ticket, &notice_ack(&input, true), 1)
        .unwrap();
    machine.input_written(permit, 2).unwrap();
    let error = machine.prepare_input(&input, 3).unwrap_err();
    assert_eq!(format!("{error:?}"), "ApplicationConflict");
}

#[test]
fn status_pending_count_includes_only_completed_transport_writes() {
    let mut machine = exact_machine();
    let input = application(b"x");
    let (ticket, _) = machine.prepare_input(&input, 0).unwrap();
    assert_eq!(machine.pending_correlations(), 0);
    let permit = machine
        .accept_notice(10, ticket, &notice_ack(&input, true), 1)
        .unwrap();
    assert_eq!(machine.pending_correlations(), 0);
    machine.input_written(permit, 2).unwrap();
    assert_eq!(machine.pending_correlations(), 1);
}

#[test]
fn semantic_source_sequence_exhaustion_never_wraps_or_becomes_bad_sequence() {
    assert_eq!(next_semantic_sequence(u64::MAX - 1), Ok(u64::MAX));
    assert_eq!(
        next_semantic_sequence(u64::MAX),
        Err(SemanticRefusal::ResourceExhausted)
    );
}
