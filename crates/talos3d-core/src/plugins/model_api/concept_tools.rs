//! MCP surface for the Design Concept Graph (ADR-064 / PP-TDR-1).
//!
//! Without these three tools the admissibility kernel is installed but
//! unreachable: nothing assigns a concept, so nothing arms it, and no host
//! publishes an anchor, so there is nothing to resolve against. A cold agent
//! could not exercise the contract at all.
//!
//! Deliberately three tools, not a subsystem:
//!
//! * `resolve_domain_term` — turn the user's word into concept candidates.
//!   Returns **every** match rather than a nearest guess; silently mapping an
//!   ambiguous term to one concept is exactly the failure this layer exists to
//!   prevent.
//! * `assign_concept` — claim a concept for an entity. This is the act that
//!   arms the kernel (ADR-064 §7).
//! * `publish_anchors` — declare the anchor instances a host offers.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plugins::identity::ElementId;
use crate::semantics::{AnchorInstanceId, SemanticContext, WorldSemanticContext};
use crate::semantics::{ConceptId, PlanIntent, SemanticGraph, SemanticPlan};

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveDomainTermRequest {
    /// The word as the user said it, in any supported locale.
    pub term: String,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptMatch {
    pub concept_id: String,
    pub preferred_label: String,
    pub locale: String,
    /// Concepts this one is explicitly *not*, so a caller that picked the wrong
    /// word can correct itself.
    pub contrasts: Vec<String>,
    /// Which building system it belongs to. Narrows anchor discovery; never
    /// implies admissibility on its own.
    pub system_membership: Vec<String>,
    pub applicable_element_classes: Vec<String>,
    pub evidence: Vec<crate::curation::EvidenceRef>,
    /// Declared publication roles and resolver references; these do not prove
    /// a runtime geometry resolver is installed.
    pub publications: Vec<crate::semantics::PublishedAnchorContract>,
    /// Anchor kinds this concept must resolve against, with the concepts that
    /// publish each — the "where does it go" answer.
    pub required_anchors: Vec<RequiredAnchor>,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequiredAnchor {
    pub anchor_kind: String,
    pub predicate: String,
    pub identity_defining: bool,
    pub published_by: Vec<String>,
    pub repair_hint: String,
    pub proposition_id: String,
    pub evidence: Vec<crate::curation::EvidenceRef>,
    pub current_anchors: Vec<AnchorInstanceId>,
    pub omitted_current_anchors: usize,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveDomainTermResult {
    pub term: String,
    /// Every candidate. More than one means genuine ambiguity the caller must
    /// resolve, not a ranking to take the top of.
    pub matches: Vec<ConceptMatch>,
    /// Set when nothing matched, so an empty result is a first-class gap route
    /// rather than a silent empty list.
    pub no_concept_found: Option<String>,
    pub edit_request_kind: String,
    pub next_tools: Vec<String>,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignConceptRequest {
    pub element_id: u64,
    pub concept_id: String,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignConceptResult {
    pub element_id: u64,
    pub concept_id: String,
    /// Anchors this entity must now resolve against, and by when.
    pub obligations: Vec<String>,
    pub note: String,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishAnchorsRequest {
    pub element_id: u64,
    pub anchors: Vec<PublishAnchorSpec>,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishAnchorSpec {
    pub anchor_kind: String,
    /// Role discriminator distinguishing siblings, e.g. `gable_end_a`.
    pub role: String,
}

#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishAnchorsResult {
    pub element_id: u64,
    pub published: Vec<String>,
    pub note: String,
}

pub fn handle_resolve_domain_term(
    world: &World,
    request: ResolveDomainTermRequest,
) -> ResolveDomainTermResult {
    let Some(graph) = world.get_resource::<SemanticGraph>() else {
        return ResolveDomainTermResult {
            term: request.term,
            matches: Vec::new(),
            no_concept_found: Some(
                "No Design Concept Graph is installed in this app composition.".to_string(),
            ),
            edit_request_kind: "core.semantic".into(),
            next_tools: vec!["request_corpus_expansion".into()],
        };
    };

    let context = WorldSemanticContext::new(world);
    let matches: Vec<ConceptMatch> = graph
        .resolve_term(&request.term)
        .into_iter()
        .map(|concept| {
            let required_anchors = graph
                .propositions_of_subject(&concept.id, None)
                .into_iter()
                .filter_map(|proposition| match &proposition.object {
                    crate::semantics::PropositionObject::AnchorKind(kind) => {
                        let mut current = context.anchor_candidates(kind);
                        current.retain(|anchor| {
                            context
                                .concept_of(anchor.publisher)
                                .is_some_and(|concept| graph.publishes_anchor(&concept, kind))
                        });
                        let omitted_current_anchors = current.len().saturating_sub(16);
                        current.truncate(16);
                        Some(RequiredAnchor {
                            anchor_kind: kind.to_string(),
                            predicate: proposition.predicate.to_string(),
                            identity_defining: proposition.identity_defining,
                            published_by: graph
                                .publishers_of(kind)
                                .iter()
                                .map(ToString::to_string)
                                .collect(),
                            repair_hint: proposition.repair_hint.clone(),
                            proposition_id: proposition.id.to_string(),
                            evidence: proposition.evidence.clone(),
                            current_anchors: current,
                            omitted_current_anchors,
                        })
                    }
                    _ => None,
                })
                .collect();
            let lexical = concept.lexical.first();
            ConceptMatch {
                concept_id: concept.id.to_string(),
                preferred_label: lexical
                    .map(|entry| entry.preferred_label.clone())
                    .unwrap_or_default(),
                locale: lexical
                    .map(|entry| entry.locale.clone())
                    .unwrap_or_default(),
                contrasts: concept.contrasts.iter().map(ToString::to_string).collect(),
                system_membership: concept
                    .system_membership
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                applicable_element_classes: concept
                    .applicable_element_classes
                    .iter()
                    .map(|class| class.0.clone())
                    .collect(),
                required_anchors,
                evidence: concept.evidence.clone(),
                publications: graph
                    .publications()
                    .iter()
                    .filter(|contract| contract.publisher == concept.id)
                    .cloned()
                    .collect(),
            }
        })
        .collect();

    let no_concept_found = matches.is_empty().then(|| {
        format!(
            "No concept matches '{}'. This is a CorpusGap: call request_corpus_expansion rather \
             than assigning an approximate concept or hand-rolling geometry.",
            request.term
        )
    });

    ResolveDomainTermResult {
        term: request.term,
        matches,
        no_concept_found,
        edit_request_kind: "core.semantic".into(),
        next_tools: vec![
            "get_entity_details".into(),
            "list_edit_requests".into(),
            "preview_edit_plan".into(),
        ],
    }
}

pub fn handle_assign_concept(
    world: &mut World,
    request: AssignConceptRequest,
) -> Result<AssignConceptResult, String> {
    let concept = ConceptId::new(request.concept_id.clone());

    let obligations = {
        let graph = world
            .get_resource::<SemanticGraph>()
            .ok_or_else(|| "No Design Concept Graph is installed.".to_string())?;
        if graph.concept(&concept).is_none() {
            return Err(format!(
                "Unknown concept '{concept}'. Call resolve_domain_term first; do not invent a \
                 concept id. If the concept is genuinely missing, that is a CorpusGap."
            ));
        }
        graph
            .propositions_of_subject(&concept, None)
            .into_iter()
            .filter_map(|proposition| {
                let crate::semantics::PropositionObject::AnchorKind(kind) = &proposition.object
                else {
                    return None;
                };
                Some(format!(
                    "{} must {} anchor `{kind}`{}",
                    concept,
                    proposition.predicate,
                    proposition
                        .required_by
                        .map(|state| format!(" by {}", state.as_str()))
                        .unwrap_or_default()
                ))
            })
            .collect::<Vec<_>>()
    };

    apply_semantic_request(
        world,
        SemanticPlan::none().with(PlanIntent::AssignConcept {
            entity: ElementId(request.element_id),
            concept: concept.clone(),
        }),
    )?;

    Ok(AssignConceptResult {
        element_id: request.element_id,
        concept_id: concept.to_string(),
        obligations,
        note: "Claiming a concept arms the admissibility kernel for this entity. Removing it is \
               an explicit semantic downgrade, not a way around a refusal."
            .to_string(),
    })
}

pub fn handle_publish_anchors(
    world: &mut World,
    request: PublishAnchorsRequest,
) -> Result<PublishAnchorsResult, String> {
    {
        let graph = world
            .get_resource::<SemanticGraph>()
            .ok_or_else(|| "No Design Concept Graph is installed.".to_string())?;
        for spec in &request.anchors {
            let kind = crate::semantics::AnchorKindId::new(spec.anchor_kind.clone());
            if graph.anchor_kind(&kind).is_none() {
                return Err(format!(
                    "Unknown anchor kind '{kind}'. Anchor kinds are public API; call \
                     resolve_domain_term to see which anchors a concept requires."
                ));
            }
        }
    }

    let published = request
        .anchors
        .iter()
        .map(|spec| format!("{}/{}", spec.anchor_kind, spec.role))
        .collect();
    let anchors = request
        .anchors
        .into_iter()
        .map(|spec| crate::semantics::plan::AnchorPublication {
            kind: spec.anchor_kind.as_str().into(),
            role: spec.role.as_str().into(),
        })
        .collect();
    apply_semantic_request(
        world,
        SemanticPlan::none().with(PlanIntent::PublishAnchors {
            entity: ElementId(request.element_id),
            anchors,
        }),
    )?;

    Ok(PublishAnchorsResult {
        element_id: request.element_id,
        published,
        note: "Other entities may now resolve against these anchors. Identity is \
               publisher+kind+role and excludes coordinates, so regenerating this host \
               invalidates dependents rather than detaching them."
            .to_string(),
    })
}

/// Compatibility tools use the same retained candidate and one history item.
fn apply_semantic_request(world: &mut World, semantic: SemanticPlan) -> Result<(), String> {
    let plan = crate::plugins::authored_edit_plan::requests::preview(
        world,
        "core.semantic",
        serde_json::to_value(semantic).map_err(|error| error.to_string())?,
    )?;
    super::apply_captured_plan_and_flush(world, &plan)
}
