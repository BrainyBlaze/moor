use crate::session::{
    ApplicationInput, ApplicationReceipt, InputNoticeAck, LeaseRequest, LeaseResult, OwnedInput,
    ReceiptProjection, Reply, Request as PolicyRequest, SemanticEvent, SemanticEventKind,
    SemanticHello, SemanticMode, SemanticRefusal, valid_source_id,
};
use bytes::BytesMut;
use smallvec::SmallVec;
use zerocopy::byteorder::{LE, U16, U32, U64};

macro_rules! wire_rules {
    (write $first:expr $(; $part:expr)*) => { [$first.as_ref(), $($part.as_ref()),*].concat() };
    (read $input:ident; $($name:ident = $value:expr);*) => { $(let $name = $value?;)* };
    (value $name:ident; $($field:ident = $value:expr);*) => { $name { $($field: $value),* } };
    (policy $name:ident; $($field:expr);*) => { PolicyRequest::$name($($field),*) };
    (controller $name:ident; $($field:expr);*) => { ControllerRequest::Policy(wire_rules!(policy $name; $($field);*)) };
    (checked $value:expr; $valid:expr) => {{ let value = $value; validated(($valid)(&value), value) }};
    (frame $kind:expr, $payload:expr) => { RuntimeReply::Frame($kind, $payload) };
    (frame $kind:expr; $first:expr $(; $part:expr)*) => { RuntimeReply::Frame($kind, wire_rules!(write $first $(; $part)*)) };
    (scoped $scope:expr, $kind:expr; $first:expr $(; $part:expr)*) => { RuntimeReply::Scoped($scope, $kind, wire_rules!(write $first $(; $part)*)) };
}

macro_rules! integer_readers {
    ($($name:ident: $kind:ty),*) => { $(
        pub(crate) fn $name(&mut self) -> Result<$kind, WireError> {
            Ok(<$kind>::from_le_bytes(self.exact()?))
        }
    )* };
}

schema!(enum pub Profile [Clone, Copy, Debug, Eq, PartialEq]; Controller, Semantic);
schema!(enum pub WireError [Clone, Debug, Eq, PartialEq]; UnknownVersion, UnknownType,
    OversizedFrame, OversizedMessage, Malformed, BadSequence, ReassemblyAborted,
    ReassemblyTimeout, ResourceExhausted);

pub(crate) fn require(valid: bool, error: WireError) -> Result<(), WireError> {
    valid.then_some(()).ok_or(error)
}

fn well_formed(valid: bool) -> Result<(), WireError> {
    require(valid, WireError::Malformed)
}

fn validated<T>(valid: bool, value: T) -> Result<T, WireError> {
    valid.then_some(value).ok_or(WireError::Malformed)
}

fn nonzero(bytes: &[u8]) -> bool {
    bytes.iter().any(|byte| *byte != 0)
}

fn ordinal<T: Copy>(value: u8, values: &[T]) -> Result<T, WireError> {
    values
        .get(value as usize)
        .copied()
        .ok_or(WireError::Malformed)
}

pub(crate) fn fixed_payload<const N: usize>(
    fields: &[(usize, &[u8])],
) -> Result<[u8; N], WireError> {
    let mut out = [0; N];
    for (at, bytes) in fields {
        let end = at
            .checked_add(bytes.len())
            .filter(|end| *end <= N)
            .ok_or(WireError::Malformed)?;
        out[*at..end].copy_from_slice(bytes);
    }
    Ok(out)
}

schema!(struct pub Message derive [Clone, Debug, Eq, PartialEq] pub fields; scope: u32, kind: u8, payload: BytesMut);

pub(crate) struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], WireError> {
        let value = self.0.get(..length).ok_or(WireError::Malformed)?;
        self.0 = &self.0[length..];
        Ok(value)
    }
    pub(crate) fn exact<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    integer_readers!(byte: u8, u16: u16, u32: u32, u64: u64);
    fn identifier<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let value = self.exact()?;
        validated(nonzero(&value), value)
    }
    fn positive(&mut self) -> Result<u32, WireError> {
        let value = self.u32()?;
        validated(value != 0, value)
    }
    pub(crate) fn wide(&mut self) -> Result<&'a [u8], WireError> {
        let length = self.u32()? as usize;
        require(length <= 1 << 20, WireError::OversizedMessage)?;
        self.take(length)
    }
    pub(crate) fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.0)
    }
    pub(crate) fn end(self) -> Result<(), WireError> {
        well_formed(self.0.is_empty())
    }
    fn finish<T>(self, value: T) -> Result<T, WireError> {
        self.end().map(|()| value)
    }
}

fn limits(profile: Profile) -> (&'static [u8; 4], u8, usize, usize, u8) {
    match profile {
        Profile::Controller => (b"MOOR", 3, 1 << 20, 16 << 20, 0x1a),
        Profile::Semantic => (b"MOOS", 1, 1 << 16, 1 << 20, 0x0a),
    }
}

fn message_size(profile: Profile, scope: u32, kind: u8) -> Result<Option<usize>, WireError> {
    require(
        kind != 0 && kind <= limits(profile).4,
        WireError::UnknownType,
    )?;
    let zero = matches!(
        (profile, kind),
        (Profile::Controller, 1) | (Profile::Semantic, 1 | 9)
    );
    well_formed(scope != 0 || zero)?;
    Ok(match (profile, kind) {
        (Profile::Controller, 10) => Some(43),
        (Profile::Controller, 0x15) => Some(40),
        (Profile::Controller, 0x16 | 0x19) => Some(24),
        (Profile::Controller, 0x17 | 0x18) => Some(20),
        (Profile::Controller, 0x1a) => Some(32),
        _ => None,
    })
}

schema!(struct FrameHeader fields; magic: [u8; 4], version: u8, kind: u8, more: u8,
    reserved: u8, scope: u32, sequence: u32, length: u32, checksum: u32);
binary_record!(RawFrameHeader => FrameHeader[24] error WireError = WireError::Malformed; fixed {}
    fields { magic: [u8; 4], version: u8, kind: u8, more: u8, reserved: u8, scope: U32<LE>,
    sequence: U32<LE>, length: U32<LE>, checksum: U32<LE> });

fn frame_header(
    profile: Profile,
    next: u32,
    bytes: &[u8],
) -> Result<Option<FrameHeader>, WireError> {
    let Some(raw) = bytes.get(..24) else {
        return Ok(None);
    };
    let header = FrameHeader::decode_raw(raw)?;
    let (magic, version, frame_max, _, _) = limits(profile);
    well_formed(&header.magic == magic)?;
    require(header.version == version, WireError::UnknownVersion)?;
    let fixed = message_size(profile, header.scope, header.kind)?;
    well_formed(header.more <= 1 && header.reserved == 0)?;
    well_formed(header.more == 0 || fixed.is_none())?;
    let error = if next == u32::MAX {
        WireError::ResourceExhausted
    } else {
        WireError::BadSequence
    };
    require(
        header.sequence == next && !matches!(next, 0 | u32::MAX),
        error,
    )?;
    require(
        header.length as usize <= frame_max,
        WireError::OversizedFrame,
    )?;
    well_formed(header.checksum == crc32c(&bytes[..20]))?;
    Ok((bytes.len() >= 24 + header.length as usize).then_some(header))
}

schema!(struct pub Codec fields; profile: Profile, buffer: BytesMut, next_in: u32, next_out: u32, run: Option<Message>, deadline: Option<u64>);

impl Codec {
    pub fn new(profile: Profile) -> Self {
        Self::with_sequences(profile, 1, 1)
    }

    pub fn with_sequences(profile: Profile, next_in: u32, next_out: u32) -> Self {
        Self {
            profile,
            buffer: BytesMut::new(),
            next_in,
            next_out,
            run: None,
            deadline: None,
        }
    }

    pub(crate) fn profile(&self) -> Profile {
        self.profile
    }

    pub fn buffered_len(&self) -> usize {
        self.buffer.len() + self.run.as_ref().map_or(0, |run| run.payload.len())
    }

    pub fn projected_len(&self, incoming: usize) -> Option<usize> {
        self.buffered_len().checked_add(incoming)
    }

    pub fn feed(
        &mut self,
        now_ms: u64,
        bytes: &[u8],
        out: &mut Vec<Message>,
    ) -> Result<(), WireError> {
        self.expire(now_ms)?;
        self.buffer.extend_from_slice(bytes);
        let message_max = limits(self.profile).3;
        while let Some(header) = frame_header(self.profile, self.next_in, &self.buffer)? {
            let length = header.length as usize;
            let mut frame = self.buffer.split_to(24 + length);
            self.next_in += 1;
            let payload = frame.split_off(24);
            let message = match self.run.take() {
                None => {
                    wire_rules!(value Message; scope = header.scope; kind = header.kind; payload = payload)
                }
                Some(mut message) => {
                    require(
                        message.kind == header.kind && message.scope == header.scope,
                        WireError::ReassemblyAborted,
                    )?;
                    let size = message
                        .payload
                        .len()
                        .checked_add(length)
                        .ok_or(WireError::OversizedMessage)?;
                    require(size <= message_max, WireError::OversizedMessage)?;
                    message.payload.unsplit(payload);
                    message
                }
            };
            if header.more != 0 {
                self.run = Some(message);
            } else {
                out.push(message);
                self.deadline = None;
            }
        }
        let deadline = self.deadline.unwrap_or(now_ms.saturating_add(5_000));
        self.deadline = (!self.buffer.is_empty() || self.run.is_some()).then_some(deadline);
        Ok(())
    }

    pub fn expire(&mut self, now_ms: u64) -> Result<(), WireError> {
        let Some(_) = self.deadline.take_if(|deadline| now_ms >= *deadline) else {
            return Ok(());
        };
        self.buffer.clear();
        self.run = None;
        Err(WireError::ReassemblyTimeout)
    }

    pub fn encode(
        &mut self,
        scope: u32,
        kind: u8,
        payload: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), WireError> {
        let fixed = message_size(self.profile, scope, kind)?;
        let (magic, version, frame_max, message_max, _) = limits(self.profile);
        require(payload.len() <= message_max, WireError::OversizedMessage)?;
        well_formed(fixed.is_none_or(|size| payload.len() == size))?;
        let chunks = payload.len().max(1).div_ceil(frame_max);
        let sequences = self.next_out != 0
            && self.next_out != u32::MAX
            && chunks <= (u32::MAX - self.next_out) as usize;
        require(sequences, WireError::ResourceExhausted)?;
        out.reserve(payload.len() + chunks * 24);
        for part in 0..chunks {
            let start = part * frame_max;
            let end = payload.len().min(start + frame_max);
            let bytes = &payload[start..end];
            let header = wire_rules!(value FrameHeader; magic = *magic; version = version; kind = kind; more = u8::from(part + 1 < chunks); reserved = 0; scope = scope; sequence = self.next_out; length = bytes.len() as u32; checksum = 0);
            let mut header = header.encode_raw();
            let checksum = crc32c(&header[..20]);
            header[20..].copy_from_slice(&checksum.to_le_bytes());
            out.extend_from_slice(&header);
            out.extend_from_slice(bytes);
            self.next_out += 1;
        }
        Ok(())
    }
}

pub fn lease_token_payload(epoch: u32, token: [u8; 16]) -> Result<[u8; 20], WireError> {
    well_formed(epoch != 0 && nonzero(&token))?;
    fixed_payload(&[(0, &epoch.to_le_bytes()), (4, &token)])
}

pub fn log_clear_payload(incarnation: [u8; 16], observed: u64) -> Result<[u8; 24], WireError> {
    well_formed(nonzero(&incarnation))?;
    fixed_payload(&[(0, &incarnation), (16, &observed.to_le_bytes())])
}

schema!(struct LogClearResult fields; outcome: u8, reason: u8, reserved: [u8; 2], epoch: u32,
    prior: u64, resulting: u64, cleared: u64);
binary_record!(RawLogClearResult => LogClearResult[32] error WireError = WireError::Malformed;
    fixed {} fields { outcome: u8, reason: u8, reserved: [u8; 2], epoch: U32<LE>,
        prior: U64<LE>, resulting: U64<LE>, cleared: U64<LE> });
impl LogClearResult {
    fn valid(&self) -> bool {
        self.reserved == [0; 2]
            && matches!(
                (self.outcome, self.reason, self.epoch != 0),
                (0, 0, true) | (1, 0, _) | (2, 1..=3, _)
            )
    }
}

pub fn log_clear_result_payload(
    outcome: u8,
    reason: u8,
    epoch: u32,
    prior: u64,
    resulting: u64,
    cleared: u64,
) -> Result<[u8; 32], WireError> {
    let value = wire_rules!(value LogClearResult; outcome = outcome; reason = reason; reserved = [0; 2]; epoch = epoch; prior = prior; resulting = resulting; cleared = cleared);
    Ok(validated(value.valid(), value)?.encode_raw())
}

schema!(struct pub InputReceipt derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; epoch: u32, request: u64,
    generation: u32, incarnation: [u8; 16], written: u64, status: u8, result: u16);

binary_record!(RawInputReceipt => InputReceipt[43] error WireError = WireError::Malformed;
    fixed {} fields { epoch: U32<LE>, request: U64<LE>, generation: U32<LE>, incarnation: [u8; 16], written: U64<LE>, status: u8,
        result: U16<LE> });

impl InputReceipt {
    fn valid(&self) -> bool {
        self.epoch != 0
            && self.request != 0
            && self.generation != 0
            && nonzero(&self.incarnation)
            && matches!((self.status, self.result), (0, 0) | (1, 1..=20))
    }

    pub fn outcome(
        epoch: u32,
        request: u64,
        generation: u32,
        incarnation: [u8; 16],
        written: u64,
        error: Option<u16>,
    ) -> Self {
        wire_rules!(value Self; epoch = epoch; request = request; generation = generation; incarnation = incarnation; written = written; status = u8::from(error.is_some()); result = error.unwrap_or(0))
    }

    pub fn encode(self) -> Result<[u8; 43], WireError> {
        Ok(validated(self.valid(), self)?.encode_raw())
    }

    pub fn decode(payload: &[u8]) -> Result<Self, WireError> {
        wire_rules!(checked Self::decode_raw(payload)?; Self::valid)
    }
}

pub fn decode_log_clear_result(payload: &[u8]) -> Result<(u8, u8, u64), WireError> {
    let value = wire_rules!(checked LogClearResult::decode_raw(payload)?; LogClearResult::valid)?;
    Ok((value.outcome, value.reason, value.prior))
}

fn valid_termination([outcome, containment, method]: [u8; 3], diagnostic: &[u8]) -> bool {
    outcome <= 4
        && containment & !0x0f == 0
        && method <= 2
        && diagnostic.is_empty() == (outcome <= 1)
}

pub fn terminate_result_payload(
    outcome: u8,
    containment: u8,
    method: u8,
    diagnostic: &[u8],
) -> Result<Vec<u8>, WireError> {
    let header = [outcome, containment, method];
    well_formed(valid_termination(header, diagnostic))?;
    with_wide(header.to_vec(), diagnostic)
}

pub fn decode_terminate_result(payload: &[u8]) -> Result<(u8, u8, u8, &[u8]), WireError> {
    let mut input = Reader(payload);
    let [outcome, containment, method] = input.exact()?;
    let diagnostic = input.wide()?;
    validated(
        valid_termination([outcome, containment, method], diagnostic),
        input.finish((outcome, containment, method, diagnostic))?,
    )
}

pub fn put_wide(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), WireError> {
    require(bytes.len() <= 1 << 20, WireError::OversizedMessage)?;
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn with_wide(mut out: Vec<u8>, bytes: &[u8]) -> Result<Vec<u8>, WireError> {
    out.reserve(4 + bytes.len());
    put_wide(&mut out, bytes)?;
    Ok(out)
}

pub(crate) fn join(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

schema!(enum pub RuntimeReply; Frame(u8, Vec<u8>), Scoped(u32, u8, Vec<u8>));

pub fn encode_reply(reply: Reply, incarnation: [u8; 16]) -> RuntimeReply {
    use Reply::*;
    match reply {
        Lease(result) => wire_rules!(frame 0x16, result.encode_wire().unwrap().to_vec()),
        Input(payload) => wire_rules!(frame 10, payload),
        Notice(notice) => {
            wire_rules!(frame 5; notice.receipt.application_id; notice.receipt.lease_epoch.to_le_bytes(); notice.receipt.request_id.to_le_bytes(); notice.byte_count.to_le_bytes(); notice.digest)
        }
        NoticeCancel(receipt) => {
            wire_rules!(frame 10; receipt.application_id; receipt.lease_epoch.to_le_bytes(); receipt.request_id.to_le_bytes(); 21u32.to_le_bytes(); b"terminal write failed")
        }
        SemanticAck(ack) => {
            let (epoch, sequence) = ack
                .position
                .map(|position| (position.epoch, position.sequence))
                .unwrap_or_default();
            wire_rules!(frame 7; ack.id; ack.sequence.to_le_bytes(); [ack.status as u8]; [0; 2]; epoch.to_le_bytes(); sequence.to_le_bytes(); [0; 4])
        }
        SemanticRefused(event, error) => {
            let code = semantic_code(error);
            if let Some(event) = event {
                wire_rules!(frame 7; event.id; event.sequence.to_le_bytes(); [2]; code.to_le_bytes(); [0; 12]; 22u32.to_le_bytes(); b"semantic event refused")
            } else {
                wire_rules!(frame 9, error_payload(code, b"semantic request refused"))
            }
        }
        SemanticHello(ack) => {
            wire_rules!(scoped ack.epoch, 2; incarnation; [u8::from(ack.snapshot_required)]; 32768u32.to_le_bytes(); 5000u32.to_le_bytes(); 60000u32.to_le_bytes(); 600000u32.to_le_bytes())
        }
        ControllerError(code, diagnostic) => {
            wire_rules!(frame 13, error_payload(code, diagnostic))
        }
        Termination(outcome, containment, method, diagnostic) => wire_rules!(
            frame 16,
            terminate_result_payload(outcome, containment, method, diagnostic).unwrap()
        ),
    }
}

fn semantic_code(error: SemanticRefusal) -> u16 {
    [10, 5, 5, 6, 12, 7, 8, 11, 14, 6, 8, 9, 10][error as usize]
}

pub fn error_payload(code: u16, diagnostic: &[u8]) -> Vec<u8> {
    wire_rules!(write code.to_le_bytes(); (diagnostic.len() as u32).to_le_bytes(); diagnostic)
}

pub fn get_wide(bytes: &[u8], at: usize, exact_tail: bool) -> Option<&[u8]> {
    let mut input = Reader(bytes.get(at..)?);
    let value = input.wide().ok()?;
    (!exact_tail || input.end().is_ok()).then_some(value)
}

pub fn controller_hello(identity: &[u8]) -> Result<Vec<u8>, WireError> {
    with_wide(b"MOOR\x03\0\0".to_vec(), identity)
}

pub fn controller_hello_ack(
    generation: u32,
    incarnation: [u8; 16],
    identity: &[u8],
) -> Result<Vec<u8>, WireError> {
    well_formed(generation != 0 && nonzero(&incarnation))?;
    with_wide(
        wire_rules!(write [3]; generation.to_le_bytes(); incarnation),
        identity,
    )
}

pub fn decode_controller_hello_ack(
    scope: u32,
    payload: &[u8],
    identity: &[u8],
) -> Option<(u32, [u8; 16])> {
    let mut input = Reader(payload);
    let (accepted, generation, incarnation) = (
        input.byte().ok()?,
        input.u32().ok()?,
        input.identifier().ok()?,
    );
    (accepted == 3
        && generation != 0
        && scope == generation
        && input.wide().ok()? == identity
        && input.end().is_ok())
    .then_some((generation, incarnation))
}

pub use crc32c::crc32c;

schema!(struct pub Query derive [Clone, Debug, Eq, PartialEq] pub fields;
    correlation: u64, epoch: u32, class: u8, bytes: Vec<u8>);

impl Query {
    fn valid(&self) -> bool {
        valid_query(self.correlation, self.epoch, self.class, &self.bytes)
    }
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        well_formed(self.valid())?;
        Ok(
            wire_rules!(write self.correlation.to_le_bytes(); self.epoch.to_le_bytes(); [self.class]; (self.bytes.len() as u16).to_le_bytes(); self.bytes),
        )
    }
}

fn valid_query(correlation: u64, epoch: u32, class: u8, bytes: &[u8]) -> bool {
    correlation != 0 && epoch != 0 && (1..=5).contains(&class) && bytes.len() <= 4096
}

fn read_query<'a>(input: &mut Reader<'a>) -> Result<(u64, u32, u8, &'a [u8]), WireError> {
    let fields = (input.u64()?, input.u32()?, input.byte()?);
    let length = input.u16()? as usize;
    let bytes = input.take(length)?;
    well_formed(valid_query(fields.0, fields.1, fields.2, bytes))?;
    Ok((fields.0, fields.1, fields.2, bytes))
}

pub fn decode_query(payload: &[u8]) -> Result<Query, WireError> {
    let mut input = Reader(payload);
    let (correlation, epoch, class, bytes) = read_query(&mut input)?;
    input.finish(
        wire_rules!(value Query; correlation = correlation; epoch = epoch; class = class; bytes = bytes.to_vec()),
    )
}

schema!(enum pub ViewerEvent<'a> [Debug, Eq, PartialEq]; Terminal(&'a [u8]), Output(u64, bool, &'a [u8]),
    Receipt(InputReceipt), Lease(LeaseResult));
schema!(struct pub ViewerStream derive [Default] pub fields; non_vt: bool, terminal: bool,
    replay: Option<ReplayDescriptor>, next: Option<(u64, u64)>, lease_epoch: Option<u32>,
    queries: SmallVec<[(Query, QueryShape); 4]>, probe: Vec<u8>);

pub fn decode_viewer<'a>(
    stream: &mut ViewerStream,
    message: &'a Message,
    expected: (&[u8], u32, [u8; 16]),
) -> Result<Option<ViewerEvent<'a>>, WireError> {
    use ViewerEvent::*;
    let mut input = Reader(&message.payload);
    let event = match message.kind {
        4 => {
            let replay =
                StatusTail::decode_for(&message.payload, expected.0, expected.1, expected.2)?
                    .replay;
            let consistent = stream.next.is_none_or(|(sequence, offset)| {
                if replay.first == 0 {
                    offset == replay.end
                } else {
                    replay.first <= sequence
                        && sequence <= replay.last.saturating_add(1)
                        && (sequence != replay.first || offset == replay.start)
                        && (sequence != replay.last.saturating_add(1) || offset == replay.end)
                }
            });
            well_formed(stream.replay.is_none() && stream.terminal && consistent)?;
            if stream.next.is_none() && replay.first <= 1 {
                let offset = if replay.first == 0 {
                    replay.end
                } else {
                    replay.start
                };
                stream.next = Some((1, offset));
            }
            stream.replay = Some(replay);
            None
        }
        5 => {
            let length = input.u16()? as usize;
            let bytes = input.rest();
            well_formed(
                !stream.terminal
                    && stream.replay.is_none()
                    && bytes.len() == length
                    && (!stream.non_vt || bytes.is_empty()),
            )?;
            stream.terminal = true;
            Some(Terminal(bytes))
        }
        6 => {
            let (sequence, offset) = (input.u64()?, input.u64()?);
            let bytes = input.rest();
            well_formed((1..=65536).contains(&bytes.len()))?;
            let end = offset
                .checked_add(bytes.len() as u64)
                .ok_or(WireError::Malformed)?;
            let replay = stream.replay.ok_or(WireError::Malformed)?;
            let (expected, expected_offset) = stream.next.ok_or(WireError::Malformed)?;
            let apply = sequence >= expected;
            let valid = (replay.first..=replay.last).contains(&sequence)
                && offset >= replay.start
                && end <= replay.end
                && (sequence != replay.first || offset == replay.start)
                && (sequence != replay.last || end == replay.end)
                && if apply {
                    sequence == expected && offset == expected_offset
                } else {
                    end <= expected_offset && (sequence + 1 != expected || end == expected_offset)
                };
            well_formed(valid)?;
            if apply {
                stream.next = sequence.checked_add(1).map(|next| (next, end));
            }
            Some(Output(sequence, apply, bytes))
        }
        8 => {
            let (first, last) = (input.u64()?, input.u64()?);
            let replay = stream.replay.ok_or(WireError::Malformed)?;
            well_formed(
                first == 1
                    && replay.first.checked_sub(1) == Some(last)
                    && stream
                        .next
                        .is_none_or(|(sequence, _)| sequence >= replay.first),
            )?;
            input.end()?;
            stream.next.get_or_insert((replay.first, replay.start));
            None
        }
        10 => Some(Receipt(InputReceipt::decode(&message.payload)?)),
        0x14 => {
            let query = decode_query(&message.payload)?;
            let shape = recognize_query(&query.bytes).ok_or(WireError::Malformed)?;
            well_formed(query.class == shape.class && Some(query.epoch) == stream.lease_epoch)?;
            stream.queries.push((query, shape));
            None
        }
        0x16 => {
            well_formed(stream.replay.is_some())?;
            Some(Lease(LeaseResult::decode_wire(&message.payload)?))
        }
        _ => None,
    };
    Ok(event)
}

impl ViewerStream {
    pub fn input(&mut self, bytes: Vec<u8>) -> (Vec<u8>, SmallVec<[Query; 2]>) {
        let mut ordinary = Vec::with_capacity(bytes.len() + self.probe.len());
        let mut replies = SmallVec::new();
        if self.queries.is_empty() {
            ordinary.append(&mut self.probe);
        }
        for byte in bytes {
            if self.probe.is_empty()
                && (self.queries.is_empty() || !matches!(byte, 0x1b | 0x90 | 0x9b))
            {
                ordinary.push(byte);
                continue;
            }
            self.probe.push(byte);
            match reply_state(&self.probe) {
                1 => {
                    let reply = std::mem::take(&mut self.probe);
                    let matched = self
                        .queries
                        .iter()
                        .position(|(_, shape)| validate_query_reply(shape, &reply))
                        .map(|at| self.queries.remove(at));
                    if let Some((mut query, _)) = matched {
                        query.bytes = reply;
                        replies.push(query);
                    } else {
                        ordinary.extend(reply);
                    }
                }
                -1 => ordinary.append(&mut self.probe),
                _ => {}
            }
        }
        (ordinary, replies)
    }

    pub fn flush_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.probe)
    }

    pub fn disconnected(&mut self) {
        self.queries.clear();
    }
}

fn reply_state(bytes: &[u8]) -> i8 {
    match bytes {
        [0x1b] => 0,
        [0x1b, b'P', ..] | [0x90, ..] if bytes.ends_with(b"\x1b\\") => 1,
        [0x1b, b'P', ..] | [0x90, ..] if bytes.ends_with(&[0x9c]) => 1,
        [0x1b, b'P', ..] | [0x90, ..] => (bytes.len() <= 256).then_some(0).unwrap_or(-1),
        [0x1b, b'[', body @ ..] | [0x9b, body @ ..] => match body.last() {
            Some(0x40..=0x7e) => 1,
            Some(0x20..=0x3f) | None if bytes.len() <= 256 => 0,
            _ => -1,
        },
        _ => -1,
    }
}

schema!(enum pub ControllerRequest<'a>; Hello(&'a [u8]), Policy(PolicyRequest<'a>), Status,
    LogClear([u8; 16], u64));

pub fn decode_controller(
    kind: u8,
    payload: &[u8],
    token: Option<[u8; 16]>,
) -> Result<ControllerRequest<'_>, WireError> {
    use ControllerRequest::*;
    let mut input = Reader(payload);
    let request = match kind {
        1 => {
            well_formed(input.exact::<7>()? == *b"MOOR\x03\0\0")?;
            Hello(input.wide()?)
        }
        3 => {
            let (columns, rows, flags) = (input.u16()?, input.u16()?, input.byte()?);
            well_formed(flags & !3 == 0)?;
            wire_rules!(controller Attach; columns; rows; flags & 1 != 0; flags & 2 != 0; token)
        }
        7 => wire_rules!(controller OutputAck; input.u64()?),
        9 => {
            let (epoch, request_id, form) = (input.u32()?, input.u64()?, input.byte()?);
            let exact_payload = payload.into();
            let application = match form {
                0 => {
                    input.rest();
                    None
                }
                1 => {
                    let application_id = input.exact()?;
                    let source = input.wide()?;
                    well_formed(nonzero(&application_id) && valid_source_id(source))?;
                    let terminal_at = payload.len() - input.rest().len();
                    Some(
                        wire_rules!(value ApplicationInput; receipt = wire_rules!(value ApplicationReceipt; application_id = application_id; lease_epoch = epoch; request_id = request_id); source = terminal_at - source.len()..terminal_at; terminal_at = terminal_at),
                    )
                }
                _ => return Err(WireError::Malformed),
            };
            let input = wire_rules!(value OwnedInput; epoch = epoch; request_id = request_id; exact_payload = exact_payload);
            wire_rules!(controller Input; input; application)
        }
        11 => wire_rules!(controller Resize; input.u32()?; input.u16()?; input.u16()?),
        12 => {
            let (correlation, epoch, class, bytes) = read_query(&mut input)?;
            wire_rules!(controller QueryReply; correlation; epoch; class; bytes)
        }
        13 => Status,
        15 => {
            wire_rules!(read input; identity = input.wide(); generation = input.u32(); incarnation = input.exact(); force = input.byte());
            well_formed(force <= 1)?;
            wire_rules!(controller Terminate; identity; generation; incarnation; force != 0)
        }
        0x15 => wire_rules!(controller Lease; LeaseRequest::decode_wire(input.rest())?; token),
        0x17 => wire_rules!(controller Release; input.positive()?; input.identifier()?),
        0x18 => wire_rules!(controller Keepalive; input.positive()?; input.identifier()?),
        0x19 => LogClear(input.identifier()?, input.u64()?),
        _ => return Err(WireError::Malformed),
    };
    input.finish(request)
}

pub fn decode_semantic(
    scope: u32,
    kind: u8,
    payload: &[u8],
) -> Result<PolicyRequest<'_>, WireError> {
    let mut input = Reader(payload);
    let request = match kind {
        1 if scope == 0 => {
            let (token, producer, generation) = (input.exact()?, input.exact()?, input.u32()?);
            let mode = ordinal(input.byte()?, &[SemanticMode::Edge, SemanticMode::Stateful])?;
            wire_rules!(policy SemanticHello; wire_rules!(value SemanticHello; token = token; producer = producer; generation = generation; mode = mode; capabilities = input.byte()?; source = input.wide()?.into()))
        }
        3 => {
            let (id, sequence) = (input.exact()?, input.u64()?);
            let kind = ordinal(
                input.byte()?,
                &[SemanticEventKind::Transition, SemanticEventKind::Snapshot],
            )?;
            wire_rules!(policy SemanticEvent; wire_rules!(value SemanticEvent; id = id; sequence = sequence; kind = kind; exact_payload = input.rest().into()); None)
        }
        4 => {
            wire_rules!(read input; id = input.exact(); sequence = input.u64(); application_id = input.exact(); lease_epoch = input.u32(); request_id = input.u64(); status = input.byte());
            well_formed(status <= 1)?;
            wire_rules!(read input; session = input.wide(); turn = input.wide());
            well_formed(session.len() <= 4096 && turn.len() <= 4096)?;
            let session = 33..33 + session.len();
            let turn = session.end + 4..session.end + 4 + turn.len();
            wire_rules!(policy SemanticEvent; wire_rules!(value SemanticEvent; id = id; sequence = sequence; kind = SemanticEventKind::ApplicationReceipt; exact_payload = payload[24..].into()); Some(wire_rules!(value ReceiptProjection; receipt = wire_rules!(value ApplicationReceipt; application_id = application_id; lease_epoch = lease_epoch; request_id = request_id); status = status; provider_session = session; provider_turn = turn)))
        }
        6 => {
            wire_rules!(read input; application_id = input.exact(); lease_epoch = input.u32(); request_id = input.u64(); prepared = input.byte());
            well_formed(prepared <= 1)?;
            wire_rules!(policy NoticeAck; wire_rules!(value InputNoticeAck; receipt = wire_rules!(value ApplicationReceipt; application_id = application_id; lease_epoch = lease_epoch; request_id = request_id); prepared = prepared == 0))
        }
        8 => {
            input.exact::<8>()?;
            PolicyRequest::SemanticHeartbeat
        }
        _ => return Err(WireError::Malformed),
    };
    input.finish(request)
}

pub fn validate_status_flags(flags: u8) -> Result<(), WireError> {
    well_formed(flags & 0x0c == 0)
}

schema!(struct pub StatusExtension derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; health: u8, log_epoch: u32,
    log_index: u64, retained_start: u64, retained_end: u64);
binary_record!(RawStatusExtension => StatusExtension[29] error WireError = WireError::Malformed;
    fixed {} fields { health: u8, log_epoch: U32<LE>, log_index: U64<LE>, retained_start: U64<LE>, retained_end: U64<LE> });

impl StatusExtension {
    fn logging(&self) -> bool {
        self.log_epoch != 0
            || self.log_index != 0
            || self.retained_start != 0
            || self.retained_end != 0
    }

    fn valid(&self, logging: bool) -> bool {
        self.health & 0xf0 == 0
            && self.retained_start <= self.retained_end
            && if logging {
                self.log_epoch != 0 && self.log_index != 0
            } else {
                self.health & 1 == 0 && !self.logging()
            }
    }
    pub fn encode(&self, logging: bool) -> Result<[u8; 29], WireError> {
        Ok(validated(self.valid(logging), *self)?.encode_raw())
    }
    pub fn decode(bytes: &[u8], logging: bool) -> Result<Self, WireError> {
        wire_rules!(checked Self::decode_raw(bytes)?; |value: &Self| value.valid(logging))
    }
}

schema!(struct pub ReplayDescriptor derive [Clone, Copy, Debug, Eq, PartialEq] pub fields;
    first: u64, last: u64, start: u64, end: u64, complete: bool, modes_exact: bool);
schema!(struct pub StatusTail derive [Clone, Debug, Eq, PartialEq] pub fields; replay: ReplayDescriptor,
    owns_lease: bool, viewers: bool, running: bool, event_writable: bool, lease_epoch: u32,
    semantic_flags: u8, semantic_pending: u16, extension: StatusExtension);
schema!(struct TailRecord fields; first: u64, last: u64, start: u64, end: u64, flags: u8,
    lease_epoch: u32, semantic_flags: u8, semantic_pending: u16, extension: [u8; 29]);
binary_record!(RawStatusTail => TailRecord[69] error WireError = WireError::Malformed; fixed {}
    fields { first: U64<LE>, last: U64<LE>, start: U64<LE>, end: U64<LE>, flags: u8,
    lease_epoch: U32<LE>, semantic_flags: u8, semantic_pending: U16<LE>, extension: [u8; 29] });

impl StatusTail {
    fn valid(&self) -> bool {
        let replay = self.replay;
        let range = replay.first == 0 && replay.last == 0 && replay.start == replay.end
            || replay.first != 0 && replay.first <= replay.last && replay.start < replay.end;
        range
            && replay.complete == (replay.first <= 1 && replay.start == 0)
            && self.semantic_flags & !7 == 0
            && self.semantic_pending <= 512
            && (!self.owns_lease || self.lease_epoch != 0)
    }
    pub fn encode(&self) -> Result<[u8; 69], WireError> {
        well_formed(self.valid())?;
        let flags = u8::from(self.replay.complete)
            | u8::from(self.replay.modes_exact) << 1
            | u8::from(self.owns_lease) << 4
            | u8::from(self.viewers) << 5
            | u8::from(self.running) << 6
            | u8::from(self.event_writable) << 7;
        Ok(wire_rules!(value TailRecord; first = self.replay.first; last = self.replay.last; start = self.replay.start; end = self.replay.end; flags = flags; lease_epoch = self.lease_epoch; semantic_flags = self.semantic_flags; semantic_pending = self.semantic_pending; extension = self.extension.encode(self.extension.logging())?).encode_raw())
    }

    fn decode_with(
        payload: &[u8],
        expected: Option<(&[u8], u32, [u8; 16])>,
    ) -> Result<Self, WireError> {
        let mut input = Reader(payload);
        validate_status_base(&mut input, expected)?;
        let record = TailRecord::decode_raw(input.rest())?;
        validate_status_flags(record.flags)?;
        let replay = wire_rules!(value ReplayDescriptor; first = record.first; last = record.last; start = record.start; end = record.end; complete = record.flags & 1 != 0; modes_exact = record.flags & 2 != 0);
        let extension = wire_rules!(checked StatusExtension::decode_raw(&record.extension)?; |value: &StatusExtension| value.valid(value.logging()))?;
        let value = wire_rules!(value Self; replay = replay; owns_lease = record.flags & 1 << 4 != 0; viewers = record.flags & 1 << 5 != 0; running = record.flags & 1 << 6 != 0; event_writable = record.flags & 1 << 7 != 0; lease_epoch = record.lease_epoch; semantic_flags = record.semantic_flags; semantic_pending = record.semantic_pending; extension = extension);
        validated(value.valid(), value)
    }
    pub fn decode(payload: &[u8]) -> Result<Self, WireError> {
        Self::decode_with(payload, None)
    }
    pub fn decode_for(
        payload: &[u8],
        identity: &[u8],
        generation: u32,
        incarnation: [u8; 16],
    ) -> Result<Self, WireError> {
        Self::decode_with(payload, Some((identity, generation, incarnation)))
    }
}

fn validate_status_base(
    input: &mut Reader<'_>,
    expected: Option<(&[u8], u32, [u8; 16])>,
) -> Result<(), WireError> {
    wire_rules!(read input; identity = input.wide(); generation = input.positive(); incarnation = input.identifier::<16>(); layout = input.byte(); event_identity = input.wide(); slot = input.byte(); commit = input.u64(); body_length = input.u64(); body_hash = input.exact::<32>());
    input.exact::<32>()?;
    wire_rules!(read input; directory = input.wide(); _pid = input.positive(); _containment = input.positive(); _birth = input.identifier::<16>());
    let identity_ok =
        matches!(identity, [1, b'/', ..]) || identity.len() == 25 && identity.first() == Some(&2);
    let commit_ok = if layout == 2 {
        slot <= 1 && commit != 0 && body_length != 0 && nonzero(&body_hash)
    } else {
        slot == 0xff && commit == 0 && body_length == 0 && !nonzero(&body_hash)
    };
    well_formed(
        identity_ok
            && expected.is_none_or(|value| (identity, generation, incarnation) == value)
            && layout <= 2
            && event_identity.is_empty() == (layout == 0)
            && commit_ok
            && !directory.is_empty(),
    )
}

schema!(struct pub Heartbeat derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; monotonic_ms: u64, flags: u8);
binary_record!(RawHeartbeat => Heartbeat[9] error WireError = WireError::Malformed; fixed {} fields { monotonic_ms: U64<LE>, flags: u8 });
impl Heartbeat {
    pub fn encode(self) -> Result<[u8; 9], WireError> {
        Ok(validated(self.flags & !0x1f == 0, self)?.encode_raw())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        wire_rules!(checked Self::decode_raw(bytes)?; |value: &Self| value.flags & !0x1f == 0)
    }
}

schema!(struct pub QueryShape derive [Clone, Copy, Debug, Eq, PartialEq] pub fields;
    class: u8, csi8: bool, mode: Option<u32>);

pub(crate) fn csi(bytes: &[u8]) -> Option<(bool, &[u8])> {
    bytes
        .strip_prefix(b"\x1b[")
        .map(|tail| (false, tail))
        .or_else(|| bytes.strip_prefix(&[0x9b]).map(|tail| (true, tail)))
}
fn csi_body<'a>(bytes: &'a [u8], prefix: &[u8], suffix: &[u8]) -> Option<&'a [u8]> {
    csi(bytes)?.1.strip_prefix(prefix)?.strip_suffix(suffix)
}
pub(crate) fn decimal(bytes: &[u8], max: u64, canonical: bool) -> Option<u64> {
    (!bytes.is_empty() && (!canonical || bytes.len() == 1 || bytes[0] != b'0')).then_some(())?;
    bytes.iter().try_fold(0u64, |value, byte| {
        let digit = byte.checked_sub(b'0').filter(|digit| *digit < 10)? as u64;
        value
            .checked_mul(10)?
            .checked_add(digit)
            .filter(|value| *value <= max)
    })
}
fn numbers(bytes: &[u8], count: std::ops::RangeInclusive<usize>, max: u64, nonzero: bool) -> bool {
    let mut fields = 0;
    let valid = bytes.split(|b| *b == b';').all(|value| {
        fields += 1;
        decimal(value, max, true).is_some_and(|n| !nonzero || n != 0)
    });
    valid && count.contains(&fields)
}

pub fn recognize_query(bytes: &[u8]) -> Option<QueryShape> {
    let (csi8, tail) = csi(bytes)?;
    let (class, mode) = match tail {
        b"c" | b"0c" => (1, None),
        b">c" | b">0c" => (2, None),
        b">0q" => (3, None),
        b"6n" => (5, None),
        _ if tail.starts_with(b"?") && tail.ends_with(b"$p") => (
            4,
            Some(decimal(&tail[1..tail.len() - 2], u32::MAX as u64, true)? as u32),
        ),
        _ => return None,
    };
    Some(wire_rules!(value QueryShape; class = class; csi8 = csi8; mode = mode))
}

pub fn validate_query_reply(query: &QueryShape, bytes: &[u8]) -> bool {
    match query.class {
        1 => csi_body(bytes, b"?", b"c")
            .is_some_and(|body| numbers(body, 1..=16, u16::MAX as u64, false)),
        2 => csi_body(bytes, b">", b"c")
            .is_some_and(|body| numbers(body, 3..=3, u32::MAX as u64, false)),
        3 => [
            (b"\x1bP>|".as_slice(), b"\x1b\\".as_slice()),
            (&[0x90, b'>', b'|'], &[0x9c]),
        ]
        .into_iter()
        .find_map(|(head, tail)| bytes.strip_prefix(head)?.strip_suffix(tail))
        .is_some_and(|text| {
            !text.is_empty() && text.len() <= 128 && text.iter().all(|b| (0x20..=0x7e).contains(b))
        }),
        4 => csi_body(bytes, b"?", b"$y").is_some_and(|body| {
            let Some(split) = body.iter().rposition(|byte| *byte == b';') else {
                return false;
            };
            decimal(&body[..split], u32::MAX as u64, true).map(|mode| mode as u32) == query.mode
                && matches!(&body[split + 1..], [b'0'..=b'4'])
        }),
        5 => csi_body(bytes, b"", b"R")
            .is_some_and(|body| numbers(body, 2..=2, u16::MAX as u64, true)),
        _ => false,
    }
}
