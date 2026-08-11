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

#[cfg(windows)]
mod launch_paths {
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};

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

    fn invoked_root() -> PathBuf {
        let output = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let sid = String::from_utf8(output.stdout).unwrap();
        let mut leaf = OsString::from(".");
        leaf.push(Path::new(env!("CARGO_BIN_EXE_moor")).file_name().unwrap());
        leaf.push("-");
        leaf.push(sid.trim());
        std::env::temp_dir().join(leaf)
    }

    const PUBLICATION_RELEASE: &str = "MOOR_TEST_PUBLICATION_RELEASE";

    #[test]
    fn publication_waiter() {
        let Some(release) = std::env::var_os(PUBLICATION_RELEASE) else {
            return;
        };
        while !Path::new(&release).exists() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
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
        let root = invoked_root();
        let _ = moor(&["list"]);
        let missing = root.join(format!("missing-child-{}", std::process::id()));
        let system = Command::new(&missing).spawn().unwrap_err().to_string();
        let program = moor::name::program(Path::new(env!("CARGO_BIN_EXE_moor")).as_os_str());
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
            let out = moor(&[
                mode,
                &session,
                "-T",
                event.to_str().unwrap(),
                "-S",
                env!("CARGO_BIN_EXE_moor"),
                missing.to_str().unwrap(),
            ]);
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
        let out = Command::new(env!("CARGO_BIN_EXE_moor"))
            .env("DESK_MOOR_LAUNCH_CHANNEL", "not-a-handle")
            .env(&generation, "2")
            .env("DESK_SESSION_GENERATION", "2")
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
        let out = moor(&[
            "start",
            &session,
            "-T",
            event.to_str().unwrap(),
            "powershell.exe",
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 10",
        ]);
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
        use windows_sys::Win32::Foundation::{FALSE, HANDLE, TRUE};
        use windows_sys::Win32::System::Console::{
            AttachConsole, CONSOLE_SCREEN_BUFFER_INFO, COORD, CTRL_BREAK_EVENT, ClosePseudoConsole,
            CreatePseudoConsole, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT,
            ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE,
            ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
            FlushConsoleInputBuffer, FreeConsole, GenerateConsoleCtrlEvent, GetConsoleMode,
            GetConsoleScreenBufferInfo, GetStdHandle, HPCON, INPUT_RECORD, KEY_EVENT,
            ReadConsoleInputW, ResizePseudoConsole, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            SetConsoleCtrlHandler, SetConsoleMode, WINDOW_BUFFER_SIZE_EVENT,
        };
        use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
        use windows_sys::Win32::System::Threading::{CREATE_NEW_CONSOLE, CREATE_NEW_PROCESS_GROUP};

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
                        0,
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

            fn wait_for(&mut self, marker: &[u8], timeout: Duration) -> io::Result<()> {
                let deadline = Instant::now() + timeout;
                while Instant::now() < deadline {
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
                        if self
                            .received
                            .windows(marker.len())
                            .any(|window| window == marker)
                        {
                            return Ok(());
                        }
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
                    if key.bKeyDown != 0 && character != 0 {
                        println!(
                            "MOOR-KEY:{}:{resized}",
                            char::from_u32(character as u32).unwrap()
                        );
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
            assert_eq!(
                input & (ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS),
                ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS
            );
            assert_eq!(
                input
                    & (ENABLE_LINE_INPUT
                        | ENABLE_ECHO_INPUT
                        | ENABLE_PROCESSED_INPUT
                        | ENABLE_QUICK_EDIT_MODE
                        | ENABLE_WINDOW_INPUT),
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
            console.write(&[0x1c]).unwrap();
            assert!(
                wait_spawn(&mut viewer, Duration::from_secs(5))
                    .unwrap()
                    .success()
            );

            let mut after = probe(&console, "after").unwrap();
            assert!(
                wait_spawn(&mut after, Duration::from_secs(5))
                    .unwrap()
                    .success()
            );
            assert_eq!(wait_modes(&mut console, "after"), before_modes);
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
