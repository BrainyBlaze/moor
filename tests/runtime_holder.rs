#![cfg(unix)]

use moor::events::{Cursor as EventCursor, EventStream, canonical_header};
use moor::runtime::holder::{CoreConfig, HolderConfig, Native, NativeExit, Runtime};
use moor::runtime::io::Duplex;
use moor::runtime::private::{ArtifactConfig, holder_artifacts, lifecycle_running, monotonic};
use moor::runtime::storage::{EventConfig, SessionStorage};
use moor::session::{LeaseOperation, LeaseRequest, LeaseResult, LeaseRole, ResultOutcome};
use moor::store::{Kind, Store};
use moor::wire::{self, Codec, Message, Profile, Query, decode_terminate_result};
use std::collections::VecDeque;
use std::fs;
use std::io::{Cursor, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
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

struct AncestryNative {
    ancestor: u32,
    checks: Arc<AtomicUsize>,
    resizes: Arc<AtomicUsize>,
}

impl Native for AncestryNative {
    fn resize(&mut self, _: u16, _: u16) -> Result<(), String> {
        self.resizes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn terminate(&mut self, force: bool) -> (u8, bool) {
        (if force { 2 } else { 1 }, false)
    }
    fn exited(&mut self) -> Result<Option<NativeExit>, String> {
        Ok(None)
    }
    fn holder_ancestor(&self, pid: u32) -> bool {
        self.checks.fetch_add(1, Ordering::Relaxed);
        pid == self.ancestor
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

fn temp(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "moor-holder-{name}-{}-{time}-{sequence}",
        std::process::id()
    ))
}

fn fixture() -> (Runtime<FakeNative>, PathBuf) {
    fixture_with_native(FakeNative)
}

fn fixture_with_native<N: Native>(native: N) -> (Runtime<N>, PathBuf) {
    let root = temp("wire");
    let running = lifecycle_running(
        b"\x01/session",
        (Some(7), 7),
        [1; 16],
        (1, 1, [2; 16]),
        ("posix-bytes", None, None),
    );
    let lifecycle = Store::create(&root, Kind::Exit, 7, running.as_bytes(), 0, 0).unwrap();
    let storage = SessionStorage::new(None, None, lifecycle, 8, 1 << 20);
    let runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty: duplex(Cursor::new(Vec::new()), std::io::sink(), 1024),
        storage,
        status: Vec::new(),
        commit_at: 0,
        synthetic: 0,
        native,
    });
    (runtime, root)
}

// The fixtures above build a runtime with an EMPTY base status, so a status
// payload in these tests is the v4 geometry pair (4 bytes) followed by the
// tail — which is why tail offsets here start at 36 rather than 32.
fn event_fixture() -> (Runtime<FakeNative>, [PathBuf; 2]) {
    event_fixture_with(1, 1)
}

fn event_fixture_with(jobs: usize, bytes: usize) -> (Runtime<FakeNative>, [PathBuf; 2]) {
    let base = temp("event");
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
    let runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [5; 16],
            replay_limit: 1024,
        },
        pty: duplex(Cursor::new(Vec::new()), std::io::sink(), 1024),
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
            jobs,
            bytes,
        ),
        status: Vec::new(),
        commit_at: 0,
        synthetic: 0,
        native: FakeNative,
    });
    (runtime, [event_path, exit_path])
}

fn connect(runtime: &mut Runtime<FakeNative>) -> Peer {
    connect_as(runtime, Profile::Controller)
}

fn connect_as<N: Native>(runtime: &mut Runtime<N>, profile: Profile) -> Peer {
    connect_as_with(runtime, profile, true, None)
}

fn connect_as_with<N: Native>(
    runtime: &mut Runtime<N>,
    profile: Profile,
    same_user: bool,
    pid: Option<u32>,
) -> Peer {
    connect_as_reserved(runtime, profile, same_user, pid, false)
}

fn connect_as_reserved<N: Native>(
    runtime: &mut Runtime<N>,
    profile: Profile,
    same_user: bool,
    pid: Option<u32>,
    exhausted: bool,
) -> Peer {
    let (client, server) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let write = server.try_clone().unwrap();
    let close = server.try_clone().unwrap();
    runtime.accept(
        Duplex::closing(server, write, 1 << 20, move || {
            let _ = close.shutdown(std::net::Shutdown::Both);
        }),
        same_user,
        pid,
        exhausted,
    );
    Peer {
        stream: client,
        codec: Codec::new(profile),
        queued: VecDeque::new(),
    }
}

#[test]
fn wrong_user_gets_a_profile_specific_refusal_after_only_the_preface() {
    for (profile, preface, kind, code) in [
        (Profile::Controller, b"MOOR".as_slice(), 0x13, 11),
        (Profile::Semantic, b"MOOS".as_slice(), 9, 4),
    ] {
        let (mut runtime, root) = fixture();
        let mut peer = connect_as_with(&mut runtime, profile, false, None);
        peer.raw(&preface[..2]);
        assert!(peer.try_recv(&mut runtime).is_none());
        let mut tail = preface[2..].to_vec();
        tail.extend_from_slice(&[0xa5; 64]);
        peer.raw(&tail);
        let error = peer.recv_kind(&mut runtime, kind);
        assert_eq!(
            u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
            code
        );
        assert_eq!(
            wire::get_compact(&error.payload, 2, true),
            Some(b"unauthorised peer".as_slice())
        );
        assert!(peer.closed(&mut runtime));

        let mut trusted = connect(&mut runtime);
        hello(&mut trusted, &mut runtime);
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn concurrent_wrong_users_each_receive_their_profile_specific_refusal() {
    let (mut runtime, root) = fixture();
    let mut controller = connect_as_with(&mut runtime, Profile::Controller, false, None);
    let mut semantic = connect_as_with(&mut runtime, Profile::Semantic, false, None);
    controller.raw(b"MOOR");
    semantic.raw(b"MOOS");
    for (peer, kind, code) in [(&mut controller, 0x13, 11), (&mut semantic, 9, 4)] {
        let error = peer.recv_kind(&mut runtime, kind);
        assert_eq!(
            u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
            code
        );
        assert!(peer.closed(&mut runtime));
    }
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_holder_ancestry_refuses_attach_before_any_attach_state_change() {
    let checks = Arc::new(AtomicUsize::new(0));
    let resizes = Arc::new(AtomicUsize::new(0));
    let (mut runtime, root) = fixture_with_native(AncestryNative {
        ancestor: 41,
        checks: checks.clone(),
        resizes: resizes.clone(),
    });
    runtime.output(b"retained history".to_vec());

    let mut malformed = connect_as_with(&mut runtime, Profile::Controller, true, Some(41));
    hello(&mut malformed, &mut runtime);
    malformed.send(7, 3, &[0, 0, 0, 0, 4]);
    assert_eq!(malformed.recv_kind(&mut runtime, 0x13).kind, 0x13);
    assert_eq!(checks.load(Ordering::Relaxed), 0);

    let mut ancestral = connect_as_with(&mut runtime, Profile::Controller, true, Some(41));
    hello(&mut ancestral, &mut runtime);
    ancestral.send(7, 3, &[80, 0, 24, 0, 1]);
    let error = ancestral.recv_kind(&mut runtime, 0x13);
    assert_eq!(
        u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
        11
    );
    assert_eq!(
        wire::get_compact(&error.payload, 2, true),
        Some(b"holder is an ancestor of attaching process".as_slice())
    );
    assert!(ancestral.closed(&mut runtime));
    assert_eq!(checks.load(Ordering::Relaxed), 1);
    assert_eq!(resizes.load(Ordering::Relaxed), 0);

    let mut unrelated = connect_as_with(&mut runtime, Profile::Controller, true, Some(42));
    hello(&mut unrelated, &mut runtime);
    unrelated.send(7, 3, &[80, 0, 24, 0, 1]);
    assert_eq!(unrelated.recv(&mut runtime).kind, 4);
    assert_eq!(unrelated.recv(&mut runtime).kind, 5);
    let lease = LeaseResult::decode_wire(&unrelated.recv_kind(&mut runtime, 0x16).payload).unwrap();
    assert_eq!(lease.outcome, ResultOutcome::Granted);
    assert_eq!(unrelated.recv(&mut runtime).kind, 6);
    let resize = [
        lease.epoch.to_le_bytes().as_slice(),
        80u16.to_le_bytes().as_slice(),
        24u16.to_le_bytes().as_slice(),
    ]
    .concat();
    unrelated.send(7, 0x0b, &resize);
    unrelated.send(7, 13, &[]);
    unrelated.recv_kind(&mut runtime, 14);
    assert_eq!(checks.load(Ordering::Relaxed), 2);
    assert_eq!(resizes.load(Ordering::Relaxed), 1);
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn seventeenth_preface_gets_resource_exhausted_without_disturbing_existing_peers() {
    let (mut runtime, root) = fixture();
    let mut pending = (0..16).map(|_| connect(&mut runtime)).collect::<Vec<_>>();
    for (index, (profile, preface, kind, code)) in [
        (Profile::Controller, b"MOOR".as_slice(), 0x13, 13),
        (Profile::Semantic, b"MOOS".as_slice(), 9, 12),
    ]
    .into_iter()
    .enumerate()
    {
        let mut excess = connect_as(&mut runtime, profile);
        if index == 0 {
            let mut beyond_overflow = connect(&mut runtime);
            assert!(beyond_overflow.closed(&mut runtime));
        }
        excess.raw(preface);
        let error = excess.recv_kind(&mut runtime, kind);
        assert_eq!(
            u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
            code
        );
        assert_eq!(
            wire::get_compact(&error.payload, 2, true),
            Some(b"connection limit exhausted".as_slice())
        );
        assert!(excess.closed(&mut runtime));
    }
    hello(&mut pending[0], &mut runtime);
    drop((pending, runtime));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reserved_overflow_remains_exhausted_when_authentication_finishes_late() {
    let (mut runtime, root) = fixture();
    let mut excess = connect_as_reserved(&mut runtime, Profile::Controller, true, Some(41), true);
    excess.raw(b"MOOR");
    let error = excess.recv_kind(&mut runtime, 0x13);
    assert_eq!(
        u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
        13
    );
    assert!(excess.closed(&mut runtime));
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sixty_four_authenticated_controllers_are_admitted_and_peer_sixty_five_is_refused() {
    let (mut runtime, root) = fixture();
    let mut peers = Vec::new();
    for _ in 0..64 {
        let mut peer = connect(&mut runtime);
        hello(&mut peer, &mut runtime);
        peer.send(7, 13, &[]);
        assert_eq!(peer.recv(&mut runtime).kind, 14);
        peers.push(peer);
    }
    let mut excess = connect(&mut runtime);
    excess.raw(b"MOOR");
    let error = excess.recv_kind(&mut runtime, 0x13);
    assert_eq!(
        u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
        13
    );
    assert!(excess.closed(&mut runtime));
    peers[0].send(7, 13, &[]);
    assert_eq!(peers[0].recv(&mut runtime).kind, 14);
    drop((peers, runtime));
    fs::remove_dir_all(root).unwrap();
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
    std::thread::sleep(Duration::from_millis(2_010));
    runtime.poll();
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
    let pty = Duplex::tracked(pty.try_clone().unwrap(), std::io::sink(), 1 << 20);
    let mut runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty,
        storage: SessionStorage::new(None, None, lifecycle, 8, 1 << 20),
        status: Vec::new(),
        commit_at: 0,
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
    let error = peer.recv_kind(&mut runtime, 0x13);
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
    let error = peer.recv_kind(&mut runtime, 0x13);
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
    peer.recv_kind(&mut runtime, 4);
    peer.recv_kind(&mut runtime, 5);
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
    // The ratified §16 V32 geometry table, byte-for-byte. Each `bytes` value is
    // the frozen `columns:u16-le, rows:u16-le` pair from the artifact, so the
    // rows below are the contract rather than a restatement of the bound.
    for (columns, rows, code, at_attach, bytes) in [
        (80u16, 0u16, 14u16, true, None),
        (0, 24, 14, true, None),
        (0, 1, 14, true, Some([0x00, 0x00, 0x01, 0x00])),
        (1, 0, 14, true, Some([0x01, 0x00, 0x00, 0x00])),
        (2001, 1000, 5, true, Some([0xD1, 0x07, 0xE8, 0x03])),
        (32_767, 62, 5, true, Some([0xFF, 0x7F, 0x3E, 0x00])),
        (32_768, 1, 5, true, Some([0x00, 0x80, 0x01, 0x00])),
        (1, 32_768, 5, true, None),
        (32_767, 32_767, 5, true, None),
        (32_768, 1, 5, false, None),
        (2001, 1000, 5, false, None),
    ] {
        // Where the artifact freezes the bytes, confirm our little-endian pair
        // is exactly those bytes before asserting the required outcome.
        if let Some(frozen) = bytes {
            let mut encoded = [0; 4];
            encoded[..2].copy_from_slice(&columns.to_le_bytes());
            encoded[2..].copy_from_slice(&rows.to_le_bytes());
            assert_eq!(encoded, frozen, "frozen V32 geometry bytes");
        }
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
            peer.recv_kind(&mut runtime, 4);
            peer.recv_kind(&mut runtime, 5);
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
        let error = peer.recv_kind(&mut runtime, 0x13);
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
        let error = peer.recv_kind(&mut runtime, 0x13);
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
    let error = peer.recv_kind(&mut runtime, 0x13);
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
    peer.recv_kind(&mut runtime, 4);
    peer.recv_kind(&mut runtime, 5);
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
        status.payload[36] & 0x10,
        0,
        "class-mismatched reply extended ownership"
    );
    assert_eq!(
        u32::from_le_bytes(status.payload[37..41].try_into().unwrap()),
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
        peer.recv_kind(&mut runtime, 4);
        peer.recv_kind(&mut runtime, 5);
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
    peer.recv_kind(&mut runtime, 4);
    peer.recv_kind(&mut runtime, 5);
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
    let mut runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty: duplex(reader, Capture(capture.clone()), 1024),
        storage: SessionStorage::new(None, None, lifecycle, 8, 1 << 20),
        status: Vec::new(),
        commit_at: 0,
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
fn a_committed_event_record_wakes_controllers_with_an_empty_frame() {
    // OB-30 gives the supervisor two carriers: HEARTBEAT for liveness and
    // WAKEUP for "the durable event stream advanced". Without WAKEUP the only
    // way to learn a session's stream moved is to poll every session's store,
    // which is the design OB-30 exists to remove. Schema §2 freezes type 0x11
    // with an empty payload.
    let (mut runtime, paths) = event_fixture_with(8, 1 << 20);
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    // A title observation is a durable `state` event, so its commit advances
    // the stream. A WAKEUP is owed even though this controller never attached.
    runtime.output(b"\x1b]2;idle\x07".to_vec());
    let wakeup = peer.recv_kind(&mut runtime, 0x11);
    assert!(wakeup.payload.is_empty(), "{wakeup:?}");
    drop(runtime);
    for path in paths {
        fs::remove_dir_all(path).unwrap();
    }
}

#[test]
fn a_quiet_session_never_wakes_a_controller() {
    // The other half of OB-30: silence on WAKEUP is the normal state of a quiet
    // session and must never be readable as death. A session that commits
    // nothing must emit no 0x11 at all — the wakeup is a level trigger on
    // durable advance, not a periodic tick. (Heartbeat cadence is a separate
    // carrier; asserting it here would mean sleeping out its 5 s interval.)
    let (mut runtime, paths) = event_fixture_with(8, 1 << 20);
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    peer.send(7, 3, &[0, 0, 0, 0, 1]);
    peer.recv_kind(&mut runtime, 4);
    peer.recv_kind(&mut runtime, 5);
    for _ in 0..8 {
        runtime.poll();
    }
    while let Some(message) = peer.try_recv(&mut runtime) {
        assert_ne!(message.kind, 0x11, "quiet session emitted a WAKEUP");
    }
    drop(runtime);
    for path in paths {
        fs::remove_dir_all(path).unwrap();
    }
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
fn expired_dialect_only_peers_immediately_free_the_normal_admission_slots() {
    let (mut runtime, root) = fixture();
    let mut stalled = (0..16).map(|_| connect(&mut runtime)).collect::<Vec<_>>();
    for peer in &mut stalled {
        peer.raw(b"MOOR");
    }
    for _ in 0..20 {
        runtime.poll();
    }
    runtime.tick(monotonic().saturating_add(5_001));

    let mut replacement = connect(&mut runtime);
    hello(&mut replacement, &mut runtime);
    drop((stalled, runtime));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hello_cannot_restart_the_whole_status_exchange_deadline() {
    let (mut runtime, root) = fixture();
    let mut peer = connect(&mut runtime);
    std::thread::sleep(Duration::from_millis(1_100));
    hello(&mut peer, &mut runtime);
    std::thread::sleep(Duration::from_millis(950));
    peer.send(7, 13, &[]);
    let reply = peer.recv(&mut runtime);
    assert_eq!(
        reply.kind, 0x13,
        "STATUS restarted the accept-time deadline"
    );
    assert_eq!(
        u16::from_le_bytes(reply.payload[..2].try_into().unwrap()),
        12
    );
    assert!(peer.closed(&mut runtime));
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn viewer_resume_cannot_restart_the_whole_attach_exchange_deadline() {
    let (mut runtime, root) = fixture();
    let mut owner = connect(&mut runtime);
    hello(&mut owner, &mut runtime);
    owner.send(7, 3, &[0, 0, 0, 0, 1]);
    owner.recv_kind(&mut runtime, 4);
    owner.recv_kind(&mut runtime, 5);
    let lease = LeaseResult::decode_wire(&owner.recv_kind(&mut runtime, 0x16).payload).unwrap();
    owner.stream.shutdown(std::net::Shutdown::Both).unwrap();
    drop(owner);
    for _ in 0..10 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(2));
    }

    let mut resumed = connect(&mut runtime);
    std::thread::sleep(Duration::from_millis(1_100));
    hello(&mut resumed, &mut runtime);
    resumed.send(
        7,
        0x15,
        &LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::Viewer,
            epoch: lease.epoch,
            incarnation: [1; 16],
            token: lease.token,
        }
        .encode_wire()
        .unwrap(),
    );
    assert_eq!(
        LeaseResult::decode_wire(&resumed.recv_kind(&mut runtime, 0x16).payload)
            .unwrap()
            .outcome,
        ResultOutcome::Resumed
    );
    std::thread::sleep(Duration::from_millis(950));
    resumed.send(7, 3, &[0; 5]);
    let error = resumed.recv(&mut runtime);
    assert_eq!(error.kind, 0x13, "resumed ATTACH restarted the deadline");
    assert_eq!(
        u16::from_le_bytes(error.payload[..2].try_into().unwrap()),
        12
    );
    assert!(resumed.closed(&mut runtime));
    drop(runtime);
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
    excess.raw(b"MOOR");
    let refusal = excess.recv_kind(&mut runtime, 0x13);
    assert_eq!(
        u16::from_le_bytes(refusal.payload[..2].try_into().unwrap()),
        13
    );
    assert_eq!(
        wire::get_compact(&refusal.payload, 2, true),
        Some(b"connection limit exhausted".as_slice())
    );
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
    peer.recv_kind(&mut runtime, 4);
    peer.recv_kind(&mut runtime, 5);
    let granted = LeaseResult::decode_wire(&peer.recv_kind(&mut runtime, 0x16).payload).unwrap();
    runtime.tick(monotonic().saturating_add(10_001));
    peer.send(7, 13, &[]);
    let status = peer.recv_kind(&mut runtime, 14);
    assert_eq!(
        status.payload[36] & 0x30,
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
    let status = peer.recv_kind(&mut runtime, 4);
    assert_eq!(status.payload[36] & 2, 2);
    assert_eq!(peer.recv_kind(&mut runtime, 5).payload.as_ref(), [0, 0]);
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
    let result = runtime.drive(|_, _| None, || None).unwrap();
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
        storage: SessionStorage::new(None, None, lifecycle, 8, 1 << 20),
        status: Vec::new(),
        commit_at: 0,
        synthetic: 0,
        native: ExitNative(exited),
    });
    let mut peer = connect_as(&mut runtime, Profile::Controller);
    hello(&mut peer, &mut runtime);
    peer.send(7, 3, &[0; 5]);
    peer.recv_kind(&mut runtime, 4);
    peer.recv_kind(&mut runtime, 5);
    while peer.try_recv(&mut runtime).is_some() {}
    assert_eq!(
        runtime.drive(|_, _| None, || None).unwrap(),
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
        None,
        false,
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

#[test]
fn status_drains_a_completed_event_commit_before_copying_its_frontier() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "moor-holder-status-race-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&root).unwrap();
    let marker = root.join("session");
    let event = root.join("events");
    let artifacts = holder_artifacts(
        b"\x01/session",
        (Some(7), 7),
        [1; 16],
        [5; 16],
        (1, 1, [2; 16]),
        ArtifactConfig {
            marker: &marker,
            event_path: Some(&event),
            encoding: "posix-bytes",
            event_identity: Some(event.as_os_str().as_encoded_bytes()),
            instrument_identity: None,
            event_store: None,
            event_directory: None,
            stores: None,
            event_layout: 2,
            log_cap: 0,
        },
    )
    .unwrap();
    let commit_at = artifacts.commit_at;
    let initial = Store::read_only(&event, Kind::Event, 7).unwrap().0;
    let mut runtime = artifacts.runtime(
        duplex(Cursor::new(Vec::new()), std::io::sink(), 1024),
        (0, FakeNative),
    );
    let mut peer = connect(&mut runtime);
    peer.send(0, 1, &wire::controller_hello(b"\x01/session").unwrap());
    assert_eq!(peer.recv(&mut runtime).kind, 2);

    runtime.output(b"\x1b]2;advanced\x07".to_vec());
    let deadline = Instant::now() + Duration::from_secs(2);
    let durable = loop {
        let selected = Store::read_only(&event, Kind::Event, 7).unwrap().0;
        if selected.index > initial.index {
            break selected;
        }
        assert!(Instant::now() < deadline, "event commit did not finish");
        std::thread::sleep(Duration::from_millis(2));
    };

    // The worker completion is queued, but Runtime::poll currently handles the
    // STATUS request before draining storage completions.
    peer.send(7, 13, &[]);
    let status = peer.recv_kind(&mut runtime, 14);
    let reported = u64::from_le_bytes(
        status.payload[commit_at + 1..commit_at + 9]
            .try_into()
            .unwrap(),
    );
    assert_eq!(reported, durable.index, "STATUS reported a stale commit");

    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn v25_status_emitter_patches_exact_selected_commit_region() {
    const HEADER: &[u8] = b"{\"v\":2,\"type\":\"header\",\"ts\":0,\"session\":\"AS90bXAvLm1vb3ItMTAwMC9idWlsZA==\",\"generation\":7,\"epoch\":0,\"next_seq\":0,\"first_retained\":0}\n";
    const COMMIT: [u8; 49] = [
        0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x85, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x2B, 0xBE, 0xEF, 0xB6, 0x37, 0x54, 0x66, 0x12, 0xD6, 0xA3, 0xA6, 0xBD, 0x7C,
        0xBD, 0xB7, 0xBE, 0x29, 0x42, 0xD6, 0xDA, 0xDD, 0xC7, 0x33, 0x39, 0x54, 0x45, 0xF9, 0xED,
        0xD7, 0x88, 0xB6, 0x4B,
    ];

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("moor-v25-status-{}-{nonce}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let marker = root.join("build");
    let event = root.join("events");
    let identity = b"\x01/tmp/.moor-1000/build";
    let incarnation = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    // V25 covers both time fields but contradicts itself: its event header
    // freezes ts=0, while its descriptor and V24 lifecycle freeze wall=1.
    // Start wall 0 selects the frozen header/commit. The assertions below
    // expose that emitted 0 rather than treating it as unspecified; only its
    // comparison with V25's frozen wall=1 awaits the consumer-team ruling.
    let mut artifacts = holder_artifacts(
        identity,
        (Some(7), 7),
        incarnation,
        [0; 16],
        (0, 2, [3; 16]),
        ArtifactConfig {
            marker: &marker,
            event_path: Some(&event),
            encoding: "posix-bytes",
            event_identity: Some(b"/tmp/events"),
            instrument_identity: None,
            event_store: None,
            event_directory: None,
            stores: None,
            event_layout: 2,
            log_cap: 0,
        },
    )
    .unwrap();

    let commit_at = artifacts.commit_at;
    let (selected, body) = Store::read_only(&event, Kind::Event, 7).unwrap();
    let digest: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&body).into();
    assert_eq!(commit_at, 62, "V25 selected-commit offset");
    assert_eq!(body, HEADER, "holder persisted the frozen V25 header");
    assert_eq!(body.len(), 133);
    assert_eq!(
        (selected.body, selected.index, selected.length),
        (0, 1, 133)
    );
    assert_eq!(selected.hash, digest);
    assert_eq!(&digest, &COMMIT[17..], "frozen V25 header SHA-256");
    assert_eq!(
        &artifacts.status[commit_at..commit_at + COMMIT.len()],
        &COMMIT,
        "prebuilt descriptor frontier"
    );
    let wall_at = commit_at + COMMIT.len();
    assert_eq!(
        u64::from_le_bytes(artifacts.status[wall_at..wall_at + 8].try_into().unwrap()),
        0,
        "wall 0 is required by the frozen header"
    );
    assert_eq!(
        u64::from_le_bytes(
            artifacts.status[wall_at + 8..wall_at + 16]
                .try_into()
                .unwrap()
        ),
        2,
        "non-contradictory frozen monotonic time"
    );
    assert_eq!(
        &artifacts.status[wall_at + 16..wall_at + 32],
        &[3; 16],
        "non-contradictory frozen boot identity"
    );

    // Complete the POSIX descriptor exactly as the launcher does. The fixed
    // sizes make the emitted payload the same 244-byte shape frozen by V25.
    wire::put_wide(&mut artifacts.status, b"/tmp").unwrap();
    artifacts.status.extend_from_slice(&0x1234u32.to_le_bytes());
    artifacts.status.extend_from_slice(&0x5678u32.to_le_bytes());
    artifacts.status.extend(0x10u8..=0x1F);
    assert_eq!(artifacts.status.len(), 175);

    // A construction-only assertion can pass even if live STATUS patching is
    // removed. Poison every cached byte so the reply must use the lane's
    // published selected-commit frontier.
    artifacts.status[commit_at..commit_at + COMMIT.len()].fill(0xA5);
    assert_ne!(
        &artifacts.status[commit_at..commit_at + COMMIT.len()],
        &COMMIT
    );
    let mut runtime = artifacts.runtime(
        duplex(Cursor::new(Vec::new()), std::io::sink(), 1024),
        (0, FakeNative),
    );
    let mut peer = connect(&mut runtime);
    peer.send(0, 1, &wire::controller_hello(identity).unwrap());
    assert_eq!(peer.recv(&mut runtime).kind, 2);
    peer.send(7, 13, &[]);
    let status = peer.recv_kind(&mut runtime, 14);

    assert_eq!(status.payload.len(), 248, "frozen V25 payload shape");
    assert_eq!(
        &status.payload[commit_at..commit_at + COMMIT.len()],
        &COMMIT,
        "live STATUS copied the real selected event commit"
    );
    wire::StatusTail::decode_for(&status.payload, identity, 7, incarnation)
        .expect("live V25-shaped STATUS descriptor decodes");

    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_native_resize_refuses_the_attach_and_keeps_the_row_model() {
    struct FailResize;
    impl Native for FailResize {
        fn resize(&mut self, _: u16, _: u16) -> Result<(), String> {
            Err("injected resize failure".into())
        }
        fn terminate(&mut self, _: bool) -> (u8, bool) {
            (0, false)
        }
        fn exited(&mut self) -> Result<Option<NativeExit>, String> {
            Ok(None)
        }
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "moor-holder-failed-resize-{}-{nonce}",
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
    let mut runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty: duplex(Cursor::new(Vec::new()), std::io::sink(), 1024),
        storage: SessionStorage::new(None, None, lifecycle, 8, 1 << 20),
        status: Vec::new(),
        commit_at: 0,
        synthetic: 0,
        native: FailResize,
    });

    let mut owner = connect_as(&mut runtime, Profile::Controller);
    hello(&mut owner, &mut runtime);
    owner.send(7, 3, &[80, 0, 50, 0, 1]);
    // v4: the platform refused the requested 50-row size, so the attach
    // fails CLOSED — no descriptor claiming a size the pty does not have,
    // no terminal bytes, no lease. The holder closes the link instead.
    let deadline = Instant::now() + Duration::from_secs(2);
    let refused = loop {
        runtime.poll();
        let mut bytes = [0; 256];
        match owner.stream.read(&mut bytes) {
            Ok(0) => break true,
            Ok(_) => break false,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(_) => break true,
        }
        assert!(
            Instant::now() < deadline,
            "holder neither replied nor closed"
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(
        refused,
        "a failed attach resize must close the link, not attach"
    );

    // Runtime starts at 24 rows. Because the requested 50-row resize failed,
    // 1..24 is still the full/default region and must serialize as CSI r.
    runtime.output(b"\x1b[1;24r".to_vec());
    let mut observer = connect_as(&mut runtime, Profile::Controller);
    hello(&mut observer, &mut runtime);
    observer.send(7, 3, &[0; 5]);
    let terminal = observer.recv_kind(&mut runtime, 5);
    let preamble = &terminal.payload[2..];
    assert!(preamble.windows(3).any(|part| part == b"\x1b[r"));
    assert!(!preamble.windows(7).any(|part| part == b"\x1b[1;24r"));

    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_attach_resize_rolls_back_the_provisional_lease_for_the_next_controller() {
    // The attach transaction grants the fresh lease BEFORE the holder can
    // attempt the fallible native resize. When that resize fails, the holder
    // closes the link without ever delivering the token — so if the grant
    // survived, it would sit as an invisible reservation until its deadline,
    // refusing every honest fresh controller for a lease nobody can use. The
    // grant is provisional until its token is delivered inside the attach
    // prefix: any earlier failure must roll it back entirely.
    struct FailTall;
    impl Native for FailTall {
        fn resize(&mut self, rows: u16, _: u16) -> Result<(), String> {
            if rows > 24 {
                Err("injected resize failure".into())
            } else {
                Ok(())
            }
        }
        fn terminate(&mut self, _: bool) -> (u8, bool) {
            (0, false)
        }
        fn exited(&mut self) -> Result<Option<NativeExit>, String> {
            Ok(None)
        }
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "moor-holder-lease-rollback-{}-{nonce}",
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
    let mut runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty: duplex(Cursor::new(Vec::new()), std::io::sink(), 1024),
        storage: SessionStorage::new(None, None, lifecycle, 8, 1 << 20),
        status: Vec::new(),
        commit_at: 0,
        synthetic: 0,
        native: FailTall,
    });

    // First controller requests a fresh lease together with a 50-row attach
    // the platform will refuse. The failure must close the link with no
    // successful prefix byte.
    let mut first = connect_as(&mut runtime, Profile::Controller);
    hello(&mut first, &mut runtime);
    first.send(7, 3, &[80, 0, 50, 0, 1]);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        runtime.poll();
        let mut bytes = [0; 256];
        match first.stream.read(&mut bytes) {
            Ok(0) => break,
            Ok(_) => panic!("a failed attach resize must emit no prefix bytes"),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
        assert!(
            Instant::now() < deadline,
            "holder neither replied nor closed"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    // The very next fresh controller must find the lease FREE and the session
    // geometry untouched by the refused 50-row request. Without rollback the
    // provisional grant lingers until its deadline and this attach receives
    // Refused for a token nobody ever held.
    let mut second = connect_as(&mut runtime, Profile::Controller);
    hello(&mut second, &mut runtime);
    second.send(7, 3, &[80, 0, 24, 0, 1]);
    assert_eq!(second.recv_kind(&mut runtime, 4).kind, 4);
    assert_eq!(second.recv_kind(&mut runtime, 5).kind, 5);
    let lease = LeaseResult::decode_wire(&second.recv_kind(&mut runtime, 0x16).payload).unwrap();
    assert_eq!(
        lease.outcome,
        ResultOutcome::Granted,
        "the rolled-back provisional lease must not shadow the next controller"
    );
    // The attach/grant transaction is atomic: the uncommitted grant consumed
    // no epoch, so the successor receives exactly the number the rolled-back
    // grant would have carried — not a silently skipped one.
    assert_eq!(
        lease.epoch, 1,
        "an uncommitted provisional grant must not consume a protocol epoch"
    );

    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_resumed_attach_resize_preserves_the_reservation_for_exact_resume() {
    // The mirror side of the rollback rule. A RESUMED viewer already holds a
    // known epoch/token from before this attach: its resize failure is
    // ordinary transport loss, so the reservation must survive — a competing
    // fresh controller may not steal the lease, and the owner must be able to
    // resume with its exact tuple afterwards. Only the fresh grant whose token
    // was never delivered rolls back.
    struct FailTall;
    impl Native for FailTall {
        fn resize(&mut self, rows: u16, _: u16) -> Result<(), String> {
            if rows > 24 {
                Err("injected resize failure".into())
            } else {
                Ok(())
            }
        }
        fn terminate(&mut self, _: bool) -> (u8, bool) {
            (0, false)
        }
        fn exited(&mut self) -> Result<Option<NativeExit>, String> {
            Ok(None)
        }
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "moor-holder-resume-reservation-{}-{nonce}",
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
    let mut runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty: duplex(Cursor::new(Vec::new()), std::io::sink(), 1024),
        storage: SessionStorage::new(None, None, lifecycle, 8, 1 << 20),
        status: Vec::new(),
        commit_at: 0,
        synthetic: 0,
        native: FailTall,
    });

    // The owner takes a fresh viewer lease with a geometry-free attach, then
    // loses its link: ordinary reservation, epoch/token intact.
    let mut owner = connect_as(&mut runtime, Profile::Controller);
    hello(&mut owner, &mut runtime);
    owner.send(7, 3, &[0, 0, 0, 0, 1]);
    owner.recv_kind(&mut runtime, 4);
    owner.recv_kind(&mut runtime, 5);
    let lease = LeaseResult::decode_wire(&owner.recv_kind(&mut runtime, 0x16).payload).unwrap();
    assert_eq!(lease.outcome, ResultOutcome::Granted);
    owner.stream.shutdown(std::net::Shutdown::Both).unwrap();
    drop(owner);
    for _ in 0..10 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(2));
    }

    // The owner resumes on a new connection, then requests an attach whose
    // 50-row geometry the platform refuses. The holder closes the link.
    let mut resumed = connect_as(&mut runtime, Profile::Controller);
    hello(&mut resumed, &mut runtime);
    resumed.send(
        7,
        0x15,
        &LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::Viewer,
            epoch: lease.epoch,
            incarnation: [1; 16],
            token: lease.token,
        }
        .encode_wire()
        .unwrap(),
    );
    let rotated = LeaseResult::decode_wire(&resumed.recv_kind(&mut runtime, 0x16).payload).unwrap();
    assert_eq!(rotated.outcome, ResultOutcome::Resumed);
    resumed.send(7, 3, &[80, 0, 50, 0, 0]);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        runtime.poll();
        let mut bytes = [0; 256];
        match resumed.stream.read(&mut bytes) {
            Ok(0) => break,
            Ok(_) => panic!("a failed resumed attach resize must emit no prefix bytes"),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
        assert!(
            Instant::now() < deadline,
            "holder neither replied nor closed"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    // A competing fresh controller must NOT obtain the lease: the reservation
    // belongs to the resumed owner's known tuple.
    let mut thief = connect_as(&mut runtime, Profile::Controller);
    hello(&mut thief, &mut runtime);
    thief.send(7, 3, &[80, 0, 24, 0, 1]);
    assert_eq!(thief.recv_kind(&mut runtime, 4).kind, 4);
    assert_eq!(thief.recv_kind(&mut runtime, 5).kind, 5);
    assert_eq!(
        LeaseResult::decode_wire(&thief.recv_kind(&mut runtime, 0x16).payload)
            .unwrap()
            .outcome,
        ResultOutcome::Refused,
        "the preserved reservation must refuse a competing fresh grant"
    );

    // And the owner's exact tuple still resumes.
    let mut returning = connect_as(&mut runtime, Profile::Controller);
    hello(&mut returning, &mut runtime);
    returning.send(
        7,
        0x15,
        &LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::Viewer,
            epoch: rotated.epoch,
            incarnation: [1; 16],
            // Resume rotates the token: the owner's known tuple is the one
            // from its LAST successful lease exchange.
            token: rotated.token,
        }
        .encode_wire()
        .unwrap(),
    );
    let comeback =
        LeaseResult::decode_wire(&returning.recv_kind(&mut runtime, 0x16).payload).unwrap();
    assert_eq!(
        comeback.outcome,
        ResultOutcome::Resumed,
        "the reservation must still admit the owner's exact tuple: {comeback:?}"
    );
    assert_eq!(
        comeback.epoch, rotated.epoch,
        "a resumed viewer's failure keeps its existing epoch, never a new one"
    );

    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_lease_reply_send_rolls_back_the_provisional_grant_at_the_queue_boundary() {
    // The provisional transaction extends through the delivery of the token
    // frame ITSELF. The status and terminal frames can enqueue while the
    // LEASE_RESULT that carries the only copy of the token hits a full
    // outbound queue: the holder disconnects, and without rollback the
    // ownerless reservation and its consumed epoch survive to refuse the
    // next honest controller. The gated transport reproduces that queue
    // boundary deterministically.
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct GatedWriter {
        inner: UnixStream,
        gate: Arc<AtomicBool>,
    }
    impl Write for GatedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            while !self.gate.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    let fixture = |tag: &str| {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "moor-holder-token-boundary-{tag}-{}-{nonce}",
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
        let runtime = Runtime::new(HolderConfig {
            core: CoreConfig {
                generation: 7,
                identity: b"session".to_vec(),
                incarnation: [1; 16],
                semantic_token: [0; 16],
                replay_limit: 1024,
            },
            pty: duplex(Cursor::new(Vec::new()), std::io::sink(), 1024),
            storage: SessionStorage::new(None, None, lifecycle, 8, 1 << 20),
            status: Vec::new(),
            commit_at: 0,
            synthetic: 0,
            native: FakeNative,
        });
        (runtime, root)
    };

    // Calibration round on a separate fixture: measure the exact on-wire
    // sizes of the status descriptor and the empty NON_VT preamble, so the
    // gated budget below admits exactly those two frames and nothing more.
    let (mut calibrate, calibration_root) = fixture("calibrate");
    let mut probe = connect_as(&mut calibrate, Profile::Controller);
    hello(&mut probe, &mut calibrate);
    probe.send(7, 3, &[0, 0, 0, 0, 3]);
    let status_len = 24 + probe.recv_kind(&mut calibrate, 4).payload.len();
    let terminal_len = 24 + probe.recv_kind(&mut calibrate, 5).payload.len();
    probe.recv_kind(&mut calibrate, 0x16);
    drop(calibrate);
    fs::remove_dir_all(calibration_root).unwrap();

    // Gated round: the handshake drains through the open gate; the gate then
    // closes, so the budget frees nothing and admits exactly status+terminal.
    let (mut runtime, root) = fixture("gated");
    let gate = Arc::new(AtomicBool::new(true));
    let (client, server) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let writer = GatedWriter {
        inner: server.try_clone().unwrap(),
        gate: Arc::clone(&gate),
    };
    let close = server.try_clone().unwrap();
    runtime.accept(
        Duplex::closing(server, writer, status_len + terminal_len, move || {
            let _ = close.shutdown(std::net::Shutdown::Both);
        }),
        true,
        None,
        false,
    );
    let mut first = Peer {
        stream: client,
        codec: Codec::new(Profile::Controller),
        queued: VecDeque::new(),
    };
    hello(&mut first, &mut runtime);
    // Let the transport thread finish accounting for the drained HELLO_ACK
    // before the gate closes, so the budget is exactly the two-frame one.
    std::thread::sleep(Duration::from_millis(50));
    gate.store(false, Ordering::Relaxed);
    first.send(7, 3, &[0, 0, 0, 0, 3]);
    // Let the holder process the attach: status and terminal enqueue into the
    // exact two-frame budget, the token frame hits Full, and the holder
    // disconnects the requester. The successor below is the detector — its
    // lease outcome distinguishes rollback from a leaked reservation. (The
    // first link's own closure is not observable here: the gated writer
    // thread parks inside the closed gate, so the transport's close callback
    // cannot run until the gate reopens at the end of the test.)
    for _ in 0..50 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(first);

    // The token never left the holder, so the grant was not committed: the
    // very next fresh controller must receive it — at epoch 1 exactly.
    let mut second = connect_as(&mut runtime, Profile::Controller);
    hello(&mut second, &mut runtime);
    second.send(7, 3, &[0, 0, 0, 0, 1]);
    assert_eq!(second.recv_kind(&mut runtime, 4).kind, 4);
    assert_eq!(second.recv_kind(&mut runtime, 5).kind, 5);
    let lease = LeaseResult::decode_wire(&second.recv_kind(&mut runtime, 0x16).payload).unwrap();
    assert_eq!(
        (lease.outcome, lease.epoch),
        (ResultOutcome::Granted, 1),
        "a token frame that never enqueued must roll the provisional grant back"
    );

    // Reopen the gate so the parked writer thread can finish against the
    // now-dead socket and the runtime can join it on drop.
    gate.store(true, Ordering::Relaxed);
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_upgrade_lease_reply_send_rolls_back_the_standalone_fresh_grant() {
    // The standalone LEASE exchange has the same queue boundary as the attach
    // shorthand: the session mutates the grant FIRST and only then emits the
    // result frame. An observer upgrading to a fresh viewer lease whose
    // result frame cannot enqueue must not leave an invisible reservation —
    // the grant was never committed, so the next fresh controller receives
    // that very epoch.
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct GatedWriter {
        inner: UnixStream,
        gate: Arc<AtomicBool>,
    }
    impl Write for GatedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            while !self.gate.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    // Calibration: the exact on-wire sizes of the observer prefix.
    let (mut calibrate, calibration_root) = fixture();
    let mut probe = connect(&mut calibrate);
    hello(&mut probe, &mut calibrate);
    probe.send(7, 3, &[0, 0, 0, 0, 0]);
    let status_len = 24 + probe.recv_kind(&mut calibrate, 4).payload.len();
    let terminal_len = 24 + probe.recv_kind(&mut calibrate, 5).payload.len();
    drop(calibrate);
    fs::remove_dir_all(calibration_root).unwrap();

    let (mut runtime, root) = fixture();
    let gate = Arc::new(AtomicBool::new(true));
    let (client, server) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let writer = GatedWriter {
        inner: server.try_clone().unwrap(),
        gate: Arc::clone(&gate),
    };
    let close = server.try_clone().unwrap();
    runtime.accept(
        Duplex::closing(server, writer, status_len + terminal_len, move || {
            let _ = close.shutdown(std::net::Shutdown::Both);
        }),
        true,
        None,
        false,
    );
    let mut observer = Peer {
        stream: client,
        codec: Codec::new(Profile::Controller),
        queued: VecDeque::new(),
    };
    hello(&mut observer, &mut runtime);
    std::thread::sleep(Duration::from_millis(50));
    gate.store(false, Ordering::Relaxed);
    // The observer prefix exactly fills the gated budget, so the upgrade's
    // result frame below cannot enqueue.
    observer.send(7, 3, &[0, 0, 0, 0, 0]);
    for _ in 0..25 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    observer.send(
        7,
        0x15,
        &LeaseRequest {
            operation: LeaseOperation::Fresh,
            role: LeaseRole::Viewer,
            epoch: 0,
            incarnation: [0; 16],
            token: [0; 16],
        }
        .encode_wire()
        .unwrap(),
    );
    for _ in 0..25 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(observer);

    let mut second = connect(&mut runtime);
    hello(&mut second, &mut runtime);
    second.send(7, 3, &[0, 0, 0, 0, 1]);
    assert_eq!(second.recv_kind(&mut runtime, 4).kind, 4);
    assert_eq!(second.recv_kind(&mut runtime, 5).kind, 5);
    let lease = LeaseResult::decode_wire(&second.recv_kind(&mut runtime, 0x16).payload).unwrap();
    assert_eq!(
        (lease.outcome, lease.epoch),
        (ResultOutcome::Granted, 1),
        "an upgrade result that never enqueued must roll the standalone grant back"
    );

    gate.store(true, Ordering::Relaxed);
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_resume_lease_reply_send_restores_the_entire_prior_reservation() {
    // Resume ROTATES the token as it re-establishes ownership. When the
    // result frame carrying the rotated token cannot enqueue, the requester
    // still knows only the OLD tuple — so the mutation must unwind entirely:
    // ownerless state, the old token, and the original deadline all return,
    // and the owner's next resume with the old tuple succeeds. A refused
    // result stays nonmutating throughout (the first, wrong-token resume
    // below proves it: the reservation it bounced off is the one restored).
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct GatedWriter {
        inner: UnixStream,
        gate: Arc<AtomicBool>,
    }
    impl Write for GatedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            while !self.gate.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    let (mut runtime, root) = fixture();

    // The owner takes a standalone fresh input-only lease, then loses its
    // link: an ordinary reservation under the granted token.
    let mut owner = connect(&mut runtime);
    hello(&mut owner, &mut runtime);
    owner.send(
        7,
        0x15,
        &LeaseRequest {
            operation: LeaseOperation::Fresh,
            role: LeaseRole::InputOnly,
            epoch: 0,
            incarnation: [0; 16],
            token: [0; 16],
        }
        .encode_wire()
        .unwrap(),
    );
    let granted_frame = owner.recv_kind(&mut runtime, 0x16);
    let result_len = 24 + granted_frame.payload.len();
    let granted = LeaseResult::decode_wire(&granted_frame.payload).unwrap();
    assert_eq!(granted.outcome, ResultOutcome::Granted);
    owner.stream.shutdown(std::net::Shutdown::Both).unwrap();
    drop(owner);
    for _ in 0..10 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(2));
    }

    // Gated connection: HELLO_ACK drains through the open gate; the gate
    // then closes. The budget is one ACK — it admits exactly one parked
    // result frame, and the second cannot enqueue.
    let gate = Arc::new(AtomicBool::new(true));
    let (client, server) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let writer = GatedWriter {
        inner: server.try_clone().unwrap(),
        gate: Arc::clone(&gate),
    };
    let close = server.try_clone().unwrap();
    let mut resumer = Peer {
        stream: client,
        codec: Codec::new(Profile::Controller),
        queued: VecDeque::new(),
    };
    resumer.send(0, 1, &wire::controller_hello(b"session").unwrap());
    // Measure the ACK to size the budget; accept AFTER building the frame so
    // the limit can depend on it? The limit must be fixed at accept time, so
    // derive it from the calibration constant instead: the ACK for this
    // fixture is deterministic, measured on the owner connection above.
    let ack_len = {
        let (mut calibrate, calibration_root) = fixture();
        let mut probe = connect(&mut calibrate);
        probe.send(0, 1, &wire::controller_hello(b"session").unwrap());
        let ack = probe.recv(&mut calibrate);
        assert_eq!(ack.kind, 2);
        let len = 24 + ack.payload.len();
        drop(calibrate);
        fs::remove_dir_all(calibration_root).unwrap();
        len
    };
    assert!(
        result_len <= ack_len && ack_len < 2 * result_len,
        "budget precondition: result {result_len}, ack {ack_len} — recalibrate the gated budget"
    );
    runtime.accept(
        Duplex::closing(server, writer, ack_len, move || {
            let _ = close.shutdown(std::net::Shutdown::Both);
        }),
        true,
        None,
        false,
    );
    assert_eq!(resumer.recv(&mut runtime).kind, 2);
    std::thread::sleep(Duration::from_millis(50));
    gate.store(false, Ordering::Relaxed);

    // First resume: WRONG token. Refused, nonmutating — and its parked
    // result frame consumes the remaining budget.
    resumer.send(
        7,
        0x15,
        &LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::InputOnly,
            epoch: granted.epoch,
            incarnation: [1; 16],
            token: [0xEE; 16],
        }
        .encode_wire()
        .unwrap(),
    );
    for _ in 0..25 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    // Second resume: the RIGHT tuple. The mutation succeeds, the rotated
    // token cannot enqueue, and the whole reservation must unwind.
    resumer.send(
        7,
        0x15,
        &LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::InputOnly,
            epoch: granted.epoch,
            incarnation: [1; 16],
            token: granted.token,
        }
        .encode_wire()
        .unwrap(),
    );
    for _ in 0..25 {
        runtime.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(resumer);

    // The owner returns with the OLD tuple — the only one it ever saw.
    let mut returning = connect(&mut runtime);
    hello(&mut returning, &mut runtime);
    returning.send(
        7,
        0x15,
        &LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::InputOnly,
            epoch: granted.epoch,
            incarnation: [1; 16],
            token: granted.token,
        }
        .encode_wire()
        .unwrap(),
    );
    let comeback =
        LeaseResult::decode_wire(&returning.recv_kind(&mut runtime, 0x16).payload).unwrap();
    assert_eq!(
        (comeback.outcome, comeback.epoch),
        (ResultOutcome::Resumed, granted.epoch),
        "a rotated token that never enqueued must restore the prior reservation: {comeback:?}"
    );

    gate.store(true, Ordering::Relaxed);
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn geometry_notifications_are_change_only_with_one_attach_redraw() {
    use std::sync::{Arc, Mutex};

    struct CountResize(Arc<Mutex<Vec<(bool, u16, u16)>>>);
    impl Native for CountResize {
        fn resize(&mut self, rows: u16, columns: u16) -> Result<(), String> {
            self.0.lock().unwrap().push((false, rows, columns));
            if (rows, columns) == (40, 120) {
                Err("injected resize failure".into())
            } else {
                Ok(())
            }
        }
        fn redraw(&mut self, rows: u16, columns: u16) -> Result<(), String> {
            self.0.lock().unwrap().push((true, rows, columns));
            Ok(())
        }
        fn terminate(&mut self, _: bool) -> (u8, bool) {
            (0, false)
        }
        fn exited(&mut self) -> Result<Option<NativeExit>, String> {
            Ok(None)
        }
    }

    let root = std::env::temp_dir().join(format!(
        "moor-holder-ordinary-resize-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let lifecycle = Store::create(
        &root,
        Kind::Exit,
        7,
        lifecycle_running(
            b"\x01/session",
            (Some(7), 7),
            [1; 16],
            (1, 1, [2; 16]),
            ("posix-bytes", None, None),
        )
        .as_bytes(),
        0,
        0,
    )
    .unwrap();
    let pty = Duplex::tracked(Cursor::new(Vec::new()), std::io::sink(), 1024);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = Runtime::new(HolderConfig {
        core: CoreConfig {
            generation: 7,
            identity: b"session".to_vec(),
            incarnation: [1; 16],
            semantic_token: [0; 16],
            replay_limit: 1024,
        },
        pty,
        storage: SessionStorage::new(None, None, lifecycle, 8, 1 << 20),
        status: Vec::new(),
        commit_at: 0,
        synthetic: 0,
        native: CountResize(Arc::clone(&calls)),
    });
    let mut owner = connect_as(&mut runtime, Profile::Controller);
    hello(&mut owner, &mut runtime);
    owner.send(7, 3, &[80, 0, 24, 0, 1]);
    owner.recv_kind(&mut runtime, 4);
    owner.recv_kind(&mut runtime, 5);
    let lease = LeaseResult::decode_wire(&owner.recv_kind(&mut runtime, 0x16).payload).unwrap();
    assert!(calls.lock().unwrap().is_empty(), "redraw none resized");

    let input = [
        lease.epoch.to_le_bytes().as_slice(),
        1u64.to_le_bytes().as_slice(),
        &[0, 0x0c],
    ]
    .concat();
    owner.send(7, 9, &input);
    owner.send(7, 13, &[]);
    owner.recv_kind(&mut runtime, 14);
    assert!(calls.lock().unwrap().is_empty(), "ctrl_l resized");

    let resize = [
        lease.epoch.to_le_bytes().as_slice(),
        80u16.to_le_bytes().as_slice(),
        24u16.to_le_bytes().as_slice(),
    ]
    .concat();
    owner.send(7, 0x0b, &resize);
    owner.send(7, 13, &[]);
    owner.recv_kind(&mut runtime, 14);
    assert!(
        calls.lock().unwrap().is_empty(),
        "an unchanged resize after ctrl_l used the winch exception"
    );

    let release = [lease.epoch.to_le_bytes().as_slice(), lease.token.as_slice()].concat();
    owner.send(7, 0x17, &release);
    owner.recv_kind(&mut runtime, 0x16);
    let mut redraw_owner = connect_as(&mut runtime, Profile::Controller);
    hello(&mut redraw_owner, &mut runtime);
    redraw_owner.send(7, 3, &[80, 0, 24, 0, 1]);
    redraw_owner.recv_kind(&mut runtime, 4);
    redraw_owner.recv_kind(&mut runtime, 5);
    let lease =
        LeaseResult::decode_wire(&redraw_owner.recv_kind(&mut runtime, 0x16).payload).unwrap();
    let resize = [
        lease.epoch.to_le_bytes().as_slice(),
        80u16.to_le_bytes().as_slice(),
        24u16.to_le_bytes().as_slice(),
    ]
    .concat();
    redraw_owner.send(7, 0x0b, &resize);
    redraw_owner.send(7, 0x0b, &resize);
    for (rows, columns) in [(30u16, 100u16), (30, 100), (40, 120), (40, 120), (30, 100)] {
        let resize = [
            lease.epoch.to_le_bytes().as_slice(),
            columns.to_le_bytes().as_slice(),
            rows.to_le_bytes().as_slice(),
        ]
        .concat();
        redraw_owner.send(7, 0x0b, &resize);
    }
    redraw_owner.send(7, 13, &[]);
    redraw_owner.recv_kind(&mut runtime, 14);
    assert_eq!(
        *calls.lock().unwrap(),
        [
            (true, 24, 80),
            (false, 30, 100),
            (false, 40, 120),
            (false, 40, 120)
        ]
    );
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejectable_source_queue_overflow_does_not_close_unrelated_semantic_peers() {
    // A zero-job lane is a deterministic saturated queue. Using a one-job
    // lane plus a background commit races its worker completion against the
    // candidate peer reader under the parallel test harness.
    let (mut runtime, paths) = event_fixture_with(0, 1 << 20);
    let semantic_hello = |producer: u8, mode: u8, source: &[u8]| {
        let mut payload = [
            [5; 16].as_slice(),
            &[producer; 16],
            &7u32.to_le_bytes(),
            &[mode, 1],
        ]
        .concat();
        wire::put_compact(&mut payload, source).unwrap();
        payload
    };

    // An edge producer needs no source-lifecycle commit and is already healthy.
    let mut existing = connect_as(&mut runtime, Profile::Semantic);
    existing.send(0, 1, &semantic_hello(6, 0, b"edge"));
    assert_eq!(existing.recv(&mut runtime).kind, 2);

    let mut candidate = connect_as(&mut runtime, Profile::Semantic);
    candidate.send(0, 1, &semantic_hello(7, 1, b"stateful"));
    let refusal = candidate.recv_kind(&mut runtime, 9);
    assert_eq!(
        u16::from_le_bytes(refusal.payload[..2].try_into().unwrap()),
        12
    );

    // The stateful hello is rejectable and may be refused, but §5.7 says the
    // stream and unrelated accepted producer stay live.
    if let Some(message) = existing.try_recv(&mut runtime) {
        assert_ne!(
            message.kind, 9,
            "unrelated producer received global exhaustion"
        );
    }
    assert!(
        !existing.closed(&mut runtime),
        "unrelated producer was closed"
    );

    drop(runtime);
    for path in paths {
        fs::remove_dir_all(path).unwrap();
    }
}

#[test]
fn final_event_commit_wakes_connected_controllers() {
    let (mut runtime, paths) = event_fixture_with(8, 1 << 20);
    let initial = Store::read_only(&paths[0], Kind::Event, 7).unwrap().0;
    let mut peer = connect(&mut runtime);
    hello(&mut peer, &mut runtime);
    while peer.try_recv(&mut runtime).is_some() {}

    let running = lifecycle_running(
        b"\x01/session",
        (Some(7), 7),
        [1; 16],
        (1, 1, [2; 16]),
        ("posix-bytes", None, None),
    );
    let (_, durable) = runtime.finish_exit(&running, NativeExit::Code(7), None);
    assert!(durable);
    let selected = Store::read_only(&paths[0], Kind::Event, 7).unwrap().0;
    assert!(selected.index > initial.index, "final event did not commit");

    let wakeup = peer.recv_kind(&mut runtime, 0x11);
    assert!(wakeup.payload.is_empty());

    drop(runtime);
    for path in paths {
        fs::remove_dir_all(path).unwrap();
    }
}
