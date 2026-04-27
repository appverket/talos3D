//! BIM spatial containment (ADR-026 Phase 6g).
//!
//! Per ADR-026 §7 the BIM exchange formats (IFC and analogues)
//! require every placed element to be assigned to **exactly one**
//! node in a spatial containment tree (project → site → building →
//! storey → space). The semantic-assembly substrate already
//! supports aggregate membership but explicitly allows
//! multi-membership, which is incompatible with the single-parent
//! tree invariant required for spatial containment.
//!
//! This module ships the typed `SpatialContainer` contract with
//! the three invariants ADR-026 §7 prescribes:
//!
//! 1. **Single-parent**: each contained Occurrence has at most one
//!    `spatial_container`.
//! 2. **Tree**: the containment graph is acyclic; no element is
//!    its own spatial ancestor.
//! 3. **No multi-membership**: spatial assignment is exclusive,
//!    unlike `SemanticAssembly`.
//!
//! Domain pack vocabulary (site / building / storey / space /
//! zone / system) is **not** hard-coded into core. Capability
//! crates register the kinds that satisfy the spatial-container
//! contract via [`SpatialContainerKindRegistry`]; core only
//! enforces the typed pointer + tree invariants.
//!
//! `SpatialContainer` is realised as a typed contract/role rather
//! than a separate authored entity family — see ADR-026's
//! Alternatives Considered: a distinct `SpatialNode` authored kind
//! is deferred unless the contract-over-role approach proves
//! insufficient.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plugins::identity::ElementId;

// ---------------------------------------------------------------------------
// Container kind registry
// ---------------------------------------------------------------------------

/// Identifier of a kind of spatial container ("storey", "space",
/// "zone", "site", "building", …). Free-form so domain packs
/// declare their own without core knowing the vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct SpatialContainerKind(pub String);

impl SpatialContainerKind {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Bevy resource: registry of `SpatialContainerKind`s that the
/// kernel will accept as spatial containers. Domain packs register
/// their kinds at plugin build time; the contract layer rejects
/// memberships in unregistered kinds.
#[derive(Resource, Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpatialContainerKindRegistry {
    pub kinds: HashSet<SpatialContainerKind>,
}

impl SpatialContainerKindRegistry {
    pub fn register(&mut self, kind: SpatialContainerKind) {
        self.kinds.insert(kind);
    }

    pub fn is_registered(&self, kind: &SpatialContainerKind) -> bool {
        self.kinds.contains(kind)
    }
}

// ---------------------------------------------------------------------------
// Container marker
// ---------------------------------------------------------------------------

/// Bevy component identifying an entity that **is** a spatial
/// container. Carries the kind label (`storey`, `space`, …) so the
/// contract layer can validate against the kind registry.
#[derive(Component, Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpatialContainer {
    pub kind: SpatialContainerKind,
}

impl SpatialContainer {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: SpatialContainerKind::new(kind),
        }
    }
}

// ---------------------------------------------------------------------------
// Membership
// ---------------------------------------------------------------------------

/// Bevy component placed on an entity that **is contained in** a
/// spatial container. Stores the parent container's `ElementId`
/// (single-parent invariant — the type is `ElementId`, not
/// `Vec<ElementId>`).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpatialMembership {
    pub container: ElementId,
}

impl SpatialMembership {
    pub fn in_container(container: ElementId) -> Self {
        Self { container }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum SpatialMembershipError {
    /// Attempted to assign an element to a container kind that is
    /// not registered.
    UnregisteredKind { kind: SpatialContainerKind },
    /// The proposed assignment would create a cycle (the proposed
    /// container is a descendant of the element being assigned, or
    /// equal to it).
    WouldCreateCycle {
        element: ElementId,
        container: ElementId,
    },
    /// The element already has a spatial assignment. The
    /// single-parent invariant is enforced at write time; callers
    /// must explicitly remove the prior assignment first.
    AlreadyAssigned {
        element: ElementId,
        existing_container: ElementId,
    },
}

impl std::fmt::Display for SpatialMembershipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnregisteredKind { kind } => write!(
                f,
                "container kind '{}' is not registered",
                kind.as_str()
            ),
            Self::WouldCreateCycle { element, container } => write!(
                f,
                "would create a cycle: assigning element {:?} to container {:?}",
                element.0, container.0
            ),
            Self::AlreadyAssigned {
                element,
                existing_container,
            } => write!(
                f,
                "element {:?} already assigned to container {:?}; remove first",
                element.0, existing_container.0
            ),
        }
    }
}

impl std::error::Error for SpatialMembershipError {}

// ---------------------------------------------------------------------------
// Adjacency view + invariant checks
// ---------------------------------------------------------------------------

/// Read-only adjacency view of the current spatial-containment
/// graph: `child → parent`. Built by the caller from the world's
/// `SpatialMembership` components and passed into the validation
/// helpers. Keeping the view explicit (rather than reaching into
/// the world directly) keeps the invariant checks unit-testable
/// without a full Bevy fixture.
#[derive(Debug, Clone, Default)]
pub struct SpatialContainmentGraph {
    /// `child_element → parent_container`. Single-parent by construction.
    pub parent_of: HashMap<ElementId, ElementId>,
}

impl SpatialContainmentGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_edge(mut self, child: ElementId, parent: ElementId) -> Self {
        self.parent_of.insert(child, parent);
        self
    }

    pub fn parent(&self, child: &ElementId) -> Option<&ElementId> {
        self.parent_of.get(child)
    }

    /// Walk ancestors of `element`. Yields the immediate parent,
    /// then its parent, … until an entry without a parent is
    /// reached. Bounded by `max_depth` to defend against malformed
    /// graphs that contain a cycle (cycles are rejected on insert,
    /// but be defensive on read).
    pub fn ancestors(&self, element: ElementId, max_depth: usize) -> Vec<ElementId> {
        let mut out = Vec::new();
        let mut cur = element;
        for _ in 0..max_depth {
            match self.parent_of.get(&cur) {
                Some(parent) => {
                    out.push(*parent);
                    cur = *parent;
                }
                None => break,
            }
        }
        out
    }

    /// Returns `true` when `element` has `candidate` somewhere in
    /// its ancestor chain (or `candidate == element`). Used to
    /// detect would-be cycles.
    pub fn is_ancestor_or_self(
        &self,
        element: ElementId,
        candidate: ElementId,
        max_depth: usize,
    ) -> bool {
        if element == candidate {
            return true;
        }
        let mut cur = element;
        for _ in 0..max_depth {
            match self.parent_of.get(&cur) {
                Some(parent) => {
                    if *parent == candidate {
                        return true;
                    }
                    cur = *parent;
                }
                None => return false,
            }
        }
        false
    }
}

/// Validate a proposed `(child, container)` membership against the
/// three invariants. The caller is responsible for actually
/// inserting the `SpatialMembership` component on success.
pub fn validate_assignment(
    graph: &SpatialContainmentGraph,
    kinds: &SpatialContainerKindRegistry,
    container_kind: &SpatialContainerKind,
    child: ElementId,
    container: ElementId,
) -> Result<(), SpatialMembershipError> {
    // Invariant 1: kind must be registered.
    if !kinds.is_registered(container_kind) {
        return Err(SpatialMembershipError::UnregisteredKind {
            kind: container_kind.clone(),
        });
    }
    // Invariant 2: the proposed container must not be a descendant
    // of the child (which would create a cycle), and the child must
    // not equal the container.
    let max_depth = graph.parent_of.len().saturating_add(1);
    if graph.is_ancestor_or_self(container, child, max_depth) {
        return Err(SpatialMembershipError::WouldCreateCycle { element: child, container });
    }
    // Invariant 3: single-parent. The child must not already have
    // a parent; reassignment requires removing the prior membership
    // first (so the API surfaces the move rather than letting it
    // silently overwrite).
    if let Some(existing) = graph.parent_of.get(&child) {
        return Err(SpatialMembershipError::AlreadyAssigned {
            element: child,
            existing_container: *existing,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Bevy plugin: installs the `SpatialContainerKindRegistry`
/// resource. Components (`SpatialContainer`, `SpatialMembership`)
/// are entity data; no further setup is needed.
pub struct SpatialContainerPlugin;

impl Plugin for SpatialContainerPlugin {
    fn build(&self, app: &mut App) {
        if !app
            .world()
            .contains_resource::<SpatialContainerKindRegistry>()
        {
            app.init_resource::<SpatialContainerKindRegistry>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storey_kind() -> SpatialContainerKind {
        SpatialContainerKind::new("storey")
    }

    fn space_kind() -> SpatialContainerKind {
        SpatialContainerKind::new("space")
    }

    fn registry_with_storey_and_space() -> SpatialContainerKindRegistry {
        let mut reg = SpatialContainerKindRegistry::default();
        reg.register(storey_kind());
        reg.register(space_kind());
        reg
    }

    // ── Kind registry ───────────────────────────────────────────

    #[test]
    fn registry_accepts_registered_kind() {
        let reg = registry_with_storey_and_space();
        assert!(reg.is_registered(&storey_kind()));
        assert!(reg.is_registered(&space_kind()));
    }

    #[test]
    fn registry_rejects_unregistered_kind() {
        let reg = registry_with_storey_and_space();
        assert!(!reg.is_registered(&SpatialContainerKind::new("zone")));
    }

    // ── SpatialContainer marker ─────────────────────────────────

    #[test]
    fn spatial_container_marker_exposes_kind() {
        let c = SpatialContainer::new("storey");
        assert_eq!(c.kind, storey_kind());
    }

    // ── Containment graph reads ─────────────────────────────────

    #[test]
    fn ancestors_walks_chain() {
        let graph = SpatialContainmentGraph::new()
            .with_edge(ElementId(10), ElementId(20)) // 10 in 20
            .with_edge(ElementId(20), ElementId(30)); // 20 in 30
        let ancestors = graph.ancestors(ElementId(10), 8);
        assert_eq!(ancestors, vec![ElementId(20), ElementId(30)]);
    }

    #[test]
    fn ancestors_terminates_on_root() {
        let graph = SpatialContainmentGraph::new()
            .with_edge(ElementId(10), ElementId(20));
        let ancestors = graph.ancestors(ElementId(20), 8);
        assert!(ancestors.is_empty());
    }

    #[test]
    fn ancestors_respects_max_depth() {
        let graph = SpatialContainmentGraph::new()
            .with_edge(ElementId(10), ElementId(20))
            .with_edge(ElementId(20), ElementId(30))
            .with_edge(ElementId(30), ElementId(40));
        let ancestors = graph.ancestors(ElementId(10), 2);
        assert_eq!(ancestors, vec![ElementId(20), ElementId(30)]);
    }

    #[test]
    fn is_ancestor_or_self_handles_identity() {
        let graph = SpatialContainmentGraph::new();
        assert!(graph.is_ancestor_or_self(ElementId(7), ElementId(7), 8));
    }

    #[test]
    fn is_ancestor_or_self_walks_chain() {
        let graph = SpatialContainmentGraph::new()
            .with_edge(ElementId(10), ElementId(20))
            .with_edge(ElementId(20), ElementId(30));
        assert!(graph.is_ancestor_or_self(ElementId(10), ElementId(30), 8));
        assert!(graph.is_ancestor_or_self(ElementId(10), ElementId(20), 8));
        assert!(!graph.is_ancestor_or_self(ElementId(10), ElementId(99), 8));
    }

    // ── validate_assignment ─────────────────────────────────────

    #[test]
    fn validate_assignment_accepts_first_valid_membership() {
        let kinds = registry_with_storey_and_space();
        let graph = SpatialContainmentGraph::new();
        let r = validate_assignment(&graph, &kinds, &storey_kind(), ElementId(1), ElementId(2));
        assert!(r.is_ok());
    }

    #[test]
    fn validate_assignment_rejects_unregistered_kind() {
        let kinds = registry_with_storey_and_space();
        let graph = SpatialContainmentGraph::new();
        let err = validate_assignment(
            &graph,
            &kinds,
            &SpatialContainerKind::new("not_registered"),
            ElementId(1),
            ElementId(2),
        )
        .unwrap_err();
        match err {
            SpatialMembershipError::UnregisteredKind { kind } => {
                assert_eq!(kind, SpatialContainerKind::new("not_registered"));
            }
            _ => panic!("expected UnregisteredKind"),
        }
    }

    #[test]
    fn validate_assignment_rejects_self_containment() {
        let kinds = registry_with_storey_and_space();
        let graph = SpatialContainmentGraph::new();
        let err = validate_assignment(&graph, &kinds, &storey_kind(), ElementId(7), ElementId(7))
            .unwrap_err();
        match err {
            SpatialMembershipError::WouldCreateCycle { element, container } => {
                assert_eq!(element, ElementId(7));
                assert_eq!(container, ElementId(7));
            }
            _ => panic!("expected WouldCreateCycle"),
        }
    }

    #[test]
    fn validate_assignment_rejects_proposal_that_would_create_cycle() {
        let kinds = registry_with_storey_and_space();
        // 10 already in 20, 20 in 30. Proposing 30 in 10 would
        // close a cycle 30 → 10 → 20 → 30.
        let graph = SpatialContainmentGraph::new()
            .with_edge(ElementId(10), ElementId(20))
            .with_edge(ElementId(20), ElementId(30));
        let err = validate_assignment(&graph, &kinds, &storey_kind(), ElementId(30), ElementId(10))
            .unwrap_err();
        assert!(matches!(err, SpatialMembershipError::WouldCreateCycle { .. }));
    }

    #[test]
    fn validate_assignment_rejects_double_assignment() {
        let kinds = registry_with_storey_and_space();
        // 10 already in 20. Proposing 10 in 30 must be rejected;
        // single-parent invariant requires explicit removal first.
        let graph = SpatialContainmentGraph::new()
            .with_edge(ElementId(10), ElementId(20));
        let err = validate_assignment(&graph, &kinds, &storey_kind(), ElementId(10), ElementId(30))
            .unwrap_err();
        match err {
            SpatialMembershipError::AlreadyAssigned {
                element,
                existing_container,
            } => {
                assert_eq!(element, ElementId(10));
                assert_eq!(existing_container, ElementId(20));
            }
            _ => panic!("expected AlreadyAssigned"),
        }
    }

    #[test]
    fn validate_assignment_allows_orthogonal_branch() {
        let kinds = registry_with_storey_and_space();
        // 10 in 20; 11 not yet placed. 11 in 20 is fine.
        let graph = SpatialContainmentGraph::new()
            .with_edge(ElementId(10), ElementId(20));
        let r = validate_assignment(&graph, &kinds, &storey_kind(), ElementId(11), ElementId(20));
        assert!(r.is_ok());
    }

    // ── Components round-trip ───────────────────────────────────

    #[test]
    fn membership_round_trips_through_json() {
        let m = SpatialMembership::in_container(ElementId(123));
        let json = serde_json::to_string(&m).unwrap();
        let parsed: SpatialMembership = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn container_round_trips_through_json() {
        let c = SpatialContainer::new("storey");
        let json = serde_json::to_string(&c).unwrap();
        let parsed: SpatialContainer = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, c);
    }

    // ── Plugin ──────────────────────────────────────────────────

    #[test]
    fn plugin_installs_kind_registry() {
        let mut app = App::new();
        app.add_plugins(SpatialContainerPlugin);
        assert!(app
            .world()
            .contains_resource::<SpatialContainerKindRegistry>());
    }

    // ── Diagnostic display ──────────────────────────────────────

    #[test]
    fn cycle_error_display_mentions_both_ids() {
        let err = SpatialMembershipError::WouldCreateCycle {
            element: ElementId(7),
            container: ElementId(11),
        };
        let display = format!("{err}");
        assert!(display.contains("7"));
        assert!(display.contains("11"));
        assert!(display.contains("cycle"));
    }
}
