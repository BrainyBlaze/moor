use crate::wire::{csi, decimal as parse_decimal, recognize_query};
use std::io::Write as _;

const PRIVATE: [u32; 12] = [1, 6, 7, 25, 1000, 1002, 1003, 1004, 1005, 1006, 1049, 2004];

schema!(enum pub Observation [Clone, Debug, Eq, PartialEq]; Ready, State(&'static str, String, bool), Link(String, bool),
    Query(u8, Vec<u8>), Degraded(&'static str, &'static str));
schema!(enum pub Scan [Clone, Debug, Eq, PartialEq]; Observation(Observation), Release(Vec<u8>));

schema!(struct default pub Modes derive [Clone, Debug] fields; inexact: bool = false,
    charset: [bool; 2] = [false; 2], private: u16 = 0b1100, scroll: Option<(u16, u16)> = None);

impl Modes {
    pub fn exact(&self) -> bool {
        !self.inexact
    }
    pub fn preamble(&self) -> Option<Vec<u8>> {
        return_if!(self.inexact, None);
        let mut out = Vec::with_capacity(192);
        flag(&mut out, 1049, self.has(1049));
        for (group, line) in [(b'(', self.charset[0]), (b')', self.charset[1])] {
            out.extend([0x1b, group, if line { b'0' } else { b'B' }]);
        }
        flag(&mut out, 7, self.has(7));
        match self.scroll {
            Some((top, bottom)) => write!(out, "\x1b[{top};{bottom}r").unwrap(),
            None => out.extend_from_slice(b"\x1b[r"),
        }
        for mode in [6u32, 1, 2004] {
            flag(&mut out, mode as u16, self.has(mode));
        }
        // Schema §6 groups 9 and 10 clear every constituent mode first and only
        // then set the tracked ones, so an arbitrary combination left in the
        // viewer cannot survive because the child selected a different member.
        for group in [[1000u32, 1002, 1003].as_slice(), &[1005, 1006]] {
            for mode in group {
                flag(&mut out, *mode as u16, false);
            }
            for mode in group.iter().filter(|mode| self.has(**mode)) {
                flag(&mut out, *mode as u16, true);
            }
        }
        for mode in [1004u32, 25] {
            flag(&mut out, mode as u16, self.has(mode));
        }
        Some(out)
    }
    pub fn query(&self, mode: u32, csi8: bool) -> Option<Vec<u8>> {
        return_if!(self.inexact, None);
        let state = mode_bit(mode).map_or(0, |bit| if self.private & bit == 0 { 2 } else { 1 });
        let mut out = if csi8 { vec![0x9b] } else { b"\x1b[".to_vec() };
        write!(out, "?{mode};{state}$y").unwrap();
        Some(out)
    }
    fn has(&self, mode: u32) -> bool {
        mode_bit(mode).is_some_and(|bit| self.private & bit != 0)
    }
    fn private(&mut self, bytes: &[u8], set: bool) {
        let mut updated = self.private;
        for value in bytes.split(|byte| *byte == b';').filter(|v| !v.is_empty()) {
            let Some(mode) = parse_decimal(value, u32::MAX as u64, false)
                .map(|mode| mode as u32)
                .filter(|mode| !matches!(*mode, 47 | 1047 | 1048 | 1001))
            else {
                return self.inexact = true;
            };
            if let Some(bit) = mode_bit(mode) {
                updated = if set { updated | bit } else { updated & !bit };
            }
        }
        self.private = updated;
    }
}

fn mode_bit(mode: u32) -> Option<u16> {
    PRIVATE.binary_search(&mode).ok().map(|at| 1 << at)
}

fn flag(out: &mut Vec<u8>, mode: u16, set: bool) {
    write!(out, "\x1b[?{mode}{}", if set { 'h' } else { 'l' }).unwrap();
}

schema!(struct pub Scanner derive [Default] fields; rows: u16, buf: smallvec::SmallVec<[u8; 36]>, sent: usize, since: u64,
    ready: bool, busy: Option<bool>, modes: Modes, episode: [bool; 2]);

impl Scanner {
    pub fn new(rows: u16) -> Self {
        Self {
            rows,
            ..Self::default()
        }
    }
    pub fn set_rows(&mut self, rows: u16) {
        self.rows = rows;
    }
    pub fn modes(&self) -> &Modes {
        &self.modes
    }
    pub fn exact(&self) -> bool {
        self.episode == [false; 2]
    }
    pub fn expire(&mut self, now: u64) -> Vec<Scan> {
        if now < self.since.saturating_add(50) {
            return Vec::new();
        }
        let mut out = Vec::new();
        if is_osc(&self.buf) && self.buf.last() == Some(&0x1b) {
            self.buf.pop();
            self.abandon("deadline", &mut out);
            self.begin(0x1b, now, &mut out);
        } else if self.sent < self.buf.len() {
            self.abandon("deadline", &mut out);
        }
        out
    }
    pub fn scan_owned(&mut self, now: u64, bytes: Vec<u8>) -> Vec<Scan> {
        if self.buf.is_empty()
            && !bytes.is_empty()
            && !bytes.iter().any(|byte| matches!(byte, 0x1b | 0x9b | 0x9d))
        {
            self.episode = [false; 2];
            vec![Scan::Release(bytes)]
        } else {
            self.scan(now, &bytes)
        }
    }
    pub fn scan(&mut self, now: u64, bytes: &[u8]) -> Vec<Scan> {
        let mut out = self.expire(now);
        for &byte in bytes {
            if self.buf.is_empty() {
                if !self.begin(byte, now, &mut out) {
                    self.episode = [false; 2];
                    release(&mut out, &[byte]);
                }
                continue;
            }
            let osc = is_osc(&self.buf);
            let osc_escape = osc && self.buf.last() == Some(&0x1b);
            if osc && matches!(byte, 0x18 | 0x1a) {
                self.abandon("cancelled", &mut out);
                release(&mut out, &[byte]);
                continue;
            }
            if osc && matches!(byte, 0x9b | 0x9d) {
                self.abandon("malformed", &mut out);
                self.begin(byte, now, &mut out);
                continue;
            }
            if osc_escape && byte != b'\\' {
                self.buf.pop();
                self.abandon("malformed", &mut out);
                self.begin(0x1b, now, &mut out);
            }
            self.buf.push(byte);
            if is_osc(&self.buf) && byte == 0x1b {
                self.since = now;
            }
            let osc = is_osc(&self.buf);
            let cap = if osc { 65536 } else { 32 };
            if self.buf.len() > cap {
                let reprocess = is_intro(byte);
                if reprocess {
                    self.buf.pop();
                }
                self.abandon("limit", &mut out);
                if reprocess {
                    self.begin(byte, now, &mut out);
                }
            } else {
                self.emit_safe(&mut out);
                if complete(&self.buf) {
                    let mut sequence = std::mem::take(&mut self.buf);
                    let emitted = std::mem::take(&mut self.sent);
                    let osc = is_osc(&sequence);
                    if self.process(&sequence, &mut out) {
                        self.episode[usize::from(!osc)] = false;
                    } else {
                        self.degraded(osc, "malformed", &mut out);
                    }
                    release(&mut out, &sequence[emitted..]);
                    sequence.clear();
                    self.buf = sequence;
                }
            }
        }
        out
    }
    fn begin(&mut self, byte: u8, now: u64, out: &mut Vec<Scan>) -> bool {
        return_if!(!is_intro(byte), false);
        self.buf.push(byte);
        self.sent = 0;
        self.since = now;
        self.emit_safe(out);
        true
    }
    fn emit_safe(&mut self, out: &mut Vec<Scan>) {
        let safe = if is_osc(&self.buf) {
            self.buf.len() - usize::from(self.buf.last() == Some(&0x1b))
        } else if self.buf.len() == 1 || possible_query(&self.buf) {
            0
        } else {
            self.buf.len()
        };
        release(out, &self.buf[self.sent..safe]);
        self.sent = safe;
    }
    fn degraded(&mut self, osc: bool, reason: &'static str, out: &mut Vec<Scan>) {
        let (scanner, at) = if osc { ("osc", 0) } else { ("query", 1) };
        if !self.episode[at] {
            observe(out, Observation::Degraded(scanner, reason));
            self.episode[at] = true;
        }
    }
    fn abandon(&mut self, reason: &'static str, out: &mut Vec<Scan>) {
        let osc = is_osc(&self.buf);
        if self.buf.starts_with(b"\x1b(")
            || self.buf.starts_with(b"\x1b)")
            || csi(&self.buf).is_some()
        {
            self.modes.inexact = true;
        }
        if osc {
            release(out, &self.buf[self.sent..]);
        }
        self.degraded(osc, reason, out);
        if !osc {
            release(out, &self.buf[self.sent..]);
        }
        self.buf.clear();
        self.sent = 0;
    }
    fn process(&mut self, sequence: &[u8], out: &mut Vec<Scan>) -> bool {
        if let Some(shape) = recognize_query(sequence) {
            if !self.ready {
                self.ready = true;
                observe(out, Observation::Ready);
            }
            observe(out, Observation::Query(shape.class, sequence.to_vec()));
            return true;
        }
        if is_osc(sequence) {
            return osc_body(sequence)
                .and_then(|body| self.osc(body, out))
                .is_some();
        }
        match sequence {
            b"\x1bc" => self.modes = Modes::default(),
            [0x1b, group @ (b'(' | b')'), value] => match value {
                b'0' | b'B' => self.modes.charset[usize::from(*group == b')')] = *value == b'0',
                _ => self.modes.inexact = true,
            },
            _ => {
                if let Some((_, tail)) = csi(sequence) {
                    self.csi(tail)
                }
            }
        }
        true
    }
    fn osc(&mut self, body: &[u8], out: &mut Vec<Scan>) -> Option<()> {
        let split = body.iter().position(|byte| *byte == b';')?;
        let (selector, value) = (&body[..split], &body[split + 1..]);
        (!selector.is_empty() && selector.iter().all(u8::is_ascii_digit)).then_some(())?;
        if matches!(selector, b"0" | b"2") {
            let (title, truncated) = text(value, 255);
            let mut chars = title.chars();
            let busy = chars
                .next()
                .is_some_and(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch))
                && chars.as_str().starts_with(' ');
            if self.busy != Some(busy) {
                self.busy = Some(busy);
                observe(
                    out,
                    Observation::State(if busy { "busy" } else { "idle" }, title, truncated),
                );
            }
        } else if selector == b"8" {
            let split = value.iter().position(|byte| *byte == b';')?;
            let params = &value[..split];
            (params.len() <= 1024 && !params.iter().any(|byte| matches!(*byte, 7 | 0x1b | 0x9c)))
                .then_some(())?;
            let (uri, truncated) = text(&value[split + 1..], 2048);
            observe(out, Observation::Link(uri, truncated));
        }
        Some(())
    }
    fn csi(&mut self, tail: &[u8]) {
        if let [b'?', values @ .., set @ (b'h' | b'l')] = tail {
            self.modes.private(values, *set == b'h');
            return;
        }
        let Some(values) = tail.strip_suffix(b"r") else {
            return;
        };
        if values.is_empty() {
            self.modes.scroll = None;
            return;
        }
        let mut parts = values.split(|byte| *byte == b';');
        let top = decimal(parts.next().unwrap(), 1).max(1);
        let bottom = match decimal(parts.next().unwrap_or_default(), self.rows as u32) {
            0 => self.rows as u32,
            bottom => bottom,
        };
        if parts.next().is_some() || bottom > self.rows as u32 {
            self.modes.inexact = true;
        } else if top == 1 && bottom == self.rows as u32 {
            self.modes.scroll = None;
        } else if bottom > top {
            self.modes.scroll = Some((top as u16, bottom as u16));
        } else {
            self.modes.inexact = true;
        }
    }
}

fn decimal(bytes: &[u8], default: u32) -> u32 {
    bytes
        .is_empty()
        .then_some(default)
        .or_else(|| parse_decimal(bytes, u32::MAX as u64, false).map(|value| value as u32))
        .unwrap_or(u32::MAX)
}
fn possible_query(bytes: &[u8]) -> bool {
    let Some((_, tail)) = csi(bytes) else {
        return false;
    };
    recognize_query(bytes).is_some()
        || match tail {
            b"" | b"0" | b">" | b">0" | b"6" | b"?" => true,
            [b'?', rest @ ..] => {
                let digits = rest.strip_suffix(b"$").unwrap_or(rest);
                parse_decimal(digits, u32::MAX as u64, true).is_some()
            }
            _ => false,
        }
}
fn is_osc(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b]") || bytes.starts_with(&[0x9d])
}
fn is_intro(byte: u8) -> bool {
    matches!(byte, 0x1b | 0x9b | 0x9d)
}
fn complete(bytes: &[u8]) -> bool {
    if is_osc(bytes) {
        matches!(bytes.last(), Some(7 | 0x9c)) || bytes.ends_with(b"\x1b\\")
    } else if let Some((_, tail)) = csi(bytes) {
        tail.last().is_some_and(|byte| (0x40..=0x7e).contains(byte))
    } else {
        bytes.len() >= 2 + usize::from(matches!(bytes.get(1), Some(b'(' | b')')))
    }
}
fn osc_body(bytes: &[u8]) -> Option<&[u8]> {
    let body = bytes
        .strip_prefix(b"\x1b]")
        .or_else(|| bytes.strip_prefix(&[0x9d]))?;
    body.strip_suffix(b"\x1b\\")
        .or_else(|| body.strip_suffix(&[7]))
        .or_else(|| body.strip_suffix(&[0x9c]))
}
fn release(out: &mut Vec<Scan>, bytes: &[u8]) {
    if let Some(Scan::Release(prior)) = out.last_mut() {
        prior.extend_from_slice(bytes);
    } else if !bytes.is_empty() {
        out.push(Scan::Release(bytes.to_vec()));
    }
}
fn observe(out: &mut Vec<Scan>, observation: Observation) {
    out.push(Scan::Observation(observation));
}
fn text(bytes: &[u8], cap: usize) -> (String, bool) {
    let mut clean = String::with_capacity(bytes.len().min(cap));
    for chunk in bytes.utf8_chunks() {
        for ch in chunk
            .valid()
            .chars()
            .chain((!chunk.invalid().is_empty()).then_some('\u{fffd}'))
        {
            let ch = if ch == '\0' { '\u{fffd}' } else { ch };
            if clean.len() + ch.len_utf8() > cap {
                return (clean, true);
            }
            clean.push(ch);
        }
    }
    (clean, false)
}
