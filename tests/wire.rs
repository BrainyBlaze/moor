use moor::session::{
    LeaseOperation, LeaseRequest, LeaseResult, LeaseRole, Request as PolicyRequest, ResultOutcome,
    ResultReason,
};
use moor::wire::{
    Codec, ControllerRequest, Heartbeat, InputReceipt, Message, Profile, Query, ReplayDescriptor,
    StatusExtension, StatusTail, ViewerEvent, ViewerStream, WireError, controller_hello,
    controller_hello_ack, crc32c, decode_controller, decode_controller_hello_ack,
    decode_log_clear_result, decode_query, decode_semantic, decode_terminate_result, decode_viewer,
    get_wide, input_payload, lease_token_payload, log_clear_payload, log_clear_result_payload,
    put_compact, put_wide, resize_payload, terminate_request_payload, terminate_result_payload,
    validate_status_flags,
};

fn progressed_codec(profile: Profile, next_in: u32, next_out: u32) -> Codec {
    assert!(next_in != 0 && next_out != 0);
    let mut codec = Codec::new(profile);
    if next_in > 1 {
        let mut sender = Codec::new(profile);
        let mut frames = Vec::new();
        for _ in 1..next_in {
            sender
                .encode(1, 1, &[], &mut frames)
                .expect("sequence fixture frame must encode");
        }
        let mut discarded = Vec::new();
        codec
            .feed(0, &frames, &mut discarded)
            .expect("sequence fixture frames must decode");
        assert_eq!(discarded.len(), (next_in - 1) as usize);
    }
    let mut discarded = Vec::new();
    for _ in 1..next_out {
        codec
            .encode(1, 1, &[], &mut discarded)
            .expect("sequence fixture frame must encode");
        discarded.clear();
    }
    codec
}

#[test]
fn viewer_decoder_types_borrowed_stream_records_and_rejects_bad_boundaries() {
    let mut stream = ViewerStream::default();
    let terminal = Message {
        scope: 7,
        kind: 5,
        payload: [1, 0, b'x'].as_slice().into(),
    };
    assert!(matches!(
        decode_viewer(&mut stream, &terminal, (b"session", 7, [9; 16])),
        Ok(Some(ViewerEvent::Terminal(b"x")))
    ));
    let malformed = Message {
        scope: 7,
        kind: 5,
        payload: [2, 0, b'x'].as_slice().into(),
    };
    assert_eq!(
        decode_viewer(&mut stream, &malformed, (b"session", 7, [9; 16])),
        Err(WireError::Malformed)
    );
}

#[test]
fn viewer_decoder_accepts_contiguous_live_output_after_the_frozen_baseline() {
    let expected = (b"\x01/tmp/session".as_slice(), 1, [1; 16]);
    let mut stream = ViewerStream::default();
    let terminal = Message {
        scope: 1,
        kind: 5,
        payload: [0, 0].as_slice().into(),
    };
    assert!(decode_viewer(&mut stream, &terminal, expected).is_ok());

    let tail = StatusTail {
        replay: ReplayDescriptor {
            first: 0,
            last: 0,
            start: 0,
            end: 0,
            complete: true,
            modes_exact: true,
        },
        owns_lease: false,
        viewers: true,
        running: true,
        event_writable: true,
        lease_epoch: 0,
        semantic_flags: 0,
        semantic_pending: 0,
        extension: StatusExtension {
            health: 0,
            log_epoch: 0,
            log_index: 0,
            retained_start: 0,
            retained_end: 0,
        },
    };
    let mut payload = status_prefix();
    payload.extend(tail.encode().unwrap());
    let ack = Message {
        scope: 1,
        kind: 4,
        payload: payload.as_slice().into(),
    };
    assert_eq!(decode_viewer(&mut stream, &ack, expected), Ok(None));

    for (sequence, offset, bytes) in [(1_u64, 0_u64, b"a".as_slice()), (2, 1, b"bc".as_slice())] {
        let mut payload = Vec::from(sequence.to_le_bytes());
        payload.extend_from_slice(&offset.to_le_bytes());
        payload.extend_from_slice(bytes);
        let output = Message {
            scope: 1,
            kind: 6,
            payload: payload.as_slice().into(),
        };
        assert_eq!(
            decode_viewer(&mut stream, &output, expected),
            Ok(Some(ViewerEvent::Output(sequence, true, bytes)))
        );
    }

    let mut payload = Vec::from(4_u64.to_le_bytes());
    payload.extend_from_slice(&3_u64.to_le_bytes());
    payload.push(b'x');
    let skipped = Message {
        scope: 1,
        kind: 6,
        payload: payload.as_slice().into(),
    };
    assert_eq!(
        decode_viewer(&mut stream, &skipped, expected),
        Err(WireError::Malformed)
    );
}

#[test]
fn viewer_decoder_requires_the_new_connection_to_receive_its_frozen_baseline() {
    let expected = (b"\x01/tmp/session".as_slice(), 1, [1; 16]);
    let mut stream = ViewerStream {
        next: Some((4, 3)),
        ..ViewerStream::default()
    };
    let terminal = Message {
        scope: 1,
        kind: 5,
        payload: [0, 0].as_slice().into(),
    };
    assert!(decode_viewer(&mut stream, &terminal, expected).is_ok());

    let mut payload = status_prefix();
    payload.extend(
        StatusTail {
            replay: ReplayDescriptor {
                first: 1,
                last: 3,
                start: 0,
                end: 3,
                complete: true,
                modes_exact: true,
            },
            owns_lease: false,
            viewers: true,
            running: true,
            event_writable: true,
            lease_epoch: 0,
            semantic_flags: 0,
            semantic_pending: 0,
            extension: StatusExtension {
                health: 0,
                log_epoch: 0,
                log_index: 0,
                retained_start: 0,
                retained_end: 0,
            },
        }
        .encode()
        .unwrap(),
    );
    let status = Message {
        scope: 1,
        kind: 4,
        payload: payload.as_slice().into(),
    };
    assert_eq!(decode_viewer(&mut stream, &status, expected), Ok(None));

    let live_payload = [&4_u64.to_le_bytes()[..], &3_u64.to_le_bytes(), b"x"].concat();
    let live = Message {
        scope: 1,
        kind: 6,
        payload: live_payload.as_slice().into(),
    };
    assert_eq!(
        decode_viewer(&mut stream, &live, expected),
        Err(WireError::Malformed)
    );
}

#[test]
fn input_receipts_round_trip_exact_identity_and_outcome() {
    let written = InputReceipt {
        epoch: 2,
        request: 3,
        generation: 4,
        incarnation: [5; 16],
        written: 6,
        status: 0,
        result: 0,
    };
    let bytes = written.encode().unwrap();
    assert_eq!(bytes.len(), 43);
    assert_eq!(InputReceipt::decode(&bytes), Ok(written));

    let refused = InputReceipt {
        status: 1,
        result: 15,
        ..written
    };
    assert_eq!(
        InputReceipt::decode(&refused.encode().unwrap()),
        Ok(refused)
    );
    for invalid in [
        InputReceipt {
            status: 0,
            result: 15,
            ..written
        },
        InputReceipt {
            status: 1,
            result: 0,
            ..written
        },
        InputReceipt {
            status: 2,
            result: 15,
            ..written
        },
        InputReceipt {
            epoch: 0,
            ..written
        },
        InputReceipt {
            request: 0,
            ..written
        },
        InputReceipt {
            generation: 0,
            ..written
        },
        InputReceipt {
            incarnation: [0; 16],
            ..written
        },
        InputReceipt {
            status: 1,
            result: 21,
            ..written
        },
    ] {
        assert_eq!(invalid.encode(), Err(WireError::Malformed));
    }
    assert_eq!(
        InputReceipt::decode(&bytes[..42]),
        Err(WireError::Malformed)
    );
}

fn hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

fn exact_payload(kind: u8, size: usize) -> Vec<u8> {
    let mut payload = vec![0; size];
    match kind {
        0x16 => {
            payload[4..8].copy_from_slice(&1u32.to_le_bytes());
            payload[8..].fill(1);
        }
        0x17 | 0x18 => {
            payload[..4].copy_from_slice(&1u32.to_le_bytes());
            payload[4..].fill(1);
        }
        0x19 => payload[..16].fill(1),
        0x1a => payload[0] = 1,
        _ => {}
    }
    payload
}

fn status_prefix() -> Vec<u8> {
    let mut payload = Vec::new();
    put_wide(&mut payload, b"\x01/tmp/session").unwrap();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&[1; 16]);
    payload.push(0);
    put_wide(&mut payload, b"").unwrap();
    payload.push(0xff);
    payload.extend_from_slice(&[0; 48 + 32]);
    put_wide(&mut payload, b"/tmp").unwrap();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&[1; 16]);
    payload
}

fn decode_status(payload: &[u8]) -> Result<StatusTail, WireError> {
    StatusTail::decode_for(payload, b"\x01/tmp/session", 1, [1; 16])
}

const V1: &str = "
4D 4F 4F 52 03 01 00 00 07 00 00 00 01 00 00 00
21 00 00 00 26 04 0D F1 4D 4F 4F 52 03 00 00 16
00 00 00 01 2F 74 6D 70 2F 2E 6D 6F 6F 72 2D 31
30 30 30 2F 62 75 69 6C 64";

const V7: &str = "
4D 4F 4F 52 03 09 01 00 07 00 00 00 14 00 00 00
11 00 00 00 33 71 5F 45 03 00 00 00 01 00 00 00
00 00 00 00 00 41 41 41 41 4D 4F 4F 52 03 09 00
00 07 00 00 00 15 00 00 00 02 00 00 00 56 61 22
D3 42 42";

const V14: &str = "
4D 4F 4F 53 01 01 00 00 00 00 00 00 01 00 00 00
2E 00 00 00 C8 42 0C 36 00 01 02 03 04 05 06 07
08 09 0A 0B 0C 0D 0E 0F 10 11 12 13 14 15 16 17
18 19 1A 1B 1C 1D 1E 1F 07 00 00 00 01 07 06 00
63 6C 61 75 64 65";

#[test]
fn controller_identity_payloads_share_one_exact_codec() {
    let identity = b"\x01/tmp/session";
    let hello = controller_hello(identity).unwrap();
    assert!(matches!(
        decode_controller(1, &hello, None),
        Ok(ControllerRequest::Hello(found)) if found == identity
    ));

    let incarnation = [7; 16];
    let ack = controller_hello_ack(42, incarnation, identity).unwrap();
    assert_eq!(
        decode_controller_hello_ack(42, &ack, identity),
        Some((42, incarnation))
    );
    assert_eq!(decode_controller_hello_ack(41, &ack, identity), None);
    assert_eq!(decode_controller_hello_ack(42, &ack, b"another"), None);
    assert!(controller_hello_ack(0, incarnation, identity).is_err());
    assert!(controller_hello_ack(42, [0; 16], identity).is_err());
    let zero = controller_hello_ack(42, incarnation, identity).unwrap();
    assert_eq!(
        decode_controller_hello_ack(42, &[&zero[..5], &[0; 16], &zero[21..]].concat(), identity),
        None
    );
    let mut zero_generation = ack;
    zero_generation[1..5].fill(0);
    assert_eq!(
        decode_controller_hello_ack(0, &zero_generation, identity),
        None
    );
}

#[test]
fn termination_results_encode_all_five_outcomes_and_reject_reserved_shapes() {
    for (outcome, method, diagnostic) in [
        (0, 1, &b""[..]),
        (1, 0, b""),
        (2, 0, b"identity"),
        (3, 2, b"timeout"),
        (4, 1, b"failed"),
    ] {
        let bytes = terminate_result_payload(outcome, 5, method, diagnostic).unwrap();
        assert_eq!(
            decode_terminate_result(&bytes).unwrap(),
            (outcome, 5, method, diagnostic)
        );
    }
    assert!(terminate_result_payload(5, 0, 0, b"bad").is_err());
    assert!(terminate_result_payload(0, 0x10, 1, b"").is_err());
    assert!(terminate_result_payload(0, 0, 3, b"").is_err());
    assert!(terminate_result_payload(0, 0, 1, b"not empty").is_err());
    assert!(terminate_result_payload(4, 0, 1, b"").is_err());
}

#[test]
fn status_tail_round_trips_replay_and_health_as_one_shape() {
    let mut payload = status_prefix();
    let tail = StatusTail {
        replay: ReplayDescriptor {
            first: 2,
            last: 3,
            start: 4,
            end: 8,
            complete: false,
            modes_exact: true,
        },
        owns_lease: true,
        viewers: true,
        running: true,
        event_writable: false,
        lease_epoch: 7,
        semantic_flags: 3,
        semantic_pending: 2,
        extension: StatusExtension {
            health: 0,
            log_epoch: 0,
            log_index: 0,
            retained_start: 0,
            retained_end: 0,
        },
    };
    payload.extend_from_slice(&tail.encode().unwrap());
    assert_eq!(decode_status(&payload).unwrap(), tail);
    assert_eq!(
        StatusTail::decode_for(&payload, b"\x01/tmp/session", 1, [1; 16]).unwrap(),
        tail
    );
    assert_eq!(
        StatusTail::decode_for(&payload, b"\x01/other", 1, [1; 16]),
        Err(WireError::Malformed)
    );
    assert_eq!(
        StatusTail::decode_for(&payload, b"\x01/tmp/session", 2, [1; 16]),
        Err(WireError::Malformed)
    );

    for range in [17..21, 21..37] {
        let mut malformed = payload.clone();
        malformed[range].fill(0);
        assert_eq!(decode_status(&malformed), Err(WireError::Malformed));
    }
    let mut malformed = payload.clone();
    malformed[37] = 3;
    assert_eq!(decode_status(&malformed), Err(WireError::Malformed));
    malformed = payload;
    malformed[42] = 0;
    assert_eq!(decode_status(&malformed), Err(WireError::Malformed));
}

#[test]
fn frozen_controller_vector_decodes_at_every_boundary_and_encodes_exactly() {
    let bytes = hex(V1);
    for split in 0..=bytes.len() {
        let mut codec = Codec::new(Profile::Controller);
        let mut out = Vec::new();
        codec.feed(0, &bytes[..split], &mut out).unwrap();
        codec.feed(1, &bytes[split..], &mut out).unwrap();
        assert_eq!(out.len(), 1, "split {split}");
        assert_eq!((out[0].scope, out[0].kind), (7, 1));
        let mut encoded = Vec::new();
        Codec::new(Profile::Controller)
            .encode(7, 1, &out[0].payload, &mut encoded)
            .unwrap();
        assert_eq!(encoded, bytes);
    }
}

#[test]
fn fragmented_input_reassembles_at_every_transport_split() {
    let bytes = hex(V7);
    for split in 0..=bytes.len() {
        let mut codec = progressed_codec(Profile::Controller, 20, 1);
        let mut out = Vec::new();
        codec.feed(0, &bytes[..split], &mut out).unwrap();
        codec.feed(1, &bytes[split..], &mut out).unwrap();
        assert_eq!(out.len(), 1, "split {split}");
        assert_eq!(&out[0].payload[13..], b"AAAABB");
    }
}

#[test]
fn semantic_vector_uses_distinct_profile_and_round_trips() {
    let bytes = hex(V14);
    let mut codec = Codec::new(Profile::Semantic);
    let mut out = Vec::new();
    codec.feed(0, &bytes, &mut out).unwrap();
    assert_eq!((out[0].scope, out[0].kind), (0, 1));
    let mut encoded = Vec::new();
    Codec::new(Profile::Semantic)
        .encode(0, 1, &out[0].payload, &mut encoded)
        .unwrap();
    assert_eq!(encoded, bytes);
}

#[test]
fn lease_and_log_payloads_are_exact_and_never_fragmented() {
    for (kind, size) in [
        (0x15, 40),
        (0x16, 24),
        (0x17, 20),
        (0x18, 20),
        (0x19, 24),
        (0x1a, 32),
    ] {
        let mut bytes = Vec::new();
        Codec::new(Profile::Controller)
            .encode(7, kind, &exact_payload(kind, size), &mut bytes)
            .unwrap();
        let mut out = Vec::new();
        Codec::new(Profile::Controller)
            .feed(0, &bytes, &mut out)
            .unwrap();
        assert_eq!(out[0].payload.len(), size);

        let mut malformed = bytes;
        malformed[6] = 1;
        let checksum = crc32c(&malformed[..20]);
        malformed[20..24].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(
            Codec::new(Profile::Controller).feed(0, &malformed, &mut Vec::new()),
            Err(WireError::Malformed)
        );
    }
}

#[test]
fn lease_and_log_payload_values_are_fail_closed() {
    let mut request = vec![0; 40];
    request[2] = 1;
    assert_eq!(
        LeaseRequest::decode_wire(&request),
        Err(WireError::Malformed)
    );
    assert_eq!(
        LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::Viewer,
            epoch: 0,
            incarnation: [0; 16],
            token: [0; 16],
        }
        .encode_wire(),
        Err(WireError::Malformed)
    );
    assert_eq!(
        LeaseResult {
            outcome: ResultOutcome::Granted,
            reason: ResultReason::None,
            role: LeaseRole::Viewer,
            epoch: 0,
            token: [0; 16],
        }
        .encode_wire(),
        Err(WireError::Malformed)
    );
    assert!(lease_token_payload(0, [0; 16]).is_err());
    assert!(log_clear_payload([0; 16], 0).is_err());
    assert!(log_clear_result_payload(3, 0, 0, 0, 0, 0).is_err());
}

#[test]
fn exact_lease_and_log_builders_emit_valid_payloads() {
    assert_eq!(lease_token_payload(1, [2; 16]).unwrap().len(), 20);
    assert_eq!(log_clear_payload([3; 16], 7).unwrap().len(), 24);
    let cleared = log_clear_result_payload(0, 0, 2, 7, 8, 99).unwrap();
    assert_eq!(&cleared[..8], &[0, 0, 0, 0, 2, 0, 0, 0]);
    let mut reserved = cleared;
    reserved[2] = 1;
    assert!(decode_log_clear_result(&reserved).is_err());
    assert!(lease_token_payload(0, [0; 16]).is_err());
    assert!(log_clear_result_payload(3, 0, 0, 0, 0, 0).is_err());
    let result = log_clear_result_payload(2, 3, 2, 7, 8, 99).unwrap();
    assert_eq!(decode_log_clear_result(&result), Ok((2, 3, 7)));
    assert!(decode_log_clear_result(&result[..31]).is_err());
}

#[test]
fn exact_controller_request_builders_round_trip_through_the_decoder() {
    let input = input_payload(3, 9, b"terminal");
    assert!(matches!(
        decode_controller(9, &input, None),
        Ok(ControllerRequest::Policy(PolicyRequest::Input(request, None)))
            if (request.epoch, request.request_id, &*request.exact_payload) == (3, 9, input.as_slice())
    ));
    let resize = resize_payload(3, 24, 80);
    assert!(matches!(
        decode_controller(0x0b, &resize, None),
        Ok(ControllerRequest::Policy(PolicyRequest::Resize(3, 80, 24)))
    ));
    let terminate = terminate_request_payload(b"session", 7, [8; 16], true).unwrap();
    assert!(matches!(
        decode_controller(15, &terminate, None),
        Ok(ControllerRequest::Policy(PolicyRequest::Terminate(
            b"session", 7, incarnation, true
        ))) if incarnation == [8; 16]
    ));
}

#[test]
fn wide_fields_round_trip_and_enforce_the_requested_tail_boundary() {
    let mut bytes = vec![7];
    put_wide(&mut bytes, b"value").unwrap();
    bytes.push(9);
    assert_eq!(get_wide(&bytes, 1, false), Some(b"value".as_slice()));
    assert_eq!(get_wide(&bytes, 1, true), None);
    assert_eq!(
        get_wide(&bytes[..bytes.len() - 1], 1, true),
        Some(b"value".as_slice())
    );

    let mut maximum = Vec::new();
    put_wide(&mut maximum, &vec![0; 1 << 20]).unwrap();
    assert_eq!(get_wide(&maximum, 0, true).unwrap().len(), 1 << 20);
    assert_eq!(
        put_wide(&mut Vec::new(), &vec![0; (1 << 20) + 1]),
        Err(WireError::OversizedMessage)
    );
    assert_eq!(get_wide(&((1_048_577u32).to_le_bytes()), 0, false), None);
}

#[test]
fn query_layout_uses_u64_correlation_and_echoes_class() {
    let q = Query {
        correlation: 0x1_0000_0001,
        epoch: 9,
        class: 4,
        bytes: b"\x1b[?2004$p".to_vec(),
    };
    let payload = q.encode().unwrap();
    assert_eq!(payload.len(), 8 + 4 + 1 + 2 + q.bytes.len());
    assert_eq!(decode_query(&payload).unwrap(), q);
    let mut bad = payload;
    bad[0..8].fill(0);
    assert!(decode_query(&bad).is_err());
    let mut bad_epoch = q.encode().unwrap();
    bad_epoch[8..12].fill(0);
    assert!(decode_query(&bad_epoch).is_err());
}

#[test]
fn typed_request_decoders_preserve_exact_conditional_payloads() {
    let mut direct = Vec::new();
    direct.extend_from_slice(&3u32.to_le_bytes());
    direct.extend_from_slice(&9u64.to_le_bytes());
    direct.push(0);
    direct.extend_from_slice(b"terminal");
    match decode_controller(9, &direct, None).unwrap() {
        ControllerRequest::Policy(PolicyRequest::Input(input, None)) => {
            assert_eq!((input.epoch, input.request_id), (3, 9));
            assert_eq!(&*input.exact_payload, direct.as_slice());
        }
        _ => panic!("unexpected direct input request"),
    }

    let mut application = Vec::new();
    application.extend_from_slice(&3u32.to_le_bytes());
    application.extend_from_slice(&10u64.to_le_bytes());
    application.push(1);
    application.extend_from_slice(&[7; 16]);
    put_compact(&mut application, b"source-1").unwrap();
    application.extend_from_slice(b"bytes");
    match decode_controller(9, &application, None).unwrap() {
        ControllerRequest::Policy(PolicyRequest::Input(input, Some(projected))) => {
            assert_eq!(
                (
                    input.epoch,
                    input.request_id,
                    projected.receipt.application_id
                ),
                (3, 10, [7; 16])
            );
            assert_eq!(&*input.exact_payload, application.as_slice());
            assert_eq!(
                (
                    &application[projected.source],
                    &application[projected.terminal_at..]
                ),
                (b"source-1".as_slice(), b"bytes".as_slice())
            );
        }
        _ => panic!("unexpected application input request"),
    }

    let mut event = vec![1; 16];
    event.extend_from_slice(&2u64.to_le_bytes());
    event.push(0);
    event.extend_from_slice(br#"{"type":"x"}"#);
    match decode_semantic(7, 3, &event).unwrap() {
        PolicyRequest::SemanticEvent(event_request, None) => {
            assert_eq!(
                (
                    event_request.id,
                    event_request.sequence,
                    &*event_request.exact_payload
                ),
                ([1; 16], 2, &event[25..])
            );
        }
        _ => panic!("unexpected semantic event request"),
    }

    let mut receipt = vec![2; 16];
    receipt.extend_from_slice(&3u64.to_le_bytes());
    receipt.extend_from_slice(&[4; 16]);
    receipt.extend_from_slice(&5u32.to_le_bytes());
    receipt.extend_from_slice(&6u64.to_le_bytes());
    receipt.push(0);
    put_compact(&mut receipt, b"session").unwrap();
    put_compact(&mut receipt, b"turn").unwrap();
    match decode_semantic(7, 4, &receipt).unwrap() {
        PolicyRequest::SemanticEvent(event_request, Some(projected)) => {
            assert_eq!(
                (event_request.id, event_request.sequence, projected.status),
                ([2; 16], 3, 0)
            );
            let exact = &*event_request.exact_payload;
            assert_eq!(
                (
                    &exact[projected.provider_session],
                    &exact[projected.provider_turn],
                    exact
                ),
                (b"session".as_slice(), b"turn".as_slice(), &receipt[24..])
            );
        }
        _ => panic!("unexpected application receipt request"),
    }
    assert!(matches!(
        decode_semantic(1, 1, &[0; 42]),
        Err(WireError::Malformed)
    ));
}

#[test]
fn crc_sequence_and_reassembly_deadline_fail_closed() {
    let mut bad = hex(V1);
    bad[20] ^= 1;
    assert_eq!(
        Codec::new(Profile::Controller).feed(0, &bad, &mut Vec::new()),
        Err(WireError::Malformed)
    );

    let first = &hex(V7)[..41];
    let mut codec = progressed_codec(Profile::Controller, 20, 1);
    codec.feed(0, first, &mut Vec::new()).unwrap();
    assert_eq!(
        (codec.buffered_len(), codec.projected_len(9)),
        (17, Some(26))
    );
    assert_eq!(codec.expire(5_000), Err(WireError::ReassemblyTimeout));
    assert_eq!(codec.buffered_len(), 0);
}

#[test]
fn status_and_heartbeat_extensions_round_trip_and_reject_reserved_bits() {
    validate_status_flags(0xf3).unwrap();
    assert_eq!(validate_status_flags(0x04), Err(WireError::Malformed));

    let status = StatusExtension {
        health: 0x0f,
        log_epoch: 3,
        log_index: 9,
        retained_start: 100,
        retained_end: 140,
    };
    let bytes = status.encode(true).unwrap();
    assert_eq!(bytes.len(), 29);
    assert_eq!(status.encode(false), Err(WireError::Malformed));
    let mut zero_epoch = status;
    zero_epoch.log_epoch = 0;
    assert_eq!(zero_epoch.encode(true), Err(WireError::Malformed));

    let heartbeat = Heartbeat {
        monotonic_ms: 42,
        flags: 0x1f,
    };
    assert_eq!(
        Heartbeat::decode(&heartbeat.encode().unwrap()).unwrap(),
        heartbeat
    );
    assert_eq!(
        Heartbeat {
            monotonic_ms: 0,
            flags: 0x20
        }
        .encode(),
        Err(WireError::Malformed)
    );
}

#[test]
fn configured_log_keeps_its_frontier_when_the_lane_is_unwritable() {
    let extension = StatusExtension {
        health: 0,
        log_epoch: 3,
        log_index: 9,
        retained_start: 100,
        retained_end: 140,
    };
    let tail = StatusTail {
        replay: ReplayDescriptor {
            first: 0,
            last: 0,
            start: 0,
            end: 0,
            complete: true,
            modes_exact: false,
        },
        owns_lease: false,
        viewers: false,
        running: true,
        event_writable: true,
        lease_epoch: 0,
        semantic_flags: 0,
        semantic_pending: 0,
        extension,
    };
    let mut payload = status_prefix();
    payload.extend(tail.encode().unwrap());
    assert_eq!(decode_status(&payload).unwrap().extension, extension);
}

#[test]
fn feed_enforces_reassembly_deadline_without_external_poll() {
    let bytes = hex(V7);
    let split = 24 + 17;
    let mut codec = progressed_codec(Profile::Controller, 20, 1);
    codec.feed(0, &bytes[..split], &mut Vec::new()).unwrap();
    assert_eq!(
        codec.feed(5_000, &bytes[split..], &mut Vec::new()),
        Err(WireError::ReassemblyTimeout)
    );

    let mut incomplete = Codec::new(Profile::Controller);
    incomplete.feed(0, &hex(V1)[..25], &mut Vec::new()).unwrap();
    assert_eq!(incomplete.expire(5_000), Err(WireError::ReassemblyTimeout));
    assert_eq!(incomplete.buffered_len(), 0);
}

#[test]
fn a_new_partial_message_gets_its_own_deadline_after_a_completed_message() {
    let payload = controller_hello(b"\x01/tmp/session").unwrap();
    let mut bytes = Vec::new();
    let mut sender = Codec::new(Profile::Controller);
    sender.encode(7, 1, &payload, &mut bytes).unwrap();
    let first_end = bytes.len();
    sender.encode(7, 1, &payload, &mut bytes).unwrap();

    let mut receiver = Codec::new(Profile::Controller);
    let mut messages = Vec::new();
    receiver.feed(0, &bytes[..10], &mut messages).unwrap();
    receiver
        .feed(4_999, &bytes[10..first_end + 1], &mut messages)
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(receiver.expire(5_000), Ok(()));
    assert_eq!(receiver.expire(9_999), Err(WireError::ReassemblyTimeout));
}
