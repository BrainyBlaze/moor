use moor::wire::{recognize_query, validate_query_reply};

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
