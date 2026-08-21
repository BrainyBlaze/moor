use crate::{events::Stored, wire::crc32c};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use fs2::FileExt as _;
use nix::fcntl::OFlag;
use nix::sys::stat::{Mode, fchmod};

const LIMITS: [u64; 4] = [0, 320 << 10, u64::MAX, 4 << 20];
const NAMES: [&str; 4] = ["body.0", "body.1", "commit.0", "commit.1"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    Event = 1,
    Log = 2,
    Exit = 3,
}

schema!(enum pub StoreError [Debug]; Io(std::io::Error), Corrupt, Exhausted);
type Result<T> = std::result::Result<T, StoreError>;
type Slots = [File; 4];
type Meta = (u8, u8, u32, [u64; 3]);
type Mutation<'a> = (Commit, Sha256, u64, &'a [u8], bool);
type Rollback = (File, Vec<[u8; 24]>, bool);

fn require(valid: bool) -> Result<()> {
    valid.then_some(()).ok_or(StoreError::Corrupt)
}
impl From<io::Error> for StoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[doc(hidden)]
schema!(enum pub StoreStep [Clone, Copy, Debug, Eq, PartialEq]; Body, Commit, Flush);

schema!(struct pub Commit derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; slot: u8, body: u8, kind: Kind, generation: u32, epoch: u32, index: u64, length: u64, start: u64, end: u64, hash: [u8; 32]);

impl Commit {
    fn valid(&self) -> bool {
        [self.slot, self.body].into_iter().all(|value| value <= 1)
            && self.generation != 0
            && self.index != 0
            && self.start <= self.end
            && self.length <= LIMITS[self.kind as usize]
    }

    pub fn encode(&self) -> [u8; 92] {
        let mut out = [0; 92];
        out[..12].copy_from_slice(b"MOORCMT1\x01\0\0\0");
        out[9..12].copy_from_slice(&[self.slot, self.body, self.kind as u8]);
        out[12..16].copy_from_slice(&self.generation.to_le_bytes());
        out[16..20].copy_from_slice(&self.epoch.to_le_bytes());
        for (field, value) in out[24..56].as_chunks_mut::<8>().0.iter_mut().zip([
            self.index,
            self.length,
            self.start,
            self.end,
        ]) {
            field.copy_from_slice(&value.to_le_bytes());
        }
        out[56..88].copy_from_slice(&self.hash);
        let checksum = crc32c(&out[..88]);
        out[88..].copy_from_slice(&checksum.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8], slot: u8, kind: Kind, generation: Option<u32>) -> Option<Self> {
        let actual = u32_at(bytes, 12);
        (&bytes[..9] == b"MOORCMT1\x01"
            && bytes[9] == slot
            && bytes[11] == kind as u8
            && bytes[20..24] == [0; 4]
            && generation.is_none_or(|expected| expected == actual)
            && u32_at(bytes, 88) == crc32c(&bytes[..88]))
        .then(|| Self {
            slot,
            body: bytes[10],
            kind,
            generation: actual,
            epoch: u32_at(bytes, 16),
            index: u64_at(bytes, 24),
            length: u64_at(bytes, 32),
            start: u64_at(bytes, 40),
            end: u64_at(bytes, 48),
            hash: bytes[56..88].try_into().unwrap(),
        })
        .filter(Self::valid)
    }
}

fn commit(kind: Kind, generation: u32, meta: Meta, bytes: &[u8]) -> Result<(Commit, Sha256)> {
    let (slot, body, epoch, [index, start, end]) = meta;
    let hash = Sha256::new().chain_update(bytes);
    let selected = Commit {
        slot,
        body,
        kind,
        generation,
        epoch,
        index,
        length: bytes.len() as u64,
        start,
        end,
        hash: hash.clone().finalize().into(),
    };
    require(selected.valid() && body_valid(&selected, bytes))?;
    Ok((selected, hash))
}

schema!(struct pub Store fields; slots: Slots, selected: Commit, hash: Sha256, _rollback: Option<Rollback>);

pub struct PreparedStore(Slots);

impl PreparedStore {
    pub fn raw_descriptors(&self) -> [std::os::fd::RawFd; 4] {
        raw_descriptors(&self.0)
    }

    pub fn lease_at(
        &self,
        directory: &File,
        kind: Kind,
        generation: u32,
        initial: &[u8],
        start: u64,
        end: u64,
    ) -> Result<Store> {
        let (selected, hash) = initial_commit(kind, generation, initial, start..end)?;
        let slots = open_prepared(directory, &self.0)?;
        Ok(Store::from_parts(slots, selected, hash))
    }

    pub fn initialize_leased_at(
        &self,
        directory: &File,
        store: &Store,
        initial: &[u8],
    ) -> Result<()> {
        self.revalidate_at(directory)?;
        directory.sync_all()?;
        durable(&store.slots[0], 0, initial)?;
        durable(&store.slots[2], 0, &store.selected.encode())
    }

    pub fn revalidate_at(&self, directory: &File) -> Result<()> {
        validate_at(directory, &self.0)
    }

    pub fn rollback_at(&self, directory: &File) {
        remove_at(directory, &self.0);
    }
}

impl Store {
    pub fn raw_descriptors(&self) -> [std::os::fd::RawFd; 4] {
        raw_descriptors(&self.slots)
    }

    pub fn remove(path: &Path) -> Result<()> {
        for name in NAMES {
            let _ = fs::remove_file(path.join(name));
        }
        fs::remove_dir(path).map_err(Into::into)
    }

    pub fn create(
        path: &Path,
        kind: Kind,
        generation: u32,
        initial: &[u8],
        start: u64,
        end: u64,
    ) -> Result<Self> {
        initial_commit(kind, generation, initial, start..end)?;
        if kind == Kind::Event && path.exists() {
            validate_event_directory(path)?;
        } else {
            create_directory(path)?;
        }
        let directory = crate::unix::open_directory(path)?;
        let prepared = Self::prepare_at(&directory)?;
        let store = prepared
            .lease_at(&directory, kind, generation, initial, start, end)
            .inspect_err(|_| prepared.rollback_at(&directory))?;
        prepared
            .initialize_leased_at(&directory, &store, initial)
            .inspect_err(|_| prepared.rollback_at(&directory))?;
        Ok(store)
    }

    pub fn prepare_at(directory: &File) -> Result<PreparedStore> {
        prepare_at(directory).map(PreparedStore)
    }

    pub fn open(path: &Path, kind: Kind, generation: impl Into<Option<u32>>) -> Result<Self> {
        let slots = open_slots(path, true)?;
        recover(&slots, kind, generation.into())
            .map(|(selected, hash, _)| Self::from_parts(slots, selected, hash))
    }

    pub fn read_only(
        path: &Path,
        kind: Kind,
        generation: impl Into<Option<u32>>,
    ) -> Result<(Commit, Vec<u8>)> {
        let slots = open_slots(path, false)?;
        recover(&slots, kind, generation.into()).map(|(commit, _, body)| (commit, body))
    }

    pub fn selected(&self) -> &Commit {
        &self.selected
    }

    pub fn duplicate(&self) -> Result<Self> {
        four(|at| self.slots[at].try_clone().map_err(Into::into))
            .map(|slots| Self::from_parts(slots, self.selected, self.hash.clone()))
    }

    pub fn selected_result(&self) -> Result<Commit> {
        let selected = self.selected;
        recover(&self.slots, selected.kind, Some(selected.generation)).map(|(commit, _, _)| commit)
    }

    #[doc(hidden)]
    pub fn append_capped_with(
        &mut self,
        bytes: &[u8],
        cap: u64,
        end: u64,
        mut gate: impl FnMut(StoreStep) -> Result<()>,
    ) -> Result<&Commit> {
        let prior = self.selected;
        let added = bytes.len() as u64;
        require(prior.kind == Kind::Log && prior.end.checked_add(added) == Some(end))?;
        let length = prior
            .length
            .checked_add(added)
            .ok_or(StoreError::Exhausted)?;
        if length <= cap {
            let index = prior.index.checked_add(1).ok_or(StoreError::Exhausted)?;
            let hash = self.hash.clone().chain_update(bytes);
            let selected = Commit {
                slot: 1 - prior.slot,
                index,
                length,
                end,
                hash: hash.clone().finalize().into(),
                ..prior
            };
            return self.install((selected, hash, prior.length, bytes, true), &mut gate);
        }
        let keep = length.min(cap);
        let fresh = usize::try_from(added.min(keep)).map_err(|_| StoreError::Exhausted)?;
        let old = keep - fresh as u64;
        let mut retained = read_range(&self.slots[prior.body as usize], prior.length - old, old)?;
        retained
            .try_reserve_exact(fresh)
            .map_err(|_| StoreError::Exhausted)?;
        retained.extend_from_slice(&bytes[bytes.len() - fresh..]);
        let epoch = prior.epoch.checked_add(1).ok_or(StoreError::Exhausted)?;
        self.replace_with(&retained, epoch, end - keep, end, gate)
    }

    pub fn replace(&mut self, bytes: &[u8], epoch: u32, start: u64, end: u64) -> Result<&Commit> {
        self.replace_with(bytes, epoch, start, end, |_| Ok(()))
    }

    #[doc(hidden)]
    pub fn replace_with(
        &mut self,
        bytes: &[u8],
        epoch: u32,
        start: u64,
        end: u64,
        mut gate: impl FnMut(StoreStep) -> Result<()>,
    ) -> Result<&Commit> {
        let prior = self.selected;
        let index = prior.index.checked_add(1).ok_or(StoreError::Exhausted)?;
        let expected = match prior.kind {
            Kind::Event if epoch == prior.epoch => epoch,
            Kind::Event | Kind::Log => prior.epoch.checked_add(1).ok_or(StoreError::Exhausted)?,
            Kind::Exit if index == 2 => 1,
            Kind::Exit => return Err(StoreError::Exhausted),
        };
        require(epoch == expected)?;
        let meta = (1 - prior.slot, 1 - prior.body, epoch, [index, start, end]);
        let (selected, hash) = commit(prior.kind, prior.generation, meta, bytes)?;
        self.install((selected, hash, 0, bytes, false), &mut gate)
    }

    fn install(
        &mut self,
        (selected, hash, offset, bytes, truncate): Mutation<'_>,
        gate: &mut impl FnMut(StoreStep) -> Result<()>,
    ) -> Result<&Commit> {
        rewrite(
            &self.slots[selected.body as usize],
            offset,
            bytes,
            truncate,
            gate,
            StoreStep::Body,
        )?;
        rewrite(
            &self.slots[2 + selected.slot as usize],
            0,
            &selected.encode(),
            false,
            gate,
            StoreStep::Commit,
        )?;
        self.selected = selected;
        self.hash = hash;
        Ok(&self.selected)
    }

    fn from_parts(slots: Slots, selected: Commit, hash: Sha256) -> Self {
        Self {
            slots,
            selected,
            hash,
            _rollback: None,
        }
    }
}

fn initial_commit(
    kind: Kind,
    generation: u32,
    bytes: &[u8],
    range: std::ops::Range<u64>,
) -> Result<(Commit, Sha256)> {
    let meta = (
        0,
        0,
        u32::from(kind != Kind::Event),
        [1, range.start, range.end],
    );
    commit(kind, generation, meta, bytes)
}

fn body_valid(commit: &Commit, body: &[u8]) -> bool {
    let stored = Stored(
        commit.generation,
        commit.epoch,
        commit.index,
        commit.start,
        commit.end,
    );
    match commit.kind {
        Kind::Event => crate::events::valid_stored_event(body, stored).is_some(),
        Kind::Exit => crate::events::valid_stored_lifecycle(body, stored).is_some(),
        Kind::Log => commit.epoch != 0 && body.len() as u64 == commit.end - commit.start,
    }
}

fn four(mut open: impl FnMut(usize) -> Result<File>) -> Result<Slots> {
    Ok([open(0)?, open(1)?, open(2)?, open(3)?])
}

fn try_lease(file: &File) -> io::Result<()> {
    file.try_lock_exclusive()
}

fn open_slots(path: &Path, write: bool) -> Result<Slots> {
    let slots = four(|at| {
        open_slot(&path.join(NAMES[at]), write, write && at == 2).map_err(|_| StoreError::Corrupt)
    })?;
    validate_slots(path, &slots, write)?;
    Ok(slots)
}

fn validate_slots(path: &Path, slots: &Slots, write: bool) -> Result<()> {
    let directory = fs::symlink_metadata(path)?;
    require(directory.is_dir() && protected(path, &directory, 0o700))?;
    let mut seen = 0u8;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let at = NAMES
            .iter()
            .position(|name| entry.file_name() == *name)
            .ok_or(StoreError::Corrupt)?;
        let meta = fs::symlink_metadata(entry.path())?;
        require(
            meta.is_file()
                && protected(&entry.path(), &meta, 0o600)
                && same_file(&meta, &slots[at].metadata()?),
        )?;
        seen |= 1 << at;
    }
    require(seen == 0b1111)?;
    if write {
        try_lease(&slots[2])?;
    }
    Ok(())
}

fn open_slot(path: &Path, write: bool, _lease: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    options.open(path).map_err(Into::into)
}

fn raw_descriptors(slots: &Slots) -> [std::os::fd::RawFd; 4] {
    use std::os::fd::AsRawFd;
    slots.each_ref().map(|slot| slot.as_raw_fd())
}

fn prepare_at(directory: &File) -> Result<Slots> {
    let meta = directory.metadata()?;
    require(meta.is_dir() && protected(Path::new(""), &meta, 0o700))?;
    let mut slots = Vec::with_capacity(4);
    for at in 0..4 {
        let slot = slot_at(directory, at, true).inspect_err(|_| remove_at(directory, &slots))?;
        slots.push(slot);
    }
    validate_at(directory, &slots).inspect_err(|_| remove_at(directory, &slots))?;
    Ok(slots.try_into().unwrap())
}

fn open_prepared(directory: &File, prepared: &Slots) -> Result<Slots> {
    let slots = four(|at| slot_at(directory, at, false))?;
    validate_at(directory, prepared)?;
    for at in 0..4 {
        require(same_file(&prepared[at].metadata()?, &slots[at].metadata()?))?;
    }
    try_lease(&slots[2])?;
    Ok(slots)
}

fn slot_at(directory: &File, at: usize, create: bool) -> Result<File> {
    let mut flags = OFlag::O_RDWR | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK;
    if create {
        flags |= OFlag::O_CREAT | OFlag::O_EXCL;
    }
    let file = crate::unix::open_file_at(
        directory,
        NAMES[at].as_ref(),
        flags,
        Mode::from_bits_retain(0o600),
    )?;
    if create && let Err(error) = fchmod(&file, Mode::from_bits_retain(0o600)) {
        remove_slot_at(directory, at, &file);
        return Err(io::Error::from(error).into());
    }
    Ok(file)
}

fn validate_at(directory_file: &File, slots: &[File]) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let mut seen = 0u8;
    crate::unix::directory_entries(directory_file, |name| -> Result<Option<()>> {
        let at = NAMES
            .iter()
            .position(|candidate| candidate.as_bytes() == name)
            .ok_or(StoreError::Corrupt)?;
        let entry = crate::unix::stat_at(directory_file, NAMES[at].as_ref())
            .map_err(|_| StoreError::Corrupt)?;
        let handle = slots[at].metadata()?;
        require(
            seen & (1 << at) == 0
                && entry.st_mode & libc::S_IFMT == libc::S_IFREG
                && entry.st_mode & 0o777 == 0o600
                && handle.is_file()
                && protected(Path::new(""), &handle, 0o600)
                && crate::unix::stat_identity(&entry) == (handle.dev(), handle.ino()),
        )?;
        seen |= 1 << at;
        Ok(None)
    })?;
    require(seen == 0b1111)
}

fn remove_slot_at(directory: &File, at: usize, slot: &File) {
    use std::os::unix::fs::MetadataExt;
    if let (Ok(opened), Ok(entry)) = (
        slot.metadata(),
        crate::unix::stat_at(directory, NAMES[at].as_ref()),
    ) && entry.st_mode & libc::S_IFMT == libc::S_IFREG
        && crate::unix::stat_identity(&entry) == (opened.dev(), opened.ino())
    {
        let _ = crate::unix::unlink_at(directory, NAMES[at].as_ref());
    }
}

fn remove_at(directory: &File, slots: &[File]) {
    for (at, slot) in slots.iter().enumerate().rev() {
        remove_slot_at(directory, at, slot);
    }
}

type Candidate = (Commit, Sha256, Vec<u8>);

fn recover(slots: &[File; 4], kind: Kind, generation: Option<u32>) -> Result<Candidate> {
    select_candidates(
        read_commit(slots, 0, kind, generation),
        read_commit(slots, 1, kind, generation),
    )
}

fn select_candidates(left: Option<Candidate>, right: Option<Candidate>) -> Result<Candidate> {
    match (left, right) {
        (Some(a), Some(b)) if a.0.index == b.0.index || a.0.generation != b.0.generation => {
            Err(StoreError::Corrupt)
        }
        (Some(a), Some(b)) => Ok(if a.0.index > b.0.index { a } else { b }),
        (Some(candidate), None) | (None, Some(candidate)) => Ok(candidate),
        (None, None) => Err(StoreError::Corrupt),
    }
}

fn read_commit(
    slots: &[File; 4],
    slot: u8,
    kind: Kind,
    generation: Option<u32>,
) -> Option<Candidate> {
    let file = &slots[2 + slot as usize];
    (file.metadata().ok()?.len() == 92).then_some(())?;
    let record = read_range(file, 0, 92).ok()?;
    let commit = Commit::decode(&record, slot, kind, generation)?;
    let body = read_range(&slots[commit.body as usize], 0, commit.length).ok()?;
    let hash = Sha256::new().chain_update(&body);
    (hash.clone().finalize().as_slice() == commit.hash && body_valid(&commit, &body))
        .then_some((commit, hash, body))
}

fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, bytes, offset)
}
fn write_at(file: &File, bytes: &[u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(file, bytes, offset)
}

fn read_range(file: &File, offset: u64, length: u64) -> Result<Vec<u8>> {
    let size = usize::try_from(length).map_err(|_| StoreError::Corrupt)?;
    let mut out = Vec::new();
    out.try_reserve_exact(size)
        .map_err(|_| StoreError::Corrupt)?;
    out.resize(size, 0);
    let (mut rest, mut at) = (out.as_mut_slice(), offset);
    while !rest.is_empty() {
        let count = read_at(file, rest, at)?;
        if count == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
        (rest, at) = (&mut rest[count..], at + count as u64);
    }
    Ok(out)
}

fn write_bytes(
    file: &File,
    mut offset: u64,
    mut bytes: &[u8],
    mut before: impl FnMut() -> Result<()>,
) -> Result<()> {
    while !bytes.is_empty() {
        before()?;
        let count = write_at(file, bytes, offset)?;
        if count == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero).into());
        }
        (bytes, offset) = (&bytes[count..], offset + count as u64);
    }
    Ok(())
}

fn durable(file: &File, offset: u64, bytes: &[u8]) -> Result<()> {
    file.set_len(offset)?;
    write_bytes(file, offset, bytes, || Ok(()))?;
    file.sync_all().map_err(Into::into)
}

fn rewrite(
    file: &File,
    offset: u64,
    bytes: &[u8],
    truncate_first: bool,
    gate: &mut impl FnMut(StoreStep) -> Result<()>,
    step: StoreStep,
) -> Result<()> {
    if truncate_first {
        gate(step)?;
        file.set_len(offset)?;
    }
    let end = offset
        .checked_add(bytes.len() as u64)
        .ok_or(StoreError::Exhausted)?;
    write_bytes(file, offset, bytes, || gate(step))?;
    if !truncate_first {
        gate(step)?;
        file.set_len(end)?;
    }
    gate(match step {
        StoreStep::Commit => StoreStep::Flush,
        step => step,
    })?;
    file.sync_all().map_err(Into::into)
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn protected(_: &Path, meta: &fs::Metadata, mode: u32) -> bool {
    crate::unix::protected(meta, mode)
        && (mode == 0o700 || std::os::unix::fs::MetadataExt::nlink(meta) == 1)
}
fn same_file(path: &fs::Metadata, handle: &fs::Metadata) -> bool {
    crate::unix::file_id(path) == crate::unix::file_id(handle)
}

pub(crate) fn private_directory(path: &Path, create: bool) -> io::Result<bool> {
    let created = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            let result = create_directory(path);
            if let Err(error) = result
                && error.kind() != io::ErrorKind::AlreadyExists
            {
                return Err(error);
            }
            Some(fs::symlink_metadata(path)?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
        Ok(_) => None,
    };
    let meta = fs::symlink_metadata(path)?;
    Ok(meta.is_dir()
        && protected(path, &meta, 0o700)
        && created.as_ref().is_none_or(|made| same_file(made, &meta)))
}

fn create_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    crate::unix::with_umask(0o077, || fs::DirBuilder::new().mode(0o700).create(path))?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        sync_dir(parent)?;
    }
    Ok(())
}

fn validate_event_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    require(meta.is_dir() && protected(path, &meta, 0o700) && fs::read_dir(path)?.next().is_none())
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
include!("../tests/unit/store.rs");
