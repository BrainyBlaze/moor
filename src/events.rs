#[rustfmt::skip]
pub enum Json<'a> { String(&'a str), Bool(bool), Number(u64) }

pub struct Event(String, u64, String);

#[rustfmt::skip]
pub fn event(name: &str, ts_ms: u64, fields: &[(&str, Json<'_>)]) -> Result<Event, EventError> {
    let keys = fields.iter().map(|(key, _)| *key).collect::<Vec<_>>().join(",");
    if schema(name).is_none_or(|variants| !variants.split('|').any(|variant| variant == keys)) {
        return Err(EventError::InvalidEvent);
    }
    let mut tail = String::new();
    for (key, value) in fields {
        let value = match value { Json::String(text) => quoted(text), Json::Bool(v) => v.to_string(), Json::Number(v) => v.to_string() };
        tail.push_str(&format!(",{}:{value}", quoted(key)));
    }
    Ok(Event(quoted(name), ts_ms, tail))
}

#[rustfmt::skip]
fn schema(name: &str) -> Option<&'static str> { Some(match name {
    "ready" => "",
    "state" => "state,title,truncated",
    "link" => "uri,truncated",
    "semantic-source" => "source,producer,source_epoch,status,reason",
    "semantic-assertion" => "source,producer,source_epoch,source_seq,event_id,assertion_kind,payload",
    "application-receipt" => "source,producer,source_epoch,source_seq,event_id,application_request_id,lease_epoch,request_id,status,provider_session,provider_turn",
    "application-receipt-missing" => "source,producer,source_epoch,application_request_id,lease_epoch,request_id,reason",
    "stream-exhausted" => "axis",
    "exit" => "ended,code|ended,signal|ended,code,method",
    "observer-degraded" => "scanner,reason",
    _ => return None, }) }

#[rustfmt::skip]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind { Transition, Snapshot }

#[rustfmt::skip]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis { Sequence, Epoch, Commit }

#[derive(Clone, Copy)]
pub struct Limits(pub u64, pub u32, pub u64);

#[rustfmt::skip]
impl Default for Limits { fn default() -> Self { Self((1 << 53) - 1, u32::MAX, u64::MAX) } }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor(pub u32, pub u64, pub u64, pub u64);

#[rustfmt::skip]
pub struct Batch { pub records: Vec<String>, pub cursor: Cursor, pub exhausted: Option<Axis> }

#[rustfmt::skip]
#[derive(Debug, Eq, PartialEq)]
pub enum EventError { Closed, EmptyTransaction, InvalidState, InvalidEvent }

#[rustfmt::skip]
pub struct EventStream { cursor: Cursor, limits: Limits, closed: bool }

#[allow(clippy::new_without_default)]
impl EventStream {
    #[rustfmt::skip]
    pub fn new() -> Self { Self::at(Cursor(0, 0, 0, 1), Limits::default()) }

    #[rustfmt::skip]
    pub fn at(cursor: Cursor, limits: Limits) -> Self { Self { cursor, limits, closed: false } }

    #[rustfmt::skip]
    pub fn transact(&mut self, snapshots: Vec<Event>, transitions: Vec<Event>, compact: bool) -> Result<Batch, EventError> {
        if self.closed { return Err(EventError::Closed); }
        let observed = transitions.last().ok_or(EventError::EmptyTransaction)?.1;
        let count = transitions.len().checked_add(if compact { snapshots.len() } else { 0 })
            .and_then(|value| u64::try_from(value).ok()).ok_or(EventError::InvalidState)?;
        let Cursor(epoch, next, _, current_commit) = self.cursor;
        if epoch > self.limits.1 || next > self.limits.0 || current_commit == 0 {
            return Err(EventError::InvalidState);
        }
        let commit = current_commit.checked_add(1).filter(|value| *value <= self.limits.2)
            .ok_or(EventError::InvalidState)?;
        if next.checked_add(count).is_none_or(|end| end > self.limits.0) {
            return self.finish(Axis::Sequence, self.cursor,
                vec![(EventKind::Transition, exhausted(Axis::Sequence, observed))], commit);
        }
        if compact && epoch == self.limits.1 {
            let mut events = tagged(EventKind::Transition, transitions);
            events.push((EventKind::Transition, exhausted(Axis::Epoch, observed)));
            return self.finish(Axis::Epoch, self.cursor, events, commit);
        }
        let mut cursor = self.cursor;
        if compact {
            cursor.0 += 1;
            cursor.2 = cursor.1;
        }
        let mut events = tagged(EventKind::Snapshot, if compact { snapshots } else { vec![] });
        events.extend(tagged(EventKind::Transition, transitions));
        if commit == self.limits.2 {
            events.push((EventKind::Transition, exhausted(Axis::Commit, observed)));
            return self.finish(Axis::Commit, cursor, events, commit);
        }
        self.publish(None, cursor, events, commit)
    }

    #[rustfmt::skip]
    fn finish(&mut self, axis: Axis, cursor: Cursor, events: Vec<(EventKind, Event)>, commit: u64) -> Result<Batch, EventError> {
        self.publish(Some(axis), cursor, events, commit)
    }

    #[rustfmt::skip]
    fn publish(&mut self, exhausted: Option<Axis>, mut cursor: Cursor, events: Vec<(EventKind, Event)>, commit: u64) -> Result<Batch, EventError> {
        let count = u64::try_from(events.len()).map_err(|_| EventError::InvalidState)?;
        let records = serialize(cursor, events);
        cursor.1 = cursor.1.checked_add(count).ok_or(EventError::InvalidState)?;
        cursor.3 = commit;
        self.cursor = cursor;
        self.closed = exhausted.is_some();
        Ok(Batch { records, cursor, exhausted })
    }
}

#[rustfmt::skip]
fn tagged(kind: EventKind, events: Vec<Event>) -> Vec<(EventKind, Event)> { events.into_iter().map(|event| (kind, event)).collect() }

#[rustfmt::skip]
fn exhausted(axis: Axis, ts_ms: u64) -> Event {
    let axis = match axis { Axis::Sequence => "seq", Axis::Epoch => "epoch", Axis::Commit => "commit" };
    event("stream-exhausted", ts_ms, &[("axis", Json::String(axis))]).expect("static schema")
}

#[rustfmt::skip]
fn serialize(cursor: Cursor, events: Vec<(EventKind, Event)>) -> Vec<String> {
    events.iter().enumerate().map(|(offset, (kind, event))|
        canonical_event(cursor.0, cursor.1 + offset as u64, *kind, event)).collect()
}

#[rustfmt::skip]
pub fn canonical_header(created_ms: u64, session: &str, generation: Option<u32>, c: Cursor) -> String {
    let generation = generation.map_or_else(|| "null".into(), |value| value.to_string());
    format!(
        "{{\"v\":2,\"type\":\"header\",\"ts\":{},\"session\":{},\"generation\":{},\"epoch\":{},\"next_seq\":{},\"first_retained\":{}}}\n",
        timestamp(created_ms), quoted(session), generation, c.0, c.1, c.2
    )
}

#[rustfmt::skip]
pub fn canonical_event(epoch: u32, seq: u64, kind: EventKind, event: &Event) -> String {
    let kind = match kind { EventKind::Transition => "transition", EventKind::Snapshot => "snapshot" };
    let mut line = format!("{{\"type\":{},\"ts\":{},\"epoch\":{},\"seq\":{},\"kind\":{}{}", event.0, timestamp(event.1), epoch, seq, quoted(kind), event.2);
    line.push_str("}\n"); line
}

#[rustfmt::skip]
fn timestamp(ms: u64) -> String { match ms % 1000 { 0 => (ms / 1000).to_string(), fraction => format!("{}.{fraction:03}", ms / 1000) } }

#[rustfmt::skip]
fn quoted(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""), '\\' => out.push_str("\\\\"), '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"), '\n' => out.push_str("\\n"), '\u{c}' => out.push_str("\\f"), '\r' => out.push_str("\\r"),
            '\u{0}'..='\u{1f}' => {
                let scalar = character as usize;
                out.push_str("\\u00");
                out.push(HEX[scalar >> 4] as char);
                out.push(HEX[scalar & 15] as char);
            }
            _ => out.push(character),
        }
    }
    out.push('"');
    out
}
