//! Publication-policy enforcement.
//!
//! `PublicationPolicy::check` walks a `CurationMeta` against a
//! `SourceRegistry` and returns findings for every floor rule it
//! violates. `publish_asset` MCP tools (landing in PP80 slice 6) reject
//! the publication attempt when any returned finding has severity
//! `Error`.
//!
//! The `JurisdictionPolicyHook` trait and per-jurisdiction rules land
//! in PP85; this slice only enforces the platform-wide floor.

use super::meta::CurationMeta;
use super::policy::{LicenseMode, PublicationPolicy, ValidityFloor};
use super::provenance::EvidenceRef;
use super::registry::SourceRegistry;
use super::scope_trust::Trust;
use super::source::{SourceRegistryEntry, SourceStatus};

/// Severity matching the shipped `plugins::refinement::FindingSeverity`
/// so call sites can fold publication findings into the existing
/// validation pipeline without a lossy conversion. Kept as a parallel
/// enum (rather than a re-export) so the curation module does not
/// force the refinement dependency on every consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PublicationFindingSeverity {
    Advice,
    Warning,
    Error,
}

impl PublicationFindingSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advice => "advice",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// A structured finding produced by `PublicationPolicy::check`. The
/// `source_id`, `revision`, and `claim_path` fields identify which piece
/// of evidence triggered the finding (if any); they are `None` for
/// floor-level failures that aren't tied to a specific evidence row.
#[derive(Debug, Clone, PartialEq)]
pub struct PublicationFinding {
    pub code: &'static str,
    pub severity: PublicationFindingSeverity,
    pub message: String,
    pub evidence_index: Option<usize>,
}

impl PublicationFinding {
    pub fn is_error(&self) -> bool {
        self.severity == PublicationFindingSeverity::Error
    }
}

impl PublicationPolicy {
    /// Run the policy against the given metadata + source registry.
    ///
    /// Returns all global floor violations. Jurisdiction-specific rules
    /// are run separately via [`PublicationPolicy::check_with_hooks`]
    /// once a `HookRegistry` is available.
    ///
    /// Note on `Draft` trust: the floor is informational for drafts —
    /// findings are still returned but a `Draft` asset isn't blocked.
    /// Enforcement happens at publication time (trust transition
    /// `Draft -> Published`).
    pub fn check(
        &self,
        meta: &CurationMeta,
        registry: &SourceRegistry,
    ) -> Vec<PublicationFinding> {
        let mut findings = Vec::new();
        let floor = &self.global_validity_floor;
        for (idx, evidence) in meta.provenance.evidence.iter().enumerate() {
            check_evidence(idx, evidence, meta, floor, registry, &mut findings);
        }
        findings
    }

    /// Extended check that additionally invokes the jurisdiction hook
    /// matching `meta.provenance.jurisdiction`, if one is installed.
    /// Findings are appended after the global-floor findings.
    pub fn check_with_hooks(
        &self,
        meta: &CurationMeta,
        registry: &SourceRegistry,
        hooks: &super::policy::HookRegistry,
    ) -> Vec<PublicationFinding> {
        let mut findings = self.check(meta, registry);
        if let Some(jurisdiction) = &meta.provenance.jurisdiction {
            if let Some(hook) = hooks.get(jurisdiction) {
                findings.extend(hook.check(meta, registry));
            }
        }
        findings
    }

    /// Convenience: returns `true` iff there are no `Error` findings
    /// in the global-floor check.
    pub fn permits(
        &self,
        meta: &CurationMeta,
        registry: &SourceRegistry,
    ) -> bool {
        !self.check(meta, registry).iter().any(|f| f.is_error())
    }

    /// Convenience: returns `true` iff global-floor + jurisdiction-hook
    /// check produces no `Error` findings.
    pub fn permits_with_hooks(
        &self,
        meta: &CurationMeta,
        registry: &SourceRegistry,
        hooks: &super::policy::HookRegistry,
    ) -> bool {
        !self
            .check_with_hooks(meta, registry, hooks)
            .iter()
            .any(|f| f.is_error())
    }
}

fn check_evidence(
    idx: usize,
    evidence: &EvidenceRef,
    meta: &CurationMeta,
    floor: &ValidityFloor,
    registry: &SourceRegistry,
    findings: &mut Vec<PublicationFinding>,
) {
    let entry = registry.get(&evidence.source_id, &evidence.revision);

    // Rule 1: resolved evidence.
    if floor.require_resolved_evidence && entry.is_none() {
        findings.push(PublicationFinding {
            code: "curation.publication.evidence_unresolved",
            severity: severity_for(meta.trust),
            message: format!(
                "evidence #{idx} references {}@{} which is not in the SourceRegistry",
                evidence.source_id.0, evidence.revision.0,
            ),
            evidence_index: Some(idx),
        });
        return;
    }

    let Some(entry) = entry else {
        // Unresolved and floor not requiring resolution; nothing else
        // to check for this evidence row.
        return;
    };

    // Rule 2: superseded sources without explicit override.
    if floor.reject_superseded_sources
        && matches!(entry.status, SourceStatus::Superseded { .. })
        && !has_supersede_override(meta)
    {
        findings.push(PublicationFinding {
            code: "curation.publication.source_superseded",
            severity: severity_for(meta.trust),
            message: format!(
                "evidence #{idx} cites superseded source {}@{}",
                entry.source_id.0, entry.revision.0,
            ),
            evidence_index: Some(idx),
        });
    }

    // Rule 2b: sunset sources are always errors regardless of override.
    if matches!(entry.status, SourceStatus::Sunset { .. }) {
        findings.push(PublicationFinding {
            code: "curation.publication.source_sunset",
            severity: PublicationFindingSeverity::Error,
            message: format!(
                "evidence #{idx} cites sunset source {}@{}",
                entry.source_id.0, entry.revision.0,
            ),
            evidence_index: Some(idx),
        });
    }

    // Rule 3: license-mode predicates.
    for mode in &floor.license_modes {
        if !mode.accepts(entry.tier, entry.license) {
            findings.push(PublicationFinding {
                code: license_finding_code(*mode),
                severity: severity_for(meta.trust),
                message: format!(
                    "evidence #{idx} cites {}@{} whose license ({:?} at tier {:?}) is not permitted by policy mode {:?}",
                    entry.source_id.0, entry.revision.0, entry.license, entry.tier, mode,
                ),
                evidence_index: Some(idx),
            });
        }
    }
}

fn license_finding_code(mode: LicenseMode) -> &'static str {
    match mode {
        LicenseMode::AllowAll => "curation.publication.license_allow_all",
        LicenseMode::AllowRedistributable => "curation.publication.license_requires_redistributable",
        LicenseMode::ForbidExcerpt => "curation.publication.license_forbids_excerpt",
    }
}

fn severity_for(trust: Trust) -> PublicationFindingSeverity {
    match trust {
        // Publication attempts on Draft still surface findings but at
        // Warning severity — `publish_recipe`/`publish_asset` promote
        // to Error when they flip trust to Published.
        Trust::Draft => PublicationFindingSeverity::Warning,
        Trust::Published => PublicationFindingSeverity::Error,
    }
}

fn has_supersede_override(meta: &CurationMeta) -> bool {
    // ADR-040: `Confidence::Certified` is the explicit override that
    // lets a specific publication attempt accept superseded evidence.
    meta.provenance.confidence == super::provenance::Confidence::Certified
}

/// Optional convenience aggregating [`SourceRegistryEntry`] lookups for
/// a whole metadata block. Not used by the floor itself; useful for
/// callers that want a "did every evidence row resolve?" check.
pub fn evidence_resolution_report<'a>(
    meta: &'a CurationMeta,
    registry: &'a SourceRegistry,
) -> Vec<(&'a EvidenceRef, Option<&'a SourceRegistryEntry>)> {
    meta.provenance
        .evidence
        .iter()
        .map(|e| (e, registry.get(&e.source_id, &e.revision)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::{
        identity::{AssetId, AssetKindId, SourceId, SourceRevision},
        provenance::{Confidence, EvidenceRef, GroundingKind, JurisdictionTag, Lineage, Provenance},
        registry::SourceRegistry,
        scope_trust::{Scope, Trust},
        source::{SourceLicense, SourceRegistryEntry, SourceTier},
    };
    use crate::plugins::refinement::{AgentId, RuleId};

    fn registered_entry(reg: &mut SourceRegistry, id: &str, rev: &str) {
        reg.insert(
            SourceRegistryEntry::new(
                SourceId::new(id),
                SourceRevision::new(rev),
                format!("{id} {rev}"),
                "Test Publisher",
                SourceTier::Jurisdictional,
                SourceLicense::OfficialGovernmentPublication,
            )
            .with_jurisdiction(JurisdictionTag::new("SE")),
        );
    }

    fn meta_with_evidence(refs: Vec<EvidenceRef>, trust: Trust, confidence: Confidence) -> CurationMeta {
        CurationMeta::new(
            AssetId::new("recipe.v1/test"),
            AssetKindId::new("recipe.v1"),
            Provenance {
                author: AgentId("test".into()),
                confidence,
                lineage: Lineage::Freeform,
                rationale: None,
                jurisdiction: Some(JurisdictionTag::new("SE")),
                catalog_dependencies: Vec::new(),
                evidence: refs,
            },
        )
        .with_scope(Scope::Project)
        .with_trust(trust)
    }

    fn ev(id: &str, rev: &str) -> EvidenceRef {
        EvidenceRef {
            source_id: SourceId::new(id),
            revision: SourceRevision::new(rev),
            claim_path: None,
            excerpt_ref: None,
            grounding_kind: GroundingKind::ExplicitRule(RuleId("test".into())),
        }
    }

    #[test]
    fn resolved_evidence_passes_default_floor() {
        let mut reg = SourceRegistry::default();
        registered_entry(&mut reg, "bbr.8", "2011:6");
        let meta = meta_with_evidence(vec![ev("bbr.8", "2011:6")], Trust::Published, Confidence::Medium);
        let findings = PublicationPolicy::default().check(&meta, &reg);
        assert!(findings.is_empty(), "findings: {findings:?}");
    }

    #[test]
    fn unresolved_evidence_is_error_when_published() {
        let reg = SourceRegistry::default();
        let meta = meta_with_evidence(vec![ev("bbr.8", "2011:6")], Trust::Published, Confidence::Medium);
        let findings = PublicationPolicy::default().check(&meta, &reg);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "curation.publication.evidence_unresolved");
        assert!(findings[0].is_error());
    }

    #[test]
    fn unresolved_evidence_is_warning_when_draft() {
        let reg = SourceRegistry::default();
        let meta = meta_with_evidence(vec![ev("bbr.8", "2011:6")], Trust::Draft, Confidence::Medium);
        let findings = PublicationPolicy::default().check(&meta, &reg);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, PublicationFindingSeverity::Warning);
    }

    #[test]
    fn superseded_source_fails_unless_certified() {
        let mut reg = SourceRegistry::default();
        registered_entry(&mut reg, "bbr.8", "2011:6");
        assert!(reg.supersede(
            &SourceId::new("bbr.8"),
            &SourceRevision::new("2011:6"),
            Some(SourceId::new("bbr.8.v2025")),
        ));
        let meta = meta_with_evidence(vec![ev("bbr.8", "2011:6")], Trust::Published, Confidence::Medium);
        let findings = PublicationPolicy::default().check(&meta, &reg);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "curation.publication.source_superseded");

        // Certified confidence overrides superseded.
        let meta = meta_with_evidence(vec![ev("bbr.8", "2011:6")], Trust::Published, Confidence::Certified);
        let findings = PublicationPolicy::default().check(&meta, &reg);
        assert!(findings.is_empty());
    }

    #[test]
    fn sunset_source_fails_even_with_certified_override() {
        let mut reg = SourceRegistry::default();
        registered_entry(&mut reg, "vendor.x", "2020");
        assert!(reg.sunset(
            &SourceId::new("vendor.x"),
            &SourceRevision::new("2020"),
            "withdrawn".into(),
        ));
        let meta = meta_with_evidence(
            vec![ev("vendor.x", "2020")],
            Trust::Published,
            Confidence::Certified,
        );
        let findings = PublicationPolicy::default().check(&meta, &reg);
        assert!(findings.iter().any(|f| f.code == "curation.publication.source_sunset"));
    }

    #[test]
    fn license_mode_allow_redistributable_rejects_licensed_excerpt() {
        let mut reg = SourceRegistry::default();
        reg.insert(SourceRegistryEntry::new(
            SourceId::new("vendor.pdf"),
            SourceRevision::new("v1"),
            "Acme manual",
            "Acme",
            SourceTier::Organizational,
            SourceLicense::LicensedExcerpt,
        ));
        let meta = meta_with_evidence(vec![ev("vendor.pdf", "v1")], Trust::Published, Confidence::Medium);
        let findings = PublicationPolicy::strict().check(&meta, &reg);
        assert!(findings
            .iter()
            .any(|f| f.code == "curation.publication.license_requires_redistributable"));
        assert!(!PublicationPolicy::strict().permits(&meta, &reg));
    }

    #[test]
    fn default_policy_permits_when_resolved_and_active() {
        let mut reg = SourceRegistry::default();
        registered_entry(&mut reg, "bbr.8", "2011:6");
        let meta = meta_with_evidence(vec![ev("bbr.8", "2011:6")], Trust::Published, Confidence::Medium);
        assert!(PublicationPolicy::default().permits(&meta, &reg));
    }

    #[test]
    fn evidence_resolution_report_pairs_refs_with_registry_hits() {
        let mut reg = SourceRegistry::default();
        registered_entry(&mut reg, "bbr.8", "2011:6");
        let meta = meta_with_evidence(
            vec![ev("bbr.8", "2011:6"), ev("missing", "v0")],
            Trust::Draft,
            Confidence::Medium,
        );
        let report = evidence_resolution_report(&meta, &reg);
        assert_eq!(report.len(), 2);
        assert!(report[0].1.is_some());
        assert!(report[1].1.is_none());
    }

    // ---- PP85 check_with_hooks ----

    struct AlwaysRejectHook(JurisdictionTag);

    impl super::super::policy::JurisdictionPolicyHook for AlwaysRejectHook {
        fn jurisdiction(&self) -> JurisdictionTag {
            self.0.clone()
        }
        fn check(
            &self,
            _meta: &CurationMeta,
            _registry: &SourceRegistry,
        ) -> Vec<PublicationFinding> {
            vec![PublicationFinding {
                code: "jurisdiction.test.always_reject",
                severity: PublicationFindingSeverity::Error,
                message: "policy denied".into(),
                evidence_index: None,
            }]
        }
    }

    #[test]
    fn check_with_hooks_invokes_matching_jurisdiction_hook() {
        use super::super::policy::HookRegistry;
        let mut reg = SourceRegistry::default();
        registered_entry(&mut reg, "bbr.8", "2011:6");
        let mut hooks = HookRegistry::default();
        hooks.install(AlwaysRejectHook(JurisdictionTag::new("SE")));
        let meta = meta_with_evidence(vec![ev("bbr.8", "2011:6")], Trust::Published, Confidence::Medium);
        let findings = PublicationPolicy::default().check_with_hooks(&meta, &reg, &hooks);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "jurisdiction.test.always_reject");
    }

    #[test]
    fn check_with_hooks_ignores_hook_for_other_jurisdiction() {
        use super::super::policy::HookRegistry;
        let mut reg = SourceRegistry::default();
        registered_entry(&mut reg, "bbr.8", "2011:6");
        let mut hooks = HookRegistry::default();
        hooks.install(AlwaysRejectHook(JurisdictionTag::new("NO")));
        let meta = meta_with_evidence(vec![ev("bbr.8", "2011:6")], Trust::Published, Confidence::Medium);
        // Asset is SE; hook is NO — should not fire.
        let findings = PublicationPolicy::default().check_with_hooks(&meta, &reg, &hooks);
        assert!(findings.is_empty());
    }

    #[test]
    fn check_with_hooks_concatenates_floor_and_hook_findings() {
        use super::super::policy::HookRegistry;
        let reg = SourceRegistry::default(); // empty → unresolved evidence
        let mut hooks = HookRegistry::default();
        hooks.install(AlwaysRejectHook(JurisdictionTag::new("SE")));
        let meta = meta_with_evidence(vec![ev("bbr.8", "2011:6")], Trust::Published, Confidence::Medium);
        let findings = PublicationPolicy::default().check_with_hooks(&meta, &reg, &hooks);
        assert_eq!(findings.len(), 2);
        assert!(findings
            .iter()
            .any(|f| f.code == "curation.publication.evidence_unresolved"));
        assert!(findings
            .iter()
            .any(|f| f.code == "jurisdiction.test.always_reject"));
    }
}
