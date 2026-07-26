//! Corpus health lint (ADR-064 §5).
//!
//! Gap classes that are **derivable by static inspection** of the graph, with
//! no agent involvement. This matters because the moment a confident model is
//! least likely to notice it is stuck is exactly the moment it is wrong — so
//! gap detection must not depend on an agent remembering to ask.
//!
//! Linkage totality is a check, not an advisory: a promoted concept that links
//! to no element class, or a proposition naming an unregistered anchor kind, is
//! a defect in the corpus rather than a style preference.

use super::graph::PropositionObject;
use super::ids::{AnchorKindId, ConceptId, PropositionId};
use super::registry::SemanticRegistry;

/// What kind of corpus defect was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CorpusFindingKind {
    /// A concept links to no element class, so it has meaning but no
    /// representation (ADR-064 §4, linkage totality).
    ConceptWithoutElementClass,
    /// A proposition references an anchor kind that is not registered.
    PropositionReferencesUnknownAnchorKind,
    /// A proposition references a concept that is not registered.
    PropositionReferencesUnknownConcept,
    /// Some concept consumes this anchor kind but nothing publishes it, so the
    /// requirement can never be satisfied.
    AnchorKindConsumedButNeverPublished,
    /// An anchor kind is published but nothing consumes it. Not fatal, but it
    /// usually means a concept family is half-authored.
    AnchorKindPublishedButNeverConsumed,
    /// A binding proposition carries no evidence. Binding claims refuse other
    /// people's work, so they may not rest on assertion alone.
    BindingPropositionWithoutEvidence,
    /// A binding proposition declares no regional scope. "General" must be a
    /// deliberate choice, not an omission — this is how a local detail
    /// silently becomes a universal rule.
    BindingPropositionWithoutRegionalScope,
    /// A publication contract names a concept that is not registered.
    PublicationByUnknownConcept,
    /// A publication contract names an anchor kind that is not registered.
    PublicationOfUnknownAnchorKind,
    /// A deprecated concept is still named as the object of a proposition.
    PropositionTargetsDeprecatedConcept,
}

impl CorpusFindingKind {
    /// Whether this finding should fail a corpus gate rather than advise.
    ///
    /// The unsatisfiable and dangling-reference cases block: they make the
    /// graph incoherent. The half-authored and missing-metadata cases advise
    /// loudly but do not stop a build, so that a pack under construction stays
    /// usable.
    pub fn is_blocking(self) -> bool {
        matches!(
            self,
            Self::PropositionReferencesUnknownAnchorKind
                | Self::PropositionReferencesUnknownConcept
                | Self::AnchorKindConsumedButNeverPublished
                | Self::PublicationByUnknownConcept
                | Self::PublicationOfUnknownAnchorKind
        )
    }
}

/// One corpus-health finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusFinding {
    pub kind: CorpusFindingKind,
    /// The record the finding is about, as a readable id.
    pub subject: String,
    pub detail: String,
}

impl CorpusFinding {
    pub fn is_blocking(&self) -> bool {
        self.kind.is_blocking()
    }
}

/// Inspect the compiled graph and report every derivable defect.
///
/// Deterministic: the registry iterates in sorted order, so findings are
/// stable across runs and diffable in CI.
pub fn lint_corpus(registry: &SemanticRegistry) -> Vec<CorpusFinding> {
    let mut findings = Vec::new();

    for concept in registry.concepts() {
        if concept.is_active() && concept.applicable_element_classes.is_empty() {
            findings.push(CorpusFinding {
                kind: CorpusFindingKind::ConceptWithoutElementClass,
                subject: concept.id.to_string(),
                detail: "Concept has meaning but links to no element class; it cannot be \
                         represented."
                    .to_string(),
            });
        }
    }

    for contract in registry.publications() {
        if registry.concept(&contract.publisher).is_none() {
            findings.push(unknown_reference(
                CorpusFindingKind::PublicationByUnknownConcept,
                &contract.publisher.to_string(),
                "publishes an anchor but is not a registered concept",
            ));
        }
        if registry.anchor_kind(&contract.anchor_kind).is_none() {
            findings.push(unknown_reference(
                CorpusFindingKind::PublicationOfUnknownAnchorKind,
                &contract.anchor_kind.to_string(),
                "is published but is not a registered anchor kind",
            ));
        }
    }

    for proposition in registry.propositions() {
        check_proposition_target(
            registry,
            proposition.id.clone(),
            &proposition.object,
            &mut findings,
        );

        let is_binding = proposition.required_by.is_some() || proposition.identity_defining;
        if is_binding && proposition.evidence.is_empty() {
            findings.push(CorpusFinding {
                kind: CorpusFindingKind::BindingPropositionWithoutEvidence,
                subject: proposition.id.to_string(),
                detail: "Binding proposition carries no evidence; it will refuse authoring on \
                         assertion alone."
                    .to_string(),
            });
        }
        if is_binding && proposition.regional_scope.is_empty() {
            findings.push(CorpusFinding {
                kind: CorpusFindingKind::BindingPropositionWithoutRegionalScope,
                subject: proposition.id.to_string(),
                detail: "Binding proposition declares no regional scope; general applicability \
                         must be deliberate, not an omission."
                    .to_string(),
            });
        }
    }

    for kind in registry.anchor_kinds() {
        let publishers = registry.publishers_of(&kind.id);
        let consumers = registry.consumers_of(&kind.id);
        if consumers.is_empty() && !publishers.is_empty() {
            findings.push(CorpusFinding {
                kind: CorpusFindingKind::AnchorKindPublishedButNeverConsumed,
                subject: kind.id.to_string(),
                detail: "Anchor kind is published but no concept resolves against it.".to_string(),
            });
        }
        if !consumers.is_empty() && publishers.is_empty() {
            findings.push(CorpusFinding {
                kind: CorpusFindingKind::AnchorKindConsumedButNeverPublished,
                subject: kind.id.to_string(),
                detail: format!(
                    "Anchor kind is required by [{}] but no concept publishes it, so the \
                     requirement can never be satisfied.",
                    consumers
                        .iter()
                        .map(ConceptId::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
    }

    findings.sort_by(|a, b| (a.kind, &a.subject).cmp(&(b.kind, &b.subject)));
    findings
}

fn check_proposition_target(
    registry: &SemanticRegistry,
    proposition: PropositionId,
    object: &PropositionObject,
    findings: &mut Vec<CorpusFinding>,
) {
    match object {
        PropositionObject::AnchorKind(kind) => {
            if registry.anchor_kind(kind).is_none() {
                findings.push(CorpusFinding {
                    kind: CorpusFindingKind::PropositionReferencesUnknownAnchorKind,
                    subject: proposition.to_string(),
                    detail: format!("References unregistered anchor kind `{kind}`."),
                });
            }
        }
        PropositionObject::Concept(concept) => match registry.concept(concept) {
            None => findings.push(CorpusFinding {
                kind: CorpusFindingKind::PropositionReferencesUnknownConcept,
                subject: proposition.to_string(),
                detail: format!("References unregistered concept `{concept}`."),
            }),
            Some(target) if !target.is_active() => findings.push(CorpusFinding {
                kind: CorpusFindingKind::PropositionTargetsDeprecatedConcept,
                subject: proposition.to_string(),
                detail: format!("Targets deprecated concept `{concept}`."),
            }),
            Some(_) => {}
        },
    }
}

fn unknown_reference(kind: CorpusFindingKind, subject: &str, detail: &str) -> CorpusFinding {
    CorpusFinding {
        kind,
        subject: subject.to_string(),
        detail: format!("`{subject}` {detail}."),
    }
}

/// Anchor kinds that some concept requires but nothing publishes — the gap
/// class most likely to strand an agent mid-task.
pub fn unsatisfiable_anchor_kinds(registry: &SemanticRegistry) -> Vec<AnchorKindId> {
    registry
        .anchor_kinds()
        .filter(|kind| {
            !registry.consumers_of(&kind.id).is_empty()
                && registry.publishers_of(&kind.id).is_empty()
        })
        .map(|kind| kind.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantics::graph::{
        AdmissibilityProposition, AnchorCardinality, AnchorGeometry, AnchorKindDescriptor,
        Cardinality, Concept,
    };
    use crate::semantics::ids::{AnchorKindId, PredicateId};
    use crate::semantics::test_fixtures::roof_edge_fixture;

    fn anchor(id: &str) -> AnchorKindDescriptor {
        AnchorKindDescriptor {
            id: AnchorKindId::new(id),
            label: id.into(),
            description: String::new(),
            geometry: AnchorGeometry::Line,
            cardinality: AnchorCardinality::ExactlyOne,
        }
    }

    fn requires(subject: &str, anchor_kind: &str) -> AdmissibilityProposition {
        AdmissibilityProposition {
            id: PropositionId::new(format!("p.{subject}")),
            subject: ConceptId::new(subject),
            predicate: PredicateId::new("follows"),
            object: PropositionObject::AnchorKind(AnchorKindId::new(anchor_kind)),
            cardinality: Cardinality::ExactlyOne,
            identity_defining: true,
            required_by: None,
            regional_scope: Vec::new(),
            evidence: Vec::new(),
            refusal_reason: String::new(),
            repair_hint: String::new(),
        }
    }

    #[test]
    fn detects_an_anchor_required_but_never_published() {
        let registry = SemanticRegistry::compile(
            [Concept::new(ConceptId::new("c"))],
            [anchor("orphan.anchor")],
            [],
            [requires("c", "orphan.anchor")],
        );
        let findings = lint_corpus(&registry);
        assert!(findings
            .iter()
            .any(|finding| finding.kind == CorpusFindingKind::AnchorKindConsumedButNeverPublished));
        assert_eq!(
            unsatisfiable_anchor_kinds(&registry),
            vec![AnchorKindId::new("orphan.anchor")]
        );
    }

    #[test]
    fn detects_a_proposition_referencing_an_unregistered_anchor_kind() {
        let registry = SemanticRegistry::compile(
            [Concept::new(ConceptId::new("c"))],
            [],
            [],
            [requires("c", "never.registered")],
        );
        let findings = lint_corpus(&registry);
        let finding = findings
            .iter()
            .find(|finding| {
                finding.kind == CorpusFindingKind::PropositionReferencesUnknownAnchorKind
            })
            .expect("dangling anchor reference must be reported");
        assert!(finding.is_blocking(), "a dangling reference must block");
    }

    #[test]
    fn detects_binding_propositions_without_evidence_or_scope() {
        let registry = SemanticRegistry::compile(
            [Concept::new(ConceptId::new("c"))],
            [anchor("a")],
            [],
            [requires("c", "a")],
        );
        let findings = lint_corpus(&registry);
        assert!(findings
            .iter()
            .any(|f| f.kind == CorpusFindingKind::BindingPropositionWithoutEvidence));
        assert!(findings
            .iter()
            .any(|f| f.kind == CorpusFindingKind::BindingPropositionWithoutRegionalScope));
    }

    #[test]
    fn linkage_totality_flags_a_concept_with_no_element_class() {
        let registry = SemanticRegistry::compile([Concept::new(ConceptId::new("c"))], [], [], []);
        assert!(lint_corpus(&registry)
            .iter()
            .any(|f| f.kind == CorpusFindingKind::ConceptWithoutElementClass));
    }

    #[test]
    fn the_roof_edge_slice_has_no_blocking_findings() {
        // The shipped slice may carry advisory findings (the fixture omits
        // evidence), but it must never be structurally incoherent.
        let blocking: Vec<_> = lint_corpus(&roof_edge_fixture())
            .into_iter()
            .filter(CorpusFinding::is_blocking)
            .collect();
        assert!(
            blocking.is_empty(),
            "unexpected blocking findings: {blocking:#?}"
        );
    }

    #[test]
    fn findings_are_deterministically_ordered() {
        let registry = roof_edge_fixture();
        assert_eq!(lint_corpus(&registry), lint_corpus(&registry));
    }
}
