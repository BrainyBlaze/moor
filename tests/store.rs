use moor::events::{
    Cursor, EventKind, Json, canonical_event, canonical_header, event, semantic_source,
};
use moor::runtime::private::{lifecycle_exit, lifecycle_running};
use moor::session::{SourceEffect, SourceReason, SourceStatus};
use moor::store::{Kind, Store, StoreError};
use moor::wire::crc32c;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn temp(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!(
        "moor-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    path
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

#[test]
fn empty_log_commit_matches_the_approved_92_byte_vector() {
    let path = temp("vector");
    let store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    assert_eq!(
        store.selected().encode(),
        [
            0x4d, 0x4f, 0x4f, 0x52, 0x43, 0x4d, 0x54, 0x31, 0x01, 0x00, 0x00, 0x02, 0x07, 0x00,
            0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55, 0xce, 0x64, 0xf3, 0xa0,
        ]
    );
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn recovery_rejects_zero_generation_even_when_the_caller_requests_zero() {
    let path = temp("zero-generation");
    drop(Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap());
    let slot = path.join("commit.0");
    let mut record = fs::read(&slot).unwrap();
    record[12..16].fill(0);
    let checksum = crc32c(&record[..88]);
    record[88..].copy_from_slice(&checksum.to_le_bytes());
    fs::write(slot, record).unwrap();
    assert!(matches!(
        Store::read_only(&path, Kind::Log, 0),
        Err(StoreError::Corrupt)
    ));
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn append_replace_recovery_and_uncommitted_tail_preserve_frontier() {
    let path = temp("store");
    let mut store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    assert_eq!(store.selected().index, 1);
    store.append_capped(b"abc", u64::MAX, 3).unwrap();
    assert_eq!(Store::read_only(&path, Kind::Log, 7).unwrap().1, b"abc");
    let body = path.join(format!("body.{}", store.selected().body));
    OpenOptions::new()
        .append(true)
        .open(body)
        .unwrap()
        .write_all(b"TAIL")
        .unwrap();
    drop(store);
    let mut recovered = Store::open(&path, Kind::Log, 7).unwrap();
    assert_eq!(Store::read_only(&path, Kind::Log, 7).unwrap().1, b"abc");
    recovered.replace(b"", 2, 3, 3).unwrap();
    assert_eq!(Store::read_only(&path, Kind::Log, 7).unwrap().1, b"");
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn capped_append_keeps_only_the_newest_suffix_and_validates_the_frontier() {
    let path = temp("capped-append");
    let mut store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    store.append_capped(b"abc", 4, 3).unwrap();
    let commit = store.append_capped(b"def", 4, 6).unwrap();
    assert_eq!((commit.epoch, commit.start, commit.end), (2, 2, 6));
    assert_eq!(Store::read_only(&path, Kind::Log, 7).unwrap().1, b"cdef");

    let commit = store.append_capped(b"ghijkl", 4, 12).unwrap();
    assert_eq!((commit.epoch, commit.start, commit.end), (3, 8, 12));
    assert_eq!(Store::read_only(&path, Kind::Log, 7).unwrap().1, b"ijkl");
    assert!(matches!(
        store.append_capped(b"x", 4, 99),
        Err(StoreError::Corrupt)
    ));
    assert_eq!(Store::read_only(&path, Kind::Log, 7).unwrap().1, b"ijkl");
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn torn_new_commit_falls_back_but_equal_valid_indexes_and_extra_entries_fail() {
    let path = temp("recovery");
    let mut store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    store.append_capped(b"abc", u64::MAX, 3).unwrap();
    fs::write(path.join("commit.1"), b"torn").unwrap();
    drop(store);
    assert_eq!(
        Store::open(&path, Kind::Log, 7).unwrap().selected().index,
        1
    );

    let mut equal = fs::read(path.join("commit.0")).unwrap();
    equal[9] = 1;
    let checksum = crc32c(&equal[..88]);
    equal[88..].copy_from_slice(&checksum.to_le_bytes());
    fs::write(path.join("commit.1"), equal).unwrap();
    assert!(matches!(
        Store::open(&path, Kind::Log, 7),
        Err(StoreError::Corrupt)
    ));
    fs::remove_file(path.join("commit.1")).unwrap();
    fs::write(path.join("extra"), b"").unwrap();
    assert!(matches!(
        Store::open(&path, Kind::Log, 7),
        Err(StoreError::Corrupt)
    ));
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn every_torn_replacement_component_preserves_the_prior_commit() {
    let path = temp("replacement-prefixes");
    let mut store = Store::create(&path, Kind::Log, 7, b"old", 0, 3).unwrap();
    store.replace(b"replacement", 2, 3, 14).unwrap();
    drop(store);
    let body = fs::read(path.join("body.1")).unwrap();
    let record = fs::read(path.join("commit.1")).unwrap();

    for length in 0..body.len() {
        fs::write(path.join("body.1"), &body[..length]).unwrap();
        let selected = Store::read_only(&path, Kind::Log, 7).unwrap();
        assert_eq!(
            (selected.0.index, selected.1.as_slice()),
            (1, b"old".as_slice())
        );
    }
    fs::write(path.join("body.1"), &body).unwrap();
    for length in 0..record.len() {
        fs::write(path.join("commit.1"), &record[..length]).unwrap();
        let selected = Store::read_only(&path, Kind::Log, 7).unwrap();
        assert_eq!(
            (selected.0.index, selected.1.as_slice()),
            (1, b"old".as_slice())
        );
    }
    fs::write(path.join("commit.1"), record).unwrap();
    assert_eq!(Store::read_only(&path, Kind::Log, 7).unwrap().1, body);
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn log_and_lifecycle_start_at_epoch_one_and_lifecycle_exits_once() {
    let log_path = temp("log-epoch");
    let log = Store::create(&log_path, Kind::Log, 7, b"", 0, 0).unwrap();
    assert_eq!(log.selected().epoch, 1);
    drop(log);
    fs::remove_dir_all(log_path).unwrap();

    let path = temp("lifecycle");
    let running = running(7);
    let exited = lifecycle_exit(&running, 3, 9, "\"ended\":\"exited\",\"code\":0");
    let mut store = Store::create(&path, Kind::Exit, 7, running.as_bytes(), 0, 0).unwrap();
    assert_eq!((store.selected().epoch, store.selected().index), (1, 1));
    store.replace(exited.as_bytes(), 1, 9, 9).unwrap();
    assert_eq!(store.selected().index, 2);
    assert!(matches!(
        store.replace(exited.as_bytes(), 1, 9, 9),
        Err(StoreError::Exhausted)
    ));
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn lifecycle_records_are_canonical_closed_and_identity_bound() {
    let valid = running(7);
    for (name, malformed) in [
        (
            "lifecycle-extra",
            valid.replace(
                "\"instrument_path\":null",
                "\"instrument_path\":null,\"extra\":0",
            ),
        ),
        (
            "lifecycle-wire",
            valid.replace("\"wire_generation\":7", "\"wire_generation\":8"),
        ),
        ("lifecycle-base64", valid.replace("AS9z", "AS9z=")),
        (
            "lifecycle-truncated",
            valid.replace(",\"instrument_path\":null", ""),
        ),
    ] {
        let path = temp(name);
        assert!(matches!(
            Store::create(&path, Kind::Exit, 7, malformed.as_bytes(), 0, 0),
            Err(StoreError::Corrupt)
        ));
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn malformed_event_and_hostile_length_fail_without_panicking() {
    let malformed = temp("malformed-event");
    assert!(matches!(
        Store::create(&malformed, Kind::Event, 7, b"garbage\n", 0, 0),
        Err(StoreError::Corrupt)
    ));
    let _ = fs::remove_dir_all(malformed);

    let path = temp("hostile-length");
    let store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    drop(store);
    let mut encoded = fs::read(path.join("commit.0")).unwrap();
    encoded[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
    encoded[48..56].copy_from_slice(&u64::MAX.to_le_bytes());
    let crc = crc32c(&encoded[..88]);
    encoded[88..92].copy_from_slice(&crc.to_le_bytes());
    fs::write(path.join("commit.0"), encoded).unwrap();
    let opened = std::panic::catch_unwind(|| Store::open(&path, Kind::Log, 7));
    assert!(matches!(opened, Ok(Err(StoreError::Corrupt))));
    fs::remove_dir_all(path).unwrap();
}

#[cfg(unix)]
#[test]
fn event_store_accepts_a_protected_precreated_empty_directory() {
    use std::os::unix::fs::PermissionsExt;
    let path = temp("precreated-event");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let header = canonical_header(1, "AS9z", Some(7), Cursor(0, 0, 0, 1));
    let store = Store::create(&path, Kind::Event, 7, header.as_bytes(), 0, 0).unwrap();
    assert_eq!(
        Store::read_only(&path, Kind::Event, 7).unwrap().1,
        header.as_bytes()
    );
    drop(store);
    fs::remove_dir_all(path).unwrap();
}

#[cfg(unix)]
#[test]
fn descriptor_bound_creation_ignores_a_replacement_at_the_original_path() {
    use std::os::unix::fs::PermissionsExt;

    let path = temp("descriptor-create");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = std::fs::File::open(&path).unwrap();
    let moved = path.with_extension("bound");
    fs::rename(&path, &moved).unwrap();
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

    let header = canonical_header(1, "AS9z", Some(7), Cursor(0, 0, 0, 1));
    let store = Store::create_at(&directory, Kind::Event, 7, header.as_bytes(), 0, 0).unwrap();

    assert_eq!(
        Store::read_only(&moved, Kind::Event, 7).unwrap().1,
        header.as_bytes()
    );
    assert!(fs::read_dir(&path).unwrap().next().is_none());
    drop(store);
    drop(directory);
    fs::remove_dir_all(path).unwrap();
    fs::remove_dir_all(moved).unwrap();
}

#[cfg(unix)]
#[test]
fn descriptor_bound_creation_produces_the_exact_locked_slot_set() {
    use fs2::FileExt as _;
    use std::os::unix::fs::PermissionsExt;

    let path = temp("descriptor-slot-set");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = std::fs::File::open(&path).unwrap();
    let store = Store::create_at(&directory, Kind::Log, 7, b"", 0, 0).unwrap();
    let mut entries = fs::read_dir(&path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries, ["body.0", "body.1", "commit.0", "commit.1"]);
    for name in entries {
        assert_eq!(
            fs::metadata(path.join(name)).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let competing_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("commit.0"))
        .unwrap();
    assert!(competing_writer.try_lock_exclusive().is_err());
    drop(store);
    drop(directory);
    fs::remove_dir_all(path).unwrap();
}

#[cfg(unix)]
#[test]
fn descriptor_bound_creation_rejects_and_preserves_an_extra_entry() {
    use std::os::unix::fs::PermissionsExt;

    let path = temp("descriptor-extra");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(path.join("extra"), b"").unwrap();
    let directory = std::fs::File::open(&path).unwrap();
    assert!(matches!(
        Store::create_at(&directory, Kind::Log, 7, b"", 0, 0),
        Err(StoreError::Corrupt)
    ));
    assert_eq!(
        fs::read_dir(&path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        ["extra"]
    );
    drop(directory);
    fs::remove_dir_all(path).unwrap();
}

#[cfg(unix)]
#[test]
fn descriptor_bound_validation_uses_a_nonblocking_directory_description() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;

    let path = temp("descriptor-nonblocking-enumeration");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = std::fs::File::open(&path).unwrap();
    let before = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_GETFL) };
    assert!(before >= 0);
    assert_eq!(before & libc::O_NONBLOCK, 0);

    let prepared = Store::prepare_at(&directory).unwrap();

    let after = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_GETFL) };
    assert!(after >= 0);
    assert_ne!(after & libc::O_NONBLOCK, 0);
    prepared.rollback_at(&directory);
    drop(directory);
    fs::remove_dir(path).unwrap();
}

#[cfg(unix)]
#[test]
fn descriptor_bound_revalidation_accepts_only_the_exact_slot_names() {
    use std::os::unix::fs::PermissionsExt;

    let path = temp("descriptor-exact-revalidation");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = std::fs::File::open(&path).unwrap();
    let prepared = Store::prepare_at(&directory).unwrap();
    prepared.revalidate_at(&directory).unwrap();

    fs::write(path.join("extra"), b"caller-owned").unwrap();
    assert!(matches!(
        prepared.revalidate_at(&directory),
        Err(StoreError::Corrupt)
    ));
    assert_eq!(fs::read(path.join("extra")).unwrap(), b"caller-owned");

    prepared.rollback_at(&directory);
    fs::remove_file(path.join("extra")).unwrap();
    drop(directory);
    fs::remove_dir(path).unwrap();
}

#[cfg(unix)]
#[test]
fn prepared_store_defers_the_writer_lease_until_descriptor_bound_initialization() {
    use fs2::FileExt as _;
    use std::os::unix::fs::PermissionsExt;

    let path = temp("prepared-lock");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = std::fs::File::open(&path).unwrap();
    let prepared = Store::prepare_at(&directory).unwrap();
    let competing_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("commit.0"))
        .unwrap();
    competing_writer.try_lock_exclusive().unwrap();
    competing_writer.unlock().unwrap();

    let store = prepared
        .lease_at(&directory, Kind::Log, 7, b"abc", 0, 3)
        .unwrap();
    assert!(competing_writer.try_lock_exclusive().is_err());
    assert!(matches!(
        Store::read_only(&path, Kind::Log, 7),
        Err(StoreError::Corrupt)
    ));
    prepared
        .initialize_leased_at(&directory, &store, b"abc")
        .unwrap();
    assert_eq!(Store::read_only(&path, Kind::Log, 7).unwrap().1, b"abc");

    drop(store);
    prepared.rollback_at(&directory);
    drop(directory);
    fs::remove_dir(path).unwrap();
}

#[cfg(unix)]
#[test]
fn prepared_store_rejects_a_substituted_slot_during_revalidation_and_initialization() {
    use std::os::unix::fs::PermissionsExt;

    let path = temp("prepared-substitution");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = std::fs::File::open(&path).unwrap();
    let prepared = Store::prepare_at(&directory).unwrap();
    let displaced = path.join("displaced-body.1");
    fs::rename(path.join("body.1"), &displaced).unwrap();
    fs::write(path.join("body.1"), b"replacement").unwrap();
    fs::set_permissions(path.join("body.1"), fs::Permissions::from_mode(0o600)).unwrap();

    assert!(matches!(
        prepared.revalidate_at(&directory),
        Err(StoreError::Corrupt)
    ));
    assert!(matches!(
        prepared.initialize_at(&directory, Kind::Log, 7, b"", 0, 0),
        Err(StoreError::Corrupt)
    ));
    assert_eq!(fs::read(path.join("body.1")).unwrap(), b"replacement");

    prepared.rollback_at(&directory);
    assert_eq!(fs::read(path.join("body.1")).unwrap(), b"replacement");
    fs::remove_file(path.join("body.1")).unwrap();
    fs::remove_file(displaced).unwrap();
    drop(directory);
    fs::remove_dir(path).unwrap();
}

#[cfg(unix)]
#[test]
fn prepared_store_rollback_removes_only_the_original_slot_identities() {
    use std::os::unix::fs::PermissionsExt;

    let path = temp("prepared-rollback");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = std::fs::File::open(&path).unwrap();
    let prepared = Store::prepare_at(&directory).unwrap();
    let displaced = path.join("displaced-commit.1");
    fs::rename(path.join("commit.1"), &displaced).unwrap();
    fs::write(path.join("commit.1"), b"replacement").unwrap();
    fs::set_permissions(path.join("commit.1"), fs::Permissions::from_mode(0o600)).unwrap();

    prepared.rollback_at(&directory);
    for name in ["body.0", "body.1", "commit.0"] {
        assert!(!path.join(name).exists());
    }
    assert_eq!(fs::read(path.join("commit.1")).unwrap(), b"replacement");
    assert!(displaced.exists());

    fs::remove_file(path.join("commit.1")).unwrap();
    fs::remove_file(displaced).unwrap();
    drop(directory);
    fs::remove_dir(path).unwrap();
}

#[cfg(unix)]
#[test]
fn created_slots_have_exact_mode_under_a_restrictive_umask() {
    const CHILD: &str = "MOOR_RESTRICTIVE_UMASK_CHILD";
    if std::env::var_os(CHILD).is_some() {
        use std::os::unix::fs::PermissionsExt;
        unsafe { libc::umask(0o777) };
        let path = temp("restrictive-umask");
        let store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
        for name in ["body.0", "body.1", "commit.0", "commit.1"] {
            assert_eq!(
                fs::metadata(path.join(name)).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(store);
        fs::remove_dir_all(path).unwrap();
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "created_slots_have_exact_mode_under_a_restrictive_umask",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn event_commit_metadata_and_closed_header_are_validated() {
    let header = canonical_header(1, "AS9z", Some(7), Cursor(2, 4, 4, 1));
    for (name, start, end, bytes) in [
        ("event-start", 3, 4, header.as_bytes()),
        ("event-end", 4, 5, header.as_bytes()),
        (
            "event-header-key",
            4,
            4,
            b"{\"v\":2,\"type\":\"header\",\"ts\":1,\"session\":\"AS9z\",\"generation\":7,\"epoch\":2,\"next_seq\":4,\"first_retained\":4,\"extra\":0}\n",
        ),
    ] {
        let path = temp(name);
        assert!(matches!(
            Store::create(&path, Kind::Event, 7, bytes, start, end),
            Err(StoreError::Corrupt)
        ));
        let _ = fs::remove_dir_all(path);
    }
    let path = temp("unknown-event-record");
    let body = [
        canonical_header(1, "AS9z", Some(7), Cursor(0, 1, 0, 1)),
        "{\"type\":\"invented\",\"ts\":1,\"epoch\":0,\"seq\":0,\"kind\":\"transition\"}\n".into(),
    ]
    .concat();
    assert!(matches!(
        Store::create(&path, Kind::Event, 7, body.as_bytes(), 0, 1),
        Err(StoreError::Corrupt)
    ));
    let _ = fs::remove_dir_all(path);

    for (name, body, start) in [
        (
            "event-unsupervised-generation",
            "{\"v\":2,\"type\":\"header\",\"ts\":1,\"session\":\"AS9z\",\"generation\":1,\"epoch\":0,\"next_seq\":0,\"first_retained\":0}\n",
            0,
        ),
        (
            "event-sequence-range",
            "{\"v\":2,\"type\":\"header\",\"ts\":1,\"session\":\"AS9z\",\"generation\":null,\"epoch\":0,\"next_seq\":9007199254740992,\"first_retained\":9007199254740992}\n",
            1 << 53,
        ),
    ] {
        let path = temp(name);
        assert!(matches!(
            Store::create(&path, Kind::Event, 1, body.as_bytes(), start, start),
            Err(StoreError::Corrupt)
        ));
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn exhausted_events_are_terminal_and_match_their_axis_frontier() {
    let ready = event("ready", 1, &[]);
    let exhausted = |axis| event("stream-exhausted", 1, &[("axis", Json::String(axis))]);
    let bodies = [
        [
            canonical_header(1, "AS9z", Some(7), Cursor(0, 2, 0, 1)),
            canonical_event(0, 0, EventKind::Transition, &exhausted("seq")),
            canonical_event(0, 1, EventKind::Transition, &ready),
        ]
        .concat(),
        [
            canonical_header(1, "AS9z", Some(7), Cursor(1, 1, 0, 1)),
            canonical_event(1, 0, EventKind::Transition, &exhausted("epoch")),
        ]
        .concat(),
        [
            canonical_header(1, "AS9z", Some(7), Cursor(0, 1, 0, 1)),
            canonical_event(0, 0, EventKind::Transition, &exhausted("commit")),
        ]
        .concat(),
    ];
    for (at, body) in bodies.iter().enumerate() {
        let path = temp(&format!("event-exhausted-frontier-{at}"));
        assert!(matches!(
            Store::create(
                &path,
                Kind::Event,
                7,
                body.as_bytes(),
                0,
                body.lines().count() as u64 - 1
            ),
            Err(StoreError::Corrupt)
        ));
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn event_overage_requires_a_compaction_prefix_and_occurrence_trigger() {
    let initial = canonical_header(1, "AS9z", Some(7), Cursor(0, 0, 0, 1));
    let link = event(
        "link",
        1,
        &[
            ("uri", Json::String(&"x".repeat(2048))),
            ("truncated", Json::Bool(false)),
        ],
    );
    let mut valid = canonical_header(1, "AS9z", Some(7), Cursor(1, 1, 0, 1));
    valid.push_str(&canonical_event(1, 0, EventKind::Transition, &link));
    let valid_path = temp("event-overage-line-control");
    let mut valid_store =
        Store::create(&valid_path, Kind::Event, 7, initial.as_bytes(), 0, 0).unwrap();
    valid_store.replace(valid.as_bytes(), 1, 0, 1).unwrap();
    drop(valid_store);
    fs::remove_dir_all(valid_path).unwrap();
    let mut history = canonical_header(1, "AS9z", Some(7), Cursor(1, 125, 0, 1));
    for sequence in 0..125 {
        history.push_str(&canonical_event(1, sequence, EventKind::Transition, &link));
    }
    assert!(history.len() > 256 << 10 && history.len() <= 320 << 10);
    let path = temp("event-overage-history");
    let mut store = Store::create(&path, Kind::Event, 7, initial.as_bytes(), 0, 0).unwrap();
    assert!(matches!(
        store.replace(history.as_bytes(), 1, 0, 125),
        Err(StoreError::Corrupt)
    ));
    drop(store);
    let _ = fs::remove_dir_all(path);

    let effect = SourceEffect {
        source: b"source".as_slice().into(),
        producer: [1; 16],
        source_epoch: 1,
        status: SourceStatus::Connected,
        reason: SourceReason::None,
    };
    let retained = semantic_source(1, &effect).unwrap();
    let mut body = canonical_header(1, "AS9z", Some(7), Cursor(1, 126, 0, 1));
    for sequence in 0..125 {
        body.push_str(&canonical_event(1, sequence, EventKind::Snapshot, &link));
    }
    body.push_str(&canonical_event(1, 125, EventKind::Transition, &retained));
    assert!(body.len() > 256 << 10 && body.len() <= 320 << 10);
    let path = temp("event-overage-retained-trigger");
    let mut store = Store::create(&path, Kind::Event, 7, initial.as_bytes(), 0, 0).unwrap();
    assert!(matches!(
        store.replace(body.as_bytes(), 1, 0, 126),
        Err(StoreError::Corrupt)
    ));
    drop(store);
    let _ = fs::remove_dir_all(path);

    let exhausted = event("stream-exhausted", 1, &[("axis", Json::String("commit"))]);
    body = body.replacen("\"next_seq\":126", "\"next_seq\":127", 1);
    body.push_str(&canonical_event(1, 126, EventKind::Transition, &exhausted));
    let path = temp("event-overage-retained-before-diagnostic");
    let mut store = Store::create(&path, Kind::Event, 7, initial.as_bytes(), 0, 0).unwrap();
    assert!(matches!(
        store.replace(body.as_bytes(), 1, 0, 127),
        Err(StoreError::Corrupt)
    ));
    drop(store);
    let _ = fs::remove_dir_all(path);
}

#[test]
fn event_recovery_accepts_the_terminal_exclusive_sequence_frontier() {
    let final_sequence = (1 << 53) - 1;
    let exhausted = event("stream-exhausted", 1, &[("axis", Json::String("seq"))]);
    let body = [
        canonical_header(1, "AS9z", Some(7), Cursor(0, 1 << 53, final_sequence, 1)),
        canonical_event(0, final_sequence, EventKind::Transition, &exhausted),
    ]
    .concat();
    let path = temp("event-terminal-sequence");
    let store = Store::create(
        &path,
        Kind::Event,
        7,
        body.as_bytes(),
        final_sequence,
        1 << 53,
    )
    .unwrap();
    assert_eq!(
        (store.selected().start, store.selected().end),
        (final_sequence, 1 << 53)
    );
    drop(store);
    assert!(Store::open(&path, Kind::Event, 7).is_ok());
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn a_live_writer_excludes_another_writer() {
    let path = temp("lease");
    let store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    assert!(matches!(
        Store::open(&path, Kind::Log, 7),
        Err(StoreError::Io(_))
    ));
    drop(store);
    assert!(Store::open(&path, Kind::Log, 7).is_ok());
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn a_reader_recovers_while_the_writer_holds_its_lease() {
    let path = temp("reader");
    let _writer = Store::create(&path, Kind::Log, 7, b"abc", 0, 3).unwrap();
    let (commit, body) = Store::read_only(&path, Kind::Log, 7).unwrap();
    assert_eq!((commit.index, body.as_slice()), (1, b"abc".as_slice()));
    fs::remove_dir_all(path).unwrap();
}

#[cfg(unix)]
#[test]
fn a_writer_keeps_using_its_validated_slot_handles_after_path_replacement() {
    use std::os::unix::fs::PermissionsExt;
    let path = temp("persistent-slot-handles");
    let mut store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    let original_body = path.join("original-body.0");
    let original_commit = path.join("original-commit.1");
    fs::rename(path.join("body.0"), &original_body).unwrap();
    fs::rename(path.join("commit.1"), &original_commit).unwrap();
    fs::write(path.join("body.0"), b"replacement").unwrap();
    fs::write(path.join("commit.1"), b"replacement").unwrap();
    for name in ["body.0", "commit.1"] {
        fs::set_permissions(path.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    store.append_capped(b"abc", 32, 3).unwrap();
    assert_eq!(fs::read(&original_body).unwrap(), b"abc");
    assert_eq!(fs::read(&original_commit).unwrap().len(), 92);
    assert_eq!(fs::read(path.join("body.0")).unwrap(), b"replacement");
    assert_eq!(fs::read(path.join("commit.1")).unwrap(), b"replacement");
    drop(store);
    fs::remove_dir_all(path).unwrap();
}

#[cfg(unix)]
#[test]
fn recovery_refuses_linked_slots() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let path = temp("linked-slot");
    let store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    drop(store);
    let outside = temp("outside-body");
    fs::write(&outside, b"").unwrap();
    fs::remove_file(path.join("body.0")).unwrap();
    symlink(&outside, path.join("body.0")).unwrap();
    assert!(matches!(
        Store::open(&path, Kind::Log, 7),
        Err(StoreError::Corrupt)
    ));
    fs::remove_file(outside).unwrap();
    fs::remove_dir_all(path).unwrap();

    let path = temp("broad-slot");
    let store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    drop(store);
    fs::set_permissions(path.join("commit.0"), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        Store::open(&path, Kind::Log, 7),
        Err(StoreError::Corrupt)
    ));
    fs::remove_dir_all(path).unwrap();

    let path = temp("hard-linked-slot");
    let store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    drop(store);
    fs::remove_file(path.join("body.1")).unwrap();
    fs::hard_link(path.join("body.0"), path.join("body.1")).unwrap();
    assert!(matches!(
        Store::open(&path, Kind::Log, 7),
        Err(StoreError::Corrupt)
    ));
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn selected_now_reports_no_frontier_rather_than_a_wrong_one() {
    // The frontier mechanism keys on this: `None` must mean "nothing selectable
    // right now", never "index zero". A caller that mistook it for a frontier
    // would regress the status descriptor instead of merely delaying it.
    let path = temp("selected-now");
    let mut store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    assert_eq!(store.selected_now().map(|commit| commit.index), Some(1));
    store.append_capped(b"xyz", 64, 3).unwrap();
    let advanced = store.selected_now().expect("a valid commit");
    assert_eq!(advanced.index, 2);
    assert_eq!(advanced.end, 3);
    // Both commit slots invalid: selectable state is gone, so the answer is
    // None and the caller keeps whatever it last saw.
    for slot in ["commit.0", "commit.1"] {
        fs::write(path.join(slot), [0u8; 92]).unwrap();
    }
    assert_eq!(store.selected_now(), None);
    // The already-open writer handle still reports its own last valid commit,
    // which is exactly the last-known-valid policy the frontier relies on.
    assert_eq!(store.selected().index, 2);
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn contended_reads_through_a_duplicated_handle_cannot_disturb_the_writer() {
    // A duplicated descriptor shares the underlying file position, so this is
    // the guard for the store being positional: with seek-then-io primitives a
    // reader moves the writer's cursor between its seek and its write and lands
    // bytes at the wrong offset. Every commit must stay selectable and the final
    // retained bytes must be exactly what was written.
    //
    // This schedule is CONTENDED, not barrier-forced: it does not guarantee the
    // interleaving on any single run. Its standing as evidence rests on the
    // seek-based mutant failing it consistently — measured 10 of 10 — rather
    // than on one lucky failure.
    let path = temp("dup-interleave");
    let mut store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    let view = store.duplicate().unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_stop = std::sync::Arc::clone(&stop);
    let reader = std::thread::spawn(move || {
        let mut highest = 0;
        while !reader_stop.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(commit) = view.selected_now() {
                assert!(commit.index >= highest, "frontier regressed");
                highest = commit.index;
            }
        }
        highest
    });
    let mut end = 0;
    for step in 0..200u64 {
        let payload = [b'a' + (step % 26) as u8; 7];
        end += payload.len() as u64;
        store.append_capped(&payload, 1 << 20, end).unwrap();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let observed = reader.join().unwrap();
    assert!(observed > 1, "the reader never saw the writer advance");

    // The writer's own view and an independent reopen must agree exactly, which
    // fails if any write landed at a cursor the reader had moved.
    assert_eq!(store.selected().end, end);
    let (commit, body) = Store::read_only(&path, Kind::Log, 7).unwrap();
    assert_eq!(commit.end, end);
    assert_eq!(body.len() as u64, commit.end - commit.start);
    fs::remove_dir_all(path).unwrap();
}
