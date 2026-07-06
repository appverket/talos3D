//! Session-scoped recipe draft registry.
//!
//! This is a bridge slice toward ADR-041's full dynamic recipe-learning
//! substrate. It lets the runtime capture missing-recipe work as session-local
//! draft artifacts linked to corpus gaps and source passages without yet
//! executing those drafts.

use std::collections::BTreeMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::curation::{CurationMeta, Scope};
use crate::plugins::knowledge_assets::{
    default_recipe_draft_meta, draft_meta, EvidenceSlot, KnowledgeResidency,
    RuntimeCapabilityClaim, RECIPE_DRAFT_KIND,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum RecipeDraftStatus {
    GapDetected,
    Sourced,
    Drafted,
    Validated,
    Installed,
}

impl RecipeDraftStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GapDetected => "gap_detected",
            Self::Sourced => "sourced",
            Self::Drafted => "drafted",
            Self::Validated => "validated",
            Self::Installed => "installed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "gap_detected" => Some(Self::GapDetected),
            "sourced" => Some(Self::Sourced),
            "draft" | "drafted" => Some(Self::Drafted),
            "validated" => Some(Self::Validated),
            "installed" => Some(Self::Installed),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parser_accepts_draft_alias_and_common_casing() {
        assert_eq!(
            RecipeDraftStatus::from_str("draft"),
            Some(RecipeDraftStatus::Drafted)
        );
        assert_eq!(
            RecipeDraftStatus::from_str("Drafted"),
            Some(RecipeDraftStatus::Drafted)
        );
        assert_eq!(
            RecipeDraftStatus::from_str("gap-detected"),
            Some(RecipeDraftStatus::GapDetected)
        );
        assert_eq!(RecipeDraftStatus::Drafted.as_str(), "drafted");
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RecipeDraftParameter {
    pub name: String,
    pub value_schema: Value,
    pub default: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RecipeDraftArtifact {
    pub id: String,
    #[serde(default = "default_recipe_draft_meta")]
    pub meta: CurationMeta,
    #[serde(default)]
    pub residency: KnowledgeResidency,
    pub label: String,
    pub description: String,
    pub target_class: String,
    pub supported_refinement_levels: Vec<String>,
    pub parameters: Vec<RecipeDraftParameter>,
    pub jurisdiction: Option<String>,
    pub gap_id: Option<String>,
    pub source_passage_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_slots: Vec<EvidenceSlot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_claims: Vec<RuntimeCapabilityClaim>,
    pub acquisition_context: Value,
    pub draft_script: Value,
    pub notes: Vec<String>,
    pub status: RecipeDraftStatus,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Resource, Default, Debug)]
pub struct RecipeDraftRegistry {
    entries: BTreeMap<String, RecipeDraftArtifact>,
    next_serial: u64,
}

impl RecipeDraftRegistry {
    pub fn get(&self, id: &str) -> Option<&RecipeDraftArtifact> {
        self.entries.get(id)
    }

    pub fn save(&mut self, mut draft: RecipeDraftArtifact) -> RecipeDraftArtifact {
        let now = unix_timestamp_seconds();
        let existing = self.entries.get(&draft.id).cloned();

        if draft.id.trim().is_empty() {
            draft.id = format!("recipe-draft-{}", self.next_serial);
            self.next_serial += 1;
        }

        if draft.meta.id.as_str().is_empty() {
            draft.meta = existing
                .as_ref()
                .map(|entry| entry.meta.clone())
                .unwrap_or_else(|| recipe_draft_meta_for(&draft, Scope::Project));
        }
        if draft.meta.id.as_str().is_empty() {
            draft.meta = recipe_draft_meta_for(&draft, Scope::Project);
        }

        draft.created_at = existing
            .as_ref()
            .map(|entry| entry.created_at)
            .unwrap_or_else(|| {
                if draft.created_at != 0 {
                    draft.created_at
                } else {
                    now
                }
            });
        draft.updated_at = now;

        self.entries.insert(draft.id.clone(), draft.clone());
        draft
    }

    pub fn set_status(
        &mut self,
        id: &str,
        status: RecipeDraftStatus,
    ) -> Result<RecipeDraftArtifact, String> {
        let Some(entry) = self.entries.get_mut(id) else {
            return Err(format!("recipe draft not found: '{id}'"));
        };
        entry.status = status;
        entry.updated_at = unix_timestamp_seconds();
        Ok(entry.clone())
    }

    pub fn list(
        &self,
        target_class: Option<&str>,
        status: Option<RecipeDraftStatus>,
    ) -> Vec<RecipeDraftArtifact> {
        self.entries
            .values()
            .filter(|entry| {
                target_class.is_none_or(|class| entry.target_class == class)
                    && status.is_none_or(|wanted| entry.status == wanted)
            })
            .cloned()
            .collect()
    }

    pub fn snapshot(&self) -> Vec<RecipeDraftArtifact> {
        self.entries.values().cloned().collect()
    }

    pub fn project_assets(&self) -> Vec<RecipeDraftArtifact> {
        self.entries
            .values()
            .filter(|entry| entry.meta.scope == Scope::Project)
            .cloned()
            .collect()
    }

    pub fn restore(&mut self, drafts: Vec<RecipeDraftArtifact>) {
        self.entries.clear();
        self.next_serial = 0;

        for draft in drafts {
            if let Some(serial) = draft
                .id
                .strip_prefix("recipe-draft-")
                .and_then(|value| value.parse::<u64>().ok())
            {
                self.next_serial = self.next_serial.max(serial + 1);
            }
            self.entries.insert(draft.id.clone(), draft);
        }
    }

    pub fn installed_for_class(&self, target_class: &str) -> Vec<RecipeDraftArtifact> {
        self.list(Some(target_class), Some(RecipeDraftStatus::Installed))
    }
}

pub fn recipe_draft_asset_id(id: &str) -> String {
    format!("{RECIPE_DRAFT_KIND}/{id}")
}

pub fn recipe_draft_meta_for(draft: &RecipeDraftArtifact, scope: Scope) -> CurationMeta {
    draft_meta(
        recipe_draft_asset_id(&draft.id),
        RECIPE_DRAFT_KIND,
        scope,
        draft.jurisdiction.as_deref(),
        draft.gap_id.as_deref(),
        None,
    )
}

pub struct RecipeDraftPlugin;

impl Plugin for RecipeDraftPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RecipeDraftRegistry>();
    }
}

fn unix_timestamp_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
