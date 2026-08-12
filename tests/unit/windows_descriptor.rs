use super::{
    BootstrapFailure, DirectoryCause, bootstrap_failure, bootstrap_failure_record,
    exact_descriptor_semantics,
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
        let failure = BootstrapFailure::Directory(cause);
        let record = bootstrap_failure_record(nonce, failure);
        assert_eq!(bootstrap_failure(&record, nonce), Some(failure));
        assert_eq!(bootstrap_failure(&record, [8; 16]), None);
    }
    let mut record = bootstrap_failure_record(
        nonce,
        BootstrapFailure::Directory(DirectoryCause::Missing),
    );
    for at in [0, 28, 55] {
        record[at] ^= 0xff;
        assert_eq!(bootstrap_failure(&record, nonce), None, "byte {at}");
        record[at] ^= 0xff;
    }
}

#[test]
fn bootstrap_execution_failure_preserves_the_nonce_and_os_error() {
    let nonce = [9; 16];
    let record = bootstrap_failure_record(nonce, BootstrapFailure::Execution(2));
    assert_eq!(
        bootstrap_failure(&record, nonce),
        Some(BootstrapFailure::Execution(2))
    );
    assert_eq!(bootstrap_failure(&record, [8; 16]), None);
    for at in [0, 12, 28, 33, 55] {
        let mut corrupt = record;
        corrupt[at] ^= 1;
        assert_eq!(bootstrap_failure(&corrupt, nonce), None, "byte {at}");
    }
    assert_eq!(
        bootstrap_failure(
            &bootstrap_failure_record(nonce, BootstrapFailure::Execution(0)),
            nonce
        ),
        None
    );
}
