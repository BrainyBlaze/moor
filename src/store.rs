use crate::{canonical_u64 as decimal, wire::crc32c};
use fs2::FileExt as _;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const LIMITS: [u64; 4] = [0, 320 << 10, u64::MAX, 4 << 20];
const NAMES: [&str; 4] = ["body.0", "body.1", "commit.0", "commit.1"];
const EVENT_CAP: u64 = 256 << 10;
const EVENT_END: u64 = 1 << 53;
const SNAPSHOT: u8 = 1;
const EXHAUSTED: u8 = 2;
const RETAINED: u8 = 4;
const SEQUENCE_END: u8 = 8;
const EPOCH_END: u8 = 16;
const COMMIT_END: u8 = 32;
const EVENT_HEADER: &str =
    "v:2,type:=header,ts:*,session:*,generation:*,epoch:u,next_seq:*,first_retained:*";
const LIFECYCLE_COMMON: &str = "v:1,type:=lifecycle,phase:t,session:*,generation:*,wire_generation:u,incarnation:b16,start_wall_ms:D,start_mono_ms:D,boot_id:b16,path_encoding:=posix-bytes/windows-wtf8,event_path:n,instrument_path:n";
const LIFECYCLE_END: &str = "|end_wall_ms:D,output_end:D,ended:=exited,code:u|end_wall_ms:D,output_end:D,ended:=signalled,signal:p|end_wall_ms:D,output_end:D,ended:=terminated,code:u,method:=graceful/forced";

fn corrupt_if(condition: bool) -> Result<(), StoreError> {
    condition.then_some(StoreError::Corrupt).map_or(Ok(()), Err)
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    Event = 1,
    Log = 2,
    Exit = 3,
}

schema!(enum pub StoreError [Debug]; Io(std::io::Error), Corrupt, Exhausted);
impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

schema!(struct pub Commit derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; slot: u8, body: u8, kind: Kind, generation: u32, epoch: u32,
    index: u64, length: u64, start: u64, end: u64, hash: [u8; 32]);

impl Commit {
    pub fn encode(&self) -> [u8; 92] {
        let mut out = [0; 92];
        out[..12].copy_from_slice(b"MOORCMT1\x01\0\0\0");
        out[9..12].copy_from_slice(&[self.slot, self.body, self.kind as u8]);
        out[12..16].copy_from_slice(&self.generation.to_le_bytes());
        out[16..20].copy_from_slice(&self.epoch.to_le_bytes());
        for (at, value) in [
            (24, self.index),
            (32, self.length),
            (40, self.start),
            (48, self.end),
        ] {
            out[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
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
) -> Result<(Commit, Sha256), StoreError> {
    let (slot, body, epoch, index, start, end) = meta;
    let hash = Sha256::new().chain_update(bytes);
    let commit = Commit {
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
    corrupt_if(commit.length > LIMITS[kind as usize])?;
    corrupt_if(
        slot > 1
            || body > 1
            || generation == 0
            || index == 0
            || start > end
            || !body_ok(&commit, bytes),
    )?;
    Ok((commit, hash))
}

schema!(struct pub Store fields; slots: [File; 4], selected: Commit, hash: Sha256);

impl Store {
    pub fn remove(path: &Path) -> Result<(), StoreError> {
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
    ) -> Result<Self, StoreError> {
        let epoch = if kind == Kind::Event { 0 } else { 1 };
        let (commit, hash) = make_commit(kind, generation, (0, 0, epoch, 1, start, end), initial)?;
        if kind == Kind::Event && path.exists() {
            let meta = fs::symlink_metadata(path)?;
            corrupt_if(
                !meta.is_dir()
                    || !protected(path, &meta, 0o700)
                    || fs::read_dir(path)?.next().is_some(),
            )?;
        } else {
            create_directory(path)?;
        }
        for name in NAMES {
            create_slot(&path.join(name))?;
        }
        sync_dir(path)?;
        let slots = open_slots(path, true)?;
        update(&slots[0], 0, initial)?;
        update(&slots[2], 0, &commit.encode())?;
        Ok(Self {
            slots,
            selected: commit,
            hash,
        })
    }
    pub fn open(
        path: &Path,
        kind: Kind,
        generation: impl Into<Option<u32>>,
    ) -> Result<Self, StoreError> {
        let slots = open_slots(path, true)?;
        let (selected, hash, _) = recover(&slots, kind, generation.into())?;
        Ok(Self {
            slots,
            selected,
            hash,
        })
    }
    pub fn read_only(
        path: &Path,
        kind: Kind,
        generation: impl Into<Option<u32>>,
    ) -> Result<(Commit, Vec<u8>), StoreError> {
        let slots = open_slots(path, false)?;
        let (commit, _, body) = recover(&slots, kind, generation.into())?;
        Ok((commit, body))
    }
    pub fn selected(&self) -> &Commit {
        &self.selected
    }
    pub fn append_capped(
        &mut self,
        bytes: &[u8],
        cap: u64,
        end: u64,
    ) -> Result<&Commit, StoreError> {
        let prior = self.selected;
        let added = bytes.len() as u64;
        corrupt_if(prior.kind != Kind::Log || prior.end.checked_add(added) != Some(end))?;
        let length = prior
            .length
            .checked_add(added)
            .ok_or(StoreError::Exhausted)?;
        if length <= cap {
            let index = prior.index.checked_add(1).ok_or(StoreError::Exhausted)?;
            update(&self.slots[prior.body as usize], prior.length, bytes)?;
            let hash = self.hash.clone().chain_update(bytes);
            let commit = Commit {
                slot: 1 - prior.slot,
                index,
                length,
                end,
                hash: hash.clone().finalize().into(),
                ..prior
            };
            return self.install(commit, hash);
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
        self.replace(&retained, epoch, end - keep, end)
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
        let expected_epoch = match prior.kind {
            Kind::Event if epoch == prior.epoch => epoch,
            Kind::Event | Kind::Log => prior.epoch.checked_add(1).ok_or(StoreError::Exhausted)?,
            Kind::Exit if index == 2 => 1,
            Kind::Exit => return Err(StoreError::Exhausted),
        };
        corrupt_if(epoch != expected_epoch)?;
        let meta = (slot, body, epoch, index, start, end);
        let (commit, hash) = make_commit(prior.kind, prior.generation, meta, bytes)?;
        update(&self.slots[body as usize], 0, bytes)?;
        self.install(commit, hash)
    }
    fn write_commit(&self, commit: &Commit) -> Result<(), StoreError> {
        update(&self.slots[2 + commit.slot as usize], 0, &commit.encode())
    }

    fn install(&mut self, commit: Commit, hash: Sha256) -> Result<&Commit, StoreError> {
        self.write_commit(&commit)?;
        self.selected = commit;
        self.hash = hash;
        Ok(&self.selected)
    }
}

fn body_ok(commit: &Commit, body: &[u8]) -> bool {
    return_if!(
        commit.kind == Kind::Log,
        commit.epoch != 0 && body.len() as u64 == commit.end - commit.start
    );
    let Some(body) = body.strip_suffix(b"\n") else {
        return false;
    };
    let mut lines = body.split(|byte| *byte == b'\n');
    let first = lines.next().unwrap();
    match commit.kind {
        Kind::Event => event_body(commit, first, lines).is_some(),
        Kind::Exit => {
            commit.epoch == 1
                && commit.index <= 2
                && commit.start == commit.end
                && lifecycle_line(first, commit)
                && lines.next().is_none()
        }
        Kind::Log => unreachable!(),
    }
}

fn event_body<'a>(
    commit: &Commit,
    header: &[u8],
    lines: impl Iterator<Item = &'a [u8]>,
) -> Option<()> {
    let (epoch, first, next) = event_header(header, commit.generation)?;
    let (mut sequence, mut transitions, mut last, mut retained) = (first, 0u64, 0, false);
    for line in lines {
        (last & EXHAUSTED == 0).then_some(())?;
        let flags = event_line(line, epoch, sequence)?;
        if flags & SNAPSHOT != 0 {
            (transitions == 0).then_some(())?;
        } else {
            transitions += 1;
            last = flags;
            retained |= flags & RETAINED != 0;
        }
        sequence = sequence.checked_add(1)?;
    }
    let overage = last & EXHAUSTED != 0 || epoch != 0 && !retained && transitions == 1;
    let frontier = match last & (SEQUENCE_END | EPOCH_END | COMMIT_END) {
        SEQUENCE_END => next <= EVENT_END,
        EPOCH_END => epoch == u32::MAX && next < EVENT_END,
        COMMIT_END => commit.index == u64::MAX && next < EVENT_END,
        0 => next < EVENT_END,
        _ => false,
    };
    ((epoch, first, next, sequence) == (commit.epoch, commit.start, commit.end, next)
        && (commit.length <= EVENT_CAP || overage)
        && frontier)
        .then_some(())
}

fn event_header(line: &[u8], generation: u32) -> Option<(u32, u64, u64)> {
    let fields = crate::events::canonical_object(line, 16)?;
    (fields_match(&fields, 0..fields.len(), EVENT_HEADER)
        && valid_generation(&fields["generation"], generation))
    .then_some(())?;
    let session = base64(fields["session"].as_str()?)?;
    (session.starts_with(&[1, b'/']) || session.len() == 25 && session.first() == Some(&2))
        .then_some(())?;
    let epoch = u32::try_from(fields["epoch"].as_u64()?).ok()?;
    let next = fields["next_seq"].as_u64()?;
    let first = fields["first_retained"].as_u64()?;
    (first <= next && next <= EVENT_END).then_some((epoch, first, next))
}

fn event_line(line: &[u8], epoch: u32, sequence: u64) -> Option<u8> {
    let values = crate::events::canonical_object(line, 32)?;
    let base = ["type", "ts", "epoch", "seq", "kind"];
    let kind = values.get("type")?.as_str()?;
    let schema = crate::events::schema(kind)?;
    let record_kind = values.get("kind")?.as_str()?;
    matches!(record_kind, "transition" | "snapshot").then_some(())?;
    let snapshot = record_kind == "snapshot";
    let assertion = values
        .get("assertion_kind")
        .and_then(serde_json::Value::as_str)
        == Some("snapshot");
    let shape = values.keys().take(5).map(String::as_str).eq(base)
        && fields_match(&values, 5..values.len(), schema)
        && values["epoch"].as_u64() == Some(u64::from(epoch))
        && sequence < EVENT_END
        && values["seq"].as_u64() == Some(sequence)
        && (!snapshot
            || matches!(
                kind,
                "ready" | "state" | "link" | "semantic-source" | "semantic-assertion"
            ))
        && (!snapshot || kind != "semantic-assertion" || assertion)
        && (kind != "semantic-source"
            || matches!(
                (values["status"].as_str(), values["reason"].as_str()),
                (Some("connected" | "exact"), Some(""))
                    | (Some("degraded"), Some("heartbeat-timeout"))
                    | (
                        Some("disconnected"),
                        Some("transport-closed" | "superseded" | "session-ending")
                    )
            ));
    shape.then(|| {
        u8::from(snapshot)
            | (u8::from(kind == "stream-exhausted") * EXHAUSTED)
            | (u8::from(kind == "semantic-source" || kind == "semantic-assertion" && assertion)
                * RETAINED)
            | (u8::from(kind == "stream-exhausted" && values["axis"] == "seq") * SEQUENCE_END)
            | (u8::from(kind == "stream-exhausted" && values["axis"] == "epoch") * EPOCH_END)
            | (u8::from(kind == "stream-exhausted" && values["axis"] == "commit") * COMMIT_END)
    })
}

fn fields_match(values: &Map<String, Value>, range: std::ops::Range<usize>, schema: &str) -> bool {
    schema.split('|').any(|choice| {
        let mut fields = choice.split(',').filter(|field| !field.is_empty());
        values
            .iter()
            .skip(range.start)
            .take(range.len())
            .all(|(key, value)| {
                fields
                    .next()
                    .and_then(|field| field.split_once(':'))
                    .is_some_and(|(name, rule)| name == key && field_value(rule, value))
            })
            && fields.next().is_none()
    })
}

fn field_value(rule: &str, value: &Value) -> bool {
    let (text, number) = (value.as_str(), value.as_u64());
    if let Some(choices) = rule.strip_prefix('=') {
        return text.is_some_and(|text| choices.split('/').any(|choice| choice == text));
    }
    let decoded = || text.and_then(base64);
    match rule {
        "t" => text.is_some(),
        "*" => true,
        "1" | "2" => number == Some(u64::from(rule.as_bytes()[0] - b'0')),
        "?" => value.is_boolean(),
        "u" => number.is_some_and(|number| number <= u64::from(u32::MAX)),
        "p" => number.is_some_and(|number| (1..=u64::from(u32::MAX)).contains(&number)),
        "d" => text.and_then(decimal).is_some_and(|number| number != 0),
        "s" => text.is_some_and(|text| crate::session::valid_source_id(text.as_bytes())),
        "b16" => decoded().is_some_and(|bytes| bytes.len() == 16),
        "b4096" => decoded().is_some_and(|bytes| bytes.len() <= 4096),
        "D" => text.and_then(decimal).is_some(),
        "n" => value.is_null() || decoded().is_some_and(|bytes| !bytes.is_empty()),
        "j" => decoded().is_some_and(|bytes| {
            bytes.len() <= 32768 && crate::events::json_object(&bytes, 64, 1024).is_some()
        }),
        _ => rule
            .strip_prefix('t')
            .and_then(|cap| cap.parse().ok())
            .is_some_and(|cap| text.is_some_and(|text| text.len() <= cap)),
    }
}

fn base64(text: &str) -> Option<Vec<u8>> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    STANDARD.decode(text).ok()
}

fn lifecycle_line(line: &[u8], commit: &Commit) -> bool {
    let Some(values) = crate::events::canonical_object(line, 20) else {
        return false;
    };
    let text = |key| values.get(key).and_then(Value::as_str);
    let number = |key| text(key).and_then(decimal);
    let encoding = text("path_encoding");
    let session = text("session")
        .and_then(base64)
        .is_some_and(|bytes| match encoding {
            Some("posix-bytes") => bytes.starts_with(&[1, b'/']),
            Some("windows-wtf8") => bytes.len() == 25 && bytes.first() == Some(&2),
            _ => false,
        });
    let common = fields_match(&values, 0..13, LIFECYCLE_COMMON)
        && session
        && values
            .get("generation")
            .is_some_and(|value| valid_generation(value, commit.generation))
        && values.get("wire_generation").and_then(Value::as_u64)
            == Some(u64::from(commit.generation))
        && fields_match(&values, 13..values.len(), LIFECYCLE_END);
    let windows = encoding == Some("windows-wtf8");
    let closed = commit.index == 2 && number("output_end") == Some(commit.end);
    common
        && match (text("phase"), text("ended")) {
            (Some("running"), None) => commit.index == 1 && commit.start == 0 && commit.end == 0,
            (Some("exited"), Some("exited")) => {
                closed
                    && values["code"]
                        .as_u64()
                        .is_some_and(|code| windows || code <= 255)
            }
            (Some("exited"), Some("signalled")) => closed && !windows,
            (Some("exited"), Some("terminated")) => closed && windows,
            _ => false,
        }
}

fn valid_generation(value: &Value, generation: u32) -> bool {
    generation == 1 && value.is_null()
        || generation != 1 && value.as_u64() == Some(u64::from(generation))
}

fn open_slots(path: &Path, write: bool) -> Result<[File; 4], StoreError> {
    let open =
        |at, lease| open_slot(&path.join(NAMES[at]), write, lease).map_err(|_| StoreError::Corrupt);
    let slots = [
        open(0, false)?,
        open(1, false)?,
        open(2, write)?,
        open(3, false)?,
    ];
    let directory = fs::symlink_metadata(path)?;
    corrupt_if(!directory.is_dir() || !protected(path, &directory, 0o700))?;
    let mut seen = 0u8;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let at = NAMES
            .iter()
            .position(|name| entry.file_name() == *name)
            .ok_or(StoreError::Corrupt)?;
        let slot = entry.path();
        let meta = fs::symlink_metadata(&slot)?;
        corrupt_if(
            !meta.is_file()
                || !protected(&slot, &meta, 0o600)
                || !same_file(&meta, &slots[at].metadata()?),
        )?;
        seen |= 1 << at;
    }
    corrupt_if(seen != 0b1111)?;
    #[cfg(windows)]
    corrupt_if(!crate::windows::valid_store_slots(path, &slots))?;
    if write {
        slots[2].try_lock_exclusive()?;
    }
    Ok(slots)
}

fn recover(
    slots: &[File; 4],
    kind: Kind,
    generation: Option<u32>,
) -> Result<(Commit, Sha256, Vec<u8>), StoreError> {
    match (
        read_commit(slots, 0, kind, generation)?,
        read_commit(slots, 1, kind, generation)?,
    ) {
        (Some(a), Some(b)) if a.0.index == b.0.index || a.0.generation != b.0.generation => {
            Err(StoreError::Corrupt)
        }
        (Some(a), Some(b)) => Ok(if a.0.index > b.0.index { a } else { b }),
        (Some(commit), None) | (None, Some(commit)) => Ok(commit),
        (None, None) => Err(StoreError::Corrupt),
    }
}

#[cfg(unix)]
fn protected(_: &Path, meta: &fs::Metadata, mode: u32) -> bool {
    crate::unix::protected(meta, mode)
        && (mode == 0o700 || std::os::unix::fs::MetadataExt::nlink(meta) == 1)
}
#[cfg(windows)]
fn protected(path: &Path, _: &fs::Metadata, mode: u32) -> bool {
    crate::windows::protected_store_path(path, mode == 0o700)
}
pub(crate) fn private_directory(path: &Path, create: bool) -> io::Result<bool> {
    let created = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            let result = create_directory(path);
            #[cfg(unix)]
            result?;
            #[cfg(windows)]
            if let Err(error) = result
                && fs::symlink_metadata(path).is_err()
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
    Ok(meta.file_type().is_dir()
        && protected(path, &meta, 0o700)
        && created.as_ref().is_none_or(|made| same_file(made, &meta)))
}
#[cfg(unix)]
fn same_file(path: &fs::Metadata, handle: &fs::Metadata) -> bool {
    crate::unix::file_id(path) == crate::unix::file_id(handle)
}
#[cfg(windows)]
fn same_file(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    true
}
fn open_slot(path: &Path, write: bool, _lease: bool) -> Result<File, StoreError> {
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
        options.share_mode(if _lease { 3 } else { 7 });
    }
    Ok(options.open(path)?)
}

fn read_commit(
    slots: &[File; 4],
    slot: u8,
    kind: Kind,
    generation: Option<u32>,
) -> Result<Option<(Commit, Sha256, Vec<u8>)>, StoreError> {
    let file = &slots[2 + slot as usize];
    return_if!(file.metadata()?.len() != 92, Ok(None));
    let bytes = read_range(file, 0, 92)?;
    let actual_generation = u32_at(&bytes, 12);
    return_if!(
        &bytes[..9] != b"MOORCMT1\x01"
            || bytes[9] != slot
            || bytes[10] > 1
            || bytes[11] != kind as u8
            || bytes[20..24] != [0; 4]
            || actual_generation == 0
            || generation.is_some_and(|expected| actual_generation != expected)
            || u32_at(&bytes, 88) != crc32c(&bytes[..88]),
        Ok(None)
    );
    let commit = Commit {
        slot,
        body: bytes[10],
        kind,
        generation: actual_generation,
        epoch: u32_at(&bytes, 16),
        index: u64_at(&bytes, 24),
        length: u64_at(&bytes, 32),
        start: u64_at(&bytes, 40),
        end: u64_at(&bytes, 48),
        hash: bytes[56..88].try_into().unwrap(),
    };
    return_if!(
        commit.index == 0 || commit.start > commit.end || commit.length > LIMITS[kind as usize],
        Ok(None)
    );
    let Some(body) = read_range(&slots[commit.body as usize], 0, commit.length).ok() else {
        return Ok(None);
    };
    let hash = Sha256::new().chain_update(&body);
    return_if!(
        hash.clone().finalize().as_slice() != commit.hash || !body_ok(&commit, &body),
        Ok(None)
    );
    Ok(Some((commit, hash, body)))
}
fn read_range(file: &File, offset: u64, length: u64) -> Result<Vec<u8>, StoreError> {
    let size: usize = length.try_into().map_err(|_| StoreError::Corrupt)?;
    let mut out = Vec::new();
    out.try_reserve_exact(size)
        .map_err(|_| StoreError::Corrupt)?;
    out.resize(size, 0);
    let mut file = file;
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut out)?;
    Ok(out)
}
fn update(file: &File, offset: u64, bytes: &[u8]) -> Result<(), StoreError> {
    let mut file = file;
    file.set_len(offset)?;
    file.seek(SeekFrom::Start(offset))?;
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
fn create_directory(path: &Path) -> io::Result<()> {
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
    crate::windows::create_store_path(path, true)?;
    Ok(())
}
fn create_slot(path: &Path) -> Result<(), StoreError> {
    #[cfg(windows)]
    return crate::windows::create_store_path(path, false).map_err(Into::into);
    #[cfg(unix)]
    {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600);
        let file = options.open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.sync_all()?;
        Ok(())
    }
}

fn sync_dir(path: &Path) -> io::Result<()> {
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
