//! The Admissibility Kernel (ADR-064 §3, agreement §6).
//!
//! Evaluates a [`SemanticPlan`] against the [`SemanticRegistry`] and returns
//! `Admit` / `AdmitWithObligation` / `Refuse`. Two rules do the real work:
//!
//! 1. **Positive admissibility.** A binding is legal only if some proposition
//!    permits it. There is no forbidden-host list; a host that publishes no
//!    matching anchor simply offers nothing to point at.
//! 2. **Missing is not contradictory.** An *absent* binding may remain an
//!    obligation until its declared refinement state. An *asserted* binding
//!    that an identity-defining proposition excludes refuses at every state —
//!    refinement defers missing detail, it never authorises a contradiction.
//!
//! The kernel never repairs. It refuses and hands back what would satisfy it.

use crate::plugins::identity::ElementId;
use crate::plugins::refinement::RefinementState;

use super::graph::{AdmissibilityProposition, PropositionObject};
use super::ids::{AnchorKindId, ConceptId, PredicateId, PropositionId};
use super::plan::{BindTarget, PlanIntent, SemanticContext, SemanticPlan};
use super::registry::SemanticRegistry;

/// An obligation the plan may proceed under, to be discharged by a later state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingObligation {
    pub entity: ElementId,
    pub proposition: PropositionId,
    pub due_by: RefinementState,
    pub summary: String,
}

/// A refusal, carrying everything the caller needs to succeed on its next try.
///
/// The fields exist so a diagnostic can name the violated proposition, what was
/// actually observed, and the specific entity/anchor that would satisfy it —
/// the standard PP-TDR-1 measures as "refusal actionability".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub violated: Option<PropositionId>,
    /// Why this is wrong, in the user's language.
    pub reason: String,
    /// What the world actually looks like.
    pub observed: String,
    /// What to do instead.
    pub repair: String,
    /// Concepts the caller may have meant, for reidentification repair.
    pub contrasts: Vec<ConceptId>,
}

impl Refusal {
    /// Single-line rendering for logs and terse tool responses.
    pub fn summary(&self) -> String {
        format!("{} {} {}", self.reason, self.observed, self.repair)
    }
}

/// The kernel's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Admit,
    AdmitWithObligation(Vec<PendingObligation>),
    Refuse(Vec<Refusal>),
}

impl Verdict {
    /// Whether the mutation may proceed. `AdmitWithObligation` proceeds.
    pub fn is_admitted(&self) -> bool {
        !matches!(self, Verdict::Refuse(_))
    }

    pub fn refusals(&self) -> &[Refusal] {
        match self {
            Verdict::Refuse(refusals) => refusals,
            _ => &[],
        }
    }

    pub fn obligations(&self) -> &[PendingObligation] {
        match self {
            Verdict::AdmitWithObligation(obligations) => obligations,
            _ => &[],
        }
    }
}

/// Evaluate a plan. Pure: same registry + context + plan yields the same verdict.
pub fn evaluate(
    registry: &SemanticRegistry,
    context: &impl SemanticContext,
    plan: &SemanticPlan,
) -> Verdict {
    if plan.is_empty() {
        // No semantic claim: the kernel is disarmed and geometry is untouched.
        return Verdict::Admit;
    }

    let jurisdiction = context.jurisdiction();
    // Concepts assigned within this same plan must be visible to later intents,
    // otherwise "assign concept then bind it" refuses spuriously.
    let mut refusals = Vec::new();
    let mut obligations = Vec::new();

    for intent in &plan.intents {
        match intent {
            PlanIntent::AssignConcept { entity, concept } => {
                evaluate_assignment(
                    registry,
                    context,
                    *entity,
                    concept,
                    &mut refusals,
                    &mut obligations,
                    jurisdiction.as_ref(),
                );
            }
            PlanIntent::RemoveConcept { entity, concept } => {
                obligations.push(downgrade_obligation(registry, *entity, concept));
            }
            PlanIntent::Bind {
                subject,
                predicate,
                target,
            } => {
                evaluate_binding(
                    registry,
                    context,
                    plan,
                    *subject,
                    predicate,
                    target,
                    &mut refusals,
                    jurisdiction.as_ref(),
                );
            }
        }
    }

    if !refusals.is_empty() {
        Verdict::Refuse(refusals)
    } else if !obligations.is_empty() {
        Verdict::AdmitWithObligation(obligations)
    } else {
        Verdict::Admit
    }
}

/// Assigning a concept raises obligations for bindings not yet present. It
/// never refuses on absence alone — that is the missing/contradictory split.
fn evaluate_assignment(
    registry: &SemanticRegistry,
    context: &impl SemanticContext,
    entity: ElementId,
    concept: &ConceptId,
    refusals: &mut Vec<Refusal>,
    obligations: &mut Vec<PendingObligation>,
    jurisdiction: Option<&crate::curation::JurisdictionTag>,
) {
    let Some(record) = registry.concept(concept) else {
        refusals.push(Refusal {
            violated: None,
            reason: format!("Unknown concept `{concept}`."),
            observed: "No concept with that id is registered.".to_string(),
            repair: "Resolve the term first, or raise a corpus gap; do not \
                     invent a concept id."
                .to_string(),
            contrasts: Vec::new(),
        });
        return;
    };

    if !record.is_active() {
        // Deprecated concepts still resolve, but assigning one is worth saying.
        obligations.push(PendingObligation {
            entity,
            proposition: PropositionId::new(format!("deprecated:{concept}")),
            due_by: RefinementState::Schematic,
            summary: format!("`{concept}` is deprecated; prefer its successor."),
        });
    }

    let state = context.refinement_state(entity);
    for proposition in registry.propositions_of_subject(concept, jurisdiction) {
        let Some(due_by) = proposition.required_by else {
            continue;
        };
        let already_bound = !context
            .existing_bindings(entity, &proposition.predicate)
            .is_empty();
        if already_bound {
            continue;
        }
        // Absence is deferrable until the declared state, then it is owed.
        obligations.push(PendingObligation {
            entity,
            proposition: proposition.id.clone(),
            due_by,
            summary: if proposition.is_required_at(state) {
                format!(
                    "`{}` requires {} by {}; it is unresolved.",
                    proposition.subject,
                    describe_object(&proposition.object),
                    due_by.as_str()
                )
            } else {
                format!(
                    "`{}` will require {} at {}.",
                    proposition.subject,
                    describe_object(&proposition.object),
                    due_by.as_str()
                )
            },
        });
    }
}

/// Removing a concept is an explicit downgrade with a loss manifest.
fn downgrade_obligation(
    registry: &SemanticRegistry,
    entity: ElementId,
    concept: &ConceptId,
) -> PendingObligation {
    let lost_anchors = registry.published_by_concept(concept);
    let lost = if lost_anchors.is_empty() {
        "no published anchors".to_string()
    } else {
        format!(
            "published anchors [{}]",
            lost_anchors
                .iter()
                .map(AnchorKindId::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    PendingObligation {
        entity,
        proposition: PropositionId::new(format!("downgrade:{concept}")),
        due_by: RefinementState::Conceptual,
        summary: format!(
            "Removing `{concept}` withdraws its domain claim and {lost}; \
             dependents resolving against them are invalidated."
        ),
    }
}

fn evaluate_binding(
    registry: &SemanticRegistry,
    context: &impl SemanticContext,
    plan: &SemanticPlan,
    subject: ElementId,
    predicate: &PredicateId,
    target: &BindTarget,
    refusals: &mut Vec<Refusal>,
    jurisdiction: Option<&crate::curation::JurisdictionTag>,
) {
    let Some(subject_concept) = concept_for(context, plan, subject) else {
        // Unclassified geometry makes no domain claim, so the kernel does not
        // govern how it is bound (agreement §7).
        return;
    };

    let propositions = registry.propositions_for(&subject_concept, predicate, jurisdiction);
    if propositions.is_empty() {
        // No proposition governs this concept/predicate pair. Nothing is
        // claimed, so nothing is violated; corpus lint surfaces the gap.
        return;
    }

    if propositions
        .iter()
        .any(|proposition| binding_satisfies(registry, context, plan, proposition, target))
    {
        return;
    }

    // Nothing admitted the binding. Refuse against the most specific
    // proposition available, preferring an identity-defining one.
    let violated = propositions
        .iter()
        .find(|proposition| proposition.identity_defining)
        .copied()
        .unwrap_or(propositions[0]);

    refusals.push(build_refusal(
        registry,
        context,
        plan,
        &subject_concept,
        violated,
        target,
    ));
}

fn binding_satisfies(
    registry: &SemanticRegistry,
    context: &impl SemanticContext,
    plan: &SemanticPlan,
    proposition: &AdmissibilityProposition,
    target: &BindTarget,
) -> bool {
    match (&proposition.object, target) {
        (PropositionObject::AnchorKind(required), BindTarget::Anchor(instance)) => {
            if &instance.kind != required {
                return false;
            }
            // The anchor must actually be published by its host. A fabricated
            // instance id must not pass.
            let publisher_concept = concept_for(context, plan, instance.publisher);
            let published = context
                .published_anchors(instance.publisher)
                .iter()
                .any(|candidate| candidate == instance);
            published
                && publisher_concept
                    .is_some_and(|concept| registry.publishes_anchor(&concept, required))
        }
        (PropositionObject::Concept(required), BindTarget::Entity(entity)) => {
            concept_for(context, plan, *entity).is_some_and(|concept| &concept == required)
        }
        // An anchor proposition is not satisfied by a bare entity, and vice
        // versa. This is the shape of the bargeboard defect.
        _ => false,
    }
}

fn build_refusal(
    registry: &SemanticRegistry,
    context: &impl SemanticContext,
    plan: &SemanticPlan,
    subject_concept: &ConceptId,
    violated: &AdmissibilityProposition,
    target: &BindTarget,
) -> Refusal {
    let observed = describe_observed(registry, context, plan, target);
    let repair = match &violated.object {
        PropositionObject::AnchorKind(kind) => {
            let publishers = registry.publishers_of(kind);
            if publishers.is_empty() {
                format!(
                    "{} No registered concept publishes `{kind}` — this is a corpus gap.",
                    violated.repair_hint
                )
            } else {
                format!(
                    "{} `{kind}` is published by [{}].",
                    violated.repair_hint,
                    publishers
                        .iter()
                        .map(ConceptId::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        PropositionObject::Concept(concept) => {
            format!("{} Expected concept `{concept}`.", violated.repair_hint)
        }
    };

    Refusal {
        violated: Some(violated.id.clone()),
        reason: format!(
            "`{subject_concept}` requires {} ({}). {}",
            describe_object(&violated.object),
            describe_cardinality(violated),
            violated.refusal_reason
        ),
        observed,
        repair,
        contrasts: registry
            .concept(subject_concept)
            .map(|concept| concept.contrasts.clone())
            .unwrap_or_default(),
    }
}

fn describe_observed(
    registry: &SemanticRegistry,
    context: &impl SemanticContext,
    plan: &SemanticPlan,
    target: &BindTarget,
) -> String {
    let entity = match target {
        BindTarget::Anchor(instance) => instance.publisher,
        BindTarget::Entity(entity) => *entity,
    };
    let concept = concept_for(context, plan, entity);
    let concept_label = concept
        .as_ref()
        .map(|concept| format!("`{concept}`"))
        .unwrap_or_else(|| "unclassified geometry".to_string());

    let published = concept
        .as_ref()
        .map(|concept| registry.published_by_concept(concept))
        .unwrap_or(&[]);

    if published.is_empty() {
        format!(
            "Entity #{} ({concept_label}) publishes no anchors.",
            entity.0
        )
    } else {
        format!(
            "Entity #{} ({concept_label}) publishes [{}].",
            entity.0,
            published
                .iter()
                .map(AnchorKindId::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Resolve an entity's concept, honouring assignments made earlier in the same
/// plan so "assign then bind" works within one command.
fn concept_for(
    context: &impl SemanticContext,
    plan: &SemanticPlan,
    entity: ElementId,
) -> Option<ConceptId> {
    plan.intents
        .iter()
        .rev()
        .find_map(|intent| match intent {
            PlanIntent::AssignConcept {
                entity: assigned,
                concept,
            } if *assigned == entity => Some(concept.clone()),
            _ => None,
        })
        .or_else(|| context.concept_of(entity))
}

fn describe_object(object: &PropositionObject) -> String {
    match object {
        PropositionObject::AnchorKind(kind) => format!("anchor `{kind}`"),
        PropositionObject::Concept(concept) => format!("concept `{concept}`"),
    }
}

fn describe_cardinality(proposition: &AdmissibilityProposition) -> String {
    let identity = if proposition.identity_defining {
        "identity-defining"
    } else {
        "constraint"
    };
    format!("{identity}, {:?}", proposition.cardinality)
}
