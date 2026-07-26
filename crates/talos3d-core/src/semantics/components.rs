//! ECS components carrying semantic state, and the `World`-backed
//! [`SemanticContext`] the kernel reads on the mutation path.
//!
//! Two components matter:
//!
//! * [`ConceptAssignment`] — the entity's claim about *what it is*. Its
//!   presence is what arms the kernel (ADR-064 §7); its absence means
//!   unclassified geometry, which stays fully editable and simply makes no
//!   domain claim.
//! * [`PublishedAnchors`] — the anchor instances this entity offers to others.
//!   Identity is publisher + kind + role and excludes coordinates, so a
//!   regenerated host keeps the same instances with a bumped revision and
//!   dependents are invalidated rather than silently detached.
//!
//! [`ConceptAssignment`] sits alongside
//! [`ElementClassAssignment`](crate::capability_registry::ElementClassAssignment)
//! rather than replacing it: concept is *meaning*, element class is
//! *representation*, and neither is generated from the other (ADR-064 §5).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::curation::JurisdictionTag;
use crate::plugins::identity::ElementId;
use crate::plugins::refinement::{RefinementState, RefinementStateComponent};

use super::ids::{AnchorKindId, AnchorRoleId, ConceptId, PredicateId};
use super::plan::{AnchorInstanceId, BindTarget, SemanticContext};
use super::registry::SemanticRegistry;

/// The concept an entity claims. Presence arms the admissibility kernel.
#[derive(Component, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ConceptAssignment {
    pub concept: ConceptId,
}

impl ConceptAssignment {
    pub fn new(concept: impl Into<ConceptId>) -> Self {
        Self {
            concept: concept.into(),
        }
    }
}

/// One resolved anchor an entity publishes.
///
/// `revision` increments when the host regenerates. Dependents compare it to
/// detect staleness; they are never detached on regeneration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PublishedAnchor {
    pub kind: AnchorKindId,
    pub role: AnchorRoleId,
    #[serde(default)]
    pub revision: u64,
}

/// Every anchor instance an entity currently publishes.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PublishedAnchors {
    #[serde(default)]
    pub anchors: Vec<PublishedAnchor>,
}

impl PublishedAnchors {
    pub fn new(anchors: impl IntoIterator<Item = PublishedAnchor>) -> Self {
        Self {
            anchors: anchors.into_iter().collect(),
        }
    }

    /// Bump the revision of every published anchor, preserving identity.
    ///
    /// This is what a host calls after regenerating: the roof re-pitched, so
    /// `rake_edge/north_west` now resolves elsewhere, but it is still the same
    /// anchor and the bargeboard resolving against it stays attached.
    pub fn bump_revisions(&mut self) {
        for anchor in &mut self.anchors {
            anchor.revision = anchor.revision.saturating_add(1);
        }
    }

    pub fn revision_of(&self, kind: &AnchorKindId, role: &AnchorRoleId) -> Option<u64> {
        self.anchors
            .iter()
            .find(|anchor| &anchor.kind == kind && &anchor.role == role)
            .map(|anchor| anchor.revision)
    }
}

/// A binding an entity holds, recorded so cardinality and obligation checks can
/// see what already exists.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SemanticBindings {
    #[serde(default)]
    pub bindings: Vec<SemanticBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SemanticBinding {
    pub predicate: PredicateId,
    pub anchor_publisher: u64,
    pub anchor_kind: AnchorKindId,
    pub anchor_role: AnchorRoleId,
}

impl SemanticBinding {
    pub fn anchor_id(&self) -> AnchorInstanceId {
        AnchorInstanceId {
            publisher: ElementId(self.anchor_publisher),
            kind: self.anchor_kind.clone(),
            role: self.anchor_role.clone(),
        }
    }
}

/// The active jurisdiction, if the project has resolved one.
///
/// `None` is a legitimate state and means only general-scope propositions
/// apply — regional context is resolved, never assumed.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct ActiveJurisdiction(pub Option<JurisdictionTag>);

/// The compiled Design Concept Graph, as a Bevy resource.
#[derive(Resource, Debug, Clone, Default)]
pub struct SemanticGraph(pub SemanticRegistry);

impl std::ops::Deref for SemanticGraph {
    type Target = SemanticRegistry;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Reads semantic state out of a live `World` for the kernel.
///
/// Borrowing `&World` keeps this cheap: every lookup is a component fetch, not
/// a query build, which matters because it runs inside the command drain.
pub struct WorldSemanticContext<'w> {
    world: &'w World,
}

impl<'w> WorldSemanticContext<'w> {
    pub fn new(world: &'w World) -> Self {
        Self { world }
    }

    /// Resolve an `ElementId` to its Bevy entity.
    fn entity_ref(&self, element: ElementId) -> Option<EntityRef<'w>> {
        crate::plugins::commands::find_entity_by_element_id_readonly(self.world, element)
            .and_then(|entity| self.world.get_entity(entity).ok())
    }
}

impl SemanticContext for WorldSemanticContext<'_> {
    fn concept_of(&self, entity: ElementId) -> Option<ConceptId> {
        self.entity_ref(entity)
            .and_then(|entity_ref| entity_ref.get::<ConceptAssignment>())
            .map(|assignment| assignment.concept.clone())
    }

    fn published_anchors(&self, entity: ElementId) -> Vec<AnchorInstanceId> {
        self.entity_ref(entity)
            .and_then(|entity_ref| entity_ref.get::<PublishedAnchors>())
            .map(|published| {
                published
                    .anchors
                    .iter()
                    .map(|anchor| AnchorInstanceId {
                        publisher: entity,
                        kind: anchor.kind.clone(),
                        role: anchor.role.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn refinement_state(&self, entity: ElementId) -> RefinementState {
        self.entity_ref(entity)
            .and_then(|entity_ref| entity_ref.get::<RefinementStateComponent>())
            .map(|component| component.state)
            .unwrap_or_default()
    }

    fn jurisdiction(&self) -> Option<JurisdictionTag> {
        self.world
            .get_resource::<ActiveJurisdiction>()
            .and_then(|active| active.0.clone())
    }

    fn existing_bindings(&self, subject: ElementId, predicate: &PredicateId) -> Vec<BindTarget> {
        self.entity_ref(subject)
            .and_then(|entity_ref| entity_ref.get::<SemanticBindings>())
            .map(|bindings| {
                bindings
                    .bindings
                    .iter()
                    .filter(|binding| &binding.predicate == predicate)
                    .map(|binding| BindTarget::Anchor(binding.anchor_id()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bumping_revisions_preserves_anchor_identity() {
        let mut published = PublishedAnchors::new([PublishedAnchor {
            kind: AnchorKindId::new("roof.rake_edge"),
            role: AnchorRoleId::new("north_west"),
            revision: 3,
        }]);
        published.bump_revisions();

        let anchor = &published.anchors[0];
        // Identity fields are untouched; only the revision moves. This is what
        // keeps dependents attached across host regeneration.
        assert_eq!(anchor.kind, AnchorKindId::new("roof.rake_edge"));
        assert_eq!(anchor.role, AnchorRoleId::new("north_west"));
        assert_eq!(anchor.revision, 4);
    }

    #[test]
    fn revision_lookup_is_by_identity_not_position() {
        let published = PublishedAnchors::new([
            PublishedAnchor {
                kind: AnchorKindId::new("roof.rake_edge"),
                role: AnchorRoleId::new("north_west"),
                revision: 1,
            },
            PublishedAnchor {
                kind: AnchorKindId::new("roof.rake_edge"),
                role: AnchorRoleId::new("north_east"),
                revision: 7,
            },
        ]);
        assert_eq!(
            published.revision_of(
                &AnchorKindId::new("roof.rake_edge"),
                &AnchorRoleId::new("north_east")
            ),
            Some(7)
        );
        assert_eq!(
            published.revision_of(
                &AnchorKindId::new("roof.eave_line"),
                &AnchorRoleId::new("north_east")
            ),
            None
        );
    }
}
