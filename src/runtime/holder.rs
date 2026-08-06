use super::io::{Duplex, Event as IoEvent};
use super::private::{PreparedArtifacts, exit_records, monotonic, now, random_array};
use super::storage::{Done, Purpose, SessionStorage, SnapshotState, StatusSnapshot, StorageError};
use crate::events::{self, Event};
#[allow(unused_imports)]
use crate::schema;
use crate::session::{
    CommitTicket, Completion, ConnId, Effect as PolicyEffect, EventPosition, LeaseOperation,
    LeaseRole, Machine, Reply, Request as PolicyRequest, SemanticRefusal, Transition,
    WriteTicket as Ticket,
};
use crate::terminal::{Observation, Scan, Scanner};
use crate::wire::{self, Codec, ControllerRequest, Message, Profile, StatusExtension, StatusTail};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;

schema!(struct pub CoreConfig derive [Debug] pub fields; generation: u32, identity: Vec<u8>, incarnation: [u8; 16],
    semantic_token: [u8; 16], replay_limit: usize);
type DecodeResult = std::result::Result<(), wire::WireError>;
type ClearResult = (u8, u8, Option<(u32, u64, u64)>);
type Refusal = (u16, u16, &'static [u8]);
const DESCRIPTOR_LIMIT: usize = 64;
const QUERY_REPLIES: [[&[u8]; 2]; 3] = [
    [b"\x1b[?62;4c", b"\x9b?62;4c"],
    [b"\x1b[>1;47;0c", b"\x9b>1;47;0c"],
    [b"\x1bP>|kitty(0.47.0)\x1b\\", b"\x90>|kitty(0.47.0)\x9c"],
];

impl<N: Native> Runtime<N> {
    fn transition<'a>(&mut self, event: Transition<'a>) -> DecodeResult {
        let effects = self.machine.transition(event)?;
        self.apply_with(effects, &mut monotonic);
        Ok(())
    }

    fn apply_with(
        &mut self,
        effects: impl IntoIterator<Item = PolicyEffect>,
        clock: &mut impl FnMut() -> u64,
    ) {
        for effect in effects {
            match effect {
                PolicyEffect::Send(id, reply) => self.reply(id, reply),
                PolicyEffect::Attached(id, non_vt, result, resize) => {
                    let (snapshot, deadline) = self
                        .status_snapshot
                        .expect("attach descriptor snapshot and deadline");
                    if clock() >= deadline {
                        self.disconnect(id);
                        continue;
                    }
                    let mut state = if non_vt {
                        Vec::new()
                    } else {
                        self.scanner.modes().preamble().unwrap_or_default()
                    };
                    let size = (state.len() as u16).to_le_bytes();
                    state.splice(..0, size);
                    if clock() >= deadline {
                        self.disconnect(id);
                        continue;
                    }
                    if !self.send(id, 5, &state) {
                        continue;
                    }
                    if let Some((rows, columns)) = resize {
                        self.resize(rows, columns, false);
                    }
                    if self.send_status(id, true, snapshot, deadline, clock) {
                        if let Some(peer) = self.peers.get_mut(&id) {
                            peer.deadline = 0;
                        }
                        if let Some(result) = result {
                            self.reply(id, Reply::Lease(result));
                        }
                    }
                }
                PolicyEffect::Resize(rows, columns) => self.resize(rows, columns, false),
                PolicyEffect::Write(ticket, bytes) => self.write(ticket, bytes),
                PolicyEffect::CommitSources(ticket, changes, mandatory) => {
                    let purpose = Purpose::Sources(ticket.get(), mandatory);
                    let submitted = changes.is_empty()
                        || events::semantic_changes(now(), changes)
                            .is_ok_and(|events| self.storage.commit(purpose, &events).is_ok());
                    if !submitted {
                        // A mandatory observation cannot be refused, so its
                        // failure is a storage failure and closes the stream.
                        // A rejectable request must fail before any state change
                        // and disturb neither the lane nor another peer, so only
                        // its own requester is resolved (closure §5.7).
                        if mandatory {
                            let _ = self.transition(Transition::Writable(false));
                        }
                        self.complete(ticket, Completion::Sources(false));
                    }
                }
                PolicyEffect::CommitSemantic(ticket, source, epoch, producer, event, receipt) => {
                    let stored = match receipt {
                        Some(receipt) => events::application_receipt(
                            now(),
                            &source,
                            producer,
                            epoch,
                            &event,
                            &receipt,
                        ),
                        None => events::semantic_assertion(now(), &source, producer, epoch, &event),
                    }
                    .map_err(|_| SemanticRefusal::InvalidPayload)
                    .and_then(|stored| {
                        self.storage
                            .commit(Purpose::Semantic(ticket.get(), false), &[stored])
                            .map_err(|_| SemanticRefusal::ResourceExhausted)
                    });
                    if let Err(error) = stored {
                        self.complete(ticket, Completion::Semantic(Err(error)));
                    }
                }
                PolicyEffect::QuerySend(id, query) => {
                    self.send(id, 0x14, &query.encode().expect("valid delegated query"));
                }
                PolicyEffect::Output(target, record) => {
                    let payload = wire::join(&[
                        &record.sequence.to_le_bytes(),
                        &record.offset.to_le_bytes(),
                        &record.bytes,
                    ]);
                    if let Some(id) = target {
                        self.send(id, 6, &payload);
                    } else {
                        let _ = self.storage.output(record.bytes, self.machine.output_end());
                        self.broadcast(6, &payload, true);
                    }
                }
                PolicyEffect::Gap(id, lost) => {
                    let mut payload = [0; 16];
                    payload[..8].copy_from_slice(&1u64.to_le_bytes());
                    payload[8..].copy_from_slice(&lost.to_le_bytes());
                    self.send(id, 8, &payload);
                }
                PolicyEffect::OutputExhausted => {
                    self.peers
                        .values_mut()
                        .filter(|peer| peer.is(Profile::Controller))
                        .for_each(|peer| peer.scope = self.config.generation);
                    self.broadcast(
                        0x13,
                        &wire::error_payload(13, b"output coordinates exhausted"),
                        false,
                    );
                }
                PolicyEffect::Terminate(force) => {
                    let (containment, forced) = self.native.terminate(force);
                    let _ = self.transition(Transition::TerminationApplied(containment, forced));
                }
                PolicyEffect::ReportTermination(id) => {
                    let _ = self.transition(Transition::ReportTermination(id));
                }
                PolicyEffect::Flush(id, deadline) => {
                    while self
                        .peers
                        .get(&id)
                        .is_some_and(|peer| peer.pipe.pending() != 0)
                        && monotonic() < deadline
                    {
                        self.poll();
                        thread::sleep(Duration::from_millis(3));
                    }
                }
                PolicyEffect::Close(id) | PolicyEffect::Replaced(id) => self.disconnect(id),
            }
        }
    }

    /// The tracked-mode scanner resolves a scroll region against the row count,
    /// so it may only adopt a geometry the platform actually applied: adopting a
    /// failed resize makes the next preamble claim a default region that is not
    /// the child's (§6 of the schema, §5.2).
    fn resize(&mut self, rows: u16, columns: u16, redraw: bool) {
        let result = if redraw {
            self.native.redraw(rows, columns)
        } else {
            self.native.resize(rows, columns)
        };
        if result.is_ok() {
            self.scanner.set_rows(rows);
        }
    }

    fn reply(&mut self, id: ConnId, reply: Reply) {
        match wire::encode_reply(reply, self.config.incarnation) {
            wire::RuntimeReply::Frame(kind, payload) => {
                self.send(id, kind, &payload);
            }
            wire::RuntimeReply::Scoped(scope, kind, payload) => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.handshake = 0;
                    peer.deadline = 0;
                    peer.scope = scope;
                }
                self.send(id, kind, &payload);
            }
        }
    }

    pub fn tick(&mut self, now: u64) {
        let scans = self.scanner.expire(now);
        self.publish_scans(now, scans);
        let mut released = 0;
        self.recipients
            .extend(self.peers.iter_mut().filter_map(|(id, peer)| {
                if peer.deadline != 0 && now >= peer.deadline {
                    Some(*id)
                } else {
                    peer.codec.as_mut().and_then(|codec| {
                        let before = codec.buffered_len();
                        let expired = codec.expire(now).is_err();
                        released += before - codec.buffered_len();
                        expired.then_some(*id)
                    })
                }
            }));
        self.buffered -= released;
        while let Some(id) = self.recipients.pop() {
            let reassembly = self.peers.get(&id).is_some_and(|peer| {
                peer.handshake == 0 && (peer.deadline == 0 || now < peer.deadline)
            });
            if reassembly {
                self.refuse_wire(id, wire::WireError::ReassemblyTimeout);
            } else {
                self.refuse(id, (12, 13, b"identity exchange deadline exceeded"));
            }
        }
        let _ = self.transition(Transition::Tick(now));
        let health = self.storage.health();
        let flags = u8::from(self.child_running)
            | (u8::from(health & 2 != 0) << 1)
            | (u8::from(health & 1 != 0) << 2)
            | (u8::from(health & 4 != 0) << 3)
            | (u8::from(self.scanner.exact()) << 4);
        if flags != self.heartbeat_flags || now >= self.heartbeat_at.saturating_add(5_000) {
            self.heartbeat_flags = flags;
            self.heartbeat_at = now;
            let payload = wire::Heartbeat {
                monotonic_ms: now,
                flags,
            }
            .encode()
            .unwrap();
            self.broadcast(0x12, &payload, true);
        }
    }
    /// The single completion-consumption path. Every caller routes through it so
    /// that a durable event advancement always produces OB-30's coalesced level
    /// trigger — a shutdown wait that consumed completions silently left a
    /// connected controller waiting for a 0x11 that never came — and so the
    /// selected-commit cache is current before any descriptor copies it.
    fn drain_storage(&mut self, awaited: Option<Purpose>) -> Option<bool> {
        let mut result = None;
        let mut advanced = false;
        for done in self.storage.poll() {
            advanced |= done.lane == SessionStorage::EVENT_LANE && done.result.is_ok();
            if awaited == Some(done.purpose) {
                result = Some(done.result.is_ok());
            } else {
                self.storage_done(done);
            }
        }
        if advanced {
            self.broadcast(0x11, &[], false);
        }
        result
    }

    fn complete(&mut self, ticket: CommitTicket, completion: Completion) {
        let _ = self.transition(Transition::Complete(monotonic(), ticket, completion));
    }

    pub(super) fn disconnect(&mut self, id: ConnId) {
        self.descriptors.retain(|pending| pending.peer != id);
        self.storage.abandon_clear(id);
        if let Some(peer) = self.peers.remove(&id) {
            self.buffered -= peer.codec.as_ref().map_or(0, Codec::buffered_len);
            peer.pipe.shutdown();
        }
        let _ = self.transition(Transition::Disconnect(id));
    }
}

type Result<T, E = String> = std::result::Result<T, E>;

schema!(enum pub NativeExit [Clone, Copy, Debug, Eq, PartialEq]; Code(u32), Signal(u32));

pub trait Native {
    fn resize(&mut self, rows: u16, columns: u16) -> Result<()>;
    fn redraw(&mut self, rows: u16, columns: u16) -> Result<()> {
        self.resize(rows, columns)
    }
    fn terminate(&mut self, force: bool) -> (u8, bool);
    fn exited(&mut self) -> Result<Option<NativeExit>>;
}

schema!(struct pub HolderConfig<N> pub fields; core: CoreConfig, pty: Duplex, writes: Receiver<(u64, Option<u16>)>, storage: SessionStorage,
    status: Vec<u8>, commit_at: usize, synthetic: u8, native: N);
schema!(struct Peer fields; pipe: Duplex, codec: Option<Codec>, preface: Vec<u8>, scope: u32, handshake: u64, deadline: u64);
schema!(enum Descriptor; Status, Attach(u16, u16, bool, bool, Option<[u8; 16]>));
schema!(struct PendingDescriptor fields; peer: ConnId, deadline: u64, request: Descriptor);

impl Peer {
    fn profile(&self) -> Option<Profile> {
        self.codec.as_ref().map(Codec::profile)
    }

    fn is(&self, profile: Profile) -> bool {
        self.profile() == Some(profile)
    }
}

schema!(struct pub Runtime<N> fields; config: CoreConfig, pty: Duplex, pty_open: bool, child_running: bool,
    writes: Receiver<(u64, Option<u16>)>, pending_writes: VecDeque<Ticket>, peers: HashMap<u64, Peer>, recipients: Vec<u64>,
    frames: Vec<Message>, buffered: usize, next_peer: u64, scanner: Scanner, storage: SessionStorage, status: Vec<u8>, commit_at: usize, synthetic: u8, native: N,
    heartbeat_at: u64, heartbeat_flags: u8, descriptors: VecDeque<PendingDescriptor>, status_snapshot: Option<(StatusSnapshot, u64)>, machine: Machine);

impl<N: Native> Runtime<N> {
    pub fn output(&mut self, bytes: Vec<u8>) {
        let time = monotonic();
        let scans = self.scanner.scan_owned(time, bytes);
        self.publish_scans(time, scans);
    }
    pub fn output_end(&self) -> u64 {
        self.machine.output_end()
    }
    /// The tracked-mode scanner resolves a scroll region's omitted bottom
    /// against the current row count (schema §6), so it has to start from the
    /// creation geometry rather than a fixed 24.
    pub fn set_rows(&mut self, rows: u16) {
        self.scanner.set_rows(rows);
    }
    #[cfg(unix)]
    pub(crate) fn observe_exit(&mut self) -> Result<Option<NativeExit>> {
        let status = self.native.exited()?;
        if status.is_some() {
            self.child_running = false;
        }
        Ok(status)
    }
    fn clear_result(&mut self, id: u64, prior: u64, result: ClearResult) -> bool {
        let (outcome, reason, commit) = result;
        let (epoch, resulting, end) = commit.unwrap_or_default();
        let payload = wire::log_clear_result_payload(outcome, reason, epoch, prior, resulting, end)
            .expect("valid log-clear result");
        self.send(id, 0x1a, &payload)
    }
    pub fn shutdown_requested(&mut self, now: u64, force: bool) {
        let _ = self.transition(Transition::Shutdown(now, force));
    }
}

impl<N: Native> Runtime<N> {
    pub fn new(config: HolderConfig<N>) -> Self {
        let mut machine = Machine::new(
            config.core.generation,
            config.core.incarnation,
            config.core.semantic_token,
        );
        machine.configure(config.core.identity.clone(), config.core.replay_limit);
        Self {
            config: config.core,
            pty: config.pty,
            pty_open: true,
            child_running: true,
            writes: config.writes,
            pending_writes: VecDeque::new(),
            peers: HashMap::new(),
            recipients: Vec::with_capacity(64),
            frames: Vec::with_capacity(8),
            buffered: 0,
            next_peer: 1,
            scanner: Scanner::new(24),
            storage: config.storage,
            status: config.status,
            commit_at: config.commit_at,
            synthetic: config.synthetic,
            native: config.native,
            heartbeat_at: 0,
            heartbeat_flags: 0,
            descriptors: VecDeque::new(),
            status_snapshot: None,
            machine,
        }
    }

    pub fn finish_exit(
        &mut self,
        running: &str,
        status: NativeExit,
        termination: Option<bool>,
    ) -> (i32, bool) {
        let method = termination.map(|forced| if forced { "forced" } else { "graceful" });
        let (exit, outcome) = match (status, method) {
            (NativeExit::Code(code), Some(method)) => (
                code as i32,
                ("terminated", "code", u64::from(code), Some(method)),
            ),
            (NativeExit::Code(code), None) => {
                (code as i32, ("exited", "code", u64::from(code), None))
            }
            (NativeExit::Signal(signal), _) => {
                (1, ("signalled", "signal", u64::from(signal), None))
            }
        };
        let ts = now();
        let records = exit_records(running, (ts, now()), self.output_end(), outcome);
        (exit, self.finish(records.0, records.1))
    }

    pub fn accept(&mut self, pipe: Duplex, same_user: bool) {
        let time = monotonic();
        self.tick(time);
        if !same_user
            || self
                .peers
                .values()
                .filter(|peer| peer.handshake != 0)
                .count()
                >= 16
            || self.next_peer == u64::MAX
        {
            return pipe.shutdown();
        }
        let id = self.next_peer;
        self.next_peer += 1;
        self.peers.insert(
            id,
            Peer {
                pipe,
                codec: None,
                preface: Vec::new(),
                scope: 0,
                handshake: time.saturating_add(2_000),
                deadline: time.saturating_add(2_000),
            },
        );
    }

    pub fn poll(&mut self) {
        self.recipients.extend(self.peers.keys().copied());
        while let Some(id) = self.recipients.pop() {
            while let Some(event) = self
                .peers
                .get(&id)
                .and_then(|peer| peer.pipe.1.try_recv().ok())
            {
                match event {
                    IoEvent::Bytes(bytes) => self.peer_bytes(id, bytes),
                    IoEvent::Closed => self.disconnect(id),
                }
            }
        }
        while let Ok(event) = self.pty.1.try_recv() {
            match event {
                IoEvent::Bytes(bytes) => self.output(bytes),
                IoEvent::Closed => self.pty_open = false,
            }
        }
        while let Ok((written, error)) = self.writes.try_recv() {
            if let Some(ticket) = self.pending_writes.pop_front()
                && ticket.get() != 0
            {
                self.complete(ticket, Completion::Write(written, error));
            }
        }
        self.drain_storage(None);
        self.poll_descriptors_with(&mut monotonic);
        let _ = self.transition(Transition::Writable(self.storage.health() & 2 != 0));
        self.tick(monotonic());
    }

    #[cfg(windows)]
    pub(crate) fn termination_method(&self) -> Option<bool> {
        self.machine.termination_forced()
    }

    pub fn finish(&mut self, event: Event, lifecycle: Vec<u8>) -> bool {
        if !self.wait_storage(None) {
            return false;
        }
        if self
            .storage
            .lifecycle(lifecycle, self.machine.output_end())
            .is_err()
            || !self.wait_storage(Some(Purpose::Lifecycle))
        {
            return false;
        }
        let mut changes = Vec::new();
        for effect in self.machine.transition(Transition::Ending).unwrap() {
            match effect {
                PolicyEffect::CommitSources(_, values, _) => changes.extend(values),
                effect => self.apply_with([effect], &mut monotonic),
            }
        }
        let mut events = events::semantic_changes(now(), changes).unwrap_or_default();
        events.push(event);
        if self.storage.health() & 2 != 0 && self.storage.commit(Purpose::Final, &events).is_ok() {
            let _ = self.wait_storage(Some(Purpose::Final));
        }
        !self.machine.termination_expired()
    }

    pub fn drive(
        &mut self,
        mut accept: impl FnMut() -> Option<(Duplex, bool)>,
        mut signal: impl FnMut() -> Option<bool>,
    ) -> Result<Option<NativeExit>> {
        let mut exited = None;
        let mut drain_until = 0;
        loop {
            if exited.is_none() {
                while let Some((transport, same_user)) = accept() {
                    self.accept(transport, same_user);
                }
                if let Some(force) = signal() {
                    self.shutdown_requested(monotonic(), force);
                }
            }
            self.poll();
            if self.machine.termination_expired() {
                return Ok(None);
            }
            if let Some(status) = exited {
                // The drain deadline bounds how long output draining may
                // continue; it never discards the observed exit itself, which
                // is the only input to the §7.4 record and the §8.2 event.
                if !self.pty_open || monotonic() >= drain_until {
                    return Ok(Some(status));
                }
            } else if let Some(status) = self.native.exited()? {
                self.child_running = false;
                exited = Some(status);
                drain_until = self.machine.termination_started().map_or_else(
                    || monotonic().saturating_add(2_000),
                    |started| started.saturating_add(10_000),
                );
                continue;
            }
            thread::sleep(Duration::from_millis(3));
        }
    }

    fn peer_bytes(&mut self, id: u64, bytes: Vec<u8>) {
        let profile = {
            let Some(peer) = self.peers.get_mut(&id) else {
                return;
            };
            if peer.codec.is_some() {
                return self.decode(id, &bytes);
            }
            peer.preface.extend(bytes);
            if peer.preface.len() < 4 {
                return;
            }
            match &peer.preface[..4] {
                b"MOOR" => Profile::Controller,
                b"MOOS" => Profile::Semantic,
                _ => return self.disconnect(id),
            }
        };
        if self
            .peers
            .values()
            .filter(|peer| peer.is(profile) && peer.handshake == 0)
            .count()
            >= 64
        {
            return self.refuse(id, (13, 12, b"connection limit exhausted"));
        }
        let peer = self.peers.get_mut(&id).unwrap();
        peer.codec = Some(Codec::new(profile));
        let bytes = std::mem::take(&mut peer.preface);
        self.decode(id, &bytes);
    }

    fn decode(&mut self, id: u64, bytes: &[u8]) {
        let mut frames = std::mem::take(&mut self.frames);
        let trailing = {
            let Some(codec) = self.peers.get_mut(&id).and_then(|peer| peer.codec.as_mut()) else {
                return;
            };
            let before = codec.buffered_len();
            let projected = self
                .buffered
                .checked_sub(before)
                .zip(codec.projected_len(bytes.len()))
                .and_then(|(used, next)| used.checked_add(next));
            let error = if projected.is_none_or(|size| size > 64 << 20) {
                Some(wire::WireError::ResourceExhausted)
            } else {
                codec.feed(monotonic(), bytes, &mut frames).err()
            };
            self.buffered = self.buffered - before + codec.buffered_len();
            error
        };
        let mut failure = None;
        for message in frames.drain(..) {
            if let Err(error) = self.message_at(id, &message, monotonic()) {
                failure = Some(error);
                break;
            }
            if !self.peers.contains_key(&id) {
                break;
            }
        }
        frames.clear();
        self.frames = frames;
        if let Some(error) = failure.or(trailing) {
            self.refuse_wire(id, error);
        }
    }

    fn message_at(&mut self, id: u64, message: &Message, time: u64) -> DecodeResult {
        if self
            .peers
            .get(&id)
            .is_some_and(|peer| peer.deadline != 0 && time >= peer.deadline)
        {
            self.refuse(id, (12, 13, b"identity exchange deadline exceeded"));
            return Ok(());
        }
        let peer = self.peers.get(&id).ok_or(wire::WireError::Malformed)?;
        let first_controller = peer.is(Profile::Controller)
            && self.machine.phase(id).is_none()
            && message.kind == 1
            && (message.scope == 0 || message.scope == self.config.generation);
        if message.scope != peer.scope && !first_controller {
            let refusal = if peer.is(Profile::Semantic) && peer.handshake != 0 {
                (5, 3, &b"semantic frame preceded hello"[..])
            } else {
                (9, 5, &b"generation or source epoch did not match"[..])
            };
            self.refuse(id, refusal);
            return Ok(());
        }
        if peer.is(Profile::Semantic) {
            let request = wire::decode_semantic(message.scope, message.kind, &message.payload)?;
            self.transition(Transition::Peer(time, id, request))?
        } else {
            self.controller_message_at(id, message, time)?
        }
        Ok(())
    }

    fn controller_message_at(&mut self, id: u64, message: &Message, time: u64) -> DecodeResult {
        wire::require(
            !self.descriptors.iter().any(|pending| pending.peer == id),
            wire::WireError::Malformed,
        )?;
        if message.kind != 1 {
            let _ = self.transition(Transition::Tick(time));
            wire::require(
                self.machine.legal(id, message.kind),
                wire::WireError::Malformed,
            )?;
        }
        let token = matches!(message.kind, 3 | 0x15)
            .then(|| random_array().ok())
            .flatten();
        match wire::decode_controller(message.kind, &message.payload, token)? {
            ControllerRequest::Hello(identity) => {
                let refusal = if message.scope != 0 && message.scope != self.config.generation {
                    Some((9, 5, &b"generation did not match"[..]))
                } else if identity != self.config.identity {
                    Some((10, 3, &b"session identity did not match"[..]))
                } else {
                    None
                };
                if let Some(refusal) = refusal {
                    self.refuse(id, refusal);
                    return Ok(());
                }
                wire::require(
                    self.peers
                        .get(&id)
                        .is_some_and(|peer| peer.is(Profile::Controller))
                        && self.machine.phase(id).is_none(),
                    wire::WireError::Malformed,
                )?;
                self.machine.register_controller(id);
                let peer = self.peers.get_mut(&id).unwrap();
                peer.handshake = 0;
                peer.scope = self.config.generation;
                let payload = wire::controller_hello_ack(
                    self.config.generation,
                    self.config.incarnation,
                    &self.config.identity,
                )
                .expect("bounded identity");
                self.send(id, 2, &payload);
            }
            ControllerRequest::Policy(PolicyRequest::Attach(
                columns,
                rows,
                lease,
                non_vt,
                token,
            )) => self.queue_descriptor(
                id,
                time,
                Descriptor::Attach(columns, rows, lease, non_vt, token),
            ),
            ControllerRequest::Policy(request) => {
                let resumed_viewer = matches!(
                    &request,
                    PolicyRequest::Lease(lease, _)
                        if lease.operation == LeaseOperation::Resume
                            && lease.role == LeaseRole::Viewer
                );
                if !resumed_viewer && let Some(peer) = self.peers.get_mut(&id) {
                    peer.deadline = 0;
                }
                self.transition(Transition::Peer(time, id, request))?
            }
            ControllerRequest::Status => self.queue_descriptor(id, time, Descriptor::Status),
            ControllerRequest::LogClear(incarnation, observed) => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.deadline = 0;
                }
                if incarnation != self.config.incarnation {
                    let _ = self.clear_result(id, observed, (2, 1, None));
                    return Ok(());
                }
                match self.storage.clear(id, observed, self.machine.output_end()) {
                    Ok(()) => {}
                    Err(StorageError::Disabled) => {
                        let _ = self.clear_result(id, observed, (1, 0, None));
                    }
                    Err(StorageError::Busy) => {
                        let _ = self.clear_result(id, observed, (2, 2, None));
                    }
                }
                return Ok(());
            }
        }
        Ok(())
    }

    pub(super) fn send(&mut self, id: u64, kind: u8, payload: &[u8]) -> bool {
        let failed = self.peers.get_mut(&id).is_none_or(|peer| {
            let mut bytes = Vec::new();
            let output = usize::from(peer.is(Profile::Controller) && kind == 6)
                * payload.len().saturating_sub(16);
            peer.codec.as_mut().is_none_or(|codec| {
                codec.encode(peer.scope, kind, payload, &mut bytes).is_err()
                    || peer.pipe.try_send_payload(bytes, output).is_err()
            })
        });
        if failed {
            self.disconnect(id);
        }
        !failed
    }

    fn broadcast(&mut self, kind: u8, payload: &[u8], attached: bool) {
        self.recipients
            .extend(self.peers.iter().filter_map(|(id, peer)| {
                (peer.is(Profile::Controller) && (!attached || self.machine.attached(*id)))
                    .then_some(*id)
            }));
        while let Some(id) = self.recipients.pop() {
            self.send(id, kind, payload);
        }
    }

    fn refuse(&mut self, id: u64, refusal: Refusal) {
        let (controller, semantic, diagnostic) = refusal;
        let profile = self.peers.get(&id).and_then(Peer::profile);
        if profile == Some(Profile::Controller)
            && self.peers.get(&id).is_some_and(|peer| peer.scope == 0)
        {
            self.peers.get_mut(&id).unwrap().scope = self.config.generation;
        }
        if let Some(profile) = profile {
            let (kind, code) = match profile {
                Profile::Controller => (0x13, controller),
                Profile::Semantic => (9, semantic),
            };
            self.send(id, kind, &wire::error_payload(code, diagnostic));
        }
        self.disconnect(id);
    }

    fn refuse_wire(&mut self, id: u64, error: wire::WireError) {
        use wire::WireError::*;
        let (controller, semantic, diagnostic): (_, _, &[u8]) = match error {
            UnknownVersion => (1, 1, b"unknown wire version"),
            UnknownType => (2, 2, b"unknown frame type"),
            OversizedFrame => (3, 3, b"frame payload exceeded its bound"),
            OversizedMessage => (4, 3, b"message payload exceeded its bound"),
            Malformed => (5, 3, b"malformed frame or payload"),
            BadSequence => (6, 7, b"frame sequence was not the expected successor"),
            ReassemblyAborted => (7, 3, b"fragment run changed type or scope"),
            ReassemblyTimeout => (8, 13, b"fragment reassembly deadline exceeded"),
            ResourceExhausted => (13, 12, b"protocol resource exhausted"),
            GenerationMismatch => (9, 3, b"generation did not match"),
        };
        self.refuse(id, (controller, semantic, diagnostic));
    }

    fn write(&mut self, ticket: Ticket, bytes: Vec<u8>) {
        let size = bytes.len();
        let error = if bytes.is_empty() {
            None
        } else if self.pty.try_send_payload(bytes, size).is_ok() {
            self.pending_writes.push_back(ticket);
            return;
        } else {
            Some(20)
        };
        if ticket.get() != 0 {
            self.complete(ticket, Completion::Write(0, error));
        }
    }

    fn synthetic_reply(&self, class: u8, shape: wire::QueryShape) -> Option<Vec<u8>> {
        if self.synthetic & 1 == 0 {
            return None;
        }
        if class == 4 {
            return self.scanner.modes().query(shape.mode?, shape.csi8);
        }
        if self.synthetic & 2 == 0 {
            return None;
        }
        Some(QUERY_REPLIES.get(class.checked_sub(1)? as usize)?[usize::from(shape.csi8)].to_vec())
    }

    fn publish_scans(&mut self, time: u64, scans: Vec<Scan>) {
        for scan in scans {
            match scan {
                Scan::Observation(Observation::Query(class, bytes)) => {
                    let shape = wire::recognize_query(&bytes).expect("scanner query");
                    let fallback = self.synthetic_reply(class, shape);
                    let _ = self.transition(Transition::Query(time, bytes.into(), shape, fallback));
                }
                Scan::Release(bytes) => {
                    let _ = self.transition(Transition::Output(time, bytes));
                }
                Scan::Observation(observation) => {
                    let _ = self.storage.observe(observation);
                }
            }
        }
    }

    fn wait_storage(&mut self, purpose: Option<Purpose>) -> bool {
        let deadline = monotonic().saturating_add(2_000);
        loop {
            let time = monotonic();
            self.tick(time);
            if self.machine.termination_expired() {
                return false;
            }
            let result = self.drain_storage(purpose);
            if let Some(result) = result {
                return result;
            }
            if purpose.is_none() && self.storage.pending() == 0 {
                return true;
            }
            if time >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(3));
        }
    }

    fn storage_done(&mut self, done: Done) {
        match done.purpose {
            Purpose::Background | Purpose::Lifecycle | Purpose::Final => {}
            Purpose::Clear(tag, prior) => {
                let result = match done.result {
                    Ok((commit, cleared)) => (
                        u8::from(cleared) * 2 + u8::from(!cleared && commit.index == prior),
                        u8::from(cleared),
                        Some((commit.epoch, commit.index, commit.end)),
                    ),
                    Err(crate::store::StoreError::Corrupt) => (2, 3, None),
                    Err(crate::store::StoreError::Exhausted) => (2, 2, None),
                    Err(crate::store::StoreError::Io(_)) => return self.disconnect(tag),
                };
                if !self.clear_result(tag, prior, result) {
                    self.storage.quarantine_log();
                }
            }
            Purpose::Semantic(tag, terminal) => {
                if let Some(ticket) = CommitTicket::from_raw(tag) {
                    let position = done.result.ok().and_then(|(commit, _)| {
                        commit
                            .end
                            .checked_sub(if terminal { 2 } else { 1 })
                            .map(|sequence| EventPosition {
                                epoch: commit.epoch,
                                sequence,
                            })
                    });
                    let result = position.ok_or(SemanticRefusal::ResourceExhausted);
                    self.complete(ticket, Completion::Semantic(result));
                }
            }
            Purpose::Sources(id, _) => {
                let success = done.result.is_ok();
                if let Some(ticket) = CommitTicket::from_raw(id) {
                    self.complete(ticket, Completion::Sources(success));
                }
            }
        }
    }

    fn queue_descriptor(&mut self, peer: ConnId, now: u64, request: Descriptor) {
        if self.descriptors.len() == DESCRIPTOR_LIMIT {
            return self.refuse_wire(peer, wire::WireError::ResourceExhausted);
        }
        self.descriptors.push_back(PendingDescriptor {
            peer,
            deadline: self
                .peers
                .get(&peer)
                .map_or(now.saturating_add(2_000), |peer| {
                    if peer.deadline == 0 {
                        now.saturating_add(2_000)
                    } else {
                        peer.deadline
                    }
                }),
            request,
        });
        self.poll_descriptors_with(&mut monotonic);
    }

    /// STATUS and ATTACH copy event/log metadata at one memory-only
    /// linearization point. A lane in BODY/COMMIT phase makes the request wait,
    /// but never makes the holder read, hash, join, or spin on storage I/O.
    fn poll_descriptors_with(&mut self, clock: &mut impl FnMut() -> u64) {
        loop {
            let now = clock();
            let Some(pending) = self.descriptors.front() else {
                return;
            };
            if !self.peers.contains_key(&pending.peer) {
                self.descriptors.pop_front();
                continue;
            }
            if now >= pending.deadline {
                let peer = pending.peer;
                self.descriptors.pop_front();
                self.refuse(peer, (12, 13, b"identity exchange deadline exceeded"));
                continue;
            }
            let snapshot = match self.storage.try_status_snapshot() {
                SnapshotState::Ready(snapshot) => snapshot,
                SnapshotState::Busy => return,
                SnapshotState::Failed => {
                    let peer = pending.peer;
                    self.descriptors.pop_front();
                    self.refuse_wire(peer, wire::WireError::ResourceExhausted);
                    continue;
                }
            };
            let pending = self.descriptors.pop_front().expect("front descriptor");
            let result = match pending.request {
                Descriptor::Status => {
                    self.storage.release_status_snapshot();
                    if self.send_status(pending.peer, false, snapshot, pending.deadline, clock)
                        && let Some(peer) = self.peers.get_mut(&pending.peer)
                    {
                        peer.deadline = 0;
                    }
                    Ok(())
                }
                Descriptor::Attach(columns, rows, lease, non_vt, token) => {
                    // The copied store frontier is the descriptor's storage
                    // linearization point. Release before policy materializes
                    // a potentially large replay effect list.
                    self.storage.release_status_snapshot();
                    let effects = self.machine.transition(Transition::Peer(
                        now,
                        pending.peer,
                        PolicyRequest::Attach(columns, rows, lease, non_vt, token),
                    ));
                    effects.map(|effects| {
                        self.status_snapshot = Some((snapshot, pending.deadline));
                        self.apply_with(effects, clock);
                        self.status_snapshot = None;
                    })
                }
            };
            if let Err(error) = result {
                self.refuse_wire(pending.peer, error);
            }
        }
    }

    pub(super) fn send_status(
        &mut self,
        id: u64,
        attach: bool,
        snapshot: StatusSnapshot,
        deadline: u64,
        clock: &mut impl FnMut() -> u64,
    ) -> bool {
        let policy = self.machine.status(id);
        let mut replay = policy.replay;
        replay.modes_exact = self.scanner.modes().exact();
        let health = snapshot.health;
        let (epoch, index, start, end) = snapshot
            .log
            .map(|commit| (commit.epoch, commit.index, commit.start, commit.end))
            .unwrap_or_default();
        let mut payload = Vec::with_capacity(self.status.len() + 69);
        payload.extend_from_slice(&self.status);
        if let Some(commit) = snapshot.event
            && let Some(fields) = payload.get_mut(self.commit_at..self.commit_at + 49)
        {
            fields[0] = commit.body;
            fields[1..9].copy_from_slice(&commit.index.to_le_bytes());
            fields[9..17].copy_from_slice(&commit.length.to_le_bytes());
            fields[17..49].copy_from_slice(&commit.hash);
        }
        payload.extend(
            StatusTail {
                replay,
                owns_lease: policy.owns_lease,
                viewers: policy.viewers,
                running: self.child_running,
                event_writable: health & 2 != 0,
                lease_epoch: policy.lease_epoch,
                semantic_flags: policy.semantic_flags,
                semantic_pending: policy.semantic_pending,
                extension: StatusExtension {
                    health: health & 1
                        | (health & 4) >> 1
                        | u8::from(self.scanner.exact()) << 2
                        | u8::from(policy.query_available) << 3,
                    log_epoch: epoch,
                    log_index: index,
                    retained_start: start,
                    retained_end: end,
                },
            }
            .encode()
            .expect("valid runtime status"),
        );
        if clock() >= deadline {
            self.disconnect(id);
            false
        } else {
            self.send(id, if attach { 4 } else { 14 }, &payload)
        }
    }

    pub fn retired(&mut self, unlinked: bool, survivor: bool) {
        let time = monotonic();
        self.tick(time);
        let _ = self.transition(Transition::Retired(unlinked, survivor));
    }
}

impl PreparedArtifacts {
    pub fn runtime<N: Native>(
        self,
        io: (Duplex, Receiver<(u64, Option<u16>)>),
        platform: (u8, N),
    ) -> Runtime<N> {
        let storage = SessionStorage::new(
            self.storage.log,
            self.storage.events,
            self.storage.lifecycle,
            64,
            4 << 20,
        );
        Runtime::new(HolderConfig {
            core: self.core,
            pty: io.0,
            writes: io.1,
            storage,
            status: self.status,
            commit_at: self.commit_at,
            synthetic: platform.0,
            native: platform.1,
        })
    }
}

#[cfg(test)]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/unit/runtime_holder.rs"
));
