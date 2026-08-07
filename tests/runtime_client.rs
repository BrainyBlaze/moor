#![cfg(unix)]

use moor::runtime::client::{Client, decode_clear_result};
use moor::runtime::io::attach_viewer_to;
use moor::session::{LeaseRequest, LeaseResult, LeaseRole, ResultOutcome, ResultReason};
use moor::{store, wire};
use std::collections::VecDeque;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};
use wire::Profile;

fn validate_input_receipt(payload: &[u8], expected: wire::InputReceipt) -> Result<(), String> {
    let receipt = wire::InputReceipt::decode(payload).map_err(|error| format!("{error:?}"))?;
    (receipt == expected)
        .then_some(())
        .ok_or_else(|| "input was not delivered".into())
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

struct Chunked(Cursor<Vec<u8>>);

impl Read for Chunked {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let mut byte = [0];
        let count = self.0.read(&mut byte)?;
        output[..count].copy_from_slice(&byte[..count]);
        Ok(count)
    }
}

fn ack(generation: u32, incarnation: [u8; 16], identity: &[u8]) -> Vec<u8> {
    wire::controller_hello_ack(generation, incarnation, identity).unwrap()
}

struct Peer {
    stream: UnixStream,
    inbound: wire::Codec,
    outbound: wire::Codec,
    queued: VecDeque<wire::Message>,
}

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

#[test]
fn controller_handshake_validates_scope_generation_incarnation_and_identity() {
    let (client, server) = UnixStream::pair().unwrap();
    let reader = client.try_clone().unwrap();
    let identity = b"\x01/tmp/session".to_vec();
    let expected = identity.clone();
    let server = thread::spawn(move || {
        let mut peer = Peer::new(server);
        let hello = peer.recv();
        assert_eq!((hello.scope, hello.kind), (0, 1));
        assert!(hello.payload.ends_with(&expected));
        peer.send(7, 2, &ack(7, [9; 16], &expected));
        let status = peer.recv();
        assert_eq!((status.scope, status.kind), (7, 13));
        peer.send(7, 14, &[]);
    });
    let mut client = handshake(reader, client, identity).unwrap();
    assert_eq!((client.generation, client.incarnation), (7, [9; 16]));
    assert_eq!(client.identity, b"\x01/tmp/session");
    client.send(13, &[]).unwrap();
    assert_eq!(client.recv().unwrap().kind, 14);
    server.join().unwrap();
}

#[test]
fn controller_owns_fragmented_reader_and_reports_disconnect() {
    let identity = b"session".to_vec();
    let mut inbound = Vec::new();
    wire::Codec::new(Profile::Controller)
        .encode(7, 2, &ack(7, [9; 16], &identity), &mut inbound)
        .unwrap();
    let mut client: Client =
        handshake(Chunked(Cursor::new(inbound)), Vec::new(), identity).unwrap();
    assert_eq!(client.recv(), Err("connection closed".into()));
}

#[test]
fn controller_preserves_multiple_frame_order_from_one_read() {
    let identity = b"session".to_vec();
    let mut inbound = Vec::new();
    let mut codec = wire::Codec::new(Profile::Controller);
    codec
        .encode(7, 2, &ack(7, [9; 16], &identity), &mut inbound)
        .unwrap();
    codec.encode(7, 13, &[], &mut inbound).unwrap();
    codec.encode(7, 14, &[], &mut inbound).unwrap();
    let mut client = handshake(Cursor::new(inbound), Vec::new(), identity).unwrap();
    assert_eq!(
        (client.recv().unwrap().kind, client.recv().unwrap().kind),
        (13, 14)
    );
}

#[test]
fn malformed_ack_is_rejected() {
    let (client, server) = UnixStream::pair().unwrap();
    let reader = client.try_clone().unwrap();
    thread::spawn(move || {
        let mut peer = Peer::new(server);
        peer.recv();
        peer.send(7, 2, &ack(8, [9; 16], b"wrong"));
    });
    assert!(handshake(reader, client, b"session".to_vec()).is_err());
}

#[test]
fn handshake_has_one_absolute_deadline_across_fragmented_reads() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let reader = client.try_clone().unwrap();
    let close = client.try_clone().unwrap();
    thread::spawn(move || {
        let mut peer = Peer::new(server.try_clone().unwrap());
        peer.recv();
        let mut bytes = Vec::new();
        wire::Codec::new(Profile::Controller)
            .encode(7, 2, &ack(7, [9; 16], b"session"), &mut bytes)
            .unwrap();
        for byte in bytes {
            if server.write_all(&[byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
    let before = Instant::now();
    assert!(
        Client::handshake_until(
            reader,
            client,
            b"session".to_vec(),
            before + Duration::from_millis(100),
            move || {
                let _ = close.shutdown(std::net::Shutdown::Both);
            }
        )
        .is_err()
    );
    assert!(before.elapsed() < Duration::from_millis(300));
}

#[test]
fn client_drop_runs_transport_cancellation_once() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    let identity = b"session".to_vec();
    let mut inbound = Vec::new();
    wire::Codec::new(Profile::Controller)
        .encode(7, 2, &ack(7, [9; 16], &identity), &mut inbound)
        .unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = Dropped(dropped.clone());
    let client = Client::handshake_until(
        Cursor::new(inbound),
        Vec::new(),
        identity,
        Instant::now() + Duration::from_secs(2),
        move || drop(guard),
    )
    .unwrap();
    assert!(!dropped.load(Ordering::Acquire));
    drop(client);
    let deadline = Instant::now() + Duration::from_millis(100);
    while !dropped.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(dropped.load(Ordering::Acquire));
}

#[test]
fn client_delivers_valid_frames_before_a_trailing_codec_failure() {
    let identity = b"session".to_vec();
    let mut inbound = Vec::new();
    let mut codec = wire::Codec::new(Profile::Controller);
    codec
        .encode(7, 2, &ack(7, [9; 16], &identity), &mut inbound)
        .unwrap();
    codec
        .encode(
            7,
            0x12,
            &wire::Heartbeat {
                monotonic_ms: 9,
                flags: 1,
            }
            .encode()
            .unwrap(),
            &mut inbound,
        )
        .unwrap();
    let bad = inbound.len();
    codec
        .encode(
            7,
            0x12,
            &wire::Heartbeat {
                monotonic_ms: 10,
                flags: 1,
            }
            .encode()
            .unwrap(),
            &mut inbound,
        )
        .unwrap();
    inbound[bad] ^= 1;
    let mut client = handshake(Cursor::new(inbound), Vec::new(), identity).unwrap();
    assert_eq!(client.recv().unwrap().kind, 0x12);
    assert!(client.recv().is_err());
}

#[test]
fn controller_error_is_reported_without_waiting_for_an_unrelated_reply() {
    let identity = b"session".to_vec();
    let mut inbound = Vec::new();
    let mut codec = wire::Codec::new(Profile::Controller);
    codec
        .encode(7, 2, &ack(7, [9; 16], &identity), &mut inbound)
        .unwrap();
    // Built through the holder's own reply encoder rather than hand-assembled,
    // so the frame type and prefix width are the ones a real holder emits. A
    // hand-written 0x13 frame would pass even while the holder emitted 0x0D,
    // which is exactly how N1 stayed invisible.
    let moor::wire::RuntimeReply::Frame(kind, refusal) = wire::encode_reply(
        moor::session::Reply::ControllerError(15, b"lease not held"),
        [9; 16],
    ) else {
        panic!("a controller refusal is an unscoped frame");
    };
    assert_eq!(kind, 0x13, "schema §2 assigns ERROR the type byte 0x13");
    codec.encode(7, kind, &refusal, &mut inbound).unwrap();
    let mut client = handshake(Cursor::new(inbound), Vec::new(), identity).unwrap();
    let error = client.receive_kind(0x16).unwrap_err();
    assert!(
        error.contains("15") && error.contains("lease not held"),
        "{error}"
    );
}

fn clear_result(outcome: u8, reason: u8, prior_delta: u64) -> Result<(), String> {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "moor-client-clear-{}-{}-{outcome}-{reason}-{sequence}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let log = root.join("session.log");
    drop(store::Store::create(&log, store::Kind::Log, 7, b"output", 0, 6).unwrap());
    let (client, server) = UnixStream::pair().unwrap();
    let reader = client.try_clone().unwrap();
    let server = thread::spawn(move || {
        let mut peer = Peer::new(server);
        let hello = peer.recv();
        peer.send(7, 2, &ack(7, [9; 16], b"session"));
        let clear = peer.recv();
        assert_eq!((hello.kind, clear.kind), (1, 0x19));
        let observed = u64::from_le_bytes(clear.payload[16..].try_into().unwrap());
        let payload = wire::log_clear_result_payload(
            outcome,
            reason,
            8,
            observed + prior_delta,
            observed + 1,
            6,
        )
        .unwrap();
        peer.send(7, 0x1a, &payload);
    });
    let mut client = handshake(reader, client, b"session".to_vec()).unwrap();
    let result = client.clear_log(&log);
    server.join().unwrap();
    fs::remove_dir_all(root).unwrap();
    result
}

#[test]
fn log_clear_accepts_only_success_outcomes_with_no_reason() {
    assert_eq!(clear_result(0, 0, 0), Ok(()));
    assert_eq!(clear_result(1, 0, 0), Ok(()));
    assert_eq!(
        clear_result(2, 1, 0),
        Err("log changed before it could be cleared".into())
    );
    assert_eq!(
        clear_result(2, 2, 0),
        Err("log store is unavailable".into())
    );
    assert_eq!(clear_result(2, 3, 0), Err("log store is corrupt".into()));
}

#[test]
fn log_clear_rejects_a_result_for_another_observed_index() {
    assert_eq!(
        clear_result(0, 0, 1),
        Err("log clear result did not match the request".into())
    );
}

#[test]
fn malformed_log_clear_result_fails_closed() {
    assert_eq!(
        decode_clear_result(&[0; 8], 1),
        Err("invalid log clear result".into())
    );
}

#[test]
fn push_accepts_only_the_exact_success_receipt() {
    let expected = wire::InputReceipt::outcome(3, 9, 7, [4; 16], 5, None);
    let bytes = expected.encode().unwrap();
    assert!(validate_input_receipt(&bytes, expected).is_ok());
    for changed in [
        wire::InputReceipt {
            request: 8,
            ..expected
        },
        wire::InputReceipt {
            incarnation: [5; 16],
            ..expected
        },
        wire::InputReceipt {
            written: 4,
            status: 1,
            result: 20,
            ..expected
        },
    ] {
        assert!(validate_input_receipt(&changed.encode().unwrap(), expected).is_err());
    }
}

#[test]
fn push_reconnects_resumes_and_replays_an_unanswered_input() {
    let identity = b"session".to_vec();
    let incarnation = [9; 16];
    let first_token = [1; 16];
    let second_token = [2; 16];
    let (first_client, first_server) = UnixStream::pair().unwrap();
    let first_reader = first_client.try_clone().unwrap();
    let (second_client, second_server) = UnixStream::pair().unwrap();
    let second_reader = second_client.try_clone().unwrap();
    let expected = identity.clone();
    let server = thread::spawn(move || {
        let mut first = Peer::new(first_server);
        first.recv();
        first.send(7, 2, &ack(7, incarnation, &expected));
        assert_eq!(
            LeaseRequest::decode_wire(&first.recv().payload).unwrap(),
            LeaseRequest::fresh(LeaseRole::InputOnly)
        );
        first.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Granted,
                reason: ResultReason::None,
                role: LeaseRole::InputOnly,
                epoch: 3,
                token: first_token,
            }
            .encode_wire()
            .unwrap(),
        );
        let unanswered = first.recv();
        assert_eq!(
            (unanswered.kind, &unanswered.payload[13..]),
            (9, b"hello".as_slice())
        );
        drop(first);

        let mut second = Peer::new(second_server);
        second.recv();
        second.send(7, 2, &ack(7, incarnation, &expected));
        let resume = LeaseRequest::decode_wire(&second.recv().payload).unwrap();
        assert_eq!(
            (resume.epoch, resume.incarnation, resume.token),
            (3, incarnation, first_token)
        );
        second.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Resumed,
                reason: ResultReason::None,
                role: LeaseRole::InputOnly,
                epoch: 3,
                token: second_token,
            }
            .encode_wire()
            .unwrap(),
        );
        let replay = second.recv();
        assert_eq!(replay.payload, unanswered.payload);
        let receipt = wire::InputReceipt {
            epoch: 3,
            request: 1,
            generation: 7,
            incarnation,
            written: 5,
            status: 0,
            result: 0,
        };
        second.send(7, 10, &receipt.encode().unwrap());
        let release = second.recv();
        assert_eq!(&release.payload[4..], second_token.as_slice());
        second.send(
            7,
            0x16,
            &LeaseResult {
                outcome: ResultOutcome::Released,
                reason: ResultReason::None,
                role: LeaseRole::InputOnly,
                epoch: 3,
                token: [0; 16],
            }
            .encode_wire()
            .unwrap(),
        );
    });
    let client = handshake(first_reader, first_client, identity.clone()).unwrap();
    let mut next = Some((second_reader, second_client));
    assert_eq!(
        client.push_from(Cursor::new(b"hello".to_vec()), |_| {
            let (reader, writer) = next.take().unwrap();
            handshake(reader, writer, identity.clone())
        }),
        Ok(0)
    );
    server.join().unwrap();
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

#[test]
fn attach_fences_gap_duplicates_offsets_and_empty_output() {
    let identity = b"\x01/session".to_vec();
    let mut inbound = Vec::new();
    let mut codec = wire::Codec::new(Profile::Controller);
    codec
        .encode(7, 2, &ack(7, [9; 16], &identity), &mut inbound)
        .unwrap();
    codec.encode(7, 5, &[1, 0, b'P'], &mut inbound).unwrap();
    codec
        .encode(7, 4, &status(2, 3, 4, 8, 2), &mut inbound)
        .unwrap();
    let gap = [&1u64.to_le_bytes()[..], &1u64.to_le_bytes()].concat();
    codec.encode(7, 8, &gap, &mut inbound).unwrap();
    let first = [&2u64.to_le_bytes()[..], &4u64.to_le_bytes(), b"aa"].concat();
    codec.encode(7, 6, &first, &mut inbound).unwrap();
    codec.encode(7, 6, &first, &mut inbound).unwrap();
    let overlap = [&3u64.to_le_bytes()[..], &5u64.to_le_bytes(), b"bb"].concat();
    codec.encode(7, 6, &overlap, &mut inbound).unwrap();
    let mut client = handshake(Cursor::new(inbound), Vec::new(), identity).unwrap();
    let mut output = Vec::new();
    assert!(
        attach_viewer_to(
            &mut client,
            &moor::cli::Options::default(),
            (0, 0),
            &mut output,
            Duration::from_secs(15),
            |_| Err("reconnect unavailable".into()),
            |_| {},
        )
        .is_err()
    );
    assert_eq!(output, b"Paa");
}
