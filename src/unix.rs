use crate::cli::{Action, CreateMode, Options, Redraw, Reset};
use crate::events::{self, Cursor, EventStream, Json};
use crate::name;
use crate::session::{
    InputAdmission, LeaseMachine, LeaseOperation, LeaseRequest, LeaseResult, LeaseRole, OwnedInput,
    ResultOutcome, TokenSource,
};
use crate::store::{Kind, Store};
use crate::terminal::{Observation, Scanner};
use crate::wire::{Codec, Message, Profile};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, String>;

#[rustfmt::skip]
struct Tx { stream: UnixStream, codec: Codec }
#[rustfmt::skip]
struct Rx { stream: UnixStream, codec: Codec, queued: VecDeque<Message> }
#[rustfmt::skip]
impl Tx { fn send(&mut self, scope: u32, kind: u8, payload: &[u8]) -> Result<()> { let mut bytes = Vec::new(); self.codec.encode(scope, kind, payload, &mut bytes).map_err(|e| format!("protocol error: {e:?}"))?; self.stream.write_all(&bytes).map_err(|e| e.to_string()) } }
#[rustfmt::skip]
impl Rx { fn recv(&mut self) -> Result<Message> { loop { if let Some(message) = self.queued.pop_front() { return Ok(message); } let mut bytes = [0; 65536]; let n = self.stream.read(&mut bytes).map_err(|e| e.to_string())?; if n == 0 { return Err("connection closed".into()); } let mut messages = Vec::new(); self.codec.feed(now(), &bytes[..n], &mut messages).map_err(|e| format!("protocol error: {e:?}"))?; self.queued.extend(messages); } } }
#[rustfmt::skip]
fn split_wire(stream: UnixStream) -> Result<(Arc<Mutex<Tx>>, Rx)> { stream.set_read_timeout(Some(Duration::from_secs(2))).map_err(|e| e.to_string())?; let write = stream.try_clone().map_err(|e| e.to_string())?; write.set_write_timeout(Some(Duration::from_millis(250))).map_err(|e| e.to_string())?; Ok((Arc::new(Mutex::new(Tx { stream: write, codec: Codec::new(Profile::Controller) })), Rx { stream, codec: Codec::new(Profile::Controller), queued: VecDeque::new() })) }

#[derive(Clone)]
#[rustfmt::skip]
struct Output { sequence: u64, offset: u64, bytes: Vec<u8> }
struct Random;
#[rustfmt::skip]
impl TokenSource for Random { fn token(&mut self) -> Option<[u8; 16]> { random16().ok() } }
#[rustfmt::skip]
struct Core { identity: Vec<u8>, incarnation: [u8; 16], master: Mutex<File>, lease: Mutex<LeaseMachine>, viewers: Mutex<HashMap<u64, Arc<Mutex<Tx>>>>, history: Mutex<VecDeque<Output>>, scanner: Mutex<Scanner>, log: Mutex<Option<(Store, u64)>>, events: Mutex<Option<EventWriter>>, lifecycle: Mutex<Option<Lifecycle>>, child_group: i32, running: AtomicBool, next_connection: AtomicU64 }
#[rustfmt::skip]
struct EventWriter { store: Store, stream: EventStream, created: u64, session: String, snapshots: Vec<(u64, Observation)> }
#[rustfmt::skip]
struct Lifecycle { store: Store, running: String }
#[rustfmt::skip]
impl EventWriter {
    fn make(observation: &Observation, ts: u64) -> Option<std::result::Result<events::Event, events::EventError>> { Some(match observation { Observation::Ready => events::event("ready", ts, &[]), Observation::State { state, title, truncated } => events::event("state", ts, &[("state", Json::String(state)), ("title", Json::String(title)), ("truncated", Json::Bool(*truncated))]), Observation::Link { uri, truncated } => events::event("link", ts, &[("uri", Json::String(uri)), ("truncated", Json::Bool(*truncated))]), Observation::Degraded { scanner, reason } => events::event("observer-degraded", ts, &[("scanner", Json::String(scanner)), ("reason", Json::String(reason))]), Observation::Query { .. } => return None }) }
    fn commit(&mut self, event: events::Event, remember: Option<(u64, Observation)>) -> bool { let snapshots = self.snapshots.iter().filter_map(|(ts, observation)| Self::make(observation, *ts)?.ok()).collect(); let Ok(batch) = self.stream.transact(snapshots, vec![event], true) else { return false }; let mut body = events::canonical_header(self.created, &self.session, None, batch.cursor); for record in batch.records { body.push_str(&record); } if self.store.replace(body.as_bytes(), batch.cursor.0, batch.cursor.2, batch.cursor.1).is_err() { return false }
        if let Some((ts, observation)) = remember { let same = |old: &Observation| matches!((&observation, old), (Observation::Ready, Observation::Ready) | (Observation::State { .. }, Observation::State { .. }) | (Observation::Link { .. }, Observation::Link { .. })); self.snapshots.retain(|(_, old)| !same(old)); if !matches!(observation, Observation::Degraded { .. } | Observation::Query { .. }) { self.snapshots.push((ts, observation)); } } true }
    fn push(&mut self, observation: Observation) -> bool { let ts = now(); let Some(Ok(event)) = Self::make(&observation, ts) else { return matches!(observation, Observation::Query { .. }) }; self.commit(event, Some((ts, observation))) }
    fn exit(&mut self, status: ExitStatus) -> bool { use std::os::unix::process::ExitStatusExt; let event = if let Some(code) = status.code() { events::event("exit", now(), &[("ended", Json::String("exited")), ("code", Json::Number(code as u64))]) } else { events::event("exit", now(), &[("ended", Json::String("signalled")), ("signal", Json::Number(status.signal().unwrap_or(0) as u64))]) }; event.is_ok_and(|event| self.commit(event, None)) }
}

#[rustfmt::skip]
struct Client { tx: Arc<Mutex<Tx>>, rx: Rx, generation: u32, incarnation: [u8; 16], identity: Vec<u8> }
#[rustfmt::skip]
impl Client {
    fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
        if peer_uid(&stream) != Some(unsafe { libc::geteuid() }) { return Err("holder peer identity mismatch".into()); }
        let (tx, mut rx) = split_wire(stream)?;
        let identity = identity(path)?;
        tx.lock().unwrap().send(0, 1, &hello(&identity))?;
        let reply = rx.recv()?;
        if reply.kind != 2 || reply.scope == 0 || reply.payload.len() < 25 {
            return Err("holder identity exchange failed".into());
        }
        let generation = u32::from_le_bytes(reply.payload[1..5].try_into().unwrap());
        let incarnation = reply.payload[5..21].try_into().unwrap();
        if reply.payload[0] != 3
            || generation != reply.scope
            || get_wide(&reply.payload, 21) != Some(identity.as_slice())
        {
            return Err("holder identity exchange failed".into());
        }
        rx.stream.set_read_timeout(None).map_err(|e| e.to_string())?;
        Ok(Self { tx, rx, generation, incarnation, identity })
    }
    fn send(&self, kind: u8, payload: &[u8]) -> Result<()> {
        self.tx.lock().unwrap().send(self.generation, kind, payload)
    }
}

pub fn run(action: Action, invoked: &OsStr, program: &str) -> i32 {
    match execute(action, invoked, program) {
        Ok(status) => status,
        Err(error) => {
            let _ = writeln!(io::stderr(), "{program}: {error}");
            1
        }
    }
}

fn execute(action: Action, invoked: &OsStr, program: &str) -> Result<i32> {
    match action {
        Action::Create {
            mode,
            session,
            command,
            options,
        } => create(mode, session, command, options, invoked, program),
        Action::Attach { session, options } => attach(&resolve(&session, invoked)?, options),
        Action::Push(session) => push(&resolve(&session, invoked)?),
        Action::Kill {
            session,
            force,
            quiet,
        } => kill(&session, &resolve(&session, invoked)?, force, quiet),
        Action::Tail {
            session,
            follow,
            lines,
        } => tail(
            &session,
            &resolve(&session, invoked)?,
            follow,
            lines,
            program,
        ),
        Action::Clear(session) => clear(session.as_ref(), invoked),
        Action::Current => current(invoked),
        Action::List { all } => list(invoked, all),
        Action::Remove {
            session,
            all,
            quiet,
        } => remove(session.as_ref(), all, quiet, invoked),
        Action::Help | Action::Version => unreachable!(),
    }
}

fn create(
    mode: CreateMode,
    session: OsString,
    command: Vec<OsString>,
    options: Options,
    invoked: &OsStr,
    program: &str,
) -> Result<i32> {
    let path = resolve(&session, invoked)?;
    match classify(&path) {
        State::Live if matches!(mode, CreateMode::Bare | CreateMode::LegacyA) => {
            return attach(&path, options);
        }
        State::Live => {
            return Err(format!(
                "session '{}' is already running",
                name::render(&session)
            ));
        }
        State::Indeterminate => {
            return Err(format!(
                "session '{}' could not be identified",
                name::render(&session)
            ));
        }
        State::Stale => cleanup(&path)?,
        State::Missing => {}
    }
    let stage = companion(&path, &format!(".stage-{}", std::process::id()));
    let _ = fs::remove_file(&stage);
    let listener = UnixListener::bind(&stage).map_err(|e| e.to_string())?;
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    let foreground = matches!(mode, CreateMode::Run | CreateMode::LegacyRun);
    let attach_after = matches!(
        mode,
        CreateMode::Bare | CreateMode::New | CreateMode::LegacyA | CreateMode::LegacyC
    );
    let config = Config {
        path: path.clone(),
        stage,
        command,
        options: options.clone(),
        invoked: invoked.to_owned(),
    };
    if foreground {
        return holder(config, listener, None, false);
    }
    let (parent, child) = UnixStream::pair().map_err(|e| e.to_string())?;
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    if pid == 0 {
        drop(parent);
        unsafe { libc::setsid() };
        let status = holder(config, listener, Some(child), true).unwrap_or(1);
        unsafe { libc::_exit(status) }
    }
    drop(child);
    drop(listener);
    let mut ready = parent;
    let mut header = [0; 3];
    ready
        .read_exact(&mut header)
        .map_err(|_| "holder failed before launch".to_string())?;
    let mut message = vec![0; u16::from_le_bytes(header[1..].try_into().unwrap()) as usize];
    ready.read_exact(&mut message).map_err(|e| e.to_string())?;
    if header[0] != 0 {
        return Err(String::from_utf8_lossy(&message).into_owned());
    }
    if !options.quiet {
        match mode {
            CreateMode::Bare | CreateMode::New => {
                println!("session '{}' created", name::render(&session))
            }
            CreateMode::Start => println!("session '{}' started", name::render(&session)),
            _ => {}
        }
    }
    if attach_after {
        attach(&path, options)
    } else {
        let _ = program;
        Ok(0)
    }
}

struct Config {
    path: PathBuf,
    stage: PathBuf,
    command: Vec<OsString>,
    options: Options,
    invoked: OsString,
}

fn holder(
    config: Config,
    listener: UnixListener,
    ready: Option<UnixStream>,
    daemon: bool,
) -> Result<i32> {
    let result = holder_setup(&config, listener);
    let (mut child, listener, core, mut master) = match result {
        Ok(value) => value,
        Err(error) => {
            ready_result(ready, 1, &error);
            let _ = fs::remove_file(&config.stage);
            let _ = cleanup(&config.path);
            if let Some(path) = &config.options.events {
                let _ = delete_store(path);
            }
            return Err(error);
        }
    };
    ready_result(ready, 0, "");
    if daemon {
        let null = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .unwrap();
        for fd in 0..=2 {
            unsafe { libc::dup2(null.as_raw_fd(), fd) };
        }
    }
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
    let mut status = None;
    while status.is_none() {
        while let Ok((stream, _)) = listener.accept() {
            let shared = core.clone();
            thread::spawn(move || connection(stream, shared));
        }
        let mut bytes = [0; 65536];
        match master.read(&mut bytes) {
            Ok(n) if n > 0 => output(&core, &bytes[..n]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            _ => {}
        }
        status = child.try_wait().map_err(|e| e.to_string())?;
        thread::sleep(Duration::from_millis(5));
    }
    let status = status.unwrap(); core.running.store(false, Ordering::Release);
    for viewer in core.viewers.lock().unwrap().values() {
        let _ = viewer
            .lock()
            .unwrap()
            .stream
            .shutdown(std::net::Shutdown::Both);
    }
    if lifecycle_exit(&core, status) { let _ = fs::remove_file(&config.path); }
    Ok(shell_status(status))
}

#[rustfmt::skip]
fn holder_setup(
    config: &Config,
    listener: UnixListener,
) -> Result<(Child, UnixListener, Arc<Core>, File)> {
    let incarnation = random16()?;
    let session_identity = identity(&config.path)?;
    let (master, slave) = pty()?;
    child_environment(&config.invoked, &config.path)?;
    let mut command = if config.command.is_empty() {
        vec![
            std::env::var_os("SHELL")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/bin/sh".into()),
        ]
    } else {
        config.command.clone()
    };
    let executable = command.remove(0);
    let mut process = Command::new(executable);
    process
        .args(command)
        .stdin(Stdio::from(slave.try_clone().map_err(|e| e.to_string())?));
    process.stdout(Stdio::from(slave.try_clone().map_err(|e| e.to_string())?));
    if let Some(path) = &config.options.stderr {
        process.stderr(Stdio::from(open_stderr(path)?));
    } else {
        process.stderr(Stdio::from(slave));
    }
    if let Some(directory) = &config.options.directory {
        process.current_dir(directory);
    }
    unsafe {
        use std::os::unix::process::CommandExt;
        process.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let log = if config.options.log_cap == 0 { None } else { Some((Store::create(&companion(&config.path, ".log"), Kind::Log, 1, b"", 0, 0).map_err(|_| "log store could not be created".to_string())?, config.options.log_cap)) };
    let running = lifecycle_running(config, &session_identity, incarnation)?; let lifecycle = Lifecycle { store: Store::create(&companion(&config.path, ".exit"), Kind::Exit, 1, running.as_bytes(), 0, 0).map_err(|_| "lifecycle store could not be created".to_string())?, running };
    let events = if let Some(path) = &config.options.events {
        if path.is_dir() && fs::read_dir(path).map_err(|e| e.to_string())?.next().is_none() { fs::remove_dir(path).map_err(|e| e.to_string())?; }
        let session = STANDARD.encode(&session_identity); let created = now(); let header = events::canonical_header(created, &session, None, Cursor(0, 0, 0, 1)); Some(EventWriter { store: Store::create(path, Kind::Event, 1, header.as_bytes(), 0, 0).map_err(|_| "event store could not be created".to_string())?, stream: EventStream::new(), created, session, snapshots: Vec::new() })
    } else { None };
    let instrument = instrument_setup(
        config.options.instrument.as_ref(),
        &config.path,
        &mut process,
    )?;
    let mut child = process
        .spawn()
        .map_err(|e| format!("could not start child: {e}"))?;
    if let Err(error) = instrument_ack(instrument, child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    fs::rename(&config.stage, &config.path).map_err(|e| e.to_string())?;
    let core = Arc::new(Core {
        identity: session_identity,
        incarnation,
        master: Mutex::new(master.try_clone().map_err(|e| e.to_string())?),
        lease: Mutex::new(LeaseMachine::new(incarnation)),
        viewers: Mutex::new(HashMap::new()),
        history: Mutex::new(VecDeque::new()),
        scanner: Mutex::new(Scanner::new(24)),
        log: Mutex::new(log),
        events: Mutex::new(events),
        lifecycle: Mutex::new(Some(lifecycle)),
        child_group: child.id() as i32,
        running: AtomicBool::new(true),
        next_connection: AtomicU64::new(1),
    });
    Ok((child, listener, core, master))
}

fn connection(stream: UnixStream, core: Arc<Core>) {
    if peer_uid(&stream) != Some(unsafe { libc::geteuid() }) {
        return;
    }
    let Ok((tx, mut rx)) = split_wire(stream) else {
        return;
    };
    let id = core.next_connection.fetch_add(1, Ordering::Relaxed);
    let mut run = || -> Result<()> {
        let first = rx.recv()?;
        if first.kind != 1 || first.scope != 0 || first.payload != hello(&core.identity) {
            return Err("identity mismatch".into());
        }
        let mut ack = vec![3];
        ack.extend_from_slice(&1u32.to_le_bytes());
        ack.extend_from_slice(&core.incarnation);
        put_wide(&mut ack, &core.identity);
        tx.lock().unwrap().send(1, 2, &ack)?;
        rx.stream.set_read_timeout(None).map_err(|e| e.to_string())?;
        loop {
            let message = rx.recv()?;
            if message.scope != 1 {
                return Err("generation mismatch".into());
            }
            match message.kind {
                3 => attach_server(id, &core, &tx, &message.payload)?,
                9 => input_server(id, &core, &tx, &message.payload)?,
                13 => tx.lock().unwrap().send(1, 14, &status(&core, id))?,
                15 => terminate_server(&core, &tx, &message.payload)?,
                0x15 => lease_server(id, &core, &tx, &message.payload)?,
                0x17 => release_server(id, &core, &tx, &message.payload)?,
                0x18 => keepalive_server(id, &core, &message.payload)?,
                0x19 => clear_server(&core, &tx, &message.payload)?,
                7 => {}
                _ => return Err("malformed controller operation".into()),
            }
        }
    };
    let _ = run();
    core.viewers.lock().unwrap().remove(&id);
    core.lease.lock().unwrap().disconnect(id);
}

fn attach_server(id: u64, core: &Arc<Core>, tx: &Arc<Mutex<Tx>>, payload: &[u8]) -> Result<()> {
    if payload.len() != 5 || payload[..4] != [0; 4] || payload[4] & !3 != 0 {
        return Err("malformed attach".into());
    }
    let lease = if payload[4] & 1 != 0 {
        Some(
            core.lease
                .lock()
                .unwrap()
                .request(id, &fresh(LeaseRole::Viewer), now(), &mut Random),
        )
    } else {
        None
    };
    let preamble = core
        .scanner
        .lock()
        .unwrap()
        .modes()
        .preamble()
        .unwrap_or_default();
    let mut state = Vec::new();
    state.extend_from_slice(&(preamble.len() as u16).to_le_bytes());
    state.extend_from_slice(&preamble);
    let history = core.history.lock().unwrap().clone();
    tx.lock().unwrap().send(1, 5, &state)?;
    core.viewers.lock().unwrap().insert(id, tx.clone());
    tx.lock().unwrap().send(1, 4, &status(core, id))?;
    if let Some(result) = lease {
        let bytes = result
            .encode_wire()
            .map_err(|e| format!("protocol error: {e:?}"))?;
        tx.lock().unwrap().send(1, 0x16, &bytes)?;
    }
    for record in &history {
        send_output(tx, record)?;
    }
    Ok(())
}

fn lease_server(id: u64, core: &Arc<Core>, tx: &Arc<Mutex<Tx>>, payload: &[u8]) -> Result<()> {
    let request = LeaseRequest::decode_wire(payload).map_err(|_| "malformed lease".to_string())?;
    let result = core
        .lease
        .lock()
        .unwrap()
        .request(id, &request, now(), &mut Random);
    let bytes = result
        .encode_wire()
        .map_err(|e| format!("protocol error: {e:?}"))?;
    tx.lock().unwrap().send(1, 0x16, &bytes)
}

fn input_server(id: u64, core: &Arc<Core>, tx: &Arc<Mutex<Tx>>, payload: &[u8]) -> Result<()> {
    if payload.len() < 13 || payload[12] != 0 {
        return Err("malformed input".into());
    }
    let input = OwnedInput {
        epoch: u32::from_le_bytes(payload[..4].try_into().unwrap()),
        request_id: u64::from_le_bytes(payload[4..12].try_into().unwrap()),
        exact_payload: payload.to_vec(),
    };
    let admission = core.lease.lock().unwrap().admit_input_at(id, &input, now());
    if let InputAdmission::Replay(receipt) = admission {
        return tx.lock().unwrap().send(1, 10, &receipt);
    }
    let mut receipt = Vec::new();
    receipt.extend_from_slice(&input.epoch.to_le_bytes());
    receipt.extend_from_slice(&input.request_id.to_le_bytes());
    receipt.extend_from_slice(&1u32.to_le_bytes());
    receipt.extend_from_slice(&core.incarnation);
    match admission {
        InputAdmission::Execute => {
            let result = core.master.lock().unwrap().write_all(&payload[13..]);
            receipt.extend_from_slice(
                &(if result.is_ok() {
                    payload.len() - 13
                } else {
                    0
                } as u64)
                    .to_le_bytes(),
            );
            receipt.push(u8::from(result.is_err()));
            receipt.extend_from_slice(&(if result.is_ok() { 0u16 } else { 20 }).to_le_bytes());
            core.lease
                .lock()
                .unwrap()
                .finish_input(id, input, receipt.clone())
                .map_err(|_| "lease lost".to_string())?;
        }
        InputAdmission::Refuse(_) => {
            receipt.extend_from_slice(&0u64.to_le_bytes());
            receipt.push(1);
            receipt.extend_from_slice(&15u16.to_le_bytes());
        }
        InputAdmission::Replay(_) => unreachable!(),
    }
    tx.lock().unwrap().send(1, 10, &receipt)
}

fn release_server(id: u64, core: &Arc<Core>, tx: &Arc<Mutex<Tx>>, payload: &[u8]) -> Result<()> {
    if payload.len() != 20 {
        return Err("malformed release".into());
    }
    let result = core.lease.lock().unwrap().release(
        id,
        u32::from_le_bytes(payload[..4].try_into().unwrap()),
        payload[4..].try_into().unwrap(),
    );
    let bytes = result
        .encode_wire()
        .map_err(|e| format!("protocol error: {e:?}"))?;
    tx.lock().unwrap().send(1, 0x16, &bytes)
}
fn keepalive_server(id: u64, core: &Arc<Core>, payload: &[u8]) -> Result<()> {
    if payload.len() != 20 {
        return Err("malformed keepalive".into());
    }
    core.lease
        .lock()
        .unwrap()
        .keepalive(
            id,
            u32::from_le_bytes(payload[..4].try_into().unwrap()),
            payload[4..].try_into().unwrap(),
            now(),
        )
        .map_err(|_| "lease not held".to_string())
}
fn terminate_server(core: &Arc<Core>, tx: &Arc<Mutex<Tx>>, payload: &[u8]) -> Result<()> {
    let at = 4 + core.identity.len();
    if payload.len() != at + 21
        || payload.get(4..at) != Some(core.identity.as_slice())
        || u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize != core.identity.len()
        || u32::from_le_bytes(payload[at..at + 4].try_into().unwrap()) != 1
        || payload[at + 4..at + 20] != core.incarnation
        || payload[at + 20] & !1 != 0
    {
        return Err("termination identity mismatch".into());
    }
    let signal = if payload[at + 20] == 1 {
        libc::SIGKILL
    } else {
        libc::SIGTERM
    };
    unsafe { libc::kill(-core.child_group, signal) };
    tx.lock().unwrap().send(
        1,
        16,
        &[0, 3, if signal == libc::SIGKILL { 2 } else { 1 }, 0, 0],
    )
}
fn clear_server(core: &Arc<Core>, tx: &Arc<Mutex<Tx>>, payload: &[u8]) -> Result<()> {
    if payload.len() != 24 || payload[..16] != core.incarnation {
        return Err("stale log status".into());
    }
    let observed = u64::from_le_bytes(payload[16..].try_into().unwrap());
    let mut log = core.log.lock().unwrap();
    let (outcome, epoch, prior, resulting, end) = if let Some((store, _)) = log.as_mut() {
        let selected = store.selected().unwrap().clone();
        if selected.index != observed {
            return Err("stale log status".into());
        }
        let end = selected.end;
        let epoch = selected.epoch.checked_add(1).ok_or("log exhausted")?;
        let resulting = store
            .replace(b"", epoch, end, end)
            .map_err(|_| "log unavailable")?
            .index;
        (0, epoch, selected.index, resulting, end)
    } else {
        (1, 0, 0, 0, 0)
    };
    let bytes = crate::wire::log_clear_result_payload(outcome, 0, epoch, prior, resulting, end)
        .map_err(|e| format!("protocol error: {e:?}"))?;
    tx.lock().unwrap().send(1, 0x1a, &bytes)
}

fn output(core: &Arc<Core>, bytes: &[u8]) {
    let observations = core.scanner.lock().unwrap().feed(now(), bytes);
    let mut events = core.events.lock().unwrap(); for observation in observations { if events.as_mut().is_some_and(|writer| !writer.push(observation)) { *events = None; break; } } drop(events);
    let mut history = core.history.lock().unwrap();
    let sequence = history.back().map_or(1, |r| r.sequence + 1);
    let offset = history
        .back()
        .map_or(0, |r| r.offset + r.bytes.len() as u64);
    let record = Output {
        sequence,
        offset,
        bytes: bytes.to_vec(),
    };
    history.push_back(record.clone());
    while history.iter().map(|r| r.bytes.len()).sum::<usize>() > 4 << 20 {
        history.pop_front();
    }
    drop(history); let mut log = core.log.lock().unwrap(); let mut log_failed = false; if let Some((store, cap)) = log.as_mut() {
        let end = offset + bytes.len() as u64;
        let selected = store.selected().unwrap().clone();
        if selected.length + bytes.len() as u64 <= *cap {
            log_failed = store.append(bytes, end).is_err();
        } else if let Ok(mut retained) = store.read() {
            retained.extend_from_slice(bytes);
            let keep = retained.len().min(*cap as usize);
            let suffix = retained.split_off(retained.len() - keep);
            log_failed = store.replace(
                &suffix,
                selected.epoch.saturating_add(1),
                end - keep as u64,
                end,
            ).is_err();
        } else { log_failed = true; }
    }
    if log_failed { *log = None; } drop(log);
    let mut dead = Vec::new();
    for (id, viewer) in core.viewers.lock().unwrap().iter() {
        if send_output(viewer, &record).is_err() {
            dead.push(*id)
        }
    }
    for id in dead {
        core.viewers.lock().unwrap().remove(&id);
        core.lease.lock().unwrap().disconnect(id);
    }
}
fn send_output(tx: &Arc<Mutex<Tx>>, record: &Output) -> Result<()> {
    let mut payload = Vec::with_capacity(16 + record.bytes.len());
    payload.extend_from_slice(&record.sequence.to_le_bytes());
    payload.extend_from_slice(&record.offset.to_le_bytes());
    payload.extend_from_slice(&record.bytes);
    tx.lock().unwrap().send(1, 6, &payload)
}

fn attach(path: &Path, options: Options) -> Result<i32> {
    let mut client = Client::connect(path).map_err(|_| {
        format!(
            "session '{}' does not exist",
            name::render(path.as_os_str())
        )
    })?;
    client.send(3, &[0, 0, 0, 0, 1 | u8::from(options.non_vt) << 1])?;
    let mut lease = None;
    let mut input_started = false;
    let detached = Arc::new(AtomicBool::new(false));
    loop {
        let message = match client.rx.recv() { Ok(message) => message, Err(_) if detached.load(Ordering::Acquire) => return Ok(0), Err(error) => return Err(error) };
        match message.kind {
            5 => {
                if message.payload.len() >= 2 {
                    let n = u16::from_le_bytes(message.payload[..2].try_into().unwrap()) as usize;
                    if message.payload.len() == n + 2 {
                        io::stdout()
                            .write_all(&message.payload[2..])
                            .map_err(|e| e.to_string())?;
                    }
                }
                if options.reset == Reset::Move {
                    io::stdout()
                        .write_all(b"\x1b[H")
                        .map_err(|e| e.to_string())?;
                }
            }
            0x16 => {
                let result = LeaseResult::decode_wire(&message.payload)
                    .map_err(|_| "malformed lease result".to_string())?;
                if matches!(
                    result.outcome,
                    ResultOutcome::Granted | ResultOutcome::Resumed
                ) {
                    lease = Some((result.epoch, result.token));
                    if !input_started {
                        input_started = true;
                        input_thread(
                            client.tx.clone(),
                            client.generation,
                            result.epoch,
                            result.token,
                            options.detach,
                            detached.clone(),
                        );
                        if options.redraw == Redraw::CtrlL {
                            send_input(&client, result.epoch, 1, b"\x0c")?;
                        }
                    }
                }
            }
            6 if message.payload.len() >= 16 => {
                io::stdout()
                    .write_all(&message.payload[16..])
                    .map_err(|e| e.to_string())?;
                io::stdout().flush().ok();
            }
            6 => {}
            10 | 4 | 11 | 14 | 17 => {}
            _ => {}
        }
        let _ = lease;
    }
}

fn input_thread(tx: Arc<Mutex<Tx>>, scope: u32, epoch: u32, token: [u8; 16], detach: Option<u8>, detached: Arc<AtomicBool>) {
    thread::spawn(move || {
        let mut input = io::stdin();
        let mut request = 1u64;
        let mut bytes = [0; 65536];
        while let Ok(n) = input.read(&mut bytes) {
            if n == 0 {
                break;
            }
            if let Some(at) = detach.and_then(|key| bytes[..n].iter().position(|b| *b == key)) {
                if at > 0 {
                    let _ = send_input_tx(&tx, scope, epoch, request, &bytes[..at]);
                }
                if let Ok(payload) = crate::wire::lease_token_payload(epoch, token) {
                    let _ = tx.lock().unwrap().send(scope, 0x17, &payload);
                }
                detached.store(true, Ordering::Release);
                let _ = tx.lock().unwrap().stream.shutdown(std::net::Shutdown::Both);
                break;
            }
            if send_input_tx(&tx, scope, epoch, request, &bytes[..n]).is_err() {
                break;
            }
            request += 1;
        }
    });
}
fn send_input(client: &Client, epoch: u32, request: u64, bytes: &[u8]) -> Result<()> {
    send_input_tx(&client.tx, client.generation, epoch, request, bytes)
}
fn send_input_tx(
    tx: &Arc<Mutex<Tx>>,
    scope: u32,
    epoch: u32,
    request: u64,
    bytes: &[u8],
) -> Result<()> {
    let mut payload = Vec::with_capacity(13 + bytes.len());
    payload.extend_from_slice(&epoch.to_le_bytes());
    payload.extend_from_slice(&request.to_le_bytes());
    payload.push(0);
    payload.extend_from_slice(bytes);
    tx.lock().unwrap().send(scope, 9, &payload)
}

fn push(path: &Path) -> Result<i32> {
    let mut client = Client::connect(path).map_err(|_| {
        format!(
            "session '{}' does not exist",
            name::render(path.as_os_str())
        )
    })?;
    let request = fresh(LeaseRole::InputOnly)
        .encode_wire()
        .map_err(|e| format!("protocol error: {e:?}"))?;
    client.send(0x15, &request)?;
    let result = loop {
        let message = client.rx.recv()?;
        if message.kind == 0x16 {
            break LeaseResult::decode_wire(&message.payload)
                .map_err(|_| "malformed lease result".to_string())?;
        }
    };
    if result.outcome != ResultOutcome::Granted {
        return Err("input lease is busy".into());
    }
    let mut bytes = [0; 65536];
    let mut request = 1;
    loop {
        let n = io::stdin().read(&mut bytes).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        send_input(&client, result.epoch, request, &bytes[..n])?;
        loop {
            if client.rx.recv()?.kind == 10 {
                break;
            }
        }
        request += 1;
    }
    let release = crate::wire::lease_token_payload(result.epoch, result.token)
        .map_err(|e| format!("protocol error: {e:?}"))?;
    client.send(0x17, &release)?;
    loop {
        if client.rx.recv()?.kind == 0x16 {
            break;
        }
    }
    Ok(0)
}

fn kill(session: &OsStr, path: &Path, force: bool, quiet: bool) -> Result<i32> {
    let mut client = Client::connect(path).map_err(|_| missing_or_stale(session, path))?;
    let mut payload = Vec::new();
    put_wide(&mut payload, &client.identity);
    payload.extend_from_slice(&client.generation.to_le_bytes());
    payload.extend_from_slice(&client.incarnation);
    payload.push(u8::from(force));
    client.send(15, &payload)?;
    if client.rx.recv()?.kind != 16 {
        return Err("termination failed".into());
    }
    for _ in 0..200 {
        if !path.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if path.exists() {
        return Err(format!("session '{}' did not stop", name::render(session)));
    }
    if !quiet {
        println!(
            "session '{}' {}",
            name::render(session),
            if force { "killed" } else { "stopped" }
        );
    }
    Ok(0)
}

fn tail(session: &OsStr, path: &Path, follow: bool, lines: u32, program: &str) -> Result<i32> {
    let logpath = companion(path, ".log");
    if !logpath.exists() {
        return Err(format!("no log for session '{}'", name::render(session)));
    }
    let mut cursor = 0;
    let mut first = true;
    loop {
        let (commit, body) = Store::read_only(&logpath, Kind::Log, 1)
            .map_err(|_| "log store is unavailable".to_string())?;
        if first {
            io::stdout()
                .write_all(last_lines(&body, lines))
                .map_err(|e| e.to_string())?;
            cursor = commit.end;
            first = false;
        } else {
            if cursor < commit.start {
                eprintln!(
                    "{program}: log gap: child-output bytes [{cursor},{}) were discarded",
                    commit.start
                );
                cursor = commit.start;
            }
            if cursor < commit.end {
                io::stdout()
                    .write_all(&body[(cursor - commit.start) as usize..])
                    .map_err(|e| e.to_string())?;
                io::stdout().flush().ok();
                cursor = commit.end;
            }
        }
        if !follow || !path.exists() {
            return Ok(0);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn clear(session: Option<&OsString>, invoked: &OsStr) -> Result<i32> {
    let owned;
    let session = if let Some(session) = session {
        session.as_os_str()
    } else {
        owned = current_paths(invoked)?
            .pop()
            .unwrap_or_default()
            .into_os_string();
        if owned.is_empty() {
            return Ok(0);
        }
        owned.as_os_str()
    };
    let path = resolve(session, invoked)?;
    let logpath = companion(&path, ".log");
    if !logpath.exists() {
        return Ok(0);
    }
    if let Ok(mut client) = Client::connect(&path) {
        let (selected, _) = Store::read_only(&logpath, Kind::Log, 1)
            .map_err(|_| "log store is unavailable".to_string())?;
        let payload = crate::wire::log_clear_payload(client.incarnation, selected.index)
            .map_err(|e| format!("protocol error: {e:?}"))?;
        client.send(0x19, &payload)?;
        if client.rx.recv()?.kind != 0x1a {
            return Err("log clear failed".into());
        }
    } else {
        let mut store = Store::open(&logpath, Kind::Log, 1)
            .map_err(|_| "log store is unavailable".to_string())?;
        let selected = store.selected().unwrap().clone();
        store
            .replace(b"", selected.epoch + 1, selected.end, selected.end)
            .map_err(|_| "log clear failed".to_string())?;
    }
    Ok(0)
}

#[derive(Clone, Copy, Eq, PartialEq)]
#[rustfmt::skip]
enum State { Missing, Live, Stale, Indeterminate }
#[rustfmt::skip]
fn classify(path: &Path) -> State {
    if !path.exists() { return if companion(path, ".exit").exists() { State::Stale } else { State::Missing }; }
    if !fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket()) { return State::Indeterminate; }
    match Client::connect(path) {
        Ok(_) => State::Live,
        Err(error) if error.contains("Connection refused") => State::Stale,
        Err(_) => State::Indeterminate,
    }
}

#[rustfmt::skip]
fn list(invoked: &OsStr, all: bool) -> Result<i32> {
    let root = root(invoked)?; let mut found = HashMap::new(); for entry in fs::read_dir(&root).map_err(|e| e.to_string())? { let entry = entry.map_err(|e| e.to_string())?; let raw = entry.file_name(); let bytes = raw.as_bytes(); let exit = bytes.strip_suffix(b".exit"); if exit.is_none() && [b".log".as_slice(), b".events", b".instrument"].iter().any(|suffix| bytes.ends_with(suffix)) { continue; } let name = exit.map_or_else(|| raw.clone(), |name| OsString::from_vec(name.to_vec())); found.entry(root.join(&name)).or_insert(name); }
    let (send, receive) = std::sync::mpsc::channel(); let mut names = Vec::new(); for (path, name) in found { let at = names.len(); let exited = !path.exists() && companion(&path, ".exit").exists(); names.push((name, path.clone(), State::Indeterminate, exited)); let sender = send.clone(); thread::spawn(move || { let _ = sender.send((at, classify(&path))); }); } drop(send); let deadline = Instant::now() + Duration::from_secs(2); while let Ok((at, state)) = receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) { names[at].2 = state; } names.retain(|(_, _, state, exited)| *state != State::Missing && (!*exited || all));
    names.sort_by_key(|(name, ..)| name.as_bytes().to_vec());
    if names.is_empty() { println!("(no sessions)"); return Ok(0); }
    for (name, path, state, exited) in names {
        let rendered = name::render(&name); let age_path = if exited { companion(&path, ".exit") } else { path }; let age = fs::metadata(age_path).ok().and_then(|m| m.modified().ok()).and_then(|t| SystemTime::now().duration_since(t).ok()).map_or("unknown".into(), |d| format!("{}s ago", d.as_secs()));
        let suffix = if exited { " [exited]" } else { match state { State::Live => "", State::Stale => " [stale]", State::Indeterminate => " [indeterminate]", State::Missing => unreachable!() } };
        println!("{rendered:<24} since {age}{suffix}");
    } Ok(0)
}

#[rustfmt::skip]
fn remove(session: Option<&OsString>, all: bool, quiet: bool, invoked: &OsStr) -> Result<i32> {
    if all {
        let root = root(invoked)?; let mut count = 0; for entry in fs::read_dir(root).map_err(|e| e.to_string())? { let path = entry.map_err(|e| e.to_string())?.path(); if classify(&path) == State::Stale && cleanup(&path).is_ok() { count += 1; } }
        if !quiet { if count == 0 { println!("nothing to remove") } else { println!("{count} session(s) removed") } } return Ok(0);
    }
    let session = session.unwrap(); let path = resolve(session, invoked)?;
    match classify(&path) {
        State::Live => Err(format!("session '{}' is running", name::render(session))),
        State::Indeterminate => Err(format!("session '{}' could not be identified", name::render(session))),
        State::Missing => Err(format!("session '{}' does not exist", name::render(session))),
        State::Stale => { cleanup(&path)?; if !quiet { println!("session '{}' removed", name::render(session)); } Ok(0) }
    }
}

#[rustfmt::skip]
fn current(invoked: &OsStr) -> Result<i32> {
    let paths = current_paths(invoked)?; if paths.is_empty() { return Ok(1); } println!("{}", paths.iter().map(|p| name::render(p.file_name().unwrap_or_default())).collect::<Vec<_>>().join(" > ")); Ok(0)
}
#[rustfmt::skip]
fn current_paths(invoked: &OsStr) -> Result<Vec<PathBuf>> {
    let legacy = std::env::var_os(env_key(invoked, "_SESSION")); let Some(encoded) = std::env::var_os(env_key(invoked, "_SESSION_V2")) else { return Ok(legacy.map_or_else(Vec::new, |value| value.as_bytes().split(|b| *b == b':').filter(|b| !b.is_empty()).map(|b| PathBuf::from(OsString::from_vec(b.to_vec()))).collect())); };
    let bytes = encoded.as_bytes(); if !bytes.starts_with(b"v2:") { return Err("session ancestry v2 is malformed".into()); } let mut native = Vec::new(); for item in bytes[3..].split(|b| *b == b':') { let value = STANDARD.decode(item).map_err(|_| "session ancestry v2 is malformed".to_string())?; if item.is_empty() || STANDARD.encode(&value).as_bytes() != item { return Err("session ancestry v2 is malformed".into()); } native.push(value); } if native.is_empty() { return Err("session ancestry v2 is malformed".into()); }
    let joined = native.iter().enumerate().flat_map(|(n, value)| [if n == 0 { &[][..] } else { b":" }, value.as_slice()]).flatten().copied().collect::<Vec<_>>(); if legacy.as_ref().is_some_and(|value| value.as_bytes() != joined) { return Err("session ancestry carriers disagree".into()); } Ok(native.into_iter().map(|value| PathBuf::from(OsString::from_vec(value))).collect())
}

#[rustfmt::skip]
fn status(core: &Core, id: u64) -> Vec<u8> {
    let mut out = Vec::new(); put_wide(&mut out, &core.identity); out.extend_from_slice(&1u32.to_le_bytes()); out.extend_from_slice(&core.incarnation); out.push(0); put_wide(&mut out, &[]); out.extend_from_slice(&[0xff]); out.extend_from_slice(&[0; 76]);
    let history = core.history.lock().unwrap(); let (first, last, start, end) = history.front().zip(history.back()).map_or((0, 0, 0, 0), |(a, b)| (a.sequence, b.sequence, a.offset, b.offset + b.bytes.len() as u64)); for value in [first, last, start, end] { out.extend_from_slice(&value.to_le_bytes()); }
    let owns = core.lease.lock().unwrap().touch_owner(id, now()).is_ok(); let event = core.events.lock().unwrap().is_some(); out.push(3 | (u8::from(owns) << 4) | (u8::from(!core.viewers.lock().unwrap().is_empty()) << 5) | (u8::from(core.running.load(Ordering::Acquire)) << 6) | (u8::from(event) << 7)); out.extend_from_slice(&0u32.to_le_bytes()); out.extend_from_slice(&[0; 3]);
    let log = core.log.lock().unwrap(); let (mut health, epoch, index, start, end) = log.as_ref().map_or((0, 0, 0, 0, 0), |(s, _)| { let c = s.selected().unwrap(); (1, c.epoch, c.index, c.start, c.end) }); health |= u8::from(core.lifecycle.lock().unwrap().is_some()) << 1 | u8::from(core.scanner.lock().unwrap().exact()) << 2; out.push(health); out.extend_from_slice(&epoch.to_le_bytes()); for value in [index, start, end] { out.extend_from_slice(&value.to_le_bytes()); } out
}

#[rustfmt::skip]
fn resolve(session: &OsStr, invoked: &OsStr) -> Result<PathBuf> { let path = PathBuf::from(session); if session.as_bytes().contains(&b'/') { absolute(&path) } else { Ok(root(invoked)?.join(path)) } }
#[rustfmt::skip]
fn root(invoked: &OsStr) -> Result<PathBuf> {
    let base = Path::new(invoked).file_name().filter(|s| !s.is_empty()).unwrap_or(OsStr::new("moor"));
    let mut name = OsString::from("."); name.push(base); name.push(format!("-{}", unsafe { libc::geteuid() })); let root = std::env::temp_dir().join(name);
    match fs::symlink_metadata(&root) {
        Ok(meta) if meta.file_type().is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o777 == 0o700 => {}
        Ok(_) => return Err(format!("session root '{}' is not owner-only", root.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::DirBuilder::new().recursive(false).mode(0o700).create(&root).map_err(|e| e.to_string())?,
        Err(error) => return Err(error.to_string()),
    } Ok(root)
}
#[rustfmt::skip]
fn absolute(path: &Path) -> Result<PathBuf> {
    let source = if path.is_absolute() { path.to_owned() } else { std::env::current_dir().map_err(|e| e.to_string())?.join(path) }; let mut out = PathBuf::from("/");
    for part in source.components() { match part { Component::Normal(p) => out.push(p), Component::ParentDir => { out.pop(); }, _ => {} } } Ok(out)
}
#[rustfmt::skip]
fn identity(path: &Path) -> Result<Vec<u8>> { let mut out = vec![1]; out.extend_from_slice(absolute(path)?.as_os_str().as_bytes()); Ok(out) }
#[rustfmt::skip]
fn companion(path: &Path, suffix: &str) -> PathBuf { let mut value = path.as_os_str().to_owned(); value.push(suffix); PathBuf::from(value) }
#[rustfmt::skip]
fn cleanup(path: &Path) -> Result<()> {
    let event = manifest_event(path); if path.exists() { let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?; if !meta.file_type().is_socket() || meta.uid() != unsafe { libc::geteuid() } { return Err("unowned rendezvous cannot be removed".into()); } fs::remove_file(path).map_err(|e| e.to_string())?; }
    for suffix in [".log", ".events", ".exit", ".instrument"] { let target = companion(path, suffix); if target.is_dir() { delete_store(&target)?; } else if target.exists() { fs::remove_file(target).map_err(|e| e.to_string())?; } } if let Some(target) = event.filter(|target| target != path && Store::read_only(target, Kind::Event, 1).is_ok()) { delete_store(&target)?; } Ok(())
}
#[rustfmt::skip]
fn manifest_event(path: &Path) -> Option<PathBuf> { let (_, body) = Store::read_only(&companion(path, ".exit"), Kind::Exit, 1).ok()?; let marker = b"\"event_path\":\""; let at = body.windows(marker.len()).position(|part| part == marker)? + marker.len(); let end = body[at..].iter().position(|byte| *byte == b'\"')? + at; let native = STANDARD.decode(&body[at..end]).ok()?; Some(PathBuf::from(OsString::from_vec(native))) }
#[rustfmt::skip]
fn delete_store(path: &Path) -> Result<()> { for slot in ["body.0", "body.1", "commit.0", "commit.1"] { let _ = fs::remove_file(path.join(slot)); } fs::remove_dir(path).map_err(|e| e.to_string()) }
#[rustfmt::skip]
fn missing_or_stale(session: &OsStr, path: &Path) -> String { if path.exists() || companion(path, ".exit").exists() { format!("session '{}' is not running", name::render(session)) } else { format!("session '{}' does not exist", name::render(session)) } }
#[rustfmt::skip]
fn fresh(role: LeaseRole) -> LeaseRequest { LeaseRequest { operation: LeaseOperation::Fresh, role, epoch: 0, incarnation: [0; 16], token: [0; 16] } }
#[rustfmt::skip]
fn hello(identity: &[u8]) -> Vec<u8> { let mut out = b"MOOR\x03\0\0".to_vec(); put_wide(&mut out, identity); out }
#[rustfmt::skip]
fn put_wide(out: &mut Vec<u8>, bytes: &[u8]) { out.extend_from_slice(&(bytes.len() as u32).to_le_bytes()); out.extend_from_slice(bytes); }
#[rustfmt::skip]
fn get_wide(bytes: &[u8], at: usize) -> Option<&[u8]> { let n = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize; bytes.get(at + 4..at + 4 + n).filter(|_| at + 4 + n == bytes.len()) }
#[rustfmt::skip]
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }
#[rustfmt::skip]
fn random16() -> Result<[u8; 16]> { let mut value = [0; 16]; File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut value)).map_err(|e| e.to_string())?; if value == [0; 16] { return Err("random source failed".into()); } Ok(value) }
#[rustfmt::skip]
fn ready_result(ready: Option<UnixStream>, status: u8, message: &str) { if let Some(mut stream) = ready { let bytes = message.as_bytes(); let _ = stream.write_all(&[&[status][..], &(bytes.len() as u16).to_le_bytes(), bytes].concat()); } }
#[rustfmt::skip]
fn pty() -> Result<(File, File)> { let (mut master, mut slave) = (-1, -1); if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) } < 0 { return Err(io::Error::last_os_error().to_string()); } Ok(unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }) }
#[rustfmt::skip]
fn open_stderr(path: &Path) -> Result<File> { let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?; if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o777 != 0o600 { return Err(format!("stderr sink '{}' is not a protected regular file", path.display())); } OpenOptions::new().append(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).map_err(|e| e.to_string()) }
#[rustfmt::skip]
struct Instrument { read: File, write: File, nonce: [u8; 16] }
#[rustfmt::skip]
fn instrument_setup(source: Option<&PathBuf>, session: &Path, process: &mut Command) -> Result<Option<Instrument>> {
    let Some(source) = source else { return Ok(None); }; let meta = fs::symlink_metadata(source).map_err(|e| e.to_string())?; let raw = source.as_os_str().as_bytes();
    #[cfg(target_os = "macos")] let bad_path = raw.contains(&b':'); #[cfg(not(target_os = "macos"))] let bad_path = raw.iter().any(|b| *b == b':' || *b == b'$' || b.is_ascii_whitespace());
    if !source.is_absolute() || !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 || bad_path { return Err("instrumentation object is not a protected loadable path".into()); }
    let stage = companion(session, ".instrument"); let mut input = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(source).map_err(|e| e.to_string())?; let mut output = OpenOptions::new().write(true).create_new(true).mode(0o500).open(&stage).map_err(|e| e.to_string())?; io::copy(&mut input, &mut output).map_err(|e| e.to_string())?; output.sync_all().map_err(|e| e.to_string())?;
    let mut fds = [-1; 2]; if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 { return Err(io::Error::last_os_error().to_string()); }
    unsafe { libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC); }
    let nonce = random16()?; let nonce_text = nonce.iter().map(|b| format!("{b:02x}")).collect::<String>(); process.env("DESK_MOOR_INSTRUMENT_CHANNEL", fds[1].to_string()).env("DESK_MOOR_INSTRUMENT_NONCE", nonce_text);
    #[cfg(target_os = "macos")] let (loader, separator) = ("DYLD_INSERT_LIBRARIES", ":"); #[cfg(not(target_os = "macos"))] let (loader, separator) = ("LD_PRELOAD", " ");
    let mut preload = stage.into_os_string(); if let Some(prior) = std::env::var_os(loader).filter(|v| !v.is_empty()) { preload.push(separator); preload.push(prior); } process.env(loader, preload);
    Ok(Some(Instrument { read: unsafe { File::from_raw_fd(fds[0]) }, write: unsafe { File::from_raw_fd(fds[1]) }, nonce }))
}
#[rustfmt::skip]
fn instrument_ack(instrument: Option<Instrument>, pid: u32) -> Result<()> {
    let Some(Instrument { mut read, write, nonce }) = instrument else { return Ok(()); }; drop(write); unsafe { libc::fcntl(read.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK); } let deadline = Instant::now() + Duration::from_secs(2); let mut bytes = Vec::new();
    loop { let remaining = deadline.saturating_duration_since(Instant::now()); if remaining.is_zero() { return Err("instrumentation acknowledgement timed out".into()); } let mut poll = libc::pollfd { fd: read.as_raw_fd(), events: libc::POLLIN | libc::POLLHUP, revents: 0 }; if unsafe { libc::poll(&mut poll, 1, remaining.as_millis().min(i32::MAX as u128) as i32) } <= 0 { continue; } let mut part = [0; 37]; match read.read(&mut part) { Ok(0) => break, Ok(n) => { bytes.extend_from_slice(&part[..n]); if bytes.len() > 36 { break; } }, Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}, Err(e) => return Err(e.to_string()) } }
    if bytes.len() != 36 || &bytes[..8] != b"MOORINS3" || bytes[8] != 1 || bytes[9..12] != [0; 3] || u32::from_le_bytes(bytes[12..16].try_into().unwrap()) != 1 || u32::from_le_bytes(bytes[16..20].try_into().unwrap()) != pid || bytes[20..] != nonce { return Err("instrumentation acknowledgement was invalid".into()); } Ok(())
}
#[rustfmt::skip]
fn lifecycle_running(config: &Config, identity: &[u8], incarnation: [u8; 16]) -> Result<String> { let path = |value: Option<PathBuf>| -> Result<String> { Ok(value.map(|path| format!("\"{}\"", STANDARD.encode(absolute(&path).unwrap().as_os_str().as_bytes()))).unwrap_or_else(|| "null".into())) }; let event_path = path(config.options.events.clone())?; let instrument_path = path(config.options.instrument.as_ref().map(|_| companion(&config.path, ".instrument")))?; let mut clock: libc::timespec = unsafe { std::mem::zeroed() }; if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut clock) } != 0 { return Err(io::Error::last_os_error().to_string()); } let mono = clock.tv_sec as u64 * 1000 + clock.tv_nsec as u64 / 1_000_000; let boot = boot_id()?; Ok(format!("{{\"v\":1,\"type\":\"lifecycle\",\"phase\":\"running\",\"session\":\"{}\",\"generation\":null,\"wire_generation\":1,\"incarnation\":\"{}\",\"start_wall_ms\":\"{}\",\"start_mono_ms\":\"{}\",\"boot_id\":\"{}\",\"path_encoding\":\"posix-bytes\",\"event_path\":{},\"instrument_path\":{}}}\n", STANDARD.encode(identity), STANDARD.encode(incarnation), now(), mono, STANDARD.encode(boot), event_path, instrument_path)) }
#[rustfmt::skip]
fn boot_id() -> Result<[u8; 16]> { if let Ok(text) = fs::read("/proc/sys/kernel/random/boot_id") { let hex = text.into_iter().filter(|b| b.is_ascii_hexdigit()).collect::<Vec<_>>(); if hex.len() == 32 { let mut out = [0; 16]; for n in 0..16 { out[n] = (char::from(hex[n * 2]).to_digit(16).unwrap() * 16 + char::from(hex[n * 2 + 1]).to_digit(16).unwrap()) as u8; } return Ok(out); } } random16() }
#[rustfmt::skip]
fn child_environment(invoked: &OsStr, path: &Path) -> Result<()> { let mut paths = current_paths(invoked)?; paths.push(absolute(path)?); let legacy = paths.iter().enumerate().flat_map(|(n, path)| [if n == 0 { &[][..] } else { b":" }, path.as_os_str().as_bytes()]).flatten().copied().collect::<Vec<_>>(); let v2 = format!("v2:{}", paths.iter().map(|path| STANDARD.encode(path.as_os_str().as_bytes())).collect::<Vec<_>>().join(":")); unsafe { std::env::set_var(env_key(invoked, "_SESSION"), OsString::from_vec(legacy)); std::env::set_var(env_key(invoked, "_SESSION_V2"), v2); } Ok(()) }
#[rustfmt::skip]
fn env_key(invoked: &OsStr, suffix: &str) -> OsString { let raw = Path::new(invoked).file_name().unwrap_or(OsStr::new("moor")).as_bytes(); let cap = 127 - suffix.len(); let mut out: Vec<u8> = raw.iter().take(cap).map(|b| if b.is_ascii_alphanumeric() { b.to_ascii_uppercase() } else { b'_' }).collect(); out.extend_from_slice(suffix.as_bytes()); OsString::from_vec(out) }
#[rustfmt::skip]
fn last_lines(bytes: &[u8], lines: u32) -> &[u8] { if lines == 0 { return &[]; } let mut remaining = lines; let mut at = bytes.len().saturating_sub(usize::from(bytes.last() == Some(&b'\n'))); while at > 0 { at -= 1; if bytes[at] == b'\n' { remaining -= 1; if remaining == 0 { return &bytes[at + 1..]; } } } bytes }
#[rustfmt::skip]
fn shell_status(status: ExitStatus) -> i32 { use std::os::unix::process::ExitStatusExt; status.code().unwrap_or_else(|| if status.signal().is_some() { 1 } else { 125 }) }
#[rustfmt::skip]
fn lifecycle_exit(core: &Core, status: ExitStatus) -> bool { use std::os::unix::process::ExitStatusExt; let end = core.history.lock().unwrap().back().map_or(0, |record| record.offset + record.bytes.len() as u64); let outcome = status.code().map_or_else(|| format!("\"ended\":\"signalled\",\"signal\":{}", status.signal().unwrap_or(0)), |code| format!("\"ended\":\"exited\",\"code\":{code}")); let mut lifecycle = core.lifecycle.lock().unwrap(); let Some(state) = lifecycle.as_mut() else { return false }; let base = state.running.strip_suffix("}\n").unwrap().replacen("\"phase\":\"running\"", "\"phase\":\"exited\"", 1); let body = format!("{base},\"end_wall_ms\":\"{}\",\"output_end\":\"{end}\",{outcome}}}\n", now()); if state.store.replace(body.as_bytes(), 1, end, end).is_err() { return false } drop(lifecycle); let mut events = core.events.lock().unwrap(); if events.as_mut().is_some_and(|writer| !writer.exit(status)) { *events = None; } true }

#[cfg(any(target_os = "linux", target_os = "android"))]
#[rustfmt::skip]
fn peer_uid(stream: &UnixStream) -> Option<u32> { let mut cred: libc::ucred = unsafe { std::mem::zeroed() }; let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t; let ok = unsafe { libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, &mut cred as *mut _ as *mut _, &mut len) }; (ok == 0).then_some(cred.uid) }
#[cfg(not(any(target_os = "linux", target_os = "android")))]
#[rustfmt::skip]
fn peer_uid(stream: &UnixStream) -> Option<u32> { let (mut uid, mut gid) = (0, 0); let ok = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) }; (ok == 0).then_some(uid) }
