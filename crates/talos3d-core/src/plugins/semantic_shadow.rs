use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::plugins::refinement::{SemanticSourceRef, UnresolvedDecisionRecord};

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SemanticShadow {
    pub source: SemanticShadowSource,
    #[serde(default)]
    pub candidates: Vec<SemanticShadowCandidate>,
    #[serde(default)]
    pub gaps: Vec<SemanticShadowGap>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SemanticShadowSource {
    pub kind: String,
    pub format_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SemanticShadowCandidate {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_class: Option<String>,
    pub confidence: f32,
    pub status: SemanticShadowCandidateStatus,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub unresolved_decisions: Vec<UnresolvedDecisionRecord>,
    #[serde(default)]
    pub source_refs: Vec<SemanticSourceRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SemanticShadowCandidateStatus {
    Inferred,
    AcceptedNativeClaim,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SemanticShadowGap {
    pub id: String,
    pub category: String,
    pub severity: SemanticShadowGapSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SemanticShadowGapSeverity {
    Info,
    Warning,
    Blocked,
}

pub fn semantic_shadow_for_import_request(
    format_name: &str,
    source_name: Option<&str>,
    request: &Value,
) -> Option<SemanticShadow> {
    let entity_type = request.get("type").and_then(Value::as_str)?;
    if entity_type != "triangle_mesh" {
        return None;
    }

    let mesh_name = request
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty());
    let label = mesh_name
        .map(|name| format!("Imported mesh '{name}'"))
        .unwrap_or_else(|| "Imported mesh geometry".to_string());
    let mut evidence = vec![format!("Imported as {entity_type} from {format_name}")];
    if let Some(mesh_name) = mesh_name {
        evidence.push(format!("Foreign object/group name: {mesh_name}"));
    }
    if let Some(source_name) = source_name {
        evidence.push(format!("Source file: {source_name}"));
    }

    let reference = source_name
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format_name.to_string());
    Some(SemanticShadow {
        source: SemanticShadowSource {
            kind: "foreign_import".to_string(),
            format_name: format_name.to_string(),
            source_name: source_name.map(ToOwned::to_owned),
        },
        candidates: vec![SemanticShadowCandidate {
            id: "imported_foreign_geometry".to_string(),
            label,
            element_class: Some("imported_foreign_geometry".to_string()),
            confidence: 0.35,
            status: SemanticShadowCandidateStatus::Inferred,
            parameters: json!({
                "source_entity_type": entity_type,
                "source_format": format_name,
                "source_name": source_name,
                "foreign_name": mesh_name,
            }),
            evidence,
            unresolved_decisions: vec![UnresolvedDecisionRecord {
                id: "semantic_class_unverified".to_string(),
                question: "Which native Talos3D element class should this imported geometry become?"
                    .to_string(),
                reason: "Foreign geometry import preserves mesh context but does not authoritatively classify semantics."
                    .to_string(),
                grounding: "semantic_shadow_import".to_string(),
            }],
            source_refs: vec![SemanticSourceRef {
                reference,
                claim: "Foreign mesh imported as inferred semantic context only".to_string(),
                grounding: "semantic_shadow_import".to_string(),
            }],
        }],
        gaps: vec![SemanticShadowGap {
            id: "unsupported_semantic_mapping".to_string(),
            category: "unsupported_semantics".to_string(),
            severity: SemanticShadowGapSeverity::Warning,
            message: "Importer did not map foreign object names, layers, or material hints into authoritative native semantic classes."
                .to_string(),
        }],
    })
}
