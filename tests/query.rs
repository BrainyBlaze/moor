use moor::session::{Effect, Machine, Reply, Request, Transition};
use moor::wire::{QueryShape, recognize_query, validate_query_reply};
use std::sync::Arc;

#[test]
fn closed_query_and_reply_grammars_cover_csi7_and_csi8() {
    for bytes in [b"\x1b[c".as_slice(), b"\x9b0c", b"\x1b[>0q", b"\x9b6n"] {
        assert!(recognize_query(bytes).is_some(), "{bytes:?}");
    }
    let mode = recognize_query(b"\x1b[?2004$p").unwrap();
    assert_eq!((mode.class, mode.mode), (4, Some(2004)));
    assert!(recognize_query(b"\x1b[?02004$p").is_none());
    assert!(recognize_query(b"\x1b[?+2004$p").is_none());
    assert!(recognize_query(b"\x1b[?4294967296$p").is_none());
    assert!(validate_query_reply(&mode, b"\x9b?2004;1$y"));
    assert!(!validate_query_reply(&mode, b"\x1b[?2005;1$y"));
    assert!(!validate_query_reply(&mode, b"\x1b[?+2004;1$y"));
}

#[test]
fn correlation_exhaustion_reports_then_cancels_in_output_order() {
    let mut machine = Machine::new(7, [1; 16], [2; 16]).correlation(u64::MAX);
    machine.register_controller(7);
    machine
        .transition(Transition::Peer(
            0,
            7,
            Request::Attach(0, 0, true, false, Some([3; 16])),
        ))
        .unwrap();
    let shape = QueryShape {
        class: 1,
        csi8: false,
        mode: None,
    };
    let first = machine
        .transition(Transition::Query(
            1,
            Arc::from(b"\x1b[c".as_slice()),
            shape,
            Some(b"old".to_vec()),
        ))
        .unwrap();
    assert!(matches!(
        first.as_slice(),
        [Effect::QuerySend(7, query)] if query.correlation == u64::MAX
    ));

    let exhausted = machine
        .transition(Transition::Query(
            2,
            Arc::from(b"\x1b[c".as_slice()),
            shape,
            Some(b"new".to_vec()),
        ))
        .unwrap();
    assert!(matches!(
        exhausted.as_slice(),
        [
            Effect::Send(7, Reply::ControllerError(13, _)),
            Effect::Close(7),
            Effect::Write(old, old_bytes),
            Effect::Write(new, new_bytes),
        ] if old.get() == 0 && new.get() == 0 && old_bytes == b"old" && new_bytes == b"new"
    ));
    assert!(!machine.status(7).query_available);
}
