//! Bounded MCP-only delivery of canonical guidance. No provider filesystem fallback.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

// Limits apply to serialized content, including JSON escaping. Only two fixed
// tool slots are retained per connection, never a caller-selected cache key.
const INLINE_BYTES: usize = 12_000;
const CHUNK_BYTES: usize = 6_000;
const DOCUMENT_BYTES: usize = 2_000_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuidanceDeliveryRequest {
    /// Omit for a new snapshot. Follow exactly the returned next_arguments
    /// until delivery_complete is true; each response is readable through MCP.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum GuidanceDeliveryKind {
    Authoring,
    Packet,
}
impl GuidanceDeliveryKind {
    fn index(self) -> usize {
        match self {
            Self::Authoring => 0,
            Self::Packet => 1,
        }
    }
    fn tool(self) -> &'static str {
        match self {
            Self::Authoring => "get_authoring_guidance",
            Self::Packet => "get_agent_guidance_packet",
        }
    }
}

#[derive(Debug)]
struct Delivery {
    id: String,
    session: Option<String>,
    content: String,
    sha256: String,
    delivered_end: usize,
    available: bool,
}
#[derive(Debug, Clone, Default)]
pub(super) struct GuidanceDeliveryStore(Arc<Mutex<[Option<Delivery>; 2]>>);

pub(super) struct DeliveredGuidance {
    pub value: Value,
    pub complete: bool,
    pub available: bool,
}

impl GuidanceDeliveryStore {
    pub fn begin(
        &self,
        kind: GuidanceDeliveryKind,
        session: Option<String>,
        value: Value,
        available: bool,
    ) -> Result<DeliveredGuidance, String> {
        let content = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        if content.len() > DOCUMENT_BYTES {
            return Err("guidance exceeds the 2 MB delivery limit; capability owner must split reference content into guidance cards".into());
        }
        let mut slots = self.0.lock().expect("guidance delivery lock poisoned");
        // A refresh of either route invalidates old completion cursors.
        *slots = [None, None];
        if content.len() <= INLINE_BYTES {
            return Ok(DeliveredGuidance {
                value,
                complete: true,
                available,
            });
        }
        let mut delivery = Delivery {
            id: uuid::Uuid::new_v4().simple().to_string(),
            session,
            sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
            content,
            delivered_end: 0,
            available,
        };
        let result = delivery.page(kind, 0)?;
        slots[kind.index()] = Some(delivery);
        Ok(result)
    }

    pub fn resume(
        &self,
        kind: GuidanceDeliveryKind,
        session: Option<String>,
        cursor: &str,
    ) -> Result<DeliveredGuidance, String> {
        let mut slots = self.0.lock().expect("guidance delivery lock poisoned");
        let delivery = slots[kind.index()]
            .as_mut()
            .ok_or("no guidance snapshot; restart this tool with empty arguments")?;
        let (id, offset) = cursor.split_once(':').ok_or("invalid guidance cursor")?;
        if id != delivery.id || session != delivery.session {
            return Err(
                "guidance cursor belongs to another snapshot/session; restart with empty arguments"
                    .into(),
            );
        }
        let offset = offset
            .parse::<usize>()
            .map_err(|_| "invalid guidance cursor offset")?;
        // Re-reading delivered pages is harmless; skipping text cannot complete
        // the guidance obligation. No caller-provided completion assertion.
        if offset > delivery.delivered_end
            || offset >= delivery.content.len()
            || !delivery.content.is_char_boundary(offset)
        {
            return Err("guidance cursor skips unread content or is out of range".into());
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
        let mut end = (offset + CHUNK_BYTES).min(self.content.len());
        while !self.content.is_char_boundary(end) {
            end -= 1;
        }
        // JSON escaping can enlarge a fragment. Bound the actual encoded field,
        // rather than assuming character count is the wire payload size.
        while serde_json::to_string(&self.content[offset..end])
            .map_err(|e| e.to_string())?
            .len()
            > INLINE_BYTES
        {
            end -= 1;
            while !self.content.is_char_boundary(end) {
                end -= 1;
            }
        }
        self.delivered_end = self.delivered_end.max(end);
        let complete = self.delivered_end == self.content.len();
        Ok(DeliveredGuidance {
            complete,
            available: self.available,
            value: json!({
                "delivery": "talos3d.guidance-page.v1",
                "content_format": "application/json",
                "instruction": "Read content in order as fragments of one canonical JSON document. Follow next_tool with next_arguments until delivery_complete. The outer live_guidance_state supersedes any state embedded in the frozen document. No filesystem access is required.",
                "snapshot_sha256": self.sha256,
                "offset_bytes": offset, "end_bytes": end, "total_bytes": self.content.len(),
                "content": &self.content[offset..end],
                "delivery_complete": complete,
                "next_tool": (end < self.content.len()).then(|| kind.tool()),
                "next_arguments": (end < self.content.len()).then(|| json!({"cursor": format!("{}:{end}", self.id)})),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_pages_reconstruct_exact_unicode_escaped_snapshot() {
        let store = GuidanceDeliveryStore::default();
        let source = json!({"prompt_text": "å屋\n\"\\".repeat(20_000)});
        let mut page = store
            .begin(
                GuidanceDeliveryKind::Authoring,
                Some("s".into()),
                source.clone(),
                true,
            )
            .unwrap();
        let mut text = String::new();
        let digest = page.value["snapshot_sha256"].clone();
        loop {
            assert!(serde_json::to_string(&page.value).unwrap().len() < 14_000);
            assert_eq!(page.value["snapshot_sha256"], digest);
            text.push_str(page.value["content"].as_str().unwrap());
            if page.complete {
                break;
            }
            let cursor = page.value["next_arguments"]["cursor"].as_str().unwrap();
            page = store
                .resume(GuidanceDeliveryKind::Authoring, Some("s".into()), cursor)
                .unwrap();
        }
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), source);
    }

    #[test]
    fn cursor_cannot_skip_cross_tools_or_cross_sessions() {
        let store = GuidanceDeliveryStore::default();
        let page = store
            .begin(
                GuidanceDeliveryKind::Packet,
                Some("s".into()),
                json!({"text": "a".repeat(30_000)}),
                true,
            )
            .unwrap();
        assert!(!page.complete);
        let cursor = page.value["next_arguments"]["cursor"].as_str().unwrap();
        let id = cursor.split(':').next().unwrap();
        assert!(store
            .resume(
                GuidanceDeliveryKind::Packet,
                Some("s".into()),
                &format!("{id}:30000")
            )
            .is_err());
        assert!(store
            .resume(GuidanceDeliveryKind::Authoring, Some("s".into()), cursor)
            .is_err());
        assert!(store
            .resume(GuidanceDeliveryKind::Packet, Some("new".into()), cursor)
            .is_err());
        let first = store
            .resume(GuidanceDeliveryKind::Packet, Some("s".into()), cursor)
            .unwrap();
        let retry = store
            .resume(GuidanceDeliveryKind::Packet, Some("s".into()), cursor)
            .unwrap();
        assert_eq!(first.value, retry.value);
        assert!(!retry.complete);
    }

    #[test]
    fn small_documents_keep_existing_shape_and_cache_is_bounded() {
        let store = GuidanceDeliveryStore::default();
        let value = json!({"prompt_text": "canonical"});
        let result = store
            .begin(GuidanceDeliveryKind::Authoring, None, value.clone(), true)
            .unwrap();
        assert!(result.complete);
        assert_eq!(result.value, value);
        assert!(store
            .begin(
                GuidanceDeliveryKind::Authoring,
                None,
                json!({"text": "a".repeat(DOCUMENT_BYTES)}),
                true
            )
            .is_err());
    }
}
