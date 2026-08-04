use moor::wire::{
    Codec, Heartbeat, Profile, Query, StatusExtension, WireError, crc32c, decode_query,
    validate_attach_flags, validate_status_flags,
};

fn hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
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
        let mut codec = Codec::with_sequences(Profile::Controller, 20, 1);
        let mut out = Vec::new();
        codec.feed(0, &bytes[..split], &mut out).unwrap();
        codec.feed(1, &bytes[split..], &mut out).unwrap();
        assert_eq!(out.len(), 1, "split {split}");
        assert!(out[0].fragmented);
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
            .encode(7, kind, &vec![0; size], &mut bytes)
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
    let mut codec = Codec::with_sequences(Profile::Controller, 20, 1);
    codec.feed(0, first, &mut Vec::new()).unwrap();
    assert_eq!(codec.expire(5_000), Err(WireError::ReassemblyTimeout));
}

#[test]
fn status_and_heartbeat_extensions_round_trip_and_reject_reserved_bits() {
    validate_status_flags(0xf3).unwrap();
    assert_eq!(validate_status_flags(0x04), Err(WireError::Malformed));
    validate_attach_flags(3).unwrap();
    assert_eq!(validate_attach_flags(4), Err(WireError::Malformed));

    let status = StatusExtension {
        health: 0x0f,
        log_epoch: 3,
        log_index: 9,
        retained_start: 100,
        retained_end: 140,
    };
    let bytes = status.encode(true).unwrap();
    assert_eq!(bytes.len(), 29);
    assert_eq!(StatusExtension::decode(&bytes, true).unwrap(), status);
    assert_eq!(
        StatusExtension::decode(&bytes, false),
        Err(WireError::Malformed)
    );

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
fn feed_enforces_reassembly_deadline_without_external_poll() {
    let bytes = hex(V7);
    let split = 24 + 17;
    let mut codec = Codec::with_sequences(Profile::Controller, 20, 1);
    codec.feed(0, &bytes[..split], &mut Vec::new()).unwrap();
    assert_eq!(
        codec.feed(5_000, &bytes[split..], &mut Vec::new()),
        Err(WireError::ReassemblyTimeout)
    );
}

#[test]
fn encode_preflights_sequence_space_and_never_leaves_a_more_prefix() {
    let mut codec = Codec::with_sequences(Profile::Semantic, 1, u32::MAX - 1);
    let mut out = b"unchanged".to_vec();
    assert_eq!(
        codec.encode(1, 3, &vec![0; (1 << 16) + 1], &mut out),
        Err(WireError::ResourceExhausted)
    );
    assert_eq!(out, b"unchanged");
}
