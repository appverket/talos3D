//! PP82 integration test: `AuthoringScript` body schema + deterministic
//! replay executor.
//!
//! Exercises the replay executor against realistic scripts using an
//! in-process `FixtureDispatcher` + `FixtureOracle`. The MCP-backed
//! `invoke_recipe { replay: true }` tool wiring (slice 3's
//! world-backed dispatcher + MCP handler) is a follow-up; these tests
//! validate that the executor produces the right behavior end-to-end
//! without the MCP transport.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use talos3d_core::curation::authoring_script::{
    ArgExpr, AuthoringScript, McpToolId, MutationScope, OutputPath, Postcondition, Step, StepId,
};
use talos3d_core::curation::replay::{
    replay, InvocationError, PostconditionOracle, PostconditionVerdict, ResolvedPostcondition,
    ToolCall, ToolDispatchError, ToolDispatcher,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Canned dispatcher — records each call, returns canned responses in
/// call order.
struct FixtureDispatcher {
    responses: Vec<Value>,
    calls: Vec<(McpToolId, Map<String, Value>)>,
}

impl FixtureDispatcher {
    fn new(responses: Vec<Value>) -> Self {
        Self {
            responses,
            calls: Vec::new(),
        }
    }
}

impl ToolDispatcher for FixtureDispatcher {
    fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
        self.calls.push((call.tool.clone(), call.args.clone()));
        if self.responses.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(self.responses.remove(0))
        }
    }
}

/// Permissive oracle: every postcondition passes.
struct AlwaysPassOracle;

impl PostconditionOracle for AlwaysPassOracle {
    fn check(
        &self,
        _pc: &ResolvedPostcondition,
        _outputs: &BTreeMap<StepId, Map<String, Value>>,
        _params: &Map<String, Value>,
    ) -> PostconditionVerdict {
        PostconditionVerdict::Pass
    }
}

// ---------------------------------------------------------------------------
// Echo-wall fixture recipe
// ---------------------------------------------------------------------------

/// An intentionally minimal fixture `AuthoringScript`: takes a
/// `length_mm` parameter, creates a definition via a synthetic
/// `create_definition` tool, instantiates one occurrence, and asserts
/// one postcondition.
fn echo_wall_script() -> AuthoringScript {
    let mut script = AuthoringScript::stub(MutationScope::RefinementSubtree {
        root_element_param: "element_id".into(),
    });

    script.parameter_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "element_id": { "type": "integer" },
            "length_mm": { "type": "number", "minimum": 100 }
        },
        "required": ["element_id", "length_mm"]
    });
    script
        .parameter_defaults
        .insert("length_mm".into(), 2400.into());

    let mut allowed: BTreeSet<McpToolId> = BTreeSet::new();
    allowed.insert(McpToolId::new("create_definition"));
    allowed.insert(McpToolId::new("instantiate_occurrence"));
    script.allowed_tools = allowed;

    // Step 1 — create_definition with length_mm parameter.
    let mut step_create = Step {
        id: StepId::new("create_def"),
        tool: McpToolId::new("create_definition"),
        args: BTreeMap::new(),
        bindings: BTreeMap::new(),
        essential: true,
        precondition: None,
    };
    step_create.args.insert(
        "length_mm".into(),
        ArgExpr::Param {
            name: "length_mm".into(),
        },
    );
    step_create
        .bindings
        .insert("def_id".into(), OutputPath::new("$.definition_id"));
    script.steps.push(step_create);

    // Step 2 — instantiate_occurrence referencing the newly-created
    // definition + the element_id param.
    let mut step_inst = Step {
        id: StepId::new("place_one"),
        tool: McpToolId::new("instantiate_occurrence"),
        args: BTreeMap::new(),
        bindings: BTreeMap::new(),
        essential: true,
        precondition: None,
    };
    step_inst.args.insert(
        "definition".into(),
        ArgExpr::StepOutput {
            step_id: StepId::new("create_def"),
            path: OutputPath::new("def_id"),
        },
    );
    step_inst.args.insert(
        "host_element".into(),
        ArgExpr::Param {
            name: "element_id".into(),
        },
    );
    step_inst
        .bindings
        .insert("occ_id".into(), OutputPath::new("$.occurrence_id"));
    script.steps.push(step_inst);

    // Postcondition: occurrence-count claim on the host.
    script.postconditions.push(Postcondition::Relation {
        relation_kind: "has_occurrence".into(),
        from: ArgExpr::Param {
            name: "element_id".into(),
        },
        to: ArgExpr::StepOutput {
            step_id: StepId::new("place_one"),
            path: OutputPath::new("occ_id"),
        },
    });

    script
}

// ---------------------------------------------------------------------------
// Integration tests
// ---------------------------------------------------------------------------

#[test]
fn echo_wall_script_structure_validates() {
    let s = echo_wall_script();
    assert!(s.validate_structure().is_ok());
}

#[test]
fn echo_wall_script_replays_end_to_end() {
    let script = echo_wall_script();

    // Dispatcher returns a definition_id from step 1, occurrence_id
    // from step 2.
    let mut dispatcher = FixtureDispatcher::new(vec![
        serde_json::json!({ "definition_id": 100 }),
        serde_json::json!({ "occurrence_id": 200 }),
    ]);
    let oracle = AlwaysPassOracle;

    let mut params = Map::new();
    params.insert("element_id".into(), Value::from(42));
    params.insert("length_mm".into(), Value::from(3000));

    let report = replay(&script, params, &mut dispatcher, &oracle).unwrap();

    // Both steps ran.
    assert_eq!(
        report.steps_run,
        vec![StepId::new("create_def"), StepId::new("place_one")]
    );
    assert!(report.steps_skipped.is_empty());

    // Captured outputs.
    assert_eq!(
        report.outputs[&StepId::new("create_def")]["def_id"],
        Value::from(100),
    );
    assert_eq!(
        report.outputs[&StepId::new("place_one")]["occ_id"],
        Value::from(200),
    );

    // Dispatcher received expected args.
    assert_eq!(dispatcher.calls[0].1["length_mm"], Value::from(3000));
    assert_eq!(dispatcher.calls[1].1["definition"], Value::from(100));
    assert_eq!(dispatcher.calls[1].1["host_element"], Value::from(42));

    // Postcondition passed.
    assert_eq!(report.postcondition_results.len(), 1);
    match &report.postcondition_results[0].postcondition {
        ResolvedPostcondition::Relation {
            relation_kind,
            from,
            to,
        } => {
            assert_eq!(relation_kind, "has_occurrence");
            assert_eq!(*from, Value::from(42));
            assert_eq!(*to, Value::from(200));
        }
        _ => panic!("expected Relation"),
    }
}

#[test]
fn echo_wall_script_default_length_mm_propagates() {
    let script = echo_wall_script();
    let mut dispatcher = FixtureDispatcher::new(vec![
        serde_json::json!({ "definition_id": 1 }),
        serde_json::json!({ "occurrence_id": 2 }),
    ]);
    let oracle = AlwaysPassOracle;

    // Omit length_mm — should take default 2400.
    let mut params = Map::new();
    params.insert("element_id".into(), Value::from(7));

    let report = replay(&script, params, &mut dispatcher, &oracle).unwrap();
    assert_eq!(report.steps_run.len(), 2);
    // Verify the default flowed through.
    assert_eq!(dispatcher.calls[0].1["length_mm"], Value::from(2400));
}

#[test]
fn echo_wall_script_missing_required_element_id_rejected() {
    let script = echo_wall_script();
    let mut dispatcher = FixtureDispatcher::new(Vec::new());
    let oracle = AlwaysPassOracle;

    // Only length_mm — element_id missing.
    let mut params = Map::new();
    params.insert("length_mm".into(), Value::from(2000));

    let err = replay(&script, params, &mut dispatcher, &oracle).unwrap_err();
    assert!(matches!(err, InvocationError::ParameterSchemaFailed { .. }));
    assert!(dispatcher.calls.is_empty(), "no dispatch on param failure");
}

#[test]
fn script_with_out_of_set_tool_rejected_by_structure_check() {
    use std::collections::BTreeSet;

    let mut script = AuthoringScript::stub(MutationScope::None);
    script.allowed_tools = BTreeSet::new(); // empty
    script.steps.push(Step {
        id: StepId::new("rogue"),
        tool: McpToolId::new("forbidden_tool"),
        args: BTreeMap::new(),
        bindings: BTreeMap::new(),
        essential: true,
        precondition: None,
    });

    let mut dispatcher = FixtureDispatcher::new(Vec::new());
    let oracle = AlwaysPassOracle;
    let err = replay(&script, Map::new(), &mut dispatcher, &oracle).unwrap_err();
    assert!(matches!(err, InvocationError::InvalidStructure(_)));
    assert!(dispatcher.calls.is_empty());
}

#[test]
fn postcondition_failure_aborts_replay() {
    struct RejectEverything;

    impl PostconditionOracle for RejectEverything {
        fn check(
            &self,
            _pc: &ResolvedPostcondition,
            _outputs: &BTreeMap<StepId, Map<String, Value>>,
            _params: &Map<String, Value>,
        ) -> PostconditionVerdict {
            PostconditionVerdict::Fail {
                reason: "test reject".into(),
            }
        }
    }

    let script = echo_wall_script();
    let mut dispatcher = FixtureDispatcher::new(vec![
        serde_json::json!({ "definition_id": 1 }),
        serde_json::json!({ "occurrence_id": 2 }),
    ]);
    let oracle = RejectEverything;

    let mut params = Map::new();
    params.insert("element_id".into(), Value::from(1));

    let err = replay(&script, params, &mut dispatcher, &oracle).unwrap_err();
    match err {
        InvocationError::PostconditionFailed { reason, .. } => {
            assert!(reason.contains("test reject"));
        }
        other => panic!("expected PostconditionFailed, got {other:?}"),
    }
}

#[test]
fn non_essential_step_skipped_when_precondition_fails() {
    use talos3d_core::curation::authoring_script::Predicate;

    let mut script = AuthoringScript::stub(MutationScope::None);
    script.allowed_tools.insert(McpToolId::new("maybe_tool"));

    script.steps.push(Step {
        id: StepId::new("guarded"),
        tool: McpToolId::new("maybe_tool"),
        args: BTreeMap::new(),
        bindings: BTreeMap::new(),
        essential: false,
        precondition: Some(Predicate::Defined {
            expr: ArgExpr::Param {
                name: "optional".into(),
            },
        }),
    });

    let mut dispatcher = FixtureDispatcher::new(Vec::new());
    let oracle = AlwaysPassOracle;
    let report = replay(&script, Map::new(), &mut dispatcher, &oracle).unwrap();
    assert!(report.steps_run.is_empty());
    assert_eq!(report.steps_skipped, vec![StepId::new("guarded")]);
    assert!(
        dispatcher.calls.is_empty(),
        "skipped step must not dispatch"
    );
}

#[test]
fn essential_step_precondition_failure_is_hard_error() {
    use talos3d_core::curation::authoring_script::Predicate;

    let mut script = AuthoringScript::stub(MutationScope::None);
    script.allowed_tools.insert(McpToolId::new("t"));
    script.steps.push(Step {
        id: StepId::new("required_step"),
        tool: McpToolId::new("t"),
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

    let mut dispatcher = FixtureDispatcher::new(Vec::new());
    let oracle = AlwaysPassOracle;
    let err = replay(&script, Map::new(), &mut dispatcher, &oracle).unwrap_err();
    assert!(matches!(
        err,
        InvocationError::EssentialStepPreconditionFailed { .. }
    ));
}
