use moor::runtime::private::{
    ArtifactConfig, SessionState, age, await_launch, clear_store, companion, decode_launch_record,
    decode_launch_result, discover_sessions, environment_key, holder_artifacts, instrument_ack,
    instrument_stage, launch_result, lifecycle_running, monotonic, now, parse_boot_uuid,
    random_array, supervised_generation, validate_instrument_ack,
};
use moor::store::{Kind, Store};
use moor::wire::{get_wide, put_wide};
use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn private_records_are_exact_and_generation_fenced() {
    let mut launch = [0; 32];
    launch[..8].copy_from_slice(b"MOORLCH3");
    launch[8] = 1;
    launch[12..16].copy_from_slice(&42u32.to_le_bytes());
    launch[16..].fill(7);
    assert_eq!(decode_launch_record(&launch), Some(42));
    launch[9] = 1;
    assert!(decode_launch_record(&launch).is_none());

    let nonce = [9; 16];
    let ack = instrument_ack(42, 123, nonce).unwrap();
    assert!(validate_instrument_ack(&ack, true, 42, 123, nonce).is_ok());
    assert!(validate_instrument_ack(&ack, false, 42, 123, nonce).is_err());
}

#[test]
fn background_result_and_instrument_stage_are_identity_bound() {
    let ready = launch_result(3, 127, 42).unwrap();
    assert_eq!(ready, [b'M', b'O', b'R', b'R', 1, 3, 127, 0, 42, 0, 0, 0]);
    assert_eq!(decode_launch_result(&ready), Some((3, 127, 42)));
    assert!(launch_result(0, 0, 42).is_none());
    assert!(decode_launch_result(&ready[..11]).is_none());

    let root = std::path::Path::new("/protected/root");
    let stage = instrument_stage(root, b"\x01/test", 42, [9; 16]).unwrap();
    assert_eq!(
        stage,
        root.join("a1d6a07694aa1392bcfe49bae764a080faa38c3e33f73e72843b7c8194f8e6f3.instrument")
    );
    assert_ne!(
        stage,
        instrument_stage(root, b"\x01/test", 43, [9; 16]).unwrap()
    );

    let complete = [
        launch_result(1, 0, 42).unwrap(),
        launch_result(2, 0, 42).unwrap(),
    ]
    .concat();
    assert_eq!(await_launch(std::io::Cursor::new(complete)), Ok((0, 42)));
    assert_eq!(await_launch(std::io::Cursor::new(ready)), Ok((127, 42)));
}

#[test]
fn shared_random_source_rejects_the_zero_identity() {
    assert_ne!(random_array::<16>().unwrap(), [0; 16]);
}

#[test]
fn linux_boot_identity_requires_the_canonical_uuid_grammar() {
    let expected = 0x00112233_4455_6677_8899_aabbccddeeff_u128.to_be_bytes();
    assert_eq!(
        parse_boot_uuid("00112233-4455-6677-8899-aabbccddeeff\n"),
        Some(expected)
    );
    assert_eq!(
        parse_boot_uuid("00112233-4455-6677-8899-AABBCCDDEEFF"),
        Some(expected)
    );
    for malformed in [
        "001122334455-6677-8899-aabbccddeeff",
        "00112233-44556677-8899-aabbccddeeff",
        "00112233-4455-6677-8899aabb-ccddeeff",
        "{00112233-4455-6677-8899-aabbccddeeff}",
        "00112233-4455-6677-8899-aabbccddeeff\n\n",
        "00000000-0000-0000-0000-000000000000",
    ] {
        assert_eq!(parse_boot_uuid(malformed), None, "{malformed:?}");
    }
}

#[test]
fn deadline_clock_is_a_distinct_monotonic_domain() {
    let first = monotonic();
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(monotonic() > first);
    assert!(now().saturating_sub(monotonic()) > 1_000_000_000);
}

#[test]
fn discovery_applies_one_deadline_to_the_complete_listing() {
    let root = std::env::temp_dir().join(format!(
        "moor-discovery-deadline-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    for index in 0..20 {
        fs::write(root.join(index.to_string()), b"").unwrap();
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let started = Instant::now();
    let entries = pool.install(|| {
        discover_sessions(&root, Some, |_, remaining| {
            std::thread::sleep(remaining.min(Duration::from_millis(250)));
            SessionState::Stale
        })
        .unwrap()
    });
    assert!(
        started.elapsed() < Duration::from_millis(2250),
        "listing took {:?}",
        started.elapsed()
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.state == SessionState::Indeterminate)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervised_generation_validates_and_sanitizes_environment_carriers() {
    let invoked = std::ffi::OsStr::new("moor-private-generation-test");
    let key = environment_key(invoked, "_GENERATION");
    unsafe {
        std::env::set_var("DESK_MOOR_LAUNCH_CHANNEL", "channel");
        std::env::set_var(&key, "42");
        std::env::set_var("DESK_SESSION_GENERATION", "42");
    }
    let result = supervised_generation(invoked, true, "invalid launch", |selector| {
        assert_eq!(selector, "channel");
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(b"MOORLCH3");
        bytes[8] = 1;
        bytes[12..16].copy_from_slice(&42u32.to_le_bytes());
        bytes[16..].fill(7);
        decode_launch_record(&bytes).ok_or_else(|| "decode failed".to_string())
    });
    assert_eq!(result.unwrap(), (42, true));
    assert!(std::env::var_os("DESK_MOOR_LAUNCH_CHANNEL").is_none());
    assert!(std::env::var_os(key).is_none());
    assert!(std::env::var_os("DESK_SESSION_GENERATION").is_none());
}

#[test]
fn environment_keys_transform_encoded_bytes_not_unicode_characters() {
    assert_eq!(
        environment_key(std::ffi::OsStr::new("mø-or"), "_SESSION"),
        "M___OR_SESSION"
    );
}

#[test]
fn wide_values_can_require_or_ignore_a_trailing_payload() {
    let mut bytes = Vec::new();
    put_wide(&mut bytes, b"identity").unwrap();
    bytes.push(1);
    assert_eq!(get_wide(&bytes, 0, false), Some(b"identity".as_slice()));
    assert!(get_wide(&bytes, 0, true).is_none());
}

#[test]
fn lifecycle_age_requires_a_matching_nonzero_boot_identity() {
    let root = std::env::temp_dir().join(format!(
        "moor-lifecycle-age-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let session = root.join("session");
    let boot = [7; 16];
    let running = lifecycle_running(
        b"\x01/test",
        (None, 1),
        [9; 16],
        (900_000, 498_000, boot),
        ("posix-bytes", None, None),
    );
    drop(
        Store::create(
            &companion(&session, ".exit"),
            Kind::Exit,
            1,
            running.as_bytes(),
            0,
            0,
        )
        .unwrap(),
    );
    assert_eq!(age(&session, (500_000, boot)), "2s ago");
    assert_eq!(age(&session, (900_000, [8; 16])), "unknown");
    assert_eq!(age(&session, (500_000, [0; 16])), "unknown");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn clearing_an_empty_log_is_an_idempotent_recovery_noop() {
    let path = std::env::temp_dir().join(format!("moor-empty-clear-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    drop(Store::create(&path, Kind::Log, 1, b"", 0, 0).unwrap());
    clear_store(&path).unwrap();
    let (commit, body) = Store::read_only(&path, Kind::Log, 1).unwrap();
    assert_eq!(
        (commit.index, commit.epoch, commit.start, commit.end, body),
        (1, 1, 0, 0, Vec::new())
    );
    assert_eq!(
        Store::open(&path, Kind::Log, 1).unwrap().selected().index,
        1
    );
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn portable_event_status_advertises_the_selected_commit_frontier() {
    let root = std::env::temp_dir().join(format!("moor-event-status-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    let marker = root.join("session");
    let event = root.join("events");
    let identity = b"\x01/test";
    let artifacts = holder_artifacts(
        identity,
        (None, 1),
        [3; 16],
        [0; 16],
        (4, 5, [6; 16]),
        ArtifactConfig {
            marker: &marker,
            event_path: Some(&event),
            encoding: "posix-bytes",
            event_identity: Some(event.as_os_str().as_encoded_bytes()),
            instrument_identity: None,
            event_layout: 2,
            log_cap: 0,
        },
    )
    .unwrap();
    let commit_at = artifacts.commit_at;
    let (mut storage, status) = (artifacts.storage, artifacts.status);
    let (commit, _) = Store::read_only(&event, Kind::Event, 1).unwrap();
    let mut at = 4 + identity.len() + 4 + 16;
    assert_eq!(status[at], 2);
    at += 1;
    let path_len = u32::from_le_bytes(status[at..at + 4].try_into().unwrap()) as usize;
    at += 4 + path_len;
    assert_eq!(status[at], commit.body);
    assert_eq!(
        u64::from_le_bytes(status[at + 1..at + 9].try_into().unwrap()),
        commit.index
    );
    assert_eq!(
        u64::from_le_bytes(status[at + 9..at + 17].try_into().unwrap()),
        commit.length
    );
    assert_eq!(&status[at + 17..at + 49], &commit.hash);

    // #23.1: the prebuilt blob necessarily holds the launch commit, so
    // send_status patches these bytes per send from the live selection. That
    // requires the recorded offset to be exactly where the fields sit...
    assert_eq!(commit_at, at, "recorded commit offset");
    // ...and the live accessor to actually advance past the launch commit,
    // which is what OB-39 means by "the commit a reader would select".
    storage.observe(moor::terminal::Observation::Ready).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while storage.event_commit().map(|selected| selected.1) == Some(commit.index)
        && std::time::Instant::now() < deadline
    {
        storage.poll();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let live = storage.event_commit().expect("an enabled event lane");
    assert!(
        live.1 > commit.index,
        "selected commit must advance past the launch commit: {} vs {}",
        live.1,
        commit.index
    );
    assert_ne!(live.3, commit.hash, "committed body hash must change");
    drop(storage);
    fs::remove_dir_all(root).unwrap();
}
