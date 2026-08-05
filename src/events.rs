use crate::session::{
    ReceiptProjection, SemanticChange, SemanticEffect, SemanticEvent, SemanticEventKind,
    SourceEffect,
};
use base64::{display::Base64Display, engine::general_purpose::STANDARD};
use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
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

fn provenance(source: &[u8], producer: [u8; 16], epoch: u32) -> Result<String, EventError> {
    let source = std::str::from_utf8(source).map_err(|_| EventError::InvalidEvent)?;
    Ok(format!(
        ",\"source\":{},\"producer\":\"{}\",\"source_epoch\":{epoch}",
        quoted(source),
        Base64Display::new(&producer, &STANDARD)
    ))
}

macro_rules! semantic_event {
    ($name:literal, $ts:expr, $source:expr, $producer:expr, $epoch:expr, $retention:expr; $($tail:tt)*) => {{
        let mut tail = provenance($source, $producer, $epoch)?;
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
    let Some((provider_session, provider_turn)) = providers
        .filter(|(session, turn)| status <= 1 && session.len() <= 4096 && turn.len() <= 4096)
    else {
        return Err(EventError::InvalidEvent);
    };
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

pub(crate) fn schema(name: &str) -> Option<&'static str> {
    Some(match name {
        "ready" => "",
        "state" => "state:=idle/busy,title:t255,truncated:?",
        "link" => "uri:t2048,truncated:?",
        "semantic-source" => "source:s,producer:b16,source_epoch:p,status:t,reason:t",
        "semantic-assertion" => {
            "source:s,producer:b16,source_epoch:p,source_seq:d,event_id:b16,assertion_kind:=transition/snapshot,payload:j"
        }
        "application-receipt" => {
            "source:s,producer:b16,source_epoch:p,source_seq:d,event_id:b16,application_request_id:b16,lease_epoch:p,request_id:d,status:=accepted/refused,provider_session:b4096,provider_turn:b4096"
        }
        "application-receipt-missing" => {
            "source:s,producer:b16,source_epoch:p,application_request_id:b16,lease_epoch:p,request_id:d,reason:=deadline/source-lost/retention-expired"
        }
        "stream-exhausted" => "axis:=seq/epoch/commit",
        "exit" => {
            "ended:=exited,code:u|ended:=signalled,signal:p|ended:=terminated,code:u,method:=graceful/forced"
        }
        "observer-degraded" => "scanner:=osc/query,reason:=deadline/limit/cancelled/malformed",
        _ => return None,
    })
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
