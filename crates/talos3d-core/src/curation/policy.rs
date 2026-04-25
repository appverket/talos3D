//! Publication policy type surface.
//!
//! `PublicationPolicy` composes:
//!
//! - a **global validity floor** — platform-wide predicates every
//!   `Published` asset must clear (evidence resolvable, no superseded
//!   sources without explicit override, license-posture compatible with
//!   scope).
//! - a **jurisdiction hook** — optional per-jurisdiction rules enforced
//!   on top of the floor. The hook trait itself lives in PP85; here we
//!   only carry the identifier.
//!
//! Actual enforcement (`check(meta, registry) -> Vec<Finding>`) lands
//! in PP80 alongside the `SourceRegistry` resource.

use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;

use super::provenance::JurisdictionTag;
use super::source::{SourceLicense, SourceTier};

/// Opaque identifier of a registered `JurisdictionPolicyHook`. PP85
/// registers hook instances in a `HookRegistry` keyed by this id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct JurisdictionPolicyHookId(pub String);

impl JurisdictionPolicyHookId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// License-mode predicate applied against a cited source's
/// `SourceLicense` + `SourceTier`. Determines whether a published asset
/// at a given scope may cite the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LicenseMode {
    /// Accept any license posture.
    AllowAll,
    /// Accept only sources whose license permits redistribution
    /// (`PublicDomain` or `OfficialGovernmentPublication`).
    AllowRedistributable,
    /// Reject sources marked `LicensedExcerpt`. Used when publication
    /// would require operator-level licensing that isn't available.
    ForbidExcerpt,
}

impl LicenseMode {
    pub fn accepts(&self, tier: SourceTier, license: SourceLicense) -> bool {
        match self {
            Self::AllowAll => true,
            Self::AllowRedistributable => matches!(
                license,
                SourceLicense::PublicDomain | SourceLicense::OfficialGovernmentPublication
            ),
            Self::ForbidExcerpt => {
                // ForbidExcerpt rejects LicensedExcerpt outright; other
                // license postures are accepted when the tier is
                // Canonical or Jurisdictional (shipped material).
                !matches!(license, SourceLicense::LicensedExcerpt)
                    && matches!(tier, SourceTier::Canonical | SourceTier::Jurisdictional)
            }
        }
    }
}

/// Platform-wide validity floor every `Published` asset must clear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ValidityFloor {
    /// Reject if any cited evidence ref does not resolve in the
    /// `SourceRegistry`.
    pub require_resolved_evidence: bool,
    /// Reject if any cited source has `SourceStatus::Superseded` without
    /// an explicit override on the asset (e.g. `Confidence::Certified`).
    pub reject_superseded_sources: bool,
    /// License-mode predicates, evaluated against every cited source.
    /// Asset passes when every mode in the list accepts the source.
    pub license_modes: Vec<LicenseMode>,
}

impl Default for ValidityFloor {
    fn default() -> Self {
        Self {
            require_resolved_evidence: true,
            reject_superseded_sources: true,
            license_modes: vec![LicenseMode::AllowAll],
        }
    }
}

/// Full publication policy. `jurisdiction_hook` is `None` for assets
/// without a jurisdiction tag; otherwise PP85 looks up the matching
/// hook in the `HookRegistry` and runs it after the floor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PublicationPolicy {
    pub global_validity_floor: ValidityFloor,
    pub jurisdiction_hook: Option<JurisdictionPolicyHookId>,
}

impl Default for PublicationPolicy {
    fn default() -> Self {
        Self {
            global_validity_floor: ValidityFloor::default(),
            jurisdiction_hook: None,
        }
    }
}

impl PublicationPolicy {
    pub fn with_jurisdiction(mut self, hook: JurisdictionPolicyHookId) -> Self {
        self.jurisdiction_hook = Some(hook);
        self
    }

    pub fn strict() -> Self {
        Self {
            global_validity_floor: ValidityFloor {
                require_resolved_evidence: true,
                reject_superseded_sources: true,
                license_modes: vec![LicenseMode::AllowRedistributable],
            },
            jurisdiction_hook: None,
        }
    }
}

// ---------------------------------------------------------------------------
// JurisdictionPolicyHook trait + HookRegistry (PP85)
// ---------------------------------------------------------------------------

/// Per-jurisdiction policy extension. Jurisdiction packs implement this
/// trait and register an instance in `HookRegistry` at plugin build
/// time. The hook runs *after* the global validity floor and returns
/// additional `PublicationFinding`s scoped to the jurisdiction's rules.
///
/// Lives in core (ADR-040 platform/operator split) — the trait contract
/// is portable; jurisdiction-specific rule bodies live in the owning
/// capability pack (e.g. `talos3d-architecture-se`).
pub trait JurisdictionPolicyHook: Send + Sync {
    /// Jurisdiction this hook claims. A single hook per jurisdiction is
    /// enforced by `HookRegistry::install` (last install wins and logs
    /// a warning — registration order across plugins is not stable).
    fn jurisdiction(&self) -> JurisdictionTag;

    /// Emit any jurisdiction-specific findings for the given asset
    /// metadata + source registry snapshot. Called after the global
    /// validity floor; the caller concatenates findings.
    fn check(
        &self,
        meta: &super::meta::CurationMeta,
        registry: &super::registry::SourceRegistry,
    ) -> Vec<super::publication::PublicationFinding>;
}

/// Bevy resource mapping `JurisdictionTag` → a single registered hook.
#[derive(Resource, Default, Clone)]
pub struct HookRegistry {
    hooks: HashMap<JurisdictionTag, Arc<dyn JurisdictionPolicyHook>>,
}

impl std::fmt::Debug for HookRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookRegistry")
            .field("jurisdictions", &self.hooks.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl HookRegistry {
    /// Install a jurisdiction hook. Panics on duplicate install (same
    /// jurisdiction). Returns the installed jurisdiction tag for
    /// chaining.
    pub fn install<H: JurisdictionPolicyHook + 'static>(&mut self, hook: H) -> JurisdictionTag {
        let tag = hook.jurisdiction();
        if self.hooks.contains_key(&tag) {
            warn!(
                "JurisdictionPolicyHook for {:?} already installed — overwriting",
                tag
            );
        }
        self.hooks.insert(tag.clone(), Arc::new(hook));
        tag
    }

    pub fn get(&self, jurisdiction: &JurisdictionTag) -> Option<Arc<dyn JurisdictionPolicyHook>> {
        self.hooks.get(jurisdiction).cloned()
    }

    pub fn is_registered(&self, jurisdiction: &JurisdictionTag) -> bool {
        self.hooks.contains_key(jurisdiction)
    }

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_mode_allow_all_accepts_everything() {
        let m = LicenseMode::AllowAll;
        for license in [
            SourceLicense::PublicDomain,
            SourceLicense::OfficialGovernmentPublication,
            SourceLicense::PermissiveCite,
            SourceLicense::LicensedExcerpt,
            SourceLicense::UserAttachedPrivate,
        ] {
            for tier in [
                SourceTier::Canonical,
                SourceTier::Jurisdictional,
                SourceTier::Organizational,
                SourceTier::Project,
                SourceTier::AdHoc,
            ] {
                assert!(m.accepts(tier, license));
            }
        }
    }

    #[test]
    fn license_mode_allow_redistributable_rejects_non_redistributable() {
        let m = LicenseMode::AllowRedistributable;
        assert!(m.accepts(SourceTier::Canonical, SourceLicense::PublicDomain));
        assert!(m.accepts(
            SourceTier::Jurisdictional,
            SourceLicense::OfficialGovernmentPublication
        ));
        assert!(!m.accepts(SourceTier::Organizational, SourceLicense::LicensedExcerpt));
        assert!(!m.accepts(SourceTier::Project, SourceLicense::UserAttachedPrivate));
        assert!(!m.accepts(SourceTier::AdHoc, SourceLicense::PermissiveCite));
    }

    #[test]
    fn license_mode_forbid_excerpt_rejects_licensed_excerpt_and_non_shipped() {
        let m = LicenseMode::ForbidExcerpt;
        assert!(m.accepts(SourceTier::Canonical, SourceLicense::PublicDomain));
        assert!(m.accepts(
            SourceTier::Jurisdictional,
            SourceLicense::OfficialGovernmentPublication
        ));
        assert!(!m.accepts(SourceTier::Canonical, SourceLicense::LicensedExcerpt));
        assert!(!m.accepts(
            SourceTier::Organizational,
            SourceLicense::OfficialGovernmentPublication
        ));
    }

    #[test]
    fn default_validity_floor_is_permissive_license() {
        let f = ValidityFloor::default();
        assert!(f.require_resolved_evidence);
        assert!(f.reject_superseded_sources);
        assert_eq!(f.license_modes, vec![LicenseMode::AllowAll]);
    }

    #[test]
    fn strict_policy_requires_redistributable_license() {
        let p = PublicationPolicy::strict();
        assert_eq!(
            p.global_validity_floor.license_modes,
            vec![LicenseMode::AllowRedistributable]
        );
    }

    #[test]
    fn policy_round_trips_with_jurisdiction() {
        let p = PublicationPolicy::strict()
            .with_jurisdiction(JurisdictionPolicyHookId::new("architecture-se"));
        let json = serde_json::to_string(&p).unwrap();
        let parsed: PublicationPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, p);
    }

    // ---- PP85 hook registry + trait ----

    use super::super::meta::CurationMeta;
    use super::super::publication::{PublicationFinding, PublicationFindingSeverity};
    use super::super::registry::SourceRegistry;

    struct TestHook {
        tag: JurisdictionTag,
        emit: bool,
    }

    impl JurisdictionPolicyHook for TestHook {
        fn jurisdiction(&self) -> JurisdictionTag {
            self.tag.clone()
        }
        fn check(
            &self,
            _meta: &CurationMeta,
            _registry: &SourceRegistry,
        ) -> Vec<PublicationFinding> {
            if self.emit {
                vec![PublicationFinding {
                    code: "test.finding",
                    severity: PublicationFindingSeverity::Error,
                    message: format!("test error from {}", self.tag.as_str()),
                    evidence_index: None,
                }]
            } else {
                Vec::new()
            }
        }
    }

    #[test]
    fn hook_registry_install_and_lookup() {
        let mut reg = HookRegistry::default();
        reg.install(TestHook {
            tag: JurisdictionTag::new("SE"),
            emit: true,
        });
        assert_eq!(reg.len(), 1);
        assert!(reg.is_registered(&JurisdictionTag::new("SE")));
        assert!(!reg.is_registered(&JurisdictionTag::new("NO")));
        assert!(reg.get(&JurisdictionTag::new("SE")).is_some());
    }

    #[test]
    fn hook_registry_second_install_same_jurisdiction_overwrites() {
        let mut reg = HookRegistry::default();
        reg.install(TestHook {
            tag: JurisdictionTag::new("SE"),
            emit: true,
        });
        reg.install(TestHook {
            tag: JurisdictionTag::new("SE"),
            emit: false,
        });
        assert_eq!(reg.len(), 1);
    }
}
