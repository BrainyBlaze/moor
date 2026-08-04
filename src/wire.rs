#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Profile {
    Controller,
    Semantic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireError {
    UnknownVersion,
    UnknownType,
    OversizedFrame,
    OversizedMessage,
    Malformed,
    BadSequence,
    ReassemblyAborted,
    ReassemblyTimeout,
    ResourceExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub scope: u32,
    pub kind: u8,
    pub payload: Vec<u8>,
    pub fragmented: bool,
}

struct Run {
    scope: u32,
    kind: u8,
    payload: Vec<u8>,
    deadline: u64,
}

pub struct Codec {
    profile: Profile,
    buffer: Vec<u8>,
    next_in: u32,
    next_out: u32,
    run: Option<Run>,
}

impl Codec {
    pub fn new(profile: Profile) -> Self {
        Self::with_sequences(profile, 1, 1)
    }

    pub fn with_sequences(profile: Profile, next_in: u32, next_out: u32) -> Self {
        Self {
            profile,
            buffer: Vec::new(),
            next_in,
            next_out,
            run: None,
        }
    }

    fn limits(&self) -> (&'static [u8; 4], u8, usize, usize, u8) {
        match self.profile {
            Profile::Controller => (b"MOOR", 3, 1 << 20, 16 << 20, 0x1a),
            Profile::Semantic => (b"MOOS", 1, 1 << 16, 1 << 20, 0x0a),
        }
    }

    fn validate_kind(&self, kind: u8) -> Result<(), WireError> {
        let max = self.limits().4;
        if kind == 0 || kind > max {
            Err(WireError::UnknownType)
        } else {
            Ok(())
        }
    }

    fn validate_scope(&self, scope: u32, kind: u8) -> Result<(), WireError> {
        let zero_ok = match self.profile {
            Profile::Controller => kind == 1,
            Profile::Semantic => kind == 1 || kind == 9,
        };
        if scope == 0 && !zero_ok {
            Err(WireError::Malformed)
        } else {
            Ok(())
        }
    }

    fn exact(kind: u8) -> Option<usize> {
        match kind {
            0x15 => Some(40),
            0x16 => Some(24),
            0x17 | 0x18 => Some(20),
            0x19 => Some(24),
            0x1a => Some(32),
            _ => None,
        }
    }

    pub fn feed(
        &mut self,
        now_ms: u64,
        bytes: &[u8],
        out: &mut Vec<Message>,
    ) -> Result<(), WireError> {
        self.expire(now_ms)?;
        self.buffer.extend_from_slice(bytes);
        loop {
            if self.buffer.len() < 24 {
                return Ok(());
            }
            let (magic, version, frame_max, message_max, _) = self.limits();
            if &self.buffer[..4] != magic {
                return Err(WireError::Malformed);
            }
            if self.buffer[4] != version {
                return Err(WireError::UnknownVersion);
            }
            let kind = self.buffer[5];
            self.validate_kind(kind)?;
            let flags = self.buffer[6];
            if flags & !1 != 0 || self.buffer[7] != 0 {
                return Err(WireError::Malformed);
            }
            let more = flags == 1;
            if more && Self::exact(kind).is_some() {
                return Err(WireError::Malformed);
            }
            let scope = u32_at(&self.buffer, 8);
            self.validate_scope(scope, kind)?;
            let sequence = u32_at(&self.buffer, 12);
            if sequence != self.next_in || sequence == 0 || sequence == u32::MAX {
                return Err(if self.next_in == u32::MAX {
                    WireError::ResourceExhausted
                } else {
                    WireError::BadSequence
                });
            }
            let length = u32_at(&self.buffer, 16) as usize;
            if length > frame_max {
                return Err(WireError::OversizedFrame);
            }
            if u32_at(&self.buffer, 20) != crc32c(&self.buffer[..20]) {
                return Err(WireError::Malformed);
            }
            let total = 24usize
                .checked_add(length)
                .ok_or(WireError::OversizedFrame)?;
            if self.buffer.len() < total {
                return Ok(());
            }
            let frame: Vec<u8> = self.buffer.drain(..total).collect();
            self.next_in += 1;
            let payload = &frame[24..];
            if let Some(mut run) = self.run.take() {
                if run.kind != kind || run.scope != scope {
                    return Err(WireError::ReassemblyAborted);
                }
                if run.payload.len() + payload.len() > message_max {
                    return Err(WireError::OversizedMessage);
                }
                run.payload.extend_from_slice(payload);
                if more {
                    self.run = Some(run);
                } else {
                    validate_payload(kind, &run.payload)?;
                    out.push(Message {
                        scope,
                        kind,
                        payload: run.payload,
                        fragmented: true,
                    });
                }
            } else if more {
                self.run = Some(Run {
                    scope,
                    kind,
                    payload: payload.to_vec(),
                    deadline: now_ms.saturating_add(5_000),
                });
            } else {
                validate_payload(kind, payload)?;
                out.push(Message {
                    scope,
                    kind,
                    payload: payload.to_vec(),
                    fragmented: false,
                });
            }
        }
    }

    pub fn expire(&mut self, now_ms: u64) -> Result<(), WireError> {
        if self.run.as_ref().is_some_and(|run| now_ms >= run.deadline) {
            self.run = None;
            Err(WireError::ReassemblyTimeout)
        } else {
            Ok(())
        }
    }

    pub fn encode(
        &mut self,
        scope: u32,
        kind: u8,
        payload: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), WireError> {
        self.validate_kind(kind)?;
        self.validate_scope(scope, kind)?;
        validate_payload(kind, payload)?;
        let (_, _, frame_max, message_max, _) = self.limits();
        if payload.len() > message_max {
            return Err(WireError::OversizedMessage);
        }
        let chunks = if payload.is_empty() {
            1
        } else {
            payload.len().div_ceil(frame_max)
        };
        if Self::exact(kind).is_some() && chunks != 1 {
            return Err(WireError::Malformed);
        }
        if self.next_out == 0
            || self.next_out == u32::MAX
            || chunks > (u32::MAX - self.next_out) as usize
        {
            return Err(WireError::ResourceExhausted);
        }
        for part in 0..chunks {
            let start = part * frame_max;
            let end = payload.len().min(start + frame_max);
            let bytes = &payload[start..end];
            let mut header = [0u8; 24];
            let (magic, version, _, _, _) = self.limits();
            header[..4].copy_from_slice(magic);
            header[4] = version;
            header[5] = kind;
            header[6] = u8::from(part + 1 < chunks);
            header[8..12].copy_from_slice(&scope.to_le_bytes());
            header[12..16].copy_from_slice(&self.next_out.to_le_bytes());
            header[16..20].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
            let checksum = crc32c(&header[..20]);
            header[20..24].copy_from_slice(&checksum.to_le_bytes());
            out.extend_from_slice(&header);
            out.extend_from_slice(bytes);
            self.next_out += 1;
        }
        Ok(())
    }
}

fn validate_payload(kind: u8, payload: &[u8]) -> Result<(), WireError> {
    if Codec::exact(kind).is_some_and(|size| payload.len() != size) {
        Err(WireError::Malformed)
    } else {
        Ok(())
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f63b78 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Query {
    pub correlation: u64,
    pub epoch: u32,
    pub class: u8,
    pub bytes: Vec<u8>,
}

impl Query {
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        if self.correlation == 0 || !(1..=5).contains(&self.class) || self.bytes.len() > 4096 {
            return Err(WireError::Malformed);
        }
        let mut out = Vec::with_capacity(15 + self.bytes.len());
        out.extend_from_slice(&self.correlation.to_le_bytes());
        out.extend_from_slice(&self.epoch.to_le_bytes());
        out.push(self.class);
        out.extend_from_slice(&(self.bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.bytes);
        Ok(out)
    }
}

pub fn decode_query(payload: &[u8]) -> Result<Query, WireError> {
    if payload.len() < 15 {
        return Err(WireError::Malformed);
    }
    let length = u16::from_le_bytes(payload[13..15].try_into().unwrap()) as usize;
    let query = Query {
        correlation: u64::from_le_bytes(payload[..8].try_into().unwrap()),
        epoch: u32_at(payload, 8),
        class: payload[12],
        bytes: payload[15..].to_vec(),
    };
    if payload.len() != 15 + length {
        return Err(WireError::Malformed);
    }
    query.encode()?;
    Ok(query)
}

pub fn validate_status_flags(flags: u8) -> Result<(), WireError> {
    if flags & 0x0c == 0 {
        Ok(())
    } else {
        Err(WireError::Malformed)
    }
}

pub fn validate_attach_flags(flags: u8) -> Result<(), WireError> {
    if flags & !3 == 0 {
        Ok(())
    } else {
        Err(WireError::Malformed)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusExtension {
    pub health: u8,
    pub log_epoch: u32,
    pub log_index: u64,
    pub retained_start: u64,
    pub retained_end: u64,
}

impl StatusExtension {
    fn valid(&self, logging: bool) -> bool {
        if self.health & 0xf0 != 0 || self.retained_start > self.retained_end {
            return false;
        }
        if logging {
            self.log_index != 0
        } else {
            self.health & 1 == 0
                && self.log_epoch == 0
                && self.log_index == 0
                && self.retained_start == 0
                && self.retained_end == 0
        }
    }
    pub fn encode(&self, logging: bool) -> Result<Vec<u8>, WireError> {
        if !self.valid(logging) {
            return Err(WireError::Malformed);
        }
        let mut out = Vec::with_capacity(29);
        out.push(self.health);
        out.extend_from_slice(&self.log_epoch.to_le_bytes());
        out.extend_from_slice(&self.log_index.to_le_bytes());
        out.extend_from_slice(&self.retained_start.to_le_bytes());
        out.extend_from_slice(&self.retained_end.to_le_bytes());
        Ok(out)
    }
    pub fn decode(bytes: &[u8], logging: bool) -> Result<Self, WireError> {
        if bytes.len() != 29 {
            return Err(WireError::Malformed);
        }
        let value = Self {
            health: bytes[0],
            log_epoch: u32_at(bytes, 1),
            log_index: u64::from_le_bytes(bytes[5..13].try_into().unwrap()),
            retained_start: u64::from_le_bytes(bytes[13..21].try_into().unwrap()),
            retained_end: u64::from_le_bytes(bytes[21..29].try_into().unwrap()),
        };
        if value.valid(logging) {
            Ok(value)
        } else {
            Err(WireError::Malformed)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Heartbeat {
    pub monotonic_ms: u64,
    pub flags: u8,
}
impl Heartbeat {
    pub fn encode(self) -> Result<[u8; 9], WireError> {
        if self.flags & !0x1f != 0 {
            return Err(WireError::Malformed);
        }
        let mut out = [0; 9];
        out[..8].copy_from_slice(&self.monotonic_ms.to_le_bytes());
        out[8] = self.flags;
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() != 9 {
            return Err(WireError::Malformed);
        }
        let value = Self {
            monotonic_ms: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            flags: bytes[8],
        };
        value.encode()?;
        Ok(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryShape {
    pub class: u8,
    pub csi8: bool,
    pub mode: Option<u32>,
}

fn csi(bytes: &[u8]) -> Option<(bool, &[u8])> {
    if bytes.starts_with(b"\x1b[") {
        Some((false, &bytes[2..]))
    } else if bytes.first() == Some(&0x9b) {
        Some((true, &bytes[1..]))
    } else {
        None
    }
}
fn number(bytes: &[u8], max: u64) -> Option<u64> {
    if bytes.is_empty()
        || (bytes.len() > 1 && bytes[0] == b'0')
        || !bytes.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    std::str::from_utf8(bytes)
        .ok()?
        .parse::<u64>()
        .ok()
        .filter(|n| *n <= max)
}
fn numbers(bytes: &[u8], count: std::ops::RangeInclusive<usize>, max: u64, nonzero: bool) -> bool {
    let values: Vec<_> = bytes.split(|b| *b == b';').collect();
    count.contains(&values.len())
        && values
            .into_iter()
            .all(|v| number(v, max).is_some_and(|n| !nonzero || n != 0))
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
            Some(number(&tail[1..tail.len() - 2], u32::MAX as u64)? as u32),
        ),
        _ => return None,
    };
    Some(QueryShape { class, csi8, mode })
}

pub fn validate_query_reply(query: &QueryShape, bytes: &[u8]) -> bool {
    match query.class {
        1 => csi(bytes).is_some_and(|(_, t)| {
            t.starts_with(b"?")
                && t.ends_with(b"c")
                && numbers(&t[1..t.len() - 1], 1..=16, u16::MAX as u64, false)
        }),
        2 => csi(bytes).is_some_and(|(_, t)| {
            t.starts_with(b">")
                && t.ends_with(b"c")
                && numbers(&t[1..t.len() - 1], 3..=3, u32::MAX as u64, false)
        }),
        3 => {
            let text = if bytes.starts_with(b"\x1bP>|") && bytes.ends_with(b"\x1b\\") {
                &bytes[4..bytes.len() - 2]
            } else if bytes.starts_with(&[0x90, b'>', b'|']) && bytes.ends_with(&[0x9c]) {
                &bytes[3..bytes.len() - 1]
            } else {
                return false;
            };
            !text.is_empty() && text.len() <= 128 && text.iter().all(|b| (0x20..=0x7e).contains(b))
        }
        4 => csi(bytes).is_some_and(|(_, t)| {
            let Some(mode) = query.mode else {
                return false;
            };
            let prefix = format!("?{mode};");
            t.starts_with(prefix.as_bytes())
                && t.ends_with(b"$y")
                && t.len() == prefix.len() + 3
                && (b'0'..=b'4').contains(&t[prefix.len()])
        }),
        5 => csi(bytes).is_some_and(|(_, t)| {
            t.ends_with(b"R") && numbers(&t[..t.len() - 1], 2..=2, u16::MAX as u64, true)
        }),
        _ => false,
    }
}
