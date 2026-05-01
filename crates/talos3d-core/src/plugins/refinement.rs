//! Proof Point 70 — Refinement Substrate.
//!
//! This module provides the generic machinery for tracking the maturity of
//! authored entities: refinement state, obligation sets, authoring provenance,
//! and per-claim grounding. No domain-specific nouns live here — the
//! components are intended to be used by any domain capability (architecture,
//! naval, terrain, etc.) as established in ADR-037.
//!
//! Two `SemanticRelation` types are also registered here:
//! - `"refinement_of"` — child → parent; records that an entity was promoted
//!   from a coarser stub.
//! - `"refined_into"` — parent → child; inverse direction, built by the
//!   promote/demote commands to keep the original stub addressable.
//!
//! The `DeclaredStateRequiresResolvedObligations` validator is a plain
//! function wired into `run_validation`. A richer scheduling engine with Bevy
//! change detection and caching will land in PP74.

use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

// `Messages` is re-exported by bevy::prelude in Bevy 0.18.
// `Message` derive macro, `BeginCommandGroup`, etc. are imported via the
// crate-local module paths below.

// ---------------------------------------------------------------------------
// Newtype ID wrappers (placeholder — real types come in PP74/75/77)
// ---------------------------------------------------------------------------

/// Opaque identity for an obligation within an `ObligationSet`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ObligationId(pub String);

/// Semantic role of an obligation (e.g. `"primary_structure"`, `"envelope"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SemanticRole(pub String);

impl SemanticRole {
    /// Returns `true` when this role designates a primary structural element.
    /// The starter validator uses this to escalate findings to `warning`.
    pub fn is_primary_structure(&self) -> bool {
        self.0 == "primary_structure"
    }
}

/// A slash-delimited path into a claim record
/// (e.g. `"riser_max_mm"`, `"envelope/exterior_finish"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ClaimPath(pub String);

/// References a rule in a rule pack.  Stubbed as `String` for PP70.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RuleId(pub String);

/// References a row in a structured catalog.  Stubbed as `String` for PP70.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CatalogRowId(pub String);

/// A tag on an LLM heuristic for traceability.  Stubbed as `String` for PP70.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct HeuristicTag(pub String);

/// Identifies the agent that set a claim.  Stubbed as `String` for PP70.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AgentId(pub String);

/// References a source document such as an imported file or a standard.
/// Stubbed as `String` for PP70.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SourceRef(pub String);

/// References a passage (section, article, paragraph) in a corpus document.
/// Stored as a slash-delimited path (e.g. `"BBR/9:2/table-1"`).
/// Stubbed as `String` for PP70; the corpus infrastructure lands in PP78.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PassageRef(pub String);

/// Identifies a parametric recipe family.  Stubbed as `String` for PP70.
/// Real recipe families arrive in PP71.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RecipeId(pub String);

// ---------------------------------------------------------------------------
// RefinementState
// ---------------------------------------------------------------------------

/// The declared maturity level of an authored entity.
///
/// Variants are ordered from least to most detailed; `PartialOrd` / `Ord`
/// implementations let the validator compare states numerically.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum RefinementState {
    /// Coarse design intent — no detail obligations in force.
    #[default]
    Conceptual,
    /// Spatial massing defined; primary-structure obligations begin firing.
    Schematic,
    /// Buildable geometry — all obligations must be resolved.
    Constructible,
    /// Full assembly detail present.
    Detailed,
    /// Ready for fabrication or manufacturing output.
    FabricationReady,
}

impl RefinementState {
    /// Human-readable label used in MCP responses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conceptual => "Conceptual",
            Self::Schematic => "Schematic",
            Self::Constructible => "Constructible",
            Self::Detailed => "Detailed",
            Self::FabricationReady => "FabricationReady",
        }
    }

    /// Parse from the string produced by `as_str`.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Conceptual" => Some(Self::Conceptual),
            "Schematic" => Some(Self::Schematic),
            "Constructible" => Some(Self::Constructible),
            "Detailed" => Some(Self::Detailed),
            "FabricationReady" => Some(Self::FabricationReady),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// RefinementStateComponent
// ---------------------------------------------------------------------------

/// ECS component that holds the declared refinement level for an entity.
#[derive(Component, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RefinementStateComponent {
    pub state: RefinementState,
}

// ---------------------------------------------------------------------------
// ObligationSet
// ---------------------------------------------------------------------------

/// The status of a single obligation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum ObligationStatus {
    /// Not yet resolved.
    Unresolved,
    /// Satisfied by the entity with the given `ElementId` (stored as `u64`
    /// to keep this module free of direct `ElementId` imports).
    SatisfiedBy(u64),
    /// Intentionally deferred with a human-readable reason.
    Deferred(String),
    /// Waived with a human-readable rationale.
    Waived(String),
}

/// A single obligation entry in an `ObligationSet`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct Obligation {
    /// Unique identifier within the set.
    pub id: ObligationId,
    /// Semantic role this obligation covers.
    pub role: SemanticRole,
    /// The lowest `RefinementState` at which this obligation must be satisfied.
    pub required_by_state: RefinementState,
    /// Current status.
    pub status: ObligationStatus,
}

/// ECS component holding the declared obligation list for an entity.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ObligationSet {
    pub entries: Vec<Obligation>,
}

// ---------------------------------------------------------------------------
// AuthoringProvenance
// ---------------------------------------------------------------------------

/// Records how an entity was created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum AuthoringMode {
    /// Entity was generated by a registered recipe family.
    ViaRecipe(RecipeId),
    /// Entity was authored by hand without a recipe.
    Freeform,
    /// Entity was imported from an external file.
    Imported(SourceRef),
    /// Entity was promoted from a coarser entity (given as `u64` element-id).
    Refined(u64),
}

/// ECS component that records *why* an entity was placed.
///
/// Orthogonal to [`ClaimGrounding`]: provenance answers "why does this entity
/// exist?"; grounding answers "why does this property have this value?".
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthoringProvenance {
    pub mode: AuthoringMode,
    pub rationale: Option<String>,
}

impl Default for AuthoringProvenance {
    fn default() -> Self {
        Self {
            mode: AuthoringMode::Freeform,
            rationale: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ClaimGrounding
// ---------------------------------------------------------------------------

/// Describes the provenance of a single typed property value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum Grounding {
    /// Grounded by an explicit rule in a rule pack.
    ExplicitRule(RuleId),
    /// Grounded by a row in a structured catalog.
    CatalogRow(CatalogRowId),
    /// Copied verbatim from an imported file.
    Imported(SourceRef),
    /// Inherited from a parent entity during refinement (element-id as `u64`).
    Refined(u64),
    /// Generated locally by a recipe's `generate` function (PP74 / F6).
    ///
    /// Use this when the claim is computed by the recipe itself (e.g. a slab
    /// computing its own `top_datum_mm` from `floor_datum_mm`). Stored as the
    /// recipe family id string to avoid a circular dependency between
    /// `refinement` and `capability_registry`.
    ///
    /// The `RecipeFamilyId` newtype in `capability_registry` converts to/from
    /// this via `RecipeFamilyId(String)`.
    GeneratedByRecipe(String),
    /// Derived from LLM implicit knowledge; requires rationale + tag.
    LLMHeuristic {
        rationale: String,
        heuristic_tag: HeuristicTag,
    },
}

/// A single grounded claim record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ClaimRecord {
    pub grounding: Grounding,
    /// Wall-clock timestamp (Unix seconds) at which the claim was set.
    pub set_at: i64,
    /// Agent that set the claim, if known.
    pub set_by: Option<AgentId>,
}

/// ECS component holding per-claim grounding for an entity.
///
/// Keys are [`ClaimPath`] values (e.g. `"riser_max_mm"`,
/// `"envelope/exterior_finish"`).
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ClaimGrounding {
    pub claims: HashMap<ClaimPath, ClaimRecord>,
}

// ---------------------------------------------------------------------------
// Validation findings
// ---------------------------------------------------------------------------

/// Severity level for a validator finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum FindingSeverity {
    Advice,
    Warning,
    Error,
}

impl FindingSeverity {
    /// Human-readable label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advice => "advice",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// A single finding emitted by a validator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ValidationFinding {
    /// Unique finding identifier (format: `"{validator_id}:{entity_id}:{obligation_id}"`).
    pub finding_id: String,
    /// Element-id of the entity this finding applies to.
    pub entity_element_id: u64,
    /// Validator that produced this finding.
    pub validator: String,
    pub severity: FindingSeverity,
    /// Human-readable message.
    pub message: String,
    /// Rationale for why this rule exists.
    pub rationale: String,
    /// The obligation id that triggered this finding, if applicable.
    pub obligation_id: Option<ObligationId>,
}

// ---------------------------------------------------------------------------
// Validator: DeclaredStateRequiresResolvedObligations
// ---------------------------------------------------------------------------

/// The PP70 starter completeness validator.
///
/// Rules (from ADR-038 §11 and the PP70 spec):
///
/// * `Conceptual` — never emits any finding.
/// * `Schematic` — for each `Unresolved` obligation whose
///   `required_by_state ≤ Schematic`:
///   * `SemanticRole::PrimaryStructure` → `warning`
///   * otherwise → `advice`
/// * `Constructible | Detailed | FabricationReady` — any `Unresolved`
///   obligation whose `required_by_state ≤ entity.state` → `error`.
///   (`SatisfiedBy | Deferred | Waived` all pass.)
pub fn validate_declared_state_obligations(
    entity_element_id: u64,
    state: RefinementState,
    obligations: &ObligationSet,
) -> Vec<ValidationFinding> {
    if state == RefinementState::Conceptual {
        return Vec::new();
    }

    let mut findings = Vec::new();

    for obligation in &obligations.entries {
        // Only obligations whose trigger threshold has been reached.
        if obligation.required_by_state > state {
            continue;
        }

        if !matches!(obligation.status, ObligationStatus::Unresolved) {
            continue;
        }

        let (severity, rationale) = match state {
            RefinementState::Conceptual => unreachable!("handled above"),
            RefinementState::Schematic => {
                if obligation.role.is_primary_structure() {
                    (
                        FindingSeverity::Warning,
                        "Primary-structure obligations must be resolved by the Schematic state \
                         to ensure structural intent is captured before further detail work begins.",
                    )
                } else {
                    (
                        FindingSeverity::Advice,
                        "This obligation is expected by the Schematic state. Consider resolving \
                         it before advancing further.",
                    )
                }
            }
            RefinementState::Constructible
            | RefinementState::Detailed
            | RefinementState::FabricationReady => (
                FindingSeverity::Error,
                "All obligations must be resolved at or above the Constructible state. \
                 Use SatisfiedBy, Deferred(reason), or Waived(rationale) to close this obligation.",
            ),
        };

        findings.push(ValidationFinding {
            finding_id: format!(
                "declared_state_obligations:{}:{}",
                entity_element_id, obligation.id.0
            ),
            entity_element_id,
            validator: "DeclaredStateRequiresResolvedObligations".to_string(),
            severity,
            message: format!(
                "Obligation '{}' (role: '{}', required by: {}) is Unresolved",
                obligation.id.0,
                obligation.role.0,
                obligation.required_by_state.as_str()
            ),
            rationale: rationale.to_string(),
            obligation_id: Some(obligation.id.clone()),
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// Promote / Demote operations (world-mutating, called from model_api handlers)
// ---------------------------------------------------------------------------

use crate::plugins::{
    commands::{ApplyEntityChangesCommand, BeginCommandGroup, EndCommandGroup},
    identity::ElementId,
    modeling::assembly::{RelationSnapshot, SemanticRelation},
};

/// A promote-refinement command payload.
///
/// In PP70, `recipe_id` and `overrides` are accepted but not acted upon
/// (no recipe families exist yet). The command updates `RefinementStateComponent`
/// and records authoring provenance.
#[derive(Debug, Clone)]
pub struct PromoteRefinementRequest {
    pub entity_element_id: u64,
    pub target_state: RefinementState,
    pub recipe_id: Option<RecipeId>,
    pub overrides: HashMap<ClaimPath, serde_json::Value>,
}

/// A demote-refinement command payload.
#[derive(Debug, Clone)]
pub struct DemoteRefinementRequest {
    pub entity_element_id: u64,
    pub target_state: RefinementState,
}

/// Apply a promotion to the world, queuing history commands for undo/redo.
///
/// Returns the new state on success.
///
/// If the entity has an `ElementClassAssignment` component, the effective
/// merged `RefinementContract` (class-minimum + recipe specialisations) is
/// computed from the registered descriptors and installed as an `ObligationSet`.
/// If a `recipe_id` is provided, the recipe's `generate` function is invoked
/// to spawn child entities and link them via `create_refinement_relation_pair`.
pub fn apply_promote_refinement(
    world: &mut World,
    request: PromoteRefinementRequest,
) -> Result<RefinementState, String> {
    use crate::capability_registry::{
        effective_obligations, effective_promotion_critical_paths, ElementClassAssignment,
        GenerateInput, RecipeFamilyId,
    };

    let eid = ElementId(request.entity_element_id);

    // Locate the entity.
    let entity = {
        let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
        q.iter(world)
            .find(|(_, id)| **id == eid)
            .map(|(entity, _)| entity)
            .ok_or_else(|| format!("Entity {} not found", request.entity_element_id))?
    };

    // Validate state direction.
    let current_state = world
        .get::<RefinementStateComponent>(entity)
        .map(|c| c.state)
        .unwrap_or_default();

    if request.target_state <= current_state {
        return Err(format!(
            "Cannot promote from {} to {} — target must be higher",
            current_state.as_str(),
            request.target_state.as_str()
        ));
    }

    let before_state = current_state;
    let target_state = request.target_state;

    // Read class/recipe assignment from the entity (if any).
    let class_assignment: Option<ElementClassAssignment> =
        world.get::<ElementClassAssignment>(entity).cloned();

    // Determine the effective recipe id: request overrides the component.
    let effective_recipe_id: Option<RecipeFamilyId> = request
        .recipe_id
        .as_ref()
        .map(|r| RecipeFamilyId(r.0.clone()))
        .or_else(|| {
            class_assignment
                .as_ref()
                .and_then(|a| a.active_recipe.clone())
        });

    // Lookup descriptors from the registry (requires CapabilityRegistry resource).
    let (obligation_templates, _critical_paths) = {
        if let Some(ref assignment) = class_assignment {
            let registry = world.get_resource::<crate::capability_registry::CapabilityRegistry>();
            if let Some(registry) = registry {
                let class_desc = registry.element_class_descriptor(&assignment.element_class);
                let recipe_desc = effective_recipe_id
                    .as_ref()
                    .and_then(|rid| registry.recipe_family_descriptor(rid));

                let templates = class_desc.map_or_else(Vec::new, |cd| {
                    effective_obligations(cd, recipe_desc, target_state)
                });
                let paths = class_desc.map_or_else(Vec::new, |cd| {
                    effective_promotion_critical_paths(cd, recipe_desc, target_state)
                });
                (templates, paths)
            } else {
                (Vec::new(), Vec::new())
            }
        } else {
            (Vec::new(), Vec::new())
        }
    };

    // Build the initial ObligationSet from templates (status = Unresolved).
    let initial_obligations: Vec<crate::plugins::refinement::Obligation> = obligation_templates
        .into_iter()
        .map(|t| crate::plugins::refinement::Obligation {
            id: t.id,
            role: t.role,
            required_by_state: t.required_by_state,
            status: ObligationStatus::Unresolved,
        })
        .collect();

    // Invoke the recipe's `generate` function if we have one.
    // Returns (obligation_id, child_element_id) satisfaction links and grounding updates.
    let (satisfaction_links, grounding_updates): (
        Vec<(ObligationId, u64)>,
        HashMap<ClaimPath, crate::plugins::refinement::ClaimRecord>,
    ) = if let Some(ref recipe_id) = effective_recipe_id {
        // Build parameter map from overrides (request.overrides).
        let parameters: std::collections::HashMap<String, serde_json::Value> = request
            .overrides
            .iter()
            .map(|(k, v)| (k.0.clone(), v.clone()))
            .collect();

        let input = GenerateInput {
            element_id: request.entity_element_id,
            target_state,
            parameters,
        };

        // Clone the GenerateFn Arc to avoid holding the registry borrow.
        let generate_fn = {
            let registry = world.get_resource::<crate::capability_registry::CapabilityRegistry>();
            registry
                .and_then(|r| r.recipe_family_descriptor(recipe_id))
                .map(|d| d.generate.clone())
        };

        if let Some(generate_fn) = generate_fn {
            match generate_fn(input, world) {
                Ok(output) => (output.satisfaction_links, output.grounding_updates),
                Err(e) => return Err(format!("Recipe generate failed: {e}")),
            }
        } else {
            (Vec::new(), HashMap::new())
        }
    } else {
        (Vec::new(), HashMap::new())
    };

    // Re-locate entity after possible world mutations from generate.
    let entity = {
        let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
        q.iter(world)
            .find(|(_, id)| **id == eid)
            .map(|(entity, _)| entity)
            .ok_or_else(|| {
                format!(
                    "Entity {} not found after generate",
                    request.entity_element_id
                )
            })?
    };

    // Apply satisfaction links to the initial obligation set.
    let mut obligations = initial_obligations;
    for (obligation_id, child_eid) in &satisfaction_links {
        if let Some(ob) = obligations.iter_mut().find(|o| &o.id == obligation_id) {
            ob.status = ObligationStatus::SatisfiedBy(*child_eid);
        }
    }

    // -----------------------------------------------------------------------
    // Cross-entity bears_on obligation resolution (PP72).
    //
    // For any obligation with id "bears_on" that is still Unresolved, search
    // for a SemanticRelation with relation_type == "bears_on" whose source is
    // this entity. If found, check whether the target is at Constructible or
    // higher AND has a top_datum_mm ClaimGrounding entry. If so, mark the
    // obligation SatisfiedBy the target entity and add a claim grounding entry
    // on this entity for "bears_on".
    //
    // This logic is intentionally generic — it applies to any class with a
    // bears_on obligation, not just walls.
    // TODO(PP76): replace the target-readiness check with a GenerationPriorDescriptor query.
    {
        // Collect bears_on SemanticRelation targets for this entity.
        let bears_on_targets: Vec<ElementId> = {
            let mut q = world.try_query::<(EntityRef,)>().unwrap();
            q.iter(world)
                .filter_map(|(entity_ref,)| {
                    let rel = entity_ref.get::<SemanticRelation>()?;
                    if rel.relation_type == "bears_on" && rel.source == eid {
                        Some(rel.target)
                    } else {
                        None
                    }
                })
                .collect()
        };

        for target_eid in bears_on_targets {
            // Check target readiness: must be Constructible+ and have top_datum_mm claim.
            let target_ready = {
                let mut q = world.try_query::<(EntityRef,)>().unwrap();
                q.iter(world).any(|(entity_ref,)| {
                    if entity_ref.get::<ElementId>().copied() != Some(target_eid) {
                        return false;
                    }
                    let state = entity_ref
                        .get::<RefinementStateComponent>()
                        .map(|c| c.state)
                        .unwrap_or_default();
                    let has_claim = entity_ref.get::<ClaimGrounding>().is_some_and(|cg| {
                        cg.claims.contains_key(&ClaimPath("top_datum_mm".into()))
                    });
                    state >= RefinementState::Constructible && has_claim
                })
            };

            if target_ready {
                // Satisfy the bears_on obligation with the target entity id.
                if let Some(ob) = obligations.iter_mut().find(|o| o.id.0 == "bears_on") {
                    if ob.status == ObligationStatus::Unresolved {
                        ob.status = ObligationStatus::SatisfiedBy(target_eid.0);
                    }
                }
            }
        }
    }

    // Install ObligationSet on the entity (replace any existing one).
    if !obligations.is_empty() {
        world.entity_mut(entity).insert(ObligationSet {
            entries: obligations,
        });
    }

    // Apply grounding updates from the generate output.
    // Updates are merged into any existing ClaimGrounding component (or a new one).
    if !grounding_updates.is_empty() {
        if let Some(mut cg) = world.get_mut::<ClaimGrounding>(entity) {
            for (path, record) in grounding_updates {
                cg.claims.insert(path, record);
            }
        } else {
            world.entity_mut(entity).insert(ClaimGrounding {
                claims: grounding_updates,
            });
        }
    }

    // Update the ElementClassAssignment to record the active recipe.
    if let Some(mut assignment) = world.get_mut::<ElementClassAssignment>(entity) {
        if effective_recipe_id.is_some() {
            assignment.active_recipe = effective_recipe_id.clone();
        }
    }

    // Update AuthoringProvenance.
    if let Some(mut prov) = world.get_mut::<AuthoringProvenance>(entity) {
        if matches!(prov.mode, AuthoringMode::Freeform) {
            prov.mode = AuthoringMode::Refined(request.entity_element_id);
        }
    }

    // Mutate refinement state.
    if let Some(mut comp) = world.get_mut::<RefinementStateComponent>(entity) {
        comp.state = target_state;
    } else {
        world.entity_mut(entity).insert(RefinementStateComponent {
            state: target_state,
        });
    }

    // Queue a history entry so the promotion is undoable.
    let before_snapshot = RefinementStateSnapshot {
        element_id: eid,
        state: before_state,
    };
    let after_snapshot = RefinementStateSnapshot {
        element_id: eid,
        state: target_state,
    };

    send_refinement_change_command(world, "Promote Refinement", before_snapshot, after_snapshot);

    Ok(target_state)
}

/// Apply a demotion to the world, queuing history commands for undo/redo.
///
/// Removes `refined_into` links whose child was generated by a recipe (stub
/// in PP70: exercises the code path with any matching `refinement_of` relation).
pub fn apply_demote_refinement(
    world: &mut World,
    request: DemoteRefinementRequest,
) -> Result<RefinementState, String> {
    let eid = ElementId(request.entity_element_id);

    let entity = {
        let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
        q.iter(world)
            .find(|(_, id)| **id == eid)
            .map(|(entity, _)| entity)
            .ok_or_else(|| format!("Entity {} not found", request.entity_element_id))?
    };

    let current_state = world
        .get::<RefinementStateComponent>(entity)
        .map(|c| c.state)
        .unwrap_or_default();

    if request.target_state >= current_state {
        return Err(format!(
            "Cannot demote from {} to {} — target must be lower",
            current_state.as_str(),
            request.target_state.as_str()
        ));
    }

    let target_state = request.target_state;

    // Collect `refined_into` relations whose parent is this entity so we
    // can remove them. In PP70 we remove all of them (no recipe-generated
    // flag yet; PP71 will add that distinction).
    let refined_into_relation_ids: Vec<ElementId> = {
        let mut q = world.try_query::<(EntityRef,)>().unwrap();
        q.iter(world)
            .filter_map(|(entity_ref,)| {
                let rel = entity_ref.get::<SemanticRelation>()?;
                let id = *entity_ref.get::<ElementId>()?;
                if rel.relation_type == "refined_into" && rel.source == eid {
                    Some(id)
                } else {
                    None
                }
            })
            .collect()
    };

    // Mutate state.
    if let Some(mut comp) = world.get_mut::<RefinementStateComponent>(entity) {
        comp.state = target_state;
    } else {
        world.entity_mut(entity).insert(RefinementStateComponent {
            state: target_state,
        });
    }

    // Despawn `refined_into` relation entities.
    for rel_id in &refined_into_relation_ids {
        let rel_entity = {
            let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
            q.iter(world)
                .find(|(_, id)| *id == rel_id)
                .map(|(entity, _)| entity)
        };
        if let Some(rel_entity) = rel_entity {
            world.despawn(rel_entity);
        }
    }

    // Queue undo/redo history.
    let before_snapshot = RefinementStateSnapshot {
        element_id: eid,
        state: current_state,
    };
    let after_snapshot = RefinementStateSnapshot {
        element_id: eid,
        state: target_state,
    };
    send_refinement_change_command(world, "Demote Refinement", before_snapshot, after_snapshot);

    Ok(target_state)
}

// ---------------------------------------------------------------------------
// Internal: lightweight undo/redo snapshot for refinement state changes
// ---------------------------------------------------------------------------

/// A minimal snapshot that records just the refinement state of an entity.
/// Used as the before/after pair in `ApplyEntityChangesCommand`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RefinementStateSnapshot {
    element_id: ElementId,
    state: RefinementState,
}

use crate::authored_entity::{
    AuthoredEntity, BoxedEntity, EntityBounds, HandleInfo, PropertyFieldDef,
};
use std::any::Any;

impl AuthoredEntity for RefinementStateSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "refinement_state_snapshot"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!("refinement:{}", self.element_id.0)
    }

    fn center(&self) -> Vec3 {
        Vec3::ZERO
    }

    fn bounds(&self) -> Option<EntityBounds> {
        None
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        Vec::new()
    }

    fn set_property_json(
        &self,
        _name: &str,
        _value: &serde_json::Value,
    ) -> Result<BoxedEntity, String> {
        Err("RefinementStateSnapshot is read-only".to_string())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        Vec::new()
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        // Find the entity and update its RefinementStateComponent.
        let entity = {
            let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
            q.iter(world)
                .find(|(_, id)| **id == self.element_id)
                .map(|(entity, _)| entity)
        };
        if let Some(entity) = entity {
            if let Some(mut comp) = world.get_mut::<RefinementStateComponent>(entity) {
                comp.state = self.state;
            } else {
                world
                    .entity_mut(entity)
                    .insert(RefinementStateComponent { state: self.state });
            }
        }
    }

    fn remove_from(&self, _world: &mut World) {
        // Removing a refinement snapshot means "undo" the state component
        // — handled by apply_to with the before snapshot; nothing to despawn.
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {}

    fn box_clone(&self) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }
}

impl From<RefinementStateSnapshot> for BoxedEntity {
    fn from(s: RefinementStateSnapshot) -> Self {
        BoxedEntity(Box::new(s))
    }
}

// ---------------------------------------------------------------------------
// Helper: send Messages resources for command pipeline
// ---------------------------------------------------------------------------

fn send_refinement_change_command(
    world: &mut World,
    label: &'static str,
    before: RefinementStateSnapshot,
    after: RefinementStateSnapshot,
) {
    // Use ApplyEntityChangesCommand to get free undo/redo via the existing pipeline.
    world
        .resource_mut::<Messages<BeginCommandGroup>>()
        .write(BeginCommandGroup { label });
    world
        .resource_mut::<Messages<ApplyEntityChangesCommand>>()
        .write(ApplyEntityChangesCommand {
            label,
            before: vec![before.into()],
            after: vec![after.into()],
        });
    world
        .resource_mut::<Messages<EndCommandGroup>>()
        .write(EndCommandGroup);
}

// ---------------------------------------------------------------------------
// Registration helper (called from ModelingPlugin or a RefinementPlugin)
// ---------------------------------------------------------------------------

use crate::capability_registry::{CapabilityRegistryAppExt, RelationTypeDescriptor};

/// Register the `refinement_of` and `refined_into` relation types.
///
/// Call this from an `App::add_systems(Startup, ...)` or from a plugin's
/// `build` method.
pub fn register_refinement_relations(app: &mut App) {
    app.register_relation_type(RelationTypeDescriptor {
        relation_type: "refinement_of".to_string(),
        label: "Refinement Of".to_string(),
        description: "Directed child → parent link that preserves the coarse stub's \
                      identity when an entity is promoted to a more detailed form. \
                      Cardinality: each child has exactly one parent."
            .to_string(),
        valid_source_types: Vec::new(), // any entity type
        valid_target_types: Vec::new(), // any entity type
        parameter_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "promoted_from_state": { "type": "string" }
            }
        }),
        participates_in_dependency_graph: false,
        external_classification: None,
        host_contract_kind: None,
    });

    app.register_relation_type(RelationTypeDescriptor {
        relation_type: "refined_into".to_string(),
        label: "Refined Into".to_string(),
        description: "Directed parent → child link, inverse of `refinement_of`. \
                      A parent entity may have multiple `refined_into` children \
                      if it was promoted in stages."
            .to_string(),
        valid_source_types: Vec::new(),
        valid_target_types: Vec::new(),
        parameter_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "target_state": { "type": "string" }
            }
        }),
        participates_in_dependency_graph: false,
        external_classification: None,
        host_contract_kind: None,
    });
}

// ---------------------------------------------------------------------------
// Refinement relation helpers (used by handlers and tests)
// ---------------------------------------------------------------------------

use crate::plugins::identity::ElementIdAllocator;

/// Create a `refinement_of(child → parent)` and a matching
/// `refined_into(parent → child)` relation pair, returning both element-ids.
///
/// Caller is responsible for queuing the spawns via the command pipeline.
pub fn create_refinement_relation_pair(
    world: &mut World,
    parent_eid: ElementId,
    child_eid: ElementId,
    promoted_from_state: RefinementState,
    target_state: RefinementState,
) -> (ElementId, ElementId) {
    let fwd_id = world.resource::<ElementIdAllocator>().next_id();
    let inv_id = world.resource::<ElementIdAllocator>().next_id();

    let fwd = RelationSnapshot {
        element_id: fwd_id,
        relation: SemanticRelation {
            source: child_eid,
            target: parent_eid,
            relation_type: "refinement_of".to_string(),
            parameters: serde_json::json!({
                "promoted_from_state": promoted_from_state.as_str()
            }),
        },
    };
    let inv = RelationSnapshot {
        element_id: inv_id,
        relation: SemanticRelation {
            source: parent_eid,
            target: child_eid,
            relation_type: "refined_into".to_string(),
            parameters: serde_json::json!({
                "target_state": target_state.as_str()
            }),
        },
    };

    fwd.apply_to(world);
    inv.apply_to(world);

    (fwd_id, inv_id)
}

/// Query all `refinement_of` child element-ids for a given parent.
pub fn query_refined_into(world: &World, parent_eid: ElementId) -> Vec<ElementId> {
    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    q.iter(world)
        .filter_map(|(entity_ref,)| {
            let rel = entity_ref.get::<SemanticRelation>()?;
            if rel.relation_type == "refined_into" && rel.source == parent_eid {
                Some(rel.target)
            } else {
                None
            }
        })
        .collect()
}

/// Query the parent element-id for a given child via `refinement_of`.
pub fn query_refinement_of(world: &World, child_eid: ElementId) -> Option<ElementId> {
    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    q.iter(world).find_map(|(entity_ref,)| {
        let rel = entity_ref.get::<SemanticRelation>()?;
        if rel.relation_type == "refinement_of" && rel.source == child_eid {
            Some(rel.target)
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- RefinementState ordering & serialization ---

    #[test]
    fn refinement_state_ordering_is_correct() {
        assert!(RefinementState::Conceptual < RefinementState::Schematic);
        assert!(RefinementState::Schematic < RefinementState::Constructible);
        assert!(RefinementState::Constructible < RefinementState::Detailed);
        assert!(RefinementState::Detailed < RefinementState::FabricationReady);
    }

    #[test]
    fn refinement_state_default_is_conceptual() {
        assert_eq!(RefinementState::default(), RefinementState::Conceptual);
    }

    #[test]
    fn refinement_state_round_trips_through_json() {
        for state in [
            RefinementState::Conceptual,
            RefinementState::Schematic,
            RefinementState::Constructible,
            RefinementState::Detailed,
            RefinementState::FabricationReady,
        ] {
            let json = serde_json::to_value(state).expect("serialize");
            let back: RefinementState = serde_json::from_value(json).expect("deserialize");
            assert_eq!(back, state);
        }
    }

    #[test]
    fn refinement_state_from_str_round_trip() {
        for state in [
            RefinementState::Conceptual,
            RefinementState::Schematic,
            RefinementState::Constructible,
            RefinementState::Detailed,
            RefinementState::FabricationReady,
        ] {
            assert_eq!(RefinementState::from_str(state.as_str()), Some(state));
        }
        assert_eq!(RefinementState::from_str("Unknown"), None);
    }

    // --- ObligationSet round-trip through JSON ---

    #[test]
    fn obligation_set_round_trips_through_json() {
        let set = ObligationSet {
            entries: vec![
                Obligation {
                    id: ObligationId("structural_layer".to_string()),
                    role: SemanticRole("primary_structure".to_string()),
                    required_by_state: RefinementState::Schematic,
                    status: ObligationStatus::Unresolved,
                },
                Obligation {
                    id: ObligationId("exterior_cladding".to_string()),
                    role: SemanticRole("envelope".to_string()),
                    required_by_state: RefinementState::Constructible,
                    status: ObligationStatus::SatisfiedBy(42),
                },
                Obligation {
                    id: ObligationId("insulation".to_string()),
                    role: SemanticRole("thermal".to_string()),
                    required_by_state: RefinementState::Constructible,
                    status: ObligationStatus::Deferred("awaiting thermal spec".to_string()),
                },
            ],
        };
        let json = serde_json::to_value(&set).expect("serialize");
        let back: ObligationSet = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, set);
    }

    // --- Validator: Conceptual → no findings ---

    #[test]
    fn validator_conceptual_emits_no_findings() {
        let obligations = ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("structure".to_string()),
                role: SemanticRole("primary_structure".to_string()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::Unresolved,
            }],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Conceptual, &obligations);
        assert!(findings.is_empty(), "Conceptual must never emit findings");
    }

    // --- Validator: Schematic severity rules ---

    #[test]
    fn validator_schematic_primary_structure_is_warning() {
        let obligations = ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("structure".to_string()),
                role: SemanticRole("primary_structure".to_string()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::Unresolved,
            }],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Schematic, &obligations);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, FindingSeverity::Warning);
    }

    #[test]
    fn validator_schematic_non_primary_is_advice() {
        let obligations = ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("envelope".to_string()),
                role: SemanticRole("envelope".to_string()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::Unresolved,
            }],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Schematic, &obligations);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, FindingSeverity::Advice);
    }

    // --- Validator: Constructible → error for unresolved ---

    #[test]
    fn validator_constructible_unresolved_is_error() {
        let obligations = ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("structure".to_string()),
                role: SemanticRole("primary_structure".to_string()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::Unresolved,
            }],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Constructible, &obligations);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, FindingSeverity::Error);
    }

    #[test]
    fn validator_satisfied_deferred_waived_pass_at_constructible() {
        let obligations = ObligationSet {
            entries: vec![
                Obligation {
                    id: ObligationId("a".to_string()),
                    role: SemanticRole("primary_structure".to_string()),
                    required_by_state: RefinementState::Schematic,
                    status: ObligationStatus::SatisfiedBy(99),
                },
                Obligation {
                    id: ObligationId("b".to_string()),
                    role: SemanticRole("envelope".to_string()),
                    required_by_state: RefinementState::Constructible,
                    status: ObligationStatus::Deferred("spec pending".to_string()),
                },
                Obligation {
                    id: ObligationId("c".to_string()),
                    role: SemanticRole("thermal".to_string()),
                    required_by_state: RefinementState::Constructible,
                    status: ObligationStatus::Waived("out of scope for phase".to_string()),
                },
            ],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Constructible, &obligations);
        assert!(
            findings.is_empty(),
            "Satisfied/Deferred/Waived must not generate findings"
        );
    }

    // --- Validator: obligation not yet required at current state ---

    #[test]
    fn validator_obligation_not_yet_required_is_not_fired() {
        // Obligation required at Constructible, but entity is only Schematic.
        let obligations = ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("detail_drawings".to_string()),
                role: SemanticRole("documentation".to_string()),
                required_by_state: RefinementState::Constructible,
                status: ObligationStatus::Unresolved,
            }],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Schematic, &obligations);
        assert!(findings.is_empty());
    }
}
