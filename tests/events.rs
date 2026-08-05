use moor::events::{
    Axis, Cursor, Event, EventKind, EventStream, Json, application_receipt, canonical_event,
    canonical_header, event, semantic_assertion, semantic_changes,
};
use moor::session::{
    ApplicationReceipt, MissingReason, ReceiptProjection, SemanticChange, SemanticEffect,
    SemanticEvent, SemanticEventKind, SourceEffect, SourceReason, SourceStatus,
};

fn bare(name: &'static str, ts_ms: u64) -> Event {
    event(name, ts_ms, &[])
}

fn push_wide(payload: &mut Vec<u8>, value: &[u8]) -> std::ops::Range<usize> {
    payload.extend_from_slice(&(value.len() as u32).to_le_bytes());
    let start = payload.len();
    payload.extend_from_slice(value);
    start..payload.len()
}

#[test]
fn semantic_assertion_preserves_provenance_and_exact_payload_as_base64() {
    let semantic = SemanticEvent {
        id: [4; 16],
        sequence: u64::MAX,
        kind: SemanticEventKind::Snapshot,
        exact_payload: b"{\"x\":1}".as_slice().into(),
    };
    let event = semantic_assertion(1, b"provider", [2; 16], 3, &semantic).unwrap();
    let line = canonical_event(5, 6, EventKind::Transition, &event);
    assert!(line.contains("\"source\":\"provider\",\"producer\":\"AgICAgICAgICAgICAgICAg==\""));
    assert!(line.contains("\"source_epoch\":3,\"source_seq\":\"18446744073709551615\""));
    assert!(line.contains("\"assertion_kind\":\"snapshot\",\"payload\":\"eyJ4IjoxfQ==\""));
}

#[test]
fn semantic_changes_preserve_source_and_missing_order() {
    let source = |name: u8| {
        SemanticChange::Source(SourceEffect {
            source: vec![name].into(),
            producer: [name; 16],
            source_epoch: 1,
            status: SourceStatus::Disconnected,
            reason: SourceReason::Superseded,
        })
    };
    let missing = SemanticChange::Missing(SemanticEffect {
        receipt: ApplicationReceipt {
            application_id: [3; 16],
            lease_epoch: 2,
            request_id: 9,
        },
        source: b"middle".as_slice().into(),
        source_epoch: 1,
        producer: [4; 16],
        reason: MissingReason::SourceLost,
    });
    let changes = semantic_changes(7, vec![source(b'a'), missing, source(b'z')]).unwrap();
    let lines = changes
        .iter()
        .map(|event| canonical_event(0, 0, EventKind::Transition, event))
        .collect::<Vec<_>>();
    assert!(
        lines[0].contains("\"type\":\"semantic-source\"") && lines[0].contains("\"source\":\"a\"")
    );
    assert!(
        lines[1].contains("\"type\":\"application-receipt-missing\"")
            && lines[1].contains("\"source\":\"middle\"")
    );
    assert!(
        lines[2].contains("\"type\":\"semantic-source\"") && lines[2].contains("\"source\":\"z\"")
    );
}
fn cursor(epoch: u32, next_seq: u64, commit: u64) -> Cursor {
    Cursor(epoch, next_seq, 0, commit)
}

#[test]
fn canonical_header_and_event_have_fixed_order_and_exact_escaping() {
    assert_eq!(
        canonical_header(1_234, "c2Vzcw==", Some(7), Cursor(0, 1, 0, 1)),
        "{\"v\":2,\"type\":\"header\",\"ts\":1.234,\"session\":\"c2Vzcw==\",\"generation\":7,\"epoch\":0,\"next_seq\":1,\"first_retained\":0}\n"
    );
    let state = event(
        "state",
        2_001,
        &[
            ("state", Json::String("busy")),
            (
                "title",
                Json::String("é/\u{2028}\u{2029}\"\\\u{8}\t\n\u{c}\r\0\u{1}"),
            ),
            ("truncated", Json::Bool(false)),
        ],
    );
    assert_eq!(
        canonical_event(0, 0, EventKind::Transition, &state),
        "{\"type\":\"state\",\"ts\":2.001,\"epoch\":0,\"seq\":0,\"kind\":\"transition\",\"state\":\"busy\",\"title\":\"é/\u{2028}\u{2029}\\\"\\\\\\b\\t\\n\\f\\r\\u0000\\u0001\",\"truncated\":false}\n"
    );
    let exited = event(
        "exit",
        0,
        &[("ended", Json::String("exited")), ("code", Json::Number(7))],
    );
    assert!(
        canonical_event(0, 1, EventKind::Transition, &exited)
            .ends_with("\"ended\":\"exited\",\"code\":7}\n")
    );
    assert!(canonical_header(0, "cw==", None, Cursor(0, 0, 0, 1)).contains("\"generation\":null"));
}

#[test]
fn application_receipt_keeps_terminal_independent_from_both_provider_ids() {
    let mut payload = vec![5; 16];
    payload.extend_from_slice(&7u32.to_le_bytes());
    payload.extend_from_slice(&9u64.to_le_bytes());
    payload.push(0);
    let provider_session = push_wide(&mut payload, b"provider-session");
    let provider_turn = push_wide(&mut payload, b"provider-turn");
    let semantic = SemanticEvent {
        id: [3; 16],
        sequence: 4,
        kind: SemanticEventKind::ApplicationReceipt,
        exact_payload: payload.into(),
    };
    let line = canonical_event(
        1,
        2,
        EventKind::Transition,
        &application_receipt(
            6,
            b"source",
            [2; 16],
            3,
            &semantic,
            &ReceiptProjection {
                receipt: ApplicationReceipt {
                    application_id: [5; 16],
                    lease_epoch: 7,
                    request_id: 9,
                },
                status: 0,
                provider_session,
                provider_turn,
            },
        )
        .unwrap(),
    );
    assert!(line.contains("\"provider_session\":\"cHJvdmlkZXItc2Vzc2lvbg==\""));
    assert!(line.contains("\"provider_turn\":\"cHJvdmlkZXItdHVybg==\""));
}

#[test]
fn application_receipt_rejects_each_provider_id_above_4096_bytes() {
    for oversized in 0..2 {
        let ids = [
            vec![b'a'; if oversized == 0 { 4097 } else { 1 }],
            vec![b'b'; if oversized == 1 { 4097 } else { 1 }],
        ];
        let mut payload = vec![5; 16];
        payload.extend_from_slice(&7u32.to_le_bytes());
        payload.extend_from_slice(&9u64.to_le_bytes());
        payload.push(0);
        let provider_session = push_wide(&mut payload, &ids[0]);
        let provider_turn = push_wide(&mut payload, &ids[1]);
        let semantic = SemanticEvent {
            id: [3; 16],
            sequence: 4,
            kind: SemanticEventKind::ApplicationReceipt,
            exact_payload: payload.into(),
        };
        assert!(matches!(
            application_receipt(
                6,
                b"source",
                [2; 16],
                3,
                &semantic,
                &ReceiptProjection {
                    receipt: ApplicationReceipt {
                        application_id: [5; 16],
                        lease_epoch: 7,
                        request_id: 9,
                    },
                    status: 0,
                    provider_session,
                    provider_turn,
                },
            ),
            Err(moor::events::EventError::InvalidEvent)
        ));
    }
}

#[test]
fn a_multi_transition_transaction_allocates_dense_sequences_atomically() {
    let mut stream = EventStream::new();
    let (batch, cursor, exhausted) = stream
        .transact(&[], &[bare("ready", 11), bare("ready", 12)], false)
        .unwrap();
    let records = batch.lines().collect::<Vec<_>>();
    assert_eq!(exhausted, None);
    assert_eq!(cursor, Cursor(0, 2, 0, 2));
    assert!(records[0].contains("\"seq\":0,\"kind\":\"transition\""));
    assert!(records[1].contains("\"seq\":1,\"kind\":\"transition\""));
}

#[test]
fn sequence_exhaustion_rejects_the_whole_triggering_transaction() {
    let mut stream = EventStream::at(cursor(0, (1 << 53) - 2, 1));
    let (batch, cursor, exhausted) = stream
        .transact(&[], &[bare("ready", 11), bare("ready", 12)], false)
        .unwrap();
    let records = batch.lines().collect::<Vec<_>>();
    assert_eq!(exhausted, Some(Axis::Sequence));
    assert_eq!(records.len(), 1);
    assert!(records[0].contains("\"type\":\"stream-exhausted\""));
    assert!(!records[0].contains("ready"));
    assert_eq!((cursor.1, cursor.3), ((1 << 53) - 1, 2));
    assert!(matches!(
        stream.transact(&[], &[bare("ready", 13)], false),
        Err(moor::events::EventError::Closed)
    ));
}

#[test]
fn compaction_emits_snapshots_then_the_trigger_exactly_once() {
    let mut stream = EventStream::at(cursor(0, 2, 2));
    let trigger = event(
        "link",
        30,
        &[
            ("uri", Json::String("https://x")),
            ("truncated", Json::Bool(false)),
        ],
    );
    let (batch, cursor, exhausted) = stream
        .transact(&[bare("ready", 20)], &[trigger], true)
        .unwrap();
    let records = batch.lines().collect::<Vec<_>>();
    assert_eq!(exhausted, None);
    assert_eq!((cursor.0, cursor.2, cursor.1), (1, 2, 4));
    assert!(records[0].contains("\"seq\":2,\"kind\":\"snapshot\""));
    assert!(records[1].contains("\"seq\":3,\"kind\":\"transition\""));
    assert_eq!(
        records
            .iter()
            .filter(|line| line.contains("\"type\":\"link\""))
            .count(),
        1
    );
}

#[test]
fn exhaustion_precedence_is_sequence_then_epoch_then_commit() {
    let mut sequence = EventStream::at(cursor(u32::MAX, (1 << 53) - 2, u64::MAX - 1));
    let (_, _, exhausted) = sequence
        .transact(&[bare("ready", 1)], &[bare("ready", 2)], true)
        .unwrap();
    assert_eq!(exhausted, Some(Axis::Sequence));

    let mut epoch = EventStream::at(cursor(u32::MAX, 1, u64::MAX - 1));
    let (batch, _, exhausted) = epoch
        .transact(&[bare("ready", 1)], &[bare("ready", 2)], true)
        .unwrap();
    let records = batch.lines().collect::<Vec<_>>();
    assert_eq!(exhausted, Some(Axis::Epoch));
    assert_eq!(records.len(), 2);
    assert!(records[0].contains("\"type\":\"ready\""));
    assert!(records[1].contains("\"axis\":\"epoch\""));

    let mut commit = EventStream::at(cursor(0, 1, u64::MAX - 1));
    let (batch, _, exhausted) = commit
        .transact(&[bare("ready", 1)], &[bare("ready", 2)], true)
        .unwrap();
    let records = batch.lines().collect::<Vec<_>>();
    assert_eq!(exhausted, Some(Axis::Commit));
    assert_eq!(records.len(), 3);
    assert!(records[0].contains("\"kind\":\"snapshot\""));
    assert!(records[1].contains("\"type\":\"ready\""));
    assert!(records[2].contains("\"axis\":\"commit\""));
}
