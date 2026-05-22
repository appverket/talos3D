//! Runtime dependency graph for the relational & parametric substrate (PP-RPS-1).
//!
//! Per ADR-007 (Entity Relationships and Dependency Propagation) and the
//! `RELATIONAL_PARAMETRIC_SUBSTRATE_AGREEMENT.md`: one typed dependency graph
//! with a single dirty scheduler. This module owns the generic mechanism only —
//! no domain nouns, no evaluation logic. Later proof points (scalar evaluator,
//! Definition interior, AuthoringScript propagation) drive *this* engine; they
//! must not reimplement their own scheduler.
//!
//! Node identity is **typed and hierarchical**: every node carries the kind of
//! thing it is and (for component-internal nodes) the owning component, so a
//! component-local sub-DAG is addressable from the global graph without a flat
//! global parameter namespace.
//!
//! The "depends on" relation is a DAG: `a` depends on `b` means `b` must be
//! evaluated before `a`. Cycles are rejected at edge-insertion time with an
//! explainable path. Propagation is serial and deterministic here; PP-RPS-8
//! adds the parallel wavefront over the same ordering.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

/// Typed dependency-graph node identity (agreement: typed/hierarchical IDs).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeId {
    /// A top-level authored entity.
    Entity { entity: u64 },
    /// A driver/derived parameter inside a component.
    ComponentParam { component: u64, name: String },
    /// A step in a component's evaluation script (`AuthoringScript`).
    ScriptStep { component: u64, step: String },
    /// A derived part produced by a component's evaluation.
    DerivedPart { component: u64, part: String },
}

impl NodeId {
    pub fn entity(id: u64) -> Self {
        NodeId::Entity { entity: id }
    }
    pub fn param(component: u64, name: impl Into<String>) -> Self {
        NodeId::ComponentParam {
            component,
            name: name.into(),
        }
    }
    pub fn step(component: u64, step: impl Into<String>) -> Self {
        NodeId::ScriptStep {
            component,
            step: step.into(),
        }
    }
    pub fn part(component: u64, part: impl Into<String>) -> Self {
        NodeId::DerivedPart {
            component,
            part: part.into(),
        }
    }

    /// The owning component for component-internal nodes; `None` for entities.
    pub fn component(&self) -> Option<u64> {
        match self {
            NodeId::Entity { .. } => None,
            NodeId::ComponentParam { component, .. }
            | NodeId::ScriptStep { component, .. }
            | NodeId::DerivedPart { component, .. } => Some(*component),
        }
    }
}

/// An explainable cycle finding: the dependency path that would close a loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleError {
    /// `path[0]` depends on `path[1]` depends on … and the proposed edge would
    /// close the loop back to `path[0]`.
    pub path: Vec<NodeId>,
}

impl std::fmt::Display for CycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dependency cycle: ")?;
        for (i, n) in self.path.iter().enumerate() {
            if i > 0 {
                write!(f, " -> ")?;
            }
            write!(f, "{n:?}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CycleError {}

/// The generic runtime dependency graph + dirty scheduler.
///
/// Stores the "depends on" relation both forward (`deps`: node → its inputs)
/// and reverse (`dependents`: node → nodes that consume it), so dirty marking
/// walks dependents in O(affected) and propagation orders inputs-first.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(into = "GraphPersist", from = "GraphPersist")]
pub struct DependencyGraph {
    nodes: BTreeSet<NodeId>,
    /// node → the nodes it depends on (its inputs).
    deps: BTreeMap<NodeId, BTreeSet<NodeId>>,
    /// node → the nodes that depend on it (its consumers).
    dependents: BTreeMap<NodeId, BTreeSet<NodeId>>,
    /// Transient: nodes needing recomputation. Not persisted.
    dirty: BTreeSet<NodeId>,
}

/// On-disk shape. `NodeId` is an enum, so it cannot be a JSON map key; we
/// persist nodes + `(dependent, dependency)` edge pairs instead. The transient
/// dirty set is intentionally not persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
struct GraphPersist {
    nodes: Vec<NodeId>,
    edges: Vec<(NodeId, NodeId)>,
}

impl From<DependencyGraph> for GraphPersist {
    fn from(g: DependencyGraph) -> Self {
        let mut edges = Vec::new();
        for (dependent, deps) in &g.deps {
            for dependency in deps {
                edges.push((dependent.clone(), dependency.clone()));
            }
        }
        GraphPersist {
            nodes: g.nodes.into_iter().collect(),
            edges,
        }
    }
}

impl From<GraphPersist> for DependencyGraph {
    fn from(p: GraphPersist) -> Self {
        let mut g = DependencyGraph {
            nodes: p.nodes.into_iter().collect(),
            deps: BTreeMap::new(),
            dependents: BTreeMap::new(),
            dirty: BTreeSet::new(),
        };
        for (dependent, dependency) in p.edges {
            g.nodes.insert(dependent.clone());
            g.nodes.insert(dependency.clone());
            g.deps
                .entry(dependent.clone())
                .or_default()
                .insert(dependency.clone());
            g.dependents
                .entry(dependency)
                .or_default()
                .insert(dependent);
        }
        g
    }
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn contains(&self, node: &NodeId) -> bool {
        self.nodes.contains(node)
    }

    pub fn add_node(&mut self, node: NodeId) {
        self.nodes.insert(node);
    }

    /// Record that `dependent` depends on `dependency` (dependency evaluated
    /// first). Both nodes are inserted if absent. Rejects edges that would
    /// create a cycle, returning the offending path.
    pub fn add_dependency(
        &mut self,
        dependent: NodeId,
        dependency: NodeId,
    ) -> Result<(), CycleError> {
        if dependent == dependency {
            return Err(CycleError {
                path: vec![dependent],
            });
        }
        // A cycle forms iff `dependency` already (transitively) depends on
        // `dependent` — i.e. `dependent` is reachable from `dependency` along
        // existing `deps` edges.
        if let Some(path) = self.reachable_via_deps(&dependency, &dependent) {
            return Err(CycleError { path });
        }
        self.nodes.insert(dependent.clone());
        self.nodes.insert(dependency.clone());
        self.deps
            .entry(dependent.clone())
            .or_default()
            .insert(dependency.clone());
        self.dependents
            .entry(dependency)
            .or_default()
            .insert(dependent);
        Ok(())
    }

    /// Inputs of `node` (nodes it depends on).
    pub fn dependencies_of(&self, node: &NodeId) -> Vec<NodeId> {
        self.deps
            .get(node)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Consumers of `node` (nodes that depend on it).
    pub fn dependents_of(&self, node: &NodeId) -> Vec<NodeId> {
        self.dependents
            .get(node)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// All nodes belonging to one component's local sub-DAG.
    pub fn nodes_in_component(&self, component: u64) -> Vec<NodeId> {
        self.nodes
            .iter()
            .filter(|n| n.component() == Some(component))
            .cloned()
            .collect()
    }

    /// Mark `node` and **all its transitive dependents** dirty. A driver edit
    /// dirties only what consumes it — never its inputs.
    pub fn mark_dirty(&mut self, node: NodeId) {
        if !self.nodes.contains(&node) {
            self.nodes.insert(node.clone());
        }
        let mut queue = VecDeque::new();
        queue.push_back(node);
        while let Some(n) = queue.pop_front() {
            if !self.dirty.insert(n.clone()) {
                continue; // already visited
            }
            if let Some(consumers) = self.dependents.get(&n) {
                for c in consumers {
                    if !self.dirty.contains(c) {
                        queue.push_back(c.clone());
                    }
                }
            }
        }
    }

    pub fn dirty_nodes(&self) -> Vec<NodeId> {
        self.dirty.iter().cloned().collect()
    }

    pub fn is_dirty(&self, node: &NodeId) -> bool {
        self.dirty.contains(node)
    }

    /// Return the dirty nodes in deterministic topological order (each node
    /// after every dirty node it depends on) and clear the dirty set. This is
    /// the order in which the affected subgraph must be recomputed.
    ///
    /// Only dirty→dirty edges constrain ordering; non-dirty inputs are already
    /// current. Determinism: ties are broken by `NodeId` ordering.
    pub fn take_dirty_topological(&mut self) -> Result<Vec<NodeId>, CycleError> {
        let dirty: BTreeSet<NodeId> = std::mem::take(&mut self.dirty);
        if dirty.is_empty() {
            return Ok(Vec::new());
        }
        // In-degree within the dirty subgraph (count of dirty inputs).
        let mut indegree: BTreeMap<NodeId, usize> = BTreeMap::new();
        for n in &dirty {
            let d = self
                .deps
                .get(n)
                .map(|s| s.iter().filter(|x| dirty.contains(*x)).count())
                .unwrap_or(0);
            indegree.insert(n.clone(), d);
        }
        // Kahn's algorithm with a sorted ready set for deterministic output.
        let mut ready: BTreeSet<NodeId> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(n, _)| n.clone())
            .collect();
        let mut order = Vec::with_capacity(dirty.len());
        while let Some(n) = ready.iter().next().cloned() {
            ready.remove(&n);
            order.push(n.clone());
            if let Some(consumers) = self.dependents.get(&n) {
                for c in consumers {
                    if let Some(d) = indegree.get_mut(c) {
                        *d -= 1;
                        if *d == 0 {
                            ready.insert(c.clone());
                        }
                    }
                }
            }
        }
        if order.len() != dirty.len() {
            // Should be impossible (edges are cycle-checked at insertion), but
            // surface a finding rather than silently dropping nodes.
            let remaining: Vec<NodeId> = dirty.into_iter().filter(|n| !order.contains(n)).collect();
            return Err(CycleError { path: remaining });
        }
        Ok(order)
    }

    /// Drive a serial recompute of the dirty subgraph: call `evaluate` on each
    /// dirty node in topological order, returning the order visited. The
    /// engine owns *ordering*; evaluation content is supplied by callers
    /// (scalar evaluator / script replay in later PPs).
    pub fn propagate<F>(&mut self, mut evaluate: F) -> Result<Vec<NodeId>, CycleError>
    where
        F: FnMut(&NodeId),
    {
        let order = self.take_dirty_topological()?;
        for n in &order {
            evaluate(n);
        }
        Ok(order)
    }

    // --- internals --------------------------------------------------------

    /// Is `target` reachable from `start` by following `deps` (inputs)?
    /// Returns the path `[start, …, target]` if so.
    fn reachable_via_deps(&self, start: &NodeId, target: &NodeId) -> Option<Vec<NodeId>> {
        if start == target {
            return Some(vec![start.clone()]);
        }
        let mut stack: Vec<(NodeId, Vec<NodeId>)> = vec![(start.clone(), vec![start.clone()])];
        let mut seen: BTreeSet<NodeId> = BTreeSet::new();
        while let Some((n, path)) = stack.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            if let Some(inputs) = self.deps.get(&n) {
                for inp in inputs {
                    let mut p = path.clone();
                    p.push(inp.clone());
                    if inp == target {
                        return Some(p);
                    }
                    stack.push((inp.clone(), p));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(c: u64, n: &str) -> NodeId {
        NodeId::param(c, n)
    }

    #[test]
    fn add_dependency_and_query() {
        let mut g = DependencyGraph::new();
        // apex depends on span and pitch
        g.add_dependency(p(1, "apex"), p(1, "span")).unwrap();
        g.add_dependency(p(1, "apex"), p(1, "pitch")).unwrap();
        assert_eq!(g.node_count(), 3);
        assert_eq!(g.dependencies_of(&p(1, "apex")).len(), 2);
        assert_eq!(g.dependents_of(&p(1, "span")), vec![p(1, "apex")]);
    }

    #[test]
    fn dirty_marks_only_transitive_dependents() {
        let mut g = DependencyGraph::new();
        // span -> apex -> ridge_height ; pitch -> apex
        g.add_dependency(p(1, "apex"), p(1, "span")).unwrap();
        g.add_dependency(p(1, "apex"), p(1, "pitch")).unwrap();
        g.add_dependency(p(1, "ridge_height"), p(1, "apex"))
            .unwrap();
        g.mark_dirty(p(1, "span"));
        let d: BTreeSet<NodeId> = g.dirty_nodes().into_iter().collect();
        // span + its transitive dependents apex, ridge_height — NOT pitch.
        assert!(d.contains(&p(1, "span")));
        assert!(d.contains(&p(1, "apex")));
        assert!(d.contains(&p(1, "ridge_height")));
        assert!(!d.contains(&p(1, "pitch")));
    }

    #[test]
    fn topological_propagation_orders_inputs_first() {
        let mut g = DependencyGraph::new();
        g.add_dependency(p(1, "apex"), p(1, "span")).unwrap();
        g.add_dependency(p(1, "ridge_height"), p(1, "apex"))
            .unwrap();
        g.mark_dirty(p(1, "span"));
        let order = g.propagate(|_| {}).unwrap();
        let pos = |n: &NodeId| order.iter().position(|x| x == n).unwrap();
        assert!(pos(&p(1, "span")) < pos(&p(1, "apex")));
        assert!(pos(&p(1, "apex")) < pos(&p(1, "ridge_height")));
        // dirty cleared after propagation
        assert!(g.dirty_nodes().is_empty());
    }

    #[test]
    fn no_op_edit_propagates_nothing() {
        let mut g = DependencyGraph::new();
        g.add_dependency(p(1, "apex"), p(1, "span")).unwrap();
        // no mark_dirty
        let order = g.propagate(|_| {}).unwrap();
        assert!(order.is_empty());
    }

    #[test]
    fn cycle_is_rejected_with_path() {
        let mut g = DependencyGraph::new();
        g.add_dependency(p(1, "b"), p(1, "a")).unwrap(); // b depends on a
        g.add_dependency(p(1, "c"), p(1, "b")).unwrap(); // c depends on b
                                                         // a depends on c would close a -> c -> b -> a
        let err = g.add_dependency(p(1, "a"), p(1, "c")).unwrap_err();
        assert!(err.path.first() == Some(&p(1, "c")));
        assert!(err.path.last() == Some(&p(1, "a")));
        // graph unchanged: a has no deps
        assert!(g.dependencies_of(&p(1, "a")).is_empty());
    }

    #[test]
    fn self_dependency_is_rejected() {
        let mut g = DependencyGraph::new();
        let err = g.add_dependency(p(1, "x"), p(1, "x")).unwrap_err();
        assert_eq!(err.path, vec![p(1, "x")]);
    }

    #[test]
    fn component_local_subdag_addressable() {
        let mut g = DependencyGraph::new();
        g.add_dependency(p(1, "apex"), p(1, "span")).unwrap();
        g.add_dependency(p(2, "sash"), p(2, "width")).unwrap();
        g.add_node(NodeId::entity(99));
        let c1 = g.nodes_in_component(1);
        assert_eq!(c1.len(), 2);
        assert!(c1.iter().all(|n| n.component() == Some(1)));
        assert_eq!(g.nodes_in_component(2).len(), 2);
        assert_eq!(NodeId::entity(99).component(), None);
    }

    #[test]
    fn serde_round_trip_preserves_nodes_and_edges() {
        let mut g = DependencyGraph::new();
        g.add_dependency(p(1, "apex"), p(1, "span")).unwrap();
        g.add_dependency(p(1, "apex"), p(1, "pitch")).unwrap();
        g.add_dependency(p(1, "ridge"), p(1, "apex")).unwrap();
        g.mark_dirty(p(1, "span")); // dirty is transient, must not persist
        let json = serde_json::to_string(&g).unwrap();
        let g2: DependencyGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(g2.node_count(), g.node_count());
        assert_eq!(g2.dependencies_of(&p(1, "apex")).len(), 2);
        assert_eq!(g2.dependents_of(&p(1, "apex")), vec![p(1, "ridge")]);
        assert!(g2.dirty_nodes().is_empty(), "dirty must not round-trip");
    }

    #[test]
    fn deterministic_order_is_stable() {
        // Build the same graph twice, propagate, expect identical order.
        let build = || {
            let mut g = DependencyGraph::new();
            for i in 0..8u64 {
                g.add_dependency(p(1, &format!("d{i}")), p(1, "root"))
                    .unwrap();
            }
            g.mark_dirty(p(1, "root"));
            g.propagate(|_| {}).unwrap()
        };
        assert_eq!(build(), build());
    }
}
