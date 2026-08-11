use super::io::{Duplex, pump};
use super::private::{
    SessionState, clear_store, companion, list_sessions_at, monotonic, print_current, remove_all,
    tail,
};
use crate::cli::{Action, CreateMode};
use crate::name;
use crate::session::{
    LeaseOperation, LeaseRequest, LeaseResult, LeaseRole, ResultOutcome, ResultReason,
};
use crate::store::{Kind, Store};
#[cfg(unix)]
use crate::unix as platform;
#[cfg(windows)]
use crate::windows as platform;
use crate::wire::{
    Codec, InputReceipt, Message, Profile, StatusTail, controller_hello,
    decode_controller_hello_ack, decode_error_payload, decode_log_clear_result,
    decode_terminate_result, input_payload, resize_payload, terminate_request_payload,
};
use interprocess::TryClone;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub type Result<T> = std::result::Result<T, String>;
pub type CommandResult<T> = std::result::Result<T, CommandError>;

pub struct CommandError(String, bool);

impl CommandError {
    pub fn output(message: impl Into<String>) -> Self {
        Self(message.into(), true)
    }
    pub fn report(self, program: &str) -> i32 {
        match self.1 {
            true => println!("{program}: {}", self.0),
            false => eprintln!("{program}: {}", self.0),
        }
        1
    }
}
impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self(message, false)
    }
}

fn session_error(session: &OsStr, phrase: &str) -> CommandError {
    CommandError::output(format!("session '{}' {phrase}", name::render(session)))
}

pub(crate) fn missing(path: &Path) -> CommandError {
    session_error(path.as_os_str(), "does not exist")
}

fn announce(session: &OsStr, quiet: bool, verb: &str) {
    if !quiet {
        println!("session '{}' {verb}", name::render(session));
    }
}

pub struct Client {
    pub(crate) transport: Duplex<Inbound>,
    outgoing: Codec,
    pub generation: u32,
    pub incarnation: [u8; 16],
    pub identity: Vec<u8>,
}

schema!(struct pub(crate) InputLease pub fields; role: LeaseRole, epoch: u32, token: [u8; 16], next: Option<u64>, pending: Vec<u8>);

pub(crate) type Inbound = std::result::Result<Message, (String, bool)>;

fn accepted(message: Message) -> Inbound {
    if message.kind != 0x13 {
        return Ok(message);
    }
    let refusal = decode_error_payload(&message.payload)
        .filter(|(_, text)| !text.is_empty())
        .map_or_else(
            || "invalid holder refusal".into(),
            |(code, text)| {
                format!(
                    "holder refused request ({code}): {}",
                    String::from_utf8_lossy(text)
                )
            },
        );
    Err((refusal, false))
}

impl Client {
    pub fn from_stream<T>(
        stream: T,
        identity: Vec<u8>,
        deadline: Instant,
        cancel: fn(&T),
    ) -> Result<Self>
    where
        T: Read + Write + TryClone + Send + 'static,
    {
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        let closer = stream.try_clone().map_err(|error| error.to_string())?;
        Self::handshake_until(stream, writer, identity, deadline, move || cancel(&closer))
    }

    pub fn handshake_until(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        identity: Vec<u8>,
        deadline: Instant,
        close: impl FnOnce() + Send + 'static,
    ) -> Result<Self> {
        let mut incoming = Codec::new(Profile::Controller);
        let mut messages = Vec::with_capacity(8);
        let transport = pump(
            reader,
            writer,
            4 << 20,
            16 << 20,
            move |bytes, events| {
                messages.clear();
                let failure = incoming
                    .feed(monotonic(), bytes, &mut messages)
                    .err()
                    .map(crate::protocol);
                if messages
                    .drain(..)
                    .map(accepted)
                    .any(|message| events.send(message).is_err())
                {
                    return false;
                }
                failure.is_none_or(|error| {
                    let _ = events.send(Err((error, false)));
                    false
                })
            },
            Err(("connection closed".into(), true)),
            close,
        );
        let mut client = Self {
            transport,
            outgoing: Codec::new(Profile::Controller),
            generation: 0,
            incarnation: [0; 16],
            identity,
        };
        let hello = controller_hello(&client.identity).map_err(crate::protocol)?;
        client.send(1, &hello)?;
        let reply = client
            .transport
            .1
            .recv_deadline(deadline)
            .map_err(|error| match error {
                crossbeam_channel::RecvTimeoutError::Timeout => {
                    "holder identity exchange timed out".to_string()
                }
                crossbeam_channel::RecvTimeoutError::Disconnected => "connection closed".into(),
            })
            .and_then(|event| event.map_err(|failure| failure.0))
            .inspect_err(|_| client.cancel())?;
        let (generation, incarnation) = (reply.kind == 2)
            .then(|| decode_controller_hello_ack(reply.scope, &reply.payload, &client.identity))
            .flatten()
            .ok_or("holder identity exchange failed")?;
        (client.generation, client.incarnation) = (generation, incarnation);
        Ok(client)
    }

    pub fn recv(&mut self) -> Result<Message> {
        self.transport
            .1
            .recv()
            .map_err(|_| "connection closed".to_string())?
            .map_err(|failure| failure.0)
    }

    pub fn send(&mut self, kind: u8, payload: &[u8]) -> Result<()> {
        let mut output = Vec::new();
        self.outgoing
            .encode(self.generation, kind, payload, &mut output)
            .map_err(crate::protocol)?;
        self.transport
            .try_send(output)
            .map_err(|_| "connection closed".to_string())?;
        matches!(self.transport.2.recv(), Ok((0, _, None)))
            .then_some(())
            .ok_or_else(|| "connection write failed".into())
    }

    pub(crate) fn cancel(&self) {
        self.transport.shutdown();
    }

    pub fn receive_kind(&mut self, kind: u8) -> Result<Message> {
        loop {
            let message = self.recv()?;
            if message.kind == kind {
                return Ok(message);
            }
        }
    }

    pub fn request(&mut self, kind: u8, payload: &[u8], reply: u8) -> Result<Message> {
        self.send(kind, payload)?;
        self.receive_kind(reply)
    }

    pub fn attached(&mut self) -> bool {
        self.request(0x0d, &[], 0x0e)
            .ok()
            .and_then(|message| {
                StatusTail::decode_for(
                    &message.payload,
                    &self.identity,
                    self.generation,
                    self.incarnation,
                )
                .ok()
            })
            .is_some_and(|status| status.viewers)
    }

    pub fn terminate(&mut self, force: bool) -> Result<(u8, u8, u8, Vec<u8>)> {
        let payload =
            terminate_request_payload(&self.identity, self.generation, self.incarnation, force)
                .map_err(crate::protocol)?;
        self.send(15, &payload)?;
        let result = self.receive_kind(16)?;
        let (outcome, containment, method, diagnostic) =
            decode_terminate_result(&result.payload).map_err(crate::protocol)?;
        Ok((outcome, containment, method, diagnostic.to_vec()))
    }

    fn lease(&mut self, request: &LeaseRequest) -> Result<LeaseResult> {
        let result = self.request(0x15, &request.encode_wire().map_err(crate::protocol)?, 0x16)?;
        LeaseResult::decode_wire(&result.payload).map_err(crate::protocol)
    }

    pub fn push_from(
        mut self,
        mut input: impl Read + Send + 'static,
        mut reconnect: impl FnMut(Duration) -> Result<Self>,
    ) -> Result<i32> {
        let mut lease = InputLease::from_result(
            self.lease(&LeaseRequest::fresh(LeaseRole::InputOnly))?,
            LeaseRole::InputOnly,
        )?
        .ok_or("input lease is busy")?;
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            loop {
                let mut bytes = vec![0; 65536];
                let read = input.read(&mut bytes).map(|count| {
                    bytes.truncate(count);
                    bytes
                });
                let done = read.as_ref().is_ok_and(Vec::is_empty);
                if send.send(read.map_err(|error| error.to_string())).is_err() || done {
                    break;
                }
            }
        });
        loop {
            let bytes = match receive.recv_timeout(Duration::from_secs(3)) {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(error)) => return Err(error),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("input reader failed".into());
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if lease.control(&mut self, 0x18).is_err() {
                        lease.resume(&mut self, &mut reconnect)?;
                    }
                    continue;
                }
            };
            if bytes.is_empty() {
                break;
            }
            lease.stage(bytes);
            loop {
                if lease.replay(&mut self).is_ok()
                    && let Ok(receipt) = self.receive_kind(10)
                {
                    let receipt =
                        InputReceipt::decode(&receipt.payload).map_err(crate::protocol)?;
                    lease.receipt(receipt, &self)?;
                    break;
                }
                lease.resume(&mut self, &mut reconnect)?;
            }
        }
        lease.control(&mut self, 0x17)?;
        let released =
            LeaseResult::decode_wire(&self.receive_kind(0x16)?.payload).map_err(crate::protocol)?;
        crate::require(
            released.outcome == ResultOutcome::Released,
            "input lease release failed",
        )?;
        Ok(0)
    }

    pub fn clear_log(&mut self, log: &Path) -> Result<()> {
        let (selected, _) =
            Store::read_only(log, Kind::Log, None).map_err(|_| "log store is unavailable")?;
        let result = self.request(
            0x19,
            &crate::wire::log_clear_payload(self.incarnation, selected.index)
                .map_err(crate::protocol)?,
            0x1a,
        )?;
        decode_clear_result(&result.payload, selected.index)
    }
}

impl InputLease {
    pub(crate) fn from_result(result: LeaseResult, role: LeaseRole) -> Result<Option<Self>> {
        crate::require(result.role == role, "unexpected input lease role")?;
        Ok(matches!(
            result.outcome,
            ResultOutcome::Granted | ResultOutcome::Resumed
        )
        .then_some(Self {
            role: result.role,
            epoch: result.epoch,
            token: result.token,
            next: Some(1),
            pending: Vec::new(),
        }))
    }

    pub(crate) fn pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(crate) fn stage(&mut self, bytes: Vec<u8>) {
        debug_assert!(!bytes.is_empty() && self.pending.is_empty());
        self.pending = bytes;
    }

    pub(crate) fn replay(&self, client: &mut Client) -> Result<()> {
        crate::return_if!(!self.pending(), Ok(()));
        let request = self.next.ok_or("input request space exhausted")?;
        client.send(9, &input_payload(self.epoch, request, &self.pending))
    }

    pub(crate) fn resize(&self, client: &mut Client, rows: u16, columns: u16) -> Result<()> {
        client.send(0x0b, &resize_payload(self.epoch, rows, columns))
    }

    pub(crate) fn control(&self, client: &mut Client, kind: u8) -> Result<()> {
        client.send(
            kind,
            &crate::wire::lease_token_payload(self.epoch, self.token).map_err(crate::protocol)?,
        )
    }

    pub(crate) fn receipt(&mut self, receipt: InputReceipt, client: &Client) -> Result<()> {
        crate::require(self.pending(), "unexpected input receipt")?;
        let expected = InputReceipt::outcome(
            self.epoch,
            self.next.ok_or("input request space exhausted")?,
            client.generation,
            client.incarnation,
            self.pending.len() as u64,
            None,
        );
        crate::require(receipt == expected, "input was not delivered")?;
        self.pending.clear();
        self.next = expected.request.checked_add(1);
        Ok(())
    }

    pub(crate) fn resume(
        &mut self,
        client: &mut Client,
        reconnect: &mut impl FnMut(Duration) -> Result<Client>,
    ) -> Result<()> {
        let (generation, incarnation) = (client.generation, client.incarnation);
        let deadline = Instant::now() + Duration::from_secs(2);
        while let Some(remaining) = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
        {
            if let Ok(mut candidate) = reconnect(remaining) {
                crate::require(
                    candidate.generation == generation
                        && candidate.incarnation == incarnation
                        && candidate.identity == client.identity,
                    "session changed while input was pending",
                )?;
                let request = LeaseRequest {
                    operation: LeaseOperation::Resume,
                    role: self.role,
                    epoch: self.epoch,
                    incarnation,
                    token: self.token,
                };
                let result = candidate.lease(&request)?;
                if result.outcome == ResultOutcome::Resumed
                    && result.role == self.role
                    && result.epoch == self.epoch
                {
                    self.token = result.token;
                    *client = candidate;
                    return Ok(());
                }
                if result.reason != ResultReason::Busy {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err("input lease could not be resumed".into())
    }
}

pub fn decode_clear_result(payload: &[u8], prior: u64) -> Result<()> {
    let (outcome, reason, observed) =
        decode_log_clear_result(payload).map_err(|_| "invalid log clear result")?;
    crate::require(
        observed == prior,
        "log clear result did not match the request",
    )?;
    match (outcome, reason) {
        (0 | 1, 0) => Ok(()),
        (2, 1) => Err("log changed before it could be cleared".into()),
        (2, 2) => Err("log store is unavailable".into()),
        (2, 3) => Err("log store is corrupt".into()),
        _ => Err("invalid log clear result".into()),
    }
}

pub fn probe_session(
    path: &Path,
    status: bool,
    valid: impl FnOnce() -> bool,
    connect: impl FnOnce() -> std::result::Result<Client, bool>,
) -> SessionState {
    if let Err(error) = std::fs::symlink_metadata(path) {
        return if error.kind() != std::io::ErrorKind::NotFound {
            SessionState::Indeterminate
        } else if companion(path, ".exit").exists() {
            SessionState::Exited
        } else {
            SessionState::Missing
        };
    }
    if !valid() {
        return SessionState::Indeterminate;
    }
    match connect() {
        Ok(mut client) => {
            if status && client.attached() {
                SessionState::Attached
            } else {
                SessionState::Live
            }
        }
        Err(true) => SessionState::Stale,
        Err(false) => SessionState::Indeterminate,
    }
}

crate::schema!(enum Decision [Clone, Copy, Debug]; Proceed, Attach, Cleanup, Offline, Missing, Stopped, Running, Already, Identify);
use Decision::*;

// Indexed by SessionState ordinal. Both stale residue shapes — an orphaned
// rendezvous and an exit-record-only remnant — are the one stale liveness state
// of §2.3, so §13.3's state-keyed message applies to both.
const ATTACH_POLICY: [Decision; 6] = [Missing, Proceed, Proceed, Stopped, Stopped, Identify];
const CLEAR_POLICY: [Decision; 6] = [Missing, Proceed, Proceed, Offline, Offline, Identify];
const REMOVE_POLICY: [Decision; 6] = [Missing, Running, Running, Cleanup, Cleanup, Identify];
const UNAVAILABLE_POLICY: [Decision; 6] = [Missing, Identify, Identify, Stopped, Stopped, Identify];

fn decide(session: &OsStr, path: &Path, table: [Decision; 6]) -> CommandResult<Decision> {
    match table[platform::classify(path) as usize] {
        Decision::Missing => Err(session_error(session, "does not exist")),
        Decision::Stopped => Err(session_error(session, "is not running")),
        Decision::Running => Err(session_error(session, "is running")),
        Decision::Already => Err(session_error(session, "is already running")),
        Decision::Identify => Err(session_error(session, "could not be identified")),
        decision => Ok(decision),
    }
}

fn unavailable(session: &OsStr, path: &Path) -> CommandError {
    decide(session, path, UNAVAILABLE_POLICY).unwrap_err()
}

/// Internal binary dispatch. Creation assumes the process has not started any
/// application threads; callers must invoke the shipped executable rather than
/// embedding this entry point in a multithreaded host.
#[doc(hidden)]
pub fn execute_commands(action: Action, program: &str, invoked: &OsStr) -> CommandResult<i32> {
    let resolve = |session: &OsStr| platform::resolve(session, invoked);
    // §13.3 freezes three states and three messages, so the diagnostic has to
    // come from the classification and not from mere path existence: an
    // indeterminate listener that accepts and then fails the identity exchange
    // is not a stale session, and the caller's next move differs (investigate
    // versus clean up).
    match action {
        Action::Create {
            mode,
            session,
            command,
            options,
        } => {
            let path = platform::preflight_create(&options, &session, invoked)?;
            let (live, attach_after, verb) = match mode {
                CreateMode::Bare => (Attach, true, Some("created")),
                CreateMode::New => (Already, true, Some("created")),
                CreateMode::LegacyA => (Attach, true, None),
                CreateMode::LegacyC => (Already, true, None),
                CreateMode::Start => (Already, false, Some("started")),
                _ => (Already, false, None),
            };
            match decide(
                &session,
                &path,
                [Proceed, live, live, Cleanup, Cleanup, Identify],
            )? {
                Attach => return platform::attach(&path, options),
                Cleanup => platform::cleanup(&path)?,
                _ => {}
            }
            let status = platform::create(mode, &path, command, &options, invoked)?;
            crate::return_if!(
                status != 0 || matches!(mode, CreateMode::Run | CreateMode::LegacyRun),
                Ok(status)
            );
            if let Some(verb) = verb {
                announce(&session, options.quiet, verb);
            }
            if attach_after {
                platform::attach(&path, options)
            } else {
                Ok(0)
            }
        }
        Action::Attach { session, options } => {
            let path = resolve(&session)?;
            decide(&session, &path, ATTACH_POLICY)?;
            platform::attach(&path, options)
        }
        Action::Push(session) => {
            let path = resolve(&session)?;
            platform::connect(&path)
                .map_err(|_| unavailable(&session, &path))?
                .push_from(std::io::stdin(), |_| platform::connect(&path))
                .map_err(CommandError::output)
        }
        Action::Kill {
            session,
            force,
            quiet,
        } => {
            let path = resolve(&session)?;
            let mut client = platform::connect(&path).map_err(|_| unavailable(&session, &path))?;
            let (outcome, _, _, diagnostic) =
                client.terminate(force).map_err(CommandError::output)?;
            crate::return_if!(outcome == 1, Err(session_error(&session, "is not running")));
            crate::return_if!(
                outcome != 0,
                Err(CommandError::output(
                    String::from_utf8_lossy(&diagnostic).into_owned()
                ))
            );
            announce(&session, quiet, if force { "killed" } else { "stopped" });
            Ok(0)
        }
        Action::Tail {
            session,
            follow,
            lines,
        } => {
            let path = resolve(&session)?;
            crate::return_if!(
                !companion(&path, ".log").exists(),
                Err(CommandError::output(format!(
                    "no log for session '{}'",
                    name::render(&session)
                )))
            );
            Ok(tail(&path, follow, lines, program)?)
        }
        Action::Clear(session) => {
            let session = match session {
                Some(session) => Some(session),
                None => platform::current_paths(invoked)?
                    .pop()
                    .map(PathBuf::into_os_string),
            };
            let Some(session) = session.filter(|session| !session.is_empty()) else {
                return Ok(0);
            };
            let path = resolve(&session)?;
            let log = companion(&path, ".log");
            crate::return_if!(!log.exists(), Ok(0));
            match decide(&session, &path, CLEAR_POLICY)? {
                Proceed => platform::connect(&path)
                    .map_err(|_| session_error(&session, "could not be identified"))?
                    .clear_log(&log)
                    .map_err(CommandError::output)?,
                Offline => clear_store(&log)?,
                _ => unreachable!(),
            }
            Ok(0)
        }
        Action::Current => Ok(print_current(&platform::current_paths(invoked)?)),
        Action::List { all } => Ok(list_sessions_at(
            platform::sessions(invoked, true)?,
            all,
            platform::clock()?,
        )),
        Action::Remove {
            session,
            all,
            quiet,
        } => {
            crate::return_if!(
                all,
                Ok(remove_all(
                    platform::sessions(invoked, false)?,
                    quiet,
                    platform::cleanup
                )?)
            );
            let session = session.unwrap();
            let path = resolve(&session)?;
            decide(&session, &path, REMOVE_POLICY)?;
            platform::cleanup(&path)?;
            announce(&session, quiet, "removed");
            Ok(0)
        }
        Action::Help | Action::Version => unreachable!(),
    }
}

/// Internal entry point for `src/main.rs`; not a supported library interface.
#[doc(hidden)]
pub fn run(action: Action, invoked: &OsStr, program: &str) -> i32 {
    execute_commands(action, program, invoked).unwrap_or_else(|error| error.report(program))
}
