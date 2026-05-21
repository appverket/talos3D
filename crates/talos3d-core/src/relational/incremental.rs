//! Incremental `AuthoringScript` propagation (PP-RPS-4).
//!
//! Per `RELATIONAL_PARAMETRIC_SUBSTRATE_AGREEMENT.md`: the `AuthoringScript` is
//! already the per-component evaluation DAG (its `ArgExpr::Param` and
//! `ArgExpr::StepOutput` references are the edges). This module lifts that
//! latent provenance into the [`DependencyGraph`] (PP-RPS-1), and turns full
//! deterministic replay into **dirty-driven partial propagation**:
//!
//! - a driver edit recomputes only the affected steps (transitive dependents),
//! - clean steps reuse their memoized outputs,
//! - evaluation is a pure function over resolved inputs (no live `World`),
//!   so apply stays a separate serial concern (PP-RPS-7),
//! - and **incremental output equals full-replay output** for the same inputs.
//!
//! The evaluation of a single step is supplied by the caller as a pure
//! `StepEvaluator` (`(tool, resolved_args) -> output`), so this module owns
//! ordering + memoization, not domain content.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use super::graph::{DependencyGraph, NodeId};
use crate::curation::authoring_script::{ArgExpr, AuthoringScript, Predicate, StepId};

/// A pure step evaluation: given the tool id and the resolved argument object,
/// produce the step's output object. Must be deterministic.
pub type StepEvaluator<'a> = dyn FnMut(&str, &Map<String, Value>) -> Map<String, Value> + 'a;

/// Lift a script's `ArgExpr` provenance into explicit dependency edges:
/// `ComponentParam(name) -> ScriptStep(step)` for `Param`, and
/// `ScriptStep(src) -> ScriptStep(step)` for `StepOutput`. Returns the graph
/// (steps depend on their inputs).
pub fn build_provenance(component: u64, script: &AuthoringScript) -> DependencyGraph {
    let mut g = DependencyGraph::new();
    for step in &script.steps {
        let step_node = NodeId::step(component, step.id.as_str());
        g.add_node(step_node.clone());
        for expr in step.args.values() {
            add_expr_edges(component, expr, &step_node, &mut g);
        }
        if let Some(pred) = &step.precondition {
            add_predicate_edges(component, pred, &step_node, &mut g);
        }
    }
    g
}

fn add_expr_edges(component: u64, expr: &ArgExpr, step_node: &NodeId, g: &mut DependencyGraph) {
    match expr {
        ArgExpr::Param { name } => {
            // step depends on the driver/param
            let _ = g.add_dependency(step_node.clone(), NodeId::param(component, name.clone()));
        }
        ArgExpr::StepOutput { step_id, .. } => {
            let _ = g.add_dependency(
                step_node.clone(),
                NodeId::step(component, step_id.as_str()),
            );
        }
        ArgExpr::Literal { .. } | ArgExpr::ClaimRef { .. } => {}
    }
}

fn add_predicate_edges(component: u64, pred: &Predicate, step_node: &NodeId, g: &mut DependencyGraph) {
    match pred {
        Predicate::Equals { lhs, rhs } => {
            add_expr_edges(component, lhs, step_node, g);
            add_expr_edges(component, rhs, step_node, g);
        }
        Predicate::Defined { expr } => add_expr_edges(component, expr, step_node, g),
        Predicate::And { children } | Predicate::Or { children } => {
            for c in children {
                add_predicate_edges(component, c, step_node, g);
            }
        }
        Predicate::Not { child } => add_predicate_edges(component, child, step_node, g),
    }
}

/// Which params (drivers) a step reads directly.
fn step_param_inputs(step: &crate::curation::authoring_script::Step) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for expr in step.args.values() {
        if let ArgExpr::Param { name } = expr {
            out.insert(name.clone());
        }
    }
    out
}

/// Resolve one `ArgExpr` against params + prior step outputs. Returns `None`
/// for unresolved claim refs (pure context).
fn resolve(
    expr: &ArgExpr,
    params: &Map<String, Value>,
    outputs: &BTreeMap<StepId, Map<String, Value>>,
) -> Option<Value> {
    match expr {
        ArgExpr::Literal { value } => Some(value.clone()),
        ArgExpr::Param { name } => params.get(name).cloned(),
        ArgExpr::StepOutput { step_id, path } => {
            let out = outputs.get(step_id)?;
            let key = path.as_str().trim_start_matches("$.").trim_start_matches('$');
            if key.is_empty() {
                Some(Value::Object(out.clone()))
            } else {
                out.get(key).cloned()
            }
        }
        ArgExpr::ClaimRef { .. } => None,
    }
}

fn resolve_args(
    step: &crate::curation::authoring_script::Step,
    params: &Map<String, Value>,
    outputs: &BTreeMap<StepId, Map<String, Value>>,
) -> Map<String, Value> {
    let mut m = Map::new();
    for (k, expr) in &step.args {
        if let Some(v) = resolve(expr, params, outputs) {
            m.insert(k.clone(), v);
        }
    }
    m
}

/// Full deterministic evaluation: run every step in order. This is the
/// reference path (also used for first build / export).
pub fn full_eval(
    script: &AuthoringScript,
    params: &Map<String, Value>,
    eval: &mut StepEvaluator<'_>,
) -> BTreeMap<StepId, Map<String, Value>> {
    let mut outputs: BTreeMap<StepId, Map<String, Value>> = BTreeMap::new();
    for step in &script.steps {
        let args = resolve_args(step, params, &outputs);
        let out = eval(step.tool.as_str(), &args);
        outputs.insert(step.id.clone(), out);
    }
    outputs
}

/// Report from an incremental evaluation: the new outputs and which steps were
/// actually recomputed (the rest reused their memoized output).
#[derive(Debug, Clone, PartialEq)]
pub struct IncrementalReport {
    pub outputs: BTreeMap<StepId, Map<String, Value>>,
    pub recomputed: Vec<StepId>,
}

/// Incremental evaluation: given the previous outputs and the set of driver
/// names that changed, recompute only the affected steps (transitive dependents
/// of the changed params), reusing memoized outputs for clean steps. Produces
/// identical outputs to `full_eval` for the same `params`.
pub fn incremental_eval(
    component: u64,
    script: &AuthoringScript,
    params: &Map<String, Value>,
    changed_params: &BTreeSet<String>,
    prev_outputs: &BTreeMap<StepId, Map<String, Value>>,
    eval: &mut StepEvaluator<'_>,
) -> IncrementalReport {
    let mut graph = build_provenance(component, script);

    // Dirty the changed driver params; the graph marks transitive dependents.
    for name in changed_params {
        graph.mark_dirty(NodeId::param(component, name.clone()));
    }
    // Also dirty any step that *appeared* (no prior output) so first build of
    // new steps recomputes.
    for step in &script.steps {
        if !prev_outputs.contains_key(&step.id) {
            graph.mark_dirty(NodeId::step(component, step.id.as_str()));
        }
    }

    let order = graph
        .take_dirty_topological()
        .expect("provenance graph is acyclic by construction");
    let dirty_steps: BTreeSet<StepId> = order
        .iter()
        .filter_map(|n| match n {
            NodeId::ScriptStep { step, .. } => Some(StepId::new(step.clone())),
            _ => None,
        })
        .collect();

    // Walk steps in script order; recompute dirty steps, reuse outputs for the
    // rest. (Script order is a valid topological order of step→step edges.)
    let mut outputs: BTreeMap<StepId, Map<String, Value>> = BTreeMap::new();
    let mut recomputed = Vec::new();
    for step in &script.steps {
        if dirty_steps.contains(&step.id) {
            let args = resolve_args(step, params, &outputs);
            let out = eval(step.tool.as_str(), &args);
            outputs.insert(step.id.clone(), out);
            recomputed.push(step.id.clone());
        } else if let Some(prev) = prev_outputs.get(&step.id) {
            outputs.insert(step.id.clone(), prev.clone());
        } else {
            // Shouldn't happen (new steps were dirtied above), but be safe.
            let args = resolve_args(step, params, &outputs);
            let out = eval(step.tool.as_str(), &args);
            outputs.insert(step.id.clone(), out);
            recomputed.push(step.id.clone());
        }
    }
    let _ = step_param_inputs; // retained for callers/tests
    IncrementalReport {
        outputs,
        recomputed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::authoring_script::{AuthoringScript, McpToolId, MutationScope, OutputPath, Step};

    // Build a small parametric "truss-ish" script:
    //   half = span / 2                 (depends on param `span`)
    //   apex = half * tan_pitch         (depends on step `half`, param `tan_pitch`)
    //   make = box(apex)                (depends on step `apex`)
    fn script() -> AuthoringScript {
        let mut s = AuthoringScript::stub(MutationScope::None);
        s.allowed_tools.insert(McpToolId::new("half"));
        s.allowed_tools.insert(McpToolId::new("apex"));
        s.allowed_tools.insert(McpToolId::new("make"));
        let step = |id: &str, tool: &str, args: Vec<(&str, ArgExpr)>| Step {
            id: StepId::new(id),
            tool: McpToolId::new(tool),
            args: args.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            bindings: Default::default(),
            essential: true,
            precondition: None,
        };
        s.steps.push(step(
            "half",
            "half",
            vec![("span", ArgExpr::Param { name: "span".into() })],
        ));
        s.steps.push(step(
            "apex",
            "apex",
            vec![
                (
                    "half",
                    ArgExpr::StepOutput {
                        step_id: StepId::new("half"),
                        path: OutputPath::new("v"),
                    },
                ),
                ("tan_pitch", ArgExpr::Param { name: "tan_pitch".into() }),
            ],
        ));
        s.steps.push(step(
            "make",
            "make",
            vec![(
                "apex",
                ArgExpr::StepOutput {
                    step_id: StepId::new("apex"),
                    path: OutputPath::new("v"),
                },
            )],
        ));
        s
    }

    // A deterministic evaluator implementing the toy math, counting calls.
    fn make_eval(calls: &mut Vec<String>) -> impl FnMut(&str, &Map<String, Value>) -> Map<String, Value> + '_ {
        move |tool: &str, args: &Map<String, Value>| {
            calls.push(tool.to_string());
            let num = |m: &Map<String, Value>, k: &str| m.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let mut out = Map::new();
            let v = match tool {
                "half" => num(args, "span") / 2.0,
                "apex" => num(args, "half") * num(args, "tan_pitch"),
                "make" => num(args, "apex"), // "geometry" stand-in
                _ => 0.0,
            };
            out.insert("v".into(), Value::from(v));
            out
        }
    }

    fn params(span: f64, tan_pitch: f64) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("span".into(), Value::from(span));
        m.insert("tan_pitch".into(), Value::from(tan_pitch));
        m
    }

    #[test]
    fn provenance_edges_lifted_from_argexpr() {
        let g = build_provenance(1, &script());
        // apex step depends on the half step and the tan_pitch param
        let apex = NodeId::step(1, "apex");
        let deps = g.dependencies_of(&apex);
        assert!(deps.contains(&NodeId::step(1, "half")));
        assert!(deps.contains(&NodeId::param(1, "tan_pitch")));
        // make depends on apex
        assert!(g.dependencies_of(&NodeId::step(1, "make")).contains(&NodeId::step(1, "apex")));
    }

    #[test]
    fn full_eval_computes_chain() {
        let s = script();
        let mut calls = Vec::new();
        let mut e = make_eval(&mut calls);
        let out = full_eval(&s, &params(6.0, 0.5), &mut e);
        assert_eq!(out[&StepId::new("half")]["v"], Value::from(3.0));
        assert_eq!(out[&StepId::new("apex")]["v"], Value::from(1.5));
        assert_eq!(out[&StepId::new("make")]["v"], Value::from(1.5));
    }

    #[test]
    fn incremental_recomputes_only_affected_and_matches_full() {
        let s = script();
        // initial full build at span=6
        let mut c0 = Vec::new();
        let mut e0 = make_eval(&mut c0);
        let base = full_eval(&s, &params(6.0, 0.5), &mut e0);
        drop(e0);

        // change ONLY tan_pitch -> 1.0. `half` does not depend on tan_pitch,
        // so only apex + make should recompute.
        let mut ci = Vec::new();
        let mut ei = make_eval(&mut ci);
        let changed: BTreeSet<String> = ["tan_pitch".to_string()].into_iter().collect();
        let inc = incremental_eval(1, &s, &params(6.0, 1.0), &changed, &base, &mut ei);
        drop(ei);

        // recomputed only apex and make (not half)
        let rc: BTreeSet<String> = inc.recomputed.iter().map(|s| s.as_str().to_string()).collect();
        assert_eq!(rc, ["apex".to_string(), "make".to_string()].into_iter().collect());

        // and the result equals a fresh full replay with the new params
        let mut cf = Vec::new();
        let mut ef = make_eval(&mut cf);
        let full = full_eval(&s, &params(6.0, 1.0), &mut ef);
        assert_eq!(inc.outputs, full, "incremental == full replay");
        assert_eq!(full[&StepId::new("apex")]["v"], Value::from(3.0));
    }

    #[test]
    fn changing_root_driver_recomputes_whole_chain() {
        let s = script();
        let mut c0 = Vec::new();
        let mut e0 = make_eval(&mut c0);
        let base = full_eval(&s, &params(6.0, 0.5), &mut e0);
        drop(e0);

        // change span -> half, apex, make all dirty
        let mut ci = Vec::new();
        let mut ei = make_eval(&mut ci);
        let changed: BTreeSet<String> = ["span".to_string()].into_iter().collect();
        let inc = incremental_eval(1, &s, &params(8.0, 0.5), &changed, &base, &mut ei);
        drop(ei);
        assert_eq!(inc.recomputed.len(), 3);
        assert_eq!(inc.outputs[&StepId::new("half")]["v"], Value::from(4.0));
    }

    #[test]
    fn no_change_recomputes_nothing() {
        let s = script();
        let mut c0 = Vec::new();
        let mut e0 = make_eval(&mut c0);
        let base = full_eval(&s, &params(6.0, 0.5), &mut e0);
        drop(e0);
        let mut ci = Vec::new();
        let mut ei = make_eval(&mut ci);
        let inc = incremental_eval(1, &s, &params(6.0, 0.5), &BTreeSet::new(), &base, &mut ei);
        drop(ei);
        assert!(inc.recomputed.is_empty());
        assert_eq!(inc.outputs, base);
    }
}
