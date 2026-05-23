//! PP-VILLA-1B — Scenario scoring harness.
//!
//! A domain-agnostic harness that scores a generated artifact against an
//! explicit [`ScenarioEnvelope`] — the required (and deferred) semantic element
//! classes and grounded claims an artifact must satisfy for pass/fail review.
//!
//! It composes the PP-VILLA-1A grounding gate
//! ([`run_grounding_gate_over_wire`]) for the zero-hallucination check and adds
//! element/claim *presence* checks plus a required-vs-deferred distinction
//! (first-artifact acceptance vs permit/drawing readiness).
//!
//! This module is the reusable, domain-neutral machinery. The villa-specific
//! envelope instance (the fixed Swedish villa prompt, plot fixture, and expected
//! envelope) belongs in the architecture domain crate next to the PP-VILLA-0B
//! sketch scenario, and feeds an [`ArtifactUnderTest`] into [`score_artifact`].

use serde::{Deserialize, Serialize};

use crate::plugins::grounding_gate::{
    run_grounding_gate_over_wire, GroundingGateReport, WireClaimEntry,
};

/// Whether a requirement must hold for *first-artifact* acceptance, or is
/// deferred to permit/drawing (constructible) readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum Requiredness {
    /// Must be satisfied for the first generated artifact to pass.
    Required,
    /// Not required for the first artifact; expected only at constructible /
    /// permit-drawing readiness.
    DeferredToConstructible,
}

/// A required semantic element class in the artifact
/// (e.g. `"conceptual_building_block"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ElementRequirement {
    /// The semantic element class that must be present.
    pub element_class: String,
    /// Minimum number of instances of this class required.
    pub min_count: usize,
    /// Whether this is required now or deferred.
    pub requiredness: Requiredness,
    /// Human-readable description for pass/fail review.
    pub description: String,
}

/// A required grounded claim: a claim path that must be present and carry a
/// valid grounding wire tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ClaimRequirement {
    /// The claim path that must be present (e.g. `"roof/intent"`).
    pub claim_path: String,
    /// Whether this is required now or deferred.
    pub requiredness: Requiredness,
    /// Human-readable description for pass/fail review.
    pub description: String,
}

/// The explicit expected-artifact envelope for a scenario.
///
/// This is the "pass/fail review" contract for a generation scenario such as
/// the Swedish villa (PP-VILLA-1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ScenarioEnvelope {
    /// Stable id for this scenario (e.g. `"swedish_villa_v1"`).
    pub scenario_id: String,
    /// The fixed prompt that drives the scenario.
    pub prompt: String,
    /// Element classes the artifact must contain.
    pub elements: Vec<ElementRequirement>,
    /// Grounded claims the artifact must carry.
    pub claims: Vec<ClaimRequirement>,
}

/// The artifact under test: the element classes present (one entry per instance)
/// and the grounding wire strings carried by its claims.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArtifactUnderTest {
    /// Semantic element class of each present element instance.
    pub element_classes: Vec<String>,
    /// `(claim_path, grounding_wire_string)` for each claim in the artifact.
    pub claims: Vec<(String, String)>,
}

/// A scenario requirement that was required-now but not satisfied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum ScenarioGap {
    /// A required element class was absent or under-represented.
    MissingElement {
        element_class: String,
        required_min: usize,
        found: usize,
    },
    /// A required claim path was not present on the artifact at all.
    MissingClaim { claim_path: String },
}

impl ScenarioGap {
    /// Human-readable description of the gap.
    pub fn message(&self) -> String {
        match self {
            Self::MissingElement {
                element_class,
                required_min,
                found,
            } => format!(
                "required element class '{}' under-represented: need {}, found {}",
                element_class, required_min, found
            ),
            Self::MissingClaim { claim_path } => {
                format!(
                    "required claim '{}' is absent from the artifact",
                    claim_path
                )
            }
        }
    }
}

/// The outcome of scoring an artifact against a [`ScenarioEnvelope`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ScenarioScoreReport {
    /// `true` when all *required-now* element/claim requirements are satisfied
    /// and the grounding gate passes.
    pub passed: bool,
    /// Required-now requirements that were not satisfied.
    pub gaps: Vec<ScenarioGap>,
    /// The grounding-gate result over the artifact's claims (zero-hallucination).
    pub grounding: GroundingGateReport,
    /// Deferred requirements not yet satisfied — informational, not failing.
    pub deferred_pending: Vec<String>,
}

/// Score an artifact against an envelope (PP-VILLA-1B).
///
/// Rules:
/// * Every `Required` [`ElementRequirement`] must be met by at least `min_count`
///   instances of its class, else a [`ScenarioGap::MissingElement`].
/// * Every `Required` [`ClaimRequirement`]'s `claim_path` must be present on the
///   artifact, else a [`ScenarioGap::MissingClaim`].
/// * The grounding gate is run over *all* the artifact's claims; any
///   untagged/ungrounded grounding fails (zero-hallucination).
/// * `DeferredToConstructible` requirements are never failing here; unmet ones
///   are reported under `deferred_pending` for visibility.
pub fn score_artifact(
    envelope: &ScenarioEnvelope,
    artifact: &ArtifactUnderTest,
) -> ScenarioScoreReport {
    let mut gaps: Vec<ScenarioGap> = Vec::new();
    let mut deferred_pending: Vec<String> = Vec::new();

    // Element requirements.
    for req in &envelope.elements {
        let found = artifact
            .element_classes
            .iter()
            .filter(|c| *c == &req.element_class)
            .count();
        let satisfied = found >= req.min_count;
        match req.requiredness {
            Requiredness::Required if !satisfied => gaps.push(ScenarioGap::MissingElement {
                element_class: req.element_class.clone(),
                required_min: req.min_count,
                found,
            }),
            Requiredness::DeferredToConstructible if !satisfied => deferred_pending.push(format!(
                "element '{}' ({})",
                req.element_class, req.description
            )),
            _ => {}
        }
    }

    // Claim presence requirements.
    for req in &envelope.claims {
        let present = artifact
            .claims
            .iter()
            .any(|(path, _)| path == &req.claim_path);
        match req.requiredness {
            Requiredness::Required if !present => gaps.push(ScenarioGap::MissingClaim {
                claim_path: req.claim_path.clone(),
            }),
            Requiredness::DeferredToConstructible if !present => {
                deferred_pending.push(format!("claim '{}' ({})", req.claim_path, req.description))
            }
            _ => {}
        }
    }

    // Zero-hallucination grounding gate over all artifact claims.
    let grounding = run_grounding_gate_over_wire(
        artifact
            .claims
            .iter()
            .map(|(path, grounding)| WireClaimEntry::new(path, grounding)),
    );

    let passed = gaps.is_empty() && grounding.passed;
    ScenarioScoreReport {
        passed,
        gaps,
        grounding,
        deferred_pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn villa_like_envelope() -> ScenarioEnvelope {
        ScenarioEnvelope {
            scenario_id: "example_villa".into(),
            prompt: "A small one-storey villa with a wood shed to the north.".into(),
            elements: vec![
                ElementRequirement {
                    element_class: "conceptual_building_block".into(),
                    min_count: 1,
                    requiredness: Requiredness::Required,
                    description: "main villa massing".into(),
                },
                ElementRequirement {
                    element_class: "conceptual_outbuilding_block".into(),
                    min_count: 1,
                    requiredness: Requiredness::Required,
                    description: "the wood shed".into(),
                },
                ElementRequirement {
                    element_class: "stair".into(),
                    min_count: 1,
                    requiredness: Requiredness::DeferredToConstructible,
                    description: "interior stair (constructible-stage)".into(),
                },
            ],
            claims: vec![
                ClaimRequirement {
                    claim_path: "roof/intent".into(),
                    requiredness: Requiredness::Required,
                    description: "gable-roof intent".into(),
                },
                ClaimRequirement {
                    claim_path: "envelope/fire_rating".into(),
                    requiredness: Requiredness::DeferredToConstructible,
                    description: "fire rating (permit-stage)".into(),
                },
            ],
        }
    }

    fn conforming_artifact() -> ArtifactUnderTest {
        ArtifactUnderTest {
            element_classes: vec![
                "conceptual_building_block".into(),
                "conceptual_outbuilding_block".into(),
            ],
            claims: vec![
                ("massing".into(), "user-specified".into()),
                ("roof/intent".into(), "policy-backed".into()),
                ("orientation".into(), "unresolved(CorpusGap)".into()),
            ],
        }
    }

    #[test]
    fn conforming_artifact_passes_and_lists_deferred() {
        let report = score_artifact(&villa_like_envelope(), &conforming_artifact());
        assert!(report.passed, "conforming artifact should pass");
        assert!(report.gaps.is_empty());
        assert!(report.grounding.passed);
        // The deferred stair + fire_rating are reported but do not fail.
        assert_eq!(report.deferred_pending.len(), 2);
    }

    #[test]
    fn missing_required_element_fails() {
        let mut artifact = conforming_artifact();
        artifact
            .element_classes
            .retain(|c| c != "conceptual_outbuilding_block");
        let report = score_artifact(&villa_like_envelope(), &artifact);
        assert!(!report.passed);
        assert!(report.gaps.iter().any(|g| matches!(
            g,
            ScenarioGap::MissingElement { element_class, .. }
                if element_class == "conceptual_outbuilding_block"
        )));
    }

    #[test]
    fn missing_required_claim_fails() {
        let mut artifact = conforming_artifact();
        artifact.claims.retain(|(path, _)| path != "roof/intent");
        let report = score_artifact(&villa_like_envelope(), &artifact);
        assert!(!report.passed);
        assert!(report.gaps.iter().any(|g| matches!(
            g,
            ScenarioGap::MissingClaim { claim_path } if claim_path == "roof/intent"
        )));
    }

    #[test]
    fn hallucinated_claim_fails_via_grounding_gate() {
        let mut artifact = conforming_artifact();
        artifact
            .claims
            .push(("beam_size".into(), "seems reasonable".into()));
        let report = score_artifact(&villa_like_envelope(), &artifact);
        assert!(!report.passed);
        assert!(
            !report.grounding.passed,
            "the grounding gate should flag it"
        );
        assert!(
            report.gaps.is_empty(),
            "presence is fine; only grounding fails"
        );
    }

    #[test]
    fn deferred_requirement_absent_does_not_fail() {
        // The conforming artifact lacks the deferred stair and fire_rating; it
        // must still pass (those are not required for the first artifact).
        let report = score_artifact(&villa_like_envelope(), &conforming_artifact());
        assert!(report.passed);
        assert!(report.deferred_pending.iter().any(|d| d.contains("stair")));
        assert!(report
            .deferred_pending
            .iter()
            .any(|d| d.contains("fire_rating")));
    }
}
