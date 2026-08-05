use moor::events::{
    Cursor, EventKind, EventStream, Json, canonical_event, canonical_header, event,
    semantic_assertion as assertion_event, semantic_source,
};
use moor::runtime::private::{lifecycle_exit, lifecycle_running};
use moor::runtime::storage::{Done, EventConfig, Purpose, SessionStorage, StorageError};
use moor::session::{SemanticEvent, SemanticEventKind, SourceEffect, SourceReason, SourceStatus};
use moor::store::{Kind, Store};
use moor::terminal::Observation;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[allow(clippy::too_many_arguments)]
fn semantic_assertion(
    ts: u64,
    source: &[u8],
    producer: [u8; 16],
    epoch: u32,
    id: [u8; 16],
    sequence: u64,
    snapshot: bool,
    payload: &[u8],
) -> Result<moor::events::Event, moor::events::EventError> {
    assertion_event(
        ts,
        source,
        producer,
        epoch,
        &SemanticEvent {
            id,
            sequence,
            kind: if snapshot {
                SemanticEventKind::Snapshot
            } else {
                SemanticEventKind::Transition
            },
            exact_payload: payload.into(),
        },
    )
}

fn temp(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    std::env::temp_dir().join(format!(
        "moor-storage-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn running(generation: u32) -> String {
    lifecycle_running(
        &[1, b'/', b's'],
        ((generation != 1).then_some(generation), generation),
        [2; 16],
        (1, 2, [3; 16]),
        ("posix-bytes", None, None),
    )
}

fn completed(storage: &mut SessionStorage, purpose: Purpose) -> Done {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(done) = storage
            .poll()
            .into_iter()
            .find(|done| done.purpose == purpose)
        {
            return done;
        }
        assert!(
            Instant::now() < deadline,
            "storage operation did not complete"
        );
        std::thread::yield_now();
    }
}

fn wait(storage: &mut SessionStorage, purpose: Purpose) {
    completed(storage, purpose).result.unwrap();
}

#[test]
fn output_events_clear_and_lifecycle_use_bounded_worker_lanes() {
    let log_path = temp("log");
    let event_path = temp("event");
    let exit_path = temp("exit");
    let log = Store::create(&log_path, Kind::Log, 1, b"", 0, 0).unwrap();
    let header = canonical_header(10, "AS9z", None, Cursor(0, 0, 0, 1));
    let event_store = Store::create(&event_path, Kind::Event, 1, header.as_bytes(), 0, 0).unwrap();
    let running = running(1);
    let lifecycle = Store::create(&exit_path, Kind::Exit, 1, running.as_bytes(), 0, 0).unwrap();
    let mut storage = SessionStorage::new(
        Some((log, 4)),
        Some(EventConfig {
            store: event_store,
            stream: EventStream::new(),
            created: 10,
            session: "AS9z".into(),
            generation: None,
        }),
        lifecycle,
        8,
        4096,
    );

    storage.output(b"abcdef".to_vec().into(), 6).unwrap();
    wait(&mut storage, Purpose::Background);
    let (commit, body) = Store::read_only(&log_path, Kind::Log, 1).unwrap();
    assert_eq!((commit.start, commit.end, body), (2, 6, b"cdef".to_vec()));

    storage.observe(Observation::Ready).unwrap();
    wait(&mut storage, Purpose::Background);
    let (_, body) = Store::read_only(&event_path, Kind::Event, 1).unwrap();
    assert!(
        String::from_utf8(body)
            .unwrap()
            .contains("\"type\":\"ready\"")
    );
    storage
        .commit(
            Purpose::Background,
            &[event(
                "exit",
                20,
                &[("ended", Json::String("exited")), ("code", Json::Number(7))],
            )],
        )
        .unwrap();
    wait(&mut storage, Purpose::Background);
    let effect = SourceEffect {
        source: b"durable-source".as_slice().into(),
        producer: [8; 16],
        source_epoch: 1,
        status: SourceStatus::Connected,
        reason: SourceReason::None,
    };
    storage
        .commit(
            Purpose::Sources(77, true),
            &[semantic_source(20, &effect).unwrap()],
        )
        .unwrap();
    wait(&mut storage, Purpose::Sources(77, true));
    let (event_commit, body) = Store::read_only(&event_path, Kind::Event, 1).unwrap();
    let body = String::from_utf8(body).unwrap();
    assert_eq!(
        (event_commit.epoch, event_commit.start, event_commit.end),
        (0, 0, 3)
    );
    assert!(body.contains("\"type\":\"ready\""));
    assert!(body.contains("\"type\":\"exit\""));
    assert!(body.contains("\"source\":\"durable-source\""));

    storage.clear(41, commit.index, 6).unwrap();
    wait(&mut storage, Purpose::Clear(41, commit.index));
    assert!(
        Store::read_only(&log_path, Kind::Log, 1)
            .unwrap()
            .1
            .is_empty()
    );
    storage.clear(43, commit.index, 6).unwrap();
    assert!(
        completed(&mut storage, Purpose::Clear(43, commit.index))
            .result
            .unwrap()
            .1
    );

    storage
        .lifecycle(
            lifecycle_exit(&running, 20, 6, "\"ended\":\"exited\",\"code\":0").into_bytes(),
            6,
        )
        .unwrap();
    wait(&mut storage, Purpose::Lifecycle);
    assert_eq!(storage.health() & 0b111, 0b111);
    for path in [log_path, event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn rejected_event_admission_does_not_advance_the_durable_cursor() {
    let event_path = temp("event-rollback");
    let exit_path = temp("exit-rollback");
    let header = canonical_header(10, "AS9z", None, Cursor(0, 0, 0, 1));
    let events = Store::create(&event_path, Kind::Event, 1, header.as_bytes(), 0, 0).unwrap();
    let running = running(1);
    let lifecycle = Store::create(&exit_path, Kind::Exit, 1, running.as_bytes(), 0, 0).unwrap();
    let mut storage = SessionStorage::new(
        None,
        Some(EventConfig {
            store: events,
            stream: EventStream::new(),
            created: 10,
            session: "AS9z".into(),
            generation: None,
        }),
        lifecycle,
        2,
        512,
    );
    let uri = "x".repeat(2048);
    let oversized = event(
        "link",
        11,
        &[
            ("uri", Json::String(&uri)),
            ("truncated", Json::Bool(false)),
        ],
    );
    assert_eq!(
        storage.commit(Purpose::Semantic(7, false), &[oversized]),
        Err(StorageError::Busy)
    );
    storage.observe(Observation::Ready).unwrap();
    wait(&mut storage, Purpose::Background);
    let body = Store::read_only(&event_path, Kind::Event, 1).unwrap().1;
    assert!(String::from_utf8(body).unwrap().contains("\"seq\":0"));
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn event_history_compacts_only_at_the_byte_cap_and_keeps_snapshot_then_trigger() {
    let event_path = temp("event-cap");
    let exit_path = temp("exit-cap");
    let header = canonical_header(10, "AS9z", None, Cursor(0, 0, 0, 1));
    let events = Store::create(&event_path, Kind::Event, 1, header.as_bytes(), 0, 0).unwrap();
    let running = running(1);
    let lifecycle = Store::create(&exit_path, Kind::Exit, 1, running.as_bytes(), 0, 0).unwrap();
    let mut storage = SessionStorage::new(
        None,
        Some(EventConfig {
            store: events,
            stream: EventStream::new(),
            created: 10,
            session: "AS9z".into(),
            generation: None,
        }),
        lifecycle,
        64,
        4 << 20,
    );
    for (tag, source) in [(1, b"z".as_slice()), (2, b"a".as_slice())] {
        let effect = SourceEffect {
            source: source.into(),
            producer: [tag; 16],
            source_epoch: 1,
            status: SourceStatus::Connected,
            reason: SourceReason::None,
        };
        storage
            .commit(
                Purpose::Background,
                &[semantic_source(11, &effect).unwrap()],
            )
            .unwrap();
        wait(&mut storage, Purpose::Background);
        storage
            .commit(
                Purpose::Semantic(tag.into(), false),
                &[
                    semantic_assertion(12, source, [tag; 16], 1, [tag + 2; 16], 1, true, b"{}")
                        .unwrap(),
                ],
            )
            .unwrap();
        wait(&mut storage, Purpose::Semantic(tag.into(), false));
    }
    for sequence in 0..140 {
        let uri = format!("{sequence:03}-{}", "x".repeat(2044));
        storage.observe(Observation::Link(uri, false)).unwrap();
        wait(&mut storage, Purpose::Background);
    }
    let (commit, body) = Store::read_only(&event_path, Kind::Event, 1).unwrap();
    let body = String::from_utf8(body).unwrap();
    assert!(commit.epoch > 0);
    assert!(body.len() <= 256 << 10);
    assert!(body.contains("\"kind\":\"snapshot\""));
    assert!(body.contains("\"kind\":\"transition\""));
    assert!(body.contains("139-"));
    assert!(!body.contains("000-"));
    let lines = body
        .lines()
        .filter(|line| line.contains("semantic-"))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 4);
    assert!(
        lines
            .iter()
            .all(|line| line.contains("\"kind\":\"snapshot\""))
    );
    assert!(
        lines[0].contains("\"type\":\"semantic-source\"") && lines[0].contains("\"source\":\"a\"")
    );
    assert!(
        lines[1].contains("\"type\":\"semantic-source\"") && lines[1].contains("\"source\":\"z\"")
    );
    assert!(
        lines[2].contains("\"type\":\"semantic-assertion\"")
            && lines[2].contains("\"source\":\"a\"")
    );
    assert!(
        lines[3].contains("\"type\":\"semantic-assertion\"")
            && lines[3].contains("\"source\":\"z\"")
    );
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn compaction_may_exceed_the_cap_only_for_its_occurrence_trigger() {
    let event_path = temp("event-overage");
    let exit_path = temp("exit-overage");
    let header = canonical_header(10, "AS9z", None, Cursor(0, 0, 0, 1));
    let events = Store::create(&event_path, Kind::Event, 1, header.as_bytes(), 0, 0).unwrap();
    let running = running(1);
    let lifecycle = Store::create(&exit_path, Kind::Exit, 1, running.as_bytes(), 0, 0).unwrap();
    let mut storage = SessionStorage::new(
        None,
        Some(EventConfig {
            store: events,
            stream: EventStream::new(),
            created: 10,
            session: "AS9z".into(),
            generation: None,
        }),
        lifecycle,
        64,
        4 << 20,
    );
    let payload = format!(r#"{{"x":"{}"}}"#, "x".repeat(32_760));
    assert_eq!(payload.len(), 32_768);
    for tag in 1..=5u8 {
        let event = semantic_assertion(
            11,
            &[b'a' + tag],
            [tag; 16],
            1,
            [tag; 16],
            1,
            true,
            payload.as_bytes(),
        )
        .unwrap();
        storage
            .commit(Purpose::Semantic(tag.into(), false), &[event])
            .unwrap();
        wait(&mut storage, Purpose::Semantic(tag.into(), false));
    }
    let trigger = semantic_assertion(
        12,
        b"edge",
        [9; 16],
        1,
        [9; 16],
        1,
        false,
        payload.as_bytes(),
    )
    .unwrap();
    storage
        .commit(Purpose::Semantic(9, false), &[trigger])
        .expect("the occurrence trigger has one bounded overage allowance");
    wait(&mut storage, Purpose::Semantic(9, false));
    let (commit, body) = Store::read_only(&event_path, Kind::Event, 1).unwrap();
    assert!(body.len() > 256 << 10 && body.len() <= 320 << 10);
    assert!(commit.epoch > 0);
    assert_eq!(
        String::from_utf8(body)
            .unwrap()
            .matches("\"kind\":\"transition\"")
            .count(),
        1
    );
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn stateful_source_admission_reserves_mandatory_maximum_baseline() {
    let event_path = temp("event-reserve");
    let exit_path = temp("exit-reserve");
    let header = canonical_header(10, "AS9z", None, Cursor(0, 0, 0, 1));
    let events = Store::create(&event_path, Kind::Event, 1, header.as_bytes(), 0, 0).unwrap();
    let lifecycle = Store::create(&exit_path, Kind::Exit, 1, running(1).as_bytes(), 0, 0).unwrap();
    let mut storage = SessionStorage::new(
        None,
        Some(EventConfig {
            store: events,
            stream: EventStream::new(),
            created: 10,
            session: "AS9z".into(),
            generation: None,
        }),
        lifecycle,
        64,
        4 << 20,
    );
    let payload = format!(r#"{{"x":"{}"}}"#, "x".repeat(32_760));
    for tag in 1..=5u8 {
        storage
            .commit(
                Purpose::Semantic(tag.into(), false),
                &[semantic_assertion(
                    11,
                    &[b'a' + tag],
                    [tag; 16],
                    1,
                    [tag; 16],
                    1,
                    true,
                    payload.as_bytes(),
                )
                .unwrap()],
            )
            .unwrap();
        wait(&mut storage, Purpose::Semantic(tag.into(), false));
    }
    let filler = format!(r#"{{"x":"{}"}}"#, "x".repeat(4_088));
    storage
        .commit(
            Purpose::Semantic(6, false),
            &[
                semantic_assertion(11, b"z", [6; 16], 1, [6; 16], 1, true, filler.as_bytes())
                    .unwrap(),
            ],
        )
        .unwrap();
    wait(&mut storage, Purpose::Semantic(6, false));
    let mut accepted = 0;
    for tag in 0..64u8 {
        let effect = SourceEffect {
            source: format!("{tag:02}-{}", "a".repeat(125)).into_bytes().into(),
            producer: [tag; 16],
            source_epoch: u32::MAX,
            status: SourceStatus::Connected,
            reason: SourceReason::None,
        };
        match storage.commit(
            Purpose::Background,
            &[semantic_source(u64::MAX, &effect).unwrap()],
        ) {
            Ok(()) => {
                accepted += 1;
                wait(&mut storage, Purpose::Background);
            }
            Err(StorageError::Busy) => break,
            other => panic!("unexpected admission result: {other:?}"),
        }
    }
    assert!(
        accepted < 64,
        "byte projection must bind before the independent source-count cap"
    );
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn sequence_exhaustion_while_compacting_preserves_the_append_prefix() {
    let limit = (1 << 53) - 1;
    let probe = limit - 1_000;
    let link = |sequence: u64| {
        let uri = format!("{sequence:03}-{}", "x".repeat(2044));
        event(
            "link",
            11,
            &[
                ("uri", Json::String(&uri)),
                ("truncated", Json::Bool(false)),
            ],
        )
    };
    let retained = semantic_assertion(11, b"source", [1; 16], 1, [2; 16], 1, true, b"{}").unwrap();
    let mut records = canonical_event(0, probe, EventKind::Transition, &retained);
    let trigger = (1..1000)
        .find_map(|sequence| {
            records.push_str(&canonical_event(
                0,
                probe + sequence,
                EventKind::Transition,
                &link(sequence),
            ));
            let next = sequence + 1;
            (canonical_header(10, "AS9z", None, Cursor(0, probe + next, probe, 1)).len()
                + records.len()
                > 256 << 10)
                .then_some(next)
        })
        .expect("bounded link records must reach the cap");

    let event_path = temp("sequence-terminal-compaction");
    let exit_path = temp("sequence-terminal-exit");
    let first = limit - trigger;
    let header = canonical_header(10, "AS9z", None, Cursor(0, first, first, 1));
    let events =
        Store::create(&event_path, Kind::Event, 1, header.as_bytes(), first, first).unwrap();
    let lifecycle = Store::create(&exit_path, Kind::Exit, 1, running(1).as_bytes(), 0, 0).unwrap();
    let mut storage = SessionStorage::new(
        None,
        Some(EventConfig {
            store: events,
            created: 10,
            session: "AS9z".into(),
            generation: None,
            stream: EventStream::at(Cursor(0, first, first, 1)),
        }),
        lifecycle,
        64,
        4 << 20,
    );
    storage
        .commit(Purpose::Semantic(1, false), &[retained])
        .unwrap();
    wait(&mut storage, Purpose::Semantic(1, false));
    for sequence in 1..trigger - 1 {
        storage
            .commit(Purpose::Background, &[link(sequence)])
            .unwrap();
        wait(&mut storage, Purpose::Background);
    }
    assert_eq!(
        storage.commit(Purpose::Background, &[link(trigger - 1)]),
        Err(StorageError::Disabled)
    );
    assert_eq!(storage.health() & 2, 0);
    wait(&mut storage, Purpose::Background);

    let (commit, body) = Store::read_only(&event_path, Kind::Event, 1).unwrap();
    let body = String::from_utf8(body).unwrap();
    assert_eq!((commit.epoch, commit.start, commit.end), (0, first, limit));
    assert!(body.contains("\"type\":\"semantic-assertion\"") && body.contains("\"uri\":\"001-"));
    assert!(body.contains("\"type\":\"stream-exhausted\"") && body.contains("\"axis\":\"seq\""));
    assert!(!body.contains(&format!("\"uri\":\"{:03}-", trigger - 1)));
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn epoch_exhaustion_preserves_history_and_reports_the_trigger_position() {
    let epoch = u32::MAX;
    let link = |sequence: u64| {
        let uri = format!("{sequence:03}-{}", "x".repeat(2044));
        event(
            "link",
            11,
            &[
                ("uri", Json::String(&uri)),
                ("truncated", Json::Bool(false)),
            ],
        )
    };
    let retained = semantic_assertion(11, b"source", [1; 16], 1, [2; 16], 1, true, b"{}").unwrap();
    let mut records = canonical_event(epoch, 0, EventKind::Transition, &retained);
    let trigger = (1..1000)
        .find_map(|sequence| {
            records.push_str(&canonical_event(
                epoch,
                sequence,
                EventKind::Transition,
                &link(sequence),
            ));
            let next = sequence + 1;
            (canonical_header(10, "AS9z", None, Cursor(epoch, next, 0, 1)).len() + records.len()
                > 256 << 10)
                .then_some(next)
        })
        .unwrap();
    let event_path = temp("epoch-terminal-compaction");
    let exit_path = temp("epoch-terminal-exit");
    let initial = canonical_header(10, "AS9z", None, Cursor(0, 0, 0, 1));
    let events = Store::create(&event_path, Kind::Event, 1, initial.as_bytes(), 0, 0).unwrap();
    let header = canonical_header(10, "AS9z", None, Cursor(epoch, 0, 0, 1));
    let mut selected = *events.selected();
    selected.epoch = epoch;
    selected.length = header.len() as u64;
    selected.hash = Sha256::digest(header.as_bytes()).into();
    drop(events);
    fs::write(event_path.join("body.0"), &header).unwrap();
    fs::write(event_path.join("commit.0"), selected.encode()).unwrap();
    let events = Store::open(&event_path, Kind::Event, 1).unwrap();
    let lifecycle = Store::create(&exit_path, Kind::Exit, 1, running(1).as_bytes(), 0, 0).unwrap();
    let mut storage = SessionStorage::new(
        None,
        Some(EventConfig {
            store: events,
            stream: EventStream::at(Cursor(epoch, 0, 0, 1)),
            created: 10,
            session: "AS9z".into(),
            generation: None,
        }),
        lifecycle,
        64,
        4 << 20,
    );
    storage
        .commit(Purpose::Semantic(1, false), &[retained])
        .unwrap();
    wait(&mut storage, Purpose::Semantic(1, false));
    for sequence in 1..trigger - 1 {
        storage
            .commit(Purpose::Background, &[link(sequence)])
            .unwrap();
        wait(&mut storage, Purpose::Background);
    }
    storage
        .commit(Purpose::Semantic(99, false), &[link(trigger - 1)])
        .unwrap();
    let done = completed(&mut storage, Purpose::Semantic(99, true));
    let (commit, _) = done.result.unwrap();
    assert_eq!((commit.epoch, commit.end - 2), (epoch, trigger - 1));
    assert_eq!(commit.end, trigger + 1);
    assert_eq!(storage.health() & 2, 0);

    let (_, body) = Store::read_only(&event_path, Kind::Event, 1).unwrap();
    let body = String::from_utf8(body).unwrap();
    assert!(
        body.contains("\"uri\":\"001-") && body.contains(&format!("\"uri\":\"{:03}-", trigger - 1))
    );
    assert!(body.contains("\"axis\":\"epoch\""));
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn final_commit_reports_the_accepted_trigger_before_its_diagnostic() {
    let event_path = temp("commit-terminal");
    let exit_path = temp("commit-terminal-exit");
    let header = canonical_header(10, "AS9z", None, Cursor(0, 0, 0, u64::MAX - 1));
    let store = Store::create(&event_path, Kind::Event, 1, header.as_bytes(), 0, 0).unwrap();
    let mut high = *store.selected();
    high.index = u64::MAX - 1;
    drop(store);
    fs::write(event_path.join("commit.0"), high.encode()).unwrap();
    let store = Store::open(&event_path, Kind::Event, 1).unwrap();
    let lifecycle = Store::create(&exit_path, Kind::Exit, 1, running(1).as_bytes(), 0, 0).unwrap();
    let mut storage = SessionStorage::new(
        None,
        Some(EventConfig {
            store,
            stream: EventStream::at(Cursor(0, 0, 0, u64::MAX - 1)),
            created: 10,
            session: "AS9z".into(),
            generation: None,
        }),
        lifecycle,
        4,
        4096,
    );
    storage
        .commit(Purpose::Semantic(7, false), &[event("ready", 11, &[])])
        .unwrap();
    let done = completed(&mut storage, Purpose::Semantic(7, true));
    let (commit, _) = done.result.unwrap();
    assert_eq!(
        (commit.index, commit.end, commit.epoch, commit.end - 2),
        (u64::MAX, 2, 0, 0)
    );
    assert_eq!(storage.health() & 2, 0);
    let body = String::from_utf8(Store::read_only(&event_path, Kind::Event, 1).unwrap().1).unwrap();
    assert!(body.contains("\"type\":\"ready\"") && body.contains("\"axis\":\"commit\""));
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn shutdown_stages_lifecycle_before_one_ordered_final_event_transaction() {
    let event_path = temp("staged-final-events");
    let exit_path = temp("staged-final-lifecycle");
    let header = canonical_header(10, "AS9z", None, Cursor(0, 0, 0, 1));
    let event_store = Store::create(&event_path, Kind::Event, 1, header.as_bytes(), 0, 0).unwrap();
    let running = running(1);
    let lifecycle = Store::create(&exit_path, Kind::Exit, 1, running.as_bytes(), 0, 0).unwrap();
    let mut storage = SessionStorage::new(
        None,
        Some(EventConfig {
            store: event_store,
            stream: EventStream::new(),
            created: 10,
            session: "AS9z".into(),
            generation: None,
        }),
        lifecycle,
        8,
        4096,
    );
    storage
        .lifecycle(
            lifecycle_exit(&running, 20, 7, "\"ended\":\"exited\",\"code\":0").into_bytes(),
            7,
        )
        .unwrap();
    wait(&mut storage, Purpose::Lifecycle);
    assert!(
        String::from_utf8(Store::read_only(&event_path, Kind::Event, 1).unwrap().1)
            .unwrap()
            .lines()
            .count()
            == 1
    );

    let source = |name: &[u8], tag| {
        semantic_source(
            20,
            &SourceEffect {
                source: name.into(),
                producer: [tag; 16],
                source_epoch: 1,
                status: SourceStatus::Disconnected,
                reason: SourceReason::SessionEnding,
            },
        )
        .unwrap()
    };
    let exit = event(
        "exit",
        20,
        &[("ended", Json::String("exited")), ("code", Json::Number(0))],
    );
    storage
        .commit(Purpose::Final, &[source(b"a", 1), source(b"b", 2), exit])
        .unwrap();
    let done = completed(&mut storage, Purpose::Final);
    assert!(done.result.is_ok());
    let (commit, body) = Store::read_only(&event_path, Kind::Event, 1).unwrap();
    let body = String::from_utf8(body).unwrap();
    assert_eq!((commit.index, commit.end), (2, 3));
    let records = body.lines().skip(1).collect::<Vec<_>>();
    assert!(
        records[0].contains("\"source\":\"a\"")
            && records[1].contains("\"source\":\"b\"")
            && records[2].contains("\"type\":\"exit\"")
    );
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn mandatory_overflow_closes_only_its_lane_while_semantic_overflow_is_rejected() {
    let make = |name: &str| {
        let event_path = temp(&format!("{name}-event"));
        let exit_path = temp(&format!("{name}-exit"));
        let header = canonical_header(10, "AS9z", None, Cursor(0, 0, 0, 1));
        let events = Store::create(&event_path, Kind::Event, 1, header.as_bytes(), 0, 0).unwrap();
        let lifecycle =
            Store::create(&exit_path, Kind::Exit, 1, running(1).as_bytes(), 0, 0).unwrap();
        (
            SessionStorage::new(
                None,
                Some(EventConfig {
                    store: events,
                    stream: EventStream::new(),
                    created: 10,
                    session: "AS9z".into(),
                    generation: None,
                }),
                lifecycle,
                8,
                1,
            ),
            event_path,
            exit_path,
        )
    };

    let (mut semantic, event_path, exit_path) = make("semantic-overflow");
    assert_eq!(
        semantic.commit(Purpose::Semantic(1, false), &[event("ready", 11, &[])]),
        Err(StorageError::Busy)
    );
    assert_ne!(semantic.health() & 2, 0);
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }

    let (mut observed, event_path, exit_path) = make("mandatory-overflow");
    assert_eq!(
        observed.observe(Observation::Ready),
        Err(StorageError::Disabled)
    );
    assert_eq!(observed.health() & 2, 0);
    assert_ne!(observed.health() & 4, 0);
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }

    // A source transition the holder observed is mandatory: §8.4.4 reserves
    // storage for it precisely so it cannot be dropped, so overflowing the
    // queue with one closes the lane rather than returning a soft Busy that
    // the caller cannot honour. Returning Busy here dropped the degraded or
    // disconnected record while the stream still reported itself writable.
    let (mut mandatory, event_path, exit_path) = make("mandatory-source-overflow");
    assert_eq!(
        mandatory.commit(Purpose::Sources(5, true), &[event("ready", 11, &[])]),
        Err(StorageError::Disabled)
    );
    assert_eq!(mandatory.health() & 2, 0);
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }

    // A producer request carried on the same purpose is rejectable, so it is
    // refused before any state change and the stream stays open.
    let (mut rejectable, event_path, exit_path) = make("rejectable-source-overflow");
    assert_eq!(
        rejectable.commit(Purpose::Sources(5, false), &[event("ready", 11, &[])]),
        Err(StorageError::Busy)
    );
    assert_ne!(rejectable.health() & 2, 0);
    for path in [event_path, exit_path] {
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn maximum_terminal_snapshot_reservation_stays_fixed() {
    let title = "\u{1}".repeat(255);
    let uri = "\u{1}".repeat(2048);
    let size: usize = [
        event("ready", u64::MAX, &[]),
        event(
            "state",
            u64::MAX,
            &[
                ("state", Json::String("busy")),
                ("title", Json::String(&title)),
                ("truncated", Json::Bool(true)),
            ],
        ),
        event(
            "link",
            u64::MAX,
            &[("uri", Json::String(&uri)), ("truncated", Json::Bool(true))],
        ),
    ]
    .into_iter()
    .map(|event| canonical_event(u32::MAX, u64::MAX, EventKind::Snapshot, &event).len())
    .sum();
    assert_eq!(size, 14_210);
}
