//! Bounded MCP-only delivery of canonical guidance resources. No provider
//! filesystem fallback.
//!
//! Guidance is delivered as identified resources (`key` + content digest),
//! never as byte fragments of one serialized document. A page carries whole
//! resources, or semantic parts of one oversize resource (object fields or
//! paragraph-bounded text segments). Every resource whose last part a page
//! carries is reported in `completed`, which is what the read ledger records.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

// Limits apply to serialized content, including JSON escaping. Retained
// snapshots are bounded per tool kind (see `GuidanceDeliveryKind::capacity`).
pub(super) const PAGE_BYTES: usize = 12_000;
// Room the server adds after paging (compact live guidance state).
const LIVE_STATE_RESERVE: usize = 640;
// Upper bound of one delivery unit. Small units let pages pack close to the
// page budget, so total calls approach total bytes / page budget. Resources
// at or under this size are always delivered whole.
const UNIT_BYTES: usize = 3_000;
const DOCUMENT_BYTES: usize = 2_000_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuidanceDeliveryRequest {
    /// Omit for a new snapshot. Follow exactly the returned next_arguments
    /// until delivery_complete is true; each response is readable through MCP.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GuidanceDeliveryKind {
    Authoring,
    Packet,
    Card,
}
impl GuidanceDeliveryKind {
    /// Concurrent snapshots retained for this tool. Cards are read in
    /// parallel by real clients, so a few independent cursors stay valid.
    fn capacity(self) -> usize {
        match self {
            Self::Card => 4,
            Self::Authoring | Self::Packet => 1,
        }
    }
    fn tool(self) -> &'static str {
        match self {
            Self::Authoring => "get_authoring_guidance",
            Self::Packet => "get_agent_guidance_packet",
            Self::Card => "get_guidance_card",
        }
    }
    /// Arguments that restart this tool's delivery from its first page.
    /// Rejecting a cursor invalidates the cursor only: a selector that names
    /// the requested resource is required by the tool's schema, so recovery
    /// must retain it. Advising "no arguments" for a selector-bearing tool
    /// cannot validate and leaves the agent with no executable next call.
    fn restart_arguments(self, binding: &str) -> Value {
        match self {
            Self::Card => json!({ "card_id": binding }),
            Self::Authoring | Self::Packet => json!({}),
        }
    }
    /// Recovery an agent can execute verbatim after a cursor is rejected.
    fn restart_advice(self, binding: &str) -> String {
        format!(
            "call {} again with arguments {} and no cursor",
            self.tool(),
            self.restart_arguments(binding)
        )
    }
}

/// Content identity used by the read ledger: sha256 of compact JSON.
pub(super) fn resource_digest(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(&bytes))
}

fn encoded_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|v| v.len())
        .unwrap_or(usize::MAX)
}

/// One identified guidance resource. `digest` is the ledger identity of the
/// obligation it satisfies; it normally equals the digest of `body`, but a
/// detail view of a canonical resource may carry the canonical digest.
#[derive(Debug, Clone)]
pub(super) struct GuidanceResource {
    pub key: String,
    pub kind: &'static str,
    pub id: String,
    pub retrieval: Value,
    pub body: Value,
    pub digest: String,
    pub bytes: usize,
}

impl GuidanceResource {
    pub fn new(key: String, kind: &'static str, id: String, retrieval: Value, body: Value) -> Self {
        let digest = resource_digest(&body);
        Self::with_digest(key, kind, id, retrieval, body, digest)
    }

    pub fn with_digest(
        key: String,
        kind: &'static str,
        id: String,
        retrieval: Value,
        body: Value,
        digest: String,
    ) -> Self {
        let bytes = encoded_len(&body);
        Self {
            key,
            kind,
            id,
            retrieval,
            body,
            digest,
            bytes,
        }
    }

    pub fn manifest_entry(&self, required: bool) -> Value {
        json!({
            "key": self.key,
            "kind": self.kind,
            "id": self.id,
            "sha256": self.digest,
            "bytes": self.bytes,
            "required": required,
            "retrieval": self.retrieval,
        })
    }

    pub fn read(&self) -> ResourceRead {
        ResourceRead {
            key: self.key.clone(),
            digest: self.digest.clone(),
            bytes: self.bytes,
        }
    }

    /// Semantic delivery units, each within the unit budget.
    fn units(&self) -> Vec<Value> {
        let mut units = Vec::new();
        if encoded_len(&self.body) <= UNIT_BYTES {
            units.push(json!({"content": self.body}));
        } else if let Value::Object(fields) = &self.body {
            let mut current = Map::new();
            let flush = |current: &mut Map<String, Value>, units: &mut Vec<Value>| {
                if !current.is_empty() {
                    units.push(json!({"fields": std::mem::take(current)}));
                }
            };
            for (name, value) in fields {
                let size = encoded_len(value) + name.len() + 4;
                if size > UNIT_BYTES {
                    flush(&mut current, &mut units);
                    for segment in text_segments(value) {
                        units.push(json!({"field": name, "text_segment": segment}));
                    }
                    continue;
                }
                if encoded_len(&Value::Object(current.clone())) + size > UNIT_BYTES {
                    flush(&mut current, &mut units);
                }
                current.insert(name.clone(), value.clone());
            }
            flush(&mut current, &mut units);
        } else {
            for segment in text_segments(&self.body) {
                units.push(json!({"text_segment": segment}));
            }
        }
        let parts = units.len();
        for (part, unit) in units.iter_mut().enumerate() {
            unit["key"] = json!(self.key);
            unit["sha256"] = json!(self.digest);
            if parts > 1 {
                unit["part"] = json!(part + 1);
                unit["parts"] = json!(parts);
            }
        }
        units
    }
}

/// Paragraph-bounded text segments. Non-string values are segmented as their
/// pretty JSON text; that only happens for an oversize non-text field, which a
/// capability owner should split into separate resources.
fn text_segments(value: &Value) -> Vec<String> {
    let text = match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    let fits = |s: &str| encoded_len(&Value::String(s.to_owned())) <= UNIT_BYTES;
    let mut pieces = Vec::new();
    for paragraph in text.split_inclusive("\n\n") {
        if fits(paragraph) {
            pieces.push(paragraph.to_owned());
            continue;
        }
        for line in paragraph.split_inclusive('\n') {
            if fits(line) {
                pieces.push(line.to_owned());
                continue;
            }
            let mut rest = line;
            while !rest.is_empty() {
                let mut end = rest.len().min(UNIT_BYTES / 2);
                while !rest.is_char_boundary(end) {
                    end -= 1;
                }
                while !fits(&rest[..end]) {
                    end -= 1;
                    while !rest.is_char_boundary(end) {
                        end -= 1;
                    }
                }
                pieces.push(rest[..end].to_owned());
                rest = &rest[end..];
            }
        }
    }
    let mut segments: Vec<String> = Vec::new();
    for piece in pieces {
        match segments.last_mut() {
            Some(last) if fits(&format!("{last}{piece}")) => last.push_str(&piece),
            _ => segments.push(piece),
        }
    }
    segments
}

/// A resource whose complete content was delivered by a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResourceRead {
    pub key: String,
    pub digest: String,
    pub bytes: usize,
}

#[derive(Debug)]
struct Delivery {
    kind: GuidanceDeliveryKind,
    id: String,
    /// Negotiated guidance session; a different session invalidates all.
    session: Option<String>,
    /// Resource binding (e.g. the card id); a cursor cannot be relabelled.
    binding: String,
    header: Map<String, Value>,
    units: Vec<Value>,
    /// Resource completed by the unit at the same index, if it is its last.
    completes: Vec<Option<ResourceRead>>,
    delivered_end: usize,
}

#[derive(Debug, Clone, Default)]
pub(super) struct GuidanceDeliveryStore(Arc<Mutex<Vec<Delivery>>>);

#[derive(Debug)]
pub(super) struct DeliveredGuidance {
    pub value: Value,
    pub completed: Vec<ResourceRead>,
    pub response_bytes: usize,
}

impl GuidanceDeliveryStore {
    /// Single-resource tools keep the plain resource shape when it fits.
    pub fn begin_single(
        &self,
        kind: GuidanceDeliveryKind,
        session: Option<String>,
        binding: &str,
        resource: GuidanceResource,
        available: bool,
    ) -> Result<DeliveredGuidance, String> {
        if resource.bytes <= PAGE_BYTES {
            drop(self.reserve(kind, &session, binding));
            let completed = if available {
                vec![resource.read()]
            } else {
                Vec::new()
            };
            return Ok(DeliveredGuidance {
                response_bytes: resource.bytes,
                value: resource.body,
                completed,
            });
        }
        self.begin(
            kind,
            session,
            binding,
            Map::new(),
            vec![resource],
            available,
        )
    }

    /// Paged delivery: `header` fields lead the first page, then units.
    pub fn begin(
        &self,
        kind: GuidanceDeliveryKind,
        session: Option<String>,
        binding: &str,
        header: Map<String, Value>,
        resources: Vec<GuidanceResource>,
        available: bool,
    ) -> Result<DeliveredGuidance, String> {
        let total: usize = resources.iter().map(|r| r.bytes).sum();
        if total > DOCUMENT_BYTES {
            return Err("guidance exceeds the 2 MB delivery limit; capability owner must split reference content into guidance cards".into());
        }
        let mut units = Vec::new();
        let mut completes = Vec::new();
        for resource in &resources {
            let resource_units = resource.units();
            let last = resource_units.len().saturating_sub(1);
            for (index, unit) in resource_units.into_iter().enumerate() {
                units.push(unit);
                completes.push((index == last && available).then(|| resource.read()));
            }
        }
        let mut delivery = Delivery {
            kind,
            id: uuid::Uuid::new_v4().simple().to_string(),
            session: session.clone(),
            binding: binding.to_owned(),
            header,
            units,
            completes,
            delivered_end: 0,
        };
        let result = delivery.page(kind, 0)?;
        let mut slots = self.reserve(kind, &session, binding);
        slots.push(delivery);
        Ok(result)
    }

    // Snapshots are immutable and bounded per tool kind. Reading a packet or
    // card never invalidates another tool's or another card's unfinished
    // snapshot. A new negotiation invalidates all snapshots. Returns the
    // locked store with room for one new snapshot of `kind`/`binding`.
    fn reserve(
        &self,
        kind: GuidanceDeliveryKind,
        session: &Option<String>,
        binding: &str,
    ) -> std::sync::MutexGuard<'_, Vec<Delivery>> {
        let mut slots = self.0.lock().expect("guidance delivery lock poisoned");
        slots.retain(|d| &d.session == session && !(d.kind == kind && d.binding == binding));
        let same_kind = slots.iter().filter(|d| d.kind == kind).count();
        if same_kind >= kind.capacity() {
            if let Some(oldest) = slots.iter().position(|d| d.kind == kind) {
                slots.remove(oldest);
            }
        }
        slots
    }

    pub fn resume(
        &self,
        kind: GuidanceDeliveryKind,
        session: Option<String>,
        binding: &str,
        cursor: &str,
    ) -> Result<DeliveredGuidance, String> {
        let mut slots = self.0.lock().expect("guidance delivery lock poisoned");
        // Every rejection names an executable restart that keeps the caller's
        // own selector, so a cursor loss stays finitely recoverable.
        let restart = || kind.restart_advice(binding);
        let (id, offset) = cursor
            .split_once(':')
            .ok_or_else(|| format!("invalid guidance cursor; {}", restart()))?;
        let delivery = slots
            .iter_mut()
            .find(|d| d.kind == kind && d.id == id)
            .ok_or_else(|| format!("no guidance snapshot for this cursor; {}", restart()))?;
        if session != delivery.session || binding != delivery.binding {
            return Err(format!(
                "guidance cursor belongs to another snapshot/session; {}",
                restart()
            ));
        }
        let offset = offset
            .parse::<usize>()
            .map_err(|_| format!("invalid guidance cursor offset; {}", restart()))?;
        // Re-reading delivered pages is harmless; skipping units cannot
        // complete an obligation. No caller-provided completion assertion.
        if offset > delivery.delivered_end || offset >= delivery.units.len() {
            return Err(format!(
                "guidance cursor skips unread content or is out of range; {}",
                restart()
            ));
        }
        delivery.page(kind, offset)
    }
}

impl Delivery {
    fn page(
        &mut self,
        kind: GuidanceDeliveryKind,
        offset: usize,
    ) -> Result<DeliveredGuidance, String> {
        let mut value = Value::Object(if offset == 0 {
            self.header.clone()
        } else {
            Map::new()
        });
        value["delivery"] = json!("talos3d.guidance-page.v2");
        value["instruction"] = json!("Units are whole resources (content) or ordered parts of one resource: merge `fields` objects and concatenate `text_segment`s per `field` in part order. Resources listed in completed_resource_keys are recorded as read; do not fetch them again. Follow next_tool with next_arguments until delivery_complete. No filesystem access is required.");
        let mut units = Vec::new();
        let mut completed = Vec::new();
        let mut end = offset;
        while end < self.units.len() {
            units.push(self.units[end].clone());
            value["units"] = json!(units);
            // Always make progress; otherwise stop before exceeding the page.
            if encoded_len(&value) > PAGE_BYTES - LIVE_STATE_RESERVE && units.len() > 1 {
                units.pop();
                break;
            }
            if let Some(read) = &self.completes[end] {
                completed.push(read.clone());
            }
            end += 1;
        }
        self.delivered_end = self.delivered_end.max(end);
        let complete = self.delivered_end == self.units.len();
        let more = end < self.units.len();
        value["units"] = json!(units);
        value["completed_resource_keys"] =
            json!(completed.iter().map(|r| &r.key).collect::<Vec<_>>());
        value["unit_range"] = json!([offset, end, self.units.len()]);
        value["delivery_complete"] = json!(complete && !more);
        value["next_tool"] = json!(more.then(|| kind.tool()));
        value["next_arguments"] =
            json!(more.then(|| json!({"cursor": format!("{}:{end}", self.id)})));
        Ok(DeliveredGuidance {
            response_bytes: encoded_len(&value),
            completed,
            value,
        })
    }
}

/// Reassemble resources from pages the way an MCP client is instructed to.
#[cfg(test)]
pub(super) fn reassemble(pages: &[Value]) -> std::collections::BTreeMap<String, Value> {
    let mut out: std::collections::BTreeMap<String, Value> = Default::default();
    for unit in pages
        .iter()
        .flat_map(|p| p["units"].as_array().cloned().unwrap_or_default())
    {
        let key = unit["key"].as_str().unwrap().to_owned();
        if let Some(content) = unit.get("content") {
            out.insert(key, content.clone());
            continue;
        }
        let entry = out.entry(key).or_insert(Value::Null);
        if let Some(fields) = unit.get("fields").and_then(Value::as_object) {
            if !entry.is_object() {
                *entry = json!({});
            }
            entry.as_object_mut().unwrap().extend(fields.clone());
        } else if let Some(segment) = unit["text_segment"].as_str() {
            let target = match unit.get("field").and_then(Value::as_str) {
                Some(field) => {
                    if !entry.is_object() {
                        *entry = json!({});
                    }
                    entry
                        .as_object_mut()
                        .unwrap()
                        .entry(field.to_owned())
                        .or_insert(json!(""))
                }
                None => {
                    if !entry.is_string() {
                        *entry = json!("");
                    }
                    entry
                }
            };
            let joined = format!("{}{segment}", target.as_str().unwrap_or_default());
            *target = json!(joined);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(key: &str, body: Value) -> GuidanceResource {
        GuidanceResource::new(
            key.into(),
            "test",
            key.into(),
            json!({"tool": "get_guidance_card"}),
            body,
        )
    }

    fn drain(
        store: &GuidanceDeliveryStore,
        kind: GuidanceDeliveryKind,
        page: DeliveredGuidance,
    ) -> (Vec<Value>, Vec<ResourceRead>) {
        drain_with(store, kind, Some("s".into()), "", page)
    }

    fn drain_with(
        store: &GuidanceDeliveryStore,
        kind: GuidanceDeliveryKind,
        session: Option<String>,
        binding: &str,
        mut page: DeliveredGuidance,
    ) -> (Vec<Value>, Vec<ResourceRead>) {
        let mut pages = Vec::new();
        let mut reads = Vec::new();
        loop {
            assert!(
                page.response_bytes <= PAGE_BYTES,
                "{} bytes",
                page.response_bytes
            );
            reads.extend(page.completed.clone());
            pages.push(page.value.clone());
            if page.value["delivery_complete"] != false {
                return (pages, reads);
            }
            let cursor = page.value["next_arguments"]["cursor"]
                .as_str()
                .unwrap()
                .to_owned();
            page = store
                .resume(kind, session.clone(), binding, &cursor)
                .unwrap();
        }
    }

    #[test]
    fn oversize_resources_split_on_semantic_boundaries_and_reassemble_exactly() {
        let store = GuidanceDeliveryStore::default();
        let prose = "Paragraph å屋 \"quoted\" \\ text.\n\n".repeat(900);
        let resources = vec![
            resource(
                "guidance",
                json!({"guidance_id": "g", "version": 3, "prompt_text": prose}),
            ),
            resource("card:a", json!({"id": "a", "summary": "small"})),
            resource("skill:b", json!("x".repeat(30_000))),
        ];
        let expected: Vec<_> = resources
            .iter()
            .map(|r| (r.key.clone(), r.body.clone()))
            .collect();
        let reads_expected: Vec<_> = resources.iter().map(GuidanceResource::read).collect();
        let first = store
            .begin(
                GuidanceDeliveryKind::Packet,
                Some("s".into()),
                "",
                Map::new(),
                resources,
                true,
            )
            .unwrap();
        let (pages, reads) = drain(&store, GuidanceDeliveryKind::Packet, first);
        let rebuilt = reassemble(&pages);
        for (key, body) in expected {
            assert_eq!(rebuilt[&key], body, "{key}");
        }
        assert_eq!(reads, reads_expected);
        // Paragraph-bounded: prose segments never cut a paragraph.
        for unit in pages
            .iter()
            .flat_map(|p| p["units"].as_array().unwrap().clone())
        {
            if unit["field"] == "prompt_text" {
                assert!(unit["text_segment"].as_str().unwrap().ends_with("\n\n"));
            }
        }
    }

    #[test]
    fn cursor_cannot_skip_cross_tools_or_cross_sessions() {
        let store = GuidanceDeliveryStore::default();
        let page = store
            .begin(
                GuidanceDeliveryKind::Packet,
                Some("s".into()),
                "",
                Map::new(),
                vec![resource("a", json!("a".repeat(30_000)))],
                true,
            )
            .unwrap();
        assert_eq!(page.value["delivery_complete"], false);
        assert!(page.completed.is_empty());
        let cursor = page.value["next_arguments"]["cursor"].as_str().unwrap();
        let id = cursor.split(':').next().unwrap();
        assert!(store
            .resume(
                GuidanceDeliveryKind::Packet,
                Some("s".into()),
                "",
                &format!("{id}:9")
            )
            .is_err());
        assert!(store
            .resume(
                GuidanceDeliveryKind::Authoring,
                Some("s".into()),
                "",
                cursor
            )
            .is_err());
        assert!(store
            .resume(GuidanceDeliveryKind::Packet, Some("new".into()), "", cursor)
            .is_err());
        let first = store
            .resume(GuidanceDeliveryKind::Packet, Some("s".into()), "", cursor)
            .unwrap();
        let retry = store
            .resume(GuidanceDeliveryKind::Packet, Some("s".into()), "", cursor)
            .unwrap();
        assert_eq!(first.value, retry.value);
    }

    #[test]
    fn interleaved_tools_keep_their_own_bounded_snapshots() {
        let store = GuidanceDeliveryStore::default();
        let kinds = [
            GuidanceDeliveryKind::Authoring,
            GuidanceDeliveryKind::Packet,
            GuidanceDeliveryKind::Card,
        ];
        let mut cursors = Vec::new();
        for kind in kinds {
            let page = store
                .begin(
                    kind,
                    Some("s".into()),
                    "",
                    Map::new(),
                    vec![resource("x", json!("x".repeat(30_000)))],
                    true,
                )
                .unwrap();
            cursors.push(
                page.value["next_arguments"]["cursor"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        for (kind, cursor) in kinds.into_iter().zip(&cursors) {
            assert!(store.resume(kind, Some("s".into()), "", cursor).is_ok());
        }
        // Refresh replaces only its own slot, including a small inline refresh.
        store
            .begin_single(
                GuidanceDeliveryKind::Packet,
                Some("s".into()),
                "",
                resource("n", json!("new")),
                true,
            )
            .unwrap();
        assert!(store
            .resume(
                GuidanceDeliveryKind::Packet,
                Some("s".into()),
                "",
                &cursors[1]
            )
            .is_err());
        assert!(store
            .resume(
                GuidanceDeliveryKind::Authoring,
                Some("s".into()),
                "",
                &cursors[0]
            )
            .is_ok());
        // A different negotiated session can never complete an old obligation.
        store
            .begin_single(
                GuidanceDeliveryKind::Packet,
                Some("new".into()),
                "",
                resource("n", json!("new")),
                true,
            )
            .unwrap();
        assert!(store
            .resume(
                GuidanceDeliveryKind::Authoring,
                Some("s".into()),
                "",
                &cursors[0]
            )
            .is_err());
    }

    #[test]
    fn small_resources_keep_existing_shape_and_cache_is_bounded() {
        let store = GuidanceDeliveryStore::default();
        let value = json!({"prompt_text": "canonical"});
        let result = store
            .begin_single(
                GuidanceDeliveryKind::Authoring,
                None,
                "",
                resource("g", value.clone()),
                true,
            )
            .unwrap();
        assert_eq!(result.value, value);
        assert_eq!(result.completed.len(), 1);
        assert!(store
            .begin(
                GuidanceDeliveryKind::Authoring,
                None,
                "",
                Map::new(),
                vec![resource("big", json!("a".repeat(DOCUMENT_BYTES)))],
                true
            )
            .is_err());
    }

    #[test]
    fn unavailable_content_never_completes_an_obligation() {
        let store = GuidanceDeliveryStore::default();
        let result = store
            .begin_single(
                GuidanceDeliveryKind::Authoring,
                None,
                "",
                resource("g", json!({})),
                false,
            )
            .unwrap();
        assert!(result.completed.is_empty());
    }

    /// Regression (acceptance trace): parallel card reads must not invalidate
    /// each other's cursors or an unfinished packet, and a card cursor cannot
    /// be relabelled to another card.
    #[test]
    fn parallel_card_reads_keep_independent_cursors() {
        let store = GuidanceDeliveryStore::default();
        let big = |key: &str| resource(key, json!("y".repeat(30_000)));
        let s = || Some("s".to_string());
        let packet = store
            .begin(
                GuidanceDeliveryKind::Packet,
                s(),
                "",
                Map::new(),
                vec![big("p")],
                true,
            )
            .unwrap();
        let cursor = |page: &DeliveredGuidance| {
            page.value["next_arguments"]["cursor"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let packet_cursor = cursor(&packet);
        let cards: Vec<_> = ["a", "b", "c"]
            .iter()
            .map(|id| {
                let page = store
                    .begin_single(GuidanceDeliveryKind::Card, s(), id, big(id), true)
                    .unwrap();
                (id.to_string(), cursor(&page))
            })
            .collect();
        for (id, c) in &cards {
            assert!(
                store.resume(GuidanceDeliveryKind::Card, s(), id, c).is_ok(),
                "{id}"
            );
        }
        assert!(store
            .resume(GuidanceDeliveryKind::Packet, s(), "", &packet_cursor)
            .is_ok());
        assert!(store
            .resume(GuidanceDeliveryKind::Card, s(), "b", &cards[0].1)
            .is_err());
        // Bounded: the oldest card snapshot is evicted past capacity.
        for id in ["d", "e"] {
            store
                .begin_single(GuidanceDeliveryKind::Card, s(), id, big(id), true)
                .unwrap();
        }
        assert!(store
            .resume(GuidanceDeliveryKind::Card, s(), "a", &cards[0].1)
            .is_err());
        assert!(store
            .resume(GuidanceDeliveryKind::Card, s(), "c", &cards[2].1)
            .is_ok());
    }

    /// Regression (restricted acceptance trace): a rejected cursor must advise
    /// a call that actually validates. The advice used to be "restart with
    /// empty arguments", which `get_guidance_card` rejects because `card_id`
    /// is required, so an obedient client looped on invalid parameters instead
    /// of recovering.
    #[test]
    fn rejected_cursors_advise_a_valid_restart_that_keeps_required_selectors() {
        let store = GuidanceDeliveryStore::default();
        let big = |key: &str| resource(key, json!("z".repeat(30_000)));
        let s = || Some("s".to_string());
        let cursor_of = |page: &DeliveredGuidance| {
            page.value["next_arguments"]["cursor"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        // The advised arguments must deserialize into the tool's real request
        // type, with the selector retained and the cursor cleared.
        let advised = |kind: GuidanceDeliveryKind, binding: &str| {
            let arguments = kind.restart_arguments(binding);
            match kind {
                GuidanceDeliveryKind::Card => {
                    let request: super::super::server::GetGuidanceCardRequest =
                        serde_json::from_value(arguments.clone()).unwrap();
                    assert_eq!(request.card_id, binding);
                    assert!(request.cursor.is_none());
                }
                _ => {
                    let request: GuidanceDeliveryRequest =
                        serde_json::from_value(arguments.clone()).unwrap();
                    assert!(request.cursor.is_none());
                }
            }
            arguments.to_string()
        };
        let check = |message: String, kind: GuidanceDeliveryKind, binding: &str| {
            assert!(!message.contains("empty arguments"), "{message}");
            assert!(message.contains(kind.tool()), "{message}");
            assert!(message.contains(&advised(kind, binding)), "{message}");
        };

        let kinds = [
            (GuidanceDeliveryKind::Card, "card.a"),
            (GuidanceDeliveryKind::Packet, ""),
            (GuidanceDeliveryKind::Authoring, ""),
        ];
        let live: Vec<_> = kinds
            .iter()
            .map(|(kind, binding)| {
                let page = store
                    .begin(*kind, s(), binding, Map::new(), vec![big("body")], true)
                    .unwrap();
                cursor_of(&page)
            })
            .collect();

        for ((kind, binding), cursor) in kinds.iter().zip(&live) {
            // Unknown snapshot id, and a malformed and an out-of-range cursor.
            for bad in [
                "00000000000000000000000000000000:0".to_owned(),
                "no-separator".to_owned(),
                format!("{}:99", cursor.split(':').next().unwrap()),
            ] {
                check(
                    store.resume(*kind, s(), binding, &bad).unwrap_err(),
                    *kind,
                    binding,
                );
            }
            // Binding mismatch advises the selector the caller asked for, not
            // the one the live snapshot holds.
            check(
                store.resume(*kind, s(), "card.other", cursor).unwrap_err(),
                *kind,
                "card.other",
            );
        }

        // Expiration: a new negotiated session drops every earlier snapshot.
        store
            .begin_single(
                GuidanceDeliveryKind::Authoring,
                Some("new".into()),
                "",
                resource("g", json!("fresh")),
                true,
            )
            .unwrap();
        for ((kind, binding), cursor) in kinds.iter().zip(&live) {
            check(
                store.resume(*kind, s(), binding, cursor).unwrap_err(),
                *kind,
                binding,
            );
        }

        // The advised restart is executable: the obligation can still be met.
        let restarted = store
            .begin_single(
                GuidanceDeliveryKind::Card,
                Some("new".into()),
                "card.a",
                big("card:a"),
                true,
            )
            .unwrap();
        let (_, reads) = drain_with(
            &store,
            GuidanceDeliveryKind::Card,
            Some("new".into()),
            "card.a",
            restarted,
        );
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].key, "card:a");
    }
}
