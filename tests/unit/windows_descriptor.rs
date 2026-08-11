use super::{
    DirectoryCause, directory_failure, directory_failure_record, exact_descriptor_semantics,
};

#[test]
fn exact_dacl_semantics_ignore_order_and_reject_every_difference() {
    let expected = [(0u8, 0u8, 0x1f01ffu32, 18u32), (0, 0, 0x1f01ff, 42)];
    let reversed = [expected[1], expected[0]];
    let matches = |actual: &[_], owner, protected| {
        exact_descriptor_semantics(
            owner,
            protected,
            actual.len(),
            expected.len(),
            |left, right| actual[left] == expected[right],
        )
    };

    assert!(matches(&reversed, true, true));
    assert!(!matches(&expected, false, true));
    assert!(!matches(&expected, true, false));
    assert!(!matches(&expected[..1], true, true));
    assert!(!matches(
        &[expected[0], expected[1], (0, 0, 0x1f01ff, 1)],
        true,
        true
    ));
    assert!(!matches(&[expected[0], expected[0]], true, true));
    assert!(!matches(&[expected[0], (0, 0, 0x120089, 42)], true, true));
    assert!(!matches(&[expected[0], (0, 2, 0x1f01ff, 42)], true, true));
    assert!(!matches(&[expected[0], (1, 0, 0x1f01ff, 42)], true, true));
}

#[test]
fn bootstrap_directory_failure_is_nonce_bound_and_closed() {
    let nonce = [7; 16];
    for cause in [
        DirectoryCause::Missing,
        DirectoryCause::NotDirectory,
        DirectoryCause::NotSearchable,
        DirectoryCause::Io,
    ] {
        let record = directory_failure_record(nonce, cause);
        assert_eq!(directory_failure(&record, nonce), Some(cause));
        assert_eq!(directory_failure(&record, [8; 16]), None);
    }
    let mut record = directory_failure_record(nonce, DirectoryCause::Missing);
    for at in [0, 28, 55] {
        record[at] ^= 0xff;
        assert_eq!(directory_failure(&record, nonce), None, "byte {at}");
        record[at] ^= 0xff;
    }
}
