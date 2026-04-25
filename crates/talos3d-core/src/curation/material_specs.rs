//! `MaterialSpec` — curated construction-material semantics on the
//! curation substrate.
//!
//! This is the first non-recipe curated kind carried by ADR-040. It is
//! intentionally narrower than a full BIM material ontology: enough to
//! express authored construction-material identity, standards linkage,
//! and common performance properties, while staying small enough for
//! early pack/bootstrap workflows.

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plugins::refinement::AgentId;

use super::{
    compatibility::{CompatibilityRef, VersionReq},
    identity::{AssetId, AssetKindId, SourceId, SourceRevision},
    meta::CurationMeta,
    provenance::{ExcerptRef, Provenance},
    scope_trust::{Scope, Trust},
};

pub const MATERIAL_SPEC_KIND: &str = "material_spec.v1";
pub const MATERIAL_SPEC_BODY_SCHEMA_VERSION: u32 = 1;

static MATERIAL_SPEC_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct StandardRef {
    pub source_id: SourceId,
    pub revision: SourceRevision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt_ref: Option<ExcerptRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct MaterialIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct StructuralProperties {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compressive_strength_mpa: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tensile_strength_mpa: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modulus_of_elasticity_mpa: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ThermalProperties {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conductivity_w_mk: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r_value_m2k_w: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specific_heat_j_kgk: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AcousticProperties {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound_reduction_db: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_sound_improvement_db: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct FireProperties {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaction_to_fire_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire_rating: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct MoistureProperties {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vapor_resistance_factor: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moisture_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct MaterialSpecBody {
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classification: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<MaterialIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub standards: Vec<StandardRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structural: Option<StructuralProperties>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal: Option<ThermalProperties>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acoustic: Option<AcousticProperties>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire: Option<FireProperties>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moisture: Option<MoistureProperties>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density_kg_m3: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_units: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_rendering_hint: Option<AssetId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct MaterialSpec {
    pub meta: CurationMeta,
    pub body: MaterialSpecBody,
}

impl MaterialSpec {
    pub fn kind() -> AssetKindId {
        AssetKindId::new(MATERIAL_SPEC_KIND)
    }

    pub fn asset_id_for(slug: impl Into<String>) -> AssetId {
        AssetId::new(format!("{MATERIAL_SPEC_KIND}/{}", slug.into()))
    }

    pub fn draft(
        asset_id: AssetId,
        body: MaterialSpecBody,
        author: AgentId,
        rationale: Option<String>,
    ) -> Self {
        let mut provenance = Provenance::freeform(author);
        provenance.rationale = rationale;
        let compatibility = CompatibilityRef::for_core(VersionReq::new("^0.1"))
            .with_capability(Self::kind(), VersionReq::new("^1"))
            .with_body_schema(Self::kind(), MATERIAL_SPEC_BODY_SCHEMA_VERSION);
        Self {
            meta: CurationMeta::new(asset_id, Self::kind(), provenance)
                .with_scope(Scope::Project)
                .with_trust(Trust::Draft)
                .with_compatibility(compatibility),
            body,
        }
    }
}

#[derive(Resource, Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct MaterialSpecRegistry {
    pub entries: BTreeMap<AssetId, MaterialSpec>,
}

impl MaterialSpecRegistry {
    pub fn insert(&mut self, spec: MaterialSpec) -> AssetId {
        let id = spec.meta.id.clone();
        self.entries.insert(id.clone(), spec);
        id
    }

    pub fn get(&self, id: &AssetId) -> Option<&MaterialSpec> {
        self.entries.get(id)
    }

    pub fn get_mut(&mut self, id: &AssetId) -> Option<&mut MaterialSpec> {
        self.entries.get_mut(id)
    }

    pub fn remove(&mut self, id: &AssetId) -> Option<MaterialSpec> {
        self.entries.remove(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &MaterialSpec> {
        self.entries.values()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub fn next_material_spec_asset_id() -> AssetId {
    let sequence = MATERIAL_SPEC_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    MaterialSpec::asset_id_for(format!("material-spec-{timestamp_ms}-{sequence}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::Confidence;

    #[test]
    fn draft_material_spec_defaults_to_project_scope() {
        let spec = MaterialSpec::draft(
            MaterialSpec::asset_id_for("c24_timber"),
            MaterialSpecBody {
                display_name: "C24 Structural Timber".into(),
                ..Default::default()
            },
            AgentId("codex".into()),
            Some("bootstrap".into()),
        );

        assert_eq!(spec.meta.kind, MaterialSpec::kind());
        assert_eq!(spec.meta.scope, Scope::Project);
        assert_eq!(spec.meta.trust, Trust::Draft);
        assert_eq!(spec.meta.provenance.confidence, Confidence::Low);
        assert_eq!(
            spec.meta
                .compatibility
                .body_schema
                .as_ref()
                .map(|schema| schema.version),
            Some(MATERIAL_SPEC_BODY_SCHEMA_VERSION)
        );
    }

    #[test]
    fn registry_round_trips_entry() {
        let mut registry = MaterialSpecRegistry::default();
        let spec = MaterialSpec::draft(
            MaterialSpec::asset_id_for("mineral_wool"),
            MaterialSpecBody {
                display_name: "Mineral Wool 45kg/m3".into(),
                ..Default::default()
            },
            AgentId("claude".into()),
            None,
        );
        let id = registry.insert(spec.clone());
        let json = serde_json::to_string(&registry).unwrap();
        let parsed: MaterialSpecRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.get(&id), Some(&spec));
    }
}
