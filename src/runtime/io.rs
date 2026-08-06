use crate::cli::{Options, Redraw, Reset};
use crate::runtime::client::{Client, InputLease};
#[allow(unused_imports)]
use crate::schema;
use crate::session::{LeaseRole, ResultOutcome};
use crate::wire::{Message, ViewerEvent, ViewerStream, decode_viewer};
use crossbeam_channel::{Receiver as CrossReceiver, Sender as CrossSender, bounded, never, select};
use interprocess::TryClone;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering::*};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

schema!(enum pub Event [Debug, Eq, PartialEq]; Bytes(Vec<u8>), Closed);
schema!(enum pub SendError [Clone, Copy, Debug, Eq, PartialEq]; Full, Closed);
schema!(enum pub InputState [Clone, Copy, Debug, Eq, PartialEq]; Ready, Pending, Closed);

schema!(struct pub InputConfig pub fields; detach: Option<u8>, pass_suspend: bool, state: Arc<AtomicU8>, last_size: Option<(u16, u16)>);

struct State(AtomicU64, u64);
type WriteJob = (Vec<u8>, u64);

pub struct Duplex<T = Event>(
    Arc<Mutex<Option<Sender<WriteJob>>>>,
    pub CrossReceiver<T>,
    Arc<State>,
);

schema!(enum Command; Input(Vec<u8>), Resize(u16, u16), Keepalive, Release(Sender<bool>), Abort);
pub struct ViewerSender(CrossSender<Command>);

impl ViewerSender {
    pub fn send(&self, bytes: &[u8]) -> bool {
        bytes.is_empty() || self.0.send(Command::Input(bytes.to_vec())).is_ok()
    }
    fn flush(&self, bytes: &mut Vec<u8>) -> bool {
        let sent = self.send(bytes);
        bytes.clear();
        sent
    }
    pub fn release(&self) -> bool {
        let (send, receive) = channel();
        self.0.send(Command::Release(send)).is_ok() && receive.recv().unwrap_or(false)
    }
}

schema!(struct Viewer<'a> fields; client: &'a mut Client, options: &'a Options, output: &'a mut dyn Write,
    start: Option<&'a mut dyn FnMut(ViewerSender, Arc<AtomicU8>)>, commands: CrossReceiver<Command>, sender: CrossSender<Command>,
    state: Arc<AtomicU8>, wire: ViewerStream, lease: Option<InputLease>,
    size: Option<(u16, u16)>, release: Option<Sender<bool>>);

impl Viewer<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.output
            .write_all(bytes)
            .map_err(|error| error.to_string())
    }

    fn accept(&mut self, message: &Message) -> Result<bool, String> {
        self.wire.lease_epoch = self.lease.as_ref().map(|lease| lease.epoch);
        let Some(event) = decode_viewer(
            &mut self.wire,
            message,
            (
                &self.client.identity,
                self.client.generation,
                self.client.incarnation,
            ),
        )
        .map_err(crate::protocol)?
        else {
            return Ok(false);
        };
        match event {
            ViewerEvent::Terminal(bytes) => {
                self.write(bytes)?;
                if !bytes.is_empty() && self.options.reset == Reset::Move {
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
                if let Some(start) = self.start.take() {
                    self.lease = InputLease::from_result(result, LeaseRole::Viewer)?;
                    start(ViewerSender(self.sender.clone()), self.state.clone());
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
        if let Some(done) = self.release.take() {
            let _ = done.send(false);
        }
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
        raw(reader, writer, limit, 4 << 20, close).0
    }

    pub fn tracked(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        limit: usize,
    ) -> (Self, Receiver<(u64, Option<u16>)>) {
        raw(reader, writer, limit, 16 << 20, || {})
    }
}

fn raw(
    reader: impl Read + Send + 'static,
    writer: impl Write + Send + 'static,
    limit: usize,
    payload_limit: usize,
    close: impl FnOnce() + Send + 'static,
) -> (Duplex, Receiver<(u64, Option<u16>)>) {
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
    pub fn try_send(&self, bytes: Vec<u8>) -> Result<(), SendError> {
        self.try_send_payload(bytes, 0)
    }

    pub fn try_send_payload(&self, bytes: Vec<u8>, payload: usize) -> Result<(), SendError> {
        let state = &self.2;
        let sender = self.0.lock().expect("duplex sender lock");
        let sender = sender.as_ref().ok_or(SendError::Closed)?;
        let Some(overhead) = bytes.len().checked_sub(payload) else {
            return Err(SendError::Full);
        };
        if bytes.is_empty() {
            return Ok(());
        }
        let charge = usage(overhead, payload).ok_or(SendError::Full)?;
        state
            .0
            .fetch_update(AcqRel, Acquire, |used| reserve(used, charge, state.1))
            .map_err(|_| SendError::Full)?;
        sender.send((bytes, charge)).map_err(|_| {
            state.0.fetch_sub(charge, AcqRel);
            SendError::Closed
        })
    }

    pub fn shutdown(&self) {
        self.0.lock().expect("duplex sender lock").take();
    }

    pub fn pending(&self) -> usize {
        let used = self.2.0.load(Acquire);
        (used >> 32) as usize + (used as u32) as usize
    }
}

pub(crate) fn pump<T: Send + 'static>(
    mut reader: impl Read + Send + 'static,
    mut writer: impl Write + Send + 'static,
    limit: usize,
    payload_limit: usize,
    mut emit: impl FnMut(&[u8], &CrossSender<T>) -> bool + Send + 'static,
    closed: T,
    close: impl FnOnce() + Send + 'static,
) -> (Duplex<T>, Receiver<(u64, Option<u16>)>) {
    let (completed, completions) = channel();
    let (out, writes) = channel::<WriteJob>();
    let out = Arc::new(Mutex::new(Some(out)));
    let (tx, events) = bounded(8);
    let state = Arc::new(State(
        AtomicU64::new(0),
        usage(limit, payload_limit).unwrap(),
    ));
    std::thread::spawn(move || {
        let mut buf = [0; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || !emit(&buf[..n], &tx) {
                break;
            }
        }
        let _ = tx.send(closed);
    });
    let ws = state.clone();
    let writer_out = out.clone();
    std::thread::spawn(move || {
        while let Ok((bytes, charge)) = writes.recv() {
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
            if error.is_some() {
                writer_out.lock().expect("duplex sender lock").take();
                ws.0.store(0, Release);
            }
            let _ = completed.send((written as u64, error));
            if error.is_some() {
                break;
            }
            ws.0.fetch_sub(charge, AcqRel);
        }
        close();
    });
    (Duplex(out, events, state), completions)
}

fn usage(overhead: usize, payload: usize) -> Option<u64> {
    Some((u64::from(u32::try_from(overhead).ok()?) << 32) | u64::from(u32::try_from(payload).ok()?))
}

fn reserve(used: u64, charge: u64, limit: u64) -> Option<u64> {
    let overhead = (used >> 32).checked_add(charge >> 32)?;
    let payload = u64::from(used as u32).checked_add(u64::from(charge as u32))?;
    (overhead <= limit >> 32 && payload <= u64::from(limit as u32))
        .then_some(overhead << 32 | payload)
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
    mut start: impl FnMut(ViewerSender, Arc<AtomicU8>),
) -> Result<i32, String> {
    let state = Arc::new(AtomicU8::new(0));
    let (sender, commands) = bounded(1);
    let mut request = [0; 5];
    request[..2].copy_from_slice(&geometry.1.to_le_bytes());
    request[2..4].copy_from_slice(&geometry.0.to_le_bytes());
    request[4] = 1 | u8::from(options.non_vt) << 1;
    let mut stream = Viewer {
        client,
        options,
        output,
        start: Some(&mut start),
        commands,
        sender,
        state: state.clone(),
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
        let commands =
            if stream.release.is_some() || stream.lease.as_ref().is_some_and(InputLease::pending) {
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
            let recovered = (|| {
                lease.resume(stream.client, &mut reconnect)?;
                request[4] &= !1;
                stream.client.send(3, &request)?;
                if let Some((rows, columns)) = stream.size {
                    lease.resize(stream.client, rows, columns)?;
                }
                stream.lease = Some(lease);
                stream.advance(vec![])
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
        state,
        mut last_size,
    } = config;
    let mut armed = None;
    let mut bytes = vec![0; 65536];
    let mut output = Vec::with_capacity(bytes.len());
    let mut renewed = now();
    let graceful = 'input: loop {
        if state.load(Acquire) != 0 {
            return;
        }
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
    state.store(if released { 1 } else { 2 }, Release);
}
