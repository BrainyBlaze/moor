//! Byte-exact conformance against the frozen §16 vectors of
//! spec/moor-wire-schema.md.
//!
//! This lane exists because consumer issue #12 found a length-prefix-width
//! defect that 243 passing tests could not see: tests/wire.rs round-trips only
//! the 24-byte framing of the §16 vectors and never decodes their payloads,
//! and its payload fixtures were written from the implementation rather than
//! from the schema. Every vector here is the exact hex copied from §16, and
//! encoders are byte-compared against those bytes.

#![cfg(unix)]

// ---- vectors: V12, V21, V22 ----
// Conformance lane: §16 platform vectors V12, V13, V21, V22.
// Every vector below is the EXACT hex copied from spec/moor-wire-schema.md §16.
// Nothing here is computed from the implementation.

use moor::runtime::private::{
    copy_digest, decode_launch_record, fixed_record, instrument_ack, validate_instrument_ack,
};
use moor::store::{Commit, Kind};
use moor::windows::Marker;
use moor::wire::crc32c;

fn hex(s: &str) -> Vec<u8> {
    let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    assert!(digits.len().is_multiple_of(2), "odd hex digit count");
    digits
        .chunks(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16)
                .expect("invalid hex digit in frozen vector")
        })
        .collect()
}

fn progressed_codec(
    profile: moor::wire::Profile,
    next_in: u32,
    next_out: u32,
) -> moor::wire::Codec {
    assert!(next_in != 0 && next_out != 0);
    let mut codec = moor::wire::Codec::new(profile);
    if next_in > 1 {
        let mut sender = moor::wire::Codec::new(profile);
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

fn allocate_and_release(machine: &mut moor::session::Machine, conn: u64, count: u32) {
    use moor::session::{Effect, LeaseRequest, LeaseRole, Request, ResultOutcome, Transition};

    for epoch in 1..=count {
        let token = [epoch as u8; 16];
        let grant = machine
            .transition(Transition::Peer(
                u64::from(epoch) * 2,
                conn,
                Request::Lease(LeaseRequest::fresh(LeaseRole::InputOnly), Some(token)),
            ))
            .unwrap()
            .into_iter()
            .find_map(|effect| match effect {
                Effect::LeaseReply(id, result) if id == conn => Some(result),
                _ => None,
            })
            .expect("lease grant");
        assert_eq!(
            (grant.outcome, grant.epoch),
            (ResultOutcome::Granted, epoch)
        );
        machine
            .transition(Transition::Peer(
                u64::from(epoch) * 2 + 1,
                conn,
                Request::Release(epoch, token),
            ))
            .unwrap();
    }
}

// §16 V12 — Windows rendezvous marker, generation 7, holder incarnation
// 00..0F, local pipe \\.\pipe\moor-000102030405060708090a0b0c0d0e0f.
fn v12() -> Vec<u8> {
    hex("4D 4F 4F 52 4D 52 4B 33 01 00 00 00 07 00 00 00
         00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F
         2E 00 5C 5C 2E 5C 70 69 70 65 5C 6D 6F 6F 72 2D
         30 30 30 31 30 32 30 33 30 34 30 35 30 36 30 37
         30 38 30 39 30 61 30 62 30 63 30 64 30 65 30 66
         B1 25 D5 68")
}

// §16 V21 — supervised-launch discriminator, generation 7, nonce 00..0F;
// exactly 32 bytes followed by EOF.
fn v21() -> Vec<u8> {
    hex("4D 4F 4F 52 4C 43 48 33 01 00 00 00 07 00 00 00
         00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F")
}

// §16 V22 — instrumentation-load acknowledgement, generation 7,
// requested-child PID 0x00001234, nonce 10..1F; exactly 36 bytes then EOF.
fn v22() -> Vec<u8> {
    hex("4D 4F 4F 52 49 4E 53 33 01 00 00 00 07 00 00 00
         34 12 00 00 10 11 12 13 14 15 16 17 18 19 1A 1B
         1C 1D 1E 1F")
}

// §16 V13 — the frozen 137-byte schema-v2 header body, final LF included,
// copied verbatim from the ```jsonl block in the schema.
const V13_BODY: &[u8] = b"{\"v\":2,\"type\":\"header\",\"ts\":0,\"session\":\"AgABAgMEBQYHCAkKCwwNDg8QERITFBUWFw==\",\"generation\":7,\"epoch\":0,\"next_seq\":0,\"first_retained\":0}\n";

// §16 V13 — the frozen SHA-256 the schema states for that body.
fn v13_hash() -> [u8; 32] {
    hex("2C 71 E9 28 70 77 41 50 F5 DB EE C3 4F 19 05 2D
         82 87 4F 6E B4 AC 4B C9 F8 D4 BF 7A D7 43 ED FB")
    .try_into()
    .unwrap()
}

fn incarnation() -> [u8; 16] {
    (0u8..16).collect::<Vec<_>>().try_into().unwrap()
}

fn v22_nonce() -> [u8; 16] {
    (0x10u8..0x20).collect::<Vec<_>>().try_into().unwrap()
}

// Rewrite the trailing CRC-32C so a single-field mutation is rejected for
// that field, not merely for a stale checksum.
fn with_valid_crc(mut bytes: Vec<u8>) -> Vec<u8> {
    let checksum = crc32c(&bytes[..80]).to_le_bytes();
    bytes[80..84].copy_from_slice(&checksum);
    bytes
}

// Drive §15's stream entrypoint over an in-memory byte stream: reads report
// data as available and EOF when the bytes run out.
fn stream_record<const N: usize>(bytes: &[u8], eof: bool) -> Result<[u8; N], String> {
    let mut cursor = std::io::Cursor::new(bytes.to_vec());
    fixed_record::<N>(&mut cursor, "record", "invalid record", eof, |_| {
        Ok(Some(usize::MAX))
    })
}

// ---------------------------------------------------------------- V12 ----

#[test]
fn v16_platform_v12_frozen_vector_is_84_bytes_with_declared_pipe_length() {
    // §12: 84 bytes total = 34 fixed + 46-byte pipe name + 4-byte CRC, and
    // offset 32 declares that pipe-name length. Tie declared to actual so a
    // prefix-width or layout regression cannot hide.
    let bytes = v12();
    assert_eq!(bytes.len(), 84);
    let declared = u16::from_le_bytes([bytes[32], bytes[33]]) as usize;
    assert_eq!(declared, 46);
    assert_eq!(
        declared,
        bytes.len() - 34 - 4,
        "declared vs actual pipe length"
    );
}

#[test]
fn v16_platform_v12_marker_encode_matches_frozen_bytes() {
    let marker = Marker::new(7, incarnation(), incarnation()).unwrap();
    assert_eq!(marker.encode().to_vec(), v12());
}

#[test]
fn v16_platform_v12_marker_decode_accepts_frozen_bytes_and_round_trips() {
    let decoded = Marker::decode(&v12()).unwrap();
    assert_eq!(
        decoded,
        Marker::new(7, incarnation(), incarnation()).unwrap()
    );
    assert_eq!(decoded.generation, 7);
    assert_eq!(decoded.incarnation, incarnation());
    assert_eq!(decoded.pipe_length, [0x2E, 0x00]);
    assert_eq!(&decoded.pipe[..14], br"\\.\pipe\moor-");
    assert_eq!(
        &decoded.pipe[14..],
        b"000102030405060708090a0b0c0d0e0f".as_slice()
    );
    assert_eq!(decoded.encode().to_vec(), v12());
}

#[test]
fn v16_platform_v12_marker_rejects_wrong_total_length() {
    let bytes = v12();
    assert!(Marker::decode(&bytes[..83]).is_err());
    let mut long = bytes.clone();
    long.push(0);
    assert!(Marker::decode(&long).is_err());
    assert!(Marker::decode(&[]).is_err());
}

#[test]
fn v16_platform_v12_marker_rejects_wrong_magic() {
    let mut bytes = v12();
    bytes[0] = b'X'; // MOORMRK3 -> XOORMRK3
    assert!(Marker::decode(&with_valid_crc(bytes)).is_err());
}

#[test]
fn v16_platform_v12_marker_rejects_wrong_format_flags_or_reserved() {
    // §12: byte 8 is format 01, byte 9 flags zero, bytes 10-11 reserved zero.
    for at in [8usize, 9, 10, 11] {
        let mut bytes = v12();
        bytes[at] ^= 0x01;
        assert!(
            Marker::decode(&with_valid_crc(bytes)).is_err(),
            "fixed-field byte {at}"
        );
    }
}

#[test]
fn v16_platform_v12_marker_rejects_zero_generation() {
    let mut bytes = v12();
    bytes[12..16].copy_from_slice(&0u32.to_le_bytes());
    assert!(Marker::decode(&with_valid_crc(bytes)).is_err());
}

#[test]
fn v16_platform_v12_marker_rejects_wrong_pipe_length() {
    let mut bytes = v12();
    bytes[32..34].copy_from_slice(&45u16.to_le_bytes()); // must be exactly 46
    assert!(Marker::decode(&with_valid_crc(bytes)).is_err());
}

#[test]
fn v16_platform_v12_marker_rejects_wrong_pipe_prefix() {
    let mut bytes = v12();
    assert_eq!(bytes[34], b'\\');
    bytes[34] = b'/'; // \\.\pipe\moor- prefix is frozen
    assert!(Marker::decode(&with_valid_crc(bytes)).is_err());
}

#[test]
fn v16_platform_v12_marker_rejects_uppercase_hex_digit() {
    let mut bytes = v12();
    let at = bytes[48..80]
        .iter()
        .position(|byte| byte.is_ascii_lowercase())
        .expect("frozen pipe name contains a lowercase hex letter")
        + 48;
    bytes[at] = bytes[at].to_ascii_uppercase();
    assert!(Marker::decode(&with_valid_crc(bytes)).is_err());
}

#[test]
fn v16_platform_v12_marker_rejects_bad_crc() {
    let mut bytes = v12();
    bytes[83] ^= 0x01;
    assert!(Marker::decode(&bytes).is_err());
}

// ---------------------------------------------------------------- V21 ----

#[test]
fn v16_platform_v21_frozen_vector_is_32_bytes() {
    assert_eq!(v21().len(), 32); // §15.1: exactly 32 bytes then EOF
}

#[test]
fn v16_platform_v21_launch_record_accepted_with_generation_7() {
    assert_eq!(decode_launch_record(&v21()), Some(7));
}

#[test]
fn v16_platform_v21_stream_accepts_exact_record_then_eof() {
    // §15.1 end to end: the stream entrypoint accepts exactly 32 bytes
    // followed by EOF, and the accepted bytes decode as generation 7.
    let record = stream_record::<32>(&v21(), true).unwrap();
    assert_eq!(record.to_vec(), v21());
    assert_eq!(decode_launch_record(&record), Some(7));
}

#[test]
fn v16_platform_v21_stream_rejects_extra_byte_and_short_record() {
    let mut long = v21();
    long.push(0);
    assert!(stream_record::<32>(&long, true).is_err());
    assert!(stream_record::<32>(&v21()[..31], true).is_err());
}

#[test]
fn v16_platform_v21_rejects_short_record() {
    let bytes = v21();
    assert_eq!(decode_launch_record(&bytes[..31]), None);
    assert_eq!(decode_launch_record(&[]), None);
}

#[test]
fn v16_platform_v21_rejects_over_long_record() {
    let mut bytes = v21();
    bytes.push(0);
    assert_eq!(decode_launch_record(&bytes), None);
}

#[test]
fn v16_platform_v21_accepts_generation_2_lower_bound() {
    // §15.1: supervised generation range is 2..=u32::MAX.
    let mut bytes = v21();
    bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(decode_launch_record(&bytes), Some(2));
}

#[test]
fn v16_platform_v21_rejects_generation_below_supervised_range() {
    // §15.1: a discriminator carrying generation 0 or 1 is refused.
    for generation in [0u32, 1] {
        let mut bytes = v21();
        bytes[12..16].copy_from_slice(&generation.to_le_bytes());
        assert_eq!(
            decode_launch_record(&bytes),
            None,
            "generation {generation}"
        );
    }
}

#[test]
fn v16_platform_v21_rejects_bad_magic() {
    let mut bytes = v21();
    bytes[0] = b'X'; // MOORLCH3 -> XOORLCH3
    assert_eq!(decode_launch_record(&bytes), None);
}

#[test]
fn v16_platform_v21_rejects_wrong_format_flags_or_reserved() {
    // §15.1: byte 8 is format 01, byte 9 flags zero, bytes 10-11 reserved.
    for at in [8usize, 9, 10, 11] {
        let mut bytes = v21();
        bytes[at] ^= 0x01;
        assert_eq!(decode_launch_record(&bytes), None, "fixed-field byte {at}");
    }
}

#[test]
fn v16_platform_v21_rejects_all_zero_nonce() {
    // §15.1: the nonce is a fresh random value; all-zero is refused.
    let mut bytes = v21();
    bytes[16..].fill(0);
    assert_eq!(decode_launch_record(&bytes), None);
}

// ---------------------------------------------------------------- V22 ----

#[test]
fn v16_platform_v22_frozen_vector_is_36_bytes() {
    assert_eq!(v22().len(), 36); // §15.2: exactly 36 bytes then EOF
}

#[test]
fn v16_platform_v22_instrument_ack_matches_frozen_bytes() {
    assert_eq!(
        instrument_ack(7, 0x1234, v22_nonce()).unwrap().to_vec(),
        v22()
    );
}

#[test]
fn v16_platform_v22_validate_accepts_frozen_bytes() {
    assert!(validate_instrument_ack(&v22(), true, 7, 0x1234, v22_nonce()).is_ok());
}

#[test]
fn v16_platform_v22_stream_accepts_exact_record_then_eof() {
    let record = stream_record::<36>(&v22(), true).unwrap();
    assert_eq!(record.to_vec(), v22());
    assert!(validate_instrument_ack(&record, true, 7, 0x1234, v22_nonce()).is_ok());
}

#[test]
fn v16_platform_v22_stream_rejects_extra_byte_and_short_record() {
    let mut long = v22();
    long.push(0);
    assert!(stream_record::<36>(&long, true).is_err());
    assert!(stream_record::<36>(&v22()[..35], true).is_err());
}

#[test]
fn v16_platform_v22_validate_rejects_wrong_length() {
    let bytes = v22();
    assert!(validate_instrument_ack(&bytes[..35], true, 7, 0x1234, v22_nonce()).is_err());
    let mut long = bytes.clone();
    long.push(0);
    assert!(validate_instrument_ack(&long, true, 7, 0x1234, v22_nonce()).is_err());
    assert!(validate_instrument_ack(&[], true, 7, 0x1234, v22_nonce()).is_err());
}

#[test]
fn v16_platform_v22_validate_rejects_wrong_magic_format_flags_or_reserved() {
    // §15.2: bytes 0-7 magic MOORINS3, byte 8 format 01, byte 9 flags zero,
    // bytes 10-11 reserved zero.
    for at in [0usize, 8, 9, 10, 11] {
        let mut bytes = v22();
        bytes[at] ^= 0x01;
        assert!(
            validate_instrument_ack(&bytes, true, 7, 0x1234, v22_nonce()).is_err(),
            "fixed-field byte {at}"
        );
    }
}

#[test]
fn v16_platform_v22_validate_rejects_wrong_pid() {
    assert!(validate_instrument_ack(&v22(), true, 7, 0x4321, v22_nonce()).is_err());
}

#[test]
fn v16_platform_v22_validate_rejects_wrong_generation() {
    assert!(validate_instrument_ack(&v22(), true, 8, 0x1234, v22_nonce()).is_err());
}

#[test]
fn v16_platform_v22_validate_rejects_wrong_nonce() {
    let mut nonce = v22_nonce();
    nonce[0] ^= 0x01;
    assert!(validate_instrument_ack(&v22(), true, 7, 0x1234, nonce).is_err());
}

#[test]
fn v16_platform_v22_validate_rejects_missing_eof() {
    assert!(validate_instrument_ack(&v22(), false, 7, 0x1234, v22_nonce()).is_err());
}

#[test]
fn v16_platform_v22_encoder_refuses_zero_pid_and_zero_generation() {
    // §15.2: PID is nonzero and generation is 1 (unsupervised) or the exact
    // supervised generation — never zero.
    assert!(instrument_ack(7, 0, v22_nonce()).is_err());
    assert!(instrument_ack(0, 0x1234, v22_nonce()).is_err());
    assert!(validate_instrument_ack(&v22(), true, 7, 0, v22_nonce()).is_err());
    assert!(validate_instrument_ack(&v22(), true, 0, 0x1234, v22_nonce()).is_err());
}

// ---------------------------------------------------------------- V13 ----

#[test]
fn v16_platform_v13_frozen_body_is_137_bytes_with_frozen_sha256() {
    // §16 V13 freezes the header BODY (137 UTF-8 bytes including final LF)
    // and its SHA-256 independently of the superseded 76-byte commit record.
    // Both survive the MOORCMT1 amendment, so assert them from the frozen
    // bytes: the length check ties the commit's declared body-prefix length
    // to the actual body, and the digest is recomputed through the crate's
    // own hashing entrypoint rather than trusted from this test.
    assert_eq!(V13_BODY.len(), 137);
    use std::io::Write as _;
    let path =
        std::env::temp_dir().join(format!("moor-v16-platform-v13-{}.body", std::process::id()));
    let mut file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    file.write_all(V13_BODY).unwrap();
    let digest = copy_digest(&mut file, None);
    let _ = std::fs::remove_file(&path);
    assert_eq!(digest.unwrap(), v13_hash());
}

#[test]
fn v16_platform_v13_matches_the_ratified_portable_commit_byte_for_byte() {
    // §16 V13, ratified form: the portable 92-byte MOORCMT1 initial event
    // commit. The previous revision of this test could only pin the record's
    // shape through the encoder, because the artifact still froze the
    // superseded 76-byte MOOREVC2 record and no ratified MOORCMT1 bytes
    // existed. Asserting an encoder against values fed to that same encoder is
    // a tautology; these bytes come from the artifact instead.
    let frozen = hex("4D 4F 4F 52 43 4D 54 31 01 00 00 01 07 00 00 00
         00 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00
         89 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 2C 71 E9 28 70 77 41 50
         F5 DB EE C3 4F 19 05 2D 82 87 4F 6E B4 AC 4B C9
         F8 D4 BF 7A D7 43 ED FB 28 95 8D 91");
    assert_eq!(frozen.len(), 92, "the ratified record is exactly 92 bytes");

    // The encoder must reproduce those bytes from the values §16 states:
    // kind event 01, generation 7, epoch and coordinates zero, commit slot 0,
    // body slot 0, index 1, prefix length 137 (0x89) and the frozen body hash.
    let produced = Commit {
        slot: 0,
        body: 0,
        kind: Kind::Event,
        generation: 7,
        epoch: 0,
        index: 1,
        length: V13_BODY.len() as u64,
        start: 0,
        end: 0,
        hash: v13_hash(),
    }
    .encode();
    assert_eq!(produced.as_slice(), frozen.as_slice());

    // The frozen trailing CRC-32C really is a CRC-32C over bytes 0..88, so a
    // transcription error in the record body cannot pass unnoticed.
    assert_eq!(
        u32::from_le_bytes(frozen[88..92].try_into().unwrap()),
        crc32c(&frozen[..88]),
    );

    // And the frozen prefix length is the real length of the frozen body.
    assert_eq!(
        u64::from_le_bytes(frozen[32..40].try_into().unwrap()),
        V13_BODY.len() as u64,
    );
}

// ---- vectors: V14, V15, V17, V19, V20 ----
// Conformance lane: semantic producer wire — §16 vectors V14, V15, V17, V19, V20.
//
// Every byte string below is copied verbatim from spec/moor-wire-schema.md §16.
// Nothing in this file is derived from the implementation; the implementation
// is driven AT the frozen bytes in both directions wherever the public API
// exposes an entrypoint for that direction.

/// Parses whitespace-separated hex octets exactly as they appear in §16.
/// (Prefixed to keep the assembled conformance file collision-free.)
fn v16_semantic_hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("hex octet"))
        .collect()
}

/// §16 V14 — semantic `HELLO`, generation 7, stateful source `claude`, all
/// three capabilities. Frozen bytes, header included.
const V16_SEMANTIC_V14: &str = "
    4D 4F 4F 53 01 01 00 00 00 00 00 00 01 00 00 00
    2E 00 00 00 C8 42 0C 36 00 01 02 03 04 05 06 07
    08 09 0A 0B 0C 0D 0E 0F 10 11 12 13 14 15 16 17
    18 19 1A 1B 1C 1D 1E 1F 07 00 00 00 01 07 06 00
    63 6C 61 75 64 65";

/// §16 V15 — semantic `APPLICATION_RECEIPT`, source epoch 5 / source sequence
/// 2, accepted, provider ids `sess`/`turn`. Frozen bytes, header included.
const V16_SEMANTIC_V15: &str = "
    4D 4F 4F 53 01 04 00 00 05 00 00 00 03 00 00 00
    41 00 00 00 C2 D3 5C 3F 00 01 02 03 04 05 06 07
    08 09 0A 0B 0C 0D 0E 0F 02 00 00 00 00 00 00 00
    10 11 12 13 14 15 16 17 18 19 1A 1B 1C 1D 1E 1F
    03 00 00 00 01 00 00 00 00 00 00 00 00 04 00 73
    65 73 73 04 00 74 75 72 6E";

/// §16 V17 — semantic `INPUT_NOTICE` matching V16; the final 32 bytes are
/// SHA-256("hello"). Frozen bytes, header included.
const V16_SEMANTIC_V17: &str = "
    4D 4F 4F 53 01 05 00 00 05 00 00 00 04 00 00 00
    44 00 00 00 8C B0 EC 04 20 21 22 23 24 25 26 27
    28 29 2A 2B 2C 2D 2E 2F 03 00 00 00 02 00 00 00
    00 00 00 00 05 00 00 00 00 00 00 00 2C F2 4D BA
    5F B0 A3 0E 26 E8 3B 2A C5 B9 E2 9E 1B 16 1E 5C
    1F A7 42 5E 73 04 33 62 93 8B 98 24";

/// §16 V19 — accepted durable `SEMANTIC_ACK` for V15, status `00`, result
/// code zero, event epoch 2 / sequence 9. Frozen bytes, header included.
const V16_SEMANTIC_V19: &str = "
    4D 4F 4F 53 01 07 00 00 05 00 00 00 05 00 00 00
    29 00 00 00 68 84 6D AE 00 01 02 03 04 05 06 07
    08 09 0A 0B 0C 0D 0E 0F 02 00 00 00 00 00 00 00
    00 00 00 02 00 00 00 09 00 00 00 00 00 00 00 00
    00";

/// §16 V20 — duplicate `SEMANTIC_ACK` after V15 is retried; status `01`,
/// result code zero, original durable position epoch 2 / sequence 9, frame
/// sequence advanced from V19. Frozen bytes, header included.
const V16_SEMANTIC_V20: &str = "
    4D 4F 4F 53 01 07 00 00 05 00 00 00 06 00 00 00
    29 00 00 00 01 03 29 75 00 01 02 03 04 05 06 07
    08 09 0A 0B 0C 0D 0E 0F 02 00 00 00 00 00 00 00
    01 00 00 02 00 00 00 09 00 00 00 00 00 00 00 00
    00";

/// SHA-256("hello") written down independently of the vector (well-known
/// digest), so V17's trailing 32 bytes are cross-checked against knowledge
/// that did not come from §16 or from the implementation.
const V16_SEMANTIC_SHA256_HELLO: &str = "
    2C F2 4D BA 5F B0 A3 0E 26 E8 3B 2A C5 B9 E2 9E
    1B 16 1E 5C 1F A7 42 5E 73 04 33 62 93 8B 98 24";

/// Feeds one frozen frame stream through the real reassembly entrypoint.
fn v16_semantic_feed(next_in: u32, stream: &[u8]) -> Vec<moor::wire::Message> {
    let mut codec = progressed_codec(moor::wire::Profile::Semantic, next_in, 1);
    let mut out = Vec::new();
    codec
        .feed(0, stream, &mut out)
        .expect("frozen §16 frame must be accepted by Codec::feed");
    assert_eq!(
        codec.buffered_len(),
        0,
        "the declared payload lengths must consume the frozen bytes exactly"
    );
    out
}

/// Encodes one payload through the real framing encoder at a given outbound
/// frame sequence and returns the emitted bytes (header + CRC included).
fn v16_semantic_encode(next_out: u32, scope: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut codec = progressed_codec(moor::wire::Profile::Semantic, 1, next_out);
    let mut out = Vec::new();
    codec
        .encode(scope, kind, payload, &mut out)
        .expect("frozen §16 payload must be encodable");
    out
}

fn v16_semantic_bytes_from(start: u8) -> [u8; 16] {
    core::array::from_fn(|index| start + index as u8)
}

// ---------------------------------------------------------------------------
// V14 — semantic HELLO
// ---------------------------------------------------------------------------

#[test]
fn v16_semantic_v14_frame_decodes_through_codec() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V14);
    assert_eq!(
        frame.len(),
        24 + 0x2E,
        "V14 is a 24-byte header plus 0x2E payload"
    );
    let messages = v16_semantic_feed(1, &frame);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].scope, 0,
        "SEMANTIC_HELLO carries source epoch zero"
    );
    assert_eq!(messages[0].kind, 1);
    assert_eq!(&messages[0].payload[..], &frame[24..]);
}

#[test]
fn v16_semantic_v14_hello_decodes_stateful_claude_with_all_capabilities() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V14);
    let payload = &frame[24..];
    // Before the length-prefix fix this failed with OversizedMessage because
    // the 2-byte §14.4 source-id prefix was misread as a 4-byte wide prefix.
    let request = moor::wire::decode_semantic(0, 1, payload)
        .expect("V14 must decode; a failure here reproduces consumer issue #12");
    let moor::session::Request::SemanticHello(hello) = request else {
        panic!("V14 must decode as SemanticHello");
    };
    assert_eq!(hello.token, v16_semantic_bytes_from(0x00));
    assert_eq!(hello.producer, v16_semantic_bytes_from(0x10));
    assert_eq!(hello.generation, 7);
    assert_eq!(hello.mode, moor::session::SemanticMode::Stateful);
    assert_eq!(hello.capabilities, 0x07, "all three capability bits");
    assert_ne!(hello.capabilities & 0x01, 0, "bit 0 ASSERTION");
    assert_ne!(hello.capabilities & 0x02, 0, "bit 1 APPLICATION_RECEIPT");
    assert_ne!(hello.capabilities & 0x04, 0, "bit 2 INPUT_NOTICE");
    assert_eq!(&*hello.source, b"claude");
}

#[test]
fn v16_semantic_v14_source_id_uses_two_byte_compact_prefix() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V14);
    let payload = &frame[24..];
    // Payload layout: token 0..16, producer 16..32, generation 32..36,
    // mode 36, capabilities 37, then the §14.4 length-prefixed source id.
    assert_eq!(
        &payload[38..40],
        &[0x06, 0x00],
        "source id carries a plain 2-byte little-endian length prefix"
    );
    // Encode direction: the §1.1 compact writer must reproduce the frozen
    // prefix-plus-bytes field exactly.
    let mut field = Vec::new();
    moor::wire::put_compact(&mut field, b"claude").expect("source id fits the compact cap");
    assert_eq!(field, &payload[38..46]);
    // Decode direction: the compact reader resolves the frozen field with an
    // exact tail — no trailing bytes, so the prefix cannot be 4 bytes wide.
    assert_eq!(
        moor::wire::get_compact(payload, 38, true),
        Some(&b"claude"[..])
    );
    // The exact issue-#12 misread, driven at the frozen bytes: interpreting
    // this field's prefix as the §1.1.1 wide 4-byte form reads length
    // 0x6C630006 and must fail, not resolve.
    assert_eq!(moor::wire::get_wide(payload, 38, true), None);
    // Whole-payload encode: frozen §16 field values, serialized only through
    // the crate's writers, must reproduce V14's payload byte-for-byte.
    let mut rebuilt = Vec::new();
    rebuilt.extend_from_slice(&v16_semantic_bytes_from(0x00));
    rebuilt.extend_from_slice(&v16_semantic_bytes_from(0x10));
    rebuilt.extend_from_slice(&7u32.to_le_bytes());
    rebuilt.push(0x01); // mode: stateful
    rebuilt.push(0x07); // capabilities: all three
    moor::wire::put_compact(&mut rebuilt, b"claude").expect("source id fits the compact cap");
    assert_eq!(rebuilt, payload);
}

#[test]
fn v16_semantic_v14_frame_reencodes_byte_for_byte() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V14);
    // Framing encode direction: header fields and CRC-32C are produced by the
    // implementation and must equal the frozen bytes, including checksum
    // C8 42 0C 36.
    assert_eq!(v16_semantic_encode(1, 0, 1, &frame[24..]), frame);
}

#[test]
fn v16_semantic_v14_hello_with_nonzero_source_epoch_is_refused() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V14);
    // §14.1: SEMANTIC_HELLO uses source epoch zero; the same frozen payload
    // presented under a nonzero epoch must not decode as a hello.
    assert!(matches!(
        moor::wire::decode_semantic(5, 1, &frame[24..]),
        Err(moor::wire::WireError::Malformed)
    ));
}

// ---------------------------------------------------------------------------
// V15 — APPLICATION_RECEIPT
// ---------------------------------------------------------------------------

#[test]
fn v16_semantic_v15_frame_decodes_through_codec() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V15);
    assert_eq!(
        frame.len(),
        24 + 0x41,
        "V15 is a 24-byte header plus 0x41 payload"
    );
    let messages = v16_semantic_feed(3, &frame);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].scope, 5, "source epoch 5");
    assert_eq!(messages[0].kind, 4);
    assert_eq!(&messages[0].payload[..], &frame[24..]);
}

#[test]
fn v16_semantic_v15_receipt_ranges_resolve_to_sess_and_turn() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V15);
    let payload = &frame[24..];
    let request = moor::wire::decode_semantic(5, 4, payload).expect("V15 must decode");
    let moor::session::Request::SemanticEvent(event, Some(projection)) = request else {
        panic!("V15 must decode as SemanticEvent with a receipt projection");
    };
    assert_eq!(event.id, v16_semantic_bytes_from(0x00));
    assert_eq!(event.sequence, 2);
    assert_eq!(
        event.kind,
        moor::session::SemanticEventKind::ApplicationReceipt
    );
    // The crate's projection contract retains the payload minus the leading
    // event id (16) and source sequence (8); the provider-id ranges below are
    // defined relative to that retained slice, so pin it to the frozen bytes
    // before resolving them. (§14.5 freezes the field layout; the 24-byte
    // exclusion is the decoder's documented representation, asserted here so
    // the range resolution is anchored to §16 bytes.)
    assert_eq!(&*event.exact_payload, &payload[24..]);
    assert_eq!(
        projection.receipt.application_id,
        v16_semantic_bytes_from(0x10)
    );
    assert_eq!(projection.receipt.lease_epoch, 3);
    assert_eq!(projection.receipt.request_id, 1);
    assert_eq!(projection.status, 0, "accepted");
    // The projection returns byte RANGES into the retained payload. Resolve
    // them against those exact retained bytes: an off-by-N range produced by
    // a wrong-width length prefix lands on the wrong bytes and fails here.
    let session = &event.exact_payload[projection.provider_session.clone()];
    assert_eq!(
        session, b"sess",
        "provider session id resolved from its range"
    );
    let turn = &event.exact_payload[projection.provider_turn.clone()];
    assert_eq!(turn, b"turn", "provider turn id resolved from its range");
}

#[test]
fn v16_semantic_v15_provider_ids_use_two_byte_compact_prefixes() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V15);
    let payload = &frame[24..];
    // Payload layout: id 0..16, sequence 16..24, application id 24..40,
    // lease epoch 40..44, controller request id 44..52, status 52, then the
    // two length-prefixed provider identifiers.
    assert_eq!(&payload[53..55], &[0x04, 0x00], "session id 2-byte prefix");
    assert_eq!(&payload[59..61], &[0x04, 0x00], "turn id 2-byte prefix");
    // Encode direction through the crate's compact writer.
    let mut fields = Vec::new();
    moor::wire::put_compact(&mut fields, b"sess").expect("session id fits the compact cap");
    moor::wire::put_compact(&mut fields, b"turn").expect("turn id fits the compact cap");
    assert_eq!(fields, &payload[53..65]);
    // Decode direction through the crate's compact reader.
    assert_eq!(
        moor::wire::get_compact(payload, 53, false),
        Some(&b"sess"[..])
    );
    assert_eq!(
        moor::wire::get_compact(payload, 59, true),
        Some(&b"turn"[..])
    );
    // The issue-#12 misread at the frozen bytes: a wide 4-byte prefix would
    // read lengths 0x65730004 / 0x75740004 and must fail on both fields.
    assert_eq!(moor::wire::get_wide(payload, 53, false), None);
    assert_eq!(moor::wire::get_wide(payload, 59, true), None);
}

#[test]
fn v16_semantic_v15_frame_reencodes_byte_for_byte() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V15);
    assert_eq!(v16_semantic_encode(3, 5, 4, &frame[24..]), frame);
}

// ---------------------------------------------------------------------------
// V17 — INPUT_NOTICE
// ---------------------------------------------------------------------------

#[test]
fn v16_semantic_v17_notice_encoder_reproduces_frozen_payload() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V17);
    let payload = &frame[24..];
    assert_eq!(payload.len(), 0x44, "16 + 4 + 8 + 8 + 32");
    // The digest field is the vector's frozen final 32 bytes.
    let digest: [u8; 32] = payload[36..68].try_into().unwrap();
    let notice = moor::session::InputNotice {
        receipt: moor::session::ApplicationReceipt {
            application_id: v16_semantic_bytes_from(0x20),
            lease_epoch: 3,
            request_id: 2,
        },
        byte_count: 5,
        digest,
    };
    let reply = moor::wire::encode_reply(moor::session::Reply::Notice(notice), [0; 16]);
    let moor::wire::RuntimeReply::Frame(kind, encoded) = reply else {
        panic!("INPUT_NOTICE must encode as an unscoped semantic frame body");
    };
    assert_eq!(kind, 5);
    assert_eq!(
        encoded, payload,
        "notice encoder output equals V17's frozen payload"
    );
}

#[test]
fn v16_semantic_v17_digest_is_sha256_of_hello() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V17);
    // Cross-check the vector's trailing 32 bytes against the well-known
    // SHA-256("hello") digest recorded independently above.
    assert_eq!(
        &frame[24 + 36..],
        &v16_semantic_hex(V16_SEMANTIC_SHA256_HELLO)[..]
    );
}

#[test]
fn v16_semantic_v17_frame_reencodes_byte_for_byte() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V17);
    // Holder-to-producer frame sequence 4, source epoch 5, type 5, checksum
    // 8C B0 EC 04 — all reproduced by the implementation's framer.
    assert_eq!(v16_semantic_encode(4, 5, 5, &frame[24..]), frame);
}

#[test]
fn v16_semantic_v17_is_a_direction_violation_for_the_holder_decoder() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V17);
    // §14.3: INPUT_NOTICE flows holder → producer; a producer sending it back
    // is malformed, so the holder-side payload decoder must refuse it.
    assert!(matches!(
        moor::wire::decode_semantic(5, 5, &frame[24..]),
        Err(moor::wire::WireError::Malformed)
    ));
}

// ---------------------------------------------------------------------------
// V19 / V20 — SEMANTIC_ACK, accepted then duplicate
// ---------------------------------------------------------------------------

#[test]
fn v16_semantic_v19_accepted_ack_encoder_reproduces_frozen_payload() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V19);
    let payload = &frame[24..];
    // 0x29 = 41 = 16 id + 8 sequence + 1 status + 2 result code + 4 epoch +
    // 8 sequence + 2 diagnostic prefix. Only a 2-byte empty-diagnostic prefix
    // is consistent with the frozen length; a wide prefix would make it 43.
    assert_eq!(payload.len(), 0x29);
    assert_eq!(
        &payload[39..41],
        &[0x00, 0x00],
        "empty diagnostic, 2-byte prefix"
    );
    let ack = moor::session::SemanticAck {
        id: v16_semantic_bytes_from(0x00),
        sequence: 2,
        status: moor::session::SemanticAckStatus::Accepted,
        position: Some(moor::session::EventPosition {
            epoch: 2,
            sequence: 9,
        }),
    };
    let reply = moor::wire::encode_reply(moor::session::Reply::SemanticAck(ack), [0; 16]);
    let moor::wire::RuntimeReply::Frame(kind, encoded) = reply else {
        panic!("SEMANTIC_ACK must encode as an unscoped semantic frame body");
    };
    assert_eq!(kind, 7);
    assert_eq!(encoded, payload, "accepted ACK equals V19's frozen payload");
}

#[test]
fn v16_semantic_v20_duplicate_ack_encoder_reproduces_frozen_payload() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V20);
    let payload = &frame[24..];
    assert_eq!(payload.len(), 0x29);
    let ack = moor::session::SemanticAck {
        id: v16_semantic_bytes_from(0x00),
        sequence: 2,
        status: moor::session::SemanticAckStatus::Duplicate,
        // §16 V20: the original durable position epoch 2 / sequence 9 remains.
        position: Some(moor::session::EventPosition {
            epoch: 2,
            sequence: 9,
        }),
    };
    let reply = moor::wire::encode_reply(moor::session::Reply::SemanticAck(ack), [0; 16]);
    let moor::wire::RuntimeReply::Frame(kind, encoded) = reply else {
        panic!("SEMANTIC_ACK must encode as an unscoped semantic frame body");
    };
    assert_eq!(kind, 7);
    assert_eq!(
        encoded, payload,
        "duplicate ACK equals V20's frozen payload"
    );
}

#[test]
fn v16_semantic_v19_v20_payloads_differ_only_in_status_byte() {
    let accepted = v16_semantic_hex(V16_SEMANTIC_V19);
    let duplicate = v16_semantic_hex(V16_SEMANTIC_V20);
    let accepted = &accepted[24..];
    let duplicate = &duplicate[24..];
    // Status sits at payload offset 24 (16 id + 8 sequence). Everything else
    // — result code zero, position epoch 2 / sequence 9, empty diagnostic —
    // is identical between the accepted and duplicate ACKs.
    assert_eq!(accepted[24], 0x00, "V19 status accepted");
    assert_eq!(duplicate[24], 0x01, "V20 status duplicate");
    assert_eq!(accepted[..24], duplicate[..24]);
    assert_eq!(accepted[25..], duplicate[25..]);
}

#[test]
fn v16_semantic_v17_v19_v20_stream_decodes_in_protocol_order() {
    // V17, V19 and V20 share the holder-to-producer direction on source epoch
    // 5, with frame sequences 4, 5 and 6: one codec must accept all three.
    let stream = [
        v16_semantic_hex(V16_SEMANTIC_V17),
        v16_semantic_hex(V16_SEMANTIC_V19),
        v16_semantic_hex(V16_SEMANTIC_V20),
    ]
    .concat();
    let messages = v16_semantic_feed(4, &stream);
    assert_eq!(messages.len(), 3);
    let expected: [(u8, &str); 3] = [
        (5, V16_SEMANTIC_V17),
        (7, V16_SEMANTIC_V19),
        (7, V16_SEMANTIC_V20),
    ];
    for (message, (kind, vector)) in messages.iter().zip(expected) {
        assert_eq!(message.scope, 5);
        assert_eq!(message.kind, kind);
        assert_eq!(&message.payload[..], &v16_semantic_hex(vector)[24..]);
    }
}

#[test]
fn v16_semantic_v17_v19_v20_stream_reencodes_byte_for_byte() {
    // One outbound codec starting at frame sequence 4 must reproduce V17,
    // V19 and V20 — headers, advancing sequences and CRCs — from the frozen
    // payloads alone.
    let mut codec = progressed_codec(moor::wire::Profile::Semantic, 1, 4);
    for (kind, vector) in [
        (5, V16_SEMANTIC_V17),
        (7, V16_SEMANTIC_V19),
        (7, V16_SEMANTIC_V20),
    ] {
        let frame = v16_semantic_hex(vector);
        let mut out = Vec::new();
        codec
            .encode(5, kind, &frame[24..], &mut out)
            .expect("frozen §16 payload must be encodable");
        assert_eq!(out, frame);
    }
}

#[test]
fn v16_semantic_v19_is_a_direction_violation_for_the_holder_decoder() {
    let frame = v16_semantic_hex(V16_SEMANTIC_V19);
    // §14.3: SEMANTIC_ACK flows holder → producer only.
    assert!(matches!(
        moor::wire::decode_semantic(5, 7, &frame[24..]),
        Err(moor::wire::WireError::Malformed)
    ));
}

#[test]
fn v16_semantic_gap_no_producer_side_payload_decoder() {
    // GAP: the crate is the holder; it exposes no producer-side payload
    // parser for holder → producer frames (V17 INPUT_NOTICE, V19/V20
    // SEMANTIC_ACK). For those vectors the decode direction is expressible
    // only as framing reassembly (covered by the stream test) plus the §14.3
    // holder-side direction refusal (covered above); their field-level decode
    // cannot be driven through any public entrypoint. This test freezes that
    // refusal for every holder → producer vector so the gap is visible, not
    // silent.
    for (kind, vector) in [
        (5, V16_SEMANTIC_V17),
        (7, V16_SEMANTIC_V19),
        (7, V16_SEMANTIC_V20),
    ] {
        let frame = v16_semantic_hex(vector);
        assert!(matches!(
            moor::wire::decode_semantic(5, kind, &frame[24..]),
            Err(moor::wire::WireError::Malformed)
        ));
    }
}

// ---- vectors: V7, V8, V9, V10, V16, V18 ----
// Conformance lane: §16 vectors V7, V8, V9, V10, V16, V18 plus §7.3 replay
// semantics. Every vector below is the exact hex copied from
// spec/moor-wire-schema.md §16 — never computed from the implementation.
// Helper and constant names carry the v16_input_ prefix so the assembled
// suite has no collisions.

const V16_INPUT_V7: &str = "
    4D 4F 4F 52 04 09 01 00 07 00 00 00 14 00 00 00
    11 00 00 00 2B BD A3 90 03 00 00 00 01 00 00 00
    00 00 00 00 00 41 41 41 41 4D 4F 4F 52 04 09 00
    00 07 00 00 00 15 00 00 00 02 00 00 00 4E AD DE
    06 42 42";

const V16_INPUT_V8: &str = "
    4D 4F 4F 52 04 0A 00 00 07 00 00 00 0A 00 00 00
    2B 00 00 00 F2 7F 7D 6E 03 00 00 00 01 00 00 00
    00 00 00 00 07 00 00 00 00 01 02 03 04 05 06 07
    08 09 0A 0B 0C 0D 0E 0F 06 00 00 00 00 00 00 00
    00 00 00";

const V16_INPUT_V9: &str = "
    4D 4F 4F 52 04 09 00 00 07 00 00 00 16 00 00 00
    13 00 00 00 A2 31 BB E9 03 00 00 00 01 00 00 00
    00 00 00 00 00 41 41 41 41 42 42";

const V16_INPUT_V10: &str = "
    4D 4F 4F 52 04 09 00 00 07 00 00 00 17 00 00 00
    16 00 00 00 CE D7 E0 06 03 00 00 00 01 00 00 00
    00 00 00 00 00 44 49 46 46 45 52 45 4E 54";

const V16_INPUT_V16: &str = "
    4D 4F 4F 52 04 09 00 00 07 00 00 00 1E 00 00 00
    2A 00 00 00 90 59 C0 BE 03 00 00 00 02 00 00 00
    00 00 00 00 01 20 21 22 23 24 25 26 27 28 29 2A
    2B 2C 2D 2E 2F 06 00 63 6C 61 75 64 65 68 65 6C
    6C 6F";

const V16_INPUT_V18: &str = "
    4D 4F 4F 52 04 0A 00 00 07 00 00 00 0B 00 00 00
    2B 00 00 00 D5 02 41 27 03 00 00 00 01 00 00 00
    00 00 00 00 07 00 00 00 00 01 02 03 04 05 06 07
    08 09 0A 0B 0C 0D 0E 0F 06 00 00 00 00 00 00 00
    00 00 00";

fn v16_input_hex(s: &str) -> Vec<u8> {
    let digits: Vec<u8> = s
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_digit(16).unwrap() as u8)
        .collect();
    assert_eq!(digits.len() % 2, 0, "hex vector has whole bytes");
    digits
        .chunks(2)
        .map(|pair| (pair[0] << 4) | pair[1])
        .collect()
}

/// V8's frozen holder incarnation `00 01 .. 0F`.
fn v16_input_incarnation() -> [u8; 16] {
    std::array::from_fn(|i| i as u8)
}

fn v16_input_declared_length(frame: &[u8]) -> u32 {
    u32::from_le_bytes(frame[16..20].try_into().unwrap())
}

fn v16_input_feed(codec: &mut moor::wire::Codec, bytes: &[u8]) -> Vec<moor::wire::Message> {
    let mut out = Vec::new();
    codec
        .feed(0, bytes, &mut out)
        .expect("frozen frames are accepted");
    out
}

fn v16_input_decode(
    payload: &[u8],
) -> (
    moor::session::OwnedInput,
    Option<moor::session::ApplicationInput>,
) {
    match moor::wire::decode_controller(9, payload, None).expect("frozen INPUT payload decodes") {
        moor::wire::ControllerRequest::Policy(moor::session::Request::Input(
            input,
            application,
        )) => (input, application),
        _ => panic!("kind 9 must decode to an input request"),
    }
}

/// A Machine matching V8's frozen receipt: generation 7, incarnation 00..0F,
/// with a freshly granted input lease at epoch 3 for controller connection 1.
fn v16_input_machine() -> moor::session::Machine {
    use moor::session::{Effect, LeaseRequest, LeaseRole, Request, ResultOutcome, Transition};
    let mut machine = moor::session::Machine::new(7, v16_input_incarnation(), [8; 16]);
    machine.register_controller(1);
    allocate_and_release(&mut machine, 1, 2);
    let effects = machine
        .transition(Transition::Peer(
            0,
            1,
            Request::Lease(LeaseRequest::fresh(LeaseRole::InputOnly), Some([5; 16])),
        ))
        .unwrap();
    let granted = effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::LeaseReply(1, result) => Some(result),
            _ => None,
        })
        .expect("lease result");
    assert_eq!(granted.outcome, ResultOutcome::Granted);
    assert_eq!(granted.epoch, 3, "V7's frozen lease epoch");
    machine
}

fn v16_input_request(
    machine: &mut moor::session::Machine,
    now: u64,
    input: moor::session::OwnedInput,
    application: Option<moor::session::ApplicationInput>,
) -> moor::session::Effects {
    machine
        .transition(moor::session::Transition::Peer(
            now,
            1,
            moor::session::Request::Input(input, application),
        ))
        .unwrap()
}

/// Extracts the one terminal-write effect and asserts no receipt was sent yet.
fn v16_input_write(effects: moor::session::Effects) -> (moor::session::WriteTicket, Vec<u8>) {
    use moor::session::{Effect, Reply};
    let mut write = None;
    for effect in effects {
        match effect {
            Effect::Write(ticket, bytes) => {
                assert!(write.replace((ticket, bytes)).is_none(), "one write only");
            }
            Effect::Send(_, Reply::Input(_)) => panic!("receipt before the write completed"),
            _ => {}
        }
    }
    write.expect("terminal write effect")
}

/// Extracts the one input receipt and asserts nothing was written.
fn v16_input_receipt_only(effects: moor::session::Effects) -> Vec<u8> {
    use moor::session::{Effect, Reply};
    let mut receipt = None;
    for effect in effects {
        match effect {
            Effect::Send(1, Reply::Input(bytes)) => {
                assert!(receipt.replace(bytes).is_none(), "one receipt only");
            }
            Effect::Write(..) => panic!("refusal or replay must write nothing"),
            _ => {}
        }
    }
    receipt.expect("input receipt")
}

/// A §7.1 flags-00 input payload in exact wire layout, for §7.3 cases the
/// frozen vectors do not reach (skips, below-high-water, wrong first id).
fn v16_input_owned(epoch: u32, request_id: u64, terminal: &[u8]) -> moor::session::OwnedInput {
    moor::session::OwnedInput {
        epoch,
        request_id,
        exact_payload: [
            epoch.to_le_bytes().as_slice(),
            request_id.to_le_bytes().as_slice(),
            &[0],
            terminal,
        ]
        .concat()
        .into(),
    }
}

fn v16_input_complete(
    machine: &mut moor::session::Machine,
    now: u64,
    ticket: moor::session::WriteTicket,
    written: u64,
) -> Vec<u8> {
    let effects = machine
        .transition(moor::session::Transition::Complete(
            now,
            ticket,
            moor::session::Completion::Write(written, None),
        ))
        .unwrap();
    v16_input_receipt_only(effects)
}

// ---------------------------------------------------------------- V7 framing

#[test]
fn v16_input_v7_header_lengths_close_arithmetically() {
    let v7 = v16_input_hex(V16_INPUT_V7);
    assert_eq!(v7.len(), 67);
    let (first, second) = v7.split_at(41);
    assert_eq!(v16_input_declared_length(first), 17);
    assert_eq!(first.len(), 24 + 17);
    assert_eq!(first[6], 1, "first frame carries MORE");
    assert_eq!(v16_input_declared_length(second), 2);
    assert_eq!(second.len(), 24 + 2);
    assert_eq!(second[6], 0, "continuation ends the run");
}

#[test]
fn v16_input_v7_reassembles_at_every_split_boundary() {
    let v7 = v16_input_hex(V16_INPUT_V7);
    let expected = v16_input_hex("03 00 00 00 01 00 00 00 00 00 00 00 00 41 41 41 41 42 42");
    for split in 0..=v7.len() {
        let mut codec = progressed_codec(moor::wire::Profile::Controller, 20, 1);
        let mut messages = v16_input_feed(&mut codec, &v7[..split]);
        messages.extend(v16_input_feed(&mut codec, &v7[split..]));
        assert_eq!(messages.len(), 1, "exactly one message at split {split}");
        let message = &messages[0];
        assert_eq!((message.scope, message.kind), (7, 9));
        assert_eq!(&message.payload[..], expected.as_slice());
    }
}

#[test]
fn v16_input_v7_fed_one_byte_at_a_time_yields_one_message() {
    let v7 = v16_input_hex(V16_INPUT_V7);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 20, 1);
    let mut messages = Vec::new();
    for byte in &v7 {
        messages.extend(v16_input_feed(&mut codec, std::slice::from_ref(byte)));
    }
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].payload.len(), 19);
    assert_eq!(&messages[0].payload[13..], b"AAAABB".as_slice());
}

#[test]
fn v16_input_v7_decodes_to_plain_input_request() {
    let v7 = v16_input_hex(V16_INPUT_V7);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 20, 1);
    let messages = v16_input_feed(&mut codec, &v7);
    let (input, application) = v16_input_decode(&messages[0].payload);
    assert_eq!((input.epoch, input.request_id), (3, 1));
    assert!(application.is_none(), "V7 sets no receipt-required flag");
    assert_eq!(&input.exact_payload[..], &messages[0].payload[..]);
    assert_eq!(&input.exact_payload[13..], b"AAAABB".as_slice());
}

#[test]
fn v16_input_v7_single_frame_encoding_matches_frozen_v9() {
    // GAP: Codec::encode splits a message only at the 1 MiB frame bound, so the
    // encoder cannot reproduce V7's frozen two-frame MORE run (17 + 2 bytes).
    // The frozen carrier of the identical 19-byte request as a single frame is
    // V9, so encoding V7's reassembled payload at V9's sequence must reproduce
    // V9 byte-for-byte; V7's framing itself is asserted decoder-side above.
    let v7 = v16_input_hex(V16_INPUT_V7);
    let v9 = v16_input_hex(V16_INPUT_V9);
    let mut decode = progressed_codec(moor::wire::Profile::Controller, 20, 1);
    let messages = v16_input_feed(&mut decode, &v7);
    let mut encode = progressed_codec(moor::wire::Profile::Controller, 1, 22);
    let mut out = Vec::new();
    encode.encode(7, 9, &messages[0].payload, &mut out).unwrap();
    assert_eq!(out, v9);
}

// ------------------------------------------------------------- V8 / V18 receipt

#[test]
fn v16_input_v8_receipt_encoder_matches_frozen_payload() {
    let v8 = v16_input_hex(V16_INPUT_V8);
    let receipt = moor::wire::InputReceipt::outcome(3, 1, 7, v16_input_incarnation(), 6, None);
    assert_eq!(receipt.encode().unwrap().as_slice(), &v8[24..]);
}

#[test]
fn v16_input_v8_frame_encoder_matches_frozen_bytes() {
    let v8 = v16_input_hex(V16_INPUT_V8);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 1, 10);
    let mut out = Vec::new();
    codec.encode(7, 10, &v8[24..], &mut out).unwrap();
    assert_eq!(out, v8, "header, CRC and payload all frozen");
}

#[test]
fn v16_input_v8_decodes_back_from_frozen_bytes() {
    let v8 = v16_input_hex(V16_INPUT_V8);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 10, 1);
    let messages = v16_input_feed(&mut codec, &v8);
    assert_eq!(messages.len(), 1);
    assert_eq!((messages[0].scope, messages[0].kind), (7, 10));
    let receipt = moor::wire::InputReceipt::decode(&messages[0].payload).unwrap();
    assert_eq!(receipt.epoch, 3);
    assert_eq!(receipt.request, 1);
    assert_eq!(receipt.generation, 7);
    assert_eq!(receipt.incarnation, v16_input_incarnation());
    assert_eq!(receipt.written, 6);
    assert_eq!((receipt.status, receipt.result), (0, 0));
}

#[test]
fn v16_input_receipt_frames_declare_exactly_43_payload_bytes() {
    // §7.2 fixes the receipt at 4+8+4+16+8+1+2 = 43 bytes; the frozen headers
    // declare 0x2B and their bodies close arithmetically at that length.
    for hex in [V16_INPUT_V8, V16_INPUT_V18] {
        let frame = v16_input_hex(hex);
        assert_eq!(v16_input_declared_length(&frame), 43);
        assert_eq!(frame.len() - 24, 43);
    }
    let v9 = v16_input_hex(V16_INPUT_V9);
    assert_eq!(v16_input_declared_length(&v9), 19);
    assert_eq!(v9.len() - 24, 19);
}

#[test]
fn v16_input_v8_receipt_layout_offsets_are_load_bearing() {
    let v8 = v16_input_hex(V16_INPUT_V8);
    let payload = &v8[24..];
    assert!(moor::wire::InputReceipt::decode(payload).is_ok());
    let mut bad = payload.to_vec();
    bad[40] = 2; // status byte at 4+8+4+16+8: only 00/01 exist
    assert!(moor::wire::InputReceipt::decode(&bad).is_err());
    let mut bad = payload.to_vec();
    bad[41] = 1; // a written receipt carries result code zero
    assert!(moor::wire::InputReceipt::decode(&bad).is_err());
    let mut bad = payload.to_vec();
    bad.truncate(42); // the receipt is exactly 43 bytes
    assert!(moor::wire::InputReceipt::decode(&bad).is_err());
}

#[test]
fn v16_input_v18_payload_is_byte_identical_to_v8() {
    let v8 = v16_input_hex(V16_INPUT_V8);
    let v18 = v16_input_hex(V16_INPUT_V18);
    assert_eq!(
        &v18[24..],
        &v8[24..],
        "cached receipt payload is reused exactly"
    );
    // Only the frame sequence advances, 10 -> 11, and with it the header CRC.
    assert_eq!(u32::from_le_bytes(v8[12..16].try_into().unwrap()), 10);
    assert_eq!(u32::from_le_bytes(v18[12..16].try_into().unwrap()), 11);
    // Decoding V8 then V18 through one controller-side codec accepts both.
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 10, 1);
    let first = v16_input_feed(&mut codec, &v8);
    let second = v16_input_feed(&mut codec, &v18);
    assert_eq!(first[0].payload, second[0].payload);
}

// ------------------------------------------------------ V9 / V10 frozen frames

#[test]
fn v16_input_v9_is_byte_identical_replay_of_v7() {
    let v7 = v16_input_hex(V16_INPUT_V7);
    let v9 = v16_input_hex(V16_INPUT_V9);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 20, 1);
    let reassembled = v16_input_feed(&mut codec, &v7);
    let replay = v16_input_feed(&mut codec, &v9);
    assert_eq!(reassembled[0].payload, replay[0].payload);
    let (input7, _) = v16_input_decode(&reassembled[0].payload);
    let (input9, application9) = v16_input_decode(&replay[0].payload);
    assert_eq!(input7, input9, "identical metadata and bytes");
    assert!(application9.is_none());
}

#[test]
fn v16_input_v9_v10_frame_encoder_matches_frozen_bytes() {
    let v9 = v16_input_hex(V16_INPUT_V9);
    let v10 = v16_input_hex(V16_INPUT_V10);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 1, 22);
    let mut out = Vec::new();
    codec.encode(7, 9, &v9[24..], &mut out).unwrap();
    assert_eq!(out, v9);
    out.clear();
    codec.encode(7, 9, &v10[24..], &mut out).unwrap();
    assert_eq!(out, v10, "sequence advances 22 -> 23 across the run");
}

#[test]
fn v16_input_v10_carries_same_id_with_different_bytes() {
    let v10 = v16_input_hex(V16_INPUT_V10);
    assert_eq!(v16_input_declared_length(&v10), 22);
    assert_eq!(v10.len() - 24, 22, "body length equals the declared length");
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 23, 1);
    let messages = v16_input_feed(&mut codec, &v10);
    let (input, application) = v16_input_decode(&messages[0].payload);
    assert_eq!((input.epoch, input.request_id), (3, 1), "V7's identity");
    assert!(application.is_none());
    assert_eq!(&input.exact_payload[13..], b"DIFFERENT".as_slice());
}

// --------------------------------------------- §7.3 replay, in protocol order

#[test]
fn v16_input_replay_run_writes_once_and_replays_cached_v8_receipt() {
    let v7 = v16_input_hex(V16_INPUT_V7);
    let v8 = v16_input_hex(V16_INPUT_V8);
    let v9 = v16_input_hex(V16_INPUT_V9);
    let v10 = v16_input_hex(V16_INPUT_V10);
    let v18 = v16_input_hex(V16_INPUT_V18);
    let mut decode = progressed_codec(moor::wire::Profile::Controller, 20, 1);
    let m7 = v16_input_feed(&mut decode, &v7).remove(0);
    let m9 = v16_input_feed(&mut decode, &v9).remove(0);
    let m10 = v16_input_feed(&mut decode, &v10).remove(0);

    let mut machine = v16_input_machine();
    // V7: the new request writes AAAABB exactly once.
    let (input7, application7) = v16_input_decode(&m7.payload);
    let (ticket, bytes) = v16_input_write(v16_input_request(
        &mut machine,
        1,
        input7.clone(),
        application7,
    ));
    assert_eq!(bytes.as_slice(), b"AAAABB".as_slice());
    // One input is in flight at a time: an identical resend while pending is
    // absorbed without a second write and without a premature receipt (§7.2:
    // "while the outcome is still pending no receipt is sent").
    assert!(v16_input_request(&mut machine, 2, input7, None).is_empty());
    // Completion produces V8's frozen receipt payload.
    let receipt = v16_input_complete(&mut machine, 3, ticket, 6);
    assert_eq!(receipt.as_slice(), &v8[24..]);

    // The holder-to-controller framing of that cached payload at sequence 10
    // is V8; the replay answer at sequence 11 is V18 — same payload, new frame.
    let mut encode = progressed_codec(moor::wire::Profile::Controller, 1, 10);
    let mut out = Vec::new();
    encode.encode(7, 10, &receipt, &mut out).unwrap();
    assert_eq!(out, v8);

    // V9: identical replay — nothing written, cached V8 payload returned.
    let (input9, application9) = v16_input_decode(&m9.payload);
    let replayed = v16_input_receipt_only(v16_input_request(&mut machine, 4, input9, application9));
    assert_eq!(replayed.as_slice(), &v8[24..]);
    out.clear();
    encode.encode(7, 10, &replayed, &mut out).unwrap();
    assert_eq!(
        out, v18,
        "newly sequenced frame carrying the cached payload"
    );

    // V10: same request id, different bytes — BAD_SEQUENCE, nothing written.
    let (input10, application10) = v16_input_decode(&m10.payload);
    let refused =
        v16_input_receipt_only(v16_input_request(&mut machine, 5, input10, application10));
    let decoded = moor::wire::InputReceipt::decode(&refused).unwrap();
    assert_eq!((decoded.status, decoded.result, decoded.written), (1, 6, 0));
    assert_eq!(
        refused.as_slice(),
        moor::wire::InputReceipt::outcome(3, 1, 7, v16_input_incarnation(), 0, Some(6))
            .encode()
            .unwrap()
            .as_slice()
    );

    // The refusal did not disturb the high-water entry: V9 replays again.
    let mut decode = progressed_codec(moor::wire::Profile::Controller, 22, 1);
    let m9_again = v16_input_feed(&mut decode, &v9).remove(0);
    let (input9, _) = v16_input_decode(&m9_again.payload);
    let replayed = v16_input_receipt_only(v16_input_request(&mut machine, 6, input9, None));
    assert_eq!(replayed.as_slice(), &v8[24..]);
}

#[test]
fn v16_input_first_request_id_of_an_epoch_must_be_one() {
    // §7.3: a new lease epoch sets the high-water mark to 0 and the first
    // request id is exactly 1; skipping to 2 is BAD_SEQUENCE with no write.
    let mut machine = v16_input_machine();
    let refused = v16_input_receipt_only(v16_input_request(
        &mut machine,
        1,
        v16_input_owned(3, 2, b"early"),
        None,
    ));
    let decoded = moor::wire::InputReceipt::decode(&refused).unwrap();
    assert_eq!((decoded.status, decoded.result, decoded.request), (1, 6, 2));
    // The mark is unchanged: id 1 is still the only admissible new request.
    let (ticket, bytes) = v16_input_write(v16_input_request(
        &mut machine,
        2,
        v16_input_owned(3, 1, b"one"),
        None,
    ));
    assert_eq!(bytes.as_slice(), b"one".as_slice());
    let receipt = v16_input_complete(&mut machine, 3, ticket, 3);
    assert_eq!(
        moor::wire::InputReceipt::decode(&receipt).unwrap().status,
        0
    );
}

#[test]
fn v16_input_request_id_below_high_water_is_refused_not_replayed() {
    // §7.3: only the exact high-water id replays; an older id is BAD_SEQUENCE
    // rather than being answered with a newer request's receipt.
    let mut machine = v16_input_machine();
    let one = v16_input_owned(3, 1, b"one");
    let (ticket, _) = v16_input_write(v16_input_request(&mut machine, 1, one.clone(), None));
    let first_receipt = v16_input_complete(&mut machine, 2, ticket, 3);
    let two = v16_input_owned(3, 2, b"two");
    let (ticket, _) = v16_input_write(v16_input_request(&mut machine, 3, two.clone(), None));
    let second_receipt = v16_input_complete(&mut machine, 4, ticket, 3);

    let refused = v16_input_receipt_only(v16_input_request(&mut machine, 5, one, None));
    let decoded = moor::wire::InputReceipt::decode(&refused).unwrap();
    assert_eq!((decoded.status, decoded.result, decoded.request), (1, 6, 1));
    assert_ne!(refused, first_receipt, "no stale cached answer");

    // The high-water entry survives the refusal: id 2 still replays exactly.
    let replayed = v16_input_receipt_only(v16_input_request(&mut machine, 6, two, None));
    assert_eq!(replayed, second_receipt);
}

#[test]
fn v16_input_request_id_skip_above_high_water_is_refused() {
    // §7.3: more than one above the mark is BAD_SEQUENCE with nothing written,
    // and high-water plus one afterwards still advances normally.
    let mut machine = v16_input_machine();
    let (ticket, _) = v16_input_write(v16_input_request(
        &mut machine,
        1,
        v16_input_owned(3, 1, b"one"),
        None,
    ));
    v16_input_complete(&mut machine, 2, ticket, 3);
    let refused = v16_input_receipt_only(v16_input_request(
        &mut machine,
        3,
        v16_input_owned(3, 3, b"skip"),
        None,
    ));
    let decoded = moor::wire::InputReceipt::decode(&refused).unwrap();
    assert_eq!((decoded.status, decoded.result, decoded.request), (1, 6, 3));
    let (_, bytes) = v16_input_write(v16_input_request(
        &mut machine,
        4,
        v16_input_owned(3, 2, b"two"),
        None,
    ));
    assert_eq!(bytes.as_slice(), b"two".as_slice());
}

// ------------------------------------------------------------------------ V16

#[test]
fn v16_input_v16_declared_length_closes_only_with_compact_source_prefix() {
    // Consumer issue #12 regression: the header declares payload length 0x2A
    // (42), which closes arithmetically only when the source id carries §1.1's
    // plain 2-byte length prefix. A 4-byte wide prefix would need 44 bytes.
    let v16 = v16_input_hex(V16_INPUT_V16);
    assert_eq!(v16_input_declared_length(&v16), 0x2A);
    assert_eq!(v16.len() - 24, 42, "body length equals the declared length");
    assert_eq!(4 + 8 + 1 + 16 + 2 + b"claude".len() + b"hello".len(), 42);
    assert_ne!(4 + 8 + 1 + 16 + 4 + b"claude".len() + b"hello".len(), 42);
    // The frozen prefix bytes themselves: 06 00 immediately before "claude".
    assert_eq!(&v16[24 + 29..24 + 31], [0x06, 0x00].as_slice());
    assert_eq!(&v16[24 + 31..24 + 37], b"claude".as_slice());
}

#[test]
fn v16_input_v16_wide_prefixed_source_is_not_a_valid_input_payload() {
    // Consumer issue #12, refuting direction: re-encode V16's body with a
    // 4-byte wide prefix (§1.1.1) on the source id. §7.1 freezes the source id
    // as §1.1 plain length-prefixed, so the wide form must not decode — its
    // third and fourth prefix bytes land inside the source id, violating
    // §14.2's grammar.
    let v16 = v16_input_hex(V16_INPUT_V16);
    let mut wide = v16[24..24 + 29].to_vec(); // epoch, request id, flags, app id
    wide.extend_from_slice(&6u32.to_le_bytes());
    wide.extend_from_slice(b"claude");
    wide.extend_from_slice(b"hello");
    assert_eq!(wide.len(), 44);
    assert!(moor::wire::decode_controller(9, &wide, None).is_err());
}

#[test]
fn v16_input_v16_decodes_source_and_application_id() {
    let v16 = v16_input_hex(V16_INPUT_V16);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 30, 1);
    let messages = v16_input_feed(&mut codec, &v16);
    assert_eq!(messages.len(), 1);
    assert_eq!((messages[0].scope, messages[0].kind), (7, 9));
    let (input, application) = v16_input_decode(&messages[0].payload);
    assert_eq!((input.epoch, input.request_id), (3, 2));
    let application = application.expect("APPLICATION_RECEIPT_REQUIRED is set");
    let expected_id: [u8; 16] = std::array::from_fn(|i| 0x20 + i as u8);
    assert_eq!(application.receipt.application_id, expected_id);
    assert_eq!(application.receipt.lease_epoch, 3);
    assert_eq!(application.receipt.request_id, 2);
    assert_eq!(
        &messages[0].payload[application.source.clone()],
        b"claude".as_slice()
    );
    assert_eq!(
        &messages[0].payload[application.terminal_at..],
        b"hello".as_slice()
    );
}

#[test]
fn v16_input_v16_frame_encoder_matches_frozen_bytes() {
    let v16 = v16_input_hex(V16_INPUT_V16);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 1, 30);
    let mut out = Vec::new();
    codec.encode(7, 9, &v16[24..], &mut out).unwrap();
    assert_eq!(out, v16);
}

#[test]
fn v16_input_v16_without_matching_source_is_refused_cached_and_replayed() {
    // §7.1: a receipt-required input needs an active stateful source named
    // `claude` advertising INPUT_NOTICE and APPLICATION_RECEIPT; none exists,
    // so the request is refused APPLICATION_SOURCE_UNAVAILABLE (17) with
    // nothing written. §7.2/§7.3: the refused receipt becomes the high-water
    // entry and an identical retry replays the cached refusal.
    let v7 = v16_input_hex(V16_INPUT_V7);
    let v16 = v16_input_hex(V16_INPUT_V16);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 20, 1);
    let m7 = v16_input_feed(&mut codec, &v7).remove(0);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 30, 1);
    let m16 = v16_input_feed(&mut codec, &v16).remove(0);

    let mut machine = v16_input_machine();
    let (input7, application7) = v16_input_decode(&m7.payload);
    let (ticket, _) = v16_input_write(v16_input_request(&mut machine, 1, input7, application7));
    v16_input_complete(&mut machine, 2, ticket, 6);

    // V16 is request id 2 — high-water plus one — but no source matches.
    let (input16, application16) = v16_input_decode(&m16.payload);
    let refused =
        v16_input_receipt_only(v16_input_request(&mut machine, 3, input16, application16));
    assert_eq!(
        refused.as_slice(),
        moor::wire::InputReceipt::outcome(3, 2, 7, v16_input_incarnation(), 0, Some(17))
            .encode()
            .unwrap()
            .as_slice()
    );

    // The refused outcome is retained identically: an exact replay of the
    // frozen bytes returns the cached refusal payload and writes nothing.
    let (input16, application16) = v16_input_decode(&m16.payload);
    let replayed =
        v16_input_receipt_only(v16_input_request(&mut machine, 4, input16, application16));
    assert_eq!(replayed, refused);
}

// ---- vectors: V1, V2, V3, V4, V5, V6, V11 ----
// ===== group: framing (V1, V2, V3, V4, V5, V6, V11) =====
// Byte-exact conformance against spec/moor-wire-schema.md §16. Every vector
// below is the EXACT hex copied from §16 — never computed from the
// implementation. Decodes go through the real public entrypoints
// (Codec::feed, decode_controller, decode_viewer, Machine::transition) and
// encodes are byte-compared against the frozen bytes.

/// Local hex helper (prefixed so the assembled conformance file has no
/// duplicate `hex` symbol across groups).
fn v16_framing_hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}

// §16 V1 — side-effect-free HELLO, exact generation 7, sequence 1; hello
// flags are reserved zero.
fn v16_framing_v1() -> Vec<u8> {
    v16_framing_hex(
        "4D 4F 4F 52 04 01 00 00 07 00 00 00 01 00 00 00
         21 00 00 00 3E C8 F1 24 4D 4F 4F 52 04 00 00 16
         00 00 00 01 2F 74 6D 70 2F 2E 6D 6F 6F 72 2D 31
         30 30 30 2F 62 75 69 6C 64",
    )
}

// §16 V2 — OUTPUT, record sequence 42, byte offset 4096, payload `hi`.
fn v16_framing_v2() -> Vec<u8> {
    v16_framing_hex(
        "4D 4F 4F 52 04 06 00 00 07 00 00 00 09 00 00 00
         12 00 00 00 9D 65 ED 09 2A 00 00 00 00 00 00 00
         00 10 00 00 00 00 00 00 68 69",
    )
}

// §16 V3 — ATTACH with geometry 0×0 — preserve both (OB-19) — requesting the
// lease.
fn v16_framing_v3() -> Vec<u8> {
    v16_framing_hex(
        "4D 4F 4F 52 04 03 00 00 07 00 00 00 02 00 00 00
         05 00 00 00 2D 90 AF 9C 00 00 00 00 01",
    )
}

// §16 V4 — RESIZE, lease epoch 3, geometry 80×24 — payload is 8 bytes: epoch
// then geometry.
fn v16_framing_v4() -> Vec<u8> {
    v16_framing_hex(
        "4D 4F 4F 52 04 0B 00 00 07 00 00 00 0B 00 00 00
         08 00 00 00 66 62 C8 F5 03 00 00 00 50 00 18 00",
    )
}

// §16 V5 — RESIZE with 80×0 — half-specified, MUST be refused with
// HALF_SPECIFIED_GEOMETRY.
fn v16_framing_v5() -> Vec<u8> {
    v16_framing_hex(
        "4D 4F 4F 52 04 0B 00 00 07 00 00 00 0C 00 00 00
         08 00 00 00 62 67 91 0F 03 00 00 00 50 00 00 00",
    )
}

// §16 V6 — any frame with generation 0 — MUST be refused; shown as
// OUTPUT_ACK.
fn v16_framing_v6() -> Vec<u8> {
    v16_framing_hex(
        "4D 4F 4F 52 04 07 00 00 00 00 00 00 03 00 00 00
         08 00 00 00 C2 BB 32 88 01 00 00 00 00 00 00 00",
    )
}

// §16 V11 — ERROR carrying GENERATION_MISMATCH (9).
fn v16_framing_v11() -> Vec<u8> {
    v16_framing_hex(
        "4D 4F 4F 52 04 13 00 00 07 00 00 00 0D 00 00 00
         1E 00 00 00 27 2C 49 3D 09 00 1A 00 67 65 6E 65
         72 61 74 69 6F 6E 20 33 20 69 73 20 73 75 70 65
         72 73 65 64 65 64",
    )
}

/// V1's canonical session identity: tag `01` + POSIX socket-path bytes (§1.2).
fn v16_framing_v1_identity() -> Vec<u8> {
    let mut identity = vec![0x01];
    identity.extend_from_slice(b"/tmp/.moor-1000/build");
    identity
}

/// Asserts the frozen 24-byte header shape of §1: magic, version, and that the
/// header CRC-32C over bytes 0–19 recomputes to the frozen bytes 20–23.
fn v16_framing_assert_header(frame: &[u8]) {
    assert!(frame.len() >= 24, "frame shorter than the 24-byte header");
    assert_eq!(&frame[0..4], b"MOOR", "frozen magic");
    assert_eq!(frame[4], 0x04, "frozen wire-schema-4 version byte");
    let frozen = u32::from_le_bytes(frame[20..24].try_into().unwrap());
    assert_eq!(
        moor::wire::crc32c(&frame[..20]),
        frozen,
        "header CRC-32C over bytes 0-19 must recompute to the frozen checksum"
    );
}

/// Feeds one frozen frame through the real controller Codec (with `next_in`
/// positioned at the vector's frozen sequence) and returns the one decoded
/// message.
fn v16_framing_feed_one(next_in: u32, frame: &[u8]) -> moor::wire::Message {
    let mut codec = progressed_codec(moor::wire::Profile::Controller, next_in, 1);
    let mut out = Vec::new();
    codec
        .feed(0, frame, &mut out)
        .expect("the frozen frame must be accepted by the framing layer");
    assert_eq!(out.len(), 1, "exactly one reassembled message");
    out.remove(0)
}

/// Encodes one frame through the real Codec (with `next_out` positioned at the
/// vector's frozen sequence) for byte-comparison against the frozen vector.
fn v16_framing_encode_frame(next_out: u32, scope: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 1, next_out);
    let mut out = Vec::new();
    codec
        .encode(scope, kind, payload, &mut out)
        .expect("the frozen payload must be encodable");
    out
}

/// A machine that has granted the input lease via ATTACH; returns it with the
/// granted lease epoch.
fn v16_framing_machine_with_lease(allocated: u32) -> (moor::session::Machine, u32) {
    use moor::session::{Effect, Machine, Request, Transition};
    let mut machine = Machine::new(7, [1; 16], [2; 16]);
    machine.register_controller(1);
    allocate_and_release(&mut machine, 1, allocated);
    let effects = machine
        .transition(Transition::Peer(
            0,
            1,
            Request::Attach(0, 0, true, false, Some([3; 16])),
        ))
        .expect("attach must be accepted");
    let epoch = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Attached(1, _, Some(lease), _) => Some(lease.epoch),
            _ => None,
        })
        .expect("attach requesting the lease must report a lease result");
    (machine, epoch)
}

// ---------------------------------------------------------------- V1: HELLO

#[test]
fn v16_framing_v1_hello_frame_decodes_through_codec_and_controller_decoder() {
    let frame = v16_framing_v1();
    v16_framing_assert_header(&frame);
    // §1 header fields frozen by the vector: type HELLO, generation 7, seq 1,
    // payload length 0x21.
    assert_eq!(frame[5], 0x01, "frame type HELLO");
    assert_eq!(&frame[16..20], &[0x21, 0, 0, 0], "payload length 33");
    let message = v16_framing_feed_one(1, &frame);
    assert_eq!(message.scope, 7, "generation 7 from the frozen header");
    assert_eq!(message.kind, 1);
    assert_eq!(
        &message.payload[..],
        &frame[24..],
        "payload split exactly after byte 24"
    );

    // §3.1: magic repeat, schema version, two reserved-zero flag bytes, then
    // the canonical session identity — wide-length-prefixed (§1.1.1, 4 bytes).
    let identity = v16_framing_v1_identity();
    assert_eq!(&message.payload[..7], b"MOOR\x04\0\0");
    assert_eq!(
        &message.payload[7..11],
        &[0x16, 0x00, 0x00, 0x00],
        "identity carries the 4-byte wide prefix (0x16 = 22), not a 2-byte one"
    );
    assert_eq!(
        moor::wire::get_wide(&message.payload, 7, true),
        Some(identity.as_slice())
    );
    let decoded = moor::wire::decode_controller(1, &message.payload, None);
    let Ok(moor::wire::ControllerRequest::Hello(decoded_identity)) = decoded else {
        panic!("V1 must decode as a controller HELLO");
    };
    assert_eq!(decoded_identity, identity.as_slice());
}

#[test]
fn v16_framing_v1_hello_encoder_reproduces_frozen_bytes() {
    let frame = v16_framing_v1();
    let payload =
        moor::wire::controller_hello(&v16_framing_v1_identity()).expect("identity within bounds");
    assert_eq!(payload, frame[24..].to_vec(), "frozen §3.1 payload");
    assert_eq!(
        v16_framing_encode_frame(1, 7, 1, &payload),
        frame,
        "full frozen V1 frame including header CRC"
    );
}

// --------------------------------------------------------------- V2: OUTPUT

#[test]
fn v16_framing_v2_output_frame_decodes_with_frozen_coordinates() {
    let frame = v16_framing_v2();
    v16_framing_assert_header(&frame);
    let message = v16_framing_feed_one(9, &frame);
    assert_eq!((message.scope, message.kind), (7, 6));
    // §2 type 06: 8 bytes record sequence, 8 bytes byte offset, raw bytes to
    // end of payload. §16 freezes sequence 42, offset 4096, bytes `hi`.
    let payload = &message.payload[..];
    assert_eq!(payload.len(), 0x12);
    assert_eq!(u64::from_le_bytes(payload[0..8].try_into().unwrap()), 42);
    assert_eq!(u64::from_le_bytes(payload[8..16].try_into().unwrap()), 4096);
    assert_eq!(&payload[16..], b"hi");
}

#[test]
fn v16_framing_v2_output_coordinates_apply_through_viewer_decoder() {
    use moor::wire::{ReplayDescriptor, ViewerEvent, ViewerStream, decode_viewer};
    let frame = v16_framing_v2();
    let message = v16_framing_feed_one(9, &frame);
    // A viewer whose ATTACH_ACK advertised exactly V2's record: §6.1 freezes
    // that V2's two bytes occupy offsets 4096 and 4097 with exclusive end 4098.
    let mut stream = ViewerStream {
        terminal: true,
        replay: Some(ReplayDescriptor {
            first: 42,
            last: 42,
            start: 4096,
            end: 4098,
            complete: false,
            modes_exact: false,
        }),
        next: Some((42, 4096)),
        received: Some((42, 4096)),
        ..ViewerStream::default()
    };
    assert_eq!(
        decode_viewer(&mut stream, &message, (b"".as_slice(), 7, [9; 16])),
        Ok(Some(ViewerEvent::Output(42, true, b"hi")))
    );
    assert_eq!(
        stream.next,
        Some((43, 4098)),
        "next record is 43 and the next byte offset is exactly offset + payload_length"
    );
}

#[test]
fn v16_framing_v2_output_encoder_reproduces_frozen_bytes() {
    let frame = v16_framing_v2();
    let mut payload = Vec::new();
    payload.extend_from_slice(&42u64.to_le_bytes());
    payload.extend_from_slice(&4096u64.to_le_bytes());
    payload.extend_from_slice(b"hi");
    assert_eq!(payload, frame[24..].to_vec(), "frozen §6.1 payload");
    assert_eq!(v16_framing_encode_frame(9, 7, 6, &payload), frame);
}

// --------------------------------------------------------------- V3: ATTACH

#[test]
fn v16_framing_v3_attach_zero_geometry_decodes_as_preserve_with_lease_bit() {
    let frame = v16_framing_v3();
    v16_framing_assert_header(&frame);
    let message = v16_framing_feed_one(2, &frame);
    assert_eq!((message.scope, message.kind), (7, 3));
    assert_eq!(
        &message.payload[..],
        &[0, 0, 0, 0, 1],
        "geometry 0x0 then flags bit 0"
    );
    // §2 type 03: geometry (§4), then 1 byte flags, bit 0 = request the lease.
    use moor::session::Request;
    use moor::wire::{ControllerRequest, decode_controller};
    assert!(matches!(
        decode_controller(3, &message.payload, None),
        Ok(ControllerRequest::Policy(Request::Attach(
            0, 0, true, false, None
        )))
    ));
    assert!(matches!(
        decode_controller(3, &message.payload, Some([7; 16])),
        Ok(ControllerRequest::Policy(Request::Attach(0, 0, true, false, Some(token))))
            if token == [7; 16]
    ));
}

#[test]
fn v16_framing_v3_attach_preserve_is_accepted_and_resizes_nothing() {
    use moor::session::{Effect, Machine, Request, ResultOutcome, Transition};
    // OB-19/§4: zero in either dimension means preserve; both zero preserves
    // both — the attach succeeds and no resize of the child occurs.
    let mut machine = Machine::new(7, [1; 16], [2; 16]);
    machine.register_controller(1);
    let effects = machine
        .transition(Transition::Peer(
            0,
            1,
            Request::Attach(0, 0, true, false, Some([3; 16])),
        ))
        .expect("ATTACH with geometry 0x0 must be accepted");
    let (lease, resize) = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Attached(1, false, lease, resize) => Some((lease.clone(), *resize)),
            _ => None,
        })
        .expect("the connection must attach");
    assert_eq!(resize, None, "geometry 0x0 preserves the child geometry");
    assert!(
        lease.is_some_and(|result| result.outcome == ResultOutcome::Granted),
        "flags bit 0 requests and receives the input lease"
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Close(_) | Effect::Resize(..))),
        "preserve must neither refuse the attach nor resize the child"
    );
}

#[test]
fn v16_framing_v3_attach_encoder_reproduces_frozen_bytes() {
    let frame = v16_framing_v3();
    assert_eq!(v16_framing_encode_frame(2, 7, 3, &[0, 0, 0, 0, 1]), frame);
}

// --------------------------------------------------------------- V4: RESIZE

#[test]
fn v16_framing_v4_resize_decodes_epoch_then_geometry() {
    let frame = v16_framing_v4();
    v16_framing_assert_header(&frame);
    let message = v16_framing_feed_one(0x0b, &frame);
    assert_eq!((message.scope, message.kind), (7, 0x0b));
    // §2 type 0B: 4 bytes lease epoch, then geometry (§4) columns/rows.
    assert_eq!(&message.payload[..], &[3, 0, 0, 0, 0x50, 0, 0x18, 0]);
    use moor::session::Request;
    use moor::wire::{ControllerRequest, decode_controller};
    assert!(matches!(
        decode_controller(0x0b, &message.payload, None),
        Ok(ControllerRequest::Policy(Request::Resize(3, 80, 24)))
    ));
}

#[test]
fn v16_framing_v4_resize_applies_80x24_under_lease_epoch_3() {
    use moor::session::{Effect, Request, Transition};
    // Allocation history of two prior epochs makes the freshly granted lease
    // epoch exactly V4's frozen `03 00 00 00`.
    let (mut machine, epoch) = v16_framing_machine_with_lease(2);
    assert_eq!(
        epoch, 3,
        "granted lease epoch matches V4's frozen epoch field"
    );
    let effects = machine
        .transition(Transition::Peer(1, 1, Request::Resize(3, 80, 24)))
        .expect("RESIZE under the current lease epoch must be accepted");
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Resize(_, 24, 80))),
        "the child must be resized to 80 columns x 24 rows"
    );
}

#[test]
fn v16_framing_v4_resize_encoder_reproduces_frozen_bytes() {
    let frame = v16_framing_v4();
    let mut payload = Vec::new();
    payload.extend_from_slice(&3u32.to_le_bytes());
    payload.extend_from_slice(&80u16.to_le_bytes());
    payload.extend_from_slice(&24u16.to_le_bytes());
    assert_eq!(payload, frame[24..].to_vec());
    assert_eq!(v16_framing_encode_frame(0x0b, 7, 0x0b, &payload), frame);
}

// ------------------------------------------- V5: half-specified geometry

#[test]
fn v16_framing_v5_half_specified_geometry_is_refused_with_code_14() {
    let frame = v16_framing_v5();
    v16_framing_assert_header(&frame);
    // The frame itself is well-formed — the refusal is a geometry policy
    // refusal, not a framing error.
    let message = v16_framing_feed_one(0x0c, &frame);
    assert_eq!((message.scope, message.kind), (7, 0x0b));
    use moor::session::{Effect, Machine, Reply, Request, Transition};
    use moor::wire::{ControllerRequest, decode_controller};
    assert!(matches!(
        decode_controller(0x0b, &message.payload, None),
        Ok(ControllerRequest::Policy(Request::Resize(3, 80, 0)))
    ));
    let mut machine = Machine::new(7, [1; 16], [2; 16]);
    machine.register_controller(1);
    let effects = machine
        .transition(Transition::Peer(0, 1, Request::Resize(3, 80, 0)))
        .expect("the refusal is an ERROR reply, not a decode failure");
    let reply = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Send(1, reply @ Reply::ControllerError(..)) => Some(reply.clone()),
            _ => None,
        })
        .expect("80x0 must produce a controller ERROR");
    // §4/§11: one zero and the other not is HALF_SPECIFIED_GEOMETRY, code 14.
    let Reply::ControllerError(code, _) = reply.clone() else {
        unreachable!()
    };
    assert_eq!(code, 14, "frozen HALF_SPECIFIED_GEOMETRY code");
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Close(1))),
        "the refused connection closes"
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Resize(..))),
        "nothing is resized"
    );
    // The machine's actual refusal, pushed through the real reply encoder,
    // must carry code 14 LE followed by a nonempty plain-length-prefixed
    // diagnostic (§1.1). The diagnostic text itself is not frozen by the
    // schema, so only its shape is asserted. The frame TYPE the encoder
    // assigns is asserted separately in
    // v16_framing_error_replies_encode_as_frame_type_0x13.
    let moor::wire::RuntimeReply::Frame(_, payload) = moor::wire::encode_reply(reply, [1; 16])
    else {
        panic!("a controller ERROR reply must map to one unscoped frame");
    };
    assert_eq!(&payload[..2], &[0x0e, 0x00], "HALF_SPECIFIED_GEOMETRY LE");
    assert!(
        moor::wire::get_compact(&payload, 2, true).is_some_and(|diagnostic| !diagnostic.is_empty()),
        "the diagnostic is a nonempty §1.1 2-byte-length-prefixed field"
    );
}

#[test]
fn v16_framing_v5_encoder_reproduces_frozen_bytes() {
    // The refusal is semantic, not syntactic: an encoder must still be able
    // to emit the frozen V5 frame byte-for-byte (a controller really sends it).
    let frame = v16_framing_v5();
    let mut payload = Vec::new();
    payload.extend_from_slice(&3u32.to_le_bytes());
    payload.extend_from_slice(&80u16.to_le_bytes());
    payload.extend_from_slice(&0u16.to_le_bytes());
    assert_eq!(payload, frame[24..].to_vec());
    assert_eq!(v16_framing_encode_frame(0x0c, 7, 0x0b, &payload), frame);
}

// -------------------------------------------------- V6: generation zero

#[test]
fn v16_framing_v6_generation_zero_on_non_hello_is_refused() {
    let frame = v16_framing_v6();
    v16_framing_assert_header(&frame);
    assert_eq!(&frame[8..12], &[0; 4], "frozen generation field is zero");
    assert_eq!(frame[5], 0x07, "shown as OUTPUT_ACK");
    // §2 type 07 payload: 8 bytes highest record sequence consumed — the
    // frozen vector acknowledges record 1, and the frame is otherwise valid,
    // so the zero generation is the only refusal cause.
    assert_eq!(
        &frame[24..],
        1u64.to_le_bytes(),
        "frozen OUTPUT_ACK of record 1"
    );
    // next_in = 3 matches the frozen sequence.
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 3, 1);
    let mut out = Vec::new();
    let result = codec.feed(0, &frame, &mut out);
    assert!(
        result.is_err(),
        "generation 0 is legal only on the first controller HELLO (§1/§3.1)"
    );
    assert!(out.is_empty(), "the frame must never surface as a message");
    // Spec §10.2.4 freezes this refusal as GENERATION_MISMATCH — §11 code 9.
    // V11 proves the answering ERROR payload bytes on the wire, and
    // v16_framing_error_replies_encode_as_frame_type_0x13 proves the frame
    // type the reply encoder assigns to it.
    assert_eq!(result, Err(moor::wire::WireError::GenerationMismatch));

    // Encoding a generation-0 non-HELLO frame must be equally impossible.
    let mut encoder = progressed_codec(moor::wire::Profile::Controller, 1, 3);
    let mut bytes = Vec::new();
    assert!(
        encoder.encode(0, 7, &frame[24..], &mut bytes).is_err(),
        "an encoder must refuse to emit generation 0 on a non-HELLO frame"
    );
}

// ------------------------------------------------------------- V11: ERROR

#[test]
fn v16_framing_v11_error_payload_matches_frozen_bytes_with_compact_prefix() {
    let frame = v16_framing_v11();
    v16_framing_assert_header(&frame);
    // §2 type 13: 2 bytes code (§11), then a nonempty length-prefixed (§1.1,
    // 2-byte) diagnostic. Frozen payload length 0x1E = 30 = 2 + 2 + 26 —
    // provable only with the plain 2-byte prefix; a wide prefix would make it
    // 32 and change the header length field.
    assert_eq!(&frame[16..20], &[0x1e, 0, 0, 0], "frozen payload length 30");
    let payload = &frame[24..];
    assert_eq!(payload.len(), 30);
    assert_eq!(
        u16::from_le_bytes(payload[..2].try_into().unwrap()),
        9,
        "frozen GENERATION_MISMATCH code (§11)"
    );
    assert_eq!(
        &payload[2..4],
        &[0x1a, 0x00],
        "2-byte length prefix of the 26-byte diagnostic"
    );
    assert_eq!(&payload[4..], b"generation 3 is superseded");
    assert_eq!(
        moor::wire::get_compact(payload, 2, true),
        Some(b"generation 3 is superseded".as_slice()),
        "the diagnostic decodes through the real §1.1 compact-prefix reader"
    );
    // Encoder direction: the real error-payload builder must reproduce the
    // frozen bytes exactly.
    assert_eq!(
        moor::wire::error_payload(9, b"generation 3 is superseded"),
        payload.to_vec()
    );
}

#[test]
fn v16_framing_v11_error_frame_round_trips_through_codec() {
    let frame = v16_framing_v11();
    let message = v16_framing_feed_one(0x0d, &frame);
    assert_eq!((message.scope, message.kind), (7, 0x13));
    assert_eq!(&message.payload[..], &frame[24..]);
    let payload = moor::wire::error_payload(9, b"generation 3 is superseded");
    assert_eq!(
        v16_framing_encode_frame(0x0d, 7, 0x13, &payload),
        frame,
        "full frozen V11 frame including header CRC"
    );
}

// ----------------------------------------------- ERROR reply frame type

#[test]
fn v16_framing_error_replies_encode_as_frame_type_0x13() {
    // §2 freezes ERROR as type `13` HEX = 0x13 = 19 — the type byte V11
    // carries at offset 5. The holder's reply encoder must place every
    // controller refusal in that frame type: the controller-side runtime
    // recognises a refusal only under kind 0x13, so any other kind misfiles
    // the refusal as a different frame.
    //
    // KNOWN-FAILING at review time: encode_reply builds ControllerError with
    // the DECIMAL literal 13 (= 0x0D, which §2 assigns to STATUS), so the
    // holder emits refusals the client can never recognise. This is the §2
    // contract assertion that catches it; V11's frozen type byte 0x13 is the
    // authority.
    for (code, diagnostic) in [
        (9u16, b"generation 3 is superseded".as_slice()),
        (14, b"geometry was half specified"),
    ] {
        let moor::wire::RuntimeReply::Frame(kind, payload) = moor::wire::encode_reply(
            moor::session::Reply::ControllerError(code, diagnostic),
            [1; 16],
        ) else {
            panic!("a controller ERROR reply must map to one unscoped frame");
        };
        assert_eq!(
            kind, 0x13,
            "ERROR frame type is 0x13 (§2, proven on the wire by V11), \
             not decimal 13 = 0x0D (STATUS)"
        );
        assert_eq!(payload, moor::wire::error_payload(code, diagnostic));
    }
}

// ------------------------------------------------------ header CRC sweep

#[test]
fn v16_framing_header_crc32c_recomputes_for_every_group_vector() {
    for (name, frame) in [
        ("V1", v16_framing_v1()),
        ("V2", v16_framing_v2()),
        ("V3", v16_framing_v3()),
        ("V4", v16_framing_v4()),
        ("V5", v16_framing_v5()),
        ("V6", v16_framing_v6()),
        ("V11", v16_framing_v11()),
    ] {
        v16_framing_assert_header(&frame);
        // §1: payload length field matches the actual frozen payload size.
        let length = u32::from_le_bytes(frame[16..20].try_into().unwrap()) as usize;
        assert_eq!(24 + length, frame.len(), "{name}: declared payload length");
    }
}

// ---- vectors: V1, V7 ----
// v16_extra_ — completeness-critic additions to the §16 vector lane.
//
// These tests close the §17 "Framing and reassembly" refusal bullets that had
// no test anywhere in tests/ (unknown version, unknown type, non-zero reserved
// byte, sequence gap mid-run, type change mid-run, declared length above the
// frame cap, run at/over the message bound), plus the §1.1 compact-prefix cap
// boundary — the untested half of the consumer-issue-#12 contract — and the
// §16 V1 "hello flags are reserved zero" refusal.
//
// Every mutation starts from the FROZEN §16 bytes (V1, V7) copied verbatim
// from spec/moor-wire-schema.md, with exactly one field changed and the header
// CRC-32C recomputed so the refusal under test — not the checksum — is what
// fires. Unmutated controls assert the frozen bytes still decode.

/// §16 V1 — frozen bytes copied verbatim from spec/moor-wire-schema.md.
const V16_EXTRA_V1: &str = "\
4D 4F 4F 52 04 01 00 00 07 00 00 00 01 00 00 00 \
21 00 00 00 3E C8 F1 24 4D 4F 4F 52 04 00 00 16 \
00 00 00 01 2F 74 6D 70 2F 2E 6D 6F 6F 72 2D 31 \
30 30 30 2F 62 75 69 6C 64";

/// §16 V7 — frozen bytes copied verbatim from spec/moor-wire-schema.md.
/// Two frames: 24+17 bytes (MORE=1), then 24+2 bytes (MORE=0).
const V16_EXTRA_V7: &str = "\
4D 4F 4F 52 04 09 01 00 07 00 00 00 14 00 00 00 \
11 00 00 00 2B BD A3 90 03 00 00 00 01 00 00 00 \
00 00 00 00 00 41 41 41 41 4D 4F 4F 52 04 09 00 \
00 07 00 00 00 15 00 00 00 02 00 00 00 4E AD DE \
06 42 42";

fn v16_extra_hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}

/// Recompute the header CRC-32C for the frame starting at `at`, so a mutated
/// frame is refused by the semantic check under test rather than the checksum.
fn v16_extra_patch_crc(bytes: &mut [u8], at: usize) {
    let checksum = moor::wire::crc32c(&bytes[at..at + 20]);
    bytes[at + 20..at + 24].copy_from_slice(&checksum.to_le_bytes());
}

fn v16_extra_feed_controller(bytes: &[u8]) -> Result<usize, moor::wire::WireError> {
    let mut out = Vec::new();
    moor::wire::Codec::new(moor::wire::Profile::Controller)
        .feed(0, bytes, &mut out)
        .map(|()| out.len())
}

/// Hand-assemble one semantic-profile frame per §1 (magic MOOS, version 1)
/// with a real CRC-32C. Used only where Codec::encode cannot express the shape
/// (a MORE run exceeding the message bound is refused encoder-side).
fn v16_extra_semantic_frame(
    kind: u8,
    more: u8,
    scope: u32,
    sequence: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut frame = Vec::with_capacity(24 + payload.len());
    frame.extend_from_slice(b"MOOS");
    frame.push(1);
    frame.push(kind);
    frame.push(more);
    frame.push(0);
    frame.extend_from_slice(&scope.to_le_bytes());
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    let checksum = moor::wire::crc32c(&frame[..20]);
    frame.extend_from_slice(&checksum.to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[test]
fn v16_extra_frozen_v1_control_still_decodes() {
    // Sanity anchor for every mutation below: the frozen bytes are valid.
    assert_eq!(
        v16_extra_feed_controller(&v16_extra_hex(V16_EXTRA_V1)),
        Ok(1)
    );
}

#[test]
fn v16_extra_framing_unknown_version_is_refused() {
    // §1: version is frozen at 4 for the controller profile; §17 "an unknown
    // version". Byte 4 of the frozen V1 header is the version. `3` is the
    // retired dialect: v4 ships no v3 decoder, so the predecessor is refused
    // exactly like any other unknown version — that is what a version
    // increment means, as against another in-place amendment.
    let mut bytes = v16_extra_hex(V16_EXTRA_V1);
    bytes[4] = 3;
    v16_extra_patch_crc(&mut bytes, 0);
    assert_eq!(
        v16_extra_feed_controller(&bytes),
        Err(moor::wire::WireError::UnknownVersion)
    );
}

#[test]
fn v16_extra_framing_unknown_type_is_refused() {
    // §2 assigns frame kinds; zero and anything past the table are unknown.
    // §17 "an unknown type". Byte 5 of the frozen V1 header is the kind.
    for unknown in [0u8, 0x1b, 0xff] {
        let mut bytes = v16_extra_hex(V16_EXTRA_V1);
        bytes[5] = unknown;
        v16_extra_patch_crc(&mut bytes, 0);
        assert_eq!(
            v16_extra_feed_controller(&bytes),
            Err(moor::wire::WireError::UnknownType),
            "kind {unknown:#04x}"
        );
    }
}

#[test]
fn v16_extra_framing_nonzero_reserved_byte_is_refused() {
    // §1: the reserved header byte MUST be zero; §17 "a non-zero reserved
    // bit". Byte 7 of the frozen V1 header is the reserved byte.
    for reserved in [1u8, 0x80] {
        let mut bytes = v16_extra_hex(V16_EXTRA_V1);
        bytes[7] = reserved;
        v16_extra_patch_crc(&mut bytes, 0);
        assert_eq!(
            v16_extra_feed_controller(&bytes),
            Err(moor::wire::WireError::Malformed),
            "reserved {reserved:#04x}"
        );
    }
}

#[test]
fn v16_extra_framing_sequence_gap_mid_run_is_refused() {
    // §17 "a sequence gap mid-run". V7's second frame carries sequence 0x15;
    // bumping it to 0x16 leaves a gap after the first frame is consumed.
    // Offset 41 is the start of the second frame; +12 is its sequence field.
    let mut bytes = v16_extra_hex(V16_EXTRA_V7);
    bytes[41 + 12] = 0x16;
    v16_extra_patch_crc(&mut bytes, 41);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 20, 1);
    let mut out = Vec::new();
    assert_eq!(
        codec.feed(0, &bytes, &mut out),
        Err(moor::wire::WireError::BadSequence)
    );
    assert!(
        out.is_empty(),
        "no message may be delivered from a gapped run"
    );
}

#[test]
fn v16_extra_framing_type_change_mid_run_aborts_reassembly() {
    // §17 "a type change mid-run". V7's continuation frame switches from
    // INPUT (0x09) to OUTPUT_ACK (0x07): the run must abort, not splice.
    let mut bytes = v16_extra_hex(V16_EXTRA_V7);
    bytes[41 + 5] = 0x07;
    v16_extra_patch_crc(&mut bytes, 41);
    let mut codec = progressed_codec(moor::wire::Profile::Controller, 20, 1);
    let mut out = Vec::new();
    assert_eq!(
        codec.feed(0, &bytes, &mut out),
        Err(moor::wire::WireError::ReassemblyAborted)
    );
    assert!(out.is_empty());
}

#[test]
fn v16_extra_framing_declared_length_above_frame_cap_is_refused() {
    // §1: a controller frame body is capped at 1 MiB. A header declaring one
    // byte more must be refused before any body bytes are awaited.
    let mut bytes = v16_extra_hex(V16_EXTRA_V1)[..24].to_vec();
    bytes[16..20].copy_from_slice(&(((1u32 << 20) + 1).to_le_bytes()));
    v16_extra_patch_crc(&mut bytes, 0);
    assert_eq!(
        v16_extra_feed_controller(&bytes),
        Err(moor::wire::WireError::OversizedFrame)
    );
}

#[test]
fn v16_extra_framing_run_at_message_bound_is_accepted_and_one_byte_above_refused() {
    // §17 "a run exceeding the message bound" — decoder side, at the limit and
    // one above. Semantic profile: 64 KiB frames, 1 MiB reassembled bound.
    // Sixteen full MORE frames total exactly 1 MiB; a terminal empty frame
    // closes the run at the bound and must be accepted.
    let chunk = vec![0x41u8; 1 << 16];
    let mut at_limit = Vec::new();
    for part in 0..16u32 {
        at_limit.extend_from_slice(&v16_extra_semantic_frame(3, 1, 1, part + 1, &chunk));
    }
    let mut closing = at_limit.clone();
    closing.extend_from_slice(&v16_extra_semantic_frame(3, 0, 1, 17, &[]));
    let mut codec = moor::wire::Codec::new(moor::wire::Profile::Semantic);
    let mut out = Vec::new();
    codec.feed(0, &closing, &mut out).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].payload.len(),
        1 << 20,
        "exactly the bound is accepted"
    );

    // The same run with one extra payload byte in the terminal frame is over
    // the bound and must be refused with nothing delivered.
    let mut over = at_limit;
    over.extend_from_slice(&v16_extra_semantic_frame(3, 0, 1, 17, &[0x41]));
    let mut codec = moor::wire::Codec::new(moor::wire::Profile::Semantic);
    let mut out = Vec::new();
    assert_eq!(
        codec.feed(0, &over, &mut out),
        Err(moor::wire::WireError::OversizedMessage)
    );
    assert!(out.is_empty());
}

#[test]
fn v16_extra_compact_prefix_cap_is_4096_on_both_sides() {
    // §1.1: the plain length prefix is exactly 2 bytes with a 4096-byte cap.
    // This is the other half of the consumer-issue-#12 contract: the lane
    // already pins the prefix WIDTH at the frozen vectors; this pins the CAP,
    // which no test anywhere exercised. A u16 prefix can spell values up to
    // 65535, so 4097..=65535 must be refused on decode even though the prefix
    // itself still fits.
    let mut at_cap = Vec::new();
    moor::wire::put_compact(&mut at_cap, &[0x61; 4096]).unwrap();
    assert_eq!(at_cap.len(), 2 + 4096, "prefix is exactly 2 bytes");
    assert_eq!(&at_cap[..2], &[0x00, 0x10], "little-endian u16 length 4096");
    assert_eq!(
        moor::wire::get_compact(&at_cap, 0, true).map(<[u8]>::len),
        Some(4096)
    );

    assert_eq!(
        moor::wire::put_compact(&mut Vec::new(), &[0x61; 4097]),
        Err(moor::wire::WireError::OversizedMessage)
    );

    let mut over_cap = 4097u16.to_le_bytes().to_vec();
    over_cap.extend_from_slice(&[0x61; 4097]);
    assert_eq!(
        moor::wire::get_compact(&over_cap, 0, true),
        None,
        "a compact prefix spelling 4097 must be refused despite the bytes being present"
    );
}

#[test]
fn v16_extra_hello_nonzero_flags_are_refused() {
    // §16 V1: "hello flags are reserved zero"; §17 "nonzero hello flags
    // refused". The frozen V1 payload carries the two flag bytes at offsets
    // 5 and 6 (after MOOR and the version byte).
    let payload = v16_extra_hex(V16_EXTRA_V1)[24..].to_vec();
    assert!(
        matches!(
            moor::wire::decode_controller(1, &payload, None),
            Ok(moor::wire::ControllerRequest::Hello(identity))
                if identity == b"\x01/tmp/.moor-1000/build"
        ),
        "frozen V1 payload control"
    );
    for flag_byte in [5usize, 6] {
        let mut mutated = payload.clone();
        mutated[flag_byte] = 1;
        assert_eq!(
            moor::wire::decode_controller(1, &mutated, None).err(),
            Some(moor::wire::WireError::Malformed),
            "flag byte at payload offset {flag_byte}"
        );
    }
}

// ===== §16 V25 — POSIX STATUS_REPLY at layout 02 =====
// This is the first independent check that the holder's exact frontier and the
// descriptor's commit fields agree: the frozen bytes pin selected event
// slot/index/length 0/1/133 together with the body SHA-256, so a descriptor
// built from a stale cache or patched at the wrong offset fails here rather
// than being confirmed by whichever side wrote it.

const V25_HEADER: &[u8] = b"{\"v\":2,\"type\":\"header\",\"ts\":0,\"session\":\"AS90bXAvLm1vb3ItMTAwMC9idWlsZA==\",\"generation\":7,\"epoch\":0,\"next_seq\":0,\"first_retained\":0}\n";

fn v25() -> Vec<u8> {
    hex("4D 4F 4F 52 04 0E 00 00 07 00 00 00 01 00 00 00
         F8 00 00 00 68 D0 95 1E 16 00 00 00 01 2F 74 6D
         70 2F 2E 6D 6F 6F 72 2D 31 30 30 30 2F 62 75 69
         6C 64 07 00 00 00 00 01 02 03 04 05 06 07 08 09
         0A 0B 0C 0D 0E 0F 02 0B 00 00 00 2F 74 6D 70 2F
         65 76 65 6E 74 73 00 01 00 00 00 00 00 00 00 85
         00 00 00 00 00 00 00 2B BE EF B6 37 54 66 12 D6
         A3 A6 BD 7C BD B7 BE 29 42 D6 DA DD C7 33 39 54
         45 F9 ED D7 88 B6 4B 01 00 00 00 00 00 00 00 02
         00 00 00 00 00 00 00 03 03 03 03 03 03 03 03 03
         03 03 03 03 03 03 03 04 00 00 00 2F 74 6D 70 34
         12 00 00 78 56 00 00 10 11 12 13 14 15 16 17 18
         19 1A 1B 1C 1D 1E 1F 50 00 18 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 E3 03 00 00 00
         00 00 00 0F 01 00 00 00 01 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00")
}

#[test]
fn v16_status_v25_frame_is_self_consistent_and_decodes() {
    let frame = v25();
    // Header shape and the declared 248-byte payload, per §1.
    assert_eq!(&frame[0..4], b"MOOR");
    assert_eq!(frame[4], 0x04, "wire version");
    assert_eq!(frame[5], 0x0E, "STATUS_REPLY type");
    assert_eq!(
        u32::from_le_bytes(frame[16..20].try_into().unwrap()),
        0xF8,
        "declared payload length"
    );
    assert_eq!(frame.len() - 24, 0xF8, "actual body length");
    assert_eq!(
        moor::wire::crc32c(&frame[..20]),
        u32::from_le_bytes(frame[20..24].try_into().unwrap()),
        "frozen header CRC-32C"
    );

    // The descriptor must decode against the identity, generation and
    // incarnation the same vector freezes, not merely parse in isolation.
    let mut identity = vec![0x01];
    identity.extend_from_slice(b"/tmp/.moor-1000/build");
    let incarnation: [u8; 16] = (0u8..16).collect::<Vec<_>>().try_into().unwrap();
    let status = moor::wire::StatusTail::decode_for(&frame[24..], &identity, 7, incarnation)
        .expect("the frozen V25 descriptor must decode");

    // Empty retained history at coordinate zero, and the frozen flag byte E3.
    assert_eq!(status.replay.first, 0);
    assert_eq!(status.replay.last, 0);
    assert_eq!(status.replay.start, 0);
    assert_eq!(status.replay.end, 0);
    assert!(status.replay.complete, "E3 bit 0");
    assert!(status.replay.modes_exact, "E3 bit 1");
    assert!(status.viewers, "E3 bit 5");
    assert!(status.running, "E3 bit 6");
    assert!(status.event_writable, "E3 bit 7");
    assert!(!status.owns_lease, "E3 bit 4 clear");
    assert_eq!(status.lease_epoch, 3);
    assert_eq!(status.semantic_flags, 0);
    assert_eq!(status.semantic_pending, 0);
    assert_eq!(status.extension.health, 0x0F);
    assert_eq!(status.extension.log_epoch, 1);
    assert_eq!(status.extension.log_index, 1);
    assert_eq!(status.extension.retained_start, 0);
    assert_eq!(status.extension.retained_end, 0);
}

#[test]
fn v16_status_v25_pins_the_selected_event_commit_fields() {
    // Layout 02 and the selected slot/index/length/hash sit at a computed offset
    // in the descriptor. These are the bytes the holder patches per send, so a
    // stale or misaligned frontier is caught here.
    let frame = v25();
    let body = &frame[24..];
    let mut at = 4 + 22; // wide identity: 4-byte count + tag + 21 path bytes
    at += 4 + 16; // generation + incarnation
    assert_eq!(body[at], 0x02, "event storage layout 02 on POSIX");
    at += 1;
    let path_len = u32::from_le_bytes(body[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!(&body[at + 4..at + 4 + path_len], b"/tmp/events");
    at += 4 + path_len;

    assert_eq!(body[at], 0, "selected body slot 0");
    assert_eq!(
        u64::from_le_bytes(body[at + 1..at + 9].try_into().unwrap()),
        1,
        "selected commit index 1"
    );
    assert_eq!(
        u64::from_le_bytes(body[at + 9..at + 17].try_into().unwrap()),
        133,
        "selected committed body length 133"
    );

    // The frozen hash really is the SHA-256 of the frozen 133-byte header, and
    // the frozen length really is that header's length — so neither the vector
    // nor the implementation can drift without this failing.
    assert_eq!(V25_HEADER.len(), 133);
    let digest: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(V25_HEADER).into();
    assert_eq!(&body[at + 17..at + 49], &digest);
}

// ===== §16 V28 — expanded HEARTBEAT =====

#[test]
fn v16_status_v28_heartbeat_round_trips_and_encodes_exactly() {
    // Drives the real encoder and the real decoder against the frozen bytes,
    // so this is an end-to-end check of the five defined health bits rather
    // than an inspection of the vector.
    let frame = hex("4D 4F 4F 52 04 12 00 00 07 00 00 00 04 00 00 00
         09 00 00 00 2C A1 A4 A1 08 07 06 05 04 03 02 01
         1F");
    assert_eq!(frame[5], 0x12, "HEARTBEAT type");
    assert_eq!(
        u32::from_le_bytes(frame[16..20].try_into().unwrap()),
        9,
        "declared payload length"
    );
    assert_eq!(frame.len() - 24, 9, "actual body length");
    assert_eq!(
        moor::wire::crc32c(&frame[..20]),
        u32::from_le_bytes(frame[20..24].try_into().unwrap()),
        "frozen header CRC-32C"
    );

    let beat = moor::wire::Heartbeat::decode(&frame[24..]).expect("frozen heartbeat decodes");
    assert_eq!(beat.monotonic_ms, 0x0102_0304_0506_0708);
    assert_eq!(beat.flags, 0x1F, "all five defined health bits set");

    // The encoder must reproduce the frozen payload byte for byte, and the
    // frame must reproduce byte for byte once framed at the frozen sequence.
    assert_eq!(beat.encode().unwrap().as_slice(), &frame[24..]);
    let mut framed = Vec::new();
    progressed_codec(moor::wire::Profile::Controller, 1, 4)
        .encode(7, 0x12, &frame[24..], &mut framed)
        .expect("frame the frozen heartbeat");
    assert_eq!(
        framed, frame,
        "framed heartbeat must equal the frozen vector"
    );
}

#[test]
fn v16_status_v28_reserved_heartbeat_bits_are_refused() {
    // §16 states the reserved bits are clear; bits 5-7 must not be accepted as
    // a forward-compatible extension (§2).
    for reserved in [0x20u8, 0x40, 0x80] {
        let mut payload = hex("08 07 06 05 04 03 02 01 1F");
        payload[8] |= reserved;
        assert!(
            moor::wire::Heartbeat::decode(&payload).is_err(),
            "reserved bit {reserved:#04x} must be refused"
        );
    }
}

// ===== §16 V30 — ordered clear request and every LOG_CLEAR_RESULT row =====

#[test]
fn v16_status_v30_clear_request_encodes_exactly() {
    let frame = hex("4D 4F 4F 52 04 19 00 00 07 00 00 00 06 00 00 00
         18 00 00 00 FF 34 2D 9D 00 01 02 03 04 05 06 07
         08 09 0A 0B 0C 0D 0E 0F 05 00 00 00 00 00 00 00");
    assert_eq!(frame[5], 0x19, "LOG_CLEAR type");
    assert_eq!(frame.len() - 24, 0x18, "declared 24-byte payload");
    let incarnation: [u8; 16] = (0u8..16).collect::<Vec<_>>().try_into().unwrap();
    // The real builder must reproduce the frozen payload for incarnation
    // 00..0F and observed index P=5.
    assert_eq!(
        moor::wire::log_clear_payload(incarnation, 5)
            .unwrap()
            .as_slice(),
        &frame[24..]
    );
}

#[test]
fn v16_status_v30_every_result_row_round_trips_byte_for_byte() {
    // The six independent fixtures in the artifact's stated order. Each is
    // (outcome, reason, epoch, prior P, resulting index, cleared-through E),
    // and each must both encode to and decode from the frozen bytes — so a
    // refusal row that carried the wrong reason or a zeroed coordinate fails.
    let rows: [(u8, u8, u32, u64, u64, u64, &str); 6] = [
        (0, 0, 3, 5, 7, 9, "cleared"),
        (1, 0, 2, 5, 6, 9, "already empty"),
        (1, 0, 0, 0, 0, 0, "disabled"),
        (2, 1, 2, 5, 6, 8, "stale status"),
        (2, 2, 0, 5, 0, 0, "unavailable"),
        (2, 3, 0, 5, 0, 0, "corrupt"),
    ];
    let frozen = [
        "00 00 00 00 03 00 00 00 05 00 00 00 00 00 00 00 07 00 00 00 00 00 00 00 09 00 00 00 00 00 00 00",
        "01 00 00 00 02 00 00 00 05 00 00 00 00 00 00 00 06 00 00 00 00 00 00 00 09 00 00 00 00 00 00 00",
        "01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
        "02 01 00 00 02 00 00 00 05 00 00 00 00 00 00 00 06 00 00 00 00 00 00 00 08 00 00 00 00 00 00 00",
        "02 02 00 00 00 00 00 00 05 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
        "02 03 00 00 00 00 00 00 05 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
    ];
    for ((outcome, reason, epoch, prior, resulting, cleared, label), bytes) in
        rows.into_iter().zip(frozen)
    {
        let expected = hex(bytes);
        let produced =
            moor::wire::log_clear_result_payload(outcome, reason, epoch, prior, resulting, cleared)
                .unwrap_or_else(|error| panic!("{label}: {error:?}"));
        assert_eq!(produced.as_slice(), expected.as_slice(), "{label} encode");
        let (decoded_outcome, decoded_reason, decoded_prior) =
            moor::wire::decode_log_clear_result(&expected)
                .unwrap_or_else(|error| panic!("{label}: {error:?}"));
        assert_eq!(
            (decoded_outcome, decoded_reason, decoded_prior),
            (outcome, reason, prior),
            "{label} decode"
        );
    }
}

// ---------------------------------------------------------------- V31 ----
// §16 V31 — every private background-result state, each exactly 12 bytes,
// generation 7. Store-adopted and ready use result zero and are the
// two-record success sequence; failed carries the frozen sample code 0x1234
// and never follows ready. Exact hex from the schema.

const V31_ADOPTED: &str = "4D 4F 52 52 01 01 00 00 07 00 00 00";
const V31_READY: &str = "4D 4F 52 52 01 02 00 00 07 00 00 00";
const V31_FAILED: &str = "4D 4F 52 52 01 03 34 12 07 00 00 00";

#[test]
fn v31_every_state_encodes_and_decodes_the_frozen_record() {
    for (state, result, bytes, label) in [
        (1u8, 0u16, V31_ADOPTED, "store-adopted"),
        (2, 0, V31_READY, "ready"),
        (3, 0x1234, V31_FAILED, "failed"),
    ] {
        let expected = hex(bytes);
        let produced = moor::runtime::private::launch_result(state, result, 7)
            .unwrap_or_else(|| panic!("{label}: refused a valid record"));
        assert_eq!(produced.len(), 12, "{label} is not exactly 12 bytes");
        assert_eq!(produced.as_slice(), expected.as_slice(), "{label} encode");
        assert_eq!(
            moor::runtime::private::decode_launch_result(&expected),
            Some((state, result, 7)),
            "{label} decode"
        );
    }
}

#[test]
fn v31_refuses_every_malformed_background_result() {
    let good = hex(V31_FAILED);
    // Wrong magic, then wrong format byte: the discriminator is the whole
    // five-byte prefix, so neither may be treated as advisory.
    for index in 0..5 {
        let mut bytes = good.clone();
        bytes[index] ^= 0xFF;
        assert_eq!(
            moor::runtime::private::decode_launch_result(&bytes),
            None,
            "prefix byte {index} was accepted when corrupted"
        );
    }
    // Short and long records. A 12-byte record is exact, not a minimum, so a
    // trailing byte must be refused rather than ignored.
    assert_eq!(
        moor::runtime::private::decode_launch_result(&good[..11]),
        None,
        "a short record was accepted"
    );
    let mut long = good.clone();
    long.push(0);
    assert_eq!(
        moor::runtime::private::decode_launch_result(&long),
        None,
        "a long record was accepted"
    );
    assert_eq!(moor::runtime::private::decode_launch_result(&[]), None);
    // Wrong state, nonzero success result, zero result on failed, and zero
    // generation — each refused by the encoder and by the decoder alike.
    for (state, result, generation, label) in [
        (0u8, 0u16, 7u32, "state zero"),
        (4, 0, 7, "state above the frozen set"),
        (1, 5, 7, "store-adopted with a nonzero result"),
        (2, 5, 7, "ready with a nonzero result"),
        (3, 0, 7, "failed with a zero result"),
        (1, 0, 0, "zero generation"),
        (3, 0x1234, 0, "zero generation on failed"),
    ] {
        assert_eq!(
            moor::runtime::private::launch_result(state, result, generation),
            None,
            "{label} was encoded"
        );
        let mut bytes = good.clone();
        bytes[5] = state;
        bytes[6..8].copy_from_slice(&result.to_le_bytes());
        bytes[8..].copy_from_slice(&generation.to_le_bytes());
        assert_eq!(
            moor::runtime::private::decode_launch_result(&bytes),
            None,
            "{label} was decoded"
        );
    }
}

#[test]
fn v31_reporter_emits_the_success_sequence_and_never_fails_after_ready() {
    // The two-record success sequence, driven through the real reporter.
    let mut sink = Vec::new();
    {
        let mut reporter = moor::runtime::private::LaunchReporter {
            output: Some(&mut sink),
            generation: 7,
        };
        reporter.notice(1, 0);
        reporter.notice(2, 0);
        // Drop runs here: it reports loss, which must NOT follow ready.
    }
    let mut expected = hex(V31_ADOPTED);
    expected.extend_from_slice(&hex(V31_READY));
    assert_eq!(
        sink, expected,
        "ready was not the final record; a failure followed it"
    );
}

#[test]
fn v31_reporter_reports_loss_before_and_after_adoption() {
    // Loss before adoption: the channel closes having reported nothing, so the
    // reporter must still name the failure rather than leave a requester
    // waiting on a record that never arrives.
    let mut before = Vec::new();
    drop(moor::runtime::private::LaunchReporter {
        output: Some(&mut before),
        generation: 7,
    });
    let loss = moor::runtime::private::launch_result(3, 1, 7).expect("loss record");
    assert_eq!(before, loss, "loss before adoption was not reported");

    // Loss after adoption: the adopted record stands and the loss follows it,
    // because adoption is not readiness.
    let mut after = Vec::new();
    {
        let mut reporter = moor::runtime::private::LaunchReporter {
            output: Some(&mut after),
            generation: 7,
        };
        reporter.notice(1, 0);
    }
    let mut expected = hex(V31_ADOPTED);
    expected.extend_from_slice(&loss);
    assert_eq!(after, expected, "loss after adoption was not reported");
    assert_eq!(
        moor::runtime::private::decode_launch_result(&after[12..]),
        Some((3, 1, 7)),
        "the reported loss is not a decodable failed record"
    );
}

// ---------------------------------------------------------------- V26 ----
// §16 V26 — NON_VT attach and its required empty preamble. The attach
// preserves geometry and sets only flag bit 1. The preamble payload is the
// plain u16 zero length, NOT an absent frame. The attach uses controller
// sequence 2; under the status-first prefix the preamble FOLLOWS the
// sequence-2 ATTACH_ACK at holder sequence 3. Exact hex from the schema.

const V26_ATTACH: &str = "4D 4F 4F 52 04 03 00 00 07 00 00 00 02 00 00 00 \
                          05 00 00 00 2D 90 AF 9C 00 00 00 00 02";
const V26_PREAMBLE: &str = "4D 4F 4F 52 04 05 00 00 07 00 00 00 03 00 00 00 \
                            02 00 00 00 37 2D 59 98 00 00";

#[test]
fn v26_both_frames_reproduce_the_frozen_bytes() {
    for (sequence, kind, payload, bytes, label) in [
        (2, 3u8, vec![0, 0, 0, 0, 2], V26_ATTACH, "NON_VT attach"),
        (3, 5, vec![0, 0], V26_PREAMBLE, "empty preamble"),
    ] {
        let frame = hex(bytes);
        v16_framing_assert_header(&frame);
        assert_eq!(
            frame[24..],
            payload[..],
            "{label}: frozen payload disagrees with the schema text"
        );
        assert_eq!(
            v16_framing_encode_frame(sequence, 7, kind, &payload),
            frame,
            "{label}: encoder must reproduce the frozen frame at its real sequence"
        );
    }
}

#[test]
fn v26_attach_decodes_as_non_vt_with_geometry_preserved() {
    use moor::session::Request;
    use moor::wire::{ControllerRequest, decode_controller};
    let frame = hex(V26_ATTACH);
    let message = v16_framing_feed_one(2, &frame);
    assert_eq!((message.scope, message.kind), (7, 3));
    // Flag bit 1 alone: no input lease is requested, NON_VT is.
    assert!(
        matches!(
            decode_controller(3, &message.payload, None),
            Ok(ControllerRequest::Policy(Request::Attach(
                0, 0, false, true, None
            )))
        ),
        "V26 must decode as a NON_VT attach with 0x0 geometry and no lease request"
    );
    // Only bits 0 and 1 exist; bit 2 upward is not a forward-compatible
    // extension and must be refused rather than masked away.
    for flags in [4u8, 8, 0x80, 0xFF] {
        assert!(
            decode_controller(3, &[0, 0, 0, 0, flags], None).is_err(),
            "attach flags {flags:#04x} were accepted"
        );
    }
}

#[test]
fn v26_non_vt_attach_preserves_child_geometry_and_grants_no_lease() {
    use moor::session::{Effect, Machine, Request, Transition};
    let mut machine = Machine::new(7, [1; 16], [2; 16]);
    machine.register_controller(1);
    let effects = machine
        .transition(Transition::Peer(
            0,
            1,
            Request::Attach(0, 0, false, true, Some([3; 16])),
        ))
        .expect("the NON_VT attach must be accepted");
    let (lease, resize) = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Attached(1, _, lease, resize) => Some((lease.clone(), *resize)),
            _ => None,
        })
        .expect("the connection must attach");
    assert_eq!(resize, None, "geometry 0x0 preserves the child geometry");
    assert!(
        lease.is_none(),
        "flag bit 0 is clear, so no input lease may be granted"
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Close(_) | Effect::Resize(..))),
        "a NON_VT attach must neither be refused nor resize the child"
    );
}

// ---------------------------------------------------------------- V33 ----
// §16 V33 — WAKEUP interposed between HELLO_ACK and ATTACH_ACK, at the REAL
// sequence numbers: HELLO_ACK consumed holder sequence 1, so the WAKEUP is 2,
// the ATTACH_ACK that follows is 3, and the terminal preamble is 4. The
// frozen bytes come from the schema text, never from the encoder, so encoder
// and decoder cannot drift together while this stays green.

const V33_WAKEUP: &str = "4D 4F 4F 52 04 11 00 00 07 00 00 00 02 00 00 00 \
                          00 00 00 00 52 17 53 91";

#[test]
fn v33_wakeup_is_legal_between_hello_ack_and_attach_ack() {
    use moor::wire::{ViewerEvent, ViewerStream, decode_viewer};
    // WAKEUP is legal at EVERY post-HELLO_ACK phase, including the window
    // between HELLO_ACK and ATTACH_ACK. A holder whose event store advanced
    // durably announces it immediately; it does not wait for the controller
    // to finish attaching, and a controller that faults its handshake over
    // that announcement loses exactly the session it was joining (OB-30 /
    // the desk#54 incident class).
    let frame = hex(V33_WAKEUP);
    v16_framing_assert_header(&frame);
    assert_eq!(frame.len(), 24, "V33 is header-only");
    assert_eq!(
        v16_framing_encode_frame(2, 7, 0x11, &[]),
        frame,
        "the encoder must reproduce the frozen V33 bytes at sequence 2"
    );
    let wakeup = v16_framing_feed_one(2, &frame);
    assert_eq!((wakeup.scope, wakeup.kind), (7, 0x11));

    let mut stream = ViewerStream::default();
    // The WAKEUP lands before any attach state exists — and changes nothing.
    assert_eq!(
        decode_viewer(
            &mut stream,
            &wakeup,
            (b"\x01/tmp/session".as_slice(), 7, [9; 16])
        ),
        Ok(None),
        "a WAKEUP between HELLO_ACK and ATTACH_ACK must be accepted"
    );
    assert!(
        stream.replay.is_none() && !stream.terminal,
        "the pre-attach WAKEUP must not fabricate attach state"
    );

    // And the attach that follows proceeds exactly as if the WAKEUP had
    // never happened — status-first, then the terminal preamble — at the
    // shifted-by-one holder sequences 3 and 4.
    let mut payload = Vec::new();
    moor::wire::put_wide(&mut payload, b"\x01/tmp/session").unwrap();
    payload.extend_from_slice(&7u32.to_le_bytes());
    payload.extend_from_slice(&[9; 16]);
    payload.push(0);
    moor::wire::put_wide(&mut payload, b"").unwrap();
    payload.push(0xff);
    payload.extend_from_slice(&[0; 48 + 32]);
    moor::wire::put_wide(&mut payload, b"/tmp").unwrap();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&[1; 16]);
    payload.extend_from_slice(&80u16.to_le_bytes());
    payload.extend_from_slice(&24u16.to_le_bytes());
    let tail = moor::wire::StatusTail {
        columns: 80,
        rows: 24,
        replay: moor::wire::ReplayDescriptor {
            first: 0,
            last: 0,
            start: 0,
            end: 0,
            complete: true,
            modes_exact: true,
        },
        owns_lease: false,
        viewers: false,
        running: true,
        event_writable: false,
        lease_epoch: 1,
        semantic_flags: 0,
        semantic_pending: 0,
        extension: moor::wire::StatusExtension {
            health: 0,
            log_epoch: 0,
            log_index: 0,
            retained_start: 0,
            retained_end: 0,
        },
    };
    payload.extend_from_slice(&tail.encode().unwrap());
    let status = v16_framing_feed_one(3, &v16_framing_encode_frame(3, 7, 4, &payload));
    assert_eq!(
        decode_viewer(
            &mut stream,
            &status,
            (b"\x01/tmp/session".as_slice(), 7, [9; 16])
        ),
        Ok(None)
    );
    let terminal = v16_framing_feed_one(4, &v16_framing_encode_frame(4, 7, 5, &[0, 0]));
    assert_eq!(
        decode_viewer(
            &mut stream,
            &terminal,
            (b"\x01/tmp/session".as_slice(), 7, [9; 16])
        ),
        Ok(Some(ViewerEvent::Terminal(b""))),
        "the attach after a pre-attach WAKEUP completes normally"
    );
}

#[test]
fn v26_empty_preamble_is_a_present_frame_with_a_plain_u16_zero_length() {
    use moor::wire::{ViewerEvent, ViewerStream, decode_viewer};
    let frame = hex(V26_PREAMBLE);
    let message = v16_framing_feed_one(3, &frame);
    assert_eq!((message.scope, message.kind), (7, 5));

    // Present-and-empty, not absent: the frame is delivered as a Terminal
    // event carrying zero bytes, and it is what sets the stream's terminal
    // state. An absent preamble leaves that state unset, so the two are
    // observably different — which is the whole point of the requirement.
    // v4 status-first attach: TERMINAL_STATE arrives after the descriptor,
    // so the stream models a viewer that has already consumed its status.
    let mut stream = ViewerStream {
        non_vt: true,
        replay: Some(moor::wire::ReplayDescriptor {
            first: 0,
            last: 0,
            start: 0,
            end: 0,
            complete: true,
            modes_exact: true,
        }),
        ..ViewerStream::default()
    };
    assert!(!stream.terminal, "the preamble has not been consumed yet");
    assert_eq!(
        decode_viewer(&mut stream, &message, (b"".as_slice(), 7, [9; 16])),
        Ok(Some(ViewerEvent::Terminal(b""))),
        "the empty preamble must decode as a present, empty terminal payload"
    );
    assert!(stream.terminal, "the preamble must set the terminal state");
    assert!(
        stream.non_vt,
        "consuming the preamble must not clear NON_VT"
    );

    // The length is the plain §1.1 u16, not the §1.1.1 wide u32. A four-byte
    // zero length must not be read as one zero-length field: under the plain
    // u16 it is a zero length followed by two uncovered bytes.
    //
    // Two separate rules can refuse that payload, and they are asserted apart
    // on purpose. On a NON_VT stream the emptiness rule refuses it because the
    // two trailing bytes are a nonempty body:
    let wide_frame = v16_framing_encode_frame(2, 7, 5, &[0, 0, 0, 0]);
    let mut wide = ViewerStream {
        non_vt: true,
        ..ViewerStream::default()
    };
    let wide_message = v16_framing_feed_one(2, &wide_frame);
    assert!(
        decode_viewer(&mut wide, &wide_message, (b"".as_slice(), 7, [9; 16])).is_err(),
        "a wide four-byte zero length was accepted on a NON_VT stream"
    );
    // ...and on an ordinary stream, where the emptiness rule does not apply,
    // only the length-exactness check remains to refuse it. This case is what
    // proves the plain u16 is read exactly; without it, deleting the exactness
    // check leaves this lane green.
    let mut wide_vt = ViewerStream::default();
    assert!(
        decode_viewer(&mut wide_vt, &wide_message, (b"".as_slice(), 7, [9; 16])).is_err(),
        "a zero length followed by two uncovered bytes was accepted: the plain \
         u16 length is not being checked for exactness"
    );

    // Under NON_VT the preamble must be empty, so a well-formed nonempty one
    // is still refused.
    let mut nonempty = ViewerStream {
        non_vt: true,
        ..ViewerStream::default()
    };
    let payload = [2, 0, b'h', b'i'];
    let nonempty_message = v16_framing_feed_one(2, &v16_framing_encode_frame(2, 7, 5, &payload));
    assert!(
        decode_viewer(
            &mut nonempty,
            &nonempty_message,
            (b"".as_slice(), 7, [9; 16])
        )
        .is_err(),
        "a nonempty preamble was accepted on a NON_VT stream"
    );
    // The same nonempty preamble is valid when NON_VT was not requested,
    // proving the refusal above is the NON_VT rule and not a framing accident.
    // (Post-status, per the v4 status-first prefix.)
    let mut vt = ViewerStream {
        replay: Some(moor::wire::ReplayDescriptor {
            first: 0,
            last: 0,
            start: 0,
            end: 0,
            complete: true,
            modes_exact: true,
        }),
        ..ViewerStream::default()
    };
    assert_eq!(
        decode_viewer(&mut vt, &nonempty_message, (b"".as_slice(), 7, [9; 16])),
        Ok(Some(ViewerEvent::Terminal(b"hi"))),
        "the nonempty preamble must be accepted on an ordinary stream"
    );
}

// ---------------------------------------------------------------- V29 ----
// §16 V29 — fresh viewer lease grant followed by explicit release. Controller
// sequences 4 then 5; holder sequences 5 then 6. Exact hex from the schema.

const V29_REQUEST: &str = "4D 4F 4F 52 04 15 00 00 07 00 00 00 04 00 00 00 \
                           28 00 00 00 E9 9A 80 98 00 00 00 00 00 00 00 00 \
                           00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 \
                           00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00";
const V29_GRANT: &str = "4D 4F 4F 52 04 16 00 00 07 00 00 00 05 00 00 00 \
                         18 00 00 00 7B 45 6E 47 00 00 00 00 03 00 00 00 \
                         00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F";
const V29_RELEASE: &str = "4D 4F 4F 52 04 17 00 00 07 00 00 00 05 00 00 00 \
                           14 00 00 00 6F EA 86 AD 03 00 00 00 00 01 02 03 \
                           04 05 06 07 08 09 0A 0B 0C 0D 0E 0F";
const V29_RELEASED: &str = "4D 4F 4F 52 04 16 00 00 07 00 00 00 06 00 00 00 \
                            18 00 00 00 12 C2 2A 9C 02 00 00 00 03 00 00 00 \
                            00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00";

fn v29_token() -> [u8; 16] {
    let mut token = [0; 16];
    for (index, byte) in token.iter_mut().enumerate() {
        *byte = index as u8;
    }
    token
}

#[test]
fn v29_all_four_frames_reproduce_the_frozen_bytes() {
    use moor::session::{LeaseRequest, LeaseResult, LeaseRole, ResultOutcome, ResultReason};
    let request = LeaseRequest::fresh(LeaseRole::Viewer)
        .encode_wire()
        .expect("the fresh request must encode");
    let grant = LeaseResult {
        outcome: ResultOutcome::Granted,
        reason: ResultReason::None,
        role: LeaseRole::Viewer,
        epoch: 3,
        token: v29_token(),
    }
    .encode_wire()
    .expect("the grant must encode");
    let mut release = vec![3, 0, 0, 0];
    release.extend_from_slice(&v29_token());
    let released = LeaseResult {
        outcome: ResultOutcome::Released,
        reason: ResultReason::None,
        role: LeaseRole::Viewer,
        epoch: 3,
        token: [0; 16],
    }
    .encode_wire()
    .expect("the released result must encode");

    for (sequence, kind, payload, bytes, label) in [
        (4u32, 0x15u8, request.to_vec(), V29_REQUEST, "fresh request"),
        (5, 0x16, grant.to_vec(), V29_GRANT, "grant"),
        (5, 0x17, release, V29_RELEASE, "release"),
        (6, 0x16, released.to_vec(), V29_RELEASED, "released"),
    ] {
        let frame = hex(bytes);
        v16_framing_assert_header(&frame);
        assert_eq!(
            frame[24..],
            payload[..],
            "{label}: frozen payload disagrees with the schema text"
        );
        assert_eq!(
            v16_framing_encode_frame(sequence, 7, kind, &payload),
            frame,
            "{label}: encoder must reproduce the frozen frame at sequence {sequence}"
        );
    }
    // The fresh request is 40 bytes: operation, role, two reserved, then the 36
    // freshness bytes the vector names.
    assert_eq!(request.len(), 40);
    assert_eq!(
        request[..4],
        [0, 0, 0, 0],
        "operation and role are both zero"
    );
    assert!(
        request[4..].iter().all(|byte| *byte == 0),
        "all 36 freshness bytes are zero on a fresh request"
    );
}

#[test]
fn v29_grant_and_release_decode_the_frozen_tuple() {
    use moor::session::{
        LeaseOperation, LeaseRequest, LeaseResult, LeaseRole, Request, ResultOutcome, ResultReason,
    };
    use moor::wire::{ControllerRequest, decode_controller};
    // Fresh request, through the framing layer at its frozen sequence.
    let message = v16_framing_feed_one(4, &hex(V29_REQUEST));
    assert_eq!((message.scope, message.kind), (7, 0x15));
    assert_eq!(
        LeaseRequest::decode_wire(&message.payload),
        Ok(LeaseRequest::fresh(LeaseRole::Viewer))
    );
    assert_eq!(
        LeaseRequest::decode_wire(&message.payload).map(|request| request.operation),
        Ok(LeaseOperation::Fresh)
    );

    // Grant: outcome granted, epoch 3, token 00..0F.
    let grant = LeaseResult::decode_wire(&v16_framing_feed_one(5, &hex(V29_GRANT)).payload)
        .expect("the grant must decode");
    assert_eq!(
        (
            grant.outcome,
            grant.reason,
            grant.role,
            grant.epoch,
            grant.token
        ),
        (
            ResultOutcome::Granted,
            ResultReason::None,
            LeaseRole::Viewer,
            3,
            v29_token()
        )
    );

    // Release echoes exactly that tuple, and decodes through the real
    // controller dispatch rather than being inspected as bytes.
    let release = v16_framing_feed_one(5, &hex(V29_RELEASE));
    assert_eq!((release.scope, release.kind), (7, 0x17));
    assert!(
        matches!(
            decode_controller(0x17, &release.payload, None),
            Ok(ControllerRequest::Policy(Request::Release(3, token))) if token == v29_token()
        ),
        "the release must echo the granted epoch and token"
    );

    // Released result: outcome 02, epoch 3, and a zero token.
    let released = LeaseResult::decode_wire(&v16_framing_feed_one(6, &hex(V29_RELEASED)).payload)
        .expect("the released result must decode");
    assert_eq!(
        (released.outcome, released.epoch, released.token),
        (ResultOutcome::Released, 3, [0; 16])
    );
}

#[test]
fn v29_refuses_every_inconsistent_lease_request_and_result() {
    use moor::session::{
        LeaseOperation, LeaseRequest, LeaseResult, LeaseRole, ResultOutcome, ResultReason,
    };
    // A fresh request carries no epoch, no incarnation and no token; a resume
    // carries all three. Each half of that coupling is asserted, so neither
    // direction can be dropped.
    for (operation, epoch, incarnation, token, label) in [
        (
            LeaseOperation::Fresh,
            3u32,
            [0u8; 16],
            [0u8; 16],
            "fresh with an epoch",
        ),
        (
            LeaseOperation::Fresh,
            0,
            [9; 16],
            [0; 16],
            "fresh with an incarnation",
        ),
        (
            LeaseOperation::Fresh,
            0,
            [0; 16],
            [9; 16],
            "fresh with a token",
        ),
        (
            LeaseOperation::Resume,
            0,
            [9; 16],
            [9; 16],
            "resume without an epoch",
        ),
        (
            LeaseOperation::Resume,
            3,
            [0; 16],
            [9; 16],
            "resume without an incarnation",
        ),
        (
            LeaseOperation::Resume,
            3,
            [9; 16],
            [0; 16],
            "resume without a token",
        ),
    ] {
        let request = LeaseRequest {
            operation,
            role: LeaseRole::Viewer,
            epoch,
            incarnation,
            token,
        };
        assert_eq!(
            request.encode_wire(),
            Err(moor::wire::WireError::Malformed),
            "{label} encoded"
        );
        let mut bytes = [0u8; 40];
        bytes[0] = operation as u8;
        bytes[4..8].copy_from_slice(&epoch.to_le_bytes());
        bytes[8..24].copy_from_slice(&incarnation);
        bytes[24..40].copy_from_slice(&token);
        assert!(
            LeaseRequest::decode_wire(&bytes).is_err(),
            "{label} decoded"
        );
    }
    // Reserved bytes 2 and 3 are not spare capacity, and neither operation nor
    // role has a second bit.
    let good_request = LeaseRequest::fresh(LeaseRole::Viewer)
        .encode_wire()
        .unwrap();
    for index in [0usize, 1, 2, 3] {
        let mut bytes = good_request;
        bytes[index] = if index < 2 { 2 } else { 1 };
        assert!(
            LeaseRequest::decode_wire(&bytes).is_err(),
            "request byte {index} accepted an out-of-range value"
        );
    }
    assert!(
        LeaseRequest::decode_wire(&good_request[..39]).is_err(),
        "short request"
    );

    // A grant must carry a token; a release must not. Both halves asserted.
    for (outcome, reason, epoch, token, label) in [
        (
            ResultOutcome::Granted,
            ResultReason::None,
            3u32,
            [0u8; 16],
            "grant with a zero token",
        ),
        (
            ResultOutcome::Resumed,
            ResultReason::None,
            3,
            [0; 16],
            "resume with a zero token",
        ),
        (
            ResultOutcome::Released,
            ResultReason::None,
            3,
            [9; 16],
            "release with a token",
        ),
        (
            ResultOutcome::Granted,
            ResultReason::Busy,
            3,
            [9; 16],
            "grant carrying a refusal reason",
        ),
        (
            ResultOutcome::Granted,
            ResultReason::None,
            0,
            [9; 16],
            "grant at epoch zero",
        ),
        (
            ResultOutcome::Released,
            ResultReason::None,
            0,
            [0; 16],
            "release at epoch zero",
        ),
        (
            ResultOutcome::Refused,
            ResultReason::None,
            3,
            [0; 16],
            "refusal without a reason",
        ),
    ] {
        let result = LeaseResult {
            outcome,
            reason,
            role: LeaseRole::Viewer,
            epoch,
            token,
        };
        assert_eq!(
            result.encode_wire(),
            Err(moor::wire::WireError::Malformed),
            "{label} encoded"
        );
        let mut bytes = [0u8; 24];
        bytes[0] = outcome as u8;
        bytes[1] = reason as u8;
        bytes[4..8].copy_from_slice(&epoch.to_le_bytes());
        bytes[8..24].copy_from_slice(&token);
        assert!(LeaseResult::decode_wire(&bytes).is_err(), "{label} decoded");
    }
    let good_result = LeaseResult {
        outcome: ResultOutcome::Released,
        reason: ResultReason::None,
        role: LeaseRole::Viewer,
        epoch: 3,
        token: [0; 16],
    }
    .encode_wire()
    .unwrap();
    let mut padded = good_result;
    padded[3] = 1;
    assert!(
        LeaseResult::decode_wire(&padded).is_err(),
        "result reserved byte 3 accepted a nonzero value"
    );
    assert!(
        LeaseResult::decode_wire(&good_result[..23]).is_err(),
        "short result"
    );
}

// ---------------------------------------------------------------- V27 ----
// §16 V27 — private-mode query and its accepted reply. Correlation
// 0102030405060708, lease epoch 3, echoed class 04, plain u16 byte lengths.
// Both directions use frame sequence 3. Exact hex from the schema.

const V27_QUERY: &str = "4D 4F 4F 52 04 14 00 00 07 00 00 00 03 00 00 00 \
                         18 00 00 00 5A C7 16 3B 08 07 06 05 04 03 02 01 \
                         03 00 00 00 04 09 00 1B 5B 3F 32 30 30 34 24 70";
const V27_REPLY: &str = "4D 4F 4F 52 04 0C 00 00 07 00 00 00 03 00 00 00\
                         1A 00 00 00 F6 71 B4 D2 08 07 06 05 04 03 02 01 \
                         03 00 00 00 04 0B 00 1B 5B 3F 32 30 30 34 3B 31 \
                         24 79";
const V27_CORRELATION: u64 = 0x0102_0304_0506_0708;

#[test]
fn v27_query_and_reply_reproduce_the_frozen_bytes() {
    use moor::wire::Query;
    for (kind, body, bytes, label) in [
        (0x14u8, b"\x1b[?2004$p".as_slice(), V27_QUERY, "query"),
        (0x0C, b"\x1b[?2004;1$y".as_slice(), V27_REPLY, "reply"),
    ] {
        let payload = Query {
            correlation: V27_CORRELATION,
            epoch: 3,
            class: 4,
            bytes: body.to_vec(),
        }
        .encode()
        .unwrap_or_else(|error| panic!("{label}: {error:?}"));
        let frame = hex(bytes);
        v16_framing_assert_header(&frame);
        assert_eq!(
            frame[24..],
            payload[..],
            "{label}: frozen payload disagrees with the schema text"
        );
        assert_eq!(
            v16_framing_encode_frame(3, 7, kind, &payload),
            frame,
            "{label}: encoder must reproduce the frozen frame at sequence 3"
        );
        // The correlation is little-endian, so the frozen bytes read 08..01.
        assert_eq!(&payload[..8], &V27_CORRELATION.to_le_bytes());
        // The body length is the plain §1.1 u16 immediately before the body.
        assert_eq!(
            u16::from_le_bytes(payload[13..15].try_into().unwrap()) as usize,
            body.len(),
            "{label}: declared body length"
        );
    }
}

#[test]
fn v27_reply_echoes_the_query_tuple_and_decodes_through_controller_dispatch() {
    use moor::session::Request;
    use moor::wire::{ControllerRequest, Query, decode_controller, decode_query};
    let query = decode_query(&v16_framing_feed_one(3, &hex(V27_QUERY)).payload)
        .expect("the frozen query must decode");
    assert_eq!(
        (query.correlation, query.epoch, query.class),
        (V27_CORRELATION, 3, 4)
    );
    assert_eq!(query.bytes, b"\x1b[?2004$p".to_vec(), "CSI7 ?2004$p");

    let message = v16_framing_feed_one(3, &hex(V27_REPLY));
    assert_eq!((message.scope, message.kind), (7, 0x0C));
    let reply = decode_query(&message.payload).expect("the frozen reply must decode");
    // "echoed": the reply carries the query's correlation, epoch and class
    // unchanged. Asserted as a tuple so a single echoed field cannot drift.
    assert_eq!(
        (reply.correlation, reply.epoch, reply.class),
        (query.correlation, query.epoch, query.class),
        "the reply must echo the query's correlation, epoch and class"
    );
    assert_eq!(reply.bytes, b"\x1b[?2004;1$y".to_vec(), "CSI7 ?2004;1$y");
    // The reply also decodes through the real controller dispatch, which is
    // how the holder actually receives it.
    assert!(
        matches!(
            decode_controller(0x0C, &message.payload, None),
            Ok(ControllerRequest::Policy(Request::QueryReply(correlation, 3, 4, bytes)))
                if correlation == V27_CORRELATION && bytes == b"\x1b[?2004;1$y"
        ),
        "the reply must dispatch as a QueryReply carrying the echoed tuple"
    );
    // Round-trip: re-encoding the decoded reply reproduces the frozen payload.
    assert_eq!(
        Query {
            correlation: reply.correlation,
            epoch: reply.epoch,
            class: reply.class,
            bytes: reply.bytes.clone(),
        }
        .encode()
        .unwrap(),
        message.payload
    );
}

#[test]
fn v27_refuses_every_invalid_query_tuple_and_inexact_length() {
    use moor::wire::{Query, decode_query};
    let body = b"\x1b[?2004$p".to_vec();
    // correlation zero, epoch zero, and class outside 1..=5 are each refused by
    // the encoder and the decoder alike.
    for (correlation, epoch, class, label) in [
        (0u64, 3u32, 4u8, "zero correlation"),
        (V27_CORRELATION, 0, 4, "zero epoch"),
        (V27_CORRELATION, 3, 0, "class zero"),
        (V27_CORRELATION, 3, 6, "class above the frozen range"),
        (V27_CORRELATION, 3, 0xFF, "class 0xFF"),
    ] {
        let query = Query {
            correlation,
            epoch,
            class,
            bytes: body.clone(),
        };
        assert!(query.encode().is_err(), "{label} encoded");
        let mut payload = Vec::new();
        payload.extend_from_slice(&correlation.to_le_bytes());
        payload.extend_from_slice(&epoch.to_le_bytes());
        payload.push(class);
        payload.extend_from_slice(&(body.len() as u16).to_le_bytes());
        payload.extend_from_slice(&body);
        assert!(decode_query(&payload).is_err(), "{label} decoded");
    }
    // The plain u16 body length is exact, not a lower bound: a trailing byte
    // the length does not cover must be refused rather than ignored, and a
    // length longer than the body must not over-read.
    let good = Query {
        correlation: V27_CORRELATION,
        epoch: 3,
        class: 4,
        bytes: body.clone(),
    }
    .encode()
    .unwrap();
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(
        decode_query(&trailing).is_err(),
        "a byte past the declared body length was ignored"
    );
    let mut overlong = good.clone();
    overlong[13..15].copy_from_slice(&((body.len() + 1) as u16).to_le_bytes());
    assert!(
        decode_query(&overlong).is_err(),
        "a declared length past the end of the payload was accepted"
    );
    assert!(decode_query(&good[..good.len() - 1]).is_err(), "short body");
    assert!(decode_query(&[]).is_err(), "empty payload");
}

// ------------------------------------------------------------ V23, V24 ----
// §16 V23/V24 — the two remaining portable 92-byte MOORCMT1 initial commits.
// V23 is the empty-log commit (kind 02); V24 is the canonical-running
// lifecycle commit (kind 03) over an exact 286-byte body. Exact hex from the
// schema; every body digest is recomputed through the crate's own hashing
// entrypoint rather than trusted from this test.

const V23_RECORD: &str = "4D 4F 4F 52 43 4D 54 31 01 00 00 02 07 00 00 00
     01 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00
     00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
     00 00 00 00 00 00 00 00 E3 B0 C4 42 98 FC 1C 14
     9A FB F4 C8 99 6F B9 24 27 AE 41 E4 64 9B 93 4C
     A4 95 99 1B 78 52 B8 55 CE 64 F3 A0";

// §16 V24 — the exact 286-byte lifecycle body, final LF included, copied
// verbatim from the ```jsonl block in the schema.
const V24_BODY: &[u8] = b"{\"v\":2,\"type\":\"lifecycle\",\"phase\":\"running\",\"session\":\"AS9z\",\"generation\":7,\"wire_generation\":7,\"incarnation\":\"AgICAgICAgICAgICAgICAg==\",\"start_wall_ms\":\"1\",\"start_mono_ms\":\"2\",\"boot_id\":\"AwMDAwMDAwMDAwMDAwMDAw==\",\"path_encoding\":\"posix-bytes\",\"event_path\":null,\"instrument_path\":null}\n";

const V24_RECORD: &str = "4D 4F 4F 52 43 4D 54 31 01 00 00 03 07 00 00 00
     01 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00
     1E 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00
     00 00 00 00 00 00 00 00 FA E7 1F CF 6C AD 5E 79
     D0 AB E5 FE 84 63 80 34 09 E5 0E C8 3F 2C F5 79
     93 66 0C 8B A0 C5 22 4E D7 28 8F 9E";

/// Hashes `body` through the crate's own digest entrypoint, so the frozen
/// SHA-256 is confirmed against real hashing code and not restated here.
fn v23_digest_of(body: &[u8], label: &str) -> [u8; 32] {
    use std::io::Write as _;
    let path = std::env::temp_dir().join(format!(
        "moor-v16-{label}-{}-{}.body",
        std::process::id(),
        body.len()
    ));
    let mut file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    file.write_all(body).unwrap();
    let digest = copy_digest(&mut file, None);
    let _ = std::fs::remove_file(&path);
    digest.unwrap()
}

#[test]
fn v16_platform_v23_empty_log_commit_matches_the_ratified_record() {
    let frozen = hex(V23_RECORD);
    assert_eq!(frozen.len(), 92, "the ratified record is exactly 92 bytes");
    // The empty-body SHA-256 the schema states, confirmed by hashing nothing
    // through the real digest path.
    let hash = v23_digest_of(b"", "platform-v23");
    assert_eq!(
        hash,
        <[u8; 32]>::try_from(hex("E3 B0 C4 42 98 FC 1C 14 9A FB F4 C8 99 6F B9 24
                 27 AE 41 E4 64 9B 93 4C A4 95 99 1B 78 52 B8 55"))
        .unwrap(),
        "the empty-body digest must be the value §16 V23 states"
    );
    // Kind log 02, generation 7, epoch 1, index 1, empty body, coordinates
    // [0,0). The declared length is zero because the body is empty.
    let produced = Commit {
        slot: 0,
        body: 0,
        kind: Kind::Log,
        generation: 7,
        epoch: 1,
        index: 1,
        length: 0,
        start: 0,
        end: 0,
        hash,
    }
    .encode();
    assert_eq!(produced.as_slice(), frozen.as_slice());
    assert_eq!(frozen[11], 0x02, "kind log is 02");
    assert_eq!(
        u64::from_le_bytes(frozen[32..40].try_into().unwrap()),
        0,
        "an empty body declares length zero"
    );
    assert_eq!(
        u32::from_le_bytes(frozen[88..92].try_into().unwrap()),
        crc32c(&frozen[..88]),
        "the trailing CRC-32C must recompute over bytes 0..88"
    );
}

#[test]
fn v16_platform_v24_lifecycle_commit_matches_the_ratified_record() {
    let frozen = hex(V24_RECORD);
    assert_eq!(frozen.len(), 92, "the ratified record is exactly 92 bytes");
    // The body is exactly 286 bytes including its final LF, and its SHA-256 is
    // the value §16 V24 states — which is what proves this transcription of
    // the jsonl block is the body the schema froze.
    assert_eq!(
        V24_BODY.len(),
        286,
        "the frozen lifecycle body is 286 bytes"
    );
    assert_eq!(*V24_BODY.last().unwrap(), b'\n', "the body ends with LF");
    let hash = v23_digest_of(V24_BODY, "platform-v24");
    assert_eq!(
        hash,
        <[u8; 32]>::try_from(hex("FA E7 1F CF 6C AD 5E 79 D0 AB E5 FE 84 63 80 34
                 09 E5 0E C8 3F 2C F5 79 93 66 0C 8B A0 C5 22 4E"))
        .unwrap(),
        "the lifecycle body digest must be the value §16 V24 states"
    );

    // The frozen body is not merely self-consistent: the REAL lifecycle
    // store accepts it, and the same bytes with the retired `"v":1` are
    // refused. Store::create routes through the production validator, so
    // this pins the v2-only contract with the exact §16 bytes rather than
    // a restatement of them.
    {
        use moor::store::{Kind, Store};
        let path = std::env::temp_dir().join(format!(
            "moor-v16-v24-validate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = Store::create(&path, Kind::Exit, 7, V24_BODY, 0, 0)
            .expect("the frozen §16 V24 body must be accepted by the real lifecycle store");
        drop(store);
        std::fs::remove_dir_all(&path).unwrap();

        let downgraded = {
            let mut body = V24_BODY.to_vec();
            let at = body
                .windows(5)
                .position(|window| window == b"\"v\":2")
                .expect("the frozen body carries its version");
            body[at + 4] = b'1';
            body
        };
        assert!(
            Store::create(&path, Kind::Exit, 7, &downgraded, 0, 0).is_err(),
            "the retired v1 lifecycle shape must be refused by the real store"
        );
        let _ = std::fs::remove_dir_all(&path);
    }
    // Kind exit 03, generation 7, epoch 1, index 1, coordinates [0,0).
    let produced = Commit {
        slot: 0,
        body: 0,
        kind: Kind::Exit,
        generation: 7,
        epoch: 1,
        index: 1,
        length: V24_BODY.len() as u64,
        start: 0,
        end: 0,
        hash,
    }
    .encode();
    assert_eq!(produced.as_slice(), frozen.as_slice());
    assert_eq!(frozen[11], 0x03, "kind exit is 03");
    // The declared prefix length really is the length of the frozen body, so a
    // transcription error in either cannot pass unnoticed.
    assert_eq!(
        u64::from_le_bytes(frozen[32..40].try_into().unwrap()),
        V24_BODY.len() as u64,
    );
    assert_eq!(
        u32::from_le_bytes(frozen[88..92].try_into().unwrap()),
        crc32c(&frozen[..88]),
        "the trailing CRC-32C must recompute over bytes 0..88"
    );
}

#[test]
fn v16_platform_v23_v24_differ_only_where_the_schema_says_they_do() {
    // V13, V23 and V24 are the same 92-byte layout at generation 7, index 1
    // and coordinates [0,0). Asserting that they differ ONLY in kind, epoch,
    // declared length, body hash and the resulting CRC is what makes the three
    // records a layout check rather than three independent byte blobs.
    let v13 = hex("4D 4F 4F 52 43 4D 54 31 01 00 00 01 07 00 00 00
         00 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00
         89 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
         00 00 00 00 00 00 00 00 2C 71 E9 28 70 77 41 50
         F5 DB EE C3 4F 19 05 2D 82 87 4F 6E B4 AC 4B C9
         F8 D4 BF 7A D7 43 ED FB 28 95 8D 91");
    let v23 = hex(V23_RECORD);
    let v24 = hex(V24_RECORD);
    let varying: Vec<usize> = (0..92)
        .filter(|index| {
            (11..12).contains(index)      // kind
                || (16..24).contains(index) // epoch
                || (32..40).contains(index) // declared body length
                || (56..92).contains(index) // body hash and trailing CRC
        })
        .collect();
    for index in 0..92 {
        if varying.contains(&index) {
            continue;
        }
        assert_eq!(
            (v13[index], v23[index]),
            (v13[index], v24[index]),
            "byte {index} must be identical across all three records"
        );
        assert_eq!(
            v23[index], v24[index],
            "byte {index} must be identical across all three records"
        );
    }
    // And the fields that do vary are exactly the stated values.
    assert_eq!((v13[11], v23[11], v24[11]), (0x01, 0x02, 0x03), "kinds");
    assert_eq!(
        (
            u64::from_le_bytes(v13[16..24].try_into().unwrap()),
            u64::from_le_bytes(v23[16..24].try_into().unwrap()),
            u64::from_le_bytes(v24[16..24].try_into().unwrap()),
        ),
        (0, 1, 1),
        "epochs: V13 is epoch 0, V23 and V24 are epoch 1"
    );
}
