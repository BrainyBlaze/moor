use moor::events::{
    Axis, Cursor, Event, EventKind, EventStream, Json, Limits, canonical_event, canonical_header,
    event,
};

fn bare(name: &str, ts_ms: u64) -> Event {
    event(name, ts_ms, &[]).unwrap()
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
    )
    .unwrap();
    assert_eq!(
        canonical_event(0, 0, EventKind::Transition, &state),
        "{\"type\":\"state\",\"ts\":2.001,\"epoch\":0,\"seq\":0,\"kind\":\"transition\",\"state\":\"busy\",\"title\":\"é/\u{2028}\u{2029}\\\"\\\\\\b\\t\\n\\f\\r\\u0000\\u0001\",\"truncated\":false}\n"
    );
    let exited = event(
        "exit",
        0,
        &[("ended", Json::String("exited")), ("code", Json::Number(7))],
    )
    .unwrap();
    assert!(
        canonical_event(0, 1, EventKind::Transition, &exited)
            .ends_with("\"ended\":\"exited\",\"code\":7}\n")
    );
    assert!(canonical_header(0, "cw==", None, Cursor(0, 0, 0, 1)).contains("\"generation\":null"));
}

#[test]
fn event_schema_rejects_unknown_or_misordered_fields() {
    assert!(matches!(
        event(
            "state",
            0,
            &[
                ("title", Json::String("x")),
                ("state", Json::String("busy")),
                ("truncated", Json::Bool(false)),
            ],
        ),
        Err(moor::events::EventError::InvalidEvent)
    ));
    assert!(matches!(
        event("unknown", 0, &[]),
        Err(moor::events::EventError::InvalidEvent)
    ));
}

#[test]
fn a_multi_transition_transaction_allocates_dense_sequences_atomically() {
    let mut stream = EventStream::new();
    let batch = stream
        .transact(vec![], vec![bare("ready", 11), bare("ready", 12)], false)
        .unwrap();
    assert_eq!(batch.exhausted, None);
    assert_eq!(batch.cursor, Cursor(0, 2, 0, 2));
    assert!(batch.records[0].contains("\"seq\":0,\"kind\":\"transition\""));
    assert!(batch.records[1].contains("\"seq\":1,\"kind\":\"transition\""));
}

#[test]
fn sequence_exhaustion_rejects_the_whole_triggering_transaction() {
    let mut stream = EventStream::at(cursor(0, 1, 1), Limits(2, 9, 9));
    let batch = stream
        .transact(vec![], vec![bare("ready", 11), bare("ready", 12)], false)
        .unwrap();
    assert_eq!(batch.exhausted, Some(Axis::Sequence));
    assert_eq!(batch.records.len(), 1);
    assert!(batch.records[0].contains("\"type\":\"stream-exhausted\""));
    assert!(!batch.records[0].contains("ready"));
    assert_eq!((batch.cursor.1, batch.cursor.3), (2, 2));
    assert!(matches!(
        stream.transact(vec![], vec![bare("ready", 13)], false),
        Err(moor::events::EventError::Closed)
    ));
}

#[test]
fn compaction_emits_snapshots_then_the_trigger_exactly_once() {
    let mut stream = EventStream::at(cursor(0, 2, 2), Limits::default());
    let trigger = event(
        "link",
        30,
        &[
            ("uri", Json::String("https://x")),
            ("truncated", Json::Bool(false)),
        ],
    )
    .unwrap();
    let batch = stream
        .transact(vec![bare("ready", 20)], vec![trigger], true)
        .unwrap();
    assert_eq!(batch.exhausted, None);
    assert_eq!((batch.cursor.0, batch.cursor.2, batch.cursor.1), (1, 2, 4));
    assert!(batch.records[0].contains("\"seq\":2,\"kind\":\"snapshot\""));
    assert!(batch.records[1].contains("\"seq\":3,\"kind\":\"transition\""));
    assert_eq!(
        batch
            .records
            .iter()
            .filter(|line| line.contains("\"type\":\"link\""))
            .count(),
        1
    );
}

#[test]
fn exhaustion_precedence_is_sequence_then_epoch_then_commit() {
    let mut sequence = EventStream::at(cursor(1, 1, 1), Limits(2, 1, 2));
    let batch = sequence
        .transact(vec![bare("ready", 1)], vec![bare("ready", 2)], true)
        .unwrap();
    assert_eq!(batch.exhausted, Some(Axis::Sequence));

    let mut epoch = EventStream::at(cursor(1, 1, 1), Limits(9, 1, 2));
    let batch = epoch
        .transact(vec![bare("ready", 1)], vec![bare("ready", 2)], true)
        .unwrap();
    assert_eq!(batch.exhausted, Some(Axis::Epoch));
    assert_eq!(batch.records.len(), 2);
    assert!(batch.records[0].contains("\"type\":\"ready\""));
    assert!(batch.records[1].contains("\"axis\":\"epoch\""));

    let mut commit = EventStream::at(cursor(0, 1, 1), Limits(9, 1, 2));
    let batch = commit
        .transact(vec![bare("ready", 1)], vec![bare("ready", 2)], true)
        .unwrap();
    assert_eq!(batch.exhausted, Some(Axis::Commit));
    assert_eq!(batch.records.len(), 3);
    assert!(batch.records[0].contains("\"kind\":\"snapshot\""));
    assert!(batch.records[1].contains("\"type\":\"ready\""));
    assert!(batch.records[2].contains("\"axis\":\"commit\""));
}
