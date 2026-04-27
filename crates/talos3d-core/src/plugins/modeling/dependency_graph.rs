//! Generic entity dependency graph (ADR-007 kernel).
//!
//! ADR-007 §1-§4 prescribe a directed acyclic graph of dependency
//! edges between authored entities, with topological-order
//! propagation of changes. This module ships the **data primitives
//! and algorithms** that any consumer of the dependency graph can
//! build on:
//!
//! - `EntityDependencies` Bevy component listing the entities a
//!   given entity depends on (edges out of this entity in the
//!   ADR-007 sense).
//! - `DependencyEdge { dependent, dependency, role }` for the
//!   read-side adjacency view.
//! - `DependencyGraph` read-only adjacency cache built from the
//!   world's `EntityDependencies` components, with `parents_of`,
//!   `children_of` (transposed), `topological_order`,
//!   `would_create_cycle`, and `bounded_descendant_walk` helpers.
//! - `DependencyGraphError` typed error: CycleDetected,
//!   UnknownEntity.
//!
//! What this module does **not** ship (deliberately, per ADR-007's
//! "incremental introduction" guidance):
//!
//! 1. A propagation system that consumes the topological order to
//!    re-evaluate dependents on dirty marks. Existing systems
//!    (`NeedsEvaluation` cascade in `mesh_generation`,
//!    `support_graph` resolver, etc.) handle propagation in
//!    domain-specific ways. The kernel here gives them a shared
//!    primitive to reuse.
//! 2. A type-registration trait for parametric definition kinds
//!    (ADR-007 §5). The existing `AuthoredEntityFactory`
//!    infrastructure plays that role today; a future slice can
//!    cross-link it to the dependency graph.
//! 3. Constraint-as-dependency-edge wiring (ADR-007 §6). Domain
//!    crates declare constraints today; reusing the dependency
//!    propagation order for them is a natural follow-up.
//!
//! The module is **additive**: no existing core type is modified.
//! Consumers opt in by attaching `EntityDependencies` to their
//! authored entities and consulting the kernel's algorithms when
//! they need topological propagation.

use std::collections::{HashMap, HashSet, VecDeque};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plugins::identity::ElementId;

// ---------------------------------------------------------------------------
// Edge label
// ---------------------------------------------------------------------------

/// Free-form role tag distinguishing dependency edges of different
/// kinds (e.g. `"parametric"` for parameter-driven edges,
/// `"hosted_on"` for an opening hosted on a wall, `"on_surface"`
/// for a constraint that pins a point to a surface). The string is
/// opaque to the kernel; consumers read it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct DependencyRole(pub String);

impl DependencyRole {
    pub fn new(role: impl Into<String>) -> Self {
        Self(role.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Per-entity component
// ---------------------------------------------------------------------------

/// A single dependency: the entity at the head depends on the
/// entity at the tail, in the given role. Stored on the head's
/// `EntityDependencies` component.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DependencyOut {
    pub on: ElementId,
    pub role: DependencyRole,
}

impl DependencyOut {
    pub fn new(on: ElementId, role: impl Into<String>) -> Self {
        Self {
            on,
            role: DependencyRole::new(role),
        }
    }
}

/// Bevy component listing the dependencies of a single entity
/// (i.e. the entities **this** entity depends on). Per ADR-007 §1
/// dependencies are stored on the dependent entity, not the
/// dependency.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityDependencies {
    pub edges: Vec<DependencyOut>,
}

impl EntityDependencies {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn with_edge(mut self, on: ElementId, role: impl Into<String>) -> Self {
        self.edges.push(DependencyOut::new(on, role));
        self
    }

    pub fn add(&mut self, on: ElementId, role: impl Into<String>) {
        self.edges.push(DependencyOut::new(on, role));
    }

    pub fn remove_to(&mut self, on: ElementId, role: &str) -> bool {
        let before = self.edges.len();
        self.edges.retain(|e| !(e.on == on && e.role.as_str() == role));
        before != self.edges.len()
    }

    /// All distinct dependency targets, ignoring role.
    pub fn target_set(&self) -> HashSet<ElementId> {
        self.edges.iter().map(|e| e.on).collect()
    }
}

// ---------------------------------------------------------------------------
// Read-side adjacency view
// ---------------------------------------------------------------------------

/// Read-only adjacency view of the current dependency graph, built
/// by the caller from `EntityDependencies` components in the world.
///
/// `out_edges[a]` is the list of `b` such that `a` depends on `b`.
/// `in_edges[b]` is the transpose: the entities that depend on `b`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DependencyGraph {
    out_edges: HashMap<ElementId, Vec<DependencyOut>>,
    in_edges: HashMap<ElementId, Vec<ElementId>>,
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder helper for tests and synthetic graphs: declare that
    /// `dependent` depends on `dependency` in the given `role`.
    pub fn with_edge(
        mut self,
        dependent: ElementId,
        dependency: ElementId,
        role: impl Into<String>,
    ) -> Self {
        self.add_edge(dependent, dependency, role);
        self
    }

    pub fn add_edge(
        &mut self,
        dependent: ElementId,
        dependency: ElementId,
        role: impl Into<String>,
    ) {
        let edge = DependencyOut::new(dependency, role);
        self.out_edges.entry(dependent).or_default().push(edge);
        self.in_edges.entry(dependency).or_default().push(dependent);
    }

    /// Replace the out-edge list of `dependent` wholesale. Used by
    /// systems that rebuild the graph from `EntityDependencies`
    /// every frame, or by command handlers that snapshot one
    /// entity's dependencies in a single edit.
    pub fn set_dependencies(&mut self, dependent: ElementId, edges: Vec<DependencyOut>) {
        // Remove old in-edges referencing this dependent.
        if let Some(prev) = self.out_edges.remove(&dependent) {
            for old in prev {
                if let Some(parents) = self.in_edges.get_mut(&old.on) {
                    parents.retain(|d| *d != dependent);
                }
            }
        }
        for e in &edges {
            self.in_edges.entry(e.on).or_default().push(dependent);
        }
        self.out_edges.insert(dependent, edges);
    }

    /// Number of distinct entities mentioned as dependent or
    /// dependency.
    pub fn node_count(&self) -> usize {
        let mut nodes: HashSet<ElementId> = HashSet::new();
        for k in self.out_edges.keys() {
            nodes.insert(*k);
        }
        for k in self.in_edges.keys() {
            nodes.insert(*k);
        }
        nodes.len()
    }

    /// Direct dependencies of `entity`.
    pub fn parents_of(&self, entity: ElementId) -> &[DependencyOut] {
        self.out_edges
            .get(&entity)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Direct dependents of `entity` (transposed view).
    pub fn children_of(&self, entity: ElementId) -> &[ElementId] {
        self.in_edges
            .get(&entity)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Walk all transitive dependents of `entity` (everything that
    /// transitively depends on it). Ordering is BFS from `entity`.
    /// Bounded by `max_depth` to defend against malformed graphs
    /// even though `topological_order` rejects cycles up front.
    pub fn bounded_descendant_walk(
        &self,
        entity: ElementId,
        max_depth: usize,
    ) -> Vec<ElementId> {
        let mut out = Vec::new();
        let mut seen: HashSet<ElementId> = HashSet::new();
        seen.insert(entity);
        let mut frontier: VecDeque<(ElementId, usize)> = VecDeque::new();
        frontier.push_back((entity, 0));
        while let Some((node, depth)) = frontier.pop_front() {
            if depth >= max_depth {
                continue;
            }
            for child in self.children_of(node) {
                if seen.insert(*child) {
                    out.push(*child);
                    frontier.push_back((*child, depth + 1));
                }
            }
        }
        out
    }

    /// Returns true when adding the edge `(dependent, dependency)`
    /// would create a cycle. Computed by checking whether
    /// `dependent` is already a transitive ancestor of
    /// `dependency` (i.e. whether `dependency` already transitively
    /// depends on `dependent`).
    ///
    /// Self-edges (`dependent == dependency`) always count as
    /// would-create-cycle.
    pub fn would_create_cycle(
        &self,
        dependent: ElementId,
        dependency: ElementId,
    ) -> bool {
        if dependent == dependency {
            return true;
        }
        // Walk the dependencies of `dependency`. If `dependent`
        // appears in the closure, the new edge would close a cycle.
        let mut stack: Vec<ElementId> = vec![dependency];
        let mut seen: HashSet<ElementId> = HashSet::new();
        seen.insert(dependency);
        while let Some(node) = stack.pop() {
            for parent in self.parents_of(node) {
                if parent.on == dependent {
                    return true;
                }
                if seen.insert(parent.on) {
                    stack.push(parent.on);
                }
            }
        }
        false
    }

    /// Topological order over all nodes in the graph. The first
    /// element of the returned vector has no dependencies; later
    /// elements depend only on earlier ones.
    ///
    /// Returns `Err(DependencyGraphError::CycleDetected)` if the
    /// graph contains a cycle. Time complexity: O(V + E) via
    /// Kahn's algorithm.
    pub fn topological_order(&self) -> Result<Vec<ElementId>, DependencyGraphError> {
        let mut indegree: HashMap<ElementId, usize> = HashMap::new();
        let mut all_nodes: HashSet<ElementId> = HashSet::new();
        for (node, edges) in &self.out_edges {
            all_nodes.insert(*node);
            indegree.entry(*node).or_insert(0);
            for e in edges {
                all_nodes.insert(e.on);
                *indegree.entry(e.on).or_insert(0) += 1;
            }
        }
        for node in &all_nodes {
            indegree.entry(*node).or_insert(0);
        }
        // Kahn's seeds: nodes with indegree 0 (no incoming
        // dependency edge i.e. no node depends on them).
        // Wait — we want "no dependencies" first. Indegree counts
        // INCOMING edges: how many entities depend on this one.
        // So nodes with indegree 0 are "leaf dependencies", which
        // is the wrong end. Re-tally on outgoing edges (degrees of
        // out_edges, i.e. number of dependencies each node has).
        let mut out_degree: HashMap<ElementId, usize> = HashMap::new();
        for node in &all_nodes {
            let count = self.out_edges.get(node).map(Vec::len).unwrap_or(0);
            out_degree.insert(*node, count);
        }
        let mut ready: VecDeque<ElementId> = VecDeque::new();
        for (node, deg) in &out_degree {
            if *deg == 0 {
                ready.push_back(*node);
            }
        }
        let mut order: Vec<ElementId> = Vec::with_capacity(all_nodes.len());
        while let Some(node) = ready.pop_front() {
            order.push(node);
            // Every dependent of `node` loses one outgoing-degree.
            for child in self.children_of(node) {
                if let Some(deg) = out_degree.get_mut(child) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        ready.push_back(*child);
                    }
                }
            }
        }
        if order.len() != all_nodes.len() {
            return Err(DependencyGraphError::CycleDetected);
        }
        Ok(order)
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum DependencyGraphError {
    CycleDetected,
}

impl std::fmt::Display for DependencyGraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CycleDetected => write!(f, "dependency graph contains a cycle"),
        }
    }
}

impl std::error::Error for DependencyGraphError {}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Bevy plugin: reserved for symmetry. The kernel is data + algorithms;
/// no resources or systems are required today. Consumers attach
/// `EntityDependencies` components and call the algorithms directly.
pub struct DependencyGraphPlugin;

impl Plugin for DependencyGraphPlugin {
    fn build(&self, _app: &mut App) {
        // Intentionally empty.
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn eid(n: u64) -> ElementId {
        ElementId(n)
    }

    // ── EntityDependencies ─────────────────────────────────────

    #[test]
    fn entity_dependencies_with_edge_and_remove() {
        let mut deps = EntityDependencies::empty()
            .with_edge(eid(1), "parametric")
            .with_edge(eid(2), "hosted_on");
        assert_eq!(deps.edges.len(), 2);
        assert!(deps.remove_to(eid(1), "parametric"));
        assert_eq!(deps.edges.len(), 1);
        // Removing non-existent role is a no-op.
        assert!(!deps.remove_to(eid(2), "no_such_role"));
    }

    #[test]
    fn entity_dependencies_target_set_dedupes_by_target() {
        let deps = EntityDependencies::empty()
            .with_edge(eid(1), "parametric")
            .with_edge(eid(1), "hosted_on")
            .with_edge(eid(2), "parametric");
        let set = deps.target_set();
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn entity_dependencies_round_trip_through_json() {
        let deps = EntityDependencies::empty()
            .with_edge(eid(1), "parametric")
            .with_edge(eid(2), "hosted_on");
        let json = serde_json::to_string(&deps).unwrap();
        let parsed: EntityDependencies = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, deps);
    }

    // ── Graph reads ────────────────────────────────────────────

    #[test]
    fn parents_of_returns_declared_edges() {
        let g = DependencyGraph::new()
            .with_edge(eid(10), eid(20), "parametric")
            .with_edge(eid(10), eid(21), "hosted_on");
        let parents = g.parents_of(eid(10));
        assert_eq!(parents.len(), 2);
    }

    #[test]
    fn children_of_returns_transposed_edges() {
        let g = DependencyGraph::new()
            .with_edge(eid(1), eid(10), "parametric")
            .with_edge(eid(2), eid(10), "parametric")
            .with_edge(eid(3), eid(20), "parametric");
        let kids = g.children_of(eid(10));
        assert_eq!(kids.len(), 2);
        assert!(kids.contains(&eid(1)));
        assert!(kids.contains(&eid(2)));
    }

    #[test]
    fn set_dependencies_replaces_previous_edges() {
        let mut g = DependencyGraph::new()
            .with_edge(eid(1), eid(10), "parametric");
        g.set_dependencies(
            eid(1),
            vec![DependencyOut::new(eid(20), "parametric")],
        );
        assert_eq!(g.parents_of(eid(1)).len(), 1);
        assert_eq!(g.parents_of(eid(1))[0].on, eid(20));
        assert!(g.children_of(eid(10)).is_empty());
        assert_eq!(g.children_of(eid(20)), &[eid(1)]);
    }

    #[test]
    fn node_count_includes_all_nodes() {
        let g = DependencyGraph::new()
            .with_edge(eid(1), eid(2), "p")
            .with_edge(eid(2), eid(3), "p");
        assert_eq!(g.node_count(), 3);
    }

    #[test]
    fn bounded_descendant_walk_visits_transitive_descendants() {
        // 1 ← 2, 2 ← 3, 2 ← 4 (3 and 4 depend on 2; 2 depends on 1)
        let g = DependencyGraph::new()
            .with_edge(eid(2), eid(1), "p")
            .with_edge(eid(3), eid(2), "p")
            .with_edge(eid(4), eid(2), "p");
        let kids = g.bounded_descendant_walk(eid(1), 8);
        assert_eq!(kids.len(), 3);
        assert!(kids.contains(&eid(2)));
        assert!(kids.contains(&eid(3)));
        assert!(kids.contains(&eid(4)));
    }

    #[test]
    fn bounded_descendant_walk_respects_max_depth() {
        // Chain: 1 ← 2 ← 3 ← 4
        let g = DependencyGraph::new()
            .with_edge(eid(2), eid(1), "p")
            .with_edge(eid(3), eid(2), "p")
            .with_edge(eid(4), eid(3), "p");
        let depth1 = g.bounded_descendant_walk(eid(1), 1);
        assert_eq!(depth1, vec![eid(2)]);
        let depth2 = g.bounded_descendant_walk(eid(1), 2);
        assert_eq!(depth2, vec![eid(2), eid(3)]);
    }

    // ── Cycle detection ────────────────────────────────────────

    #[test]
    fn would_create_cycle_self_edge_is_cycle() {
        let g = DependencyGraph::new();
        assert!(g.would_create_cycle(eid(1), eid(1)));
    }

    #[test]
    fn would_create_cycle_detects_indirect_cycle() {
        // Existing: 1 → 2 → 3. Adding 3 → 1 closes a cycle.
        let g = DependencyGraph::new()
            .with_edge(eid(1), eid(2), "p")
            .with_edge(eid(2), eid(3), "p");
        assert!(g.would_create_cycle(eid(3), eid(1)));
    }

    #[test]
    fn would_create_cycle_returns_false_for_orthogonal_edges() {
        let g = DependencyGraph::new()
            .with_edge(eid(1), eid(2), "p")
            .with_edge(eid(3), eid(4), "p");
        assert!(!g.would_create_cycle(eid(1), eid(3)));
        assert!(!g.would_create_cycle(eid(5), eid(1)));
    }

    #[test]
    fn would_create_cycle_returns_false_for_extending_chain() {
        // Existing 1 → 2 → 3. Adding 3 → 4 (4 a fresh node) is fine.
        let g = DependencyGraph::new()
            .with_edge(eid(1), eid(2), "p")
            .with_edge(eid(2), eid(3), "p");
        assert!(!g.would_create_cycle(eid(3), eid(4)));
    }

    // ── Topological order ──────────────────────────────────────

    #[test]
    fn topological_order_emits_dependencies_before_dependents() {
        // 1 depends on 2, 2 depends on 3 → topo: 3, 2, 1
        let g = DependencyGraph::new()
            .with_edge(eid(1), eid(2), "p")
            .with_edge(eid(2), eid(3), "p");
        let order = g.topological_order().unwrap();
        let pos = |e: ElementId| order.iter().position(|x| *x == e).unwrap();
        assert!(pos(eid(3)) < pos(eid(2)));
        assert!(pos(eid(2)) < pos(eid(1)));
    }

    #[test]
    fn topological_order_handles_diamond() {
        // 1 → {2, 3} → 4 (1 depends on 2 and 3; 2 and 3 depend on 4)
        let g = DependencyGraph::new()
            .with_edge(eid(1), eid(2), "p")
            .with_edge(eid(1), eid(3), "p")
            .with_edge(eid(2), eid(4), "p")
            .with_edge(eid(3), eid(4), "p");
        let order = g.topological_order().unwrap();
        let pos = |e: ElementId| order.iter().position(|x| *x == e).unwrap();
        assert!(pos(eid(4)) < pos(eid(2)));
        assert!(pos(eid(4)) < pos(eid(3)));
        assert!(pos(eid(2)) < pos(eid(1)));
        assert!(pos(eid(3)) < pos(eid(1)));
    }

    #[test]
    fn topological_order_rejects_cycle() {
        // Direct cycle 1 → 2 → 1.
        let g = DependencyGraph::new()
            .with_edge(eid(1), eid(2), "p")
            .with_edge(eid(2), eid(1), "p");
        let err = g.topological_order().unwrap_err();
        assert_eq!(err, DependencyGraphError::CycleDetected);
    }

    #[test]
    fn topological_order_handles_disconnected_components() {
        let g = DependencyGraph::new()
            .with_edge(eid(1), eid(2), "p")
            .with_edge(eid(10), eid(20), "p");
        let order = g.topological_order().unwrap();
        let pos = |e: ElementId| order.iter().position(|x| *x == e).unwrap();
        assert!(pos(eid(2)) < pos(eid(1)));
        assert!(pos(eid(20)) < pos(eid(10)));
    }

    #[test]
    fn topological_order_empty_graph_is_empty() {
        let g = DependencyGraph::new();
        let order = g.topological_order().unwrap();
        assert!(order.is_empty());
    }

    // ── Plugin ─────────────────────────────────────────────────

    #[test]
    fn plugin_can_be_added_without_panic() {
        let mut app = App::new();
        app.add_plugins(DependencyGraphPlugin);
        app.update();
    }

    // ── Display ────────────────────────────────────────────────

    #[test]
    fn cycle_error_display_mentions_cycle() {
        let err = DependencyGraphError::CycleDetected;
        let display = format!("{err}");
        assert!(display.contains("cycle"));
    }
}
