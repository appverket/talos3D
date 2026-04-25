//! Cross-kind dependency references.
//!
//! Per ADR-040, dependencies are **cross-kind**: a recipe may depend on a
//! material spec; a product entry may depend on a material spec; a rule
//! pack may depend on a source revision; a jurisdiction pack may depend
//! on a recipe pack and a code-rule pack. This is expressed by including
//! both `target_kind` and `target_id` on every `DependencyRef`.
//!
//! The `role` distinction matters: "depends on for execution", "depends
//! on for validation", and "depends on for citation/provenance
//! completeness" are not the same relationship. PP84 uses `role` to
//! decide when unresolved deps error vs warn.

use serde::{Deserialize, Serialize};

use super::identity::{AssetId, AssetKindId, AssetRevision};

/// Why one asset depends on another.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord, Default,
)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DependencyRole {
    /// Required to execute the asset (e.g. a recipe invoking a material
    /// spec). Unresolved ⇒ `invoke_*` errors.
    #[default]
    Execution,
    /// Required to validate the asset (e.g. a rule pack citing a specific
    /// source revision). Unresolved ⇒ `validate_*` / `publish_*` warns
    /// or errors depending on publication policy.
    Validation,
    /// Required only so the asset's provenance is complete (e.g. a
    /// product entry citing a manufacturer-installation manual).
    /// Unresolved ⇒ `publish_*` errors.
    Citation,
}

/// Reference to another curated asset as a dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct DependencyRef {
    pub target_kind: AssetKindId,
    pub target_id: AssetId,
    pub revision: AssetRevision,
    #[serde(default, skip_serializing_if = "is_default_role")]
    pub role: DependencyRole,
    #[serde(default, skip_serializing_if = "is_false")]
    pub optional: bool,
}

fn is_default_role(r: &DependencyRole) -> bool {
    *r == DependencyRole::Execution
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl DependencyRef {
    pub fn new(target_kind: AssetKindId, target_id: AssetId, revision: AssetRevision) -> Self {
        Self {
            target_kind,
            target_id,
            revision,
            role: DependencyRole::Execution,
            optional: false,
        }
    }

    pub fn with_role(mut self, role: DependencyRole) -> Self {
        self.role = role;
        self
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::identity::ContentHash;

    fn rev(v: u64) -> AssetRevision {
        AssetRevision {
            version: v,
            content_hash: Some(ContentHash::new(format!("blake3:v{v}"))),
        }
    }

    #[test]
    fn dependency_ref_defaults_to_execution_role() {
        let d = DependencyRef::new(
            AssetKindId::new("material_spec.v1"),
            AssetId::new("material_spec.v1/timber_c24"),
            rev(1),
        );
        assert_eq!(d.role, DependencyRole::Execution);
        assert!(!d.optional);
    }

    #[test]
    fn dependency_ref_builder_sets_role_and_optional() {
        let d = DependencyRef::new(
            AssetKindId::new("source"),
            AssetId::new("boverket.bbr.8"),
            rev(2),
        )
        .with_role(DependencyRole::Citation)
        .optional();
        assert_eq!(d.role, DependencyRole::Citation);
        assert!(d.optional);
    }

    #[test]
    fn dependency_ref_elides_default_role_in_json() {
        let d = DependencyRef::new(
            AssetKindId::new("material_spec.v1"),
            AssetId::new("material_spec.v1/timber_c24"),
            rev(1),
        );
        let json = serde_json::to_string(&d).unwrap();
        assert!(!json.contains("\"role\""));
    }

    #[test]
    fn dependency_ref_emits_non_default_role_in_json() {
        let d = DependencyRef::new(
            AssetKindId::new("source"),
            AssetId::new("boverket.bbr.8"),
            rev(1),
        )
        .with_role(DependencyRole::Validation);
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"role\":\"validation\""));
    }

    #[test]
    fn dependency_ref_round_trips_all_variants() {
        let variants = [
            DependencyRef::new(
                AssetKindId::new("recipe.v1"),
                AssetId::new("recipe.v1/foo"),
                rev(1),
            ),
            DependencyRef::new(
                AssetKindId::new("material_spec.v1"),
                AssetId::new("material_spec.v1/timber_c24"),
                rev(3),
            )
            .with_role(DependencyRole::Execution)
            .optional(),
            DependencyRef::new(
                AssetKindId::new("source"),
                AssetId::new("boverket.bbr.8"),
                rev(2),
            )
            .with_role(DependencyRole::Citation),
        ];
        for d in variants {
            let json = serde_json::to_string(&d).unwrap();
            let parsed: DependencyRef = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, d);
        }
    }
}
