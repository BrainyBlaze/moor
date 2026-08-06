use moor::runtime::private::{exit_records as shared_exit_records, lifecycle_running};
use moor::windows::{
    BootstrapRecord, Marker, accept_bootstrap_command, bootstrap_command, cim_boot_identity,
    instrument_ack, validate_instrument_ack, wtf8_decode, wtf8_encode,
};

fn exit_records(
    running: &str,
    ts: u64,
    end: u64,
    code: u32,
    forced: Option<bool>,
) -> (moor::events::Event, Vec<u8>) {
    let method = forced.map(|forced| if forced { "forced" } else { "graceful" });
    shared_exit_records(
        running,
        (ts, ts),
        end,
        (
            if method.is_some() {
                "terminated"
            } else {
                "exited"
            },
            "code",
            u64::from(code),
            method,
        ),
    )
}

const MARKER: [u8; 84] = [
    0x4d, 0x4f, 0x4f, 0x52, 0x4d, 0x52, 0x4b, 0x33, 1, 0, 0, 0, 7, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7,
    8, 9, 10, 11, 12, 13, 14, 15, 46, 0, 92, 92, 46, 92, 112, 105, 112, 101, 92, 109, 111, 111,
    114, 45, 48, 48, 48, 49, 48, 50, 48, 51, 48, 52, 48, 53, 48, 54, 48, 55, 48, 56, 48, 57, 48,
    97, 48, 98, 48, 99, 48, 100, 48, 101, 48, 102, 0xb1, 0x25, 0xd5, 0x68,
];

fn running() -> String {
    lifecycle_running(
        &[1, b'/', b's'],
        (Some(7), 7),
        [2; 16],
        (1, 2, [3; 16]),
        ("posix-bytes", None, None),
    )
}

#[test]
fn cim_boot_time_is_converted_to_utc_filetime_ticks() {
    let mut expected = [0; 16];
    expected[..8].copy_from_slice(&133_486_346_451_234_560u64.to_le_bytes());
    assert_eq!(
        cim_boot_identity("20240102030405.123456+060"),
        Some(expected)
    );
    assert_eq!(cim_boot_identity("20230229030405.000000+000"), None);
    assert_eq!(cim_boot_identity("20240102030405.123456*060"), None);
}

#[test]
fn marker_matches_v12_and_rejects_every_frozen_field() {
    let marker = Marker::new(
        7,
        core::array::from_fn(|n| n as u8),
        core::array::from_fn(|n| n as u8),
    )
    .unwrap();
    assert_eq!(marker.encode(), MARKER);
    assert_eq!(Marker::decode(&MARKER).unwrap(), marker);
    for at in [0, 8, 9, 10, 12, 32, 34, 48, 80] {
        let mut bad = MARKER;
        bad[at] ^= 1;
        assert!(Marker::decode(&bad).is_err(), "accepted byte {at}");
    }
    assert!(Marker::new(0, [0; 16], [0; 16]).is_err());
    assert!(Marker::decode(&MARKER[..83]).is_err());
}

#[test]
fn instrument_ack_matches_v22_and_requires_eof_and_identity() {
    let nonce = core::array::from_fn(|n| n as u8 + 0x10);
    let expected = [
        0x4d, 0x4f, 0x4f, 0x52, 0x49, 0x4e, 0x53, 0x33, 1, 0, 0, 0, 7, 0, 0, 0, 0x34, 0x12, 0, 0,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ];
    assert_eq!(instrument_ack(7, 0x1234, nonce).unwrap(), expected);
    assert!(validate_instrument_ack(&expected, true, 7, 0x1234, nonce).is_ok());
    for (bytes, eof, generation, pid) in [
        (&expected[..35], true, 7, 0x1234),
        (&expected[..], false, 7, 0x1234),
        (&expected[..], true, 8, 0x1234),
        (&expected[..], true, 7, 0),
    ] {
        assert!(validate_instrument_ack(bytes, eof, generation, pid, nonce).is_err());
    }
}

#[test]
fn bootstrap_identity_and_commands_are_nonce_bound_and_ordered() {
    let nonce = [7; 16];
    let record = BootstrapRecord {
        nonce,
        pid: 42,
        process: 0x1234,
        thread: 0x5678,
        created: 99,
    };
    let bytes = record.encode();
    assert_eq!(BootstrapRecord::decode(&bytes, nonce), Some(record));
    for at in 0..28 {
        let mut bad = bytes;
        bad[at] ^= 1;
        assert_eq!(
            BootstrapRecord::decode(&bad, nonce),
            None,
            "accepted mutation at {at}"
        );
    }
    for range in [28..32, 32..40, 40..48, 48..56] {
        let mut bad = bytes;
        bad[range].fill(0);
        assert_eq!(BootstrapRecord::decode(&bad, nonce), None);
    }
    assert_eq!(BootstrapRecord::decode(&bytes[..55], nonce), None);
    let mut resumed = false;
    assert_eq!(
        accept_bootstrap_command(&bootstrap_command(2, nonce), nonce, &mut resumed),
        None
    );
    assert_eq!(
        accept_bootstrap_command(&bootstrap_command(1, nonce), nonce, &mut resumed),
        Some(1)
    );
    assert_eq!(
        accept_bootstrap_command(&bootstrap_command(1, nonce), nonce, &mut resumed),
        None
    );
    assert_eq!(
        accept_bootstrap_command(&bootstrap_command(2, nonce), nonce, &mut resumed),
        Some(2)
    );
    assert_eq!(
        accept_bootstrap_command(&bootstrap_command(2, [8; 16]), nonce, &mut resumed),
        None
    );
}

#[test]
fn windows_native_paths_round_trip_through_canonical_wtf8() {
    let native = [0x41, 0xd800, 0x20ac, 0xd83d, 0xde00];
    let encoded = [
        0x41, 0xed, 0xa0, 0x80, 0xe2, 0x82, 0xac, 0xf0, 0x9f, 0x98, 0x80,
    ];
    assert_eq!(wtf8_encode(&native), encoded);
    assert_eq!(wtf8_decode(&encoded).unwrap(), native);
    for malformed in [
        &b"\xc0\x80"[..],
        &b"\xed\xa0"[..],
        &b"\xf4\x90\x80\x80"[..],
        &b"\x80"[..],
    ] {
        assert!(wtf8_decode(malformed).is_err());
    }
    assert!(
        wtf8_decode(b"\xed\xa0\x80\xed\xb0\x80").is_err(),
        "adjacent surrogate triples are not canonical interchange"
    );
    assert!(
        wtf8_decode(b"\xed\xa0\0").is_err(),
        "validator acceptance alone is insufficient"
    );
}

#[test]
fn holder_caused_windows_exits_are_terminated_in_both_durable_records() {
    use moor::events::EventStream;
    for (forced, method) in [(false, "graceful"), (true, "forced")] {
        let (event, lifecycle) = exit_records(&running(), 10, 12, 0xc000_013a, Some(forced));
        let record = EventStream::new().transact(&[], &[event], false).unwrap().0;
        let lifecycle = String::from_utf8(lifecycle).unwrap();
        for body in [&record, &lifecycle] {
            assert!(body.contains("\"ended\":\"terminated\""));
            assert!(body.contains(&format!("\"method\":\"{method}\"")));
            assert!(body.contains("\"code\":3221225786"));
        }
    }
    let (event, lifecycle) = exit_records(&running(), 10, 12, 7, None);
    let record = EventStream::new().transact(&[], &[event], false).unwrap().0;
    for body in [&record, std::str::from_utf8(&lifecycle).unwrap()] {
        assert!(body.contains("\"ended\":\"exited\""));
        assert!(!body.contains("\"method\""));
    }
}

#[cfg(unix)]
#[test]
fn shared_holder_resizes_only_for_the_lease_owner() {
    use moor::runtime::holder::{HolderConfig, Native, NativeExit, Runtime};
    use moor::runtime::{holder::CoreConfig, io::Duplex, storage::SessionStorage};
    use moor::store::{Kind, Store};
    use moor::wire::{Codec, Profile, put_wide};
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    struct Fake(Arc<Mutex<Vec<(u16, u16)>>>);
    impl Native for Fake {
        fn resize(&mut self, rows: u16, columns: u16) -> Result<(), String> {
            self.0.lock().unwrap().push((rows, columns));
            Ok(())
        }
        fn terminate(&mut self, _: bool) -> (u8, bool) {
            (0, false)
        }
        fn exited(&mut self) -> Result<Option<NativeExit>, String> {
            Ok(None)
        }
    }
    fn send(codec: &mut Codec, stream: &mut UnixStream, scope: u32, kind: u8, payload: &[u8]) {
        let mut bytes = Vec::new();
        codec.encode(scope, kind, payload, &mut bytes).unwrap();
        stream.write_all(&bytes).unwrap();
    }
    fn add(runtime: &mut Runtime<Fake>, identity: &[u8], size: (u16, u16)) -> UnixStream {
        let (mut client, server) = UnixStream::pair().unwrap();
        runtime.accept(
            Duplex::closing(server.try_clone().unwrap(), server, 1 << 20, || {}),
            true,
        );
        let mut codec = Codec::new(Profile::Controller);
        let mut hello = b"MOOR\x03\0\0".to_vec();
        put_wide(&mut hello, identity).unwrap();
        send(&mut codec, &mut client, 0, 1, &hello);
        let mut attach = Vec::new();
        attach.extend_from_slice(&size.1.to_le_bytes());
        attach.extend_from_slice(&size.0.to_le_bytes());
        attach.push(1);
        send(&mut codec, &mut client, 7, 3, &attach);
        for _ in 0..20 {
            runtime.poll();
            thread::sleep(Duration::from_millis(2));
        }
        client
    }

    let root = std::env::temp_dir().join(format!(
        "moor-windows-holder-test-{}-{}",
        std::process::id(),
        moor::runtime::private::now()
    ));
    std::fs::create_dir(&root).unwrap();
    let running = running();
    let lifecycle =
        Store::create(&root.join("exit"), Kind::Exit, 7, running.as_bytes(), 0, 0).unwrap();
    let (pty, child) = UnixStream::pair().unwrap();
    let sizes = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty: Duplex::closing(pty.try_clone().unwrap(), pty, 1024, || {}),
        storage: SessionStorage::new(None, None, lifecycle, 4, 1024),
        status: Vec::new(),
        commit_at: 0,
        synthetic: 3,
        native: Fake(sizes.clone()),
    });
    let owner = add(&mut runtime, b"session", (25, 80));
    let observer = add(&mut runtime, b"session", (40, 100));
    assert_eq!(*sizes.lock().unwrap(), [(25, 80)]);
    drop((owner, observer, child, runtime));
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn holder_reports_exact_log_clear_barriers_and_keeps_validated_handles() {
    use moor::runtime::client::Client;
    use moor::runtime::holder::{HolderConfig, Native, NativeExit, Runtime};
    use moor::runtime::{holder::CoreConfig, io::Duplex, storage::SessionStorage};
    use moor::store::{Kind, Store};
    use moor::wire::log_clear_payload;
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    struct Fake;
    impl Native for Fake {
        fn resize(&mut self, _: u16, _: u16) -> Result<(), String> {
            Ok(())
        }
        fn terminate(&mut self, _: bool) -> (u8, bool) {
            (0, false)
        }
        fn exited(&mut self) -> Result<Option<NativeExit>, String> {
            Ok(None)
        }
    }
    fn expected(
        outcome: u8,
        reason: u8,
        epoch: u32,
        prior: u64,
        resulting: u64,
        end: u64,
    ) -> Vec<u8> {
        let mut bytes = vec![outcome, reason, 0, 0];
        bytes.extend_from_slice(&epoch.to_le_bytes());
        for value in [prior, resulting, end] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }
    fn clear(client: &mut Client, observed: u64) -> Vec<u8> {
        client
            .send(0x19, &log_clear_payload([1; 16], observed).unwrap())
            .unwrap();
        client.receive_kind(0x1a).unwrap().payload.to_vec()
    }
    fn wait_index(client: &mut Client, expected: u64) {
        loop {
            client.send(13, &[]).unwrap();
            let payload = client.receive_kind(14).unwrap().payload;
            let at = payload.len() - 24;
            if u64::from_le_bytes(payload[at..at + 8].try_into().unwrap()) == expected {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    let root = std::env::temp_dir().join(format!(
        "moor-clear-test-{}-{}",
        std::process::id(),
        moor::runtime::private::now()
    ));
    std::fs::create_dir(&root).unwrap();
    let log_path = root.join("log");
    let log = Store::create(&log_path, Kind::Log, 7, b"", 0, 0).unwrap();
    let running = running();
    let lifecycle =
        Store::create(&root.join("exit"), Kind::Exit, 7, running.as_bytes(), 0, 0).unwrap();
    let (pty, mut child) = UnixStream::pair().unwrap();
    let (client_stream, holder_stream) = UnixStream::pair().unwrap();
    client_stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty: Duplex::closing(pty.try_clone().unwrap(), pty, 1024, || {}),
        storage: SessionStorage::new(Some((log, 1024)), None, lifecycle, 4, 1024),
        status: Vec::new(),
        commit_at: 0,
        synthetic: 3,
        native: Fake,
    });
    runtime.accept(
        Duplex::closing(
            holder_stream.try_clone().unwrap(),
            holder_stream,
            1 << 20,
            || {},
        ),
        true,
    );
    let stop = Arc::new(AtomicBool::new(false));
    let holder_stop = stop.clone();
    let holder = thread::spawn(move || {
        while !holder_stop.load(Ordering::Acquire) {
            runtime.poll();
            thread::sleep(Duration::from_millis(1));
        }
    });
    let mut client = Client::handshake_until(
        client_stream.try_clone().unwrap(),
        client_stream,
        b"session".to_vec(),
        std::time::Instant::now() + Duration::from_secs(2),
        || {},
    )
    .unwrap();

    child.write_all(b"abc").unwrap();
    wait_index(&mut client, 2);
    assert_eq!(clear(&mut client, 2), expected(0, 0, 2, 2, 3, 3));
    assert_eq!(clear(&mut client, 3), expected(1, 0, 2, 3, 3, 3));
    assert_eq!(clear(&mut client, 2), expected(2, 1, 2, 2, 3, 3));

    child.write_all(b"x").unwrap();
    wait_index(&mut client, 4);
    std::fs::remove_file(log_path.join("body.0")).unwrap();
    assert_eq!(clear(&mut client, 4), expected(0, 0, 3, 4, 5, 4));

    stop.store(true, Ordering::Release);
    holder.join().unwrap();
    drop((client, child));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn storage_reports_the_selected_log_commit() {
    use moor::runtime::storage::SessionStorage;
    use moor::store::{Kind, Store};
    use std::thread;
    use std::time::Duration;

    let root = std::env::temp_dir().join(format!(
        "moor-log-status-test-{}-{}",
        std::process::id(),
        moor::runtime::private::now()
    ));
    std::fs::create_dir(&root).unwrap();
    let log = Store::create(&root.join("log"), Kind::Log, 7, b"", 0, 0).unwrap();
    let running = running();
    let lifecycle =
        Store::create(&root.join("exit"), Kind::Exit, 7, running.as_bytes(), 0, 0).unwrap();
    let mut storage = SessionStorage::new(Some((log, 1024)), None, lifecycle, 4, 1024);
    let selected = || {
        let commit = Store::read_only(&root.join("log"), Kind::Log, 7).unwrap().0;
        (commit.epoch, commit.index, commit.start, commit.end)
    };
    assert_eq!(selected(), (1, 1, 0, 0));
    storage.output(b"abc".to_vec().into(), 3).unwrap();
    for _ in 0..50 {
        storage.poll();
        if selected() == (1, 2, 0, 3) {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(selected(), (1, 2, 0, 3));
    drop(storage);
    std::fs::remove_dir_all(root).unwrap();
}
