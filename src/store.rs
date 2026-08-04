use crate::wire::crc32c;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const EVENT_LIMIT: u64 = 4 << 20;
const LIFECYCLE_LIMIT: u64 = 4 << 20;
const NAMES: [&str; 4] = ["body.0", "body.1", "commit.0", "commit.1"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    Event = 1,
    Log = 2,
    Exit = 3,
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Corrupt,
    Exhausted,
}
impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Commit {
    pub slot: u8,
    pub body: u8,
    pub kind: Kind,
    pub generation: u32,
    pub epoch: u32,
    pub index: u64,
    pub length: u64,
    pub start: u64,
    pub end: u64,
    pub hash: [u8; 32],
}

impl Commit {
    pub fn encode(&self) -> [u8; 92] {
        let mut out = [0; 92];
        out[..8].copy_from_slice(b"MOORCMT1");
        out[8] = 1;
        out[9] = self.slot;
        out[10] = self.body;
        out[11] = self.kind as u8;
        out[12..16].copy_from_slice(&self.generation.to_le_bytes());
        out[16..20].copy_from_slice(&self.epoch.to_le_bytes());
        out[24..32].copy_from_slice(&self.index.to_le_bytes());
        out[32..40].copy_from_slice(&self.length.to_le_bytes());
        out[40..48].copy_from_slice(&self.start.to_le_bytes());
        out[48..56].copy_from_slice(&self.end.to_le_bytes());
        out[56..88].copy_from_slice(&self.hash);
        let checksum = crc32c(&out[..88]);
        out[88..].copy_from_slice(&checksum.to_le_bytes());
        out
    }
}

fn make_commit(
    kind: Kind,
    generation: u32,
    meta: (u8, u8, u32, u64, u64, u64),
    bytes: &[u8],
) -> Result<Commit, StoreError> {
    let (slot, body, epoch, index, start, end) = meta;
    if slot > 1
        || body > 1
        || generation == 0
        || index == 0
        || start > end
        || !valid_body(kind, epoch, index, start, end, bytes)
    {
        return Err(StoreError::Corrupt);
    }
    Ok(Commit {
        slot,
        body,
        kind,
        generation,
        epoch,
        index,
        length: bytes.len() as u64,
        start,
        end,
        hash: Sha256::digest(bytes).into(),
    })
}

pub struct Store {
    path: PathBuf,
    kind: Kind,
    generation: u32,
    selected: Commit,
    _lease: File,
}

impl Store {
    pub fn create(
        path: &Path,
        kind: Kind,
        generation: u32,
        initial: &[u8],
        start: u64,
        end: u64,
    ) -> Result<Self, StoreError> {
        let epoch = if kind == Kind::Event { 0 } else { 1 };
        let commit = make_commit(kind, generation, (0, 0, epoch, 1, start, end), initial)?;
        create_directory(path)?;
        for name in NAMES {
            create_slot(&path.join(name))?;
        }
        sync_dir(path)?;
        let lease = lock_writer(&path.join("commit.0"))?;
        write_file(&path.join("body.0"), initial)?;
        write_file(&path.join("commit.0"), &commit.encode())?;
        Ok(Self {
            path: path.into(),
            kind,
            generation,
            selected: commit,
            _lease: lease,
        })
    }
    pub fn open(path: &Path, kind: Kind, generation: u32) -> Result<Self, StoreError> {
        validate_slots(path)?;
        let lease = lock_writer(&path.join("commit.0"))?;
        let selected = recover(path, kind, generation)?;
        Ok(Self {
            path: path.into(),
            kind,
            generation,
            selected,
            _lease: lease,
        })
    }
    pub fn read_only(
        path: &Path,
        kind: Kind,
        generation: u32,
    ) -> Result<(Commit, Vec<u8>), StoreError> {
        validate_slots(path)?;
        let commit = recover(path, kind, generation)?;
        let body = read_prefix(&path.join(format!("body.{}", commit.body)), commit.length)?;
        Ok((commit, body))
    }
    pub fn selected(&self) -> Option<&Commit> {
        Some(&self.selected)
    }
    pub fn read(&self) -> Result<Vec<u8>, StoreError> {
        read_prefix(
            &self.path.join(format!("body.{}", self.selected.body)),
            self.selected.length,
        )
    }
    pub fn append(&mut self, bytes: &[u8], end: u64) -> Result<&Commit, StoreError> {
        if self.kind == Kind::Exit {
            return Err(StoreError::Corrupt);
        }
        let prior = self.selected.clone();
        let index = prior.index.checked_add(1).ok_or(StoreError::Exhausted)?;
        if self.kind == Kind::Log && prior.end.checked_add(bytes.len() as u64) != Some(end) {
            return Err(StoreError::Corrupt);
        }
        let path = self.path.join(format!("body.{}", prior.body));
        let mut file = open_slot(&path, true)?;
        file.set_len(prior.length)?;
        file.seek(SeekFrom::Start(prior.length))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        let length = prior
            .length
            .checked_add(bytes.len() as u64)
            .ok_or(StoreError::Exhausted)?;
        let body = read_prefix(&path, length)?;
        let meta = (
            1 - prior.slot,
            prior.body,
            prior.epoch,
            index,
            prior.start,
            end,
        );
        let commit = make_commit(self.kind, self.generation, meta, &body)?;
        self.write_commit(&commit)?;
        self.selected = commit;
        Ok(&self.selected)
    }
    pub fn replace(
        &mut self,
        bytes: &[u8],
        epoch: u32,
        start: u64,
        end: u64,
    ) -> Result<&Commit, StoreError> {
        let prior = &self.selected;
        let (slot, body) = (1 - prior.slot, 1 - prior.body);
        let index = prior.index.checked_add(1).ok_or(StoreError::Exhausted)?;
        let expected_epoch = match self.kind {
            Kind::Event | Kind::Log => prior.epoch.checked_add(1).ok_or(StoreError::Exhausted)?,
            Kind::Exit if index == 2 => 1,
            Kind::Exit => return Err(StoreError::Exhausted),
        };
        if epoch != expected_epoch {
            return Err(StoreError::Corrupt);
        }
        write_file(&self.path.join(format!("body.{body}")), bytes)?;
        let commit = make_commit(
            self.kind,
            self.generation,
            (slot, body, epoch, index, start, end),
            bytes,
        )?;
        self.write_commit(&commit)?;
        self.selected = commit;
        Ok(&self.selected)
    }
    fn write_commit(&self, commit: &Commit) -> Result<(), StoreError> {
        write_file(
            &self.path.join(format!("commit.{}", commit.slot)),
            &commit.encode(),
        )
    }
}

fn valid_body(kind: Kind, epoch: u32, index: u64, start: u64, end: u64, body: &[u8]) -> bool {
    if kind == Kind::Log {
        return epoch != 0 && body.len() as u64 == end - start;
    }
    let Some(body) = body.strip_suffix(b"\n") else {
        return false;
    };
    let mut lines = body.split(|byte| *byte == b'\n');
    let first = lines.next().unwrap();
    let object = |line: &[u8]| line.starts_with(b"{") && line.ends_with(b"}");
    let needle: &[u8] = match (kind, index) {
        (Kind::Event, _) => b"\"type\":\"header\"",
        (Kind::Exit, 1) => b"\"phase\":\"running\"",
        (Kind::Exit, _) => b"\"phase\":\"exited\"",
        _ => unreachable!(),
    };
    let contains = first.windows(needle.len()).any(|part| part == needle);
    object(first)
        && contains
        && match kind {
            Kind::Event => (body.len() as u64) < EVENT_LIMIT && lines.all(object),
            Kind::Exit => {
                epoch == 1
                    && index <= 2
                    && start == end
                    && (index != 1 || start == 0)
                    && (body.len() as u64) < LIFECYCLE_LIMIT
                    && lines.next().is_none()
            }
            Kind::Log => unreachable!(),
        }
}

fn validate_slots(path: &Path) -> Result<(), StoreError> {
    let mut names = fs::read_dir(path)?
        .map(|entry| Ok(entry?.file_name()))
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    names.sort();
    if names != NAMES.map(std::ffi::OsString::from) {
        return Err(StoreError::Corrupt);
    }
    let directory = fs::symlink_metadata(path)?;
    if !directory.is_dir() || !protected(&directory, 0o700) {
        return Err(StoreError::Corrupt);
    }
    for name in NAMES {
        let meta = fs::symlink_metadata(path.join(name))?;
        if !meta.is_file() || !protected(&meta, 0o600) {
            return Err(StoreError::Corrupt);
        }
    }
    Ok(())
}

fn recover(path: &Path, kind: Kind, generation: u32) -> Result<Commit, StoreError> {
    match (
        read_commit(path, 0, kind, generation)?,
        read_commit(path, 1, kind, generation)?,
    ) {
        (Some(a), Some(b)) if a.index == b.index => Err(StoreError::Corrupt),
        (Some(a), Some(b)) => Ok(if a.index > b.index { a } else { b }),
        (Some(commit), None) | (None, Some(commit)) => Ok(commit),
        (None, None) => Err(StoreError::Corrupt),
    }
}

#[cfg(unix)]
fn protected(meta: &fs::Metadata, mode: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o777 == mode
}
#[cfg(windows)]
fn protected(_: &fs::Metadata, _: u32) -> bool {
    true
}

fn open_slot(path: &Path, write: bool) -> Result<File, StoreError> {
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    Ok(options.open(path)?)
}

#[cfg(unix)]
fn lock_writer(path: &Path) -> Result<File, StoreError> {
    use std::os::fd::AsRawFd;
    let file = open_slot(path, true)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(file)
}
#[cfg(windows)]
fn lock_writer(path: &Path) -> Result<File, StoreError> {
    use std::ffi::c_void;
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    #[repr(C)]
    struct Overlapped(usize, usize, u32, u32, *mut c_void);
    type Handle = *mut c_void;
    unsafe extern "system" {
        fn LockFileEx(_: Handle, _: u32, _: u32, _: u32, _: u32, _: *mut Overlapped) -> i32;
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .share_mode(3)
        .custom_flags(0x00200000);
    let file = options.open(path)?;
    let mut overlapped = Overlapped(0, 0, 0, 0, std::ptr::null_mut());
    if unsafe { LockFileEx(file.as_raw_handle(), 3, 0, 1, 0, &mut overlapped) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(file)
}

fn read_commit(
    path: &Path,
    slot: u8,
    kind: Kind,
    generation: u32,
) -> Result<Option<Commit>, StoreError> {
    let commit_path = path.join(format!("commit.{slot}"));
    let file = open_slot(&commit_path, false)?;
    if file.metadata()?.len() != 92 {
        return Ok(None);
    }
    let bytes = read_prefix(&commit_path, 92)?;
    if &bytes[..8] != b"MOORCMT1"
        || bytes[8] != 1
        || bytes[9] != slot
        || bytes[10] > 1
        || bytes[11] != kind as u8
        || bytes[20..24] != [0; 4]
        || u32_at(&bytes, 12) != generation
        || u32_at(&bytes, 88) != crc32c(&bytes[..88])
    {
        return Ok(None);
    }
    let commit = Commit {
        slot,
        body: bytes[10],
        kind,
        generation,
        epoch: u32_at(&bytes, 16),
        index: u64_at(&bytes, 24),
        length: u64_at(&bytes, 32),
        start: u64_at(&bytes, 40),
        end: u64_at(&bytes, 48),
        hash: bytes[56..88].try_into().unwrap(),
    };
    if commit.index == 0 || commit.start > commit.end {
        return Ok(None);
    }
    if (kind == Kind::Event && commit.length > EVENT_LIMIT)
        || (kind == Kind::Exit && commit.length > LIFECYCLE_LIMIT)
    {
        return Ok(None);
    }
    let body_path = path.join(format!("body.{}", commit.body));
    if hash_prefix(&body_path, commit.length).ok() != Some(commit.hash) {
        return Ok(None);
    }
    let valid = if kind == Kind::Log {
        commit.epoch != 0 && commit.length == commit.end - commit.start
    } else {
        read_prefix(&body_path, commit.length).is_ok_and(|body| {
            valid_body(
                kind,
                commit.epoch,
                commit.index,
                commit.start,
                commit.end,
                &body,
            )
        })
    };
    if !valid {
        return Ok(None);
    }
    Ok(Some(commit))
}
fn read_prefix(path: &Path, length: u64) -> Result<Vec<u8>, StoreError> {
    let size: usize = length.try_into().map_err(|_| StoreError::Corrupt)?;
    let mut out = Vec::new();
    out.try_reserve_exact(size)
        .map_err(|_| StoreError::Corrupt)?;
    open_slot(path, false)?.take(length).read_to_end(&mut out)?;
    if out.len() != size {
        return Err(StoreError::Corrupt);
    }
    Ok(out)
}
fn hash_prefix(path: &Path, length: u64) -> Result<[u8; 32], StoreError> {
    let mut file = open_slot(path, false)?.take(length);
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    let mut read = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        hash.update(&buf[..n]);
    }
    if read != length {
        return Err(StoreError::Corrupt);
    }
    Ok(hash.finalize().into())
}
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut file = open_slot(path, true)?;
    file.set_len(0)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn create_directory(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            sync_dir(parent)?;
        }
    }
    #[cfg(windows)]
    fs::create_dir(path)?;
    Ok(())
}
fn create_slot(path: &Path) -> Result<(), StoreError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?;
    Ok(())
}

fn sync_dir(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(0x02000000)
            .open(path)?
            .sync_all()?;
    }
    Ok(())
}
