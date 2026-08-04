use moor::session::{
    ApplicationInput, ApplicationReceipt, EventPosition, MissingReason, SemanticAckStatus,
    SemanticAdmission, SemanticEvent, SemanticEventKind, SemanticHello, SemanticMachine,
    SemanticMode, SemanticRefusal,
};

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
        source: b"claude".to_vec(),
    }
}

#[test]
fn semantic_ack_exists_only_after_durable_commit_and_duplicate_keeps_position() {
    let mut machine = SemanticMachine::new(id(1), 7);
    let accepted = machine.hello(10, &hello(SemanticMode::Stateful)).unwrap();
    assert!(accepted.snapshot_required);
    let event = SemanticEvent {
        id: id(3),
        sequence: 1,
        kind: SemanticEventKind::Snapshot,
        exact_payload: br#"{"state":"ready"}"#.to_vec(),
    };
    let ticket = match machine.admit(10, &event) {
        Ok(SemanticAdmission::Append(ticket)) => ticket,
        other => panic!("unexpected {other:?}"),
    };
    let ack = machine
        .committed(
            ticket,
            EventPosition {
                epoch: 0,
                sequence: 0,
            },
        )
        .unwrap();
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
}

#[test]
fn source_mode_is_fixed_and_stateful_replacement_gets_a_new_epoch() {
    let mut machine = SemanticMachine::new(id(1), 7);
    let first = machine.hello(10, &hello(SemanticMode::Stateful)).unwrap();
    let second = machine.hello(11, &hello(SemanticMode::Stateful)).unwrap();
    assert_eq!(second.epoch, first.epoch + 1);
    assert!(second.snapshot_required);
    assert_eq!(
        machine.hello(12, &hello(SemanticMode::Edge)),
        Err(SemanticRefusal::SourceConflict)
    );
}

#[test]
fn snapshot_gate_and_exact_payload_conflicts_fail_closed() {
    let mut machine = SemanticMachine::new(id(1), 7);
    machine.hello(10, &hello(SemanticMode::Stateful)).unwrap();
    let transition = SemanticEvent {
        id: id(4),
        sequence: 1,
        kind: SemanticEventKind::Transition,
        exact_payload: vec![1],
    };
    assert_eq!(
        machine.admit(10, &transition),
        Err(SemanticRefusal::SnapshotRequired)
    );
}

fn exact_machine() -> SemanticMachine {
    let mut machine = SemanticMachine::new(id(1), 7);
    machine.hello(10, &hello(SemanticMode::Stateful)).unwrap();
    let snapshot = SemanticEvent {
        id: id(8),
        sequence: 1,
        kind: SemanticEventKind::Snapshot,
        exact_payload: b"{}".to_vec(),
    };
    let SemanticAdmission::Append(ticket) = machine.admit(10, &snapshot).unwrap() else {
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
fn application_notice_precedes_write_and_correlation_resolves_only_after_commit() {
    let mut machine = exact_machine();
    let input = ApplicationInput {
        application_id: id(9),
        lease_epoch: 3,
        request_id: 2,
        source: b"claude".to_vec(),
        terminal: b"hello".to_vec(),
    };
    let (ticket, notice) = machine.prepare_input(&input, 10).unwrap();
    assert_eq!(
        notice.digest,
        [
            0x2c, 0xf2, 0x4d, 0xba, 0x5f, 0xb0, 0xa3, 0x0e, 0x26, 0xe8, 0x3b, 0x2a, 0xc5, 0xb9,
            0xe2, 0x9e, 0x1b, 0x16, 0x1e, 0x5c, 0x1f, 0xa7, 0x42, 0x5e, 0x73, 0x04, 0x33, 0x62,
            0x93, 0x8b, 0x98, 0x24,
        ]
    );
    let permit = machine.accept_notice(10, ticket, true, 20).unwrap();
    machine.input_written(permit, 30).unwrap();
    assert_eq!(machine.pending_correlations(), 1);

    let receipt_event = SemanticEvent {
        id: id(10),
        sequence: 2,
        kind: SemanticEventKind::ApplicationReceipt,
        exact_payload: b"receipt".to_vec(),
    };
    let receipt = ApplicationReceipt {
        application_id: id(9),
        lease_epoch: 3,
        request_id: 2,
    };
    let SemanticAdmission::Append(commit) =
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
fn correlation_mismatch_source_loss_deadline_and_expiry_emit_once() {
    let mut machine = exact_machine();
    let input = ApplicationInput {
        application_id: id(9),
        lease_epoch: 3,
        request_id: 2,
        source: b"claude".to_vec(),
        terminal: b"x".to_vec(),
    };
    let (ticket, _) = machine.prepare_input(&input, 0).unwrap();
    let permit = machine.accept_notice(10, ticket, true, 1).unwrap();
    machine.input_written(permit, 2).unwrap();
    let bad = ApplicationReceipt {
        application_id: id(9),
        lease_epoch: 3,
        request_id: 99,
    };
    let event = SemanticEvent {
        id: id(10),
        sequence: 2,
        kind: SemanticEventKind::ApplicationReceipt,
        exact_payload: vec![1],
    };
    assert_eq!(
        machine.admit_receipt(10, &event, bad),
        Err(SemanticRefusal::UnknownApplication)
    );
    assert_eq!(
        machine
            .source_lost(10, 3)
            .iter()
            .filter(|e| e.reason == MissingReason::SourceLost)
            .count(),
        1
    );
    assert!(machine.source_lost(10, 4).is_empty());
    assert_eq!(
        machine
            .poll(60_002)
            .iter()
            .filter(|e| e.reason == MissingReason::Deadline)
            .count(),
        1
    );
    assert!(machine.poll(60_003).is_empty());
    assert_eq!(
        machine
            .poll(600_002)
            .iter()
            .filter(|e| e.reason == MissingReason::RetentionExpired)
            .count(),
        1
    );
    assert_eq!(machine.pending_correlations(), 0);
}

#[test]
fn epoch_event_identity_commit_failure_and_stream_health_are_fenced() {
    let mut machine = SemanticMachine::new(id(1), 7);
    let hello = machine.hello(10, &hello(SemanticMode::Stateful)).unwrap();
    assert_eq!(machine.semantic_flags(), 0);
    let snapshot = SemanticEvent {
        id: id(3),
        sequence: 1,
        kind: SemanticEventKind::Snapshot,
        exact_payload: b"{}".to_vec(),
    };
    assert_eq!(
        machine.admit_epoch(10, hello.epoch - 1, &snapshot),
        Err(SemanticRefusal::Superseded)
    );
    let SemanticAdmission::Append(failed) =
        machine.admit_epoch(10, hello.epoch, &snapshot).unwrap()
    else {
        unreachable!()
    };
    machine.failed(failed).unwrap();
    let SemanticAdmission::Append(commit) =
        machine.admit_epoch(10, hello.epoch, &snapshot).unwrap()
    else {
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
        exact_payload: b"{}".to_vec(),
    };
    assert_eq!(
        machine.admit_epoch(10, hello.epoch, &reused),
        Err(SemanticRefusal::EventConflict)
    );
    machine.set_writable(false);
    let next = SemanticEvent {
        id: id(4),
        sequence: 2,
        kind: SemanticEventKind::Transition,
        exact_payload: b"{}".to_vec(),
    };
    assert_eq!(
        machine.admit_epoch(10, hello.epoch, &next),
        Err(SemanticRefusal::ResourceExhausted)
    );
}

#[test]
fn unsupervised_zero_generation_and_heartbeat_degradation_are_explicit() {
    let mut machine = SemanticMachine::new(id(1), 1);
    let mut unsupervised = hello(SemanticMode::Stateful);
    unsupervised.generation = 0;
    machine.hello_at(10, &unsupervised, 100).unwrap();
    machine.heartbeat(10, 1_000).unwrap();
    assert!(machine.poll(15_999).is_empty());
    machine.poll(16_000);
    assert_eq!(machine.semantic_flags() & 2, 2);
}
