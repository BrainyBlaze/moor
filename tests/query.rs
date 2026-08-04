use moor::session::{QueryAction, QueryContext, QueryMachine};
use moor::wire::{Query, recognize_query, validate_query_reply};

#[test]
fn closed_query_and_reply_grammars_cover_csi7_and_csi8() {
    for bytes in [b"\x1b[c".as_slice(), b"\x9b0c", b"\x1b[>0q", b"\x9b6n"] {
        assert!(recognize_query(bytes).is_some(), "{bytes:?}");
    }
    let mode = recognize_query(b"\x1b[?2004$p").unwrap();
    assert_eq!((mode.class, mode.mode), (4, Some(2004)));
    assert!(recognize_query(b"\x1b[?02004$p").is_none());
    assert!(recognize_query(b"\x1b[?4294967296$p").is_none());
    assert!(validate_query_reply(&mode, b"\x9b?2004;1$y"));
    assert!(!validate_query_reply(&mode, b"\x1b[?2005;1$y"));
}

#[test]
fn delegation_precedes_raw_release_and_valid_reply_reaches_child() {
    let mut machine = QueryMachine::new();
    let context = QueryContext {
        owner: Some((7, 3)),
        synthetic: Some(b"synthetic".to_vec()),
    };
    let actions = machine.recognize(0, b"\x1b[c", context);
    let query = match &actions[..] {
        [
            QueryAction::Delegate { conn: 7, query },
            QueryAction::Release(raw),
        ] if raw == b"\x1b[c" => query.clone(),
        other => panic!("unexpected {other:?}"),
    };
    let reply = Query {
        bytes: b"\x1b[?62;4c".to_vec(),
        ..query
    };
    assert_eq!(
        machine.reply(1, 7, &reply),
        vec![QueryAction::ChildReply(reply.bytes)]
    );
    assert_eq!(machine.pending(), 0);
}

#[test]
fn malformed_reply_stays_pending_until_synthetic_deadline() {
    let mut machine = QueryMachine::new();
    let context = QueryContext {
        owner: Some((7, 3)),
        synthetic: Some(b"fallback".to_vec()),
    };
    let actions = machine.recognize(0, b"\x1b[c", context);
    let mut query = match &actions[0] {
        QueryAction::Delegate { query, .. } => query.clone(),
        _ => unreachable!(),
    };
    query.bytes = b"bad".to_vec();
    assert!(machine.reply(1, 7, &query).is_empty());
    assert_eq!(machine.pending(), 1);
    assert_eq!(
        machine.poll(250),
        vec![QueryAction::ChildReply(b"fallback".to_vec())]
    );
}

#[test]
fn correlation_capacity_and_u64_exhaustion_disconnect_once_and_never_wrap() {
    let context = QueryContext {
        owner: Some((7, 3)),
        synthetic: None,
    };
    let mut full = QueryMachine::new();
    for _ in 0..64 {
        full.recognize(0, b"\x1b[c", context.clone());
    }
    let overloaded = full.recognize(0, b"\x1b[c", context.clone());
    assert!(matches!(
        overloaded.first(),
        Some(QueryAction::Disconnect { conn: 7 })
    ));
    assert_eq!(full.pending(), 0);

    let mut final_id = QueryMachine::with_next(u64::MAX);
    let first = final_id.recognize(0, b"\x1b[c", context.clone());
    assert!(
        matches!(&first[0], QueryAction::Delegate { query, .. } if query.correlation == u64::MAX)
    );
    assert!(!final_id.delegation_allocatable());
    let exhausted = final_id.recognize(1, b"\x1b[c", context.clone());
    assert!(format!("{exhausted:?}").contains("ResourceExhausted"));
    assert!(matches!(
        exhausted.get(1),
        Some(QueryAction::Disconnect { conn: 7 })
    ));
    let later = final_id.recognize(2, b"\x1b[c", context);
    assert!(!later.iter().any(|a| matches!(
        a,
        QueryAction::Delegate { .. } | QueryAction::Disconnect { .. }
    )));
}

#[test]
fn reply_at_or_after_deadline_is_discarded_even_without_poll() {
    let mut machine = QueryMachine::new();
    let actions = machine.recognize(
        0,
        b"\x1b[c",
        QueryContext {
            owner: Some((7, 3)),
            synthetic: Some(b"fallback".to_vec()),
        },
    );
    let mut reply = match &actions[0] {
        QueryAction::Delegate { query, .. } => query.clone(),
        _ => unreachable!(),
    };
    reply.bytes = b"\x1b[?62;4c".to_vec();
    assert_eq!(
        machine.reply(250, 7, &reply),
        vec![QueryAction::ChildReply(b"fallback".to_vec())]
    );
    assert_eq!(machine.pending(), 0);
}
