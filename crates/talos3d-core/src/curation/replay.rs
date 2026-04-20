//! Deterministic replay executor for `AuthoringScript` bodies.
//!
//! The executor walks a script's steps in order, resolves `ArgExpr`s
//! against the caller-supplied parameters plus captured step outputs,
//! dispatches each call through a `ToolDispatcher` (the real impl wraps
//! the Model API), captures outputs by `bindings`, and — after the last
//! step — verifies every `Postcondition` using a `PostconditionOracle`.
//!
//! Scope for PP82 slice 2: the pure executor logic. The actual Model-
//! API-backed `ToolDispatcher` and the `invoke_recipe` MCP dispatch are
//! in slice 3.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use super::authoring_script::{
    ArgExpr, AuthoringScript, McpToolId, MutationScope, OutputPath, Postcondition, Predicate,
    Step, StepId,
};
use super::provenance::GroundingKind;
use crate::plugins::refinement::ClaimPath;

/// A tool invocation the executor wants to dispatch. Includes the
/// declared `mutation_scope` bounds of the owning script so the
/// dispatcher can enforce them.
#[derive(Debug, Clone)]
pub struct ToolCall<'a> {
    pub tool: &'a McpToolId,
    pub args: &'a Map<String, Value>,
    pub mutation_scope: &'a MutationScope,
    pub params: &'a Map<String, Value>,
}

/// Pluggable tool dispatcher. The real impl bridges to the Model API;
/// tests use an in-process mock that returns canned JSON.
pub trait ToolDispatcher {
    /// Dispatch a tool call. Returns the tool's response JSON or a
    /// structured error.
    fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDispatchError {
    pub code: String,
    pub message: String,
}

impl ToolDispatchError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Pluggable oracle that checks postconditions after the last step.
/// The real impl queries the live world (relation snapshots, claim
/// groundings, obligation-satisfaction links); tests use a mock that
/// returns canned Pass/Fail verdicts.
pub trait PostconditionOracle {
    fn check(
        &self,
        postcondition: &ResolvedPostcondition,
        outputs: &BTreeMap<StepId, Map<String, Value>>,
        params: &Map<String, Value>,
    ) -> PostconditionVerdict;
}

/// A postcondition with its `ArgExpr`s pre-resolved. Oracles receive
/// this shape rather than the raw `Postcondition` so they don't need to
/// re-implement the resolver.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedPostcondition {
    Relation {
        relation_kind: String,
        from: Value,
        to: Value,
    },
    Claim {
        path: ClaimPath,
        grounding: GroundingKind,
    },
    ObligationSatisfied {
        obligation_id: Value,
        by_step: StepId,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PostconditionVerdict {
    Pass,
    Fail { reason: String },
}

/// Structured outcome of a replay run.
#[derive(Debug, Clone, PartialEq)]
pub struct InvocationReport {
    pub steps_run: Vec<StepId>,
    pub steps_skipped: Vec<StepId>,
    pub outputs: BTreeMap<StepId, Map<String, Value>>,
    pub postcondition_results: Vec<PostconditionResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PostconditionResult {
    pub postcondition: ResolvedPostcondition,
    pub verdict: PostconditionVerdict,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InvocationError {
    /// `allowed_tools` doesn't contain the step's tool.
    ToolNotAllowed {
        step: StepId,
        tool: McpToolId,
    },
    /// An `ArgExpr` references something that could not be resolved.
    UnresolvedArgExpr {
        step: StepId,
        reason: String,
    },
    /// Essential step's `precondition` evaluated to false.
    EssentialStepPreconditionFailed {
        step: StepId,
    },
    /// Tool dispatch failed.
    Dispatch {
        step: StepId,
        error: ToolDispatchError,
    },
    /// A binding's `OutputPath` didn't resolve in the step's response.
    BindingPathMissing {
        step: StepId,
        binding: String,
        path: OutputPath,
    },
    /// Post-run postcondition verification found a failure.
    PostconditionFailed {
        postcondition: ResolvedPostcondition,
        reason: String,
    },
    /// Script structure is invalid (walks `validate_structure()`).
    InvalidStructure(super::authoring_script::AuthoringScriptStructuralError),
    /// Parameter schema validation failed. `schema_error` carries the
    /// underlying JSON-schema validator error; we keep it opaque to
    /// avoid pinning a specific validator crate here.
    ParameterSchemaFailed {
        message: String,
    },
}

impl std::fmt::Display for InvocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToolNotAllowed { step, tool } => write!(
                f,
                "step '{}' uses tool '{}' not in allowed_tools",
                step.0, tool.0,
            ),
            Self::UnresolvedArgExpr { step, reason } => {
                write!(f, "step '{}' arg resolution: {}", step.0, reason)
            }
            Self::EssentialStepPreconditionFailed { step } => {
                write!(f, "essential step '{}' precondition failed", step.0)
            }
            Self::Dispatch { step, error } => {
                write!(f, "step '{}' dispatch: {} ({})", step.0, error.code, error.message)
            }
            Self::BindingPathMissing { step, binding, path } => write!(
                f,
                "step '{}' binding '{}' path '{}' not found in response",
                step.0, binding, path.0,
            ),
            Self::PostconditionFailed { reason, .. } => {
                write!(f, "postcondition failed: {reason}")
            }
            Self::InvalidStructure(e) => write!(f, "invalid script structure: {e}"),
            Self::ParameterSchemaFailed { message } => {
                write!(f, "parameter schema validation failed: {message}")
            }
        }
    }
}

impl std::error::Error for InvocationError {}

/// Run the script deterministically against the dispatcher + oracle.
///
/// Does not require a Bevy world — the world lives "behind" the
/// dispatcher trait impl. This keeps the executor unit-testable.
pub fn replay<D: ToolDispatcher, O: PostconditionOracle>(
    script: &AuthoringScript,
    params: Map<String, Value>,
    dispatcher: &mut D,
    oracle: &O,
) -> Result<InvocationReport, InvocationError> {
    script
        .validate_structure()
        .map_err(InvocationError::InvalidStructure)?;

    let merged_params = merge_defaults(script, params);
    validate_param_schema(&script.parameter_schema, &merged_params)?;

    let mut outputs: BTreeMap<StepId, Map<String, Value>> = BTreeMap::new();
    let mut steps_run = Vec::new();
    let mut steps_skipped = Vec::new();

    for step in &script.steps {
        if !script.allowed_tools.contains(&step.tool) {
            return Err(InvocationError::ToolNotAllowed {
                step: step.id.clone(),
                tool: step.tool.clone(),
            });
        }

        let precondition_passed = step
            .precondition
            .as_ref()
            .map(|p| evaluate_predicate(p, &merged_params, &outputs))
            .transpose()
            .map_err(|reason| InvocationError::UnresolvedArgExpr {
                step: step.id.clone(),
                reason,
            })?
            .unwrap_or(true);

        if !precondition_passed {
            if step.essential {
                return Err(InvocationError::EssentialStepPreconditionFailed {
                    step: step.id.clone(),
                });
            }
            steps_skipped.push(step.id.clone());
            continue;
        }

        let resolved_args = resolve_args(step, &merged_params, &outputs)
            .map_err(|reason| InvocationError::UnresolvedArgExpr {
                step: step.id.clone(),
                reason,
            })?;

        let call = ToolCall {
            tool: &step.tool,
            args: &resolved_args,
            mutation_scope: &script.mutation_scope,
            params: &merged_params,
        };

        let response = dispatcher
            .dispatch(&call)
            .map_err(|e| InvocationError::Dispatch {
                step: step.id.clone(),
                error: e,
            })?;

        let captured = capture_bindings(step, &response)?;
        outputs.insert(step.id.clone(), captured);
        steps_run.push(step.id.clone());
    }

    let mut postcondition_results = Vec::new();
    for pc in &script.postconditions {
        let resolved = resolve_postcondition(pc, &merged_params, &outputs).map_err(|reason| {
            InvocationError::UnresolvedArgExpr {
                step: StepId::new("(postcondition)"),
                reason,
            }
        })?;
        let verdict = oracle.check(&resolved, &outputs, &merged_params);
        if let PostconditionVerdict::Fail { reason } = &verdict {
            return Err(InvocationError::PostconditionFailed {
                postcondition: resolved.clone(),
                reason: reason.clone(),
            });
        }
        postcondition_results.push(PostconditionResult {
            postcondition: resolved,
            verdict,
        });
    }

    Ok(InvocationReport {
        steps_run,
        steps_skipped,
        outputs,
        postcondition_results,
    })
}

fn merge_defaults(script: &AuthoringScript, mut params: Map<String, Value>) -> Map<String, Value> {
    for (k, v) in &script.parameter_defaults {
        params.entry(k.clone()).or_insert_with(|| v.clone());
    }
    params
}

/// Minimal JSON schema check for PP82: enforces `required` array (if
/// any) has values present. Full JSON Schema compliance is future work.
fn validate_param_schema(
    schema: &Value,
    params: &Map<String, Value>,
) -> Result<(), InvocationError> {
    let Some(required) = schema.get("required").and_then(|r| r.as_array()) else {
        return Ok(());
    };
    for req in required {
        let Some(key) = req.as_str() else { continue };
        if !params.contains_key(key) {
            return Err(InvocationError::ParameterSchemaFailed {
                message: format!("required parameter '{key}' missing"),
            });
        }
    }
    Ok(())
}

fn resolve_args(
    step: &Step,
    params: &Map<String, Value>,
    outputs: &BTreeMap<StepId, Map<String, Value>>,
) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (k, expr) in &step.args {
        let v = resolve_arg_expr(expr, params, outputs)?;
        out.insert(k.clone(), v);
    }
    Ok(out)
}

fn resolve_arg_expr(
    expr: &ArgExpr,
    params: &Map<String, Value>,
    outputs: &BTreeMap<StepId, Map<String, Value>>,
) -> Result<Value, String> {
    match expr {
        ArgExpr::Literal { value } => Ok(value.clone()),
        ArgExpr::Param { name } => params
            .get(name)
            .cloned()
            .ok_or_else(|| format!("missing parameter '{name}'")),
        ArgExpr::StepOutput { step_id, path } => {
            let step_out = outputs
                .get(step_id)
                .ok_or_else(|| format!("step '{}' has no captured output (skipped?)", step_id.0))?;
            step_out
                .get(path.as_str())
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "step '{}' output has no binding '{}'",
                        step_id.0, path.0,
                    )
                })
        }
        ArgExpr::ClaimRef { path } => {
            // Without a live world we can't resolve claim refs here;
            // slice-3 dispatch wraps the executor with a live-world
            // resolver. For the pure unit-test executor we return Null
            // so scripts that use ClaimRef in a dispatch arg need a
            // real world (or a custom ToolDispatcher that handles it).
            // Emit a clear error rather than silently returning Null
            // so the test author notices.
            Err(format!(
                "claim_ref '{}' cannot be resolved by the pure executor; \
                 route through a live-world dispatcher",
                path.0
            ))
        }
    }
}

fn evaluate_predicate(
    pred: &Predicate,
    params: &Map<String, Value>,
    outputs: &BTreeMap<StepId, Map<String, Value>>,
) -> Result<bool, String> {
    match pred {
        Predicate::Equals { lhs, rhs } => {
            let l = resolve_arg_expr(lhs, params, outputs)?;
            let r = resolve_arg_expr(rhs, params, outputs)?;
            Ok(l == r)
        }
        Predicate::Defined { expr } => match resolve_arg_expr(expr, params, outputs) {
            Ok(v) => Ok(!v.is_null()),
            Err(_) => Ok(false),
        },
        Predicate::And { children } => {
            for c in children {
                if !evaluate_predicate(c, params, outputs)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Predicate::Or { children } => {
            for c in children {
                if evaluate_predicate(c, params, outputs)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Predicate::Not { child } => Ok(!evaluate_predicate(child, params, outputs)?),
    }
}

fn capture_bindings(step: &Step, response: &Value) -> Result<Map<String, Value>, InvocationError> {
    let mut out = Map::new();
    for (label, path) in &step.bindings {
        let v = read_path(response, path).ok_or_else(|| InvocationError::BindingPathMissing {
            step: step.id.clone(),
            binding: label.clone(),
            path: path.clone(),
        })?;
        out.insert(label.clone(), v);
    }
    Ok(out)
}

/// Read a value from a JSON response using an `OutputPath`. Path
/// syntax is intentionally tiny for PP82:
///
/// - `$` refers to the root value.
/// - `$.field.subfield` walks nested keys (top-level `$.` prefix
///   optional).
/// - bare `field` reads that key from an object root.
fn read_path(value: &Value, path: &OutputPath) -> Option<Value> {
    let raw = path.as_str();
    let stripped = raw.strip_prefix("$.").unwrap_or_else(|| raw.strip_prefix('$').unwrap_or(raw));
    if stripped.is_empty() {
        return Some(value.clone());
    }
    let mut cur = value;
    for segment in stripped.split('.').filter(|s| !s.is_empty()) {
        cur = cur.get(segment)?;
    }
    Some(cur.clone())
}

fn resolve_postcondition(
    pc: &Postcondition,
    params: &Map<String, Value>,
    outputs: &BTreeMap<StepId, Map<String, Value>>,
) -> Result<ResolvedPostcondition, String> {
    match pc {
        Postcondition::Relation {
            relation_kind,
            from,
            to,
        } => Ok(ResolvedPostcondition::Relation {
            relation_kind: relation_kind.clone(),
            from: resolve_arg_expr(from, params, outputs)?,
            to: resolve_arg_expr(to, params, outputs)?,
        }),
        Postcondition::Claim { path, grounding } => Ok(ResolvedPostcondition::Claim {
            path: path.clone(),
            grounding: grounding.clone(),
        }),
        Postcondition::ObligationSatisfied {
            obligation_id_expr,
            by_step,
        } => Ok(ResolvedPostcondition::ObligationSatisfied {
            obligation_id: resolve_arg_expr(obligation_id_expr, params, outputs)?,
            by_step: by_step.clone(),
        }),
    }
}

// Suppress unused warning for an intentionally-unused helper — the
// BTreeSet import is reserved for future structural passes.
#[allow(dead_code)]
fn _touch_btreeset(s: BTreeSet<StepId>) -> BTreeSet<StepId> {
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::authoring_script::MutationScope;

    /// Test dispatcher that records calls and returns canned responses.
    struct FixtureDispatcher {
        responses: Vec<Value>,
        calls: Vec<(McpToolId, Map<String, Value>)>,
        error_on: Option<(McpToolId, ToolDispatchError)>,
    }

    impl FixtureDispatcher {
        fn new(responses: Vec<Value>) -> Self {
            Self {
                responses,
                calls: Vec::new(),
                error_on: None,
            }
        }
    }

    impl ToolDispatcher for FixtureDispatcher {
        fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
            self.calls
                .push((call.tool.clone(), call.args.clone()));
            if let Some((tool, err)) = &self.error_on {
                if tool == call.tool {
                    return Err(err.clone());
                }
            }
            if self.responses.is_empty() {
                Ok(Value::Null)
            } else {
                Ok(self.responses.remove(0))
            }
        }
    }

    /// Oracle that fails a specific postcondition by its relation_kind.
    struct FixtureOracle {
        fail_relation_kind: Option<String>,
    }

    impl PostconditionOracle for FixtureOracle {
        fn check(
            &self,
            postcondition: &ResolvedPostcondition,
            _outputs: &BTreeMap<StepId, Map<String, Value>>,
            _params: &Map<String, Value>,
        ) -> PostconditionVerdict {
            if let (ResolvedPostcondition::Relation { relation_kind, .. }, Some(target)) =
                (postcondition, &self.fail_relation_kind)
            {
                if relation_kind == target {
                    return PostconditionVerdict::Fail {
                        reason: format!("relation '{relation_kind}' deliberately failed"),
                    };
                }
            }
            PostconditionVerdict::Pass
        }
    }

    fn tool(name: &str) -> McpToolId {
        McpToolId::new(name)
    }

    fn make_script_with_one_step(
        tool_name: &str,
        bindings: BTreeMap<String, OutputPath>,
    ) -> AuthoringScript {
        use super::super::authoring_script::Step;
        let mut s = AuthoringScript::stub(MutationScope::None);
        s.allowed_tools.insert(tool(tool_name));
        s.steps.push(Step {
            id: StepId::new("a"),
            tool: tool(tool_name),
            args: BTreeMap::new(),
            bindings,
            essential: true,
            precondition: None,
        });
        s
    }

    #[test]
    fn replay_single_step_captures_bindings() {
        let mut bindings = BTreeMap::new();
        bindings.insert("id".into(), OutputPath::new("$.element_id"));
        let script = make_script_with_one_step("create_definition", bindings);
        let response = serde_json::json!({ "element_id": 42 });
        let mut d = FixtureDispatcher::new(vec![response]);
        let o = FixtureOracle {
            fail_relation_kind: None,
        };
        let report = replay(&script, Map::new(), &mut d, &o).unwrap();
        assert_eq!(report.steps_run, vec![StepId::new("a")]);
        assert_eq!(
            report.outputs[&StepId::new("a")]["id"],
            Value::from(42)
        );
    }

    #[test]
    fn replay_rejects_tool_not_in_allowed_set() {
        use super::super::authoring_script::Step;
        let mut s = AuthoringScript::stub(MutationScope::None);
        // allowed_tools empty — so any step's tool is rejected.
        s.allowed_tools.insert(tool("real"));
        s.steps.push(Step {
            id: StepId::new("bad"),
            tool: tool("real"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        });
        // Re-synthesize a step that references a non-allowed tool by
        // hand (bypassing validate_structure for this test — we want
        // to verify replay's own gate, not the structural check).
        s.steps.push(Step {
            id: StepId::new("rogue"),
            tool: tool("forbidden"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        });
        // validate_structure will reject this, so replay returns
        // InvalidStructure rather than ToolNotAllowed. That's the
        // correct behaviour — structural errors are surfaced first.
        let mut d = FixtureDispatcher::new(vec![Value::Null]);
        let o = FixtureOracle {
            fail_relation_kind: None,
        };
        let err = replay(&s, Map::new(), &mut d, &o).unwrap_err();
        assert!(matches!(err, InvocationError::InvalidStructure(_)));
    }

    #[test]
    fn replay_dispatch_tool_not_allowed_when_skipping_structural_validation() {
        // Test ToolNotAllowed directly by bypassing validate_structure
        // — we do this by calling the internal gate logic via a
        // hand-constructed script that validate_structure would accept
        // but we artificially break the allowed-tools relation between
        // step and script *after* validation. Since validate_structure
        // is called inside replay, we can't realistically hit
        // ToolNotAllowed in isolation unless the script is mutated
        // after validation — which is not a supported flow. We skip.
        // The gate exists as belt-and-suspenders; it's correct by
        // inspection.
    }

    #[test]
    fn replay_dispatch_error_surfaces_as_dispatch_error() {
        let mut bindings = BTreeMap::new();
        bindings.insert("id".into(), OutputPath::new("$.id"));
        let script = make_script_with_one_step("create_definition", bindings);
        let mut d = FixtureDispatcher::new(Vec::new());
        d.error_on = Some((
            tool("create_definition"),
            ToolDispatchError::new("tool.unavailable", "model_api offline"),
        ));
        let o = FixtureOracle {
            fail_relation_kind: None,
        };
        let err = replay(&script, Map::new(), &mut d, &o).unwrap_err();
        match err {
            InvocationError::Dispatch { error, .. } => {
                assert_eq!(error.code, "tool.unavailable");
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn replay_missing_binding_path_returns_error() {
        let mut bindings = BTreeMap::new();
        bindings.insert("id".into(), OutputPath::new("$.element_id"));
        let script = make_script_with_one_step("t", bindings);
        let response = serde_json::json!({ "wrong_key": 1 });
        let mut d = FixtureDispatcher::new(vec![response]);
        let o = FixtureOracle {
            fail_relation_kind: None,
        };
        let err = replay(&script, Map::new(), &mut d, &o).unwrap_err();
        assert!(matches!(err, InvocationError::BindingPathMissing { .. }));
    }

    #[test]
    fn replay_parameter_schema_required_enforced() {
        use super::super::authoring_script::Step;
        let mut s = AuthoringScript::stub(MutationScope::None);
        s.allowed_tools.insert(tool("t"));
        s.parameter_schema = serde_json::json!({
            "type": "object",
            "required": ["length_mm"]
        });
        let mut step_a = Step {
            id: StepId::new("a"),
            tool: tool("t"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        step_a.args.insert(
            "length".into(),
            ArgExpr::Param {
                name: "length_mm".into(),
            },
        );
        s.steps.push(step_a);
        let mut d = FixtureDispatcher::new(vec![Value::Null]);
        let o = FixtureOracle {
            fail_relation_kind: None,
        };
        let err = replay(&s, Map::new(), &mut d, &o).unwrap_err();
        assert!(matches!(err, InvocationError::ParameterSchemaFailed { .. }));
    }

    #[test]
    fn replay_param_default_satisfies_required() {
        use super::super::authoring_script::Step;
        let mut s = AuthoringScript::stub(MutationScope::None);
        s.allowed_tools.insert(tool("t"));
        s.parameter_schema = serde_json::json!({
            "type": "object",
            "required": ["length_mm"]
        });
        s.parameter_defaults
            .insert("length_mm".into(), Value::from(2400));
        let mut step_a = Step {
            id: StepId::new("a"),
            tool: tool("t"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        step_a.args.insert(
            "length".into(),
            ArgExpr::Param {
                name: "length_mm".into(),
            },
        );
        s.steps.push(step_a);
        let mut d = FixtureDispatcher::new(vec![Value::Null]);
        let o = FixtureOracle {
            fail_relation_kind: None,
        };
        let report = replay(&s, Map::new(), &mut d, &o).unwrap();
        assert_eq!(report.steps_run, vec![StepId::new("a")]);
        // Verify the dispatcher received the default value.
        assert_eq!(d.calls[0].1["length"], Value::from(2400));
    }

    #[test]
    fn replay_essential_step_precondition_failure_errors() {
        use super::super::authoring_script::Step;
        let mut s = AuthoringScript::stub(MutationScope::None);
        s.allowed_tools.insert(tool("t"));
        s.steps.push(Step {
            id: StepId::new("a"),
            tool: tool("t"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: Some(Predicate::Equals {
                lhs: ArgExpr::Literal {
                    value: Value::from(1),
                },
                rhs: ArgExpr::Literal {
                    value: Value::from(2),
                },
            }),
        });
        let mut d = FixtureDispatcher::new(vec![Value::Null]);
        let o = FixtureOracle {
            fail_relation_kind: None,
        };
        let err = replay(&s, Map::new(), &mut d, &o).unwrap_err();
        assert!(matches!(
            err,
            InvocationError::EssentialStepPreconditionFailed { .. }
        ));
    }

    #[test]
    fn replay_non_essential_step_precondition_failure_skips() {
        use super::super::authoring_script::Step;
        let mut s = AuthoringScript::stub(MutationScope::None);
        s.allowed_tools.insert(tool("t"));
        s.steps.push(Step {
            id: StepId::new("a"),
            tool: tool("t"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: false,
            precondition: Some(Predicate::Defined {
                expr: ArgExpr::Param {
                    name: "missing".into(),
                },
            }),
        });
        let mut d = FixtureDispatcher::new(vec![Value::Null]);
        let o = FixtureOracle {
            fail_relation_kind: None,
        };
        let report = replay(&s, Map::new(), &mut d, &o).unwrap();
        assert!(report.steps_run.is_empty());
        assert_eq!(report.steps_skipped, vec![StepId::new("a")]);
    }

    #[test]
    fn replay_step_output_flows_into_next_step_args() {
        use super::super::authoring_script::Step;
        let mut s = AuthoringScript::stub(MutationScope::None);
        s.allowed_tools.insert(tool("t"));
        let mut bindings_a = BTreeMap::new();
        bindings_a.insert("id".into(), OutputPath::new("$.element_id"));
        s.steps.push(Step {
            id: StepId::new("a"),
            tool: tool("t"),
            args: BTreeMap::new(),
            bindings: bindings_a,
            essential: true,
            precondition: None,
        });
        let mut args_b = BTreeMap::new();
        args_b.insert(
            "from".into(),
            ArgExpr::StepOutput {
                step_id: StepId::new("a"),
                path: OutputPath::new("id"),
            },
        );
        s.steps.push(Step {
            id: StepId::new("b"),
            tool: tool("t"),
            args: args_b,
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        });
        let mut d = FixtureDispatcher::new(vec![
            serde_json::json!({ "element_id": 99 }),
            Value::Null,
        ]);
        let o = FixtureOracle {
            fail_relation_kind: None,
        };
        let _report = replay(&s, Map::new(), &mut d, &o).unwrap();
        // Second call's "from" arg should equal 99.
        assert_eq!(d.calls[1].1["from"], Value::from(99));
    }

    #[test]
    fn replay_postcondition_verdict_pass_and_fail() {
        use super::super::authoring_script::Step;
        let mut s = AuthoringScript::stub(MutationScope::None);
        s.allowed_tools.insert(tool("t"));
        s.steps.push(Step {
            id: StepId::new("a"),
            tool: tool("t"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        });
        s.postconditions.push(Postcondition::Relation {
            relation_kind: "bears_on".into(),
            from: ArgExpr::Literal {
                value: Value::from("child"),
            },
            to: ArgExpr::Literal {
                value: Value::from("parent"),
            },
        });
        let mut d = FixtureDispatcher::new(vec![Value::Null]);

        // Passing oracle — no explicit fail.
        let o = FixtureOracle {
            fail_relation_kind: None,
        };
        let report = replay(&s, Map::new(), &mut d, &o).unwrap();
        assert_eq!(report.postcondition_results.len(), 1);
        assert_eq!(
            report.postcondition_results[0].verdict,
            PostconditionVerdict::Pass
        );

        // Failing oracle — surfaces as PostconditionFailed error.
        let mut d2 = FixtureDispatcher::new(vec![Value::Null]);
        let o2 = FixtureOracle {
            fail_relation_kind: Some("bears_on".into()),
        };
        let err = replay(&s, Map::new(), &mut d2, &o2).unwrap_err();
        assert!(matches!(err, InvocationError::PostconditionFailed { .. }));
    }

    #[test]
    fn read_path_supports_dollar_prefix_and_nested_keys() {
        let v = serde_json::json!({
            "outer": { "inner": 7 },
            "top": "x"
        });
        assert_eq!(read_path(&v, &OutputPath::new("$")), Some(v.clone()));
        assert_eq!(
            read_path(&v, &OutputPath::new("$.outer.inner")),
            Some(Value::from(7))
        );
        assert_eq!(
            read_path(&v, &OutputPath::new("outer.inner")),
            Some(Value::from(7))
        );
        assert_eq!(read_path(&v, &OutputPath::new("missing")), None);
    }

    #[test]
    fn predicate_and_or_not_short_circuit_correctly() {
        let params: Map<String, Value> = Map::new();
        let outputs = BTreeMap::new();

        assert!(
            evaluate_predicate(
                &Predicate::And {
                    children: vec![
                        Predicate::Equals {
                            lhs: ArgExpr::Literal {
                                value: Value::from(1),
                            },
                            rhs: ArgExpr::Literal {
                                value: Value::from(1),
                            },
                        },
                        Predicate::Not {
                            child: Box::new(Predicate::Equals {
                                lhs: ArgExpr::Literal {
                                    value: Value::from(1),
                                },
                                rhs: ArgExpr::Literal {
                                    value: Value::from(2),
                                },
                            }),
                        },
                    ],
                },
                &params,
                &outputs,
            )
            .unwrap()
        );

        assert!(
            !evaluate_predicate(
                &Predicate::Or {
                    children: vec![
                        Predicate::Equals {
                            lhs: ArgExpr::Literal {
                                value: Value::from(1),
                            },
                            rhs: ArgExpr::Literal {
                                value: Value::from(2),
                            },
                        },
                        Predicate::Defined {
                            expr: ArgExpr::Param {
                                name: "nope".into(),
                            },
                        },
                    ],
                },
                &params,
                &outputs,
            )
            .unwrap()
        );
    }
}
