#[macro_use]
extern crate moor;

pub mod store {
    pub use moor::store::*;
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum Purpose {
    Test(u64),
    Sources(u64, bool),
    Background,
    Lifecycle,
    Final,
}

#[derive(Debug, Eq, PartialEq)]
enum StorageError {
    Disabled,
    Busy,
}

struct Done {
    #[allow(dead_code)]
    lane: usize,
    purpose: Purpose,
    result: Result<(Commit, bool), StoreError>,
}

#[allow(dead_code)]
#[path = "../src/runtime/storage/worker.rs"]
mod worker;

use moor::runtime::private::lifecycle_running;
use moor::store::{Commit, Kind, Store, StoreError};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use worker::{Frontier, Lane, Work};

fn temp(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    std::env::temp_dir().join(format!(
        "moor-lane-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn running() -> String {
    lifecycle_running(
        &[1, b'/', b's'],
        (None, 1),
        [2; 16],
        (1, 2, [3; 16]),
        ("posix-bytes", None, None),
    )
}

fn next(lane: &mut Lane) -> Done {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(done) = lane.try_complete(Instant::now()) {
            return done;
        }
        assert!(Instant::now() < deadline, "worker did not complete");
        std::thread::yield_now();
    }
}

fn frontier(lane: &Lane) -> Commit {
    lane.selected().expect("validated frontier")
}

fn selected(done: Done) -> (u64, u64, u32, u64, u64, u64) {
    let Purpose::Test(id) = done.purpose else {
        panic!("unexpected purpose")
    };
    let (commit, stale) = done.result.expect("unexpected store failure");
    assert!(!stale, "operation unexpectedly reported a stale clear");
    (
        id,
        commit.index,
        commit.epoch,
        commit.length,
        commit.start,
        commit.end,
    )
}

fn capped(
    lane: &mut Lane,
    purpose: Purpose,
    bytes: Vec<u8>,
    cap: u64,
    end: u64,
) -> Result<(), StorageError> {
    lane.submit(purpose, Work::Append(bytes.into(), cap, end))
}

fn replace(
    lane: &mut Lane,
    purpose: Purpose,
    bytes: Vec<u8>,
    epoch: u32,
    start: u64,
    end: u64,
) -> Result<(), StorageError> {
    lane.submit(purpose, Work::Replace(bytes, epoch, start, end))
}

fn clear(lane: &mut Lane, purpose: Purpose, observed: u64, end: u64) -> Result<(), StorageError> {
    lane.submit(purpose, Work::Clear(observed, end))
}

#[test]
fn append_replace_and_read_or_clear_are_fifo_barriers() {
    let path = temp("ordered");
    let store = Store::create(&path, Kind::Log, 1, b"", 0, 0).unwrap();
    let mut lane = Lane::new(store, 8, 1024);
    capped(&mut lane, Purpose::Test(11), b"abc".to_vec(), 1024, 3).unwrap();
    replace(&mut lane, Purpose::Test(12), b"xy".to_vec(), 2, 3, 5).unwrap();
    clear(&mut lane, Purpose::Test(13), 3, 5).unwrap();
    assert_eq!(lane.pending(), 3);

    assert_eq!(selected(next(&mut lane)), (11, 2, 1, 3, 0, 3));
    assert_eq!(lane.pending(), 2);
    assert_eq!(selected(next(&mut lane)), (12, 3, 2, 2, 3, 5));
    assert_eq!(selected(next(&mut lane)), (13, 4, 3, 0, 5, 5));
    assert_eq!(lane.pending(), 0);

    clear(&mut lane, Purpose::Test(14), 3, 5).unwrap();
    let done = next(&mut lane);
    let Purpose::Test(id) = done.purpose else {
        panic!("unexpected purpose")
    };
    let (commit, stale) = done.result.unwrap();
    assert_eq!((id, commit.index, stale), (14, 4, true));
    drop(lane);
    let _ = fs::remove_dir_all(path);
}

#[test]
fn capped_output_rotates_to_the_exact_newest_suffix() {
    let path = temp("capped");
    let store = Store::create(&path, Kind::Log, 1, b"", 0, 0).unwrap();
    let mut lane = Lane::new(store, 4, 1024);
    capped(&mut lane, Purpose::Test(1), b"abc".to_vec(), 4, 3).unwrap();
    assert_eq!(selected(next(&mut lane)), (1, 2, 1, 3, 0, 3));
    capped(&mut lane, Purpose::Test(2), b"def".to_vec(), 4, 6).unwrap();
    assert_eq!(selected(next(&mut lane)), (2, 3, 2, 4, 2, 6));
    drop(lane);
    let (_, body) = Store::read_only(&path, Kind::Log, 1).unwrap();
    assert_eq!(body, b"cdef");
    let _ = fs::remove_dir_all(path);
}

#[test]
fn admission_enforces_byte_and_job_caps_without_running_store_io() {
    let path = temp("caps");
    let store = Store::create(&path, Kind::Log, 1, b"", 0, 0).unwrap();
    let mut lane = Lane::new(store, 1, 2);
    assert!(matches!(
        capped(&mut lane, Purpose::Test(1), b"abc".to_vec(), 1024, 3),
        Err(StorageError::Busy)
    ));
    drop(lane);

    let path2 = temp("zero-jobs");
    let store = Store::create(&path2, Kind::Log, 1, b"", 0, 0).unwrap();
    let mut lane = Lane::new(store, 0, 2);
    assert!(matches!(
        capped(&mut lane, Purpose::Test(2), b"a".to_vec(), 1024, 1),
        Err(StorageError::Busy)
    ));
    drop(lane);
    let _ = fs::remove_dir_all(path);
    let _ = fs::remove_dir_all(path2);
}

#[test]
fn store_failure_closes_writable_health_and_future_admission() {
    let path = temp("failure");
    let running = running();
    let store = Store::create(&path, Kind::Exit, 1, running.as_bytes(), 0, 0).unwrap();
    let mut lane = Lane::new(store, 2, 1024);
    capped(&mut lane, Purpose::Test(1), b"x".to_vec(), 1024, 1).unwrap();
    assert!(matches!(
        next(&mut lane),
        Done {
            purpose: Purpose::Test(1),
            result: Err(StoreError::Corrupt),
            ..
        }
    ));
    assert!(!lane.writable());
    assert!(matches!(
        replace(
            &mut lane,
            Purpose::Test(2),
            b"{\"phase\":\"exited\"}\n".to_vec(),
            1,
            0,
            0
        ),
        Err(StorageError::Disabled)
    ));
    drop(lane);
    let _ = fs::remove_dir_all(path);
}

#[test]
fn missed_progress_deadline_quarantines_the_worker_and_fails_later_jobs() {
    let path = temp("deadline");
    let store = Store::create(&path, Kind::Log, 1, b"", 0, 0).unwrap();
    let mut lane = Lane::new(store, 2, 1024);
    let (release, held) = mpsc::channel::<()>();
    lane.submit(Purpose::Test(1), Work::Hold(held)).unwrap();
    capped(&mut lane, Purpose::Test(2), b"later".to_vec(), 1024, 5).unwrap();
    std::thread::sleep(Duration::from_millis(2_010));
    for id in [1, 2] {
        let done = next(&mut lane);
        let Purpose::Test(actual) = done.purpose else {
            panic!("unexpected purpose")
        };
        let result = done.result;
        assert_eq!(actual, id);
        assert!(
            matches!(result, Err(StoreError::Io(ref error)) if error.kind() == std::io::ErrorKind::TimedOut)
        );
    }
    assert!(!lane.writable());
    assert!(matches!(
        capped(&mut lane, Purpose::Test(3), b"never".to_vec(), 1024, 5),
        Err(StorageError::Disabled)
    ));
    drop(release);
    drop(lane);
    let _ = fs::remove_dir_all(path);
}

#[test]
fn explicit_quarantine_prevents_a_queued_write_after_the_in_flight_operation() {
    let path = temp("quarantine");
    let store = Store::create(&path, Kind::Log, 1, b"", 0, 0).unwrap();
    let mut lane = Lane::new(store, 2, 1024);
    let (release, held) = mpsc::channel::<()>();
    lane.submit(Purpose::Test(1), Work::Hold(held)).unwrap();
    capped(&mut lane, Purpose::Test(2), b"later".to_vec(), 1024, 5).unwrap();
    lane.close();
    for id in [1, 2] {
        let done = next(&mut lane);
        let Purpose::Test(actual) = done.purpose else {
            panic!("unexpected purpose")
        };
        let result = done.result;
        assert_eq!(actual, id);
        assert!(
            matches!(result, Err(StoreError::Io(ref error)) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }
    drop(release);
    std::thread::sleep(Duration::from_millis(20));
    assert!(Store::read_only(&path, Kind::Log, 1).unwrap().1.is_empty());
    drop(lane);
    let _ = fs::remove_dir_all(path);
}

fn staged_lane(name: &str) -> (Lane, PathBuf) {
    let path = temp(name);
    let store = Store::create(&path, Kind::Log, 7, b"", 0, 0).unwrap();
    (Lane::new(store, 4, 1 << 20), path)
}

fn wait_until_receiver_is_dropped(sender: &std::sync::mpsc::SyncSender<()>) {
    use std::sync::mpsc::TrySendError;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match sender.try_send(()) {
            Err(TrySendError::Disconnected(_)) => return,
            Err(TrySendError::Full(_)) if Instant::now() < deadline => std::thread::yield_now(),
            Ok(()) => panic!("worker reached a forbidden queued successor"),
            Err(TrySendError::Full(_)) => panic!("worker did not leave the quarantined lane"),
        }
    }
}

#[test]
fn a_completion_selected_after_its_deadline_is_rejected_even_when_already_queued() {
    let (mut lane, path) = staged_lane("queued-late-completion");
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    lane.submit(
        Purpose::Test(1),
        Work::Staged(b"late".to_vec(), 2, 0, 4, Some((announce, gate)), false),
    )
    .unwrap();
    lane.submit(
        Purpose::Test(2),
        Work::Staged(b"next".to_vec(), 3, 4, 8, None, true),
    )
    .unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    std::thread::sleep(Duration::from_millis(2_010));
    drop(release);
    let wait = Instant::now() + Duration::from_secs(2);
    while lane.writable() && Instant::now() < wait {
        std::thread::yield_now();
    }
    assert!(!lane.writable(), "both FIFO results were not queued");
    let done = lane
        .try_complete(Instant::now())
        .expect("missing first result");
    assert!(
        matches!(done.result, Err(StoreError::Io(ref error)) if error.kind() == std::io::ErrorKind::TimedOut),
        "late queued completion was accepted: {:?}",
        done.result
    );
    assert_eq!(fs::metadata(path.join("body.1")).unwrap().len(), 0);
    assert_eq!(fs::metadata(path.join("commit.1")).unwrap().len(), 0);
    assert_eq!(frontier(&lane).index, 1, "a queued successor was selected");
    let _ = fs::remove_dir_all(path);
}

#[test]
fn a_completion_selected_on_time_remains_valid_when_the_holder_polls_late() {
    let (mut lane, path) = staged_lane("queued-on-time-completion");
    let base = frontier(&lane).index;
    lane.submit(
        Purpose::Test(1),
        Work::Staged(b"kept".to_vec(), 2, 0, 4, None, false),
    )
    .unwrap();
    let wait = Instant::now() + Duration::from_secs(2);
    while frontier(&lane).index == base && Instant::now() < wait {
        std::thread::yield_now();
    }
    assert!(
        frontier(&lane).index > base,
        "on-time result did not finish"
    );
    std::thread::sleep(Duration::from_millis(2_010));
    let done = lane
        .try_complete(Instant::now())
        .expect("missing on-time result");
    assert!(
        done.result.is_ok(),
        "late polling changed an on-time result"
    );
    assert!(
        lane.writable(),
        "an on-time result must not quarantine its lane"
    );
    let _ = fs::remove_dir_all(path);
}

#[test]
fn quarantine_before_any_write_leaves_both_inactive_slots_empty() {
    let (mut lane, path) = staged_lane("quarantine-before-write");
    let base = frontier(&lane).index;
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (sentinel, held) = mpsc::sync_channel(0);
    lane.submit(
        Purpose::Test(1),
        Work::Staged(
            b"forbidden".to_vec(),
            2,
            0,
            9,
            Some((announce, gate)),
            false,
        ),
    )
    .unwrap();
    lane.submit(Purpose::Test(2), Work::Hold(held)).unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let done = lane
        .try_complete(Instant::now() + Duration::from_secs(3))
        .expect("missing timeout");
    assert!(done.result.is_err(), "expected timeout quarantine");
    drop(release);
    wait_until_receiver_is_dropped(&sentinel);
    assert_eq!(fs::metadata(path.join("body.1")).unwrap().len(), 0);
    assert_eq!(fs::metadata(path.join("commit.1")).unwrap().len(), 0);
    assert_eq!(frontier(&lane).index, base);
    let _ = fs::remove_dir_all(path);
}

#[test]
fn quarantine_after_body_flush_prevents_the_not_yet_issued_commit() {
    let (mut lane, path) = staged_lane("quarantine-before-commit");
    let base = frontier(&lane).index;
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (sentinel, held) = mpsc::sync_channel(0);
    lane.submit(
        Purpose::Test(1),
        Work::Phased(b"inactive".to_vec(), 2, 0, 8, 1, announce, gate, false),
    )
    .unwrap();
    lane.submit(Purpose::Test(2), Work::Hold(held)).unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let done = lane
        .try_complete(Instant::now() + Duration::from_secs(3))
        .expect("missing timeout");
    assert!(done.result.is_err(), "expected timeout quarantine");
    drop(release);
    wait_until_receiver_is_dropped(&sentinel);
    assert_eq!(fs::metadata(path.join("body.1")).unwrap().len(), 8);
    assert_eq!(fs::metadata(path.join("commit.1")).unwrap().len(), 0);
    assert_eq!(frontier(&lane).index, base);
    let _ = fs::remove_dir_all(path);
}

#[test]
fn quarantined_append_cannot_select_its_flushed_tail() {
    let (mut lane, path) = staged_lane("append-before-commit");
    let base = frontier(&lane).index;
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (sentinel, held) = mpsc::sync_channel(0);
    lane.submit(
        Purpose::Test(1),
        Work::AppendPhased(b"inactive".to_vec(), 64, 8, 1, announce, gate),
    )
    .unwrap();
    lane.submit(Purpose::Test(2), Work::Hold(held)).unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        lane.try_complete(Instant::now() + Duration::from_secs(3))
            .unwrap()
            .result
            .is_err()
    );
    drop(release);
    wait_until_receiver_is_dropped(&sentinel);
    assert_eq!(fs::metadata(path.join("body.0")).unwrap().len(), 8);
    assert_eq!(fs::metadata(path.join("commit.1")).unwrap().len(), 0);
    assert_eq!(frontier(&lane).index, base);
    assert!(Store::read_only(&path, Kind::Log, 7).unwrap().1.is_empty());
    let _ = fs::remove_dir_all(path);
}

#[test]
fn quarantined_clear_cannot_replace_the_selected_log() {
    let (mut lane, path) = staged_lane("clear-before-commit");
    capped(&mut lane, Purpose::Test(0), b"old".to_vec(), 64, 3).unwrap();
    let base = selected(next(&mut lane)).1;
    let prior_commit = fs::read(path.join("commit.0")).unwrap();
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (sentinel, held) = mpsc::sync_channel(0);
    lane.submit(
        Purpose::Test(1),
        Work::ClearPhased(base, 3, 1, announce, gate),
    )
    .unwrap();
    lane.submit(Purpose::Test(2), Work::Hold(held)).unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        lane.try_complete(Instant::now() + Duration::from_secs(3))
            .unwrap()
            .result
            .is_err()
    );
    drop(release);
    wait_until_receiver_is_dropped(&sentinel);
    assert_eq!(fs::read(path.join("commit.0")).unwrap(), prior_commit);
    assert_eq!(frontier(&lane).index, base);
    assert_eq!(Store::read_only(&path, Kind::Log, 7).unwrap().1, b"old");
    let _ = fs::remove_dir_all(path);
}

#[test]
fn an_atomically_issued_commit_may_finish_after_quarantine() {
    let (mut lane, path) = staged_lane("issued-commit-after-quarantine");
    let base = frontier(&lane).index;
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    lane.submit(
        Purpose::Test(1),
        Work::Phased(b"selected".to_vec(), 2, 0, 8, 2, announce, gate, false),
    )
    .unwrap();
    // Stage 2 is reached only after the alternate commit has been completely
    // written and truncated and the atomic BODY -> COMMIT transition has won.
    // Quarantine may therefore leave only that commit's already-issued flush
    // to finish; it must never permit a later write or truncate operation.
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        fs::metadata(path.join("commit.1")).unwrap().len(),
        92,
        "COMMIT was announced before its mutation syscalls completed"
    );
    let done = lane
        .try_complete(Instant::now() + Duration::from_secs(3))
        .expect("missing timeout");
    assert!(done.result.is_err(), "expected timeout quarantine");
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(2);
    while frontier(&lane).index == base && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(
        frontier(&lane).index > base,
        "issued commit never published"
    );
    let (selected, body) = Store::read_only(&path, Kind::Log, 7).unwrap();
    assert_eq!(
        (selected.index, body.as_slice()),
        (base + 1, &b"selected"[..])
    );
    let _ = fs::remove_dir_all(path);
}

#[test]
fn quarantine_after_commit_mutation_publishes_an_unknown_frontier() {
    let (mut lane, path) = staged_lane("commit-written-before-issuance");
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    lane.submit(
        Purpose::Test(1),
        Work::Phased(b"ambiguous".to_vec(), 2, 0, 9, 3, announce, gate, false),
    )
    .unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(fs::metadata(path.join("commit.1")).unwrap().len(), 92);
    assert!(
        lane.try_complete(Instant::now() + Duration::from_secs(3))
            .unwrap()
            .result
            .is_err()
    );
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(2);
    while lane.selected().is_some() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(
        lane.selected(),
        None,
        "a possibly selectable commit was hidden"
    );
    let _ = fs::remove_dir_all(path);
}

#[test]
fn a_status_hold_is_nonblocking_during_commit_and_exact_after_publication() {
    let (mut lane, path) = staged_lane("status-linearization");
    let base = frontier(&lane).index;
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    lane.submit(
        Purpose::Test(1),
        Work::Phased(b"frontier".to_vec(), 2, 0, 8, 2, announce, gate, false),
    )
    .unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(!lane.hold(), "a commit-phase lane cannot be snapshotted");
    drop(release);
    assert!(next(&mut lane).result.is_ok());
    assert!(lane.hold(), "an idle lane must be held without waiting");
    let Frontier::Ready(Some(snapshot)) = lane.snapshot() else {
        panic!("validated frontier was not ready")
    };
    assert_eq!(snapshot.index, base + 1);
    lane.release();
    let _ = fs::remove_dir_all(path);
}

#[test]
fn an_on_time_completion_claims_publication_before_deadline_expiry() {
    let (mut lane, path) = staged_lane("publication-deadline");
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    lane.delay_publication(announce, gate);
    lane.submit(
        Purpose::Test(1),
        Work::Staged(b"kept".to_vec(), 2, 0, 4, None, false),
    )
    .unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    std::thread::sleep(Duration::from_millis(2_010));
    assert!(
        lane.try_complete(Instant::now()).is_none(),
        "deadline expiry beat an already selected on-time completion"
    );
    drop(release);
    let done = next(&mut lane);
    assert!(
        done.result.is_ok(),
        "publication contention changed the outcome"
    );
    assert!(lane.writable());
    let _ = fs::remove_dir_all(path);
}

#[test]
fn a_phase_zero_publication_lock_cannot_block_a_status_snapshot() {
    let (lane, path) = staged_lane("phase-zero-publication");
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    lane.block_publication(announce, gate);
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let (started, running) = mpsc::channel();
    let (finished, observed) = mpsc::channel();
    let snapshot = std::thread::spawn(move || {
        assert!(lane.hold());
        let _ = started.send(());
        let busy = matches!(lane.snapshot(), Frontier::Busy);
        lane.release();
        let _ = finished.send(busy);
        lane
    });

    running.recv_timeout(Duration::from_secs(2)).unwrap();
    let prompt = observed.recv_timeout(Duration::from_secs(2));
    drop(release);
    let lane = snapshot.join().unwrap();

    assert_eq!(prompt, Ok(true), "the holder waited or mapped Busy wrongly");
    drop(lane);
    let _ = fs::remove_dir_all(path);
}

#[test]
fn ambiguous_recovery_failure_publishes_an_unknown_frontier_without_panicking() {
    let (mut lane, path) = staged_lane("unknown-frontier");
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    lane.submit(
        Purpose::Test(1),
        Work::Recover(b"candidate".to_vec(), 2, 0, 9, announce, gate),
    )
    .unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    fs::write(path.join("commit.0"), b"torn").unwrap();
    fs::write(path.join("commit.1"), b"torn").unwrap();
    drop(release);
    assert!(next(&mut lane).result.is_err());
    assert_eq!(lane.selected(), None);
    assert!(lane.hold());
    assert!(matches!(lane.snapshot(), Frontier::Ready(None)));
    lane.release();
    let _ = fs::remove_dir_all(path);
}

#[test]
fn a_valid_candidate_followed_by_a_reported_error_still_advances_the_frontier() {
    // A write or flush can report an error after leaving a candidate readers
    // validate, so publishing only on Ok would leave the frontier behind
    // permanently. Recovery runs after both outcomes.
    let (mut lane, path) = staged_lane("post-write-error");
    let base = frontier(&lane).index;
    lane.submit(
        Purpose::Test(1),
        Work::Staged(b"kept".to_vec(), 2, 0, 4, None, true),
    )
    .unwrap();
    let done = next(&mut lane);
    assert!(done.result.is_err(), "the operation must report failure");
    assert!(
        frontier(&lane).index > base,
        "frontier stayed at {base} despite a valid committed candidate"
    );
    let _ = fs::remove_dir_all(path);
}
