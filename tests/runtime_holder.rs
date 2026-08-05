#![cfg(unix)]

use moor::events::{Cursor as EventCursor, EventStream, canonical_header};
use moor::runtime::holder::{CoreConfig, HolderConfig, Native, NativeExit, Runtime};
use moor::runtime::io::Duplex;
use moor::runtime::private::{lifecycle_running, monotonic};
use moor::runtime::storage::{EventConfig, SessionStorage};
use moor::session::{LeaseOperation, LeaseRequest, LeaseResult, LeaseRole, ResultOutcome};
use moor::store::{Kind, Store};
use moor::wire::{self, Codec, Message, Profile, Query, decode_terminate_result};
use std::collections::VecDeque;
use std::fs;
use std::io::{Cursor, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn duplex<R: Read + Send + 'static, W: Write + Send + 'static>(
    reader: R,
    writer: W,
    limit: usize,
) -> Duplex {
    Duplex::closing(reader, writer, limit, || {})
}

struct FakeNative;

impl Native for FakeNative {
    fn resize(&mut self, _: u16, _: u16) -> Result<(), String> {
        Ok(())
    }
    fn terminate(&mut self, force: bool) -> (u8, bool) {
        (if force { 2 } else { 1 }, false)
    }
    fn exited(&mut self) -> Result<Option<NativeExit>, String> {
        Ok(Some(NativeExit::Code(9)))
    }
}

struct Peer {
    stream: UnixStream,
    codec: Codec,
    queued: VecDeque<Message>,
}

impl Peer {
    fn send(&mut self, scope: u32, kind: u8, payload: &[u8]) {
        let mut bytes = Vec::new();
        self.codec.encode(scope, kind, payload, &mut bytes).unwrap();
        self.stream.write_all(&bytes).unwrap();
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
    }

    fn recv<N: Native>(&mut self, runtime: &mut Runtime<N>) -> Message {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            runtime.poll();
            if let Some(message) = self.queued.pop_front() {
                return message;
            }
            let mut bytes = [0; 8192];
            match self.stream.read(&mut bytes) {
                Ok(0) => panic!("runtime closed the compiled-wire peer"),
                Ok(count) => {
                    let mut messages = Vec::new();
                    self.codec
                        .feed(monotonic(), &bytes[..count], &mut messages)
                        .unwrap();
                    self.queued.extend(messages);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => panic!("read runtime peer: {error}"),
            }
            assert!(Instant::now() < deadline, "runtime reply timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn recv_kind<N: Native>(&mut self, runtime: &mut Runtime<N>, kind: u8) -> Message {
        loop {
            let message = self.recv(runtime);
            if message.kind == kind {
                return message;
            }
        }
    }

    fn try_recv<N: Native>(&mut self, runtime: &mut Runtime<N>) -> Option<Message> {
        runtime.poll();
        if let Some(message) = self.queued.pop_front() {
            return Some(message);
        }
        let mut bytes = [0; 8192];
        let count = self.stream.read(&mut bytes).ok()?;
        let mut messages = Vec::new();
        self.codec
            .feed(monotonic(), &bytes[..count], &mut messages)
            .unwrap();
        self.queued.extend(messages);
        self.queued.pop_front()
    }

    fn closed<N: Native>(&mut self, runtime: &mut Runtime<N>) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            runtime.poll();
            let mut byte = [0];
            match self.stream.read(&mut byte) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(_) => return true,
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn fixture() -> (Runtime<FakeNative>, PathBuf) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("moor-holder-wire-{}-{nonce}", std::process::id()));
    let running = lifecycle_running(
        b"\x01/session",
        (Some(7), 7),
        [1; 16],
        (1, 1, [2; 16]),
        ("posix-bytes", None, None),
    );
    let lifecycle = Store::create(&root, Kind::Exit, 7, running.as_bytes(), 0, 0).unwrap();
    let storage = SessionStorage::new(None, None, lifecycle, 8, 1 << 20);
    let (_, writes) = mpsc::channel();
    let runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty: duplex(Cursor::new(Vec::new()), std::io::sink(), 1024),
        writes,
        storage,
        status: Vec::new(),
        synthetic: 0,
        native: FakeNative,
    });
    (runtime, root)
}

fn event_fixture() -> (Runtime<FakeNative>, [PathBuf; 2]) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base =
        std::env::temp_dir().join(format!("moor-holder-event-{}-{nonce}", std::process::id()));
    let event_path = base.with_extension("events");
    let exit_path = base.with_extension("exit");
    let header = canonical_header(1, "AS9zZXNzaW9u", Some(7), EventCursor(0, 0, 0, 1));
    let events = Store::create(&event_path, Kind::Event, 7, header.as_bytes(), 0, 0).unwrap();
    let running = lifecycle_running(
        b"\x01/session",
        (Some(7), 7),
        [1; 16],
        (1, 1, [2; 16]),
        ("posix-bytes", None, None),
    );
    let lifecycle = Store::create(&exit_path, Kind::Exit, 7, running.as_bytes(), 0, 0).unwrap();
    let (_, writes) = mpsc::channel();
    let runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [5; 16],
            replay_limit: 1024,
        },
        pty: duplex(Cursor::new(Vec::new()), std::io::sink(), 1024),
        writes,
        storage: SessionStorage::new(
            None,
            Some(EventConfig {
                store: events,
                stream: EventStream::new(),
                created: 1,
                session: "AS9zZXNzaW9u".into(),
                generation: Some(7),
            }),
            lifecycle,
            1,
            1,
        ),
        status: Vec::new(),
        synthetic: 0,
        native: FakeNative,
    });
    (runtime, [event_path, exit_path])
}

fn connect(runtime: &mut Runtime<FakeNative>) -> Peer {
    connect_as(runtime, Profile::Controller)
}

fn connect_as<N: Native>(runtime: &mut Runtime<N>, profile: Profile) -> Peer {
    let (client, server) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let write = server.try_clone().unwrap();
    let close = server.try_clone().unwrap();
    runtime.accept(
        Duplex::closing(server, write, 1 << 20, move || {
            let _ = close.shutdown(std::net::Shutdown::Both);
        }),
        true,
    );
    Peer {
        stream: client,
        codec: Codec::new(profile),
        queued: VecDeque::new(),
    }
}

fn hello<N: Native>(peer: &mut Peer, runtime: &mut Runtime<N>) {
    peer.send(0, 1, &wire::controller_hello(b"session").unwrap());
    assert_eq!(peer.recv(runtime).kind, 2);
}

#[test]
fn compiled_wire_keepalive_then_release_consumes_each_field_once() {
    let (mut runtime, root) = fixture();
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    let request = LeaseRequest {
        operation: LeaseOperation::Fresh,
        role: LeaseRole::InputOnly,
        epoch: 0,
        incarnation: [0; 16],
        token: [0; 16],
    };
    peer.send(7, 0x15, &request.encode_wire().unwrap());
    let granted = LeaseResult::decode_wire(&peer.recv(&mut runtime).payload).unwrap();
    assert_eq!(granted.outcome, ResultOutcome::Granted);
    let payload = [
        granted.epoch.to_le_bytes().as_slice(),
        granted.token.as_slice(),
    ]
    .concat();
    peer.send(7, 0x18, &payload);
    for _ in 0..5 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    peer.send(7, 0x17, &payload);
    let released = LeaseResult::decode_wire(&peer.recv(&mut runtime).payload).unwrap();
    assert_eq!(released.outcome, ResultOutcome::Released);
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn maximum_valid_controller_input_crosses_the_pty_queue_without_local_refusal() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "moor-holder-large-input-{}-{nonce}",
        std::process::id()
    ));
    let running = lifecycle_running(
        b"\x01/session",
        (Some(7), 7),
        [1; 16],
        (1, 1, [2; 16]),
        ("posix-bytes", None, None),
    );
    let lifecycle = Store::create(&root, Kind::Exit, 7, running.as_bytes(), 0, 0).unwrap();
    let (pty, child) = UnixStream::pair().unwrap();
    let (pty, writes) = Duplex::tracked(pty.try_clone().unwrap(), std::io::sink(), 1 << 20);
    let mut runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty,
        writes,
        storage: SessionStorage::new(None, None, lifecycle, 8, 1 << 20),
        status: Vec::new(),
        synthetic: 0,
        native: FakeNative,
    });
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    let request = LeaseRequest {
        operation: LeaseOperation::Fresh,
        role: LeaseRole::InputOnly,
        epoch: 0,
        incarnation: [0; 16],
        token: [0; 16],
    };
    peer.send(7, 0x15, &request.encode_wire().unwrap());
    let lease = LeaseResult::decode_wire(&peer.recv_kind(&mut runtime, 0x16).payload).unwrap();
    let terminal = vec![b'x'; (16 << 20) - 13];
    let payload = [
        lease.epoch.to_le_bytes().as_slice(),
        &1u64.to_le_bytes(),
        &[0],
        &terminal,
    ]
    .concat();
    let sender = std::thread::spawn(move || {
        peer.stream.set_nonblocking(false).unwrap();
        peer.send(7, 9, &payload);
        peer.stream.set_nonblocking(true).unwrap();
        peer
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !sender.is_finished() && Instant::now() < deadline {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        sender.is_finished(),
        "maximum input did not reach the holder"
    );
    let mut peer = sender.join().unwrap();
    let receipt = wire::InputReceipt::decode(&peer.recv_kind(&mut runtime, 10).payload).unwrap();
    assert_eq!(
        (receipt.status, receipt.result, receipt.written),
        (0, 0, terminal.len() as u64)
    );
    drop((child, runtime));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_keepalive_reports_lease_not_held_before_closing() {
    let (mut runtime, root) = fixture();
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    let request = LeaseRequest {
        operation: LeaseOperation::Fresh,
        role: LeaseRole::InputOnly,
        epoch: 0,
        incarnation: [0; 16],
        token: [0; 16],
    };
    peer.send(7, 0x15, &request.encode_wire().unwrap());
    let granted = LeaseResult::decode_wire(&peer.recv(&mut runtime).payload).unwrap();
    peer.send(
        7,
        0x18,
        &[
            granted.epoch.wrapping_add(1).to_le_bytes().as_slice(),
            granted.token.as_slice(),
        ]
        .concat(),
    );
    let error = peer.recv_kind(&mut runtime, 13);
    assert_eq!(
        u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
        15
    );
    assert_eq!(
        wire::get_compact(&error.payload, 2, true),
        Some(b"lease not held".as_slice())
    );
    assert!(peer.closed(&mut runtime));
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unattached_geometry_without_a_lease_request_is_malformed() {
    let (mut runtime, root) = fixture();
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    peer.send(7, 3, &[80, 0, 24, 0, 0]);
    let error = peer.recv_kind(&mut runtime, 13);
    assert_eq!(
        u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
        5
    );
    assert!(peer.closed(&mut runtime));
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn geometry_bounds_are_enforced_at_both_ingresses_with_distinct_frozen_codes() {
    // The largest geometry inside both frozen bounds: 2000x1000 is exactly the
    // 2,000,000-cell cap, and each dimension is within 1..=32767.
    let (mut runtime, root) = fixture();
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    peer.send(
        7,
        3,
        &[
            2000u16.to_le_bytes().as_slice(),
            &1000u16.to_le_bytes(),
            &[1],
        ]
        .concat(),
    );
    peer.recv_kind(&mut runtime, 5);
    peer.recv_kind(&mut runtime, 4);
    let lease = LeaseResult::decode_wire(&peer.recv_kind(&mut runtime, 0x16).payload).unwrap();
    peer.send(
        7,
        11,
        &[
            lease.epoch.to_le_bytes().as_slice(),
            &2000u16.to_le_bytes(),
            &1000u16.to_le_bytes(),
        ]
        .concat(),
    );
    peer.send(7, 13, &[]);
    peer.recv_kind(&mut runtime, 14);
    drop(runtime);
    fs::remove_dir_all(root).unwrap();

    // Every out-of-range shape is refused, each with its own frozen code:
    // half-specified is 14, out-of-range is 5. 32768 is one past the signed
    // 16-bit console ceiling; 2001x1000 is one cell past the product cap; and
    // 32767x32767 is the case a u16 product computation would silently wrap.
    for (columns, rows, code, at_attach) in [
        (80u16, 0u16, 14u16, true),
        (0, 24, 14, true),
        (32_768, 1, 5, true),
        (1, 32_768, 5, true),
        (2001, 1000, 5, true),
        (32_767, 32_767, 5, true),
        (32_768, 1, 5, false),
        (2001, 1000, 5, false),
    ] {
        let (mut runtime, root) = fixture();
        let mut peer = connect(&mut runtime);
        hello(&mut peer, &mut runtime);
        if at_attach {
            peer.send(
                7,
                3,
                &[columns.to_le_bytes().as_slice(), &rows.to_le_bytes(), &[1]].concat(),
            );
        } else {
            peer.send(
                7,
                3,
                &[0u16.to_le_bytes().as_slice(), &0u16.to_le_bytes(), &[1]].concat(),
            );
            peer.recv_kind(&mut runtime, 5);
            peer.recv_kind(&mut runtime, 4);
            let lease =
                LeaseResult::decode_wire(&peer.recv_kind(&mut runtime, 0x16).payload).unwrap();
            peer.send(
                7,
                11,
                &[
                    lease.epoch.to_le_bytes().as_slice(),
                    &columns.to_le_bytes(),
                    &rows.to_le_bytes(),
                ]
                .concat(),
            );
        }
        let error = peer.recv_kind(&mut runtime, 13);
        assert_eq!(
            u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
            code,
            "{columns}x{rows} at_attach={at_attach}"
        );
        assert!(peer.closed(&mut runtime));
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn hello_mismatches_and_trailing_codec_faults_are_encoded_before_close() {
    for (scope, identity, expected) in [(6, b"session".as_slice(), 9), (0, b"other".as_slice(), 10)]
    {
        let (mut runtime, root) = fixture();
        let mut peer = connect(&mut runtime);
        peer.send(scope, 1, &wire::controller_hello(identity).unwrap());
        let error = peer.recv_kind(&mut runtime, 13);
        assert_eq!(
            u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
            expected
        );
        assert!(peer.closed(&mut runtime));
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }

    let (mut runtime, root) = fixture();
    let mut peer = connect(&mut runtime);
    let mut sender = Codec::new(Profile::Controller);
    let mut bytes = Vec::new();
    sender
        .encode(
            0,
            1,
            &wire::controller_hello(b"session").unwrap(),
            &mut bytes,
        )
        .unwrap();
    let bad = bytes.len();
    sender
        .encode(
            0,
            1,
            &wire::controller_hello(b"session").unwrap(),
            &mut bytes,
        )
        .unwrap();
    bytes[bad + 5] = 0xff;
    let checksum = wire::crc32c(&bytes[bad..bad + 20]);
    bytes[bad + 20..bad + 24].copy_from_slice(&checksum.to_le_bytes());
    peer.raw(&bytes);
    assert_eq!(
        peer.recv(&mut runtime).kind,
        2,
        "valid coalesced prefix was discarded"
    );
    let error = peer.recv_kind(&mut runtime, 13);
    assert_eq!(
        u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
        2
    );
    assert!(peer.closed(&mut runtime));
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_query_reply_does_not_refresh_the_lease() {
    let (mut runtime, root) = fixture();
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    peer.send(7, 3, &[0, 0, 0, 0, 1]);
    peer.recv_kind(&mut runtime, 5);
    peer.recv_kind(&mut runtime, 4);
    let lease = LeaseResult::decode_wire(&peer.recv_kind(&mut runtime, 0x16).payload).unwrap();
    let granted = monotonic();
    runtime.output(b"\x1b[?2004$p".to_vec());
    let delegated = wire::decode_query(&peer.recv_kind(&mut runtime, 0x14).payload).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let wrong = Query {
        class: 1,
        bytes: b"\x1b[?1c".to_vec(),
        ..delegated
    };
    peer.send(7, 12, &wrong.encode().unwrap());
    for _ in 0..20 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(1));
    }
    runtime.tick(granted.saturating_add(10_001));
    peer.send(7, 13, &[]);
    let status = peer.recv_kind(&mut runtime, 14);
    assert_eq!(
        status.payload[32] & 0x10,
        0,
        "class-mismatched reply extended ownership"
    );
    assert_eq!(
        u32::from_le_bytes(status.payload[33..37].try_into().unwrap()),
        lease.epoch
    );
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn split_queries_are_delegated_before_exact_raw_release_and_quiet_candidates_expire() {
    let query = b"\x1b[?2004$p";
    for split in 0..=query.len() {
        let (mut runtime, root) = fixture();
        let mut peer = connect(&mut runtime);
        hello(&mut peer, &mut runtime);
        peer.send(7, 3, &[0, 0, 0, 0, 1]);
        peer.recv_kind(&mut runtime, 5);
        peer.recv_kind(&mut runtime, 4);
        peer.recv_kind(&mut runtime, 0x16);
        while peer.try_recv(&mut runtime).is_some() {}
        runtime.output(query[..split].to_vec());
        if split < query.len() {
            assert!(
                peer.try_recv(&mut runtime).is_none(),
                "query prefix leaked at split {split}"
            );
            runtime.output(query[split..].to_vec());
        }
        assert_eq!(
            peer.recv(&mut runtime).kind,
            0x14,
            "QUERY was not first at split {split}"
        );
        let output = peer.recv_kind(&mut runtime, 6);
        assert_eq!(&output.payload[16..], query);
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }

    let (mut runtime, root) = fixture();
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    peer.send(7, 3, &[0; 5]);
    peer.recv_kind(&mut runtime, 5);
    peer.recv_kind(&mut runtime, 4);
    while peer.try_recv(&mut runtime).is_some() {}
    runtime.output(b"\x1b[".to_vec());
    assert!(peer.try_recv(&mut runtime).is_none());
    runtime.tick(monotonic().saturating_add(50));
    let output = peer.recv_kind(&mut runtime, 6);
    assert_eq!(&output.payload[16..], b"\x1b[");
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn c1_queries_receive_matching_c1_synthetic_replies_for_all_identity_classes() {
    use std::sync::{Arc, Mutex};
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("moor-holder-c1-{}-{nonce}", std::process::id()));
    let running = lifecycle_running(
        b"\x01/session",
        (Some(7), 7),
        [1; 16],
        (1, 1, [2; 16]),
        ("posix-bytes", None, None),
    );
    let lifecycle = Store::create(&root, Kind::Exit, 7, running.as_bytes(), 0, 0).unwrap();
    let (reader, child) = UnixStream::pair().unwrap();
    let capture = Arc::new(Mutex::new(Vec::new()));
    let (_, writes) = mpsc::channel();
    let mut runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty: duplex(reader, Capture(capture.clone()), 1024),
        writes,
        storage: SessionStorage::new(None, None, lifecycle, 8, 1 << 20),
        status: Vec::new(),
        synthetic: 3,
        native: FakeNative,
    });
    for query in [b"\x9bc".as_slice(), b"\x9b>c", b"\x9b>0q"] {
        runtime.output(query.to_vec());
    }
    let expected = [
        b"\x9b?62;4c".as_slice(),
        b"\x9b>1;47;0c",
        b"\x90>|kitty(0.47.0)\x9c",
    ]
    .concat();
    let deadline = Instant::now() + Duration::from_secs(2);
    while capture.lock().unwrap().len() < expected.len() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(*capture.lock().unwrap(), expected);
    drop((child, runtime));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn event_lane_failure_reports_resource_exhausted_before_closing_semantic_peer() {
    let (mut runtime, paths) = event_fixture();
    let mut peer = connect_as(&mut runtime, Profile::Semantic);
    let mut hello = [[5; 16].as_slice(), &[6; 16], &7u32.to_le_bytes(), &[0, 0]].concat();
    wire::put_compact(&mut hello, b"source").unwrap();
    peer.send(0, 1, &hello);
    assert_eq!(peer.recv(&mut runtime).kind, 2);
    runtime.output(b"\x1b]2;idle\x07\x1b]8;;https://example.test\x07".to_vec());
    let error = peer.recv_kind(&mut runtime, 9);
    assert_eq!(
        u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
        12
    );
    assert!(peer.closed(&mut runtime));
    drop(runtime);
    for path in paths {
        fs::remove_dir_all(path).unwrap();
    }
}

#[test]
fn parsed_semantic_refusals_and_durable_failures_use_refused_ack_shape() {
    let check = |message: Message, id: [u8; 16], sequence: u64, code: u16| {
        assert_eq!(message.kind, 7);
        assert_eq!(&message.payload[..16], &id);
        assert_eq!(
            u64::from_le_bytes(message.payload[16..24].try_into().unwrap()),
            sequence
        );
        assert_eq!(message.payload[24], 2);
        assert_eq!(
            u16::from_le_bytes(message.payload[25..27].try_into().unwrap()),
            code
        );
        assert_eq!(&message.payload[27..39], &[0; 12]);
        assert!(
            !wire::get_compact(&message.payload, 39, true)
                .unwrap()
                .is_empty()
        );
    };
    let connect_source = |runtime: &mut Runtime<FakeNative>| {
        let mut peer = connect_as(runtime, Profile::Semantic);
        let mut hello = [[5; 16].as_slice(), &[6; 16], &7u32.to_le_bytes(), &[0, 1]].concat();
        wire::put_compact(&mut hello, b"source").unwrap();
        peer.send(0, 1, &hello);
        assert_eq!(peer.recv(runtime).kind, 2);
        peer
    };

    let (mut runtime, paths) = event_fixture();
    let mut peer = connect_source(&mut runtime);
    let id = [7; 16];
    peer.send(
        1,
        3,
        &[id.as_slice(), &2u64.to_le_bytes(), &[0], b"{}"].concat(),
    );
    check(peer.recv(&mut runtime), id, 2, 7);
    drop(runtime);
    for path in paths {
        fs::remove_dir_all(path).unwrap();
    }

    let (mut runtime, paths) = event_fixture();
    let mut peer = connect_source(&mut runtime);
    let id = [8; 16];
    peer.send(
        1,
        3,
        &[id.as_slice(), &1u64.to_le_bytes(), &[0], b"{}"].concat(),
    );
    check(peer.recv_kind(&mut runtime, 7), id, 1, 12);
    drop(runtime);
    for path in paths {
        fs::remove_dir_all(path).unwrap();
    }
}

#[test]
fn silent_handshakes_expire_before_the_admission_limit_is_checked() {
    let (mut runtime, root) = fixture();
    let silent = (0..16).map(|_| connect(&mut runtime)).collect::<Vec<_>>();
    runtime.tick(monotonic().saturating_add(5_001));
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    drop((silent, runtime));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dialect_only_peers_still_consume_the_sixteen_initial_hello_slots() {
    let (mut runtime, root) = fixture();
    let mut stalled = (0..16).map(|_| connect(&mut runtime)).collect::<Vec<_>>();
    for peer in &mut stalled {
        peer.raw(b"MOOR");
    }
    for _ in 0..20 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut excess = connect(&mut runtime);
    assert!(excess.closed(&mut runtime));
    drop((stalled, runtime));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn semantic_data_before_hello_is_malformed_not_a_stale_token() {
    let (mut runtime, root) = fixture();
    let mut peer = connect_as(&mut runtime, Profile::Semantic);
    peer.send(1, 3, b"premature");
    let error = peer.recv_kind(&mut runtime, 9);
    assert_eq!(
        u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
        3
    );
    assert!(peer.closed(&mut runtime));
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn authenticated_semantic_source_epoch_is_fenced_before_admission() {
    let (mut runtime, paths) = event_fixture();
    let mut peer = connect_as(&mut runtime, Profile::Semantic);
    let mut hello = [[5; 16].as_slice(), &[6; 16], &7u32.to_le_bytes(), &[0, 1]].concat();
    wire::put_compact(&mut hello, b"source").unwrap();
    peer.send(0, 1, &hello);
    assert_eq!(peer.recv(&mut runtime).kind, 2);
    peer.send(
        2,
        3,
        &[[7; 16].as_slice(), &1u64.to_le_bytes(), &[0], b"{}"].concat(),
    );
    let error = peer.recv_kind(&mut runtime, 9);
    assert_eq!(
        u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
        5
    );
    assert!(peer.closed(&mut runtime));
    drop(runtime);
    for path in paths {
        fs::remove_dir_all(path).unwrap();
    }
}

#[test]
fn expired_viewer_ownership_is_removed_from_status_queries_and_resize() {
    let (mut runtime, root) = fixture();
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    peer.send(7, 3, &[0, 0, 0, 0, 1]);
    peer.recv_kind(&mut runtime, 5);
    peer.recv_kind(&mut runtime, 4);
    let granted = LeaseResult::decode_wire(&peer.recv_kind(&mut runtime, 0x16).payload).unwrap();
    runtime.tick(monotonic().saturating_add(10_001));
    peer.send(7, 13, &[]);
    let status = peer.recv_kind(&mut runtime, 14);
    assert_eq!(
        status.payload[32] & 0x30,
        0x20,
        "observer remains attached without ownership"
    );
    peer.queued.clear();
    runtime.output(b"\x1b[?2004$p".to_vec());
    loop {
        match peer.recv(&mut runtime).kind {
            6 => break,
            0x14 => panic!("query delegated after lease expiry"),
            _ => {}
        }
    }
    peer.send(
        7,
        11,
        &[
            granted.epoch.to_le_bytes().as_slice(),
            80u16.to_le_bytes().as_slice(),
            24u16.to_le_bytes().as_slice(),
        ]
        .concat(),
    );
    assert!(peer.closed(&mut runtime));
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn non_vt_attach_omits_terminal_bytes_without_erasing_tracked_exactness() {
    let (mut runtime, root) = fixture();
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    peer.send(7, 3, &[0, 0, 0, 0, 3]);
    assert_eq!(peer.recv_kind(&mut runtime, 5).payload.as_ref(), [0, 0]);
    let status = peer.recv_kind(&mut runtime, 4);
    assert_eq!(status.payload[32] & 2, 2);
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn termination_refuses_identity_waits_for_retirement_and_times_out_distinctly() {
    let (mut runtime, root) = fixture();
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    let request = |identity: &[u8], force| {
        let mut payload = Vec::new();
        wire::put_wide(&mut payload, identity).unwrap();
        payload.extend([7u32.to_le_bytes().as_slice(), &[1; 16], &[u8::from(force)]].concat());
        payload
    };
    peer.send(7, 15, &request(b"wrong", false));
    let refused = peer.recv_kind(&mut runtime, 16);
    assert_eq!(decode_terminate_result(&refused.payload).unwrap().0, 2);
    peer.send(7, 15, &request(b"session", false));
    for _ in 0..10 {
        assert!(
            peer.try_recv(&mut runtime).is_none(),
            "termination completed before child exit and unlink"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    runtime.tick(monotonic().saturating_add(10_001));
    let timed_out = peer.recv_kind(&mut runtime, 16);
    let (outcome, containment, method, _) = decode_terminate_result(&timed_out.payload).unwrap();
    assert_eq!((outcome, containment, method), (3, 7, 2));
    drop(runtime);

    let (mut runtime, second_root) = fixture();
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    peer.send(7, 15, &request(b"session", true));
    for _ in 0..10 {
        assert!(peer.try_recv(&mut runtime).is_none());
        std::thread::sleep(Duration::from_millis(2));
    }
    runtime.retired(true, false);
    let completed = peer.recv_kind(&mut runtime, 16);
    let result = decode_terminate_result(&completed.payload).unwrap();
    assert_eq!((result.0, result.1, result.2), (0, 2, 2));
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(second_root).unwrap();
}

#[test]
fn termination_deadline_abandons_drive_before_observing_a_late_exit() {
    let (mut runtime, root) = fixture();
    runtime.shutdown_requested(0, false);
    runtime.tick(10_001);
    let result = runtime.drive(|| None, || None).unwrap();
    assert_eq!(result, None);
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn child_exit_waits_for_delayed_pty_eof_and_final_bytes() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Delayed(Option<Vec<u8>>, Arc<AtomicBool>);
    impl Read for Delayed {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let Some(value) = self.0.take() else {
                return Ok(0);
            };
            while !self.1.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(2));
            }
            bytes[..value.len()].copy_from_slice(&value);
            Ok(value.len())
        }
    }
    struct ExitNative(Arc<AtomicBool>);
    impl Native for ExitNative {
        fn resize(&mut self, _: u16, _: u16) -> Result<(), String> {
            Ok(())
        }
        fn terminate(&mut self, force: bool) -> (u8, bool) {
            (if force { 2 } else { 1 }, false)
        }
        fn exited(&mut self) -> Result<Option<NativeExit>, String> {
            self.0.store(true, Ordering::Release);
            Ok(Some(NativeExit::Code(9)))
        }
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("moor-holder-drain-{}-{nonce}", std::process::id()));
    let running = lifecycle_running(
        b"\x01/session",
        (Some(7), 7),
        [1; 16],
        (1, 1, [2; 16]),
        ("posix-bytes", None, None),
    );
    let lifecycle = Store::create(&root, Kind::Exit, 7, running.as_bytes(), 0, 0).unwrap();
    let (_, writes) = mpsc::channel();
    let exited = Arc::new(AtomicBool::new(false));
    let mut runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty: duplex(
            Delayed(Some(b"final bytes".to_vec()), exited.clone()),
            std::io::sink(),
            1024,
        ),
        writes,
        storage: SessionStorage::new(None, None, lifecycle, 8, 1 << 20),
        status: Vec::new(),
        synthetic: 0,
        native: ExitNative(exited),
    });
    let mut peer = connect_as(&mut runtime, Profile::Controller);
    hello(&mut peer, &mut runtime);
    peer.send(7, 3, &[0; 5]);
    peer.recv_kind(&mut runtime, 5);
    peer.recv_kind(&mut runtime, 4);
    while peer.try_recv(&mut runtime).is_some() {}
    assert_eq!(
        runtime.drive(|| None, || None).unwrap(),
        Some(NativeExit::Code(9))
    );
    assert_eq!(runtime.output_end(), 11);
    let mut stopped = false;
    loop {
        let message = peer.recv(&mut runtime);
        if message.kind == 0x12 {
            stopped |= wire::Heartbeat::decode(&message.payload).unwrap().flags & 1 == 0;
        } else if message.kind == 6 {
            assert!(
                stopped,
                "final output was forwarded before child-running became false"
            );
            assert_eq!(&message.payload[16..], b"final bytes");
            break;
        }
    }
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn retirement_waits_for_the_termination_result_to_flush() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct DelayedWriter {
        stream: UnixStream,
        delay: Arc<AtomicBool>,
        finished: Arc<AtomicBool>,
    }
    impl Write for DelayedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.delay.swap(false, Ordering::AcqRel) {
                std::thread::sleep(Duration::from_millis(120));
                self.finished.store(true, Ordering::Release);
            }
            self.stream.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.stream.flush()
        }
    }

    let (mut runtime, root) = fixture();
    let (client, server) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let close = server.try_clone().unwrap();
    let delay = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    runtime.accept(
        Duplex::closing(
            server.try_clone().unwrap(),
            DelayedWriter {
                stream: server,
                delay: delay.clone(),
                finished: finished.clone(),
            },
            1 << 20,
            move || {
                let _ = close.shutdown(std::net::Shutdown::Both);
            },
        ),
        true,
    );
    let mut peer = Peer {
        stream: client,
        codec: Codec::new(Profile::Controller),
        queued: VecDeque::new(),
    };
    hello(&mut peer, &mut runtime);
    let mut request = Vec::new();
    wire::put_wide(&mut request, b"session").unwrap();
    request.extend([7u32.to_le_bytes().as_slice(), &[1; 16], &[1]].concat());
    peer.send(7, 15, &request);
    for _ in 0..5 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    delay.store(true, Ordering::Release);
    runtime.retired(true, false);
    assert!(
        finished.load(Ordering::Acquire),
        "retired before the accepted result write completed"
    );
    assert_eq!(
        decode_terminate_result(&peer.recv_kind(&mut runtime, 16).payload)
            .unwrap()
            .0,
        0
    );
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}
