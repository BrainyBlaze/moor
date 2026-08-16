use moor::runtime::private::{
    AdoptedLaunchOutcome, AdoptionReceiptError, ArtifactConfig, LaunchRecordObservation,
    SessionState, SupervisedLaunchCause, adoption_receipt, age, await_launch, await_launch_probe,
    classify_adopted_launch, clear_store, companion, decode_launch_record, decode_launch_result,
    discover_sessions, environment_key, first_failed_record, holder_artifacts, instrument_ack,
    instrument_stage, launch_result, lifecycle_running, monotonic, now, observe_launch_result_with,
    parse_boot_uuid, random_array, read_adoption_receipt_with, read_supervised_launch_record_with,
    session_name, supervised_generation, test_creator_rollback_authority,
    test_never_resumed_death_proof, validate_instrument_ack,
};
use moor::runtime::storage::SessionStorage;
use moor::store::{Kind, Store};
use moor::wire::{get_wide, put_wide};
use std::cell::Cell;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Cursor, Read};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct AdoptionObservedInput {
    input: Cursor<Vec<u8>>,
    adopted: Rc<Cell<Option<u32>>>,
}

#[test]
fn discovery_suffixes_share_exact_case_sensitive_and_insensitive_rules() {
    for suffix in [".log", ".events", ".instrument"] {
        assert_eq!(session_name(format!("session{suffix}").into(), false), None);
    }
    assert_eq!(
        session_name(OsString::from("session.exit"), false),
        Some(OsString::from("session"))
    );
    assert_eq!(
        session_name(OsString::from("session.EXIT"), true),
        Some(OsString::from("session"))
    );
    assert_eq!(
        session_name(OsString::from("session.LOG"), false),
        Some(OsString::from("session.LOG"))
    );
    assert_eq!(session_name(OsString::from("session.LOG"), true), None);
}

impl Read for AdoptionObservedInput {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.input.position() >= 12 {
            assert_eq!(
                self.adopted.get(),
                Some(42),
                "the state-1 callback must run before reading the next record"
            );
        }
        self.input.read(output)
    }
}

fn adoption_probe(bytes: Vec<u8>, published: bool) -> (Result<(u16, u32), String>, Option<u32>) {
    let adopted = Rc::new(Cell::new(None));
    let input = AdoptionObservedInput {
        input: Cursor::new(bytes),
        adopted: adopted.clone(),
    };
    let result = await_launch_probe(
        input,
        |_| published,
        |generation| {
            assert_eq!(adopted.replace(Some(generation)), None);
        },
    );
    (result, adopted.get())
}

#[test]
fn private_records_are_exact_and_generation_fenced() {
    let mut launch = [0; 32];
    launch[..8].copy_from_slice(b"MOORLCH3");
    launch[8] = 1;
    launch[12..16].copy_from_slice(&42u32.to_le_bytes());
    launch[16..].fill(7);
    assert_eq!(decode_launch_record(&launch), Some(42));
    launch[9] = 1;
    assert!(decode_launch_record(&launch).is_none());

    let nonce = [9; 16];
    let ack = instrument_ack(42, 123, nonce).unwrap();
    assert!(validate_instrument_ack(&ack, true, 42, 123, nonce).is_ok());
    assert!(validate_instrument_ack(&ack, false, 42, 123, nonce).is_err());
}

#[test]
fn background_result_and_instrument_stage_are_identity_bound() {
    let ready = launch_result(3, 127, 42).unwrap();
    assert_eq!(ready, [b'M', b'O', b'R', b'R', 1, 3, 127, 0, 42, 0, 0, 0]);
    assert_eq!(decode_launch_result(&ready), Some((3, 127, 42)));
    assert!(launch_result(0, 0, 42).is_none());
    assert!(decode_launch_result(&ready[..11]).is_none());

    let root = std::path::Path::new("/protected/root");
    let stage = instrument_stage(root, b"\x01/test", 42, [9; 16]).unwrap();
    assert_eq!(
        stage,
        root.join("a1d6a07694aa1392bcfe49bae764a080faa38c3e33f73e72843b7c8194f8e6f3.instrument")
    );
    assert_ne!(
        stage,
        instrument_stage(root, b"\x01/test", 43, [9; 16]).unwrap()
    );

    let complete = [
        launch_result(1, 0, 42).unwrap(),
        launch_result(2, 0, 42).unwrap(),
    ]
    .concat();
    assert_eq!(await_launch(std::io::Cursor::new(complete)), Ok((0, 42)));
    assert_eq!(await_launch(std::io::Cursor::new(ready)), Ok((127, 42)));
}

#[test]
fn launch_adoption_is_reported_before_every_state_one_continuation() {
    let adopted = launch_result(1, 0, 42).unwrap();

    let ready = [adopted, launch_result(2, 0, 42).unwrap()].concat();
    assert_eq!(adoption_probe(ready, false), (Ok((0, 42)), Some(42)));

    let failed = [adopted, launch_result(3, 127, 42).unwrap()].concat();
    assert_eq!(adoption_probe(failed, false), (Ok((127, 42)), Some(42)));

    assert_eq!(
        adoption_probe(adopted.to_vec(), false),
        (Err("holder failed before launch".into()), Some(42))
    );
    assert_eq!(
        adoption_probe(adopted.to_vec(), true),
        (Ok((0, 42)), Some(42))
    );

    let malformed = [adopted, [0; 12]].concat();
    assert_eq!(
        adoption_probe(malformed, false),
        (
            Err("holder returned an invalid launch result".into()),
            Some(42)
        )
    );
}

#[test]
fn state_three_without_adoption_does_not_invoke_the_callback() {
    let failed = launch_result(3, 127, 42).unwrap();
    assert_eq!(
        adoption_probe(failed.to_vec(), false),
        (Ok((127, 42)), None)
    );
}

fn launch_transaction_record_case(
    bytes: Vec<u8>,
    remaining: Vec<Duration>,
    polls: Vec<PollStep>,
) -> LaunchRecordObservation {
    let mut input = Cursor::new(bytes);
    let mut remaining = VecDeque::from(remaining);
    let mut polls = VecDeque::from(polls);
    observe_launch_result_with(
        &mut input,
        || remaining.pop_front().unwrap_or(Duration::from_secs(1)),
        |_| match polls.pop_front().unwrap_or(PollStep::Eof) {
            PollStep::Available(count) => Ok(Some(count)),
            PollStep::Eof => Ok(None),
            PollStep::Error => Err(io::Error::other("injected poll failure")),
        },
    )
}

#[test]
fn launch_transaction_first_record_preserves_every_indeterminate_boundary() {
    let failed = launch_result(3, 17, 42).unwrap();
    let observed =
        launch_transaction_record_case(failed.to_vec(), vec![], vec![PollStep::Available(12)]);
    assert_eq!(observed, LaunchRecordObservation::Complete(3, 17, 42));
    let pending = first_failed_record(&observed, 42).expect("exact failure was not classified");
    assert_eq!((pending.generation(), pending.result()), (42, 17));
    assert!(first_failed_record(&observed, 43).is_none());

    for length in 0..12 {
        let prefix = failed[..length].to_vec();
        let polls = if length == 0 {
            vec![PollStep::Eof]
        } else {
            vec![PollStep::Available(length), PollStep::Eof]
        };
        assert_eq!(
            launch_transaction_record_case(prefix.clone(), vec![], polls),
            LaunchRecordObservation::Eof(prefix),
            "lost the exact EOF boundary after {length} bytes"
        );
    }

    let prefix = failed[..7].to_vec();
    assert_eq!(
        launch_transaction_record_case(
            prefix.clone(),
            vec![Duration::from_secs(1), Duration::ZERO],
            vec![PollStep::Available(prefix.len())],
        ),
        LaunchRecordObservation::Timeout(prefix.clone())
    );
    assert_eq!(
        launch_transaction_record_case(
            prefix.clone(),
            vec![],
            vec![PollStep::Available(prefix.len()), PollStep::Error],
        ),
        LaunchRecordObservation::ReadError(prefix)
    );
    let invalid = [0xa5; 12];
    assert_eq!(
        launch_transaction_record_case(invalid.to_vec(), vec![], vec![PollStep::Available(12)],),
        LaunchRecordObservation::Invalid(invalid)
    );
}

struct RollbackCapability {
    dropped: Rc<Cell<u32>>,
    deleted: Rc<Cell<bool>>,
}

impl RollbackCapability {
    fn delete(self) {
        self.deleted.set(true);
    }
}

impl Drop for RollbackCapability {
    fn drop(&mut self) {
        self.dropped.set(self.dropped.get() + 1);
    }
}

fn rollback_capability() -> (RollbackCapability, Rc<Cell<u32>>, Rc<Cell<bool>>) {
    let dropped = Rc::new(Cell::new(0));
    let deleted = Rc::new(Cell::new(false));
    (
        RollbackCapability {
            dropped: dropped.clone(),
            deleted: deleted.clone(),
        },
        dropped,
        deleted,
    )
}

#[test]
fn launch_transaction_authority_is_one_way_and_exact_proof_gated() {
    let nonce = [9; 16];
    let adopted_record = LaunchRecordObservation::Complete(1, 0, 42);
    let (capability, dropped, deleted) = rollback_capability();
    let authority = test_creator_rollback_authority(42, capability);
    let running = authority.process_created().resume_attempted();
    let Ok((adopted, receipt)) = running.accept_store_adopted(&adopted_record, nonce) else {
        panic!("exact state 1 did not consume pre-adoption authority");
    };
    assert_eq!(adopted.generation(), 42);
    assert_eq!(receipt, adoption_receipt(42, nonce).unwrap());
    assert_eq!(dropped.get(), 1, "rollback guards were not dropped once");
    assert!(!deleted.get(), "adoption invoked rollback deletion");

    assert_eq!(
        classify_adopted_launch(
            &adopted,
            &LaunchRecordObservation::Complete(2, 0, 42),
            false,
        ),
        AdoptedLaunchOutcome::Ready
    );
    assert_eq!(
        classify_adopted_launch(
            &adopted,
            &LaunchRecordObservation::Complete(3, 23, 42),
            false,
        ),
        AdoptedLaunchOutcome::Failed(23)
    );
    assert_eq!(
        classify_adopted_launch(&adopted, &LaunchRecordObservation::Eof(Vec::new()), true,),
        AdoptedLaunchOutcome::Ready,
        "authenticated publication did not recover EOF after adoption"
    );
    assert_eq!(
        classify_adopted_launch(&adopted, &LaunchRecordObservation::Complete(2, 0, 43), true,),
        AdoptedLaunchOutcome::Indeterminate,
        "a generation discontinuity was accepted"
    );

    let (capability, dropped, deleted) = rollback_capability();
    let running = test_creator_rollback_authority(42, capability)
        .process_created()
        .resume_attempted();
    let failed_record = LaunchRecordObservation::Complete(3, 17, 42);
    let pending = first_failed_record(&failed_record, 42).unwrap();
    assert_eq!(dropped.get(), 0);
    assert!(!deleted.get());
    let proof = pending.test_confirm_exact_holder_death();
    let Ok(capability) = running.rollback_after_first_failed(proof) else {
        panic!("exact first-failed/death proof did not release rollback capability");
    };
    capability.delete();
    assert_eq!(dropped.get(), 1);
    assert!(deleted.get());

    let (capability, dropped, deleted) = rollback_capability();
    let armed = test_creator_rollback_authority(42, capability).process_created();
    let capability = armed.rollback_never_resumed(test_never_resumed_death_proof());
    capability.delete();
    assert_eq!(dropped.get(), 1);
    assert!(deleted.get());

    let failed = launch_result(3, 17, 42).unwrap();
    for observation in [
        LaunchRecordObservation::Eof(Vec::new()),
        LaunchRecordObservation::Eof(failed[..7].to_vec()),
        LaunchRecordObservation::Timeout(Vec::new()),
        LaunchRecordObservation::ReadError(Vec::new()),
        LaunchRecordObservation::Invalid([0; 12]),
        LaunchRecordObservation::Complete(3, 17, 43),
    ] {
        assert!(
            first_failed_record(&observation, 42).is_none(),
            "indeterminate observation manufactured a death-proof precursor: {observation:?}"
        );
        let (capability, dropped, deleted) = rollback_capability();
        let running = test_creator_rollback_authority(42, capability)
            .process_created()
            .resume_attempted();
        drop(running);
        assert_eq!(dropped.get(), 1);
        assert!(!deleted.get(), "indeterminate observation deleted storage");
    }
}

fn adoption_receipt_case(
    bytes: Vec<u8>,
    remaining: Vec<Duration>,
    polls: Vec<PollStep>,
    generation: u32,
    nonce: [u8; 16],
) -> Result<moor::runtime::private::AdoptionReceiptAccepted, AdoptionReceiptError> {
    let mut input = Cursor::new(bytes);
    let mut remaining = VecDeque::from(remaining);
    let mut polls = VecDeque::from(polls);
    read_adoption_receipt_with(
        &mut input,
        || remaining.pop_front().unwrap_or(Duration::from_secs(1)),
        |_| match polls.pop_front().unwrap_or(PollStep::Eof) {
            PollStep::Available(count) => Ok(Some(count)),
            PollStep::Eof => Ok(None),
            PollStep::Error => Err(io::Error::other("injected poll failure")),
        },
        generation,
        nonce,
    )
}

#[test]
fn launch_transaction_receipt_is_exact_deadline_bound_and_gates_continuation() {
    let nonce = [0x5a; 16];
    let receipt = adoption_receipt(42, nonce).unwrap();
    let mut expected = [0; 32];
    expected[..8].copy_from_slice(b"MOORACK1");
    expected[8] = 1;
    expected[12..16].copy_from_slice(&42u32.to_le_bytes());
    expected[16..].copy_from_slice(&nonce);
    assert_eq!(receipt, expected);

    let accepted = adoption_receipt_case(
        receipt.to_vec(),
        vec![],
        vec![PollStep::Available(32), PollStep::Eof],
        42,
        nonce,
    )
    .unwrap();
    let initialized = Rc::new(Cell::new(false));
    let launched = Rc::new(Cell::new(false));
    accepted.continue_holder({
        let initialized = initialized.clone();
        let launched = launched.clone();
        move || {
            initialized.set(true);
            launched.set(true);
        }
    });
    assert!(initialized.get() && launched.get());

    for length in 0..32 {
        let polls = if length == 0 {
            vec![PollStep::Eof]
        } else {
            vec![PollStep::Available(length), PollStep::Eof]
        };
        assert_eq!(
            adoption_receipt_case(receipt[..length].to_vec(), vec![], polls, 42, nonce)
                .unwrap_err(),
            AdoptionReceiptError::WrongLength,
            "accepted EOF after {length} acknowledgement bytes"
        );
    }

    let mut long = receipt.to_vec();
    long.push(0xaa);
    assert_eq!(
        adoption_receipt_case(long, vec![], vec![PollStep::Available(33)], 42, nonce,).unwrap_err(),
        AdoptionReceiptError::WrongLength
    );

    for index in 0..32 {
        let mut malformed = receipt;
        malformed[index] ^= 0xff;
        assert_eq!(
            adoption_receipt_case(
                malformed.to_vec(),
                vec![],
                vec![PollStep::Available(32), PollStep::Eof],
                42,
                nonce,
            )
            .unwrap_err(),
            AdoptionReceiptError::Malformed,
            "accepted acknowledgement corruption at byte {index}"
        );
    }

    assert_eq!(
        adoption_receipt_case(
            receipt.to_vec(),
            vec![Duration::from_secs(1), Duration::ZERO],
            vec![PollStep::Available(32)],
            42,
            nonce,
        )
        .unwrap_err(),
        AdoptionReceiptError::Timeout,
        "exact bytes without EOF did not time out"
    );
    assert_eq!(
        adoption_receipt_case(
            receipt.to_vec(),
            vec![Duration::from_secs(1), Duration::ZERO],
            vec![PollStep::Available(32), PollStep::Eof],
            42,
            nonce,
        )
        .unwrap_err(),
        AdoptionReceiptError::Timeout,
        "late EOF was accepted"
    );
    assert_eq!(
        adoption_receipt_case(vec![], vec![], vec![PollStep::Error], 42, nonce).unwrap_err(),
        AdoptionReceiptError::IoError
    );

    struct ReadFailure;
    impl Read for ReadFailure {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("injected receipt read failure"))
        }
    }
    assert_eq!(
        read_adoption_receipt_with(
            &mut ReadFailure,
            || Duration::from_secs(1),
            |_| Ok(Some(1)),
            42,
            nonce,
        )
        .unwrap_err(),
        AdoptionReceiptError::IoError
    );

    let initialized = Rc::new(Cell::new(false));
    let launched = Rc::new(Cell::new(false));
    let invalid = adoption_receipt_case(
        receipt[..31].to_vec(),
        vec![],
        vec![PollStep::Available(31), PollStep::Eof],
        42,
        nonce,
    );
    if let Ok(accepted) = invalid {
        accepted.continue_holder({
            let initialized = initialized.clone();
            let launched = launched.clone();
            move || {
                initialized.set(true);
                launched.set(true);
            }
        });
    }
    assert!(!initialized.get() && !launched.get());
}

#[test]
fn shared_random_source_rejects_the_zero_identity() {
    assert_ne!(random_array::<16>().unwrap(), [0; 16]);
}

#[test]
fn linux_boot_identity_requires_the_canonical_uuid_grammar() {
    let expected = 0x00112233_4455_6677_8899_aabbccddeeff_u128.to_be_bytes();
    assert_eq!(
        parse_boot_uuid("00112233-4455-6677-8899-aabbccddeeff\n"),
        Some(expected)
    );
    assert_eq!(
        parse_boot_uuid("00112233-4455-6677-8899-AABBCCDDEEFF"),
        Some(expected)
    );
    for malformed in [
        "001122334455-6677-8899-aabbccddeeff",
        "00112233-44556677-8899-aabbccddeeff",
        "00112233-4455-6677-8899aabb-ccddeeff",
        "{00112233-4455-6677-8899-aabbccddeeff}",
        "00112233-4455-6677-8899-aabbccddeeff\n\n",
        "00000000-0000-0000-0000-000000000000",
    ] {
        assert_eq!(parse_boot_uuid(malformed), None, "{malformed:?}");
    }
}

#[test]
fn deadline_clock_is_a_distinct_monotonic_domain() {
    let first = monotonic();
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(monotonic() > first);
    assert!(now().saturating_sub(monotonic()) > 1_000_000_000);
}

#[test]
fn discovery_applies_one_deadline_to_the_complete_listing() {
    let root = std::env::temp_dir().join(format!(
        "moor-discovery-deadline-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    for index in 0..20 {
        fs::write(root.join(index.to_string()), b"").unwrap();
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let started = Instant::now();
    let entries = pool.install(|| {
        discover_sessions(&root, Some, |_, remaining| {
            std::thread::sleep(remaining.min(Duration::from_millis(250)));
            SessionState::Stale
        })
        .unwrap()
    });
    assert!(
        started.elapsed() < Duration::from_millis(2250),
        "listing took {:?}",
        started.elapsed()
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.state == SessionState::Indeterminate)
    );
    fs::remove_dir_all(root).unwrap();
}

fn supervised_case(
    selector: Option<&str>,
    first: Option<&str>,
    second: Option<&str>,
    record: Result<u32, SupervisedLaunchCause>,
    deferred: Option<SupervisedLaunchCause>,
) -> Result<(u32, bool), SupervisedLaunchCause> {
    let invoked = std::ffi::OsStr::new("moor-private-supervised-launch-test");
    let key = environment_key(invoked, "_GENERATION");
    let launch = environment_key(invoked, "_LAUNCH_CHANNEL");
    unsafe {
        for (key, value) in [
            (launch.as_os_str(), selector),
            (key.as_os_str(), first),
            (std::ffi::OsStr::new("MOOR_SESSION_GENERATION"), second),
        ] {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
    let result = supervised_generation(invoked, true, deferred, |value| {
        assert_eq!(value, "channel");
        record
    });
    assert!(std::env::var_os(&launch).is_none());
    assert!(std::env::var_os(&key).is_none());
    assert!(std::env::var_os("MOOR_SESSION_GENERATION").is_none());
    result
}

#[test]
fn supervised_launch_causes_are_typed_ordered_and_exact() {
    use SupervisedLaunchCause::{
        ChannelTimeout, GenerationDisagree, GenerationMalformed, GenerationMismatch,
        GenerationMissing, IoError, PreparationFailed, RecordMalformed, RecordWrongLength,
        SelectorInvalid,
    };

    assert_eq!(
        supervised_case(None, Some("poison"), Some("poison"), Ok(42), None),
        Ok((1, false))
    );
    assert_eq!(
        supervised_case(Some("channel"), None, Some("malformed"), Ok(42), None),
        Err(GenerationMissing),
        "a missing carrier wins over a malformed present carrier"
    );
    for value in ["0", "1", "02", "4294967296", "not-a-generation"] {
        assert_eq!(
            supervised_case(Some("channel"), Some(value), Some(value), Ok(42), None),
            Err(GenerationMalformed),
            "accepted malformed/range-invalid carrier {value:?}"
        );
    }
    assert_eq!(
        supervised_case(Some("channel"), Some("41"), Some("42"), Ok(42), None),
        Err(GenerationDisagree)
    );
    assert_eq!(
        supervised_case(Some("channel"), Some("42"), Some("42"), Ok(43), None),
        Err(GenerationMismatch)
    );
    assert_eq!(
        supervised_case(Some("channel"), Some("42"), Some("42"), Ok(42), None),
        Ok((42, true))
    );

    for cause in [
        SelectorInvalid,
        ChannelTimeout,
        RecordWrongLength,
        RecordMalformed,
        IoError,
    ] {
        assert_eq!(
            supervised_case(
                Some("channel"),
                Some("42"),
                Some("42"),
                Err(cause),
                Some(PreparationFailed),
            ),
            Err(cause),
            "selector/channel/record cause lost precedence"
        );
    }
    assert_eq!(
        supervised_case(
            Some("channel"),
            None,
            Some("42"),
            Ok(42),
            Some(PreparationFailed),
        ),
        Err(GenerationMissing),
        "carrier failure lost precedence over deferred preparation"
    );
    assert_eq!(
        supervised_case(
            Some("channel"),
            Some("42"),
            Some("42"),
            Ok(42),
            Some(PreparationFailed),
        ),
        Err(PreparationFailed)
    );
    assert_eq!(
        launch_result(3, 1, 42).and_then(|bytes| decode_launch_result(&bytes)),
        Some((3, 1, 42)),
        "preparation-failed must use MORR state 3 result 1"
    );

    for (cause, text) in [
        (GenerationMissing, "generation-missing"),
        (GenerationMalformed, "generation-malformed"),
        (GenerationDisagree, "generation-disagree"),
        (SelectorInvalid, "selector-invalid"),
        (ChannelTimeout, "channel-timeout"),
        (RecordWrongLength, "record-wrong-length"),
        (RecordMalformed, "record-malformed"),
        (GenerationMismatch, "generation-mismatch"),
        (PreparationFailed, "preparation-failed"),
        (IoError, "io-error"),
    ] {
        assert_eq!(cause.as_str(), text);
        assert_eq!(
            cause.rejection(),
            format!("supervised launch rejected ({text})")
        );
    }
}

enum PollStep {
    Available(usize),
    Eof,
    Error,
}

fn supervised_record_case(
    bytes: Vec<u8>,
    remaining: Vec<Duration>,
    polls: Vec<PollStep>,
) -> Result<u32, SupervisedLaunchCause> {
    let mut input = Cursor::new(bytes);
    let mut remaining = VecDeque::from(remaining);
    let mut polls = VecDeque::from(polls);
    read_supervised_launch_record_with(
        &mut input,
        || remaining.pop_front().unwrap_or(Duration::from_secs(1)),
        |_| match polls.pop_front().unwrap_or(PollStep::Eof) {
            PollStep::Available(count) => Ok(Some(count)),
            PollStep::Eof => Ok(None),
            PollStep::Error => Err(io::Error::other("injected poll failure")),
        },
    )
}

#[test]
fn supervised_launch_record_boundaries_are_typed_without_sleeping() {
    use SupervisedLaunchCause::{ChannelTimeout, IoError, RecordMalformed, RecordWrongLength};

    let mut record = [0; 32];
    record[..8].copy_from_slice(b"MOORLCH3");
    record[8] = 1;
    record[12..16].copy_from_slice(&42u32.to_le_bytes());
    record[16..].fill(7);

    for length in 0..32 {
        let polls = if length == 0 {
            vec![PollStep::Eof]
        } else {
            vec![PollStep::Available(length), PollStep::Eof]
        };
        assert_eq!(
            supervised_record_case(record[..length].to_vec(), vec![], polls),
            Err(RecordWrongLength),
            "accepted clean EOF after {length} bytes"
        );
    }

    let mut long = record.to_vec();
    long.push(0xaa);
    assert_eq!(
        supervised_record_case(
            long,
            vec![],
            vec![PollStep::Available(33), PollStep::Available(1)],
        ),
        Err(RecordWrongLength),
        "byte 33 must reject immediately"
    );
    assert_eq!(
        supervised_record_case(
            record.to_vec(),
            vec![Duration::from_secs(1), Duration::ZERO],
            vec![PollStep::Available(32)],
        ),
        Err(ChannelTimeout),
        "exact bytes without EOF must time out"
    );
    assert_eq!(
        supervised_record_case(vec![], vec![], vec![PollStep::Error]),
        Err(IoError)
    );
    assert_eq!(
        supervised_record_case(
            record.to_vec(),
            vec![],
            vec![PollStep::Available(32), PollStep::Eof],
        ),
        Ok(42)
    );
    record[9] = 1;
    assert_eq!(
        supervised_record_case(
            record.to_vec(),
            vec![],
            vec![PollStep::Available(32), PollStep::Eof],
        ),
        Err(RecordMalformed)
    );
}

#[test]
fn environment_keys_transform_encoded_bytes_not_unicode_characters() {
    assert_eq!(
        environment_key(std::ffi::OsStr::new("mø-or"), "_SESSION"),
        "M___OR_SESSION"
    );
}

#[test]
fn launch_channel_key_uses_the_full_environment_name_boundary() {
    let basename = "a".repeat(113);
    let key = environment_key(std::ffi::OsStr::new(&basename), "_LAUNCH_CHANNEL");
    let bytes = key.as_encoded_bytes();
    assert_eq!(bytes.len(), 127);
    assert_eq!(&bytes[..112], "A".repeat(112).as_bytes());
    assert_eq!(&bytes[112..], b"_LAUNCH_CHANNEL");
}

#[test]
fn wide_values_can_require_or_ignore_a_trailing_payload() {
    let mut bytes = Vec::new();
    put_wide(&mut bytes, b"identity").unwrap();
    bytes.push(1);
    assert_eq!(get_wide(&bytes, 0, false), Some(b"identity".as_slice()));
    assert!(get_wide(&bytes, 0, true).is_none());
}

#[test]
fn lifecycle_age_requires_a_matching_nonzero_boot_identity() {
    let root = std::env::temp_dir().join(format!(
        "moor-lifecycle-age-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let session = root.join("session");
    let boot = [7; 16];
    let running = lifecycle_running(
        b"\x01/test",
        (None, 1),
        [9; 16],
        (900_000, 498_000, boot),
        ("posix-bytes", None, None),
    );
    drop(
        Store::create(
            &companion(&session, ".exit"),
            Kind::Exit,
            1,
            running.as_bytes(),
            0,
            0,
        )
        .unwrap(),
    );
    assert_eq!(age(&session, (500_000, boot)), "2s ago");
    assert_eq!(age(&session, (900_000, [8; 16])), "unknown");
    assert_eq!(age(&session, (500_000, [0; 16])), "unknown");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn clearing_an_empty_log_is_an_idempotent_recovery_noop() {
    let path = std::env::temp_dir().join(format!("moor-empty-clear-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    drop(Store::create(&path, Kind::Log, 1, b"", 0, 0).unwrap());
    clear_store(&path).unwrap();
    let (commit, body) = Store::read_only(&path, Kind::Log, 1).unwrap();
    assert_eq!(
        (commit.index, commit.epoch, commit.start, commit.end, body),
        (1, 1, 0, 0, Vec::new())
    );
    assert_eq!(
        Store::open(&path, Kind::Log, 1).unwrap().selected().index,
        1
    );
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn portable_event_status_advertises_the_selected_commit_frontier() {
    let root = std::env::temp_dir().join(format!("moor-event-status-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    let marker = root.join("session");
    let event = root.join("events");
    let identity = b"\x01/test";
    let artifacts = holder_artifacts(
        identity,
        (None, 1),
        [3; 16],
        [0; 16],
        (4, 5, [6; 16]),
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
    let mut storage = SessionStorage::new(
        artifacts.storage.log,
        artifacts.storage.events,
        artifacts.storage.lifecycle,
        64,
        4 << 20,
    );
    let status = artifacts.status;
    let (commit, _) = Store::read_only(&event, Kind::Event, 1).unwrap();
    let mut at = 4 + identity.len() + 4 + 16;
    assert_eq!(status[at], 2);
    at += 1;
    let path_len = u32::from_le_bytes(status[at..at + 4].try_into().unwrap()) as usize;
    at += 4 + path_len;
    assert_eq!(status[at], commit.body);
    assert_eq!(
        u64::from_le_bytes(status[at + 1..at + 9].try_into().unwrap()),
        commit.index
    );
    assert_eq!(
        u64::from_le_bytes(status[at + 9..at + 17].try_into().unwrap()),
        commit.length
    );
    assert_eq!(&status[at + 17..at + 49], &commit.hash);

    // #23.1: the prebuilt blob necessarily holds the launch commit, so
    // send_status patches these bytes per send from the live selection. That
    // requires the recorded offset to be exactly where the fields sit...
    assert_eq!(commit_at, at, "recorded commit offset");
    // ...and the live accessor to actually advance past the launch commit,
    // which is what OB-39 means by "the commit a reader would select".
    storage.observe(moor::terminal::Observation::Ready).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let selected = || Store::read_only(&event, Kind::Event, 1).unwrap().0;
    while selected().index == commit.index && std::time::Instant::now() < deadline {
        storage.poll();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let live = selected();
    assert!(
        live.index > commit.index,
        "selected commit must advance past the launch commit: {} vs {}",
        live.index,
        commit.index
    );
    assert_ne!(live.hash, commit.hash, "committed body hash must change");
    drop(storage);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejected_status_identity_is_preflighted_before_any_store_is_created() {
    let root = std::env::temp_dir().join(format!(
        "moor-status-preflight-{}-{}",
        std::process::id(),
        now()
    ));
    fs::create_dir(&root).unwrap();
    let marker = root.join("session");
    let identity = b"\x01/status-preflight";
    let oversized = vec![7; (1 << 20) + 1];
    let result = holder_artifacts(
        identity,
        (None, 1),
        [3; 16],
        [0; 16],
        (4, 5, [6; 16]),
        ArtifactConfig {
            marker: &marker,
            event_path: None,
            encoding: "posix-bytes",
            event_identity: Some(&oversized),
            instrument_identity: None,
            event_store: None,
            event_directory: None,
            stores: None,
            event_layout: 2,
            log_cap: 1,
        },
    );
    let Err(error) = result else {
        panic!("oversized status identity was accepted")
    };
    assert!(error.starts_with("protocol error:"), "{error}");
    let leaked = [".exit", ".log"]
        .into_iter()
        .map(|suffix| companion(&marker, suffix))
        .filter(|path| fs::symlink_metadata(path).is_ok())
        .collect::<Vec<_>>();
    fs::remove_dir_all(root).unwrap();
    assert!(leaked.is_empty(), "leaked {leaked:?}");
}
