use moor::runtime::private::{
    companion, session_name, test_first_failed_record_death_proof,
};
use moor::store::{
    PreparedStore, WindowsPreparationReservation, WindowsPreparedDirectory,
    WindowsPreparedHandleInfo, WindowsPreparedStoreSelectors,
};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

const GENERATION: u32 = 7;
const NONCE: [u8; 16] = [0x5a; 16];
const SLOT_NAMES: [&str; 4] = ["body.0", "body.1", "commit.0", "commit.1"];

struct NativeCase {
    root: PathBuf,
    marker: PathBuf,
}

impl NativeCase {
    fn new(name: &str) -> Self {
        let root = temp(&format!("windows-prepared-{name}"));
        fs::create_dir(&root).unwrap();
        let marker = root.join("session");
        Self { root, marker }
    }

    fn reservation_path(&self) -> PathBuf {
        companion(&self.marker, ".exit.instrument")
    }

    fn lifecycle_path(&self) -> PathBuf {
        companion(&self.marker, ".exit")
    }
}

impl Drop for NativeCase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn reservation_record(generation: u32, nonce: [u8; 16]) -> [u8; 32] {
    let mut record = [0; 32];
    record[..8].copy_from_slice(b"MOORPRE1");
    record[8] = 1;
    record[12..16].copy_from_slice(&generation.to_le_bytes());
    record[16..].copy_from_slice(&nonce);
    record
}

fn exact_failed_proof(
    generation: u32,
) -> moor::runtime::private::FirstFailedRecordDeathProof {
    test_first_failed_record_death_proof(generation)
}

fn assert_blank_uncommitted(path: &PathBuf) {
    let mut entries = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(
        entries,
        SLOT_NAMES
            .into_iter()
            .map(OsStr::new)
            .map(OsStr::to_owned)
            .collect::<Vec<_>>()
    );
    for name in SLOT_NAMES {
        assert_eq!(fs::metadata(path.join(name)).unwrap().len(), 0, "{name}");
    }
    assert!(matches!(
        Store::read_only(path, Kind::Exit, GENERATION),
        Err(StoreError::Corrupt)
    ));
}

fn assert_inventory_relationships(inventory: &[WindowsPreparedHandleInfo]) {
    let owning = inventory
        .iter()
        .filter(|entry| entry.owning)
        .map(|entry| entry.raw)
        .collect::<Vec<_>>();
    assert_eq!(
        owning.iter().copied().collect::<HashSet<_>>().len(),
        owning.len(),
        "two owning capability entries used the same raw HANDLE"
    );

    let mut objects = HashMap::<[u8; 24], u32>::new();
    for entry in inventory {
        if let Some(group) = objects.insert(entry.identity, entry.group) {
            assert_eq!(
                group, entry.group,
                "one kernel object crossed logical capability groups"
            );
        }
    }
}

#[test]
fn reservation_is_the_first_durable_mutation_and_existing_bytes_are_never_trusted() {
    let case = NativeCase::new("reservation");
    let path = case.reservation_path();
    let lifecycle = case.lifecycle_path();
    let reservation =
        WindowsPreparationReservation::create(&case.marker, GENERATION, NONCE).unwrap();
    assert_eq!(reservation.path(), path);
    assert_eq!(reservation.record(), reservation_record(GENERATION, NONCE));
    assert_eq!(fs::read(&path).unwrap(), reservation_record(GENERATION, NONCE));
    assert_eq!(fs::metadata(&path).unwrap().len(), 32);
    assert_ne!(reservation.identity(), [0; 24]);
    assert_inventory_relationships(reservation.windows_inventory());
    assert!(reservation.validate_exact());
    assert!(!lifecycle.exists());

    let displaced = case.root.join("displaced-reservation");
    fs::rename(&path, &displaced).unwrap();
    fs::write(&path, reservation_record(GENERATION, NONCE)).unwrap();
    assert!(!reservation.validate_exact(), "accepted a same-byte successor");
    assert!(PreparedStore::prepare_windows_owned(&reservation, &lifecycle, false).is_err());
    assert!(!lifecycle.exists());
    drop(reservation);

    for (name, bytes) in [
        ("empty", Vec::new()),
        ("short", reservation_record(GENERATION, NONCE)[..31].to_vec()),
        ("malformed", vec![0xa5; 32]),
        ("valid", reservation_record(GENERATION, NONCE).to_vec()),
    ] {
        let attempt = NativeCase::new(name);
        fs::write(attempt.reservation_path(), bytes).unwrap();
        assert!(
            WindowsPreparationReservation::create(&attempt.marker, GENERATION, NONCE).is_err(),
            "trusted existing {name} reservation bytes"
        );
        assert!(!attempt.lifecycle_path().exists());
    }

    let substituted = NativeCase::new("reservation-directory");
    fs::create_dir(substituted.reservation_path()).unwrap();
    assert!(
        WindowsPreparationReservation::create(&substituted.marker, GENERATION, NONCE).is_err()
    );
    assert!(!substituted.lifecycle_path().exists());
}

#[test]
fn prepared_slots_are_blank_unique_and_bound_to_the_exact_directory() {
    let case = NativeCase::new("slots");
    let reservation =
        WindowsPreparationReservation::create(&case.marker, GENERATION, NONCE).unwrap();
    let lifecycle = case.lifecycle_path();
    let prepared =
        PreparedStore::prepare_windows_owned(&reservation, &lifecycle, false).unwrap();
    assert_blank_uncommitted(&lifecycle);

    let metadata = prepared.windows_metadata();
    assert_eq!(metadata.path, lifecycle);
    assert!(metadata.directory_created);
    assert!(metadata.directory_delete_present);
    assert_eq!(
        metadata
            .slot_identities
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len(),
        4
    );
    assert!(!metadata.slot_identities.contains(&metadata.directory_identity));
    assert_inventory_relationships(prepared.windows_inventory());
    assert!(
        prepared
            .windows_inventory()
            .iter()
            .fold(HashMap::<[u8; 24], usize>::new(), |mut counts, entry| {
                *counts.entry(entry.identity).or_default() += 1;
                counts
            })
            .values()
            .filter(|count| **count > 1)
            .count()
            >= 5,
        "directory and four slots lacked distinct guard/writer facets"
    );
}

#[test]
fn transferred_handles_reconstruct_one_holder_lease_and_initialize_once() {
    let case = NativeCase::new("transfer");
    let reservation =
        WindowsPreparationReservation::create(&case.marker, GENERATION, NONCE).unwrap();
    let lifecycle = case.lifecycle_path();
    let prepared =
        PreparedStore::prepare_windows_owned(&reservation, &lifecycle, false).unwrap();
    let (rollback, transfer) = prepared.into_windows_transfer().unwrap();

    assert_inventory_relationships(transfer.windows_inventory());
    assert!(
        transfer
            .windows_inventory()
            .iter()
            .filter(|entry| entry.holder_access)
            .all(|entry| !entry.delete_access)
    );
    assert!(
        transfer
            .windows_inventory()
            .iter()
            .filter(|entry| entry.directory_origin)
            .all(|entry| entry.share_delete)
    );

    let selectors = transfer.selectors();
    let independently_reopened = OpenOptions::new()
        .access_mode(GENERIC_READ | GENERIC_WRITE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(lifecycle.join("commit.0"))
        .unwrap();
    assert_ne!(
        independently_reopened.as_raw_handle() as usize,
        selectors.slots[2]
    );
    let mut reopened_selectors = selectors.clone();
    reopened_selectors.slots[2] = independently_reopened.as_raw_handle() as usize;
    drop(transfer.reconstruct(&reopened_selectors).unwrap());

    let mut holder = transfer.reconstruct(&selectors).unwrap();
    let initial = running(GENERATION);
    let store = holder
        .initialize(Kind::Exit, GENERATION, initial.as_bytes(), 0, 0)
        .unwrap();
    assert_eq!(
        Store::read_only(&lifecycle, Kind::Exit, GENERATION)
            .unwrap()
            .1,
        initial.as_bytes()
    );
    assert!(
        holder
            .initialize(Kind::Exit, GENERATION, initial.as_bytes(), 0, 0)
            .is_err(),
        "initialized one prepared store twice"
    );
    drop(store);
    drop(holder);
    drop(rollback);
}

fn selectors(case: &NativeCase) -> (
    WindowsPreparationReservation,
    moor::store::WindowsPreparedStoreRollback,
    moor::store::WindowsPreparedStoreTransfer,
    WindowsPreparedStoreSelectors,
) {
    let reservation =
        WindowsPreparationReservation::create(&case.marker, GENERATION, NONCE).unwrap();
    let prepared =
        PreparedStore::prepare_windows_owned(&reservation, &case.lifecycle_path(), false).unwrap();
    let (rollback, transfer) = prepared.into_windows_transfer().unwrap();
    let selectors = transfer.selectors();
    (reservation, rollback, transfer, selectors)
}

#[test]
fn reconstruction_rejects_missing_aliased_and_metadata_inconsistent_capabilities() {
    let case = NativeCase::new("reconstruction");
    let (_reservation, _rollback, transfer, exact) = selectors(&case);
    let unprotected_path = case.root.join("unprotected");
    fs::write(&unprotected_path, b"x").unwrap();
    let unprotected = File::open(&unprotected_path).unwrap();

    let mut invalid = Vec::new();

    let mut missing = exact.clone();
    missing.slots[0] = 0;
    invalid.push(("missing", missing));

    let mut duplicate = exact.clone();
    duplicate.slots[1] = duplicate.slots[0];
    invalid.push(("duplicate", duplicate));

    let mut cross_group_alias = exact.clone();
    cross_group_alias.slots[0] = cross_group_alias.directory;
    invalid.push(("cross-group-alias", cross_group_alias));

    let mut wrong_type = exact.clone();
    wrong_type.slots[0] = wrong_type.directory;
    invalid.push(("wrong-type", wrong_type));

    let mut wrong_dacl = exact.clone();
    wrong_dacl.slots[0] = unprotected.as_raw_handle() as usize;
    invalid.push(("wrong-dacl", wrong_dacl));

    let mut wrong_path = exact.clone();
    wrong_path.metadata.path = case.root.join("elsewhere");
    wrong_path.metadata.recompute_digest();
    invalid.push(("wrong-path", wrong_path));

    let mut wrong_identity = exact.clone();
    wrong_identity.metadata.slot_identities[0][0] ^= 0x80;
    wrong_identity.metadata.recompute_digest();
    invalid.push(("wrong-identity", wrong_identity));

    let mut forged_created = exact.clone();
    forged_created.metadata.directory_created = !forged_created.metadata.directory_created;
    forged_created.metadata.directory_delete_present = false;
    forged_created.metadata.recompute_digest();
    invalid.push(("created-without-capability", forged_created));

    let mut missing_delete = exact.clone();
    missing_delete.directory_delete = None;
    invalid.push(("missing-created-directory-capability", missing_delete));

    for (name, selector) in invalid {
        assert!(
            transfer.reconstruct(&selector).is_err(),
            "accepted {name} capability table"
        );
        assert_blank_uncommitted(&case.lifecycle_path());
    }
}

#[test]
fn borrowed_event_directory_is_the_only_same_raw_relation_and_rollback_is_exact() {
    let case = NativeCase::new("rollback");
    let event_path = companion(&case.marker, ".events");
    drop(Store::create(&event_path, Kind::Log, GENERATION, b"", 0, 0).unwrap());
    for name in SLOT_NAMES {
        fs::remove_file(event_path.join(name)).unwrap();
    }
    let reservation =
        WindowsPreparationReservation::create(&case.marker, GENERATION, NONCE).unwrap();
    let event_directory =
        WindowsPreparedDirectory::prepare(&reservation, &event_path, true).unwrap();
    let event = PreparedStore::prepare_windows_borrowed(&reservation, &event_directory).unwrap();
    assert!(!event.windows_metadata().directory_created);
    assert!(!event.windows_metadata().directory_delete_present);

    let directory_raw = event_directory.access_raw();
    let borrowed = event
        .windows_inventory()
        .iter()
        .filter(|entry| !entry.owning)
        .collect::<Vec<_>>();
    assert_eq!(borrowed.len(), 1);
    assert_eq!(borrowed[0].raw, directory_raw);
    assert_inventory_relationships(event_directory.windows_inventory());
    assert_inventory_relationships(event.windows_inventory());

    let (event_rollback, event_transfer) = event.into_windows_transfer().unwrap();
    drop(event_transfer);
    let lifecycle_path = case.lifecycle_path();
    let lifecycle =
        PreparedStore::prepare_windows_owned(&reservation, &lifecycle_path, false).unwrap();
    let (lifecycle_rollback, lifecycle_transfer) = lifecycle.into_windows_transfer().unwrap();
    drop(lifecycle_transfer);

    let displaced = lifecycle_path.join("body.original");
    fs::rename(lifecycle_path.join("body.0"), &displaced).unwrap();
    fs::write(lifecycle_path.join("body.0"), b"successor").unwrap();

    reservation
        .rollback_after_first_failed(
            exact_failed_proof(GENERATION),
            vec![event_rollback, lifecycle_rollback],
            vec![event_directory],
        )
        .unwrap();
    assert!(!case.reservation_path().exists());
    assert_eq!(fs::read(lifecycle_path.join("body.0")).unwrap(), b"successor");
    assert!(event_path.exists(), "event-target owner was deleted out of band");
}

#[test]
fn reservation_name_discovery_stage_and_event_aliases_are_disjoint() {
    let case = NativeCase::new("names");
    let reservation = case.reservation_path();
    assert!(!moor::name::valid_session(reservation.file_name().unwrap()));
    assert_eq!(session_name(reservation.file_name().unwrap().to_owned(), true), None);

    let immutable_stage = case
        .root
        .join(format!("{}.instrument", "a".repeat(64)));
    assert_ne!(reservation, immutable_stage);
    assert!(WindowsPreparationReservation::validate_event_target(
        &case.marker,
        &case.root.join("events")
    )
    .is_ok());
    assert!(
        WindowsPreparationReservation::validate_event_target(&case.marker, &reservation).is_err()
    );
    assert!(!reservation.exists(), "read-only alias validation mutated the reservation");
}
