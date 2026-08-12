use moor::runtime::private::{
    exit_records as shared_exit_records, instrument_ack, lifecycle_running, validate_instrument_ack,
};
use moor::windows::{
    BootstrapRecord, Marker, accept_bootstrap_command, bootstrap_command, cim_boot_identity,
    wtf8_decode, wtf8_encode,
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
    let mut aliased = record;
    aliased.thread = aliased.process;
    assert_eq!(BootstrapRecord::decode(&aliased.encode(), nonce), None);
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
            None,
            false,
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
        None,
        false,
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
    use std::time::{Duration, Instant};

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
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match std::fs::remove_dir_all(&root) {
            Ok(()) => break,
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("store worker did not release cleanup handles: {error}"),
        }
    }
}

#[cfg(windows)]
mod launch_paths {
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::OsStringExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::sync::OnceLock;

    fn moor(args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_moor"))
            .args(args)
            .output()
            .unwrap()
    }

    fn companion(path: &Path, suffix: &str) -> PathBuf {
        let mut value = path.as_os_str().to_owned();
        value.push(suffix);
        value.into()
    }

    fn current_sid() -> String {
        windows_permissions::utilities::current_process_sid()
            .unwrap()
            .to_string()
    }

    fn invoked_root_for(executable: &Path) -> PathBuf {
        let sid = current_sid();
        let mut leaf = OsString::from(".");
        leaf.push(executable.file_name().unwrap());
        leaf.push("-");
        leaf.push(sid);
        std::env::temp_dir().join(leaf)
    }

    fn invoked_root() -> PathBuf {
        invoked_root_for(Path::new(env!("CARGO_BIN_EXE_moor")))
    }

    fn protect_file(path: &Path) {
        use windows_permissions::{LocalBox, SecurityDescriptor, constants::*, wrappers};

        let sid = current_sid();
        let descriptor: LocalBox<SecurityDescriptor> =
            format!("O:{sid}D:P(A;;FA;;;SY)(A;;FA;;;{sid})")
                .parse()
                .unwrap();
        wrappers::SetNamedSecurityInfo(
            path.as_os_str(),
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner
                | SecurityInformation::Dacl
                | SecurityInformation::ProtectedDacl,
            descriptor.owner(),
            None,
            descriptor.dacl(),
            None,
        )
        .unwrap_or_else(|error| panic!("protect fixture {path:?}: {error}"));
    }

    fn own_file(path: &Path) {
        use windows_permissions::{LocalBox, Sid, constants::*, wrappers};

        let sid: LocalBox<Sid> = current_sid().parse().unwrap();
        wrappers::SetNamedSecurityInfo(
            path.as_os_str(),
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner,
            Some(&sid),
            None,
            None,
            None,
        )
        .unwrap_or_else(|error| panic!("own fixture {path:?}: {error}"));
    }

    fn instrumentation_fixture() -> &'static Path {
        static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
        FIXTURE.get_or_init(|| {
            let path = std::env::var_os("MOOR_TEST_WINDOWS_INSTRUMENT_DLL")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    let path = std::env::temp_dir().join(format!(
                        "moor-instrument-{}-{}.dll",
                        std::process::id(),
                        moor::runtime::private::now()
                    ));
                    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/fixtures/windows_instrument.rs");
                    let mut command = Command::new("rustc");
                    command.args(["--edition=2024", "--crate-type=cdylib"]);
                    if cfg!(target_env = "msvc") {
                        command.args(["-C", "target-feature=+crt-static"]);
                    }
                    let output = command.arg(source).arg("-o").arg(&path).output().unwrap();
                    assert!(output.status.success(), "{output:?}");
                    path
                });
            protect_file(&path);
            path
        })
    }

    const PUBLICATION_RELEASE: &str = "MOOR_TEST_PUBLICATION_RELEASE";
    const SEMANTIC_TOKEN_PROBE: &str = "MOOR_TEST_SEMANTIC_TOKEN_PROBE";
    const EARLY_EXIT_READY: &str = "MOOR_TEST_EARLY_EXIT_READY";
    const EARLY_EXIT_RELEASE: &str = "MOOR_TEST_EARLY_EXIT_RELEASE";
    const EARLY_EXIT_OUTPUT: &[u8] = b"MOOR-EARLY-OUTPUT";

    #[test]
    fn publication_waiter() {
        let Some(release) = std::env::var_os(PUBLICATION_RELEASE) else {
            return;
        };
        while !Path::new(&release).exists() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn semantic_token_probe() {
        let Some(output) = std::env::var_os(SEMANTIC_TOKEN_PROBE) else {
            return;
        };
        let actual = std::env::var_os("MOOR_SESSION_SEMANTIC_TOKEN").map_or_else(
            || "absent".into(),
            |value| value.to_string_lossy().into_owned(),
        );
        std::fs::write(output, actual).unwrap();
    }

    #[test]
    fn prepublication_exit_probe() {
        let (Some(ready), Some(release)) = (
            std::env::var_os(EARLY_EXIT_READY),
            std::env::var_os(EARLY_EXIT_RELEASE),
        ) else {
            return;
        };
        print!("{}", String::from_utf8_lossy(EARLY_EXIT_OUTPUT));
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
        std::fs::write(ready, b"ready").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !Path::new(&release).exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "prepublication exit release timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        std::process::exit(23);
    }

    #[test]
    fn requested_child_semantic_token_is_fresh_iff_events_are_enabled() {
        let root = invoked_root();
        let _ = moor(&["list"]);
        let mut enabled_tokens = Vec::new();
        for (at, events) in [false, true, true].into_iter().enumerate() {
            let label = if events { "enabled" } else { "disabled" };
            let session = format!("semantic-token-{label}-{}", std::process::id());
            let session = format!("{session}-{at}");
            let event = root.join(format!("{session}-events"));
            let probe = root.join(format!("{session}-probe"));
            let _ = std::fs::remove_file(&probe);
            let mut command = Command::new(env!("CARGO_BIN_EXE_moor"));
            command
                .env("MOOR_SESSION_SEMANTIC_TOKEN", "poison")
                .env(SEMANTIC_TOKEN_PROBE, &probe)
                .args(["run", &session]);
            if events {
                command.arg("-T").arg(&event);
            }
            let output = command
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "launch_paths::semantic_token_probe",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            let token = std::fs::read_to_string(&probe).unwrap();
            if events {
                assert_ne!(token, "poison", "inherited semantic token was not replaced");
                assert!(
                    token.len() == 32
                        && token
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                    "semantic token is not lowercase 32-hex: {token:?}"
                );
                enabled_tokens.push(token);
            } else {
                assert_eq!(token, "absent", "inherited semantic token: {token:?}");
            }
            let removed = moor(&["rm", "-q", &session]);
            assert!(removed.status.success(), "{removed:?}");
            assert!(!event.exists(), "event store survived removal: {event:?}");
            std::fs::remove_file(probe).unwrap();
        }
        assert_ne!(enabled_tokens[0], enabled_tokens[1]);
    }

    #[test]
    fn redirected_stderr_is_opened_once_and_requires_the_exact_protected_file() {
        let root = invoked_root();
        let _ = moor(&["list"]);
        let program = moor::name::program(Path::new(env!("CARGO_BIN_EXE_moor")).as_os_str());
        let sink = root.join(format!("stderr-sink-{}", std::process::id()));
        std::fs::write(&sink, b"before\r\n").unwrap();
        protect_file(&sink);
        let session = format!("stderr-ok-{}", std::process::id());
        let output = moor(&[
            "run",
            &session,
            "-2",
            sink.to_str().unwrap(),
            "cmd.exe",
            "/d",
            "/c",
            "echo after 1>&2",
        ]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        assert_eq!(std::fs::read(&sink).unwrap(), b"before\r\nafter \r\n");
        assert!(moor(&["rm", "-q", &session]).status.success());

        for (label, path, cause) in [
            (
                "missing",
                root.join(format!("stderr-sink-missing-{}", std::process::id())),
                "missing",
            ),
            (
                "broad",
                root.join(format!("stderr-sink-broad-{}", std::process::id())),
                "broad-dacl",
            ),
        ] {
            if label == "broad" {
                std::fs::write(&path, b"").unwrap();
                own_file(&path);
            }
            let session = format!("stderr-{label}-{}", std::process::id());
            let output = moor(&[
                "run",
                &session,
                "-2",
                path.to_str().unwrap(),
                "cmd.exe",
                "/d",
                "/c",
                "exit 0",
            ]);
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                format!(
                    "{program}: standard-error sink rejected: {} ({cause})\n",
                    moor::name::render(path.as_os_str())
                ),
                "{output:?}"
            );
            assert!(!root.join(&session).exists());
            let _ = std::fs::remove_file(path);
        }
        std::fs::remove_file(sink).unwrap();
    }

    #[test]
    fn instrumentation_operand_rejections_use_the_frozen_row_and_original_spelling() {
        let root = invoked_root();
        let _ = moor(&["list"]);
        let program = moor::name::program(Path::new(env!("CARGO_BIN_EXE_moor")).as_os_str());
        let missing = root.join(format!("instrument-source-missing-{}", std::process::id()));
        let broad = root.join(format!("instrument-source-broad-{}", std::process::id()));
        std::fs::write(&broad, b"not a DLL").unwrap();
        own_file(&broad);
        let relative = PathBuf::from(format!("relative-instrument-{}", std::process::id()));
        for (label, path, cause) in [
            ("missing", missing, "missing"),
            ("broad", broad.clone(), "broad-dacl"),
            ("relative", relative, "not-absolute"),
        ] {
            let session = format!("instrument-{label}-{}", std::process::id());
            let output = Command::new(env!("CARGO_BIN_EXE_moor"))
                .args(["run", &session, "-S"])
                .arg(&path)
                .args(["cmd.exe", "/d", "/c", "exit 0"])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                format!(
                    "{program}: instrumentation rejected: {} ({cause})\n",
                    moor::name::render(path.as_os_str())
                ),
                "{output:?}"
            );
            assert!(!root.join(session).exists());
        }
        std::fs::remove_file(broad).unwrap();
    }

    #[test]
    fn fast_prepublication_exit_is_durable_and_status_is_mode_specific() {
        use moor::store::{Kind, Store};

        let root = invoked_root();
        let _ = moor(&["list"]);
        let program = moor::name::program(Path::new(env!("CARGO_BIN_EXE_moor")).as_os_str());
        for (mode, expected) in [("start", 1), ("run", 23)] {
            let session = format!("early-{mode}-{}", std::process::id());
            let marker = root.join(&session);
            let event = root.join(format!("{session}-events"));
            let ready = root.join(format!("{session}-child-ready"));
            let release = root.join(format!("{session}-child-release"));
            let _ = std::fs::remove_file(&ready);
            let _ = std::fs::remove_file(&release);
            let mut child = Command::new(env!("CARGO_BIN_EXE_moor"))
                .args([mode, &session, "-T"])
                .arg(&event)
                .arg("-S")
                .arg(instrumentation_fixture())
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "launch_paths::prepublication_exit_probe",
                    "--nocapture",
                ])
                .env(EARLY_EXIT_READY, &ready)
                .env(EARLY_EXIT_RELEASE, &release)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !event.is_dir() {
                if let Some(status) = child.try_wait().unwrap() {
                    let output = child.wait_with_output().unwrap();
                    panic!("creator exited before event materialization: {status:?}: {output:?}");
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "event materialization timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
                .and_then(|mut file| std::io::Write::write_all(&mut file, b"competing"))
                .unwrap();
            while !ready.exists() {
                if let Some(status) = child.try_wait().unwrap() {
                    let output = child.wait_with_output().unwrap();
                    panic!("creator exited before child readiness: {status:?}: {output:?}");
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "child readiness timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            std::fs::write(&release, b"release").unwrap();
            let output = child.wait_with_output().unwrap();
            let _ = std::fs::remove_file(&ready);
            let _ = std::fs::remove_file(&release);
            assert_eq!(output.status.code(), Some(expected), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            let diagnostic = (mode == "start")
                .then(|| format!("{program}: child exited before session publication\n"));
            assert_eq!(
                output.stderr,
                diagnostic.as_deref().unwrap_or_default().as_bytes(),
                "{output:?}"
            );
            assert_eq!(std::fs::read(&marker).unwrap(), b"competing");
            std::fs::remove_file(&marker).unwrap();

            let (commit, lifecycle) =
                Store::read_only(&companion(&marker, ".exit"), Kind::Exit, 1).unwrap();
            let lifecycle = String::from_utf8(lifecycle).unwrap();
            assert_eq!(commit.index, 2, "{lifecycle}");
            assert!(lifecycle.contains("\"phase\":\"exited\""), "{lifecycle}");
            assert!(lifecycle.contains("\"code\":23"), "{lifecycle}");
            let lifecycle: serde_json::Value = serde_json::from_str(&lifecycle).unwrap();
            let instrument = lifecycle["instrument_path"]
                .as_str()
                .and_then(|encoded| {
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .ok()
                })
                .and_then(|bytes| moor::windows::wtf8_decode(&bytes).ok())
                .map(|wide| OsString::from_wide(&wide))
                .map(PathBuf::from)
                .expect("durable exit omitted the instrumentation stage");
            assert!(
                instrument.is_file(),
                "missing instrument stage: {instrument:?}"
            );
            let (_, events) = Store::read_only(&event, Kind::Event, 1).unwrap();
            assert!(
                String::from_utf8(events)
                    .unwrap()
                    .contains("\"type\":\"exit\"")
            );
            assert!(
                Store::read_only(&companion(&marker, ".log"), Kind::Log, 1).is_ok(),
                "log store was not retained"
            );
            let (_, log) = Store::read_only(&companion(&marker, ".log"), Kind::Log, 1).unwrap();
            assert_eq!(
                log.windows(EARLY_EXIT_OUTPUT.len())
                    .filter(|bytes| *bytes == EARLY_EXIT_OUTPUT)
                    .count(),
                1,
                "child output was not retained exactly once"
            );
            let listed = moor(&["list", "-a"]);
            let listed = String::from_utf8(listed.stdout).unwrap();
            assert!(
                listed
                    .lines()
                    .any(|line| line.contains(&session) && line.contains("[exited]")),
                "{listed:?}"
            );
            let removed = moor(&["rm", "-q", &session]);
            assert!(removed.status.success(), "{removed:?}");
            assert!(!event.exists(), "event store survived removal: {event:?}");
            assert!(
                std::fs::symlink_metadata(&instrument).is_err(),
                "instrument stage survived removal: {instrument:?}"
            );
        }
    }

    #[test]
    fn instrumentation_exit_before_release_rolls_back_instead_of_becoming_exited() {
        let root = invoked_root();
        let _ = moor(&["list"]);
        let session = format!("instrument-exit-{}", std::process::id());
        let marker = root.join(&session);
        let event = root.join(format!("{session}-events"));
        let output = Command::new(env!("CARGO_BIN_EXE_moor"))
            .env("MOOR_TEST_INSTRUMENT_EXIT", "1")
            .arg("run")
            .arg(&session)
            .arg("-T")
            .arg(&event)
            .arg("-S")
            .arg(instrumentation_fixture())
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "launch_paths::publication_waiter", "--nocapture"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr)
                .contains("child exited before session publication"),
            "pre-release failure was converted to a natural exit: {output:?}"
        );
        for path in [
            marker.clone(),
            companion(&marker, ".log"),
            companion(&marker, ".exit"),
            event,
        ] {
            assert!(std::fs::symlink_metadata(&path).is_err(), "leaked {path:?}");
        }
        assert!(
            !String::from_utf8(moor(&["list", "-a"]).stdout)
                .unwrap()
                .lines()
                .any(|line| line.contains(&session)),
            "pre-release instrumentation failure became discoverable residue"
        );
    }

    fn published_run(session: &OsStr, event: Option<&Path>, marker: &Path) -> Output {
        let release = companion(marker, ".release");
        let _ = std::fs::remove_file(&release);
        let mut command = Command::new(env!("CARGO_BIN_EXE_moor"));
        command.arg("run").arg(session);
        if let Some(event) = event {
            command.arg("-T").arg(event);
        }
        let mut child = command
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "launch_paths::publication_waiter", "--nocapture"])
            .env(PUBLICATION_RELEASE, &release)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                panic!("child exited before publication: {status:?}: {output:?}");
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!("marker publication timed out: {output:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::fs::write(&release, b"release").unwrap();
        let output = child.wait_with_output().unwrap();
        let _ = std::fs::remove_file(release);
        output
    }

    #[test]
    fn child_start_failure_is_127_crlf_and_rolls_back_unpublished_artifacts() {
        let executable = std::env::temp_dir().join(format!(
            "moor-exec-failure-{}-{}.exe",
            std::process::id(),
            moor::runtime::private::now()
        ));
        std::fs::copy(env!("CARGO_BIN_EXE_moor"), &executable).unwrap();
        let root = invoked_root_for(&executable);
        let _ = Command::new(&executable).arg("list").output().unwrap();
        let instrument = instrumentation_fixture();
        let missing = root.join(format!("missing-child-{}", std::process::id()));
        let system = Command::new(&missing).spawn().unwrap_err().to_string();
        let program = moor::name::program(executable.as_os_str());
        let instruments = || {
            let mut paths = std::fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|value| value == "instrument"))
                .collect::<Vec<_>>();
            paths.sort();
            paths
        };
        let prior_instruments = instruments();

        for mode in ["run", "start"] {
            let session = format!("exec-failure-{mode}-{}", std::process::id());
            let marker = root.join(&session);
            let event = root.join(format!("{session}-events"));
            let out = Command::new(&executable)
                .args([
                    mode,
                    &session,
                    "-T",
                    event.to_str().unwrap(),
                    "-S",
                    instrument.to_str().unwrap(),
                    missing.to_str().unwrap(),
                ])
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(127), "{out:?}");
            assert!(out.stdout.is_empty(), "{out:?}");
            assert_eq!(
                out.stderr,
                format!(
                    "{program}: could not execute {}: {system}\r\n",
                    moor::name::render(missing.as_os_str())
                )
                .as_bytes(),
                "{out:?}"
            );
            for path in [
                marker.clone(),
                companion(&marker, ".log"),
                companion(&marker, ".exit"),
                event.clone(),
            ] {
                assert!(std::fs::symlink_metadata(&path).is_err(), "leaked {path:?}");
            }
            let stage = format!("{session}.stage-");
            assert!(
                std::fs::read_dir(&root).unwrap().all(|entry| {
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&stage)
                }),
                "leaked marker stage for {session}"
            );
            assert_eq!(
                instruments(),
                prior_instruments,
                "leaked instrumentation stage"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match std::fs::remove_file(&executable) {
                Ok(()) => break,
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("detached launch retained test executable: {error}"),
            }
        }
    }

    #[test]
    fn rejected_supervised_generation_does_not_materialize_the_event_directory() {
        let root = invoked_root();
        let _ = moor(&["list"]);
        let session = format!("generation-preflight-{}", std::process::id());
        let marker = root.join(&session);
        let event = root.join(format!("{session}-events"));
        let invoked = Path::new(env!("CARGO_BIN_EXE_moor")).file_name().unwrap();
        let generation = moor::runtime::private::environment_key(invoked, "_GENERATION");
        let launch = moor::runtime::private::environment_key(invoked, "_LAUNCH_CHANNEL");
        let out = Command::new(env!("CARGO_BIN_EXE_moor"))
            .env(launch, "not-a-handle")
            .env(&generation, "2")
            .env("MOOR_SESSION_GENERATION", "2")
            .args([
                "start",
                &session,
                "-T",
                event.to_str().unwrap(),
                "powershell.exe",
                "-NoProfile",
                "-Command",
                "exit 0",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        for path in [marker, event] {
            assert!(std::fs::symlink_metadata(&path).is_err(), "leaked {path:?}");
        }
    }

    #[test]
    fn normal_retirement_deletes_only_the_published_marker_identity() {
        let root = invoked_root();
        let _ = moor(&["list"]);
        let session = format!("retirement-identity-{}", std::process::id());
        let marker = root.join(&session);
        let displaced = root.join(format!("{session}-displaced"));
        let release = root.join(format!("{session}-release"));
        let _ = std::fs::remove_file(&release);
        let out = Command::new(env!("CARGO_BIN_EXE_moor"))
            .args(["start", &session])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "launch_paths::publication_waiter", "--nocapture"])
            .env(PUBLICATION_RELEASE, &release)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        std::fs::rename(&marker, &displaced).unwrap();
        std::fs::write(&marker, b"successor").unwrap();
        std::fs::write(&release, b"go").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while displaced.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let successor = std::fs::read(&marker);
        let original_survived = displaced.exists();
        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(&displaced);
        let _ = std::fs::remove_file(&release);
        for suffix in [".log", ".exit"] {
            let _ = std::fs::remove_dir_all(companion(&marker, suffix));
        }
        assert_eq!(successor.unwrap(), b"successor");
        assert!(
            !original_survived,
            "published marker identity survived retirement"
        );
    }

    #[test]
    fn publication_conflict_rolls_back_owned_artifacts_but_preserves_the_conflict() {
        let root = invoked_root();
        let _ = moor(&["list"]);
        let session = format!("publish-race-{}", std::process::id());
        let marker = root.join(&session);
        let event = root.join(format!("{session}-events"));
        let release = root.join(format!("{session}-child-release"));
        let _ = std::fs::remove_file(&release);
        let watched = event.clone();
        let competing = marker.clone();
        let racer = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if watched.is_dir() {
                    return std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(competing)
                        .and_then(|mut file| std::io::Write::write_all(&mut file, b"competing"))
                        .is_ok();
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            false
        });
        let out = Command::new(env!("CARGO_BIN_EXE_moor"))
            .args(["start", &session, "-T"])
            .arg(&event)
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "launch_paths::publication_waiter", "--nocapture"])
            .env(PUBLICATION_RELEASE, &release)
            .output()
            .unwrap();
        assert!(racer.join().unwrap(), "publication race was not installed");
        let competing = std::fs::read(&marker);
        let leaked = [
            companion(&marker, ".log"),
            companion(&marker, ".exit"),
            event,
        ]
        .into_iter()
        .filter(|path| std::fs::symlink_metadata(path).is_ok())
        .collect::<Vec<_>>();
        let stage = format!("{session}.stage-");
        let staged = std::fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(&stage)
        });
        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(&release);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert_eq!(competing.unwrap(), b"competing");
        assert!(leaked.is_empty(), "leaked {leaked:?}");
        assert!(!staged, "leaked marker stage");
    }

    #[test]
    fn rejected_working_directory_names_the_directory_not_the_executable() {
        // Closure §6.2 / OB-32 on Windows: `could not enter <path> (<cause>)`,
        // stderr, status 1 — never 127, never the executable's name.
        let dir = std::env::temp_dir().join(format!("moor-win-wd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let session = dir.join("wd");
        // OB-29: expectations use name::render, which escapes Windows
        // backslashes; Path::display would pin the wrong contract.
        let rendered = |path: &std::path::Path| moor::name::render(path.as_os_str());
        // OB-29: the diagnostic prefix is the invoked basename, which is
        // moor.exe under CARGO_BIN_EXE, not a hardcoded "moor".
        let program =
            moor::name::program(std::path::Path::new(env!("CARGO_BIN_EXE_moor")).as_os_str());

        let gone = dir.join("nonexistent");
        let out = moor(&[
            "run",
            session.to_str().unwrap(),
            "-d",
            gone.to_str().unwrap(),
            "cmd",
            "/c",
            "exit",
        ]);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            stderr,
            format!("{program}: could not enter {} (missing)\n", rendered(&gone)),
            "{out:?}"
        );

        // Detached creation owns the same one-line diagnostic; the launch
        // result pipe must not race it away or make the common layer print it
        // a second time.
        let out = moor(&[
            "start",
            session.to_str().unwrap(),
            "-d",
            gone.to_str().unwrap(),
            "cmd",
            "/c",
            "exit",
        ]);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("{program}: could not enter {} (missing)\n", rendered(&gone)),
            "{out:?}"
        );

        let file = dir.join("plain");
        std::fs::write(&file, b"x").unwrap();
        let out = moor(&[
            "run",
            session.to_str().unwrap(),
            "-d",
            file.to_str().unwrap(),
            "cmd",
            "/c",
            "exit",
        ]);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!(
                "{program}: could not enter {} (not-directory)\n",
                rendered(&file)
            ),
            "{out:?}"
        );

        // Precedence: a rejected directory wins over a missing executable —
        // status 1 with the directory row, not 127 with the exec row.
        let out = moor(&[
            "run",
            session.to_str().unwrap(),
            "-d",
            gone.to_str().unwrap(),
            "no-such-executable-anywhere",
        ]);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .starts_with(&format!("{program}: could not enter ")),
            "directory validation must precede executable resolution: {out:?}"
        );

        // A real successful launch proves the child inherits the requested
        // directory, rather than a validator that rejects every operand.
        let proof = dir.join("cwd-proof");
        let out = moor(&[
            "run",
            session.to_str().unwrap(),
            "-d",
            dir.to_str().unwrap(),
            "cmd",
            "/c",
            "type nul > cwd-proof",
        ]);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert!(
            proof.is_file(),
            "child did not enter the requested directory"
        );

        // Denied entry is exactly not-searchable. A full deny is deliberate:
        // hosted service accounts may hold traverse-bypass privilege, which
        // can make a narrow (X) denial ineffective.
        let sealed = dir.join("no-traverse");
        std::fs::create_dir(&sealed).unwrap();
        let user = std::env::var("USERNAME").unwrap();
        let denied = Command::new("icacls")
            .args([sealed.to_str().unwrap(), "/deny", &format!("{user}:(F)")])
            .output()
            .unwrap();
        // Fixture setup is mandatory: a silent skip would turn the required
        // behaviour into an unreported pass on the supported lanes.
        assert!(
            denied.status.success(),
            "icacls deny failed; the denied-traverse fixture is required: {denied:?}"
        );
        {
            let out = moor(&[
                "run",
                session.to_str().unwrap(),
                "-d",
                sealed.to_str().unwrap(),
                "cmd",
                "/c",
                "exit",
            ]);
            let restored = Command::new("icacls")
                .args([sealed.to_str().unwrap(), "/remove:d", &user])
                .output()
                .unwrap();
            assert!(restored.status.success(), "{restored:?}");
            assert_eq!(out.status.code(), Some(1), "{out:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stderr),
                format!(
                    "{program}: could not enter {} (not-searchable)\n",
                    rendered(&sealed)
                ),
                "{out:?}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_form_session_does_not_move_the_event_root_boundary() {
        let dir = std::env::temp_dir().join(format!("moor-win-event-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let session = dir.join("path-form-session");
        let event = dir.join("outside-events");
        let initial = published_run(session.as_os_str(), None, &session);
        assert_eq!(initial.status.code(), Some(0), "{initial:?}");
        let lifecycle = companion(&session, ".exit");
        assert!(lifecycle.is_dir(), "missing stale fixture {lifecycle:?}");
        let out = moor(&[
            "run",
            session.to_str().unwrap(),
            "-T",
            event.to_str().unwrap(),
            "cmd",
            "/c",
            "exit",
        ]);
        let program =
            moor::name::program(std::path::Path::new(env!("CARGO_BIN_EXE_moor")).as_os_str());
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!(
                "{program}: event store rejected: {} (outside-root)\n",
                moor::name::render(event.as_os_str())
            ),
            "{out:?}"
        );
        assert!(!session.exists());
        assert!(!event.exists());
        assert!(
            lifecycle.is_dir(),
            "outside-root rejection mutated stale lifecycle state"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_absolute_event_operands_preserve_spelling_and_precede_cleanup_or_child_start() {
        let root = invoked_root();
        let _ = moor(&["list"]);
        let program = moor::name::program(Path::new(env!("CARGO_BIN_EXE_moor")).as_os_str());
        let relative = PathBuf::from(format!("relative-events-{}", std::process::id()));
        let current = std::env::current_dir().unwrap();
        let drive = current
            .components()
            .next()
            .unwrap()
            .as_os_str()
            .to_string_lossy()
            .into_owned();
        let drive_relative = PathBuf::from(format!(
            "{drive}drive-relative-events-{}",
            std::process::id()
        ));
        let root_relative = PathBuf::from(format!("\\root-relative-events-{}", std::process::id()));
        for (at, operand) in [relative, drive_relative, root_relative]
            .into_iter()
            .enumerate()
        {
            assert!(
                !operand.is_absolute(),
                "fixture became absolute: {operand:?}"
            );
            let session = format!("relative-event-{at}-{}", std::process::id());
            let stale = root.join(&session);
            std::fs::write(&stale, b"foreign stale marker").unwrap();
            let child_proof = root.join(format!("{session}-child-proof"));
            let output = Command::new(env!("CARGO_BIN_EXE_moor"))
                .args(["start", &session, "-T"])
                .arg(&operand)
                .args(["cmd.exe", "/d", "/c", "type nul >"])
                .arg(&child_proof)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                format!(
                    "{program}: event store rejected: {} (not-absolute)\n",
                    moor::name::render(operand.as_os_str())
                ),
                "{output:?}"
            );
            assert_eq!(std::fs::read(&stale).unwrap(), b"foreign stale marker");
            assert!(
                !child_proof.exists(),
                "child started despite preflight refusal"
            );
            std::fs::remove_file(stale).unwrap();
        }
    }

    #[test]
    fn junction_cannot_escape_the_protected_event_root() {
        let dir = std::env::temp_dir().join(format!("moor-win-junction-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let outside = dir.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let listed = moor(&["list"]);
        assert!(listed.status.success(), "{listed:?}");
        let root = invoked_root();
        assert!(root.is_dir(), "missing invoked root {root:?}");
        let junction = root.join(format!("event-junction-{}", std::process::id()));
        let _ = std::fs::remove_dir(&junction);
        let linked = Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            linked.status.success(),
            "junction fixture failed: {linked:?}"
        );

        let session_name = format!("junction-session-{}", std::process::id());
        let event = junction.join("events");
        let out = moor(&[
            "run",
            &session_name,
            "-T",
            event.to_str().unwrap(),
            "cmd",
            "/c",
            "exit",
        ]);
        let program = moor::name::program(Path::new(env!("CARGO_BIN_EXE_moor")).as_os_str());
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!(
                "{program}: event store rejected: {} (reparse-point)\n",
                moor::name::render(event.as_os_str())
            ),
            "{out:?}"
        );
        assert!(!outside.join("events").exists());
        assert!(!root.join(&session_name).exists());

        std::fs::remove_dir(&junction).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn equivalent_case_event_root_is_accepted() {
        let root = invoked_root();
        let leaf = root.file_name().unwrap().to_string_lossy();
        let alias = root.with_file_name(leaf.to_ascii_uppercase());
        assert_ne!(root, alias, "fixture must use different path spelling");
        let event = alias.join(format!("case-events-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&event);
        let session = format!("case-session-{}", std::process::id());

        let marker = root.join(&session);
        let out = published_run(OsStr::new(&session), Some(&event), &marker);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert!(event.is_dir(), "event store was not created at {event:?}");

        let removed = moor(&["rm", &session]);
        assert_eq!(removed.status.code(), Some(0), "{removed:?}");
        assert!(!event.exists(), "event store survived removal: {event:?}");
    }

    mod native_console {
        use super::{companion, invoked_root, moor};
        use moor::store::{Kind, Store};
        use std::fs::File;
        use std::io::{self, Read, Write};
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
        use std::os::windows::process::CommandExt;
        use std::process::{Command, ExitStatus, Stdio};
        use std::sync::atomic::{AtomicU8, Ordering};
        use std::thread;
        use std::time::{Duration, Instant};
        use windows_spawn::{AsPseudoConsole, Child, Command as SpawnCommand, SpawnOptions};
        use windows_sys::Win32::Foundation::{
            FALSE, HANDLE, STATUS_CONTROL_C_EXIT, TRUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
        };
        use windows_sys::Win32::Globalization::CP_UTF8;
        use windows_sys::Win32::Storage::FileSystem::ReadFile;
        use windows_sys::Win32::System::Console::{
            AttachConsole, CONSOLE_SCREEN_BUFFER_INFO, COORD, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT,
            CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, ClosePseudoConsole, CreatePseudoConsole,
            ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT,
            ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE,
            ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
            ENHANCED_KEY, FlushConsoleInputBuffer, FreeConsole, GenerateConsoleCtrlEvent,
            GetConsoleCP, GetConsoleMode, GetConsoleScreenBufferInfo, GetConsoleWindow,
            GetStdHandle, HPCON, INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT, KEY_EVENT_RECORD,
            ReadConsoleInputW, ResizePseudoConsole, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            SetConsoleCP, SetConsoleCtrlHandler, SetConsoleMode, WINDOW_BUFFER_SIZE_EVENT,
            WriteConsoleInputW,
        };
        use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
        use windows_sys::Win32::System::Threading::{
            CREATE_NEW_CONSOLE, CREATE_NEW_PROCESS_GROUP, WaitForSingleObject,
        };
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_UP;
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

        struct Console {
            value: HPCON,
            input: Option<File>,
            output: Option<File>,
            received: Vec<u8>,
        }

        impl Console {
            fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
                let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
                if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(unsafe {
                    (
                        OwnedHandle::from_raw_handle(read as RawHandle),
                        OwnedHandle::from_raw_handle(write as RawHandle),
                    )
                })
            }

            fn new(rows: i16, columns: i16) -> io::Result<Self> {
                Self::new_with_flags(rows, columns, 0)
            }

            fn new_with_flags(rows: i16, columns: i16, flags: u32) -> io::Result<Self> {
                let (input_reader, input_writer) = Self::pipe()?;
                let (output_reader, output_writer) = Self::pipe()?;
                let mut value = 0;
                let result = unsafe {
                    CreatePseudoConsole(
                        COORD {
                            X: columns,
                            Y: rows,
                        },
                        input_reader.as_raw_handle() as HANDLE,
                        output_writer.as_raw_handle() as HANDLE,
                        flags,
                        &mut value,
                    )
                };
                if result < 0 {
                    return Err(io::Error::other(format!(
                        "CreatePseudoConsole failed with HRESULT {result:#x}"
                    )));
                }
                drop((input_reader, output_writer));
                Ok(Self {
                    value,
                    input: Some(input_writer.into()),
                    output: Some(output_reader.into()),
                    received: Vec::new(),
                })
            }

            fn write(&self, bytes: &[u8]) -> io::Result<()> {
                let mut input = self.input.as_ref().unwrap();
                input.write_all(bytes)
            }

            fn resize(&self, rows: i16, columns: i16) -> io::Result<()> {
                let result = unsafe {
                    ResizePseudoConsole(
                        self.value,
                        COORD {
                            X: columns,
                            Y: rows,
                        },
                    )
                };
                (result >= 0)
                    .then_some(())
                    .ok_or_else(|| io::Error::other(format!("ResizePseudoConsole: {result:#x}")))
            }

            fn pump(&mut self) -> io::Result<()> {
                let output = self.output.as_mut().unwrap();
                let mut available = 0;
                if unsafe {
                    PeekNamedPipe(
                        output.as_raw_handle() as HANDLE,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                if available != 0 {
                    let mut bytes = vec![0; available as usize];
                    output.read_exact(&mut bytes)?;
                    self.received.extend_from_slice(&bytes);
                }
                Ok(())
            }

            fn wait_for(&mut self, marker: &[u8], timeout: Duration) -> io::Result<()> {
                let deadline = Instant::now() + timeout;
                loop {
                    self.pump()?;
                    if self
                        .received
                        .windows(marker.len())
                        .any(|window| window == marker)
                    {
                        return Ok(());
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "missing {:?} in {:?}",
                        String::from_utf8_lossy(marker),
                        String::from_utf8_lossy(&self.received)
                    ),
                ))
            }

            fn spawn(&self, mut command: SpawnCommand) -> io::Result<Child> {
                command.spawn_with(SpawnOptions::new().pseudoconsole(self))
            }
        }

        impl Drop for Console {
            fn drop(&mut self) {
                drop(self.input.take());
                drop(self.output.take());
                let value = self.value;
                let _ = thread::spawn(move || unsafe { ClosePseudoConsole(value) });
            }
        }

        unsafe impl AsPseudoConsole for Console {
            fn raw_pseudoconsole(&self) -> isize {
                self.value
            }
        }

        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = moor(&["kill", "-f", "-q", &self.0]);
                let _ = moor(&["rm", "-q", &self.0]);
            }
        }

        fn wait_spawn(child: &mut Child, timeout: Duration) -> io::Result<ExitStatus> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if let Some(status) = child.try_wait()? {
                    return Ok(status);
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
            Err(io::Error::new(io::ErrorKind::TimedOut, "child timed out"))
        }

        fn wait_console_spawn(
            console: &mut Console,
            child: &mut Child,
            timeout: Duration,
        ) -> io::Result<ExitStatus> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                console.pump()?;
                if let Some(status) = child.try_wait()? {
                    console.pump()?;
                    return Ok(status);
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "child timed out; console output: {:?}",
                    String::from_utf8_lossy(&console.received)
                ),
            ))
        }

        fn wait_std(child: &mut std::process::Child, timeout: Duration) -> io::Result<ExitStatus> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if let Some(status) = child.try_wait()? {
                    return Ok(status);
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "moor run timed out",
            ))
        }

        fn probe(console: &Console, label: &str) -> io::Result<Child> {
            let mut command = SpawnCommand::new(std::env::current_exe()?);
            command
                .args([
                    "--exact",
                    "launch_paths::native_console::console_mode_probe",
                    "--nocapture",
                ])
                .env("MOOR_CONSOLE_MODE_PROBE", label);
            console.spawn(command)
        }

        fn modes(bytes: &[u8], label: &str) -> (u32, u32) {
            let text = String::from_utf8_lossy(bytes);
            let prefix = format!("MOOR-MODE-{label}:");
            let value = &text[text.rfind(&prefix).unwrap() + prefix.len()..];
            let mut fields = value.split(':');
            (
                u32::from_str_radix(fields.next().unwrap(), 16).unwrap(),
                u32::from_str_radix(fields.next().unwrap(), 16).unwrap(),
            )
        }

        fn wait_modes(console: &mut Console, label: &str) -> (u32, u32) {
            let end = format!("MOOR-MODE-{label}-END");
            console
                .wait_for(end.as_bytes(), Duration::from_secs(5))
                .unwrap();
            modes(&console.received, label)
        }

        #[test]
        fn console_mode_probe() {
            let Some(label) = std::env::var_os("MOOR_CONSOLE_MODE_PROBE") else {
                return;
            };
            if label == "before" {
                assert_ne!(unsafe { SetConsoleCP(CP_UTF8) }, 0);
            }
            assert_eq!(unsafe { GetConsoleCP() }, CP_UTF8);
            let (mut input, mut output) = (0, 0);
            assert!(unsafe {
                GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut input) != 0
                    && GetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), &mut output) != 0
            });
            println!(
                "MOOR-MODE-{}:{input:08x}:{output:08x}:END",
                label.to_string_lossy()
            );
            println!("MOOR-MODE-{}-END", label.to_string_lossy());
        }

        #[test]
        fn console_geometry_probe() {
            let Some(mode) = std::env::var_os("MOOR_CONSOLE_GEOMETRY_PROBE") else {
                return;
            };
            let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
            let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
            assert!(unsafe { GetConsoleScreenBufferInfo(output, &mut info) } != 0);
            let rows = info.srWindow.Bottom - info.srWindow.Top + 1;
            let columns = info.srWindow.Right - info.srWindow.Left + 1;
            println!("MOOR-GEOM-{}:{rows}:{columns}", mode.to_string_lossy());
            io::stdout().flush().unwrap();
            if mode.as_os_str() == "foreground" {
                let release = std::env::var_os("MOOR_CONSOLE_GEOMETRY_RELEASE").unwrap();
                while !std::path::Path::new(&release).exists() {
                    thread::sleep(Duration::from_millis(10));
                }
                return;
            }
            let mut input_mode = 0;
            assert!(unsafe { GetConsoleMode(input, &mut input_mode) } != 0);
            assert!(unsafe {
                SetConsoleMode(
                    input,
                    (input_mode | ENABLE_WINDOW_INPUT) & !ENABLE_VIRTUAL_TERMINAL_INPUT,
                ) != 0
            });
            thread::sleep(Duration::from_millis(100));
            assert!(unsafe { FlushConsoleInputBuffer(input) } != 0);
            println!("MOOR-GEOM-READY");
            let mut resized = 0;
            loop {
                let (mut record, mut read) = (INPUT_RECORD::default(), 0);
                assert!(unsafe { ReadConsoleInputW(input, &mut record, 1, &mut read) } != 0);
                if read == 0 {
                    continue;
                }
                if record.EventType == WINDOW_BUFFER_SIZE_EVENT as u16 {
                    let size = unsafe { record.Event.WindowBufferSizeEvent.dwSize };
                    resized += 1;
                    println!("MOOR-RESIZE:{resized}:{}:{}", size.Y, size.X);
                } else if record.EventType == KEY_EVENT as u16 {
                    let key = unsafe { record.Event.KeyEvent };
                    let character = unsafe { key.uChar.UnicodeChar };
                    if key.bKeyDown != 0 {
                        println!("MOOR-UNIT:{character:04X}:{resized}");
                        if let Some(character) = char::from_u32(character as u32) {
                            println!("MOOR-KEY:{character}:{resized}");
                        }
                    }
                }
                io::stdout().flush().unwrap();
            }
        }

        #[test]
        fn shipped_viewer_uses_real_geometry_resize_and_restores_console_modes() {
            let mut console = Console::new(37, 93).unwrap();
            let mut before = probe(&console, "before").unwrap();
            assert!(
                wait_spawn(&mut before, Duration::from_secs(5))
                    .unwrap()
                    .success()
            );
            let before_modes = wait_modes(&mut console, "before");
            let foreground_session = format!("console-run-e2e-{}", std::process::id());
            let _foreground_cleanup = Cleanup(foreground_session.clone());
            let foreground_release =
                invoked_root().join(format!("console-run-release-{}", std::process::id()));
            let _ = std::fs::remove_file(&foreground_release);
            let executable = std::env::current_exe().unwrap();
            let mut foreground = SpawnCommand::new(env!("CARGO_BIN_EXE_moor"));
            foreground
                .args([
                    "run".as_ref(),
                    foreground_session.as_ref(),
                    executable.as_os_str(),
                    "--exact".as_ref(),
                    "launch_paths::native_console::console_geometry_probe".as_ref(),
                    "--nocapture".as_ref(),
                ])
                .env("MOOR_CONSOLE_GEOMETRY_PROBE", "foreground")
                .env("MOOR_CONSOLE_GEOMETRY_RELEASE", &foreground_release);
            let mut foreground = console.spawn(foreground).unwrap();
            wait_log(
                &invoked_root().join(&foreground_session),
                b"MOOR-GEOM-foreground:37:93",
            );
            std::fs::write(&foreground_release, b"release").unwrap();
            assert!(
                wait_spawn(&mut foreground, Duration::from_secs(5))
                    .unwrap()
                    .success()
            );
            let _ = std::fs::remove_file(foreground_release);

            let session = format!("console-e2e-{}", std::process::id());
            let _cleanup = Cleanup(session.clone());
            let marker = invoked_root().join(&session);
            let mut command = SpawnCommand::new(env!("CARGO_BIN_EXE_moor"));
            command
                .args([
                    "new".as_ref(),
                    session.as_ref(),
                    executable.as_os_str(),
                    "--exact".as_ref(),
                    "launch_paths::native_console::console_geometry_probe".as_ref(),
                    "--nocapture".as_ref(),
                ])
                .env("MOOR_CONSOLE_GEOMETRY_PROBE", "detached");
            let mut viewer = console.spawn(command).unwrap();
            console
                .wait_for(b"MOOR-GEOM-detached:37:93", Duration::from_secs(10))
                .unwrap();
            console
                .wait_for(b"MOOR-GEOM-READY", Duration::from_secs(10))
                .unwrap();

            let mut live = probe(&console, "live").unwrap();
            assert!(
                wait_spawn(&mut live, Duration::from_secs(5))
                    .unwrap()
                    .success()
            );
            let (input, output) = wait_modes(&mut console, "live");
            assert_ne!(input & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);
            assert_eq!(
                input & (ENABLE_WINDOW_INPUT | ENABLE_MOUSE_INPUT | ENABLE_EXTENDED_FLAGS),
                ENABLE_WINDOW_INPUT | ENABLE_MOUSE_INPUT | ENABLE_EXTENDED_FLAGS
            );
            assert_eq!(
                input
                    & (ENABLE_LINE_INPUT
                        | ENABLE_ECHO_INPUT
                        | ENABLE_PROCESSED_INPUT
                        | ENABLE_QUICK_EDIT_MODE),
                0
            );
            assert_eq!(
                output & (ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING),
                ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING
            );

            console.resize(37, 93).unwrap();
            thread::sleep(Duration::from_millis(500));
            console.write(b"A").unwrap();
            console
                .wait_for(b"MOOR-KEY:A:0", Duration::from_secs(5))
                .unwrap();
            console.resize(41, 101).unwrap();
            console
                .wait_for(b"MOOR-RESIZE:1:41:101", Duration::from_secs(5))
                .unwrap();
            console.write(b"B").unwrap();
            console
                .wait_for(b"MOOR-KEY:B:1", Duration::from_secs(5))
                .unwrap();
            let trace = String::from_utf8_lossy(&console.received);
            let mut after = 0;
            for marker in ["MOOR-KEY:A:0", "MOOR-RESIZE:1:41:101", "MOOR-KEY:B:1"] {
                after += trace[after..].find(marker).expect("input/resize order") + marker.len();
            }
            console.write(&[0x1c]).unwrap();
            let detached = wait_console_spawn(&mut console, &mut viewer, Duration::from_secs(5))
                .unwrap_or_else(|error| {
                    panic!(
                        "{error}; console output: {:?}",
                        String::from_utf8_lossy(&console.received)
                    )
                });
            assert!(detached.success(), "{detached:?}");
            console
                .wait_for(WIN32_INPUT_DISABLE, Duration::from_secs(5))
                .unwrap();
            let disabled_at = last_marker(&console.received, WIN32_INPUT_DISABLE);
            assert!(
                marker.is_file(),
                "detach retired the session marker: {marker:?}"
            );
            let listed = moor(&["list"]);
            assert!(listed.status.success(), "{listed:?}");
            let listed = String::from_utf8(listed.stdout).unwrap();
            assert!(
                listed.lines().any(|line| line.contains(&session)),
                "detached session is not live: {listed:?}"
            );

            let mut after = probe(&console, "after").unwrap();
            assert!(
                wait_spawn(&mut after, Duration::from_secs(5))
                    .unwrap()
                    .success()
            );
            assert_eq!(wait_modes(&mut console, "after"), before_modes);
            assert!(
                disabled_at < last_marker(&console.received, b"MOOR-MODE-after:"),
                "input-mode disable did not precede console restoration evidence"
            );
            let killed = moor(&["kill", "-f", "-q", &session]);
            assert!(killed.status.success(), "{killed:?}");
        }

        const VT_INPUT_SENTINEL: &str = "MOOR_CONSOLE_VT_INPUT_SENTINEL";
        const CLOSE_READY: &str = "MOOR_CONSOLE_CLOSE_READY";
        const CLOSE_RELEASE: &str = "MOOR_CONSOLE_CLOSE_RELEASE";
        const VT_INPUT_READY: &[u8] = b"MOOR-VT-INPUT-READY";
        const VT_INPUT_VERIFIED: &[u8] = b"MOOR-VT-INPUT-VERIFIED";
        const VT_INPUT_EXPECTED: &[u8] = b"A\x1bOAZ";
        const WIN32_INPUT_MODE: u32 = 4;
        const WIN32_INPUT_ENABLE: &[u8] = b"\x1b[?9001h";
        const WIN32_INPUT_DISABLE: &[u8] = b"\x1b[?9001l";
        const WIN32_INPUT_VECTOR: &[u8] =
            b"\x1b[65;30;65;1;0;1_\x1b[38;72;0;1;256;1_\x1b[90;44;90;1;0;1_";
        const LEGACY_OUTER_CODEPAGE: u32 = 437;

        fn wait_file(path: &std::path::Path, timeout: Duration) {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if path.is_file() {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("timed out waiting for {path:?}");
        }

        fn last_marker(bytes: &[u8], marker: &[u8]) -> usize {
            bytes
                .windows(marker.len())
                .rposition(|window| window == marker)
                .unwrap_or_else(|| {
                    panic!(
                        "missing {:?} in {:?}",
                        String::from_utf8_lossy(marker),
                        String::from_utf8_lossy(bytes)
                    )
                })
        }

        #[test]
        fn console_vt_input_probe() {
            let Some(sentinel) = std::env::var_os(VT_INPUT_SENTINEL) else {
                return;
            };
            let sentinel = std::path::PathBuf::from(sentinel);
            let release = companion(&sentinel, ".release");
            let observed = companion(&sentinel, ".observed");
            let verified = companion(&sentinel, ".verified");
            let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            assert_ne!(unsafe { SetConsoleCP(CP_UTF8) }, 0);
            assert_eq!(unsafe { GetConsoleCP() }, CP_UTF8);

            let raw = ENABLE_PROCESSED_INPUT
                | ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_QUICK_EDIT_MODE
                | ENABLE_WINDOW_INPUT;
            let mut mode = 0;
            assert_ne!(unsafe { GetConsoleMode(input, &mut mode) }, 0);
            assert_ne!(
                unsafe {
                    SetConsoleMode(
                        input,
                        (mode | ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS) & !raw,
                    )
                },
                0
            );
            assert_ne!(unsafe { GetConsoleMode(input, &mut mode) }, 0);
            assert_ne!(mode & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);
            assert_eq!(mode & raw, 0);
            assert_ne!(mode & ENABLE_EXTENDED_FLAGS, 0);
            assert_ne!(unsafe { FlushConsoleInputBuffer(input) }, 0);

            println!("\x1b[?1h{}", String::from_utf8_lossy(VT_INPUT_READY));
            io::stdout().flush().unwrap();

            wait_file(&sentinel, Duration::from_secs(5));
            let mut received = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(5);
            while received.len() < VT_INPUT_EXPECTED.len() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "timed out waiting for VT input bytes: {received:?}"
                );
                let wait_ms = remaining.min(Duration::from_millis(100)).as_millis() as u32;
                match unsafe { WaitForSingleObject(input, wait_ms.max(1)) } {
                    WAIT_TIMEOUT => continue,
                    WAIT_OBJECT_0 => {}
                    wait => panic!("unexpected console input wait result {wait:#x}"),
                }
                let mut bytes = [0; 64];
                let mut count = 0;
                assert_ne!(
                    unsafe {
                        ReadFile(
                            input,
                            bytes.as_mut_ptr(),
                            bytes.len() as u32,
                            &mut count,
                            std::ptr::null_mut(),
                        )
                    },
                    0,
                    "ReadFile failed: {}",
                    io::Error::last_os_error()
                );
                assert_ne!(count, 0, "VT input closed before the sentinel");
                received.extend_from_slice(&bytes[..count as usize]);
            }
            std::fs::write(&observed, &received).unwrap();
            assert_eq!(received, VT_INPUT_EXPECTED);
            assert_eq!(
                unsafe { WaitForSingleObject(input, 500) },
                WAIT_TIMEOUT,
                "unexpected delayed VT input after the exact vector"
            );
            std::fs::write(&verified, b"verified").unwrap();
            println!("{}", String::from_utf8_lossy(VT_INPUT_VERIFIED));
            io::stdout().flush().unwrap();
            wait_file(&release, Duration::from_secs(10));
        }

        fn record_key(unit: u16) -> INPUT_RECORD {
            let mut key = KEY_EVENT_RECORD {
                bKeyDown: TRUE,
                wRepeatCount: 1,
                ..KEY_EVENT_RECORD::default()
            };
            key.uChar.UnicodeChar = unit;
            INPUT_RECORD {
                EventType: KEY_EVENT as u16,
                Event: INPUT_RECORD_0 { KeyEvent: key },
            }
        }

        fn record_up() -> INPUT_RECORD {
            let key = KEY_EVENT_RECORD {
                bKeyDown: TRUE,
                wRepeatCount: 1,
                wVirtualKeyCode: VK_UP,
                wVirtualScanCode: 0x48,
                dwControlKeyState: ENHANCED_KEY,
                ..KEY_EVENT_RECORD::default()
            };
            INPUT_RECORD {
                EventType: KEY_EVENT as u16,
                Event: INPUT_RECORD_0 { KeyEvent: key },
            }
        }

        #[test]
        fn console_record_sender() {
            let Some(sentinel) = std::env::var_os(VT_INPUT_SENTINEL) else {
                return;
            };
            let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            let previous = unsafe { GetConsoleCP() };
            assert_ne!(previous, 0);
            assert_ne!(unsafe { SetConsoleCP(LEGACY_OUTER_CODEPAGE) }, 0);
            assert_eq!(unsafe { GetConsoleCP() }, LEGACY_OUTER_CODEPAGE);
            let records = [
                record_key(u16::from(b'A')),
                record_up(),
                record_key(u16::from(b'Z')),
                record_key(0x1c),
            ];
            let mut written = 0;
            assert_ne!(
                unsafe {
                    WriteConsoleInputW(input, records.as_ptr(), records.len() as u32, &mut written)
                },
                0,
                "WriteConsoleInputW failed: {}",
                io::Error::last_os_error()
            );
            assert_eq!(written as usize, records.len());
            assert_ne!(unsafe { SetConsoleCP(previous) }, 0);
            std::fs::write(sentinel, b"complete").unwrap();
        }

        struct FileCleanup(Vec<std::path::PathBuf>);

        impl FileCleanup {
            fn new(paths: impl IntoIterator<Item = std::path::PathBuf>) -> Self {
                let paths = paths.into_iter().collect::<Vec<_>>();
                for path in &paths {
                    let _ = std::fs::remove_file(path);
                }
                Self(paths)
            }
        }

        impl Drop for FileCleanup {
            fn drop(&mut self) {
                for path in &self.0 {
                    let _ = std::fs::remove_file(path);
                }
            }
        }

        #[test]
        fn platform_w32_input_mode_preserves_application_cursor_semantics() {
            let mut console = Console::new_with_flags(37, 93, WIN32_INPUT_MODE).unwrap();
            let sentinel = std::env::temp_dir()
                .join(format!("moor-platform-w32-input-{}", std::process::id()));
            let release = companion(&sentinel, ".release");
            let observed = companion(&sentinel, ".observed");
            let verified = companion(&sentinel, ".verified");
            let _files = FileCleanup::new([
                sentinel.clone(),
                release.clone(),
                observed.clone(),
                verified.clone(),
            ]);

            let mut command = SpawnCommand::new(std::env::current_exe().unwrap());
            command
                .args([
                    std::ffi::OsStr::new("--exact"),
                    std::ffi::OsStr::new("launch_paths::native_console::console_vt_input_probe"),
                    std::ffi::OsStr::new("--nocapture"),
                ])
                .env(VT_INPUT_SENTINEL, &sentinel);
            let mut child = console.spawn(command).unwrap();
            console
                .wait_for(VT_INPUT_READY, Duration::from_secs(10))
                .unwrap();

            console.write(WIN32_INPUT_VECTOR).unwrap();
            std::fs::write(&sentinel, b"complete").unwrap();
            wait_file(&verified, Duration::from_secs(5));
            assert_eq!(std::fs::read(&observed).unwrap(), VT_INPUT_EXPECTED);
            std::fs::write(&release, b"release").unwrap();
            let status = wait_spawn(&mut child, Duration::from_secs(5)).unwrap();
            assert!(status.success(), "W32 input probe exited with {status:?}");
        }

        fn wait_lifecycle_exit(marker: &std::path::Path, timeout: Duration) -> String {
            let exit = companion(marker, ".exit");
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if let Ok((_, body)) = Store::read_only(&exit, Kind::Exit, 1) {
                    let body = String::from_utf8(body).unwrap();
                    if body.contains("\"phase\":\"exited\"") {
                        return body;
                    }
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("child lifecycle did not exit at {exit:?}");
        }

        #[test]
        fn shipped_viewer_preserves_exact_vt_input_bytes() {
            let mut console = Console::new(37, 93).unwrap();
            let mut before = probe(&console, "before").unwrap();
            assert!(
                wait_spawn(&mut before, Duration::from_secs(5))
                    .unwrap()
                    .success()
            );
            let before_modes = wait_modes(&mut console, "before");
            let session = format!("console-vt-input-{}", std::process::id());
            let _cleanup = Cleanup(session.clone());
            let marker = invoked_root().join(&session);
            let sentinel = std::env::temp_dir().join(format!(
                "moor-console-vt-input-sender-{}",
                std::process::id()
            ));
            let release = companion(&sentinel, ".release");
            let observed = companion(&sentinel, ".observed");
            let verified = companion(&sentinel, ".verified");
            let _files = FileCleanup::new([
                sentinel.clone(),
                release.clone(),
                observed.clone(),
                verified.clone(),
            ]);

            let executable = std::env::current_exe().unwrap();
            let mut command = SpawnCommand::new(env!("CARGO_BIN_EXE_moor"));
            command
                .args([
                    "new".as_ref(),
                    session.as_ref(),
                    executable.as_os_str(),
                    "--exact".as_ref(),
                    "launch_paths::native_console::console_vt_input_probe".as_ref(),
                    "--nocapture".as_ref(),
                ])
                .env(VT_INPUT_SENTINEL, &sentinel);
            let mut viewer = console.spawn(command).unwrap();
            console
                .wait_for(VT_INPUT_READY, Duration::from_secs(10))
                .unwrap();
            console
                .wait_for(WIN32_INPUT_ENABLE, Duration::from_secs(10))
                .unwrap();

            let mut sender = SpawnCommand::new(&executable);
            sender
                .args([
                    std::ffi::OsStr::new("--exact"),
                    std::ffi::OsStr::new("launch_paths::native_console::console_record_sender"),
                    std::ffi::OsStr::new("--nocapture"),
                ])
                .env(VT_INPUT_SENTINEL, &sentinel);
            let mut sender = console.spawn(sender).unwrap();
            let sender_status =
                wait_console_spawn(&mut console, &mut sender, Duration::from_secs(5)).unwrap();
            assert!(
                sender_status.success(),
                "record sender exited with {sender_status:?}; console output: {:?}",
                String::from_utf8_lossy(&console.received)
            );
            assert!(
                sentinel.is_file(),
                "record sender omitted completion sentinel"
            );
            wait_file(&verified, Duration::from_secs(5));
            assert_eq!(
                std::fs::read(&observed).unwrap(),
                VT_INPUT_EXPECTED,
                "outer console output: {:?}",
                String::from_utf8_lossy(&console.received)
            );

            let viewer_status =
                wait_console_spawn(&mut console, &mut viewer, Duration::from_secs(5)).unwrap();
            assert!(
                viewer_status.success(),
                "default detach failed: {viewer_status:?}; console output: {:?}",
                String::from_utf8_lossy(&console.received)
            );
            console
                .wait_for(WIN32_INPUT_DISABLE, Duration::from_secs(5))
                .unwrap();
            let disabled_at = last_marker(&console.received, WIN32_INPUT_DISABLE);
            assert!(
                marker.is_file(),
                "detach retired the session marker: {marker:?}"
            );
            let listed = moor(&["list"]);
            assert!(listed.status.success(), "{listed:?}");
            let listed = String::from_utf8(listed.stdout).unwrap();
            assert!(
                listed.lines().any(|line| line.contains(&session)),
                "detached session is not live: {listed:?}"
            );
            let mut after = probe(&console, "after").unwrap();
            assert!(
                wait_spawn(&mut after, Duration::from_secs(5))
                    .unwrap()
                    .success()
            );
            assert_eq!(wait_modes(&mut console, "after"), before_modes);
            assert!(
                disabled_at < last_marker(&console.received, b"MOOR-MODE-after:"),
                "input-mode disable did not precede console restoration evidence"
            );

            std::fs::write(&release, b"release").unwrap();
            let lifecycle = wait_lifecycle_exit(&marker, Duration::from_secs(10));
            let lifecycle: serde_json::Value = serde_json::from_str(&lifecycle).unwrap();
            assert_eq!(lifecycle["phase"].as_str(), Some("exited"));
            assert_eq!(lifecycle["ended"].as_str(), Some("exited"));
            assert_eq!(lifecycle["code"].as_u64(), Some(0));
        }

        static CONTROL_BREAKS: AtomicU8 = AtomicU8::new(0);

        unsafe extern "system" fn control_probe_handler(kind: u32) -> i32 {
            if kind == CTRL_BREAK_EVENT {
                CONTROL_BREAKS.fetch_add(1, Ordering::Relaxed);
                TRUE
            } else {
                FALSE
            }
        }

        unsafe extern "system" fn ignore_console_control(_: u32) -> i32 {
            TRUE
        }

        #[test]
        fn console_control_probe() {
            let Some(mode) = std::env::var_os("MOOR_CONSOLE_CONTROL_PROBE") else {
                return;
            };
            let graceful = mode.as_os_str() == "graceful";
            assert!(graceful || mode.as_os_str() == "ignore");
            CONTROL_BREAKS.store(0, Ordering::Relaxed);
            assert!(unsafe { SetConsoleCtrlHandler(Some(control_probe_handler), TRUE) } != 0);
            println!("MOOR-CONTROL-READY");
            io::stdout().flush().unwrap();
            while !graceful || CONTROL_BREAKS.load(Ordering::Relaxed) == 0 {
                thread::sleep(Duration::from_millis(10));
            }
        }

        #[test]
        fn console_signal_sender() {
            let Some(target) = std::env::var_os("MOOR_CONSOLE_SIGNAL_TARGET") else {
                return;
            };
            let target = target.to_string_lossy().parse::<u32>().unwrap();
            unsafe {
                FreeConsole();
                assert!(AttachConsole(target) != 0);
                assert!(SetConsoleCtrlHandler(None, TRUE) != 0);
                assert!(GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, target) != 0);
            }
            thread::sleep(Duration::from_millis(100));
        }

        #[test]
        fn console_close_sender() {
            let Some(target) = std::env::var_os("MOOR_CONSOLE_CLOSE_TARGET") else {
                return;
            };
            let ready = std::path::PathBuf::from(std::env::var_os(CLOSE_READY).unwrap());
            let release = std::path::PathBuf::from(std::env::var_os(CLOSE_RELEASE).unwrap());
            let target = target.to_string_lossy().parse::<u32>().unwrap();
            unsafe {
                FreeConsole();
                assert!(AttachConsole(target) != 0);
                assert!(SetConsoleCtrlHandler(Some(ignore_console_control), TRUE) != 0);
                let window = GetConsoleWindow();
                assert!(!window.is_null());
                std::fs::write(&ready, b"ready").unwrap();
                while !release.is_file() {
                    thread::sleep(Duration::from_millis(1));
                }
                assert!(PostMessageW(window, WM_CLOSE, 0, 0) != 0);
                assert!(FreeConsole() != 0);
            }
        }

        fn signal(target: u32) {
            let out = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "launch_paths::native_console::console_signal_sender",
                    "--nocapture",
                ])
                .env("MOOR_CONSOLE_SIGNAL_TARGET", target.to_string())
                .output()
                .unwrap();
            assert!(out.status.success(), "signal sender failed: {out:?}");
        }

        fn close_console(target: u32) -> Instant {
            let ready = std::env::temp_dir().join(format!(
                "moor-console-close-ready-{}-{target}",
                std::process::id()
            ));
            let release = companion(&ready, ".release");
            let _files = FileCleanup::new([ready.clone(), release.clone()]);
            let mut sender = Command::new(std::env::current_exe().unwrap());
            let mut sender = sender
                .args([
                    "--exact",
                    "launch_paths::native_console::console_close_sender",
                    "--nocapture",
                ])
                .env("MOOR_CONSOLE_CLOSE_TARGET", target.to_string())
                .env(CLOSE_READY, &ready)
                .env(CLOSE_RELEASE, &release)
                .creation_flags(CREATE_NEW_CONSOLE)
                .spawn()
                .unwrap();
            wait_file(&ready, Duration::from_secs(5));
            let started = Instant::now();
            std::fs::write(&release, b"release").unwrap();
            let status = wait_std(&mut sender, Duration::from_secs(5)).unwrap();
            assert!(
                status.success() || status.code() == Some(STATUS_CONTROL_C_EXIT),
                "close sender failed before posting WM_CLOSE: {status:?}"
            );
            started
        }

        fn wait_log(marker: &std::path::Path, needle: &[u8]) {
            let log = companion(marker, ".log");
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if Store::read_only(&log, Kind::Log, 1)
                    .is_ok_and(|(_, body)| body.windows(needle.len()).any(|part| part == needle))
                {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("child readiness was not logged at {log:?}");
        }

        fn control_case(label: &str, ignore: bool, second: bool) -> (Duration, String) {
            let session = format!("console-control-{label}-{}", std::process::id());
            let _cleanup = Cleanup(session.clone());
            let marker = invoked_root().join(&session);
            let mut child = Command::new(env!("CARGO_BIN_EXE_moor"))
                .args(["run", &session])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "launch_paths::native_console::console_control_probe",
                    "--nocapture",
                ])
                .env(
                    "MOOR_CONSOLE_CONTROL_PROBE",
                    if ignore { "ignore" } else { "graceful" },
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP)
                .spawn()
                .unwrap();
            wait_log(&marker, b"MOOR-CONTROL-READY");
            let started = Instant::now();
            signal(child.id());
            if second {
                thread::sleep(Duration::from_millis(250));
                signal(child.id());
            }
            let status = wait_std(&mut child, Duration::from_secs(10)).unwrap();
            assert!(status.code().is_some(), "holder has no exit status");
            let elapsed = started.elapsed();
            let body = String::from_utf8(
                Store::read_only(&companion(&marker, ".exit"), Kind::Exit, 1)
                    .unwrap()
                    .1,
            )
            .unwrap();
            (elapsed, body)
        }

        fn close_case(label: &str, ignore: bool) -> (Duration, String) {
            let session = format!("console-close-{label}-{}", std::process::id());
            let _cleanup = Cleanup(session.clone());
            let marker = invoked_root().join(&session);
            let mut holder = Command::new(env!("CARGO_BIN_EXE_moor"))
                .args(["run", &session])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "launch_paths::native_console::console_control_probe",
                    "--nocapture",
                ])
                .env(
                    "MOOR_CONSOLE_CONTROL_PROBE",
                    if ignore { "ignore" } else { "graceful" },
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP)
                .spawn()
                .unwrap();
            wait_log(&marker, b"MOOR-CONTROL-READY");
            let started = close_console(holder.id());
            let status = wait_std(&mut holder, Duration::from_secs(10)).unwrap();
            assert!(status.code().is_some(), "holder has no close exit status");
            let elapsed = started.elapsed();
            let body = String::from_utf8(
                Store::read_only(&companion(&marker, ".exit"), Kind::Exit, 1)
                    .unwrap()
                    .1,
            )
            .unwrap();
            (elapsed, body)
        }

        #[test]
        fn shipped_console_close_finishes_durable_graceful_retirement() {
            let (elapsed, body) = close_case("graceful", false);
            assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
            assert!(body.contains("\"phase\":\"exited\""), "{body}");
            assert!(body.contains("\"method\":\"graceful\""), "{body}");
        }

        #[test]
        fn shipped_console_close_forces_ignoring_child_with_durable_retirement() {
            let (elapsed, body) = close_case("ignore", true);
            assert!(elapsed >= Duration::from_secs(2), "{elapsed:?}");
            assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
            assert!(body.contains("\"phase\":\"exited\""), "{body}");
            assert!(body.contains("\"method\":\"forced\""), "{body}");
        }

        #[test]
        fn terminal_console_control_kinds_are_distinct_from_repeatable_signals() {
            for kind in [CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT] {
                assert_eq!(moor::windows::console_control_kind(kind), Some(true));
            }
            assert_eq!(
                moor::windows::console_control_kind(CTRL_BREAK_EVENT),
                Some(false)
            );
            assert_eq!(moor::windows::console_control_kind(u32::MAX), None);
        }

        #[test]
        fn shipped_console_control_is_graceful_then_bounded_and_immediately_escalated() {
            let (graceful, lifecycle) = control_case("graceful", false, false);
            assert!(graceful < Duration::from_secs(5), "{graceful:?}");
            assert!(lifecycle.contains("\"method\":\"graceful\""), "{lifecycle}");

            let (bounded, lifecycle) = control_case("bounded", true, false);
            assert!(bounded >= Duration::from_secs(4), "{bounded:?}");
            assert!(bounded < Duration::from_secs(10), "{bounded:?}");
            assert!(lifecycle.contains("\"method\":\"forced\""), "{lifecycle}");

            let (immediate, lifecycle) = control_case("immediate", true, true);
            assert!(immediate < Duration::from_secs(5), "{immediate:?}");
            assert!(lifecycle.contains("\"method\":\"forced\""), "{lifecycle}");
        }
    }
}
