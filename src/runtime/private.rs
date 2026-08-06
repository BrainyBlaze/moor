use crate::events::{self, Cursor, Event, EventStream, Json};
use crate::name;
use crate::runtime::holder::CoreConfig;
use crate::runtime::storage::EventConfig;
use crate::store::{Kind, Store};
use crate::wire::put_wide;
use base64::{Engine as _, display::Base64Display, engine::general_purpose::STANDARD};
use rayon::prelude::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

type Result<T> = std::result::Result<T, String>;
type Clock = (u64, [u8; 16]);
type Generation = (Option<u32>, u32);
type Start = (u64, u64, [u8; 16]);

fn text_error(value: impl ToString) -> String {
    value.to_string()
}

crate::schema!(struct pub SessionEntry pub fields; name: OsString, path: PathBuf, state: SessionState);
crate::schema!(struct pub ArtifactConfig<'a> pub fields; marker: &'a Path, event_path: Option<&'a Path>, encoding: &'a str,
    event_identity: Option<&'a [u8]>, instrument_identity: Option<&'a [u8]>, event_store: Option<Store>, stores: Option<ArtifactStores>, event_layout: u8, log_cap: u64);
crate::schema!(struct pub ArtifactStores pub fields; lifecycle: Store, event: Option<Store>, log: Option<Store>);
crate::schema!(struct pub PreparedStorage pub fields; log: Option<(Store, u64)>, events: Option<EventConfig>, lifecycle: Store);
crate::schema!(struct pub PreparedArtifacts pub fields; core: CoreConfig, storage: PreparedStorage, status: Vec<u8>, commit_at: usize, running: String);
crate::schema!(struct Lifecycle derive [Deserialize] fields; session: String, wire_generation: u32, incarnation: String,
    start_mono_ms: String, boot_id: String, event_path: Option<String>, instrument_path: Option<String>);

pub fn copy_digest(input: &mut fs::File, mut output: Option<&mut fs::File>) -> Result<[u8; 32]> {
    input.rewind().map_err(text_error)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 8192];
    loop {
        let count = input.read(&mut bytes).map_err(text_error)?;
        crate::return_if!(count == 0, Ok(hash.finalize().into()));
        hash.update(&bytes[..count]);
        if let Some(file) = &mut output {
            file.write_all(&bytes[..count]).map_err(text_error)?;
        }
    }
}

pub fn decode_launch_record(bytes: &[u8]) -> Option<u32> {
    let record: &[u8; 32] = bytes.try_into().ok()?;
    let generation = u32::from_le_bytes(record[12..16].try_into().unwrap());
    (&record[..9] == b"MOORLCH3\x01"
        && record[9..12] == [0; 3]
        && generation >= 2
        && record[16..] != [0; 16])
        .then_some(generation)
}

pub fn lowercase_hex(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub fn launch_result(state: u8, result: u16, generation: u32) -> Option<[u8; 12]> {
    valid_launch(state, result, generation).then_some(())?;
    let mut bytes = *b"MORR\x01\0\0\0\0\0\0\0";
    bytes[5] = state;
    bytes[6..8].copy_from_slice(&result.to_le_bytes());
    bytes[8..].copy_from_slice(&generation.to_le_bytes());
    Some(bytes)
}

pub struct LaunchReporter<W: Write> {
    pub output: Option<W>,
    pub generation: u32,
}

impl<W: Write> Default for LaunchReporter<W> {
    fn default() -> Self {
        Self {
            output: None,
            generation: 1,
        }
    }
}

impl<W: Write> LaunchReporter<W> {
    pub fn notice(&mut self, state: u8, result: u16) {
        if let Some(output) = self.output.as_mut() {
            let _ = output.write_all(&launch_result(state, result, self.generation).unwrap());
        }
        if state != 1 {
            self.output.take();
        }
    }
}

impl<W: Write> Drop for LaunchReporter<W> {
    fn drop(&mut self) {
        self.notice(3, 1);
    }
}

pub fn await_launch(mut input: impl Read) -> Result<(u16, u32)> {
    await_launch_probe(&mut input, |_| false, |_| {})
}

pub fn await_launch_probe(
    mut input: impl Read,
    published: impl FnOnce(u32) -> bool,
    adopted: impl FnOnce(u32),
) -> Result<(u16, u32)> {
    let mut next = || {
        let mut record = [0; 12];
        input
            .read_exact(&mut record)
            .map_err(|_| "holder failed before launch")?;
        decode_launch_result(&record).ok_or("holder returned an invalid launch result")
    };
    match next()? {
        (1, 0, generation) => {
            adopted(generation);
            match next() {
                Ok((2, 0, same)) if same == generation => Ok((0, generation)),
                Ok((3, result, same)) if same == generation && result != 0 => {
                    Ok((result, generation))
                }
                Err(_) if published(generation) => Ok((0, generation)),
                Ok(_) => Err("holder returned an invalid launch result".into()),
                Err(error) => Err(error.into()),
            }
        }
        (3, result, generation) => Ok((result, generation)),
        _ => Err("holder returned an invalid launch result".into()),
    }
}

pub fn fixed_record<const N: usize>(
    input: &mut impl Read,
    what: &str,
    invalid: &str,
    eof: bool,
    mut poll: impl FnMut(Duration) -> io::Result<Option<usize>>,
) -> Result<[u8; N]> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut record = [0; N];
    let mut extra = [0];
    let mut used = 0;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        crate::ensure!(!remaining.is_zero(), format!("{what} timed out"));
        match poll(remaining).map_err(text_error)? {
            Some(0) => thread::sleep(Duration::from_millis(2)),
            Some(available) => {
                let buffer = if used == N {
                    &mut extra[..]
                } else {
                    &mut record[used..]
                };
                let available = buffer.len().min(available);
                let count = input.read(&mut buffer[..available]).map_err(text_error)?;
                if count == 0 {
                    crate::ensure!(eof && used == N, invalid);
                    return Ok(record);
                }
                crate::ensure!(used < N, invalid);
                used += count;
                if used == N && !eof {
                    return Ok(record);
                }
            }
            None => {
                crate::ensure!(eof && used == N, invalid);
                return Ok(record);
            }
        }
    }
}

pub fn decode_launch_result(bytes: &[u8]) -> Option<(u8, u16, u32)> {
    let record: &[u8; 12] = bytes.try_into().ok()?;
    let decoded = (
        record[5],
        u16::from_le_bytes(record[6..8].try_into().unwrap()),
        u32::from_le_bytes(record[8..].try_into().unwrap()),
    );
    (&record[..5] == b"MORR\x01" && valid_launch(decoded.0, decoded.1, decoded.2))
        .then_some(decoded)
}

fn valid_launch(state: u8, result: u16, generation: u32) -> bool {
    generation != 0 && matches!((state, result), (1 | 2, 0) | (3, 1..=u16::MAX))
}

pub fn instrument_stage(
    root: &Path,
    identity: &[u8],
    generation: u32,
    incarnation: [u8; 16],
) -> Result<PathBuf> {
    let mut bound = Vec::with_capacity(identity.len() + 24);
    put_wide(&mut bound, identity).map_err(crate::protocol)?;
    bound.extend_from_slice(&generation.to_le_bytes());
    bound.extend_from_slice(&incarnation);
    Ok(root.join(format!("{:x}.instrument", Sha256::digest(bound))))
}

pub fn instrument_ack(generation: u32, pid: u32, nonce: [u8; 16]) -> Result<[u8; 36]> {
    crate::ensure!(generation != 0 && pid != 0, "zero instrumentation identity");
    let mut bytes = [0; 36];
    bytes[..8].copy_from_slice(b"MOORINS3");
    bytes[8] = 1;
    bytes[12..16].copy_from_slice(&generation.to_le_bytes());
    bytes[16..20].copy_from_slice(&pid.to_le_bytes());
    bytes[20..].copy_from_slice(&nonce);
    Ok(bytes)
}

pub fn validate_instrument_ack(
    bytes: &[u8],
    eof: bool,
    generation: u32,
    pid: u32,
    nonce: [u8; 16],
) -> Result<()> {
    crate::ensure!(
        eof && bytes == instrument_ack(generation, pid, nonce)?,
        "instrumentation acknowledgement was invalid"
    );
    Ok(())
}

pub fn last_lines(bytes: &[u8], lines: u32) -> &[u8] {
    crate::return_if!(lines == 0, &[]);
    let end = bytes.len() - usize::from(bytes.last() == Some(&b'\n'));
    bytes[..end]
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .nth_back(lines.saturating_sub(1) as usize)
        .map_or(bytes, |(at, _)| &bytes[at + 1..])
}

pub fn age(path: &Path, clock: Clock) -> String {
    let elapsed = lifecycle(path).and_then(|value| {
        let start = value.start_mono_ms.parse::<u64>().ok()?;
        let boot = decode16(&value.boot_id)?;
        (boot != [0; 16] && boot == clock.1)
            .then(|| clock.0.checked_sub(start))
            .flatten()
    });
    let Some(seconds) = elapsed.map(|millis| millis / 1000) else {
        return "unknown".into();
    };
    let (scale, unit) = [(86_400, "d"), (3_600, "h"), (60, "m")]
        .into_iter()
        .find(|(scale, _)| seconds >= *scale)
        .unwrap_or((1, "s"));
    format!("{}{unit} ago", seconds / scale)
}

pub fn tail(path: &Path, mut follow: bool, lines: u32, program: &str) -> Result<i32> {
    let log = companion(path, ".log");
    let read = || {
        Store::read_only(&log, Kind::Log, None).map_err(|_| "log store is unavailable".to_string())
    };
    let (commit, body) = read()?;
    let mut output = io::stdout().lock();
    output
        .write_all(last_lines(&body, lines))
        .map_err(text_error)?;
    let mut cursor = commit.end;
    while follow {
        thread::sleep(Duration::from_millis(50));
        follow = path.exists();
        let (commit, body) = read()?;
        if cursor < commit.start {
            eprintln!(
                "{program}: log gap: child-output bytes [{cursor},{}) were discarded",
                commit.start
            );
            cursor = commit.start;
        }
        if cursor < commit.end {
            output
                .write_all(&body[(cursor - commit.start) as usize..])
                .map_err(text_error)?;
            output.flush().ok();
            cursor = commit.end;
        }
    }
    Ok(0)
}

pub fn clear_store(log: &Path) -> Result<()> {
    let mut store = Store::open(log, Kind::Log, None).map_err(|_| "log store is unavailable")?;
    let selected = *store.selected();
    crate::return_if!(selected.length == 0, Ok(()));
    let epoch = selected.epoch.checked_add(1).ok_or("log clear failed")?;
    store
        .replace(b"", epoch, selected.end, selected.end)
        .map(|_| ())
        .map_err(|_| "log clear failed".into())
}

pub fn companion(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    value.into()
}

pub fn environment_key(invoked: &OsStr, suffix: &str) -> OsString {
    let raw = Path::new(invoked)
        .file_name()
        .unwrap_or(OsStr::new("moor"))
        .as_encoded_bytes();
    let mut key = String::with_capacity(raw.len().min(127 - suffix.len()) + suffix.len());
    for byte in raw.iter().take(127 - suffix.len()) {
        key.push(if byte.is_ascii_alphanumeric() {
            byte.to_ascii_uppercase() as char
        } else {
            '_'
        });
    }
    key.push_str(suffix);
    key.into()
}

pub fn supervised_generation(
    invoked: &OsStr,
    clear_supervised: bool,
    invalid: &str,
    read: impl FnOnce(&OsStr) -> Result<u32>,
) -> Result<(u32, bool)> {
    let key = environment_key(invoked, "_GENERATION");
    let selector = std::env::var_os("DESK_MOOR_LAUNCH_CHANNEL");
    let first = std::env::var_os(&key);
    let second = std::env::var_os("DESK_SESSION_GENERATION");
    unsafe {
        std::env::remove_var("DESK_MOOR_LAUNCH_CHANNEL");
        if clear_supervised || selector.is_none() {
            std::env::remove_var(&key);
            std::env::remove_var("DESK_SESSION_GENERATION");
        }
    }
    let Some(selector) = selector else {
        return Ok((1, false));
    };
    let generation = read(&selector)?;
    let expected = OsString::from(generation.to_string());
    let valid = first.as_ref() == Some(&expected) && second.as_ref() == Some(&expected);
    crate::ensure!(valid, invalid);
    Ok((generation, true))
}

pub fn terminal_environment(invoked: &OsStr) -> u8 {
    let supplied = std::env::var_os("TERM_PROGRAM").is_none();
    unsafe {
        if supplied {
            std::env::set_var("TERM_PROGRAM", "kitty");
            std::env::set_var("TERM_PROGRAM_VERSION", "0.47.0");
        }
        if std::env::var_os("LC_TERMINAL").is_none() {
            std::env::set_var("LC_TERMINAL", "kitty");
        }
    }
    let enabled = std::env::var_os(environment_key(invoked, "_NO_TERM_AUTORESPONSE"))
        .is_none_or(|value| value.is_empty());
    u8::from(enabled) | u8::from(supplied) << 1
}

fn ancestry_text(paths: &[PathBuf]) -> OsString {
    let mut joined = OsString::new();
    for path in paths {
        if !joined.is_empty() {
            joined.push(":");
        }
        joined.push(path);
    }
    joined
}

pub fn ancestry_paths(
    invoked: &OsStr,
    mut decode: impl FnMut(&[u8]) -> Result<OsString>,
) -> Result<Vec<PathBuf>> {
    let legacy = std::env::var_os(environment_key(invoked, "_SESSION"));
    let encoded = std::env::var_os(environment_key(invoked, "_SESSION_V2"));
    let Some(text) = encoded else {
        let Some(value) = legacy else {
            return Ok(Vec::new());
        };
        return value
            .as_encoded_bytes()
            .split(|byte| *byte == b':')
            .filter(|part| !part.is_empty())
            .map(|part| decode(part).map(PathBuf::from))
            .collect();
    };
    const INVALID: &str = "session ancestry v2 is malformed";
    let text = text
        .to_str()
        .and_then(|text| text.strip_prefix("v2:"))
        .ok_or(INVALID)?;
    let paths = text
        .split(':')
        .map(|item| {
            crate::ensure!(!item.is_empty(), INVALID);
            let bytes = STANDARD.decode(item).map_err(|_| INVALID)?;
            decode(&bytes).map(PathBuf::from)
        })
        .collect::<Result<Vec<_>>>()?;
    let carriers_agree = legacy.is_none_or(|value| value == ancestry_text(&paths));
    crate::ensure!(
        !paths.is_empty() && carriers_agree,
        "session ancestry carriers disagree"
    );
    Ok(paths)
}

pub fn extend_ancestry(
    invoked: &OsStr,
    path: PathBuf,
    decode: impl FnMut(&[u8]) -> Result<OsString>,
    encode: impl Fn(&OsStr) -> Vec<u8>,
) -> Result<()> {
    let mut paths = ancestry_paths(invoked, decode)?;
    paths.push(path);
    let mut v2 = "v2".to_string();
    for path in &paths {
        v2.push(':');
        STANDARD.encode_string(encode(path.as_os_str()), &mut v2);
    }
    unsafe {
        std::env::set_var(environment_key(invoked, "_SESSION"), ancestry_text(&paths));
        std::env::set_var(environment_key(invoked, "_SESSION_V2"), v2);
    }
    Ok(())
}

crate::schema!(enum ordinal pub SessionState; Missing, Live, Attached, Stale, Exited, Indeterminate);

pub fn session_name(name: OsString, insensitive: bool) -> Option<OsString> {
    let bytes = name.as_encoded_bytes();
    match name::artifact_suffix_len(bytes, insensitive) {
        Some(length) if length == b".exit".len() => {
            let at = bytes.len() - length;
            let base = unsafe { OsStr::from_encoded_bytes_unchecked(&bytes[..at]) };
            Some(base.to_owned())
        }
        Some(_) => None,
        None => Some(name),
    }
}

pub fn discover_sessions(
    root: &Path,
    mut classify: impl FnMut(OsString) -> Option<OsString>,
    inspect: impl Fn(&Path, Duration) -> SessionState + Sync,
) -> Result<Vec<SessionEntry>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut found = HashSet::new();
    for entry in fs::read_dir(root).map_err(text_error)? {
        let name = classify(entry.map_err(text_error)?.file_name());
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            found.insert(name);
        }
    }
    let mut entries = found
        .into_par_iter()
        .map(|name| {
            let path = root.join(&name);
            let remaining = deadline.saturating_duration_since(Instant::now());
            let state = if remaining.is_zero() {
                SessionState::Indeterminate
            } else {
                inspect(&path, remaining)
            };
            SessionEntry { state, path, name }
        })
        .filter(|entry| entry.state != SessionState::Missing)
        .collect::<Vec<_>>();
    entries.sort_by_cached_key(|entry| name::render(&entry.name));
    Ok(entries)
}

pub fn list_sessions_at(entries: Vec<SessionEntry>, all: bool, clock: Clock) -> i32 {
    let mut entries = entries
        .into_iter()
        .filter(|entry| all || entry.state != SessionState::Exited)
        .peekable();
    crate::return_if!(entries.peek().is_none(), {
        println!("(no sessions)");
        0
    });
    for entry in entries {
        let suffix = match entry.state {
            SessionState::Exited => " [exited]",
            SessionState::Attached => " [attached]",
            SessionState::Live => "",
            SessionState::Stale => " [stale]",
            SessionState::Indeterminate => " [indeterminate]",
            SessionState::Missing => unreachable!(),
        };
        let shown = name::render(&entry.name);
        println!("{shown:<24} since {}{suffix}", age(&entry.path, clock));
    }
    0
}

pub fn remove_all(
    entries: Vec<SessionEntry>,
    quiet: bool,
    mut cleanup: impl FnMut(&Path) -> Result<()>,
) -> Result<i32> {
    let mut count = 0;
    for entry in entries {
        let shown = || name::render(&entry.name);
        match entry.state {
            SessionState::Stale | SessionState::Exited => {
                cleanup(&entry.path)?;
                count += 1;
                if !quiet {
                    println!("removed {}", shown());
                }
            }
            SessionState::Live | SessionState::Attached => {
                println!("skipped {} (running)", shown())
            }
            SessionState::Indeterminate => println!("skipped {} (indeterminate)", shown()),
            SessionState::Missing => {}
        }
    }
    if !quiet {
        if count == 0 {
            println!("nothing to remove")
        } else {
            println!("{count} session(s) removed")
        }
    }
    Ok(0)
}

pub fn print_current(paths: &[PathBuf]) -> i32 {
    crate::return_if!(paths.is_empty(), 1);
    for (at, path) in paths.iter().enumerate() {
        print!(
            "{}{}",
            if at == 0 { "" } else { " > " },
            name::render(path.file_name().unwrap_or_default())
        );
    }
    println!();
    0
}

pub fn lifecycle_running(
    identity: &[u8],
    generation: Generation,
    incarnation: [u8; 16],
    start: Start,
    paths: (&str, Option<&[u8]>, Option<&[u8]>),
) -> String {
    let path = |value: Option<&[u8]>| {
        value.map_or_else(
            || "null".into(),
            |value| format!("\"{}\"", STANDARD.encode(value)),
        )
    };
    let session = Base64Display::new(identity, &STANDARD);
    let allocated = generation
        .0
        .map_or("null".into(), |value| value.to_string());
    let wire = generation.1;
    let incarnation = Base64Display::new(&incarnation, &STANDARD);
    let (wall, mono, boot) = (start.0, start.1, Base64Display::new(&start.2, &STANDARD));
    let (encoding, event, instrument) = (paths.0, path(paths.1), path(paths.2));
    format!(
        "{{\"v\":1,\"type\":\"lifecycle\",\"phase\":\"running\",\"session\":\"{session}\",\"generation\":{allocated},\"wire_generation\":{wire},\"incarnation\":\"{incarnation}\",\"start_wall_ms\":\"{wall}\",\"start_mono_ms\":\"{mono}\",\"boot_id\":\"{boot}\",\"path_encoding\":\"{encoding}\",\"event_path\":{event},\"instrument_path\":{instrument}}}\n"
    )
}

pub fn holder_artifacts(
    identity: &[u8],
    generation: Generation,
    incarnation: [u8; 16],
    semantic_token: [u8; 16],
    start: Start,
    mut config: ArtifactConfig<'_>,
) -> Result<PreparedArtifacts> {
    let running = lifecycle_running(
        identity,
        generation,
        incarnation,
        start,
        (
            config.encoding,
            config.event_identity,
            config.instrument_identity,
        ),
    );
    let session = STANDARD.encode(identity);
    let ArtifactStores {
        lifecycle,
        event,
        log,
    } = if let Some(stores) = config.stores.take() {
        stores
    } else {
        let deadline = Instant::now() + Duration::from_secs(2);
        let create = |path: &Path, kind, body: &[u8]| {
            Store::create(path, kind, generation.1, body, 0, 0)
                .map_err(|error| format!("store initialization failed: {error:?}"))
        };
        let event_header =
            || events::canonical_header(start.0, &session, generation.0, Cursor(0, 0, 0, 1));
        let lifecycle = create(
            &companion(config.marker, ".exit"),
            Kind::Exit,
            running.as_bytes(),
        )?;
        let event = config
            .event_path
            .map(|path| {
                config
                    .event_store
                    .take()
                    .map_or_else(|| create(path, Kind::Event, event_header().as_bytes()), Ok)
            })
            .transpose()?;
        let log = (config.log_cap != 0)
            .then(|| create(&companion(config.marker, ".log"), Kind::Log, &[]))
            .transpose()?;
        crate::ensure!(Instant::now() <= deadline, "store initialization timed out");
        ArtifactStores {
            lifecycle,
            event,
            log,
        }
    };
    let commit = event.as_ref().map(|store| *store.selected());
    let events = event.map(|store| EventConfig {
        store,
        stream: EventStream::new(),
        created: start.0,
        session,
        generation: generation.0,
    });
    let log = log.map(|store| (store, config.log_cap));
    let storage = PreparedStorage {
        log,
        events,
        lifecycle,
    };
    let event_len = config.event_identity.map_or(0, <[u8]>::len);
    let mut status = Vec::with_capacity(identity.len() + event_len + 110);
    put_wide(&mut status, identity).map_err(crate::protocol)?;
    status.extend_from_slice(&generation.1.to_le_bytes());
    status.extend_from_slice(&incarnation);
    status.push(config.event_path.map_or(0, |_| config.event_layout));
    put_wide(&mut status, config.event_identity.unwrap_or_default()).map_err(crate::protocol)?;
    let commit_at = status.len();
    if let Some(commit) = commit.filter(|_| config.event_layout == 2) {
        status.push(commit.body);
        status.extend_from_slice(&commit.index.to_le_bytes());
        status.extend_from_slice(&commit.length.to_le_bytes());
        status.extend_from_slice(&commit.hash);
    } else {
        status.push(0xff);
        status.resize(status.len() + 48, 0);
    }
    status.extend_from_slice(&start.0.to_le_bytes());
    status.extend_from_slice(&start.1.to_le_bytes());
    status.extend_from_slice(&start.2);
    Ok(PreparedArtifacts {
        core: CoreConfig {
            generation: generation.1,
            identity: identity.to_vec(),
            incarnation,
            semantic_token,
            replay_limit: 4 << 20,
        },
        storage,
        status,
        commit_at,
        running,
    })
}

pub fn lifecycle_exit(running: &str, end_wall: u64, output_end: u64, outcome: &str) -> String {
    let mut exited = running.strip_suffix("}\n").unwrap().replacen(
        "\"phase\":\"running\"",
        "\"phase\":\"exited\"",
        1,
    );
    writeln!(
        exited,
        ",\"end_wall_ms\":\"{end_wall}\",\"output_end\":\"{output_end}\",{outcome}}}"
    )
    .unwrap();
    exited
}

pub fn exit_records(
    running: &str,
    ts: (u64, u64),
    end: u64,
    outcome: (&str, &str, u64, Option<&str>),
) -> (Event, Vec<u8>) {
    let (ended, key, value, method) = outcome;
    let include_method = method.is_some();
    let method = method.unwrap_or_default();
    let fields = [
        ("ended", Json::String(ended)),
        (key, Json::Number(value)),
        ("method", Json::String(method)),
    ];
    let mut suffix = format!("\"ended\":\"{ended}\",\"{key}\":{value}");
    if include_method {
        write!(suffix, ",\"method\":\"{method}\"").unwrap();
    }
    (
        events::event("exit", ts.0, &fields[..2 + usize::from(include_method)]),
        lifecycle_exit(running, ts.1, end, &suffix).into_bytes(),
    )
}

pub fn cleanup_artifacts(
    path: &Path,
    identity: Option<&[u8]>,
    decode: impl Fn(Vec<u8>) -> Option<PathBuf>,
) -> ([Option<PathBuf>; 2], Option<PathBuf>) {
    let Some(value) = lifecycle(path) else {
        return ([None, None], None);
    };
    let binding = STANDARD
        .decode(value.session)
        .ok()
        .zip(decode16(&value.incarnation))
        .map(|(identity, incarnation)| (identity, value.wire_generation, incarnation));
    if identity.is_some_and(|expected| {
        binding
            .as_ref()
            .is_none_or(|(actual, _, _)| actual != expected)
    }) {
        return ([None, None], None);
    }
    let paths = [value.event_path, value.instrument_path]
        .map(|path| path.and_then(|text| STANDARD.decode(text).ok()));
    let external = paths.map(|path| path.and_then(&decode));
    let expected = external[1].as_ref().and_then(|target| {
        let (identity, generation, incarnation) = binding.as_ref()?;
        instrument_stage(target.parent()?, identity, *generation, *incarnation).ok()
    });
    (external, expected)
}

fn lifecycle(path: &Path) -> Option<Lifecycle> {
    let (_, body) = Store::read_only(&companion(path, ".exit"), Kind::Exit, None).ok()?;
    serde_json::from_slice(&body).ok()
}

fn decode16(value: &str) -> Option<[u8; 16]> {
    let mut bytes = [0; 16];
    (STANDARD.decode_slice(value, &mut bytes).ok() == Some(16)).then_some(bytes)
}

pub fn cleanup_companions(
    path: &Path,
    external: [Option<PathBuf>; 2],
    allow_store_files: bool,
    valid_external: impl Fn(&Path) -> bool,
) -> Result<()> {
    let remove = |target: &Path, file: bool| {
        let metadata = match fs::symlink_metadata(target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        if metadata.is_dir() {
            return Store::remove(target)
                .map_err(|error| format!("store removal failed: {error:?}"));
        }
        crate::ensure!(file, "store path is not a directory");
        fs::remove_file(target).map_err(text_error)
    };
    for suffix in [".log", ".events", ".exit"] {
        remove(&companion(path, suffix), allow_store_files)?;
    }
    for target in external.into_iter().flatten() {
        if target == path {
            continue;
        }
        crate::ensure!(valid_external(&target), "external artifact is not owned");
        remove(&target, true)?;
    }
    Ok(())
}

pub fn now() -> u64 {
    UNIX_EPOCH.elapsed().unwrap_or_default().as_millis() as u64
}

pub fn monotonic() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

pub fn parse_boot_uuid(text: &str) -> Option<[u8; 16]> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let hyphens = [8, 13, 18, 23]
        .into_iter()
        .all(|at| text.as_bytes().get(at) == Some(&b'-'));
    crate::return_if!(text.len() != 36 || !hyphens, None);
    let bytes = uuid::Uuid::parse_str(text).ok()?.into_bytes();
    (bytes != [0; 16]).then_some(bytes)
}

pub fn random_array<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| "random source failed")?;
    crate::ensure!(bytes != [0; N], "random source failed");
    Ok(bytes)
}
