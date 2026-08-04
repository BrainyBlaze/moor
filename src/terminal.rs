use crate::wire::recognize_query;
use std::fmt::Write as _;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Observation {
    Ready,
    State {
        state: &'static str,
        title: String,
        truncated: bool,
    },
    Link {
        uri: String,
        truncated: bool,
    },
    Query {
        class: u8,
        bytes: Vec<u8>,
    },
    Degraded {
        scanner: &'static str,
        reason: &'static str,
    },
}

#[derive(Clone, Debug)]
pub struct Modes {
    exact: bool,
    alternate: bool,
    g0_line: bool,
    g1_line: bool,
    wrap: bool,
    scroll: Option<(u16, u16)>,
    origin: bool,
    cursor_keys: bool,
    paste: bool,
    mouse: u8,
    mouse_encoding: u8,
    focus: bool,
    cursor_visible: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            exact: true,
            alternate: false,
            g0_line: false,
            g1_line: false,
            wrap: true,
            scroll: None,
            origin: false,
            cursor_keys: false,
            paste: false,
            mouse: 0,
            mouse_encoding: 0,
            focus: false,
            cursor_visible: true,
        }
    }
}

impl Modes {
    pub fn exact(&self) -> bool {
        self.exact
    }
    pub fn preamble(&self) -> Option<Vec<u8>> {
        if !self.exact {
            return None;
        }
        let mut out = String::new();
        flag(&mut out, 1049, self.alternate);
        out.push_str(if self.g0_line { "\x1b(0" } else { "\x1b(B" });
        out.push_str(if self.g1_line { "\x1b)0" } else { "\x1b)B" });
        flag(&mut out, 7, self.wrap);
        if let Some((top, bottom)) = self.scroll {
            write!(out, "\x1b[{top};{bottom}r").unwrap();
        } else {
            out.push_str("\x1b[r");
        }
        flag(&mut out, 6, self.origin);
        flag(&mut out, 1, self.cursor_keys);
        flag(&mut out, 2004, self.paste);
        for (mode, set) in [
            (1000, self.mouse & 1 != 0),
            (1002, self.mouse & 2 != 0),
            (1003, self.mouse & 4 != 0),
            (1005, self.mouse_encoding & 1 != 0),
            (1006, self.mouse_encoding & 2 != 0),
        ] {
            flag(&mut out, mode, set);
        }
        flag(&mut out, 1004, self.focus);
        flag(&mut out, 25, self.cursor_visible);
        Some(out.into_bytes())
    }
    fn private(&mut self, bytes: &[u8], set: bool) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            self.exact = false;
            return;
        };
        for value in text.split(';').filter(|value| !value.is_empty()) {
            let Ok(mode) = value.parse::<u32>() else {
                self.exact = false;
                return;
            };
            match mode {
                1 => self.cursor_keys = set,
                6 => self.origin = set,
                7 => self.wrap = set,
                25 => self.cursor_visible = set,
                1000 => bit(&mut self.mouse, 0, set),
                1002 => bit(&mut self.mouse, 1, set),
                1003 => bit(&mut self.mouse, 2, set),
                1004 => self.focus = set,
                1005 => bit(&mut self.mouse_encoding, 0, set),
                1006 => bit(&mut self.mouse_encoding, 1, set),
                1049 => self.alternate = set,
                2004 => self.paste = set,
                47 | 1047 | 1048 | 1001 => self.exact = false,
                _ => {}
            }
        }
    }
}

fn flag(out: &mut String, mode: u16, set: bool) {
    write!(out, "\x1b[?{mode}{}", if set { 'h' } else { 'l' }).unwrap();
}
fn bit(value: &mut u8, bit: u8, set: bool) {
    *value = (*value & !(1 << bit)) | (u8::from(set) << bit);
}

#[derive(Default)]
pub struct Scanner {
    rows: u16,
    candidate: Vec<u8>,
    started: u64,
    ready: bool,
    busy: Option<bool>,
    modes: Modes,
    episode: [bool; 2],
}

impl Scanner {
    pub fn new(rows: u16) -> Self {
        Self {
            rows,
            ..Self::default()
        }
    }
    pub fn modes(&self) -> &Modes {
        &self.modes
    }
    pub fn exact(&self) -> bool { self.episode == [false; 2] }
    pub fn feed(&mut self, now: u64, bytes: &[u8]) -> Vec<Observation> {
        let mut out = Vec::new();
        if !self.candidate.is_empty()
            && !is_osc(&self.candidate)
            && now >= self.started.saturating_add(50)
        {
            self.abandon("deadline", &mut out);
        }
        for byte in bytes {
            if self.candidate.is_empty() {
                if matches!(*byte, 0x1b | 0x9b | 0x9d) {
                    self.candidate.push(*byte);
                    self.started = now;
                } else {
                    self.episode = [false; 2];
                }
                continue;
            }
            if is_osc(&self.candidate) && matches!(*byte, 0x18 | 0x1a) {
                self.abandon("cancelled", &mut out);
                continue;
            }
            if is_osc(&self.candidate) && matches!(*byte, 0x9b | 0x9d) {
                self.abandon("malformed", &mut out);
                self.candidate.push(*byte);
                self.started = now;
                continue;
            }
            if is_osc(&self.candidate) && self.candidate.last() == Some(&0x1b) && *byte != b'\\' {
                self.abandon("malformed", &mut out);
                self.candidate.push(0x1b);
                self.started = now;
            }
            self.candidate.push(*byte);
            let cap = if is_osc(&self.candidate) { 65536 } else { 32 };
            if self.candidate.len() > cap {
                self.abandon("limit", &mut out);
                if matches!(*byte, 0x1b | 0x9b | 0x9d) {
                    self.candidate.push(*byte);
                    self.started = now;
                }
            } else if complete(&self.candidate) {
                let sequence = std::mem::take(&mut self.candidate);
                if self.process(&sequence, &mut out) {
                    self.episode[usize::from(!is_osc(&sequence))] = false;
                } else {
                    self.degraded("osc", "malformed", &mut out);
                }
            }
        }
        out
    }
    fn degraded(
        &mut self,
        scanner: &'static str,
        reason: &'static str,
        out: &mut Vec<Observation>,
    ) {
        let at = usize::from(scanner == "query");
        if !self.episode[at] {
            out.push(Observation::Degraded { scanner, reason });
            self.episode[at] = true;
        }
    }
    fn abandon(&mut self, reason: &'static str, out: &mut Vec<Observation>) {
        let scanner = if is_osc(&self.candidate) {
            "osc"
        } else {
            "query"
        };
        if self.candidate.starts_with(b"\x1b(")
            || self.candidate.starts_with(b"\x1b)")
            || csi_tail(&self.candidate).is_some()
        {
            self.modes.exact = false;
        }
        self.candidate.clear();
        self.degraded(scanner, reason, out);
    }
    fn process(&mut self, sequence: &[u8], out: &mut Vec<Observation>) -> bool {
        if let Some(shape) = recognize_query(sequence) {
            if !self.ready {
                self.ready = true;
                out.push(Observation::Ready);
            }
            out.push(Observation::Query {
                class: shape.class,
                bytes: sequence.to_vec(),
            });
            return true;
        }
        if let Some(body) = osc_body(sequence) {
            let separator = body.iter().position(|byte| *byte == b';');
            if separator.is_none_or(|at| at == 0 || !body[..at].iter().all(u8::is_ascii_digit)) {
                return false;
            }
            if body.starts_with(b"0;") || body.starts_with(b"2;") {
                let (title, truncated) = text(&body[2..], 255);
                let mut chars = title.chars();
                let busy = chars
                    .next()
                    .is_some_and(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch))
                    && chars.as_str().starts_with(' ');
                if self.busy != Some(busy) {
                    self.busy = Some(busy);
                    out.push(Observation::State {
                        state: if busy { "busy" } else { "idle" },
                        title,
                        truncated,
                    });
                }
            } else if body.starts_with(b"8;")
                && let Some(split) = body[2..].iter().position(|byte| *byte == b';')
            {
                let params = &body[2..2 + split];
                if params.len() > 1024 || params.iter().any(|byte| matches!(*byte, 7 | 0x1b | 0x9c))
                {
                    return false;
                }
                let (uri, truncated) = text(&body[3 + split..], 2048);
                out.push(Observation::Link { uri, truncated });
            } else if body.starts_with(b"8;") {
                return false;
            }
            return true;
        }
        if sequence == b"\x1bc" {
            self.modes = Modes::default();
            return true;
        }
        if sequence.len() == 3 && sequence[0] == 0x1b && matches!(sequence[1], b'(' | b')') {
            let line = match sequence[2] {
                b'0' => true,
                b'B' => false,
                _ => {
                    self.modes.exact = false;
                    return true;
                }
            };
            if sequence[1] == b'(' {
                self.modes.g0_line = line;
            } else {
                self.modes.g1_line = line;
            }
            return true;
        }
        let Some(tail) = csi_tail(sequence) else {
            return true;
        };
        if tail.starts_with(b"?") && matches!(tail.last(), Some(b'h' | b'l')) {
            self.modes
                .private(&tail[1..tail.len() - 1], tail.last() == Some(&b'h'));
        } else if tail.ends_with(b"r") {
            let values = &tail[..tail.len() - 1];
            if values.is_empty() {
                self.modes.scroll = None;
                return true;
            }
            let mut parts = values.split(|byte| *byte == b';');
            let top = decimal(parts.next().unwrap(), 1);
            let bottom = decimal(parts.next().unwrap_or_default(), self.rows as u32);
            let top = if top == 0 { 1 } else { top };
            let bottom = if bottom == 0 {
                self.rows as u32
            } else {
                bottom
            };
            if parts.next().is_some() || bottom > self.rows as u32 {
                self.modes.exact = false;
            } else if top == 1 && bottom == self.rows as u32 {
                self.modes.scroll = None;
            } else if bottom <= top {
                self.modes.exact = false;
            } else {
                self.modes.scroll = Some((top as u16, bottom as u16));
            }
        }
        true
    }
}

fn decimal(bytes: &[u8], default: u32) -> u32 {
    if bytes.is_empty() {
        return default;
    }
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(u32::MAX)
}
fn csi_tail(bytes: &[u8]) -> Option<&[u8]> {
    bytes
        .strip_prefix(b"\x1b[")
        .or_else(|| bytes.strip_prefix(&[0x9b]))
}
fn is_osc(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b]") || bytes.first() == Some(&0x9d)
}
fn complete(bytes: &[u8]) -> bool {
    if is_osc(bytes) {
        return bytes.last() == Some(&7)
            || bytes.last() == Some(&0x9c)
            || bytes.ends_with(b"\x1b\\");
    }
    if let Some(tail) = csi_tail(bytes) {
        return tail.last().is_some_and(|byte| (0x40..=0x7e).contains(byte));
    }
    let charset = matches!(bytes.get(1), Some(b'(' | b')'));
    bytes.len() >= if charset { 3 } else { 2 }
}
fn osc_body(bytes: &[u8]) -> Option<&[u8]> {
    let body = bytes
        .strip_prefix(b"\x1b]")
        .or_else(|| bytes.strip_prefix(&[0x9d]))?;
    body.strip_suffix(b"\x1b\\")
        .or_else(|| body.strip_suffix(&[7]))
        .or_else(|| body.strip_suffix(&[0x9c]))
}
fn text(bytes: &[u8], cap: usize) -> (String, bool) {
    let clean = String::from_utf8_lossy(bytes).replace('\0', "\u{fffd}");
    if clean.len() <= cap {
        return (clean, false);
    }
    let mut end = cap;
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    (clean[..end].to_owned(), true)
}
