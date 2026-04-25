//! Session-scoped assembly pattern draft registry.
//!
//! This mirrors the recipe-draft bridge slice, but for reusable layered
//! assembly patterns such as wall or roof build-ups. These drafts are
//! consultable and cacheable, but not executable code.

use std::collections::BTreeMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::capability_registry::{
    AssemblyPatternDescriptor, AssemblyPatternLayerDescriptor, AssemblyPatternRelationRule,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum AssemblyPatternDraftStatus {
    GapDetected,
    Sourced,
    Drafted,
    Validated,
    Installed,
}

impl AssemblyPatternDraftStatus {
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
pub struct AssemblyPatternDraftArtifact {
    pub id: String,
    pub label: String,
    pub description: String,
    pub target_types: Vec<String>,
    pub axis: String,
    pub layers: Vec<AssemblyPatternLayerDescriptor>,
    pub relation_rules: Vec<AssemblyPatternRelationRule>,
    pub root_layer_ids: Vec<String>,
    pub requires_support_path: bool,
    pub tags: Vec<String>,
    pub parameter_schema: serde_json::Value,
    pub jurisdiction: Option<String>,
    pub gap_id: Option<String>,
    pub source_passage_refs: Vec<String>,
    pub acquisition_context: serde_json::Value,
    pub notes: Vec<String>,
    pub status: AssemblyPatternDraftStatus,
    pub created_at: i64,
    pub updated_at: i64,
}

impl AssemblyPatternDraftArtifact {
    pub fn to_descriptor(&self) -> AssemblyPatternDescriptor {
        AssemblyPatternDescriptor {
            pattern_id: self.id.clone(),
            label: self.label.clone(),
            description: self.description.clone(),
            target_types: self.target_types.clone(),
            axis: self.axis.clone(),
            layers: self.layers.clone(),
            relation_rules: self.relation_rules.clone(),
            root_layer_ids: self.root_layer_ids.clone(),
            requires_support_path: self.requires_support_path,
            tags: self.tags.clone(),
            parameter_schema: self.parameter_schema.clone(),
        }
    }
}

#[derive(Resource, Default, Debug)]
pub struct AssemblyPatternDraftRegistry {
    entries: BTreeMap<String, AssemblyPatternDraftArtifact>,
    next_serial: u64,
}

impl AssemblyPatternDraftRegistry {
    pub fn get(&self, id: &str) -> Option<&AssemblyPatternDraftArtifact> {
        self.entries.get(id)
    }

    pub fn save(
        &mut self,
        mut draft: AssemblyPatternDraftArtifact,
    ) -> AssemblyPatternDraftArtifact {
        let now = unix_timestamp_seconds();
        let existing = self.entries.get(&draft.id).cloned();

        if draft.id.trim().is_empty() {
            draft.id = format!("assembly-pattern-draft-{}", self.next_serial);
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
        status: AssemblyPatternDraftStatus,
    ) -> Result<AssemblyPatternDraftArtifact, String> {
        let Some(entry) = self.entries.get_mut(id) else {
            return Err(format!("assembly pattern draft not found: '{id}'"));
        };
        entry.status = status;
        entry.updated_at = unix_timestamp_seconds();
        Ok(entry.clone())
    }

    pub fn list(
        &self,
        target_type: Option<&str>,
        status: Option<AssemblyPatternDraftStatus>,
    ) -> Vec<AssemblyPatternDraftArtifact> {
        self.entries
            .values()
            .filter(|entry| {
                target_type
                    .is_none_or(|wanted| entry.target_types.iter().any(|value| value == wanted))
                    && status.is_none_or(|wanted| entry.status == wanted)
            })
            .cloned()
            .collect()
    }

    pub fn snapshot(&self) -> Vec<AssemblyPatternDraftArtifact> {
        self.entries.values().cloned().collect()
    }

    pub fn restore(&mut self, drafts: Vec<AssemblyPatternDraftArtifact>) {
        self.entries.clear();
        self.next_serial = 0;

        for draft in drafts {
            if let Some(serial) = draft
                .id
                .strip_prefix("assembly-pattern-draft-")
                .and_then(|value| value.parse::<u64>().ok())
            {
                self.next_serial = self.next_serial.max(serial + 1);
            }
            self.entries.insert(draft.id.clone(), draft);
        }
    }

    pub fn installed_for_target_type(
        &self,
        target_type: &str,
    ) -> Vec<AssemblyPatternDraftArtifact> {
        self.list(
            Some(target_type),
            Some(AssemblyPatternDraftStatus::Installed),
        )
    }

    pub fn installed_descriptors(&self) -> Vec<AssemblyPatternDescriptor> {
        self.entries
            .values()
            .filter(|entry| entry.status == AssemblyPatternDraftStatus::Installed)
            .map(AssemblyPatternDraftArtifact::to_descriptor)
            .collect()
    }
}

pub struct AssemblyPatternDraftPlugin;

impl Plugin for AssemblyPatternDraftPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AssemblyPatternDraftRegistry>();
    }
}

fn unix_timestamp_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
