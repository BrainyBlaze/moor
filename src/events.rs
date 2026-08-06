use crate::{
    canonical_u64 as decimal,
    session::{
        ReceiptProjection, SemanticChange, SemanticEffect, SemanticEvent, SemanticEventKind,
        SourceEffect,
    },
};
use base64::{Engine as _, display::Base64Display, engine::general_purpose::STANDARD};
use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::fmt::Write as _;
use std::sync::Arc;

schema!(enum pub Json<'a>; String(&'a str), Bool(bool), Number(u64));

schema!(tuple pub Event [Clone]; fields; &'static str, u64, Arc<str>, Option<(usize, Arc<[u8]>)>);

impl Event {
    pub(crate) fn retention(&self) -> Option<(usize, &Arc<[u8]>)> {
        self.3.as_ref().map(|(class, source)| (*class, source))
    }
}

schema!(enum pub EventKind [Clone, Copy, Debug, Eq, PartialEq]; Transition, Snapshot);
schema!(enum pub Axis [Clone, Copy, Debug, Eq, PartialEq]; Sequence, Epoch, Commit);
schema!(enum pub EventError [Debug, Eq, PartialEq]; Closed, EmptyTransaction, InvalidState, InvalidEvent);

schema!(tuple pub Cursor [Clone, Copy, Debug, Eq, PartialEq]; fields pub; u32, u64, u64, u64);
schema!(tuple pub(crate) Stored [Clone, Copy]; fields pub; u32, u32, u64, u64, u64);
const STORE_EVENT_END: u64 = 1 << 53;
const STORE_EVENT_CAP: u64 = 256 << 10;
const STORE_HEADER: &str =
    "v:2,type:=header,ts:*,session:*,generation:*,epoch:u,next_seq:*,first_retained:*";
const STORE_LIFECYCLE: &str = "v:1,type:=lifecycle,phase:t,session:*,generation:*,wire_generation:u,incarnation:b16,start_wall_ms:D,start_mono_ms:D,boot_id:b16,path_encoding:=posix-bytes/windows-wtf8,event_path:n,instrument_path:n";
const STORE_LIFECYCLE_END: &str = "|end_wall_ms:D,output_end:D,ended:=exited,code:u|end_wall_ms:D,output_end:D,ended:=signalled,signal:p|end_wall_ms:D,output_end:D,ended:=terminated,code:u,method:=graceful/forced";

pub fn event(name: &'static str, ts: u64, fields: &[(&str, Json<'_>)]) -> Event {
    let mut tail = String::with_capacity(fields.len().saturating_mul(32));
    for (key, value) in fields {
        write!(tail, ",\"{key}\":").expect("string writes cannot fail");
        match value {
            Json::String(value) => tail.push_str(&quoted(value)),
            Json::Bool(value) => tail.push_str(if *value { "true" } else { "false" }),
            Json::Number(value) => write!(tail, "{value}").expect("string writes cannot fail"),
        }
    }
    Event(name, ts, tail.into(), None)
}

macro_rules! semantic_event {
    ($name:literal, $ts:expr, $source:expr, $producer:expr, $epoch:expr, $retention:expr; $($tail:tt)*) => {{
        let source_text = std::str::from_utf8($source).map_err(|_| EventError::InvalidEvent)?;
        let mut tail = format!(
            ",\"source\":{},\"producer\":\"{}\",\"source_epoch\":{}",
            quoted(source_text), Base64Display::new(&$producer, &STANDARD), $epoch
        );
        write!(tail, $($tail)*).expect("string writes cannot fail");
        Ok(Event($name, $ts, tail.into(), $retention))
    }};
}

pub fn semantic_assertion(
    ts: u64,
    source: &[u8],
    producer: [u8; 16],
    source_epoch: u32,
    event: &SemanticEvent,
) -> Result<Event, EventError> {
    let snapshot = event.kind == SemanticEventKind::Snapshot;
    let payload = &event.exact_payload;
    (payload.len() <= 32768 && json_object(payload, 64, 1024).is_some())
        .then_some(())
        .ok_or(EventError::InvalidEvent)?;
    semantic_event!("semantic-assertion", ts, source, producer, source_epoch, snapshot.then(|| (1, source.into()));
        ",\"source_seq\":\"{}\",\"event_id\":\"{}\",\"assertion_kind\":\"{}\",\"payload\":\"{}\"",
        event.sequence,
        Base64Display::new(&event.id, &STANDARD),
        if snapshot { "snapshot" } else { "transition" },
        Base64Display::new(payload, &STANDARD)
    )
}

pub fn application_receipt(
    ts: u64,
    source: &[u8],
    producer: [u8; 16],
    epoch: u32,
    semantic: &SemanticEvent,
    projection: &ReceiptProjection,
) -> Result<Event, EventError> {
    let (receipt, status) = (projection.receipt, projection.status);
    let payload = &semantic.exact_payload;
    let providers = payload
        .get(projection.provider_session.clone())
        .zip(payload.get(projection.provider_turn.clone()));
    let (provider_session, provider_turn) = providers
        .filter(|(session, turn)| status <= 1 && session.len() <= 4096 && turn.len() <= 4096)
        .ok_or(EventError::InvalidEvent)?;
    let (lease, request) = (receipt.lease_epoch, receipt.request_id);
    semantic_event!("application-receipt", ts, source, producer, epoch, None;
        ",\"source_seq\":\"{}\",\"event_id\":\"{}\",\"application_request_id\":\"{}\",\"lease_epoch\":{lease},\"request_id\":\"{request}\",\"status\":\"{}\",\"provider_session\":\"{}\",\"provider_turn\":\"{}\"",
        semantic.sequence,
        Base64Display::new(&semantic.id, &STANDARD),
        Base64Display::new(&receipt.application_id, &STANDARD),
        if status == 0 { "accepted" } else { "refused" },
        Base64Display::new(provider_session, &STANDARD),
        Base64Display::new(provider_turn, &STANDARD)
    )
}

pub fn application_missing(ts: u64, effect: &SemanticEffect) -> Result<Event, EventError> {
    const REASONS: [&str; 3] = ["deadline", "source-lost", "retention-expired"];
    let reason = REASONS[effect.reason as usize];
    let receipt = effect.receipt;
    semantic_event!("application-receipt-missing", ts, &effect.source, effect.producer, effect.source_epoch, None;
        ",\"application_request_id\":\"{}\",\"lease_epoch\":{},\"request_id\":\"{}\",\"reason\":\"{reason}\"",
        Base64Display::new(&receipt.application_id, &STANDARD),
        receipt.lease_epoch,
        receipt.request_id
    )
}

pub fn semantic_source(ts: u64, effect: &SourceEffect) -> Result<Event, EventError> {
    const STATUS: [&str; 4] = ["connected", "exact", "degraded", "disconnected"];
    const REASON: [&str; 5] = [
        "",
        "heartbeat-timeout",
        "transport-closed",
        "superseded",
        "session-ending",
    ];
    let (status, reason) = (
        STATUS[effect.status as usize],
        REASON[effect.reason as usize],
    );
    semantic_event!("semantic-source", ts, &effect.source, effect.producer, effect.source_epoch, Some((0, effect.source.as_ref().into()));
        ",\"status\":\"{status}\",\"reason\":\"{reason}\"")
}

pub fn semantic_changes(ts: u64, changes: Vec<SemanticChange>) -> Result<Vec<Event>, EventError> {
    changes
        .into_iter()
        .map(|change| match change {
            SemanticChange::Source(effect) => semantic_source(ts, &effect),
            SemanticChange::Missing(effect) => application_missing(ts, &effect),
        })
        .collect()
}

schema!(map fn event_schema(name: &str) -> &'static str;
    "ready" => "type:=ready,ts:*,epoch:u,seq:*,kind:=transition/snapshot",
    "state" => "type:=state,ts:*,epoch:u,seq:*,kind:=transition/snapshot,state:=idle/busy,title:t255,truncated:?",
    "link" => "type:=link,ts:*,epoch:u,seq:*,kind:=transition/snapshot,uri:t2048,truncated:?",
    "semantic-source" => "type:=semantic-source,ts:*,epoch:u,seq:*,kind:=transition/snapshot,source:s,producer:b16,source_epoch:p,status:=connected,reason:=|type:=semantic-source,ts:*,epoch:u,seq:*,kind:=transition/snapshot,source:s,producer:b16,source_epoch:p,status:=exact,reason:=|type:=semantic-source,ts:*,epoch:u,seq:*,kind:=transition/snapshot,source:s,producer:b16,source_epoch:p,status:=degraded,reason:=heartbeat-timeout|type:=semantic-source,ts:*,epoch:u,seq:*,kind:=transition/snapshot,source:s,producer:b16,source_epoch:p,status:=disconnected,reason:=transport-closed/superseded/session-ending",
    "semantic-assertion" => "type:=semantic-assertion,ts:*,epoch:u,seq:*,kind:=transition,source:s,producer:b16,source_epoch:p,source_seq:d,event_id:b16,assertion_kind:=transition/snapshot,payload:j|type:=semantic-assertion,ts:*,epoch:u,seq:*,kind:=snapshot,source:s,producer:b16,source_epoch:p,source_seq:d,event_id:b16,assertion_kind:=snapshot,payload:j",
    "application-receipt" => "type:=application-receipt,ts:*,epoch:u,seq:*,kind:=transition,source:s,producer:b16,source_epoch:p,source_seq:d,event_id:b16,application_request_id:b16,lease_epoch:p,request_id:d,status:=accepted/refused,provider_session:b4096,provider_turn:b4096",
    "application-receipt-missing" => "type:=application-receipt-missing,ts:*,epoch:u,seq:*,kind:=transition,source:s,producer:b16,source_epoch:p,application_request_id:b16,lease_epoch:p,request_id:d,reason:=deadline/source-lost/retention-expired",
    "stream-exhausted" => "type:=stream-exhausted,ts:*,epoch:u,seq:*,kind:=transition,axis:=seq/epoch/commit",
    "exit" => "type:=exit,ts:*,epoch:u,seq:*,kind:=transition,ended:=exited,code:u|type:=exit,ts:*,epoch:u,seq:*,kind:=transition,ended:=signalled,signal:p|type:=exit,ts:*,epoch:u,seq:*,kind:=transition,ended:=terminated,code:u,method:=graceful/forced",
    "observer-degraded" => "type:=observer-degraded,ts:*,epoch:u,seq:*,kind:=transition,scanner:=osc/query,reason:=deadline/limit/cancelled/malformed",
);

pub(crate) fn valid_stored_event(
    body: &[u8],
    Stored(generation, expected_epoch, index, start, expected_end): Stored,
) -> Option<()> {
    let body = body.strip_suffix(b"\n")?;
    let mut lines = body.split(|byte| *byte == b'\n');
    let header = lines.next()?;
    let (epoch, first, end) = stored_header(header, generation)?;
    let (mut sequence, mut transitions, mut last, mut retained) = (first, 0u64, 0u8, false);
    for line in lines {
        (last & 2 == 0).then_some(())?;
        let flags = stored_line(line, epoch, sequence)?;
        if flags & 1 != 0 {
            (transitions == 0).then_some(())?;
        } else {
            transitions += 1;
            last = flags;
            retained |= flags & 4 != 0;
        }
        sequence = sequence.checked_add(1)?;
    }
    let overage = last & 2 != 0 || epoch != 0 && !retained && transitions == 1;
    let frontier = match last & (8 | 16 | 32) {
        8 => end <= STORE_EVENT_END,
        16 => epoch == u32::MAX && end < STORE_EVENT_END,
        32 => index == u64::MAX && end < STORE_EVENT_END,
        0 => end < STORE_EVENT_END,
        _ => false,
    };
    ((epoch, first, end, sequence) == (expected_epoch, start, expected_end, end)
        && (body.len() < STORE_EVENT_CAP as usize || overage)
        && frontier)
        .then_some(())
}

fn stored_header(line: &[u8], generation: u32) -> Option<(u32, u64, u64)> {
    let fields = canonical_object(line, 16)?;
    (store_fields(&fields, 0..fields.len(), STORE_HEADER)
        && stored_generation(&fields["generation"], generation))
    .then_some(())?;
    let session = stored_base64(fields["session"].as_str()?)?;
    (session.starts_with(&[1, b'/']) || session.len() == 25 && session.first() == Some(&2))
        .then_some(())?;
    let epoch = u32::try_from(fields["epoch"].as_u64()?).ok()?;
    let end = fields["next_seq"].as_u64()?;
    let first = fields["first_retained"].as_u64()?;
    (first <= end && end <= STORE_EVENT_END).then_some((epoch, first, end))
}

fn stored_line(line: &[u8], epoch: u32, sequence: u64) -> Option<u8> {
    let fields = canonical_object(line, 32)?;
    let kind = fields.get("type")?.as_str()?;
    let snapshot = fields.get("kind")?.as_str()? == "snapshot";
    let assertion = fields.get("assertion_kind").and_then(Value::as_str) == Some("snapshot");
    let shape = store_fields(&fields, 0..fields.len(), event_schema(kind)?)
        && fields["epoch"].as_u64() == Some(u64::from(epoch))
        && sequence < STORE_EVENT_END
        && fields["seq"].as_u64() == Some(sequence);
    shape.then(|| {
        u8::from(snapshot)
            | (u8::from(kind == "stream-exhausted") * 2)
            | (u8::from(kind == "semantic-source" || kind == "semantic-assertion" && assertion) * 4)
            | (u8::from(kind == "stream-exhausted" && fields["axis"] == "seq") * 8)
            | (u8::from(kind == "stream-exhausted" && fields["axis"] == "epoch") * 16)
            | (u8::from(kind == "stream-exhausted" && fields["axis"] == "commit") * 32)
    })
}

pub(crate) fn valid_stored_lifecycle(
    body: &[u8],
    Stored(generation, epoch, index, start, end): Stored,
) -> Option<()> {
    let line = body.strip_suffix(b"\n")?;
    (!line.contains(&b'\n') && epoch == 1 && index <= 2 && start == end).then_some(())?;
    let fields = canonical_object(line, 20)?;
    let text = |key| fields.get(key).and_then(Value::as_str);
    let number = |key| text(key).and_then(decimal);
    let encoding = text("path_encoding");
    let session = text("session")
        .and_then(stored_base64)
        .is_some_and(|bytes| match encoding {
            Some("posix-bytes") => bytes.starts_with(&[1, b'/']),
            Some("windows-wtf8") => bytes.len() == 25 && bytes.first() == Some(&2),
            _ => false,
        });
    let common = store_fields(&fields, 0..13, STORE_LIFECYCLE)
        && store_fields(&fields, 13..fields.len(), STORE_LIFECYCLE_END)
        && session
        && stored_generation(&fields["generation"], generation)
        && fields["wire_generation"].as_u64() == Some(u64::from(generation));
    let windows = encoding == Some("windows-wtf8");
    let closed = index == 2 && number("output_end") == Some(end);
    (common
        && match (text("phase"), text("ended")) {
            (Some("running"), None) => index == 1 && start == 0 && end == 0,
            (Some("exited"), Some("exited")) => {
                closed
                    && fields["code"]
                        .as_u64()
                        .is_some_and(|code| windows || code <= 255)
            }
            (Some("exited"), Some("signalled")) => closed && !windows,
            (Some("exited"), Some("terminated")) => closed && windows,
            _ => false,
        })
    .then_some(())
}

fn store_fields(fields: &Map<String, Value>, range: std::ops::Range<usize>, schema: &str) -> bool {
    schema.split('|').any(|choice| {
        let rules = choice.split(',').filter(|rule| !rule.is_empty());
        range.end <= fields.len()
            && rules.clone().count() == range.len()
            && fields
                .iter()
                .skip(range.start)
                .zip(rules)
                .all(|((key, value), rule)| {
                    rule.split_once(':')
                        .is_some_and(|(name, rule)| name == key && store_field(rule, value))
                })
    })
}

fn store_field(rule: &str, value: &Value) -> bool {
    let (text, number) = (value.as_str(), value.as_u64());
    if let Some(choices) = rule.strip_prefix('=') {
        return text.is_some_and(|text| choices.split('/').any(|choice| choice == text));
    }
    let decoded = || text.and_then(stored_base64);
    match rule {
        "t" => text.is_some(),
        "*" => true,
        "1" | "2" => number == Some(u64::from(rule.as_bytes()[0] - b'0')),
        "?" => value.is_boolean(),
        "u" => number.is_some_and(|n| u32::try_from(n).is_ok()),
        "p" => number.is_some_and(|n| u32::try_from(n).is_ok() && n != 0),
        "d" => text.and_then(decimal).is_some_and(|n| n != 0),
        "s" => text.is_some_and(|text| crate::session::valid_source_id(text.as_bytes())),
        "b16" => decoded().is_some_and(|bytes| bytes.len() == 16),
        "b4096" => decoded().is_some_and(|bytes| bytes.len() <= 4096),
        "D" => text.and_then(decimal).is_some(),
        "n" => value.is_null() || decoded().is_some_and(|bytes| !bytes.is_empty()),
        "j" => decoded()
            .is_some_and(|bytes| bytes.len() <= 32768 && json_object(&bytes, 64, 1024).is_some()),
        _ => rule
            .strip_prefix('t')
            .and_then(|cap| cap.parse().ok())
            .is_some_and(|cap| text.is_some_and(|text| text.len() <= cap)),
    }
}

fn stored_base64(text: &str) -> Option<Vec<u8>> {
    STANDARD.decode(text).ok()
}

fn stored_generation(value: &Value, generation: u32) -> bool {
    generation == 1 && value.is_null()
        || generation != 1 && value.as_u64() == Some(u64::from(generation))
}

schema!(tuple Bounded [Clone, Copy]; fields; usize, usize);

macro_rules! value_visitors {
    ($($name:ident($kind:ty)),+ $(,)?) => {$(
        fn $name<E>(self, value: $kind) -> Result<Self::Value, E> { Ok(value.into()) }
    )+};
}

impl<'de> DeserializeSeed<'de> for Bounded {
    type Value = serde_json::Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        input.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Bounded {
    type Value = serde_json::Value;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("bounded JSON")
    }
    value_visitors! { visit_bool(bool), visit_i64(i64), visit_u64(u64), visit_str(&str) }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
        serde_json::Number::from_f64(value)
            .map(Into::into)
            .ok_or_else(|| E::custom("non-finite number"))
    }
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
        return_if!(self.0 == 0, Err(A::Error::custom("JSON nesting limit")));
        let mut values = Vec::new();
        while let Some(value) = input.next_element_seed(Bounded(self.0 - 1, self.1))? {
            values.push(value);
        }
        Ok(values.into())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
        return_if!(self.0 == 0, Err(A::Error::custom("JSON nesting limit")));
        let mut values = serde_json::Map::new();
        while let Some(key) = input.next_key::<String>()? {
            return_if!(
                values.len() == self.1,
                Err(A::Error::custom("excess object member"))
            );
            let value = input.next_value_seed(Bounded(self.0 - 1, self.1))?;
            return_if!(
                values.insert(key, value).is_some(),
                Err(A::Error::custom("duplicate object member"))
            );
        }
        Ok(values.into())
    }
}

fn bounded(bytes: &[u8], depth: usize, members: usize) -> Option<serde_json::Value> {
    let mut input = serde_json::Deserializer::from_slice(bytes);
    let value = Bounded(depth, members).deserialize(&mut input).ok()?;
    input.end().ok()?;
    Some(value)
}

pub(crate) fn json_object(bytes: &[u8], depth: usize, members: usize) -> Option<()> {
    bounded(bytes, depth, members)?.is_object().then_some(())
}

pub(crate) fn canonical_object(
    bytes: &[u8],
    members: usize,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let serde_json::Value::Object(object) = bounded(bytes, 64, members)? else {
        return None;
    };
    let mut encoded = serde_json::to_vec(&object).ok()?;
    upper_escapes(&mut encoded);
    let timestamped = object.contains_key("ts");
    return_if!(!timestamped, (encoded == bytes).then_some(object));
    let marker = bytes.windows(5).position(|part| part == b"\"ts\":")? + 5;
    let end = bytes[marker..]
        .iter()
        .position(|byte| matches!(*byte, b',' | b'}'))?
        + marker;
    parse_timestamp(std::str::from_utf8(&bytes[marker..end]).ok()?)?;
    let timestamp = object.get("ts")?.as_number()?.to_string();
    let matches = encoded.len() == marker + timestamp.len() + bytes.len() - end
        && encoded[..marker] == bytes[..marker]
        && encoded[marker..marker + timestamp.len()] == *timestamp.as_bytes()
        && encoded[marker + timestamp.len()..] == bytes[end..];
    matches.then_some(object)
}

schema!(tuple pub EventStream [Clone, Copy]; fields; Option<Cursor>);

#[allow(clippy::new_without_default)]
impl EventStream {
    pub fn new() -> Self {
        Self::at(Cursor(0, 0, 0, 1))
    }

    pub fn at(cursor: Cursor) -> Self {
        Self(Some(cursor))
    }

    pub fn writable(&self) -> bool {
        self.0.is_some()
    }

    pub fn transact(
        &mut self,
        snapshots: &[Event],
        transitions: &[Event],
        compact: bool,
    ) -> Result<(String, Cursor, Option<Axis>), EventError> {
        let observed = transitions.last().ok_or(EventError::EmptyTransaction)?.1;
        let added = transitions.len() + usize::from(compact) * snapshots.len();
        let Cursor(epoch, next, first, index) = self.0.ok_or(EventError::Closed)?;
        return_if!(next >= 1 << 53 || index == 0, Err(EventError::InvalidState));
        let commit = index.checked_add(1).ok_or(EventError::InvalidState)?;
        let mut cursor = Cursor(epoch, next, first, commit);
        let axis = if next
            .checked_add(added as u64)
            .is_none_or(|end| end >= 1 << 53)
        {
            Some(Axis::Sequence)
        } else if compact && epoch == u32::MAX {
            Some(Axis::Epoch)
        } else {
            if compact {
                cursor.0 += 1;
                cursor.2 = cursor.1;
            }
            (commit == u64::MAX).then_some(Axis::Commit)
        };
        let mut records = String::new();
        let mut sequence = cursor.1;
        let mut append = |kind, events: &[Event]| {
            for event in events {
                append_event(&mut records, cursor.0, sequence, kind, event);
                sequence += 1;
            }
        };
        if compact && !matches!(axis, Some(Axis::Sequence | Axis::Epoch)) {
            append(EventKind::Snapshot, snapshots);
        }
        if axis != Some(Axis::Sequence) {
            append(EventKind::Transition, transitions);
        }
        if let Some(diagnostic) = axis.map(|axis| exhausted(axis, observed)) {
            append(EventKind::Transition, std::slice::from_ref(&diagnostic));
        }
        cursor.1 = sequence;
        self.0 = axis.is_none().then_some(cursor);
        Ok((records, cursor, axis))
    }
}

fn exhausted(axis: Axis, ts: u64) -> Event {
    let axis = ["seq", "epoch", "commit"][axis as usize];
    event("stream-exhausted", ts, &[("axis", Json::String(axis))])
}

pub fn canonical_header(created: u64, session: &str, generation: Option<u32>, c: Cursor) -> String {
    let generation = generation.map_or_else(|| "null".into(), |value| value.to_string());
    let Cursor(epoch, next, first, _) = c;
    format!(
        "{{\"v\":2,\"type\":\"header\",\"ts\":{},\"session\":{},\"generation\":{generation},\"epoch\":{epoch},\"next_seq\":{next},\"first_retained\":{first}}}\n",
        timestamp(created),
        quoted(session),
    )
}

pub fn canonical_event(epoch: u32, seq: u64, kind: EventKind, event: &Event) -> String {
    let mut out = String::new();
    append_event(&mut out, epoch, seq, kind, event);
    out
}

fn append_event(out: &mut String, epoch: u32, seq: u64, kind: EventKind, event: &Event) {
    let kind = ["transition", "snapshot"][kind as usize];
    writeln!(
        out,
        "{{\"type\":\"{}\",\"ts\":{},\"epoch\":{epoch},\"seq\":{seq},\"kind\":\"{kind}\"{}}}",
        event.0,
        timestamp(event.1),
        event.2
    )
    .expect("string writes cannot fail");
}

fn parse_timestamp(value: &str) -> Option<u64> {
    let (whole, fraction) = match value.split_once('.') {
        None => (value, 0),
        Some((whole, fraction)) if fraction.len() == 3 && fraction != "000" => {
            (whole, fraction.parse().ok()?)
        }
        _ => return None,
    };
    crate::canonical_u64(whole)?
        .checked_mul(1000)?
        .checked_add(fraction)
}

fn timestamp(ms: u64) -> String {
    match ms % 1000 {
        0 => (ms / 1000).to_string(),
        fraction => format!("{}.{fraction:03}", ms / 1000),
    }
}

fn quoted(value: &str) -> String {
    let mut bytes = serde_json::to_vec(value).expect("strings serialize");
    upper_escapes(&mut bytes);
    String::from_utf8(bytes).expect("JSON is UTF-8")
}

fn upper_escapes(bytes: &mut [u8]) {
    // Walks escapes rather than scanning for the literal bytes `\u00`, because
    // a serialized `\\` is a backslash of the value's own content: scanning
    // would uppercase the `ab` in a title whose text is `«` and silently
    // alter what the child emitted (§9.4).
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'\\' {
            at += 1;
        } else if bytes.get(at + 1) == Some(&b'u') {
            for hex in at + 2..(at + 6).min(bytes.len()) {
                bytes[hex].make_ascii_uppercase();
            }
            at += 6;
        } else {
            at += 2;
        }
    }
}
