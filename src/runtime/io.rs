use crate::cli::{Options, Redraw, Reset};
use crate::runtime::client::{Client, InputLease};
#[allow(unused_imports)]
use crate::schema;
use crate::session::{LeaseRole, ResultOutcome};
use crate::wire::{Message, ViewerEvent, ViewerStream, decode_viewer};
use crossbeam_channel::{Receiver, Sender, bounded, never, select, unbounded};
use interprocess::TryClone;
use std::io::{self, Read, Write};
use std::sync::mpsc::{Sender as SyncSender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

schema!(enum pub Event [Debug, Eq, PartialEq]; Bytes(Vec<u8>), Closed);
schema!(enum pub SendError [Clone, Copy, Debug, Eq, PartialEq]; Full, Closed);
schema!(enum pub InputState [Clone, Copy, Debug, Eq, PartialEq]; Ready, Pending, Closed);

schema!(struct pub InputConfig pub fields; detach: Option<u8>, pass_suspend: bool, last_size: Option<(u16, u16)>);

type Charge = (usize, usize);
pub type SendResult = Result<(), SendError>;
pub type WriteResult = (u64, u64, Option<u16>);
type WriteJob = (Vec<u8>, Charge, u64);
schema!(struct State fields; sender: Option<SyncSender<WriteJob>>, used: Charge, limit: Charge);
type SharedState = Arc<Mutex<State>>;

pub struct Duplex<T = Event>(SharedState, pub Receiver<T>, pub Receiver<WriteResult>);

schema!(enum Command; Input(Vec<u8>), Resize(u16, u16), Keepalive, Release(SyncSender<bool>), Abort);
schema!(tuple pub ViewerSender [Clone]; fields; Sender<Command>);
schema!(enum ViewerPhase<'a>; Starting(&'a mut dyn FnMut(ViewerSender)), Attached, Reattaching);

impl ViewerSender {
    fn flush(&self, bytes: &mut Vec<u8>) -> bool {
        let input = std::mem::take(bytes);
        input.is_empty() || self.0.send(Command::Input(input)).is_ok()
    }
    pub fn release(&self) -> bool {
        let (send, receive) = channel();
        self.0.send(Command::Release(send)).is_ok() && receive.recv().unwrap_or(false)
    }
}

schema!(struct Viewer<'a> fields; client: &'a mut Client, options: &'a Options, output: &'a mut dyn Write,
    phase: ViewerPhase<'a>, commands: Receiver<Command>, sender: Sender<Command>, wire: ViewerStream,
    lease: Option<InputLease>, size: Option<(u16, u16)>, release: Option<SyncSender<bool>>);

impl Viewer<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.output
            .write_all(bytes)
            .map_err(|error| error.to_string())
    }

    fn accept(&mut self, message: &Message) -> Result<bool, String> {
        self.wire.lease_epoch = self.lease.as_ref().map(|lease| lease.epoch);
        let event = decode_viewer(
            &mut self.wire,
            message,
            (
                &self.client.identity,
                self.client.generation,
                self.client.incarnation,
            ),
        )
        .map_err(crate::protocol)?;
        if message.kind == 4 && matches!(self.phase, ViewerPhase::Reattaching) {
            self.phase = ViewerPhase::Attached;
            if self.options.redraw == Redraw::Winch {
                let (rows, columns) = self.size.unwrap();
                self.command(Command::Resize(rows, columns))?;
            }
            self.advance(vec![])?;
        }
        let Some(event) = event else {
            return Ok(false);
        };
        match event {
            ViewerEvent::Terminal(bytes) => {
                self.write(bytes)?;
                // Closure §6.3: `move` follows TERMINAL_STATE unconditionally;
                // an empty preamble (inexact tracking) is a legal branch, not
                // an implicit downgrade to `none`.
                if self.options.reset == Reset::Move {
                    self.write(b"\x1b[H")?;
                }
            }
            ViewerEvent::Output(sequence, apply, bytes) => {
                if apply {
                    self.write(bytes)?;
                    self.output.flush().ok();
                }
                self.client.send(7, &sequence.to_le_bytes())?;
            }
            ViewerEvent::Receipt(receipt) => {
                self.lease
                    .as_mut()
                    .ok_or("viewer lease lost")?
                    .receipt(receipt, self.client)?;
                self.advance(vec![])?;
            }
            ViewerEvent::Lease(result) => {
                let phase = std::mem::replace(&mut self.phase, ViewerPhase::Attached);
                if let ViewerPhase::Starting(start) = phase {
                    self.lease = InputLease::from_result(result, LeaseRole::Viewer)?;
                    start(ViewerSender(self.sender.clone()));
                    if self.lease.is_some() {
                        match self.options.redraw {
                            Redraw::CtrlL => self.advance(b"\x0c".to_vec())?,
                            Redraw::Winch => {
                                let (rows, columns) = self.size.unwrap();
                                self.command(Command::Resize(rows, columns))?;
                            }
                            Redraw::None => {}
                        }
                    }
                } else {
                    let done = self
                        .release
                        .take()
                        .ok_or("unexpected viewer lease result")?;
                    let success = result.outcome == ResultOutcome::Released
                        && result.role == LeaseRole::Viewer;
                    let _ = done.send(success);
                    crate::require(success, "viewer lease release failed")?;
                    self.lease = None;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn advance(&mut self, input: Vec<u8>) -> Result<(), String> {
        let lease = self.lease.as_mut().ok_or("viewer lease lost")?;
        if !input.is_empty() {
            lease.stage(input);
        }
        if self.release.is_some() && !lease.pending() {
            lease.control(self.client, 0x17)
        } else {
            lease.replay(self.client)
        }
    }

    fn command(&mut self, command: Command) -> Result<bool, String> {
        match command {
            Command::Input(bytes) => {
                let (ordinary, replies) = self.wire.input(bytes);
                for query in replies {
                    self.client
                        .send(12, &query.encode().map_err(crate::protocol)?)?;
                }
                self.advance(ordinary)?;
            }
            Command::Resize(rows, columns) => {
                self.size = Some((rows, columns));
                if let Some(lease) = &self.lease {
                    lease.resize(self.client, rows, columns)?;
                }
            }
            Command::Keepalive => {
                if let Some(lease) = &self.lease {
                    lease.control(self.client, 0x18)?;
                }
            }
            Command::Release(done) => {
                let ordinary = self.wire.flush_input();
                if self.lease.is_none() {
                    let _ = done.send(true);
                    self.client.cancel();
                    return Ok(true);
                }
                self.release = Some(done);
                self.advance(ordinary)?;
            }
            Command::Abort => unreachable!(),
        }
        Ok(false)
    }

    fn fail<T>(&mut self, error: impl Into<String>) -> Result<T, String> {
        let _ = self.release.take().map(|done| done.send(false));
        Err(error.into())
    }
}

impl Duplex {
    pub fn socket<T, P>(stream: T, preface: P, cancel: fn(&T)) -> io::Result<Self>
    where
        T: Read + Write + TryClone + Send + 'static,
        P: AsRef<[u8]> + Send + 'static,
    {
        let writer = stream.try_clone()?;
        let closer = stream.try_clone()?;
        Ok(Self::closing(
            io::Cursor::new(preface).chain(stream),
            writer,
            1 << 20,
            move || cancel(&closer),
        ))
    }

    pub fn closing(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        limit: usize,
        close: impl FnOnce() + Send + 'static,
    ) -> Self {
        let transport = raw(reader, writer, limit, 4 << 20, close);
        Duplex(transport.0, transport.1, never())
    }

    pub fn tracked(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        limit: usize,
    ) -> Self {
        raw(reader, writer, limit, 16 << 20, || {})
    }
}

fn raw(
    reader: impl Read + Send + 'static,
    writer: impl Write + Send + 'static,
    limit: usize,
    payload_limit: usize,
    close: impl FnOnce() + Send + 'static,
) -> Duplex {
    pump(
        reader,
        writer,
        limit,
        payload_limit,
        |bytes, events| events.send(Event::Bytes(bytes.to_vec())).is_ok(),
        Event::Closed,
        close,
    )
}

impl<T> Duplex<T> {
    pub fn try_send(&self, bytes: Vec<u8>) -> SendResult {
        self.try_send_payload(0, bytes, 0)
    }

    pub fn try_send_payload(&self, tag: u64, bytes: Vec<u8>, payload: usize) -> SendResult {
        let mut state = self.0.lock().expect("duplex state lock");
        let sender = state.sender.as_ref().cloned().ok_or(SendError::Closed)?;
        let charge = (
            bytes.len().checked_sub(payload).ok_or(SendError::Full)?,
            payload,
        );
        if bytes.is_empty() {
            return Ok(());
        }
        state.used = reserve(state.used, charge, state.limit).ok_or(SendError::Full)?;
        if sender.send((bytes, charge, tag)).is_err() {
            state.sender.take();
            state.used = (0, 0);
            return Err(SendError::Closed);
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        self.0.lock().expect("duplex state lock").sender.take();
    }

    pub fn pending(&self) -> usize {
        let (overhead, payload) = self.0.lock().expect("duplex state lock").used;
        overhead + payload
    }
}

pub(crate) fn pump<T: Send + 'static>(
    mut reader: impl Read + Send + 'static,
    mut writer: impl Write + Send + 'static,
    limit: usize,
    payload_limit: usize,
    mut emit: impl FnMut(&[u8], &Sender<T>) -> bool + Send + 'static,
    closed: T,
    close: impl FnOnce() + Send + 'static,
) -> Duplex<T> {
    let (completed, completions) = unbounded();
    let (out, writes) = channel::<WriteJob>();
    let (tx, events) = bounded(8);
    let state = Arc::new(Mutex::new(State {
        sender: Some(out),
        used: (0, 0),
        limit: (limit, payload_limit),
    }));
    std::thread::spawn(move || {
        let mut buf = [0; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || !emit(&buf[..n], &tx) {
                break;
            }
        }
        let _ = tx.send(closed);
    });
    let writer_state = Arc::downgrade(&state);
    std::thread::spawn(move || {
        while let Ok((bytes, charge, tag)) = writes.recv() {
            let mut written = 0;
            let error = loop {
                match writer.write(&bytes[written..]) {
                    Ok(0) => break Some(20),
                    Ok(count) => {
                        written += count;
                        if written == bytes.len() {
                            break None;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break Some(20),
                }
            };
            if let Some(state) = writer_state.upgrade() {
                let mut state = state.lock().expect("duplex state lock");
                if error.is_some() {
                    state.sender.take();
                    state.used = (0, 0);
                } else {
                    state.used.0 -= charge.0;
                    state.used.1 -= charge.1;
                }
            }
            let _ = completed.send((tag, written as u64, error));
            if let Some(error) = error {
                while let Ok((_, _, tag)) = writes.recv() {
                    let _ = completed.send((tag, 0, Some(error)));
                }
                break;
            }
        }
        close();
    });
    Duplex(state, events, completions)
}

fn reserve(used: Charge, charge: Charge, limit: Charge) -> Option<Charge> {
    let next = (used.0.checked_add(charge.0)?, used.1.checked_add(charge.1)?);
    (next.0 <= limit.0 && next.1 <= limit.1).then_some(next)
}

#[cfg(test)]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/unit/runtime_io.rs"
));

pub fn attach_viewer_to(
    client: &mut Client,
    options: &Options,
    geometry: (u16, u16),
    output: &mut dyn Write,
    liveness: Duration,
    mut reconnect: impl FnMut(Duration) -> Result<Client, String>,
    mut start: impl FnMut(ViewerSender),
) -> Result<i32, String> {
    let (sender, commands) = bounded(1);
    let mut request = [0; 5];
    request[..2].copy_from_slice(&geometry.1.to_le_bytes());
    request[2..4].copy_from_slice(&geometry.0.to_le_bytes());
    request[4] = 1 | u8::from(options.non_vt) << 1;
    let mut stream = Viewer {
        client,
        options,
        output,
        phase: ViewerPhase::Starting(&mut start),
        commands,
        sender,
        wire: ViewerStream {
            non_vt: options.non_vt,
            ..ViewerStream::default()
        },
        lease: None,
        size: (options.redraw == Redraw::Winch).then_some(geometry),
        release: None,
    };
    let mut heartbeat = Instant::now() + liveness;
    stream.client.send(3, &request)?;
    let paused = never();
    loop {
        let wait = heartbeat.saturating_duration_since(Instant::now());
        let commands = if matches!(stream.phase, ViewerPhase::Reattaching)
            || stream.release.is_some()
            || stream.lease.as_ref().is_some_and(InputLease::pending)
        {
            &paused
        } else {
            &stream.commands
        };
        let failure = select! {
            recv(commands) -> command => match command {
                Ok(Command::Abort) => {
                    stream.client.cancel();
                    return stream.fail("viewer input failed");
                }
                Ok(command) => match stream.command(command) {
                    Ok(true) => return Ok(0),
                    Ok(false) => None,
                    Err(error) => Some((error, true)),
                },
                Err(_) => Some(("viewer input failed".into(), false)),
            },
            recv(stream.client.transport.1) -> event => match event {
                Ok(Ok(message)) => {
                    if message.kind == 0x12 {
                        crate::wire::Heartbeat::decode(&message.payload).map_err(crate::protocol)?;
                        heartbeat = Instant::now() + liveness;
                    }
                    if stream.accept(&message)? { return Ok(0); }
                    None
                }
                Ok(Err(failure)) => Some(failure),
                Err(_) => Some(("connection closed".into(), true)),
            },
            default(wait) => (Instant::now() >= heartbeat)
                .then(|| ("holder heartbeat timed out".into(), true)),
        };
        if let Some((error, recoverable)) = failure {
            if !recoverable {
                return stream.fail(error);
            }
            stream.client.cancel();
            stream.wire.disconnected();
            let Some(mut lease) = stream.lease.take() else {
                return stream.fail(error);
            };
            let recovered: Result<(), String> = (|| {
                lease.resume(stream.client, &mut reconnect)?;
                if let Some((rows, columns)) = stream.size {
                    request[..2].copy_from_slice(&columns.to_le_bytes());
                    request[2..4].copy_from_slice(&rows.to_le_bytes());
                }
                request[4] &= !1;
                stream.client.send(3, &request)?;
                stream.lease = Some(lease);
                stream.phase = ViewerPhase::Reattaching;
                Ok(())
            })();
            if let Err(error) = recovered {
                return stream.fail(error);
            }
            heartbeat = Instant::now() + liveness;
        }
    }
}

pub fn run_viewer_input(
    mut input: impl Read,
    sender: ViewerSender,
    config: InputConfig,
    mut ready: impl FnMut() -> InputState,
    mut size: impl FnMut() -> Option<(u16, u16)>,
    mut suspend: impl FnMut(),
    mut now: impl FnMut() -> Instant,
) {
    let InputConfig {
        detach,
        pass_suspend,
        mut last_size,
    } = config;
    let mut armed = None;
    let mut bytes = vec![0; 65536];
    let mut output = Vec::with_capacity(bytes.len());
    let mut renewed = now();
    let graceful = 'input: loop {
        let current = now();
        if current.saturating_duration_since(renewed) >= Duration::from_secs(3) {
            if sender.0.send(Command::Keepalive).is_err() {
                break false;
            }
            renewed = current;
        }
        let next_size = size();
        if next_size != last_size {
            if let Some((rows, columns)) = next_size {
                let _ = sender.0.send(Command::Resize(rows, columns));
            }
            last_size = next_size;
        }
        if armed
            .is_some_and(|at| current.saturating_duration_since(at) >= Duration::from_millis(250))
        {
            break true;
        }
        match ready() {
            InputState::Pending => continue,
            InputState::Closed => break true,
            InputState::Ready => {}
        }
        let count = match input.read(&mut bytes) {
            Ok(0) => break true,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break false,
        };
        output.clear();
        for byte in bytes[..count].iter().copied() {
            if armed.take().is_some() {
                output.push(byte);
                if Some(byte) != detach {
                    break 'input sender.flush(&mut output);
                }
            } else if Some(byte) == detach || byte == 0x1a && !pass_suspend {
                if !sender.flush(&mut output) {
                    break 'input false;
                }
                if Some(byte) == detach {
                    armed = Some(current);
                } else {
                    suspend();
                    last_size = None;
                }
            } else {
                output.push(byte);
            }
        }
        if !sender.flush(&mut output) {
            break false;
        }
    };
    let released = graceful && sender.release();
    if !released {
        let _ = sender.0.send(Command::Abort);
    }
}
