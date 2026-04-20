//! `RecipeArtifact` — the recipe-kind instantiation of `CurationMeta`.
//!
//! Per ADR-041 a recipe artifact wraps today's `RecipeFamilyDescriptor`
//! with governance metadata and a body union:
//!
//! ```text
//! RecipeArtifact {
//!     meta: CurationMeta,
//!     body: RecipeBody,          // NativeFnRef | AuthoringScript (PP82)
//!     parameter_schema: JsonSchema,
//!     target_class: ElementClassId,
//!     supported_refinement_states: Vec<RefinementState>,
//!     tests: Vec<ScenarioTest>,
//! }
//! ```
//!
//! PP81 (this slice) lands the type surface and the empty
//! `RecipeArtifactRegistry` resource. A later slice installs a startup
//! mirror that walks `CapabilityRegistry.recipe_family_descriptors` and
//! fills the artifact registry at `Scope::Shipped, Trust::Published,
//! body: NativeFnRef`. The `AuthoringScript` body variant gets its
//! actual schema in PP82; here it's a minimal opaque placeholder.

use std::collections::{BTreeMap, HashMap};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capability_registry::RecipeFamilyId;
use crate::plugins::refinement::RefinementState;

use super::identity::{AssetId, AssetKindId};
use super::meta::CurationMeta;

/// Kind id for recipe artifacts.
pub const RECIPE_ARTIFACT_KIND: &str = "recipe.v1";

/// Stable identifier of a registered native generation function.
/// Typically the `RecipeFamilyId` string; kept as a distinct newtype so
/// future non-recipe native-function kinds can share the convention.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct NativeFnId(pub String);

impl NativeFnId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Derive a `NativeFnId` from a `RecipeFamilyId`. The convention
    /// is that a native recipe body is named after its family id.
    pub fn from_family(family_id: &RecipeFamilyId) -> Self {
        Self(family_id.0.clone())
    }
}

/// Recipe body union. `NativeFnRef` points at a `GenerateFn` registered
/// in `CapabilityRegistry`; `AuthoringScript` carries a normalized
/// parameterized script over the Model API surface (shape filled in by
/// PP82).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecipeBody {
    /// Reference to a native `GenerateFn`. `family_id` identifies the
    /// registered closure inside `CapabilityRegistry`.
    NativeFnRef { family_id: RecipeFamilyId, fn_id: NativeFnId },
    /// Placeholder for the `AuthoringScript` body format. PP82 replaces
    /// the `opaque` field with the real typed schema.
    AuthoringScript { opaque: Value },
}

impl RecipeBody {
    pub fn native(family_id: RecipeFamilyId) -> Self {
        let fn_id = NativeFnId::from_family(&family_id);
        Self::NativeFnRef { family_id, fn_id }
    }

    pub fn is_native(&self) -> bool {
        matches!(self, Self::NativeFnRef { .. })
    }

    pub fn family_id(&self) -> Option<&RecipeFamilyId> {
        match self {
            Self::NativeFnRef { family_id, .. } => Some(family_id),
            Self::AuthoringScript { .. } => None,
        }
    }
}

/// Scenario-test stub. PP81 mirrors whatever tests the shipped
/// descriptor declares via a `name` plus a JSON payload; PP82 replaces
/// this with the structured ScenarioTest from ADR-041 once the
/// `AuthoringScript` body lands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ScenarioTest {
    pub name: String,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub expectations: Value,
}

/// Recipe artifact — the PP81/ADR-041 wrapper around today's
/// `RecipeFamilyDescriptor`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RecipeArtifact {
    pub meta: CurationMeta,
    pub body: RecipeBody,
    /// Mirrors `RecipeFamilyDescriptor.parameters` (serialized JSON
    /// schema of accepted parameters with defaults).
    pub parameter_schema: Value,
    /// Owning element-class id as a string. Mirrors
    /// `RecipeFamilyDescriptor.target_class.0`. Kept as a String here
    /// to avoid a direct dependency on the shipped ElementClassId type
    /// from the curation module.
    pub target_class: String,
    pub supported_refinement_states: Vec<RefinementState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<ScenarioTest>,
}

impl RecipeArtifact {
    /// Deterministic `AssetId` for a recipe family: `recipe.v1/<family>`.
    pub fn asset_id_for(family_id: &RecipeFamilyId) -> AssetId {
        AssetId::new(format!("{RECIPE_ARTIFACT_KIND}/{}", family_id.0))
    }

    pub fn kind() -> AssetKindId {
        AssetKindId::new(RECIPE_ARTIFACT_KIND)
    }

    pub fn family_id(&self) -> Option<&RecipeFamilyId> {
        self.body.family_id()
    }
}

/// Bevy resource holding all registered `RecipeArtifact`s keyed by
/// `AssetId`, plus a `family_id → asset_id` index for lookups from the
/// shipped descriptor vocabulary.
#[derive(Resource, Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeArtifactRegistry {
    pub entries: BTreeMap<AssetId, RecipeArtifact>,
    /// `RecipeFamilyId` does not implement `Ord` in the shipped API, so
    /// this index is a `HashMap` rather than a `BTreeMap`. Iteration
    /// order is not part of the contract here; use `entries` for
    /// deterministic walks.
    pub by_family_id: HashMap<RecipeFamilyId, AssetId>,
}

impl RecipeArtifactRegistry {
    pub fn insert(&mut self, artifact: RecipeArtifact) -> AssetId {
        let asset_id = artifact.meta.id.clone();
        if let Some(family) = artifact.family_id().cloned() {
            self.by_family_id.insert(family, asset_id.clone());
        }
        self.entries.insert(asset_id.clone(), artifact);
        asset_id
    }

    pub fn get(&self, id: &AssetId) -> Option<&RecipeArtifact> {
        self.entries.get(id)
    }

    pub fn get_by_family(&self, family_id: &RecipeFamilyId) -> Option<&RecipeArtifact> {
        self.by_family_id.get(family_id).and_then(|id| self.entries.get(id))
    }

    pub fn iter(&self) -> impl Iterator<Item = &RecipeArtifact> {
        self.entries.values()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::ElementClassId;
    use crate::curation::{
        identity::AssetRevision,
        provenance::{Confidence, Lineage, Provenance},
        scope_trust::{Scope, Trust},
    };
    use crate::plugins::refinement::AgentId;

    fn sample_meta(family: &RecipeFamilyId) -> CurationMeta {
        let id = RecipeArtifact::asset_id_for(family);
        CurationMeta::new(
            id,
            RecipeArtifact::kind(),
            Provenance {
                author: AgentId("test".into()),
                confidence: Confidence::High,
                lineage: Lineage::Freeform,
                rationale: None,
                jurisdiction: None,
                catalog_dependencies: Vec::new(),
                evidence: Vec::new(),
            },
        )
        .with_scope(Scope::Shipped)
        .with_trust(Trust::Published)
    }

    fn sample_artifact(family: &str, target: &str) -> RecipeArtifact {
        let family = RecipeFamilyId(family.into());
        RecipeArtifact {
            meta: sample_meta(&family),
            body: RecipeBody::native(family),
            parameter_schema: serde_json::json!({"type": "object"}),
            target_class: target.into(),
            supported_refinement_states: vec![RefinementState::Constructible],
            tests: Vec::new(),
        }
    }

    #[test]
    fn asset_id_for_is_stable_slug() {
        let id = RecipeArtifact::asset_id_for(&RecipeFamilyId("stair_straight_residential".into()));
        assert_eq!(id.as_str(), "recipe.v1/stair_straight_residential");
    }

    #[test]
    fn recipe_body_native_carries_family_and_fn_id() {
        let body = RecipeBody::native(RecipeFamilyId("foo".into()));
        match body {
            RecipeBody::NativeFnRef { family_id, fn_id } => {
                assert_eq!(family_id.0, "foo");
                assert_eq!(fn_id.as_str(), "foo");
            }
            _ => panic!("expected NativeFnRef"),
        }
    }

    #[test]
    fn recipe_body_is_native_helper() {
        let n = RecipeBody::native(RecipeFamilyId("x".into()));
        assert!(n.is_native());
        assert_eq!(n.family_id().unwrap().0, "x");

        let s = RecipeBody::AuthoringScript {
            opaque: serde_json::json!({}),
        };
        assert!(!s.is_native());
        assert!(s.family_id().is_none());
    }

    #[test]
    fn registry_insert_tracks_by_asset_and_by_family() {
        let mut reg = RecipeArtifactRegistry::default();
        let art = sample_artifact("pier_foundation", "foundation_system");
        let id = reg.insert(art);
        assert_eq!(reg.len(), 1);
        assert!(reg.get(&id).is_some());
        let by_fam = reg
            .get_by_family(&RecipeFamilyId("pier_foundation".into()))
            .expect("family index");
        assert_eq!(by_fam.target_class, "foundation_system");
    }

    #[test]
    fn recipe_artifact_round_trips_through_json() {
        let art = sample_artifact("wall_light_frame_exterior", "wall_assembly");
        let json = serde_json::to_string(&art).unwrap();
        let parsed: RecipeArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, art);
    }

    #[test]
    fn registry_round_trips() {
        let mut reg = RecipeArtifactRegistry::default();
        reg.insert(sample_artifact("pier_foundation", "foundation_system"));
        reg.insert(sample_artifact(
            "wall_light_frame_exterior",
            "wall_assembly",
        ));
        let json = serde_json::to_string(&reg).unwrap();
        let parsed: RecipeArtifactRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, reg);
    }

    #[test]
    fn asset_id_kind_is_recipe_v1() {
        assert_eq!(RecipeArtifact::kind().as_str(), "recipe.v1");
    }

    #[test]
    fn native_fn_id_from_family() {
        let id = NativeFnId::from_family(&RecipeFamilyId("wall_light_frame_exterior".into()));
        assert_eq!(id.as_str(), "wall_light_frame_exterior");
    }

    // Importing ElementClassId here only to document that the curation
    // module is agnostic to it — target_class is stored as a String,
    // not as the shipped newtype. The import silences an "unused"
    // warning by participating in a debug_assert_eq.
    #[test]
    fn target_class_is_string_not_element_class_id() {
        let art = sample_artifact("pier_foundation", "foundation_system");
        let round_tripped = serde_json::to_string(&art).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&round_tripped).unwrap();
        assert_eq!(raw["target_class"], "foundation_system");
        let _cls = ElementClassId("foundation_system".into()); // just reference the type
    }
}
