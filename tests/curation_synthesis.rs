//! PP86 integration tests: synthesis-mode invocation with FixtureSynthesisLlm.
//!
//! Tests cover:
//! - Draft + fixture happy path
//! - Published body rejection
//! - Out-of-scope tool call rejected before dispatch
//! - Postcondition failure → InvocationFailed
//! - Ungrounded claim → CorpusGap emitted via VecGapSink
//! - Step budget enforcement
//!
//! No external LLM required — all tests are hermetic via `FixtureSynthesisLlm`.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use talos3d_core::curation::{
    authoring_script::{
        ArgExpr, AuthoringScript, McpToolId, MutationScope, OutputPath, Postcondition, Step, StepId,
    },
    identity::AssetKindId,
    replay::{
        PostconditionOracle, PostconditionVerdict, ResolvedPostcondition, ToolCall,
        ToolDispatchError, ToolDispatcher,
    },
    scope_trust::Trust,
    synthesis::{
        synthesize, FixtureSynthesisLlm, McpCall, SynthesisError, SynthesisPrompt, VecGapSink,
    },
};

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

struct EchoDispatcher {
    response: Value,
}

impl EchoDispatcher {
    fn new(response: Value) -> Self {
        Self { response }
    }
}

impl ToolDispatcher for EchoDispatcher {
    fn dispatch(&mut self, _call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
        Ok(self.response.clone())
    }
}

/// Records whether dispatch was called at all.
struct TrackingDispatcher {
    response: Value,
    pub call_count: usize,
}

impl TrackingDispatcher {
    fn new(response: Value) -> Self {
        Self {
            response,
            call_count: 0,
        }
    }
}

impl ToolDispatcher for TrackingDispatcher {
    fn dispatch(&mut self, _call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
        self.call_count += 1;
        Ok(self.response.clone())
    }
}

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

struct AlwaysFailOracle;

impl PostconditionOracle for AlwaysFailOracle {
    fn check(
        &self,
        _pc: &ResolvedPostcondition,
        _outputs: &BTreeMap<StepId, Map<String, Value>>,
        _params: &Map<String, Value>,
    ) -> PostconditionVerdict {
        PostconditionVerdict::Fail {
            reason: "test oracle rejects all".into(),
        }
    }
}

/// Build a minimal one-step script with `my_tool` in allowed_tools.
fn one_step_script() -> AuthoringScript {
    let mut s = AuthoringScript::stub(MutationScope::None);
    s.allowed_tools.insert(McpToolId::new("my_tool"));
    s.steps.push(Step {
        id: StepId::new("step1"),
        tool: McpToolId::new("my_tool"),
        args: BTreeMap::new(),
        bindings: {
            let mut b = BTreeMap::new();
            b.insert("result".into(), OutputPath::new("$.value"));
            b
        },
        essential: true,
        precondition: None,
    });
    s
}

fn prompt_for(script: &AuthoringScript) -> SynthesisPrompt {
    SynthesisPrompt {
        script: script.clone(),
        params: Map::new(),
        target_class: "test_element".into(),
        supported_refinement_states: vec!["conceptual".into()],
        world_state_snapshot: serde_json::json!({}),
    }
}

fn kind() -> AssetKindId {
    AssetKindId::new("recipe.v1")
}

// ---------------------------------------------------------------------------
// Test: Draft + fixture happy path
// ---------------------------------------------------------------------------

#[test]
fn draft_fixture_happy_path_steps_run_and_bindings_captured() {
    let script = one_step_script();
    let prompt = prompt_for(&script);

    // LLM emits one call to my_tool with x=42.
    let calls = vec![McpCall::new(
        "my_tool",
        serde_json::from_str(r#"{"x": 42}"#).unwrap(),
    )];
    let fixture = FixtureSynthesisLlm::new(vec![calls]);
    let sink = VecGapSink::new();
    let mut dispatcher = EchoDispatcher::new(serde_json::json!({"value": 99}));
    let oracle = AlwaysPassOracle;

    let report = synthesize(
        Trust::Draft,
        &script,
        prompt,
        None,
        &mut dispatcher,
        &oracle,
        &fixture,
        &sink,
        kind(),
        "test",
        0,
    )
    .unwrap();

    assert_eq!(report.steps_run, vec![StepId::new("step1")]);
    assert_eq!(
        report.outputs[&StepId::new("step1")]["result"],
        Value::from(99)
    );
    assert_eq!(sink.gap_count(), 0);
}

// ---------------------------------------------------------------------------
// Test: Published body is rejected with the expected message
// ---------------------------------------------------------------------------

#[test]
fn published_body_synthesis_rejected_with_typed_error() {
    let script = one_step_script();
    let prompt = prompt_for(&script);
    let fixture = FixtureSynthesisLlm::new(vec![]);
    let sink = VecGapSink::new();
    let mut dispatcher = EchoDispatcher::new(Value::Null);
    let oracle = AlwaysPassOracle;

    let err = synthesize(
        Trust::Published,
        &script,
        prompt,
        None,
        &mut dispatcher,
        &oracle,
        &fixture,
        &sink,
        kind(),
        "test",
        0,
    )
    .unwrap_err();

    assert_eq!(err, SynthesisError::SynthesisRejectedForPublished);
    assert!(
        err.to_string().contains("Published"),
        "error message should mention Published: {err}",
    );
}

// ---------------------------------------------------------------------------
// Test: Out-of-scope tool call rejected before any dispatch
// ---------------------------------------------------------------------------

#[test]
fn out_of_scope_tool_call_rejected_before_dispatch() {
    let script = one_step_script();
    let prompt = prompt_for(&script);

    // Emit a call to a tool not in allowed_tools.
    let bad_calls = vec![McpCall::new("forbidden_tool", Map::new())];
    let fixture = FixtureSynthesisLlm::new(vec![bad_calls]);
    let sink = VecGapSink::new();
    let mut dispatcher = TrackingDispatcher::new(Value::Null);
    let oracle = AlwaysPassOracle;

    let err = synthesize(
        Trust::Draft,
        &script,
        prompt,
        None,
        &mut dispatcher,
        &oracle,
        &fixture,
        &sink,
        kind(),
        "test",
        0,
    )
    .unwrap_err();

    assert!(matches!(
        err,
        SynthesisError::EmittedCallToolNotAllowed { .. }
    ));
    assert_eq!(
        dispatcher.call_count, 0,
        "forbidden tool call must not reach the dispatcher"
    );
}

// ---------------------------------------------------------------------------
// Test: Postcondition failure → InvocationFailed with specific postcondition
// ---------------------------------------------------------------------------

#[test]
fn postcondition_failure_surfaces_as_invocation_failed() {
    let mut script = one_step_script();
    script.postconditions.push(Postcondition::Relation {
        relation_kind: "test_rel".into(),
        from: ArgExpr::Literal {
            value: Value::from("a"),
        },
        to: ArgExpr::Literal {
            value: Value::from("b"),
        },
    });
    let prompt = prompt_for(&script);

    let calls = vec![McpCall::new("my_tool", Map::new())];
    let fixture = FixtureSynthesisLlm::new(vec![calls]);
    let sink = VecGapSink::new();
    let mut dispatcher = EchoDispatcher::new(serde_json::json!({"value": 1}));
    let oracle = AlwaysFailOracle;

    let err = synthesize(
        Trust::Draft,
        &script,
        prompt,
        None,
        &mut dispatcher,
        &oracle,
        &fixture,
        &sink,
        kind(),
        "test",
        0,
    )
    .unwrap_err();

    match err {
        SynthesisError::InvocationFailed(inv_err) => {
            assert!(
                inv_err.to_string().contains("postcondition"),
                "invocation error should mention postcondition: {inv_err}",
            );
        }
        other => panic!("expected InvocationFailed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test: UngroundedClaim → CorpusGap enqueued via GapSink
// ---------------------------------------------------------------------------

#[test]
fn ungrounded_claim_enqueues_corpus_gap_and_returns_error() {
    let script = one_step_script();
    let prompt = prompt_for(&script);

    let llm = FixtureSynthesisLlm::ungrounded("bbr_stair_rule_pack");
    let sink = VecGapSink::new();
    let mut dispatcher = EchoDispatcher::new(Value::Null);
    let oracle = AlwaysPassOracle;

    let err = synthesize(
        Trust::Draft,
        &script,
        prompt,
        None,
        &mut dispatcher,
        &oracle,
        &llm,
        &sink,
        kind(),
        "agent:test",
        0,
    )
    .unwrap_err();

    match err {
        SynthesisError::UngroundedClaim {
            missing_artifact_kind,
            gap_id,
        } => {
            assert_eq!(missing_artifact_kind, "bbr_stair_rule_pack");
            assert!(gap_id.starts_with("gap-"), "gap_id = {gap_id}");
        }
        other => panic!("expected UngroundedClaim, got {other:?}"),
    }

    // Sink should have recorded the gap.
    assert_eq!(sink.gap_count(), 1);
    let gaps = sink.into_gaps();
    assert_eq!(gaps[0].missing_artifact_kind, "bbr_stair_rule_pack");
    assert_eq!(gaps[0].kind.as_str(), "recipe.v1");
}

// ---------------------------------------------------------------------------
// Test: Step budget enforcement
// ---------------------------------------------------------------------------

#[test]
fn step_budget_exceeded_before_dispatch() {
    let script = one_step_script();
    let prompt = prompt_for(&script);

    // Emit more calls than the budget allows.
    let calls: Vec<McpCall> = (0..5)
        .map(|_| McpCall::new("my_tool", Map::new()))
        .collect();
    let fixture = FixtureSynthesisLlm::new(vec![calls]);
    let sink = VecGapSink::new();
    let mut dispatcher = TrackingDispatcher::new(Value::Null);
    let oracle = AlwaysPassOracle;

    let err = synthesize(
        Trust::Draft,
        &script,
        prompt,
        Some(3),
        &mut dispatcher,
        &oracle,
        &fixture,
        &sink,
        kind(),
        "test",
        0,
    )
    .unwrap_err();

    assert!(matches!(
        err,
        SynthesisError::StepBudgetExceeded { limit: 3 }
    ));
    // Budget check is before dispatch.
    assert_eq!(dispatcher.call_count, 0);
}
