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
use worker::{Lane, Work};

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

#[test]
fn a_commit_landing_after_its_lane_is_quarantined_still_advances_the_frontier() {
    // The worker checks its open flag before each operation but not during, so
    // an already-issued commit may land after try_complete times out and
    // quarantines the lane. The descriptor must still name it: this is the
    // permanent case a completion-only frontier can never observe. Uses the
    // worker frontier alone — no holder duplicate involved.
    let (mut lane, path) = staged_lane("late-timeout");
    let base = lane.selected().index;
    let (announce, entered) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    lane.submit(
        Purpose::Test(1),
        Work::Staged(b"late".to_vec(), 2, 0, 4, Some((announce, gate)), false),
    )
    .unwrap();
    // Deterministic, not timing-dependent: the operation announces entry and
    // then waits BEFORE its write, so the timeout below is forced while it is
    // already issued but has not yet committed. The commit therefore lands
    // strictly after the lane is quarantined and its job popped, which is the
    // stronger ordering the contract permits.
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("the staged commit never reported entry");
    let done = lane.try_complete(Instant::now() + Duration::from_secs(3));
    assert!(
        matches!(done, Some(Done { result: Err(_), .. })),
        "expected a timeout"
    );
    assert!(!lane.writable(), "the lane must be quarantined");
    // Now let the already-issued commit finish.
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(2);
    while lane.selected().index == base && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        lane.selected().index > base,
        "frontier stayed at {base} after a post-quarantine commit"
    );
    let _ = fs::remove_dir_all(path);
}

#[test]
fn a_valid_candidate_followed_by_a_reported_error_still_advances_the_frontier() {
    // A write or flush can report an error after leaving a candidate readers
    // validate, so publishing only on Ok would leave the frontier behind
    // permanently. Recovery runs after both outcomes.
    let (mut lane, path) = staged_lane("post-write-error");
    let base = lane.selected().index;
    lane.submit(
        Purpose::Test(1),
        Work::Staged(b"kept".to_vec(), 2, 0, 4, None, true),
    )
    .unwrap();
    let done = next(&mut lane);
    assert!(
        matches!(done.result, Err(_)),
        "the operation must report failure"
    );
    assert!(
        lane.selected().index > base,
        "frontier stayed at {base} despite a valid committed candidate"
    );
    let _ = fs::remove_dir_all(path);
}
