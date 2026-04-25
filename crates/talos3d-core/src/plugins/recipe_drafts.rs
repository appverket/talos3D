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

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "gap_detected" => Some(Self::GapDetected),
            "sourced" => Some(Self::Sourced),
            "drafted" => Some(Self::Drafted),
            "validated" => Some(Self::Validated),
            "installed" => Some(Self::Installed),
            _ => None,
        }
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
    pub label: String,
    pub description: String,
    pub target_class: String,
    pub supported_refinement_levels: Vec<String>,
    pub parameters: Vec<RecipeDraftParameter>,
    pub jurisdiction: Option<String>,
    pub gap_id: Option<String>,
    pub source_passage_refs: Vec<String>,
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
