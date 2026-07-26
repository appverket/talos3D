//! The semantic edit plan and the context the kernel reads it against.
//!
//! ADR-064 §3: every undoable mutation converges on `PendingCommandQueue`, so
//! the plan is what an `EditorCommand` declares about its semantic intent and
//! the kernel evaluates *before* `apply`. Preview evaluates the identical plan
//! through the identical function without pushing, which is what makes
//! preview/commit parity true by construction rather than by test discipline.

use crate::curation::JurisdictionTag;
use crate::plugins::identity::ElementId;
use crate::plugins::refinement::RefinementState;

use super::ids::{AnchorKindId, AnchorRoleId, ConceptId, PredicateId};

/// Identity of one resolved anchor on one host.
///
/// Deliberately excludes coordinates. Identity is publisher + kind + role, so a
/// roof regenerated at a new pitch keeps the *same* anchor instances with a new
/// revision — dependents are invalidated, never silently detached (ADR-064 §1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnchorInstanceId {
    pub publisher: ElementId,
    pub kind: AnchorKindId,
    pub role: AnchorRoleId,
}

impl AnchorInstanceId {
    pub fn new(
        publisher: ElementId,
        kind: impl Into<AnchorKindId>,
        role: impl Into<AnchorRoleId>,
    ) -> Self {
        Self {
            publisher,
            kind: kind.into(),
            role: role.into(),
        }
    }
}

/// What a binding points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindTarget {
    /// The well-formed case: resolve against a published anchor.
    Anchor(AnchorInstanceId),
    /// A bare entity with no anchor. Legal only where a proposition names a
    /// concept object; otherwise this is the shape of the bargeboard defect.
    Entity(ElementId),
}

/// One unit of semantic intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanIntent {
    /// Claiming a concept is what arms the kernel (agreement §7).
    AssignConcept {
        entity: ElementId,
        concept: ConceptId,
    },
    /// An explicit semantic downgrade. Never a quiet escape from a refusal.
    RemoveConcept {
        entity: ElementId,
        concept: ConceptId,
    },
    /// Bind a concept-bearing entity through a registered predicate.
    Bind {
        subject: ElementId,
        predicate: PredicateId,
        target: BindTarget,
    },
}

/// The semantic intent of one command.
///
/// [`SemanticPlan::none`] is the default an `EditorCommand` returns when it
/// makes no semantic claim, which keeps the existing command population
/// untouched and geometry-only commands geometry-only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SemanticPlan {
    pub intents: Vec<PlanIntent>,
}

impl SemanticPlan {
    /// This command makes no semantic claim; the kernel stays disarmed.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.intents.is_empty()
    }

    pub fn with(mut self, intent: PlanIntent) -> Self {
        self.intents.push(intent);
        self
    }

    /// Merge plans, used when a `GroupedCommand` is evaluated as one unit so a
    /// refusal anywhere in the group refuses the group (ADR-064 §3.2).
    pub fn merge(mut self, other: Self) -> Self {
        self.intents.extend(other.intents);
        self
    }
}

impl FromIterator<PlanIntent> for SemanticPlan {
    fn from_iter<T: IntoIterator<Item = PlanIntent>>(iter: T) -> Self {
        Self {
            intents: iter.into_iter().collect(),
        }
    }
}

/// What the kernel needs to know about the world.
///
/// A trait rather than a `&World` so the kernel stays pure, unit-testable
/// without Bevy, and reusable by preview, command, and MCP paths alike.
pub trait SemanticContext {
    /// The concept an entity currently claims, if any. `None` means
    /// unclassified geometry: editable, but making no domain claim.
    fn concept_of(&self, entity: ElementId) -> Option<ConceptId>;

    /// Anchor instances an entity actually publishes right now.
    fn published_anchors(&self, entity: ElementId) -> Vec<AnchorInstanceId>;

    /// Refinement state governing `entity`.
    fn refinement_state(&self, entity: ElementId) -> RefinementState;

    /// Active jurisdiction, if resolved. `None` means unscoped, and only
    /// general-scope propositions apply.
    fn jurisdiction(&self) -> Option<JurisdictionTag> {
        None
    }

    /// Bindings the subject already holds for a predicate, for cardinality
    /// checks. Default empty: a context that does not track bindings simply
    /// cannot violate cardinality.
    fn existing_bindings(&self, _subject: ElementId, _predicate: &PredicateId) -> Vec<BindTarget> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_plan_is_empty_and_disarms_the_kernel() {
        assert!(SemanticPlan::none().is_empty());
        assert!(SemanticPlan::default().is_empty());
    }

    #[test]
    fn plans_merge_for_grouped_commands() {
        let a = SemanticPlan::none().with(PlanIntent::AssignConcept {
            entity: ElementId(1),
            concept: ConceptId::new("c"),
        });
        let b = SemanticPlan::none().with(PlanIntent::RemoveConcept {
            entity: ElementId(2),
            concept: ConceptId::new("c"),
        });
        assert_eq!(a.merge(b).intents.len(), 2);
    }

    #[test]
    fn anchor_identity_ignores_geometry_and_depends_on_role() {
        let a = AnchorInstanceId::new(ElementId(388), "roof.rake_edge", "north_west");
        let b = AnchorInstanceId::new(ElementId(388), "roof.rake_edge", "north_west");
        let c = AnchorInstanceId::new(ElementId(388), "roof.rake_edge", "north_east");
        assert_eq!(a, b, "same publisher/kind/role is the same anchor");
        assert_ne!(a, c, "role discriminates sibling anchors");
    }
}
