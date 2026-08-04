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

#[test]
fn empty_log_commit_matches_the_approved_92_byte_vector() {
    let path = temp("vector");
    let store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    assert_eq!(
        store.selected().unwrap().encode(),
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
fn append_replace_recovery_and_uncommitted_tail_preserve_frontier() {
    let path = temp("store");
    let mut store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    assert_eq!(store.selected().unwrap().index, 1);
    store.append(b"abc", 3).unwrap();
    assert_eq!(store.read().unwrap(), b"abc");
    let body = path.join(format!("body.{}", store.selected().unwrap().body));
    OpenOptions::new()
        .append(true)
        .open(body)
        .unwrap()
        .write_all(b"TAIL")
        .unwrap();
    drop(store);
    let mut recovered = Store::open(&path, Kind::Log, 7).unwrap();
    assert_eq!(recovered.read().unwrap(), b"abc");
    recovered.replace(b"", 2, 3, 3).unwrap();
    assert_eq!(recovered.read().unwrap(), b"");
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn torn_new_commit_falls_back_but_equal_valid_indexes_and_extra_entries_fail() {
    let path = temp("recovery");
    let mut store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    store.append(b"abc", 3).unwrap();
    fs::write(path.join("commit.1"), b"torn").unwrap();
    drop(store);
    assert_eq!(
        Store::open(&path, Kind::Log, 7)
            .unwrap()
            .selected()
            .unwrap()
            .index,
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
fn log_and_lifecycle_start_at_epoch_one_and_lifecycle_exits_once() {
    let log_path = temp("log-epoch");
    let log = Store::create(&log_path, Kind::Log, 7, b"", 0, 0).unwrap();
    assert_eq!(log.selected().unwrap().epoch, 1);
    drop(log);
    fs::remove_dir_all(log_path).unwrap();

    let path = temp("lifecycle");
    let running = b"{\"phase\":\"running\"}\n";
    let exited = b"{\"phase\":\"exited\"}\n";
    let mut store = Store::create(&path, Kind::Exit, 7, running, 0, 0).unwrap();
    assert_eq!(
        (
            store.selected().unwrap().epoch,
            store.selected().unwrap().index
        ),
        (1, 1)
    );
    store.replace(exited, 1, 9, 9).unwrap();
    assert_eq!(store.selected().unwrap().index, 2);
    assert!(matches!(
        store.replace(exited, 1, 9, 9),
        Err(StoreError::Exhausted)
    ));
    fs::remove_dir_all(path).unwrap();
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
}
