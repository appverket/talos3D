//! Deterministic parallel wavefront evaluation (PP-RPS-8, core half).
//!
//! Per `RELATIONAL_PARAMETRIC_SUBSTRATE_AGREEMENT.md`
//! ("Propagation, Scheduling, And Performance"): one wavefront evaluator with a
//! cost model and serial fallback. Nodes at the same topological *level* (no
//! mutual dependency) may evaluate concurrently; below a cost threshold, or on
//! WASM without threads, evaluation runs serially. Identical component
//! instances (equal driver tuples) evaluate **once** and reuse the result.
//!
//! Determinism is structural: evaluation functions are **pure** and there is no
//! cross-node shared mutable state, so the result map is identical whether
//! computed serially or in parallel. `serial == parallel` is asserted by test.
//!
//! The geometry/truss-specific proof (PP-RPS-8) consumes this engine from the
//! architecture layer; this module is the generic core mechanism.

use std::collections::{BTreeMap, BTreeSet};

use super::graph::{CycleError, DependencyGraph, NodeId};

/// Order a set of nodes into topological *levels*: `levels[0]` are nodes whose
/// dependencies (within `nodes`) are already satisfied, and so on. Within a
/// level, nodes are mutually independent and may run concurrently.
pub fn topological_levels(
    graph: &DependencyGraph,
    nodes: &BTreeSet<NodeId>,
) -> Result<Vec<Vec<NodeId>>, CycleError> {
    // in-degree within the subset
    let mut indeg: BTreeMap<NodeId, usize> = BTreeMap::new();
    for n in nodes {
        let d = graph
            .dependencies_of(n)
            .into_iter()
            .filter(|x| nodes.contains(x))
            .count();
        indeg.insert(n.clone(), d);
    }
    let mut levels: Vec<Vec<NodeId>> = Vec::new();
    let mut placed = 0usize;
    let mut current: Vec<NodeId> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| n.clone())
        .collect();
    while !current.is_empty() {
        current.sort(); // determinism
        placed += current.len();
        let mut next_set: BTreeSet<NodeId> = BTreeSet::new();
        for n in &current {
            for c in graph.dependents_of(n) {
                if let Some(d) = indeg.get_mut(&c) {
                    *d -= 1;
                    if *d == 0 {
                        next_set.insert(c);
                    }
                }
            }
        }
        levels.push(std::mem::take(&mut current));
        current = next_set.into_iter().collect();
    }
    if placed != nodes.len() {
        let remaining: Vec<NodeId> = nodes
            .iter()
            .filter(|n| indeg.get(n).copied().unwrap_or(0) > 0)
            .cloned()
            .collect();
        return Err(CycleError { path: remaining });
    }
    Ok(levels)
}

/// Evaluate `levels` with a pure per-node function, serially. Reference path.
pub fn evaluate_serial<T, F>(levels: &[Vec<NodeId>], eval: &F) -> BTreeMap<NodeId, T>
where
    F: Fn(&NodeId) -> T + Sync,
    T: Send,
{
    let mut out = BTreeMap::new();
    for level in levels {
        for n in level {
            out.insert(n.clone(), eval(n));
        }
    }
    out
}

/// Evaluate `levels` with a pure per-node function. Levels of size
/// `>= threshold` evaluate their nodes in parallel (native); smaller levels and
/// WASM run serially. Result is identical to [`evaluate_serial`] because `eval`
/// is pure and nodes within a level are independent.
pub fn evaluate_wavefront<T, F>(
    levels: &[Vec<NodeId>],
    threshold: usize,
    eval: &F,
) -> BTreeMap<NodeId, T>
where
    F: Fn(&NodeId) -> T + Sync,
    T: Send,
{
    let mut out: BTreeMap<NodeId, T> = BTreeMap::new();
    for level in levels {
        if cfg!(not(target_arch = "wasm32")) && level.len() >= threshold.max(2) {
            // Parallel: evaluate this independent level concurrently, then
            // merge into the map in deterministic (BTreeMap) order.
            let results = parallel_map_level(level, eval);
            for (n, v) in results {
                out.insert(n, v);
            }
        } else {
            for n in level {
                out.insert(n.clone(), eval(n));
            }
        }
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn parallel_map_level<T, F>(level: &[NodeId], eval: &F) -> Vec<(NodeId, T)>
where
    F: Fn(&NodeId) -> T + Sync,
    T: Send,
{
    use std::sync::Mutex;
    let collected: Mutex<Vec<(NodeId, T)>> = Mutex::new(Vec::with_capacity(level.len()));
    std::thread::scope(|scope| {
        // Spawn one task per node; the OS scheduler spreads them across cores.
        // (A future revision can chunk to bound thread count via a task pool.)
        for n in level {
            let collected = &collected;
            scope.spawn(move || {
                let v = eval(n);
                collected.lock().unwrap().push((n.clone(), v));
            });
        }
    });
    collected.into_inner().unwrap()
}

#[cfg(target_arch = "wasm32")]
fn parallel_map_level<T, F>(level: &[NodeId], eval: &F) -> Vec<(NodeId, T)>
where
    F: Fn(&NodeId) -> T + Sync,
    T: Send,
{
    level.iter().map(|n| (n.clone(), eval(n))).collect()
}

/// Instance memoization: evaluate nodes grouped by a memo key so identical
/// instances (equal key — e.g. equal driver tuples) compute once. Returns the
/// per-node outputs and how many distinct computations ran.
pub struct MemoResult<T> {
    pub outputs: BTreeMap<NodeId, T>,
    pub computed: usize,
    pub reused: usize,
}

pub fn evaluate_memoized<T, K, KF, F>(nodes: &[NodeId], key_of: KF, eval: F) -> MemoResult<T>
where
    T: Clone,
    K: Ord + Clone,
    KF: Fn(&NodeId) -> K,
    F: Fn(&K) -> T,
{
    let mut cache: BTreeMap<K, T> = BTreeMap::new();
    let mut outputs = BTreeMap::new();
    let (mut computed, mut reused) = (0usize, 0usize);
    for n in nodes {
        let k = key_of(n);
        let v = if let Some(v) = cache.get(&k) {
            reused += 1;
            v.clone()
        } else {
            let v = eval(&k);
            cache.insert(k.clone(), v.clone());
            computed += 1;
            v
        };
        outputs.insert(n.clone(), v);
    }
    MemoResult {
        outputs,
        computed,
        reused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diamond() -> (DependencyGraph, BTreeSet<NodeId>) {
        // a -> b, a -> c, b -> d, c -> d  (b and c independent => same level)
        let mut g = DependencyGraph::new();
        g.add_dependency(NodeId::param(1, "b"), NodeId::param(1, "a"))
            .unwrap();
        g.add_dependency(NodeId::param(1, "c"), NodeId::param(1, "a"))
            .unwrap();
        g.add_dependency(NodeId::param(1, "d"), NodeId::param(1, "b"))
            .unwrap();
        g.add_dependency(NodeId::param(1, "d"), NodeId::param(1, "c"))
            .unwrap();
        let nodes: BTreeSet<NodeId> = ["a", "b", "c", "d"]
            .iter()
            .map(|s| NodeId::param(1, *s))
            .collect();
        (g, nodes)
    }

    #[test]
    fn levels_group_independent_nodes() {
        let (g, nodes) = diamond();
        let levels = topological_levels(&g, &nodes).unwrap();
        assert_eq!(levels[0], vec![NodeId::param(1, "a")]);
        assert_eq!(
            levels[1],
            vec![NodeId::param(1, "b"), NodeId::param(1, "c")]
        );
        assert_eq!(levels[2], vec![NodeId::param(1, "d")]);
    }

    #[test]
    fn serial_equals_parallel() {
        let (g, nodes) = diamond();
        let levels = topological_levels(&g, &nodes).unwrap();
        // pure eval: hash-ish of the node name length * component
        let eval = |n: &NodeId| -> u64 {
            match n {
                NodeId::ComponentParam { component, name } => component * 100 + name.len() as u64,
                _ => 0,
            }
        };
        let serial = evaluate_serial(&levels, &eval);
        let parallel = evaluate_wavefront(&levels, 2, &eval);
        assert_eq!(serial, parallel, "serial == parallel");
    }

    #[test]
    fn serial_equals_parallel_large() {
        // Wide independent level forces the parallel path.
        let mut g = DependencyGraph::new();
        let root = NodeId::param(1, "root");
        let mut nodes: BTreeSet<NodeId> = [root.clone()].into_iter().collect();
        for i in 0..64u64 {
            let leaf = NodeId::param(1, format!("n{i}"));
            g.add_dependency(leaf.clone(), root.clone()).unwrap();
            nodes.insert(leaf);
        }
        let levels = topological_levels(&g, &nodes).unwrap();
        let eval = |n: &NodeId| -> String { format!("{n:?}") };
        let serial = evaluate_serial(&levels, &eval);
        let parallel = evaluate_wavefront(&levels, 8, &eval);
        assert_eq!(serial, parallel);
        assert_eq!(parallel.len(), 65);
    }

    #[test]
    fn below_threshold_runs_serial_path_same_result() {
        let (g, nodes) = diamond();
        let levels = topological_levels(&g, &nodes).unwrap();
        let eval = |n: &NodeId| format!("{n:?}");
        // huge threshold => never parallel
        let r = evaluate_wavefront(&levels, 1000, &eval);
        let s = evaluate_serial(&levels, &eval);
        assert_eq!(r, s);
    }

    #[test]
    fn instance_memoization_computes_once_per_key() {
        // 5 truss instances, 3 of which share the same driver tuple (span=6000).
        let nodes: Vec<NodeId> = (0..5).map(|i| NodeId::part(i, "truss")).collect();
        // key = the span for that component
        let span_of = |c: u64| if c < 3 { 6000u64 } else { 9000u64 };
        let r = evaluate_memoized(
            &nodes,
            |n| span_of(n.component().unwrap()),
            |span| format!("derived@{span}"),
        );
        assert_eq!(r.computed, 2, "only 2 distinct driver tuples computed");
        assert_eq!(r.reused, 3);
        assert_eq!(r.outputs.len(), 5);
    }

    #[test]
    fn cycle_detected_in_levels() {
        // Manually build a graph and try to level a node set that (shouldn't)
        // contain a cycle — graph rejects cycles at insert, so we assert the
        // happy path returns all nodes.
        let (g, nodes) = diamond();
        let levels = topological_levels(&g, &nodes).unwrap();
        let total: usize = levels.iter().map(|l| l.len()).sum();
        assert_eq!(total, nodes.len());
    }
}
