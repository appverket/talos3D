//! Scope and trust axes plus the shared validation-status type.
//!
//! Per ADR-040 the approval model is two **orthogonal** axes:
//!
//! - `Scope` — who can see / use the asset (Session → Project → Org → Shipped)
//! - `Trust` — whether it has been reviewed for reuse (Draft / Published)
//!
//! Promotion is two independent moves: widen scope, elevate trust.

use serde::{Deserialize, Serialize};

/// Visibility / addressability scope of a curated asset.
///
/// Default is `Session`: the artifact lives only for the duration of the
/// current authoring session unless explicitly saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum Scope {
    /// Only visible for the current authoring session. Not persisted.
    #[default]
    Session,
    /// Persisted with the current project file.
    Project,
    /// Shared across the authoring organization (hosted library or local org registry).
    Org,
    /// Part of a shipped capability pack (Canonical or Jurisdictional).
    Shipped,
}

impl Scope {
    /// Returns `true` when `self` is at least as broad as `other`.
    pub fn at_least(self, other: Scope) -> bool {
        self >= other
    }
}

/// Review/trust level of a curated asset. Orthogonal to `Scope` — a
/// Project-scope asset can be Published, and a Shipped-scope asset can be
/// Draft during a release candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum Trust {
    /// Author-declared; not yet reviewed for reuse. Synthesis-mode
    /// invocation is permitted for Draft recipe bodies.
    #[default]
    Draft,
    /// Reviewed, has tests or explicit sign-off, safe for reuse. Replay-
    /// mode invocation is the default for Published recipe bodies.
    Published,
}

/// Validation status of a curated asset. Set by the substrate's
/// `validate_asset` / `publish_asset` flows; does not drive authoring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ValidationStatus {
    /// No validation run has happened yet. Default for freshly-authored
    /// artifacts.
    #[default]
    Unchecked,
    /// Last validation run passed all enforced policies.
    Passing,
    /// Last validation run produced findings. Findings are identified by
    /// opaque strings so this module does not take on the dependency on
    /// `FindingId` from the constraint layer.
    Failing {
        findings: Vec<String>,
    },
}

impl ValidationStatus {
    pub fn is_passing(&self) -> bool {
        matches!(self, Self::Passing)
    }

    pub fn is_failing(&self) -> bool {
        matches!(self, Self::Failing { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_ordering_is_widening() {
        assert!(Scope::Session < Scope::Project);
        assert!(Scope::Project < Scope::Org);
        assert!(Scope::Org < Scope::Shipped);
    }

    #[test]
    fn scope_at_least_matches_ordering() {
        assert!(Scope::Shipped.at_least(Scope::Session));
        assert!(Scope::Project.at_least(Scope::Project));
        assert!(!Scope::Session.at_least(Scope::Project));
    }

    #[test]
    fn trust_default_is_draft() {
        assert_eq!(Trust::default(), Trust::Draft);
    }

    #[test]
    fn validation_status_round_trips() {
        let unchecked = ValidationStatus::Unchecked;
        let passing = ValidationStatus::Passing;
        let failing = ValidationStatus::Failing {
            findings: vec!["bbr_stair_riser_max".into(), "missing_evidence".into()],
        };

        for s in [unchecked, passing, failing] {
            let json = serde_json::to_string(&s).unwrap();
            let parsed: ValidationStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, s);
        }
    }

    #[test]
    fn validation_status_discriminators_are_stable() {
        assert_eq!(
            serde_json::to_string(&ValidationStatus::Unchecked).unwrap(),
            "{\"state\":\"unchecked\"}"
        );
        assert_eq!(
            serde_json::to_string(&ValidationStatus::Passing).unwrap(),
            "{\"state\":\"passing\"}"
        );
    }

    #[test]
    fn scope_and_trust_are_orthogonal() {
        // A Project-scope asset can be Published; a Shipped-scope asset can be Draft.
        let combos: &[(Scope, Trust)] = &[
            (Scope::Session, Trust::Draft),
            (Scope::Project, Trust::Published),
            (Scope::Org, Trust::Draft),
            (Scope::Shipped, Trust::Draft),
            (Scope::Shipped, Trust::Published),
        ];
        for (s, t) in combos {
            let json = serde_json::to_string(&(s, t)).unwrap();
            let _: (Scope, Trust) = serde_json::from_str(&json).unwrap();
        }
    }
}
