use moor::cli::Options;
use moor::runtime::client::Client;
use moor::runtime::io::{self, Duplex, Event, SendError, ViewerSender};
use moor::session::{LeaseResult, LeaseRole, ResultOutcome, ResultReason};
use moor::wire::{self, Profile};
use std::collections::VecDeque;
use std::io::{Cursor, Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::{
    Arc, Barrier, Condvar, Mutex,
    atomic::{AtomicU8, AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

fn duplex<R: Read + Send + 'static, W: Write + Send + 'static>(
    reader: R,
    writer: W,
    limit: usize,
) -> Duplex {
    Duplex::closing(reader, writer, limit, || {})
}

fn handshake<R: Read + Send + 'static, W: Write + Send + 'static>(
    reader: R,
    writer: W,
    identity: Vec<u8>,
) -> Result<Client, String> {
    Client::handshake_until(
        reader,
        writer,
        identity,
        Instant::now() + Duration::from_secs(2),
        || {},
    )
}

#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    wake: Condvar,
}

impl Gate {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

struct BlockingReader(Arc<Gate>);

impl Read for BlockingReader {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        let mut open = self.0.open.lock().unwrap();
        while !*open {
            open = self.0.wake.wait(open).unwrap();
        }
        Ok(0)
    }
}

struct BurstReader(Arc<AtomicUsize>);

struct Chunks(VecDeque<Vec<u8>>);

impl Read for Chunks {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let Some(next) = self.0.pop_front() else {
            return Ok(0);
        };
        bytes[..next.len()].copy_from_slice(&next);
        Ok(next.len())
    }
}

struct GatedReader(Arc<Gate>, VecDeque<Vec<u8>>);

impl Read for GatedReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let mut open = self.0.open.lock().unwrap();
        while !*open {
            open = self.0.wake.wait(open).unwrap();
        }
        let Some(input) = self.1.pop_front() else {
            return Ok(0);
        };
        bytes[..input.len()].copy_from_slice(&input);
        Ok(input.len())
    }
}

struct SignalWriter(Arc<Mutex<Vec<u8>>>, Arc<Gate>);

impl Write for SignalWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        self.1.open();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for BurstReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.fetch_add(1, Ordering::Relaxed);
        bytes[0] = 1;
        Ok(1)
    }
}

struct PartialFail(bool);

impl Write for PartialFail {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        if std::mem::replace(&mut self.0, false) {
            Ok(2)
        } else {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct SlowWriter {
    started: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    bytes: Arc<Mutex<Vec<u8>>>,
}

#[cfg(unix)]
struct Peer {
    stream: UnixStream,
    inbound: wire::Codec,
    outbound: wire::Codec,
    queued: VecDeque<wire::Message>,
}

#[cfg(unix)]
impl Peer {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            inbound: wire::Codec::new(Profile::Controller),
            outbound: wire::Codec::new(Profile::Controller),
            queued: VecDeque::new(),
        }
    }

    fn recv(&mut self) -> wire::Message {
        loop {
            if let Some(message) = self.queued.pop_front() {
                return message;
            }
            let mut bytes = [0; 8192];
            let count = self.stream.read(&mut bytes).unwrap();
            let mut messages = Vec::new();
            self.inbound
                .feed(0, &bytes[..count], &mut messages)
                .unwrap();
            self.queued.extend(messages);
        }
    }

    fn send(&mut self, scope: u32, kind: u8, payload: &[u8]) {
        let mut bytes = Vec::new();
        self.outbound
            .encode(scope, kind, payload, &mut bytes)
            .unwrap();
        self.stream.write_all(&bytes).unwrap();
    }
}

fn status(first: u64, last: u64, start: u64, end: u64, flags: u8) -> Vec<u8> {
    let mut out = Vec::new();
    wire::put_wide(&mut out, b"\x01/session").unwrap();
    out.extend_from_slice(&7u32.to_le_bytes());
    out.extend_from_slice(&[9; 16]);
    out.push(0);
    wire::put_wide(&mut out, b"").unwrap();
    out.push(0xff);
    out.extend_from_slice(&[0; 48 + 8 + 8 + 16]);
    wire::put_wide(&mut out, b"/tmp").unwrap();
    out.extend_from_slice(&1_u32.to_le_bytes());
    out.extend_from_slice(&1_u32.to_le_bytes());
    out.extend_from_slice(&[1; 16]);
    for value in [first, last, start, end] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.push(flags);
    out.extend_from_slice(&[0; 7 + 29]);
    out
}

#[cfg(unix)]
fn viewer_pair(
    server: impl FnOnce(Peer) + Send + 'static,
) -> (Client, std::thread::JoinHandle<()>) {
    let (client, peer) = UnixStream::pair().unwrap();
    let reader = client.try_clone().unwrap();
    let server = std::thread::spawn(move || server(Peer::new(peer)));
    (
        handshake(reader, client, b"\x01/session".to_vec()).unwrap(),
        server,
    )
}

#[test]
fn attach_owns_input_state_api() {
    fn compiles(client: &mut Client, options: &Options) {
        let _ = io::attach_viewer_to(
            client,
            options,
            (0, 0),
            &mut std::io::sink(),
            Duration::from_secs(15),
            |_| Err("reconnect unavailable".into()),
            |_: ViewerSender, state: Arc<AtomicU8>| {
                assert_eq!(state.load(std::sync::atomic::Ordering::Relaxed), 0)
            },
        );
    }
    let _ = compiles;
}

#[test]
#[cfg(unix)]
fn attach_acknowledges_each_applied_output_record() {
    let (mut client, server) = viewer_pair(|mut peer| {
        assert_eq!(peer.recv().kind, 1);
        peer.send(
            7,
            2,
            &wire::controller_hello_ack(7, [9; 16], b"\x01/session").unwrap(),
        );
        assert_eq!(peer.recv().kind, 3);
        peer.send(7, 5, &[0, 0]);
        peer.send(7, 4, &status(1, 1, 0, 1, 1));
        let output = [
            1_u64.to_le_bytes().as_slice(),
            0_u64.to_le_bytes().as_slice(),
            b"x",
        ]
        .concat();
        peer.send(7, 6, &output);
        let ack = peer.recv();
        assert_eq!(
            (ack.kind, ack.payload.as_ref()),
            (7, 1_u64.to_le_bytes().as_slice())
        );
    });
    let mut output = Vec::new();
    assert!(
        io::attach_viewer_to(
            &mut client,
            &Options::default(),
            (0, 0),
            &mut output,
            Duration::from_secs(15),
            |_| Err("reconnect unavailable".into()),
            |_, _| {},
        )
        .is_err()
    );
    assert_eq!(output, b"x");
    server.join().unwrap();
}

#[test]
#[cfg(unix)]
fn idle_viewer_renews_then_releases_its_lease() {
    let epoch = 3;
    let token = [4; 16];
    let expected = wire::lease_token_payload(epoch, token).unwrap();
    let (mut client, server) = viewer_pair(move |mut peer| {
        assert_eq!(peer.recv().kind, 1);
        peer.send(
            7,
            2,
            &wire::controller_hello_ack(7, [9; 16], b"\x01/session").unwrap(),
        );
        assert_eq!(peer.recv().kind, 3);
        peer.send(7, 5, &[0, 0]);
        peer.send(7, 4, &status(0, 0, 0, 0, 1));
        peer.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Granted,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch,
                token,
            }
            .encode_wire()
            .unwrap(),
        );
        for kind in [0x18, 0x17] {
            let request = peer.recv();
            assert_eq!(
                (request.kind, request.payload.as_ref()),
                (kind, expected.as_slice())
            );
        }
        peer.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Released,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch,
                token: [0; 16],
            }
            .encode_wire()
            .unwrap(),
        );
    });
    let mut started = false;
    let mut thread = None;
    let result = io::attach_viewer_to(
        &mut client,
        &Options::default(),
        (0, 0),
        &mut std::io::sink(),
        Duration::from_secs(15),
        |_| Err("reconnect unavailable".into()),
        |sender, state| {
            started = true;
            thread = Some(std::thread::spawn(move || {
                let base = Instant::now();
                let mut ticks = 0;
                io::run_viewer_input(
                    Cursor::new(Vec::<u8>::new()),
                    sender,
                    io::InputConfig {
                        detach: None,
                        pass_suspend: true,
                        state,
                        last_size: None,
                    },
                    || io::InputState::Closed,
                    || None,
                    || {},
                    || {
                        let now = base + Duration::from_secs(3 * ticks);
                        ticks += 1;
                        now
                    },
                );
            }));
        },
    );
    assert!(started);
    assert_eq!(result, Ok(0));
    thread.unwrap().join().unwrap();
    server.join().unwrap();
}

#[test]
#[cfg(unix)]
fn viewer_allows_only_one_input_until_its_exact_receipt() {
    let epoch = 3;
    let token = [4; 16];
    let (mut client, server) = viewer_pair(move |mut peer| {
        assert_eq!(peer.recv().kind, 1);
        peer.send(
            7,
            2,
            &wire::controller_hello_ack(7, [9; 16], b"\x01/session").unwrap(),
        );
        assert_eq!(peer.recv().kind, 3);
        peer.send(7, 5, &[0, 0]);
        peer.send(7, 4, &status(0, 0, 0, 0, 1));
        peer.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Granted,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch,
                token,
            }
            .encode_wire()
            .unwrap(),
        );
        let first = peer.recv();
        assert_eq!((first.kind, &first.payload[13..]), (9, b"a".as_slice()));
        assert!(
            peer.queued.is_empty(),
            "a second input was framed before the first receipt"
        );
        peer.stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut bytes = [0; 256];
        assert!(
            matches!(peer.stream.read(&mut bytes), Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
        );
        peer.stream.set_read_timeout(None).unwrap();
        peer.send(
            7,
            10,
            &wire::InputReceipt::outcome(epoch, 1, 7, [9; 16], 1, None)
                .encode()
                .unwrap(),
        );
        let second = peer.recv();
        assert_eq!((second.kind, &second.payload[13..]), (9, b"b".as_slice()));
        peer.send(
            7,
            10,
            &wire::InputReceipt::outcome(epoch, 2, 7, [9; 16], 1, None)
                .encode()
                .unwrap(),
        );
        assert_eq!(peer.recv().kind, 0x17);
        peer.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Released,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch,
                token: [0; 16],
            }
            .encode_wire()
            .unwrap(),
        );
    });
    let mut thread = None;
    let result = io::attach_viewer_to(
        &mut client,
        &Options::default(),
        (0, 0),
        &mut std::io::sink(),
        Duration::from_secs(15),
        |_| Err("reconnect unavailable".into()),
        |sender, state| {
            thread = Some(std::thread::spawn(move || {
                io::run_viewer_input(
                    Chunks(VecDeque::from([b"a".to_vec(), b"b".to_vec()])),
                    sender,
                    io::InputConfig {
                        detach: None,
                        pass_suspend: true,
                        state,
                        last_size: None,
                    },
                    || io::InputState::Ready,
                    || None,
                    || {},
                    Instant::now,
                );
            }));
        },
    );
    assert_eq!(result, Ok(0));
    thread.unwrap().join().unwrap();
    server.join().unwrap();
}

#[test]
#[cfg(unix)]
fn viewer_routes_terminal_query_replies_without_altering_ordinary_input() {
    let (epoch, token) = (3, [4; 16]);
    let query = wire::Query {
        correlation: 11,
        epoch,
        class: 5,
        bytes: b"\x1b[6n".to_vec(),
    };
    let expected_query = query.clone();
    let (mut client, server) = viewer_pair(move |mut peer| {
        assert_eq!(peer.recv().kind, 1);
        peer.send(
            7,
            2,
            &wire::controller_hello_ack(7, [9; 16], b"\x01/session").unwrap(),
        );
        assert_eq!(peer.recv().kind, 3);
        peer.send(7, 5, &[0, 0]);
        peer.send(7, 4, &status(1, 1, 0, 4, 1));
        peer.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Granted,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch,
                token,
            }
            .encode_wire()
            .unwrap(),
        );
        peer.send(7, 0x14, &expected_query.encode().unwrap());
        peer.send(
            7,
            6,
            &[&1_u64.to_le_bytes()[..], &0_u64.to_le_bytes(), b"\x1b[6n"].concat(),
        );
        let mut ack = false;
        let mut replied = false;
        let mut ordinary = Vec::new();
        while !ack || !replied || ordinary != b"ab" {
            let message = peer.recv();
            match message.kind {
                7 => {
                    assert_eq!(message.payload.as_ref(), 1_u64.to_le_bytes());
                    ack = true;
                }
                9 => {
                    let request = u64::from_le_bytes(message.payload[4..12].try_into().unwrap());
                    ordinary.extend_from_slice(&message.payload[13..]);
                    peer.send(
                        7,
                        10,
                        &wire::InputReceipt::outcome(
                            epoch,
                            request,
                            7,
                            [9; 16],
                            (message.payload.len() - 13) as u64,
                            None,
                        )
                        .encode()
                        .unwrap(),
                    );
                }
                12 => {
                    let reply = wire::decode_query(&message.payload).unwrap();
                    assert_eq!(
                        (reply.correlation, reply.epoch, reply.class, reply.bytes),
                        (11, epoch, 5, b"\x9b12;34R".to_vec())
                    );
                    replied = true;
                }
                kind => panic!("unexpected viewer frame {kind}"),
            }
        }
        assert_eq!(peer.recv().kind, 0x17);
        peer.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Released,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch,
                token: [0; 16],
            }
            .encode_wire()
            .unwrap(),
        );
    });
    let gate = Arc::new(Gate::default());
    let output = Arc::new(Mutex::new(Vec::new()));
    let mut sink = SignalWriter(output.clone(), gate.clone());
    let mut thread = None;
    let result = io::attach_viewer_to(
        &mut client,
        &Options::default(),
        (0, 0),
        &mut sink,
        Duration::from_secs(15),
        |_| Err("reconnect unavailable".into()),
        |sender, state| {
            let gate = gate.clone();
            thread = Some(std::thread::spawn(move || {
                io::run_viewer_input(
                    GatedReader(
                        gate,
                        VecDeque::from([b"a\x9b12".to_vec(), b";34Rb".to_vec()]),
                    ),
                    sender,
                    io::InputConfig {
                        detach: None,
                        pass_suspend: true,
                        state,
                        last_size: None,
                    },
                    || io::InputState::Ready,
                    || None,
                    || {},
                    Instant::now,
                );
            }));
        },
    );
    assert_eq!(result, Ok(0));
    thread.unwrap().join().unwrap();
    server.join().unwrap();
    assert_eq!(*output.lock().unwrap(), b"\x1b[6n");
}

#[test]
#[cfg(unix)]
fn viewer_resumes_pending_input_and_replay_without_duplicate_output() {
    let identity = b"\x01/session".to_vec();
    let (first_client, first_server) = UnixStream::pair().unwrap();
    let (second_client, second_server) = UnixStream::pair().unwrap();
    let first_reader = first_client.try_clone().unwrap();
    let server = std::thread::spawn(move || {
        let mut first = Peer::new(first_server);
        assert_eq!(first.recv().kind, 1);
        first.send(
            7,
            2,
            &wire::controller_hello_ack(7, [9; 16], b"\x01/session").unwrap(),
        );
        assert_eq!(first.recv().kind, 3);
        first.send(7, 5, &[0, 0]);
        first.send(7, 4, &status(1, 1, 0, 1, 1));
        first.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Granted,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch: 3,
                token: [4; 16],
            }
            .encode_wire()
            .unwrap(),
        );
        first.send(
            7,
            6,
            &[&1_u64.to_le_bytes()[..], &0_u64.to_le_bytes(), b"x"].concat(),
        );
        let mut saw = [false; 2];
        while !saw.into_iter().all(|value| value) {
            let message = first.recv();
            match message.kind {
                7 => {
                    assert_eq!(message.payload.as_ref(), 1_u64.to_le_bytes());
                    saw[0] = true;
                }
                9 => {
                    assert_eq!(
                        &message.payload[..13],
                        &[3_u8, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]
                    );
                    assert_eq!(&message.payload[13..], b"a");
                    saw[1] = true;
                }
                kind => panic!("unexpected initial viewer frame {kind}"),
            }
        }
        drop(first);

        let mut second = Peer::new(second_server);
        assert_eq!(second.recv().kind, 1);
        second.send(
            7,
            2,
            &wire::controller_hello_ack(7, [9; 16], b"\x01/session").unwrap(),
        );
        let resumed = second.recv();
        assert_eq!(resumed.kind, 0x15);
        assert_eq!(
            moor::session::LeaseRequest::decode_wire(&resumed.payload).unwrap(),
            moor::session::LeaseRequest {
                operation: moor::session::LeaseOperation::Resume,
                role: LeaseRole::Viewer,
                epoch: 3,
                incarnation: [9; 16],
                token: [4; 16],
            }
        );
        second.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Resumed,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch: 3,
                token: [5; 16],
            }
            .encode_wire()
            .unwrap(),
        );
        let attach = second.recv();
        assert_eq!((attach.kind, attach.payload[4] & 1), (3, 0));
        second.send(7, 5, &[0, 0]);
        second.send(7, 4, &status(1, 2, 0, 2, 1));
        second.send(
            7,
            6,
            &[&1_u64.to_le_bytes()[..], &0_u64.to_le_bytes(), b"x"].concat(),
        );
        second.send(
            7,
            6,
            &[&2_u64.to_le_bytes()[..], &1_u64.to_le_bytes(), b"y"].concat(),
        );
        let mut acks = Vec::new();
        let mut replayed = false;
        while acks.len() != 2 || !replayed {
            let message = second.recv();
            match message.kind {
                7 => acks.push(u64::from_le_bytes(
                    message.payload.as_ref().try_into().unwrap(),
                )),
                9 => {
                    assert_eq!(&message.payload[13..], b"a");
                    replayed = true;
                    second.send(
                        7,
                        10,
                        &wire::InputReceipt::outcome(3, 1, 7, [9; 16], 1, None)
                            .encode()
                            .unwrap(),
                    );
                }
                kind => panic!("unexpected resumed viewer frame {kind}"),
            }
        }
        assert_eq!(acks, [1, 2]);
        let release = second.recv();
        assert_eq!(
            (release.kind, release.payload.as_ref()),
            (
                0x17,
                wire::lease_token_payload(3, [5; 16]).unwrap().as_slice()
            )
        );
        second.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Released,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch: 3,
                token: [0; 16],
            }
            .encode_wire()
            .unwrap(),
        );
    });
    let mut client = handshake(first_reader, first_client, identity.clone()).unwrap();
    let gate = Arc::new(Gate::default());
    let output = Arc::new(Mutex::new(Vec::new()));
    let mut sink = SignalWriter(output.clone(), gate.clone());
    let mut replacement = Some(second_client);
    let mut input = None;
    let result = io::attach_viewer_to(
        &mut client,
        &Options::default(),
        (0, 0),
        &mut sink,
        Duration::from_secs(15),
        |_| {
            let stream = replacement.take().ok_or("unexpected reconnect")?;
            handshake(
                stream.try_clone().map_err(|error| error.to_string())?,
                stream,
                identity.clone(),
            )
        },
        |sender, state| {
            let gate = gate.clone();
            input = Some(std::thread::spawn(move || {
                io::run_viewer_input(
                    GatedReader(gate, VecDeque::from([b"a".to_vec()])),
                    sender,
                    io::InputConfig {
                        detach: None,
                        pass_suspend: true,
                        state,
                        last_size: None,
                    },
                    || io::InputState::Ready,
                    || None,
                    || {},
                    Instant::now,
                );
            }));
        },
    );
    assert_eq!(result, Ok(0));
    input.unwrap().join().unwrap();
    server.join().unwrap();
    assert_eq!(*output.lock().unwrap(), b"xy");
}

#[test]
#[cfg(unix)]
fn transport_loss_after_release_request_is_not_a_successful_detach() {
    let (mut client, server) = viewer_pair(|mut peer| {
        assert_eq!(peer.recv().kind, 1);
        peer.send(
            7,
            2,
            &wire::controller_hello_ack(7, [9; 16], b"\x01/session").unwrap(),
        );
        assert_eq!(peer.recv().kind, 3);
        peer.send(7, 5, &[0, 0]);
        peer.send(7, 4, &status(0, 0, 0, 0, 1));
        peer.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Granted,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch: 3,
                token: [4; 16],
            }
            .encode_wire()
            .unwrap(),
        );
        assert_eq!(peer.recv().kind, 0x17);
    });
    let mut input = None;
    let result = io::attach_viewer_to(
        &mut client,
        &Options::default(),
        (0, 0),
        &mut std::io::sink(),
        Duration::from_secs(15),
        |_| Err("reconnect refused".into()),
        |sender, state| {
            input = Some(std::thread::spawn(move || {
                io::run_viewer_input(
                    Cursor::new(Vec::<u8>::new()),
                    sender,
                    io::InputConfig {
                        detach: None,
                        pass_suspend: true,
                        state,
                        last_size: None,
                    },
                    || io::InputState::Closed,
                    || None,
                    || {},
                    Instant::now,
                );
            }));
        },
    );
    assert!(result.is_err());
    input.unwrap().join().unwrap();
    server.join().unwrap();
}

#[test]
#[cfg(unix)]
fn viewer_quiesces_commands_while_release_is_awaiting_acknowledgement() {
    let (observed, await_observed) = mpsc::channel();
    let (queued, await_queued) = mpsc::channel();
    let mut await_observed = Some(await_observed);
    let (mut client, server) = viewer_pair(move |mut peer| {
        assert_eq!(peer.recv().kind, 1);
        peer.send(
            7,
            2,
            &wire::controller_hello_ack(7, [9; 16], b"\x01/session").unwrap(),
        );
        assert_eq!(peer.recv().kind, 3);
        peer.send(7, 5, &[0, 0]);
        peer.send(7, 4, &status(0, 0, 0, 0, 1));
        peer.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Granted,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch: 3,
                token: [4; 16],
            }
            .encode_wire()
            .unwrap(),
        );
        assert_eq!(peer.recv().kind, 0x17);
        observed.send(()).unwrap();
        await_queued.recv_timeout(Duration::from_secs(1)).unwrap();
        peer.stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut bytes = [0; 256];
        assert!(matches!(
            peer.stream.read(&mut bytes),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
        ));
        peer.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Released,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch: 3,
                token: [0; 16],
            }
            .encode_wire()
            .unwrap(),
        );
    });
    let mut workers = None;
    let result = io::attach_viewer_to(
        &mut client,
        &Options::default(),
        (0, 0),
        &mut std::io::sink(),
        Duration::from_secs(15),
        |_| Err("reconnect unavailable".into()),
        |sender, _| {
            let await_observed = await_observed.take().unwrap();
            let sender = Arc::new(sender);
            let release = Arc::clone(&sender);
            let queued = queued.clone();
            workers = Some((
                std::thread::spawn(move || release.release()),
                std::thread::spawn(move || {
                    await_observed.recv().unwrap();
                    let sent = sender.send(b"late");
                    queued.send(()).unwrap();
                    sent
                }),
            ));
        },
    );
    assert_eq!(result, Ok(0));
    let (release, late) = workers.unwrap();
    assert!(release.join().unwrap());
    assert!(late.join().unwrap());
    server.join().unwrap();
}

#[test]
#[cfg(unix)]
fn wakeups_do_not_postpone_the_viewer_heartbeat_deadline() {
    let (stream, server) = UnixStream::pair().unwrap();
    let reader = stream.try_clone().unwrap();
    let close = stream.try_clone().unwrap();
    let server = std::thread::spawn(move || {
        let mut peer = Peer::new(server);
        assert_eq!(peer.recv().kind, 1);
        peer.send(
            7,
            2,
            &wire::controller_hello_ack(7, [9; 16], b"\x01/session").unwrap(),
        );
        assert_eq!(peer.recv().kind, 3);
        peer.send(7, 5, &[0, 0]);
        peer.send(7, 4, &status(0, 0, 0, 0, 1));
        peer.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Granted,
                reason: ResultReason::None,
                role: LeaseRole::Viewer,
                epoch: 3,
                token: [4; 16],
            }
            .encode_wire()
            .unwrap(),
        );
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(5));
            peer.send(7, 0x11, &[]);
        }
        std::thread::sleep(Duration::from_millis(100));
    });
    let mut client = handshake(reader, stream, b"\x01/session".to_vec()).unwrap();
    client.set_cancel(move || {
        let _ = close.shutdown(std::net::Shutdown::Both);
    });
    let started = Instant::now();
    let probed = Arc::new(Mutex::new(None));
    let observed = probed.clone();
    let result = io::attach_viewer_to(
        &mut client,
        &Options::default(),
        (0, 0),
        &mut Vec::new(),
        Duration::from_millis(30),
        |_| {
            observed
                .lock()
                .unwrap()
                .get_or_insert_with(|| started.elapsed());
            Err("probe indeterminate".into())
        },
        |_, _| {},
    );
    assert!(result.is_err());
    assert!(probed.lock().unwrap().unwrap() < Duration::from_millis(80));
    server.join().unwrap();
}

impl Write for SlowWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = self.started.send(());
        let _ = self.release.recv();
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn reader_emits_bytes_then_closed() {
    let pump = duplex(Cursor::new(b"hello".to_vec()), std::io::sink(), 16);
    assert_eq!(
        pump.1.recv_timeout(Duration::from_secs(1)).unwrap(),
        Event::Bytes(b"hello".to_vec())
    );
    assert_eq!(
        pump.1.recv_timeout(Duration::from_secs(1)).unwrap(),
        Event::Closed
    );
}

#[test]
fn unread_input_is_backpressured_to_a_bounded_burst() {
    let reads = Arc::new(AtomicUsize::new(0));
    let pump = duplex(BurstReader(reads.clone()), std::io::sink(), 16);
    let deadline = Instant::now() + Duration::from_secs(1);
    while reads.load(Ordering::Relaxed) < 9 && Instant::now() < deadline {
        std::thread::yield_now()
    }
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(reads.load(Ordering::Relaxed), 9);
    assert!(matches!(pump.1.recv().unwrap(), Event::Bytes(_)));
}

#[test]
fn tracked_writer_reports_completed_bytes() {
    let read_gate = Arc::new(Gate::default());
    let (pump, completed) = Duplex::tracked(BlockingReader(read_gate.clone()), std::io::sink(), 16);
    pump.try_send(b"hello".to_vec()).unwrap();
    assert_eq!(
        completed.recv_timeout(Duration::from_secs(1)).unwrap(),
        (5, None)
    );
    pump.shutdown();
    read_gate.open();
}

#[test]
fn tracked_writer_reports_progress_before_failure() {
    let read_gate = Arc::new(Gate::default());
    let (pump, completed) =
        Duplex::tracked(BlockingReader(read_gate.clone()), PartialFail(true), 16);
    pump.try_send(b"hello".to_vec()).unwrap();
    assert_eq!(
        completed.recv_timeout(Duration::from_secs(1)).unwrap(),
        (2, Some(20))
    );
    assert_eq!(pump.pending(), 0);
    assert_eq!(pump.try_send(vec![1]), Err(SendError::Closed));
    assert_eq!(pump.try_send(Vec::new()), Err(SendError::Closed));
    read_gate.open();
}

#[test]
fn blocked_writer_keeps_bytes_charged_to_limit() {
    let read_gate = Arc::new(Gate::default());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let written = Arc::new(Mutex::new(Vec::new()));
    let pump = duplex(
        BlockingReader(read_gate.clone()),
        SlowWriter {
            started: started_tx,
            release: release_rx,
            bytes: written.clone(),
        },
        4,
    );

    assert_eq!(pump.try_send(vec![1, 2, 3, 4]), Ok(()));
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(pump.try_send(vec![5]), Err(SendError::Full));
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while pump.try_send(vec![5]) == Err(SendError::Full) && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(*written.lock().unwrap(), vec![1, 2, 3, 4]);
    pump.shutdown();
    read_gate.open();
}

#[test]
fn viewer_payload_and_control_overhead_have_independent_limits() {
    let read_gate = Arc::new(Gate::default());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let pump = duplex(
        BlockingReader(read_gate.clone()),
        SlowWriter {
            started: started_tx,
            release: release_rx,
            bytes: Arc::new(Mutex::new(Vec::new())),
        },
        1,
    );
    assert_eq!(
        pump.try_send_payload(vec![0; (4 << 20) + 1], 4 << 20),
        Ok(())
    );
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(pump.try_send_payload(vec![0], 1), Err(SendError::Full));
    assert_eq!(pump.try_send(vec![0]), Err(SendError::Full));
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while pump.pending() != 0 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(pump.pending(), 0);
    pump.shutdown();
    read_gate.open();
}

#[test]
fn shutdown_and_drop_do_not_wait_for_blocked_io() {
    let read_gate = Arc::new(Gate::default());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let written = Arc::new(Mutex::new(Vec::new()));
    let pump = duplex(
        BlockingReader(read_gate.clone()),
        SlowWriter {
            started: started_tx,
            release: release_rx,
            bytes: written.clone(),
        },
        1,
    );
    pump.try_send(vec![1]).unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

    let started = Instant::now();
    pump.shutdown();
    assert_eq!(pump.try_send(vec![2]), Err(SendError::Closed));
    drop(pump);
    assert!(started.elapsed() < Duration::from_millis(100));
    release_tx.send(()).unwrap();
    read_gate.open();
    let deadline = Instant::now() + Duration::from_secs(1);
    while written.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(
        *written.lock().unwrap(),
        vec![1],
        "shutdown must drain accepted output before cancellation"
    );
}

#[test]
fn concurrent_shutdown_drains_every_accepted_send() {
    const SENDERS: usize = 32;
    let read_gate = Arc::new(Gate::default());
    let (pump, completed) =
        Duplex::tracked(BlockingReader(read_gate.clone()), std::io::sink(), SENDERS);
    let pump = Arc::new(pump);
    let start = Arc::new(Barrier::new(SENDERS + 2));
    let sends = (0..SENDERS)
        .map(|byte| {
            let pump = pump.clone();
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                pump.try_send(vec![byte as u8])
            })
        })
        .collect::<Vec<_>>();
    let close = {
        let pump = pump.clone();
        let start = start.clone();
        std::thread::spawn(move || {
            start.wait();
            pump.shutdown();
        })
    };

    start.wait();
    let outcomes = sends
        .into_iter()
        .map(|send| send.join().unwrap())
        .collect::<Vec<_>>();
    close.join().unwrap();
    let accepted = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, Ok(()) | Err(SendError::Closed)))
    );
    for _ in 0..accepted {
        assert_eq!(
            completed.recv_timeout(Duration::from_secs(1)).unwrap(),
            (1, None)
        );
    }
    assert_eq!(pump.try_send(vec![0]), Err(SendError::Closed));
    read_gate.open();
}
