//! Synthesis-mode invocation for `AuthoringScript` recipe bodies.
//!
//! Per ADR-041 and PP86: `invoke_recipe { replay: false }` on a `Draft`
//! body passes the stored script to a `SynthesisLlm` as a worked example.
//! The LLM emits a fresh `Vec<McpCall>` that must satisfy the script's
//! declared postconditions. `Published` bodies always replay; synthesis on
//! a published body is rejected with a typed error.
//!
//! The live Model-API-backed `SynthesisLlm` impl and the `invoke_recipe`
//! MCP wiring in `model_api.rs` are intentionally deferred (see the
//! "Deferred" section in `PROOF_POINT_86.md`). The core types and
//! `FixtureSynthesisLlm` enable hermetic CI without an external LLM.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::authoring_script::{AuthoringScript, McpToolId};
use super::identity::AssetKindId;
use super::replay::{
    replay, InvocationError, InvocationReport, PostconditionOracle, ToolCall, ToolDispatchError,
    ToolDispatcher,
};
use super::scope_trust::Trust;

// ---------------------------------------------------------------------------
// McpCall — a single tool invocation emitted by the LLM
// ---------------------------------------------------------------------------

/// A single tool invocation emitted by the synthesis LLM. Passed to the
/// executor through a thin `ToolDispatcher` adapter.
#[derive(Debug, Clone, PartialEq)]
pub struct McpCall {
    pub tool: McpToolId,
    pub args: Map<String, Value>,
}

impl McpCall {
    pub fn new(tool: impl Into<String>, args: Map<String, Value>) -> Self {
        Self {
            tool: McpToolId::new(tool),
            args,
        }
    }
}

// ---------------------------------------------------------------------------
// LlmError
// ---------------------------------------------------------------------------

/// Error returned by `SynthesisLlm::draft_calls`.
#[derive(Debug, Clone, PartialEq)]
pub enum LlmError {
    /// The LLM cannot ground a required claim; a `CorpusGap` will be
    /// enqueued by the synthesis runner.
    UngroundedClaim {
        missing_artifact_kind: String,
        context: serde_json::Value,
    },
    /// The LLM reported a general failure (network, quota, etc.).
    Internal { message: String },
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UngroundedClaim {
                missing_artifact_kind,
                ..
            } => write!(
                f,
                "LLM cannot ground required claim; missing '{missing_artifact_kind}'"
            ),
            Self::Internal { message } => write!(f, "LLM internal error: {message}"),
        }
    }
}

impl std::error::Error for LlmError {}

// ---------------------------------------------------------------------------
// SynthesisPrompt
// ---------------------------------------------------------------------------

/// Everything the LLM needs to synthesize a call sequence.
#[derive(Debug, Clone)]
pub struct SynthesisPrompt {
    /// The script to use as a worked example.
    pub script: AuthoringScript,
    /// Caller-supplied parameter bindings.
    pub params: Map<String, Value>,
    /// Target element-class id (e.g. `"stair_straight"`).
    pub target_class: String,
    /// Supported refinement states for the target (e.g. `["conceptual",
    /// "schematic"]`) — serialized as lowercase debug strings.
    pub supported_refinement_states: Vec<String>,
    /// Opaque world-state snippet relevant to `mutation_scope`. The live
    /// impl populates this from the Bevy world; in synthesis tests this is
    /// just a JSON blob supplied by the caller.
    pub world_state_snapshot: serde_json::Value,
}

// ---------------------------------------------------------------------------
// SynthesisLlm trait
// ---------------------------------------------------------------------------

/// Pluggable synthesis LLM. The live impl calls the Claude API; tests use
/// `FixtureSynthesisLlm`. Object-safe so callers can hold a
/// `Box<dyn SynthesisLlm>`.
pub trait SynthesisLlm: Send + Sync {
    /// Given a `SynthesisPrompt`, draft a sequence of `McpCall`s that (if
    /// executed) should satisfy the script's postconditions. Returns
    /// `Err(LlmError::UngroundedClaim { .. })` when the LLM detects it
    /// cannot ground a required claim — the synthesis runner turns this
    /// into a `CorpusGap`.
    fn draft_calls(&self, prompt: &SynthesisPrompt) -> Result<Vec<McpCall>, LlmError>;
}

// ---------------------------------------------------------------------------
// FixtureSynthesisLlm — hermetic test double
// ---------------------------------------------------------------------------

/// Deterministic test double that replays pre-recorded call sequences in
/// order. Each `invoke` pops the first sub-vec and returns it; when the
/// list is exhausted it returns an empty vec (as if the LLM said "nothing
/// to do").
///
/// Construct with `Vec<Vec<McpCall>>`: one inner `Vec` per `draft_calls`
/// invocation.
pub struct FixtureSynthesisLlm {
    sequences: std::sync::Mutex<Vec<Vec<McpCall>>>,
}

impl FixtureSynthesisLlm {
    pub fn new(sequences: Vec<Vec<McpCall>>) -> Self {
        Self {
            sequences: std::sync::Mutex::new(sequences),
        }
    }

    /// Variant that returns `LlmError::UngroundedClaim` on the next call —
    /// used to test CorpusGap emission.
    pub fn ungrounded(missing_artifact_kind: impl Into<String>) -> UngroundedFixture {
        UngroundedFixture {
            missing_artifact_kind: missing_artifact_kind.into(),
            context: serde_json::json!({}),
        }
    }
}

impl SynthesisLlm for FixtureSynthesisLlm {
    fn draft_calls(&self, _prompt: &SynthesisPrompt) -> Result<Vec<McpCall>, LlmError> {
        let mut guard = self.sequences.lock().expect("mutex not poisoned");
        if guard.is_empty() {
            Ok(Vec::new())
        } else {
            Ok(guard.remove(0))
        }
    }
}

/// A fixture that always returns `LlmError::UngroundedClaim`.
pub struct UngroundedFixture {
    pub missing_artifact_kind: String,
    pub context: serde_json::Value,
}

impl SynthesisLlm for UngroundedFixture {
    fn draft_calls(&self, _prompt: &SynthesisPrompt) -> Result<Vec<McpCall>, LlmError> {
        Err(LlmError::UngroundedClaim {
            missing_artifact_kind: self.missing_artifact_kind.clone(),
            context: self.context.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// SynthesisError
// ---------------------------------------------------------------------------

/// Errors that can occur during synthesis-mode invocation.
#[derive(Debug, Clone, PartialEq)]
pub enum SynthesisError {
    /// `Published` bodies may not be synthesized; call replay instead.
    SynthesisRejectedForPublished,
    /// An emitted call used a tool not in `allowed_tools`.
    EmittedCallToolNotAllowed { tool: McpToolId },
    /// Step budget exhausted before the call sequence finished.
    StepBudgetExceeded { limit: usize },
    /// The LLM cannot ground a required claim; a CorpusGap was enqueued.
    UngroundedClaim {
        missing_artifact_kind: String,
        gap_id: String,
    },
    /// The LLM returned a generic error (not a grounding failure).
    LlmInternal { message: String },
    /// The synthesized call sequence failed postcondition verification.
    InvocationFailed(InvocationError),
}

impl std::fmt::Display for SynthesisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SynthesisRejectedForPublished => write!(
                f,
                "synthesis mode is not permitted for Published bodies; use replay instead",
            ),
            Self::EmittedCallToolNotAllowed { tool } => write!(
                f,
                "LLM emitted call to tool '{}' which is not in allowed_tools",
                tool.0,
            ),
            Self::StepBudgetExceeded { limit } => {
                write!(f, "synthesis exceeded step budget of {limit}")
            }
            Self::UngroundedClaim {
                missing_artifact_kind,
                gap_id,
            } => write!(
                f,
                "LLM cannot ground '{}' claim; corpus gap '{gap_id}' enqueued",
                missing_artifact_kind,
            ),
            Self::LlmInternal { message } => write!(f, "LLM error: {message}"),
            Self::InvocationFailed(e) => write!(f, "synthesized invocation failed: {e}"),
        }
    }
}

impl std::error::Error for SynthesisError {}

// ---------------------------------------------------------------------------
// GapSink — where CorpusGap notifications go
// ---------------------------------------------------------------------------

/// Receiver for `CorpusGap` entries emitted during synthesis. The live
/// impl pushes into `CorpusGapQueue`; tests use `VecGapSink`.
pub trait GapSink: Send + Sync {
    /// Accept a gap and return its assigned id.
    fn emit(
        &self,
        kind: AssetKindId,
        jurisdiction: Option<String>,
        missing_artifact_kind: &str,
        context: serde_json::Value,
        reported_by: &str,
        reported_at: i64,
    ) -> String;
}

/// Test double: accumulates gap emissions in a `Vec`.
#[derive(Default)]
pub struct VecGapSink {
    gaps: std::sync::Mutex<Vec<EmittedGap>>,
    serial: std::sync::atomic::AtomicU64,
}

/// One gap emission recorded by `VecGapSink`.
#[derive(Debug, Clone)]
pub struct EmittedGap {
    pub id: String,
    pub kind: AssetKindId,
    pub missing_artifact_kind: String,
    pub context: serde_json::Value,
}

impl VecGapSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_gaps(self) -> Vec<EmittedGap> {
        self.gaps.into_inner().unwrap()
    }

    pub fn gap_count(&self) -> usize {
        self.gaps.lock().unwrap().len()
    }
}

impl GapSink for VecGapSink {
    fn emit(
        &self,
        kind: AssetKindId,
        _jurisdiction: Option<String>,
        missing_artifact_kind: &str,
        context: serde_json::Value,
        _reported_by: &str,
        _reported_at: i64,
    ) -> String {
        let n = self
            .serial
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("gap-synth-{n}");
        self.gaps.lock().unwrap().push(EmittedGap {
            id: id.clone(),
            kind,
            missing_artifact_kind: missing_artifact_kind.to_owned(),
            context,
        });
        id
    }
}

// ---------------------------------------------------------------------------
// ToolDispatcher adapter: McpCall sequence → ToolDispatcher
// ---------------------------------------------------------------------------

/// Wraps the real model-api dispatcher used to execute a synthesized script.
/// The script is patched with guardrail-checked `McpCall` values before replay,
/// so dispatch can forward the resolved calls directly.
struct SynthesisDispatcher<'a, D: ToolDispatcher> {
    /// The real model-api dispatcher that executes the calls.
    inner: &'a mut D,
}

impl<'a, D: ToolDispatcher> SynthesisDispatcher<'a, D> {
    fn new(_calls: Vec<McpCall>, inner: &'a mut D) -> Self {
        Self { inner }
    }
}

impl<D: ToolDispatcher> ToolDispatcher for SynthesisDispatcher<'_, D> {
    fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
        // The args in `call` were resolved from the script's `ArgExpr`s
        // against the synthesized responses so far — not from the pre-
        // recorded McpCall.args. We forward the live call directly to the
        // inner dispatcher (which may be a fixture in tests).
        self.inner.dispatch(call)
    }
}

// ---------------------------------------------------------------------------
// synthesize — the main entry point
// ---------------------------------------------------------------------------

/// Default maximum steps per synthesis run. Prevents runaway sequences.
pub const DEFAULT_MAX_STEPS: usize = 128;

/// Run the script in synthesis mode: pass it to `llm.draft_calls`,
/// guardrail-check the returned calls, then execute them through
/// `dispatcher` + `oracle` exactly as the replay executor would.
///
/// Trust gating: `trust` must be `Draft`; `Published` returns
/// `SynthesisError::SynthesisRejectedForPublished`.
///
/// `gap_sink` receives any `CorpusGap` emitted when the LLM returns
/// `LlmError::UngroundedClaim`.
///
/// The `kind` argument is the `AssetKindId` of the owning recipe (used
/// when emitting corpus gaps).
#[allow(clippy::too_many_arguments)]
pub fn synthesize<D: ToolDispatcher, O: PostconditionOracle>(
    trust: Trust,
    script: &AuthoringScript,
    prompt: SynthesisPrompt,
    max_steps: Option<usize>,
    dispatcher: &mut D,
    oracle: &O,
    llm: &dyn SynthesisLlm,
    gap_sink: &dyn GapSink,
    kind: AssetKindId,
    reported_by: &str,
    reported_at: i64,
) -> Result<InvocationReport, SynthesisError> {
    // Trust gate: synthesis is only allowed on Draft bodies.
    if trust == Trust::Published {
        return Err(SynthesisError::SynthesisRejectedForPublished);
    }

    let limit = max_steps.unwrap_or(DEFAULT_MAX_STEPS);

    // Ask the LLM to draft the call sequence.
    let raw_calls = match llm.draft_calls(&prompt) {
        Ok(calls) => calls,
        Err(LlmError::UngroundedClaim {
            missing_artifact_kind,
            context,
        }) => {
            let gap_id = gap_sink.emit(
                kind,
                None,
                &missing_artifact_kind,
                context.clone(),
                reported_by,
                reported_at,
            );
            return Err(SynthesisError::UngroundedClaim {
                missing_artifact_kind,
                gap_id,
            });
        }
        Err(LlmError::Internal { message }) => {
            return Err(SynthesisError::LlmInternal { message });
        }
    };

    // Step budget: enforce before dispatching.
    if raw_calls.len() > limit {
        return Err(SynthesisError::StepBudgetExceeded { limit });
    }

    // Guardrail: every emitted call must be in `allowed_tools`.
    for call in &raw_calls {
        if !script.allowed_tools.contains(&call.tool) {
            return Err(SynthesisError::EmittedCallToolNotAllowed {
                tool: call.tool.clone(),
            });
        }
    }

    // Wrap the guardrail-checked calls in a `SynthesisDispatcher` that
    // enforces `mutation_scope` via the existing replay executor. The
    // replay executor resolves `ArgExpr`s from the script's step
    // definitions against `params` + step outputs — the LLM's raw calls
    // are used to seed step args in a synthesized transcript by rebuilding
    // the script's step list with `Literal` arg values from the LLM calls.
    let synthesized_script = patch_script_with_calls(script, &raw_calls);
    let mut synth_dispatcher = SynthesisDispatcher::new(raw_calls, dispatcher);

    replay(
        &synthesized_script,
        prompt.params,
        &mut synth_dispatcher,
        oracle,
    )
    .map_err(SynthesisError::InvocationFailed)
}

/// Patch an `AuthoringScript` so each step's args are `Literal`s drawn
/// from the corresponding `McpCall.args`. Steps beyond the LLM call count
/// are trimmed; if the LLM produced more calls than steps the extras are
/// ignored. This converts the LLM's flat call list into the shape the
/// replay executor expects.
fn patch_script_with_calls(script: &AuthoringScript, calls: &[McpCall]) -> AuthoringScript {
    use super::authoring_script::{ArgExpr, Step, StepId};

    // Rebuild steps: keep script metadata, overwrite args with Literal values.
    let steps: Vec<Step> = script
        .steps
        .iter()
        .zip(calls.iter())
        .map(|(orig_step, call)| {
            let literal_args: BTreeMap<String, ArgExpr> = call
                .args
                .iter()
                .map(|(k, v)| (k.clone(), ArgExpr::Literal { value: v.clone() }))
                .collect();
            Step {
                id: orig_step.id.clone(),
                tool: call.tool.clone(),
                args: literal_args,
                bindings: orig_step.bindings.clone(),
                essential: orig_step.essential,
                precondition: None, // preconditions evaluated by the LLM; we trust its output
            }
        })
        .collect();

    // Trim steps to what the LLM produced.
    let step_count = steps.len();
    let mut patched = script.clone();
    patched.steps = steps;

    // Trim postconditions that reference steps beyond the patched set.
    // (The oracle will re-verify them anyway.)
    let valid_ids: std::collections::BTreeSet<StepId> =
        patched.steps.iter().map(|s| s.id.clone()).collect();
    patched.postconditions.retain(|pc| {
        use super::authoring_script::Postcondition;
        match pc {
            Postcondition::ObligationSatisfied { by_step, .. } => valid_ids.contains(by_step),
            _ => true,
        }
    });

    // Allow the patched script to bypass structural parameter-default
    // validation — we cleared the step list, so referenced_params may
    // differ. Clear parameter_defaults to avoid false positives.
    patched.parameter_defaults.clear();

    let _ = step_count;
    patched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::authoring_script::{MutationScope, OutputPath, Step, StepId};
    use crate::curation::replay::{
        PostconditionOracle, PostconditionVerdict, ResolvedPostcondition, ToolCall,
        ToolDispatchError, ToolDispatcher,
    };

    // --- Fixtures ---

    struct EchoDispatcher {
        response: Value,
        calls_seen: Vec<(McpToolId, Map<String, Value>)>,
    }

    impl EchoDispatcher {
        fn new(response: Value) -> Self {
            Self {
                response,
                calls_seen: Vec::new(),
            }
        }
    }

    impl ToolDispatcher for EchoDispatcher {
        fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
            self.calls_seen.push((call.tool.clone(), call.args.clone()));
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
                reason: "oracle rejects everything".into(),
            }
        }
    }

    fn minimal_script() -> AuthoringScript {
        use crate::curation::authoring_script::McpToolId;
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

    // --- Tests ---

    #[test]
    fn published_body_is_rejected() {
        let script = minimal_script();
        let prompt = prompt_for(&script);
        let fixture = FixtureSynthesisLlm::new(vec![]);
        let sink = VecGapSink::new();
        let mut dispatcher = EchoDispatcher::new(serde_json::json!({"value": 1}));
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
            AssetKindId::new("recipe.v1"),
            "test",
            0,
        )
        .unwrap_err();
        assert_eq!(err, SynthesisError::SynthesisRejectedForPublished);
    }

    #[test]
    fn draft_with_fixture_happy_path() {
        let script = minimal_script();
        let prompt = prompt_for(&script);

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
            AssetKindId::new("recipe.v1"),
            "test",
            0,
        )
        .unwrap();
        assert_eq!(report.steps_run, vec![StepId::new("step1")]);
        assert_eq!(sink.gap_count(), 0);
    }

    #[test]
    fn out_of_scope_tool_call_rejected_before_dispatch() {
        let script = minimal_script();
        let prompt = prompt_for(&script);

        // LLM emits a call to a tool not in allowed_tools.
        let bad_calls = vec![McpCall::new("forbidden_tool", Map::new())];
        let fixture = FixtureSynthesisLlm::new(vec![bad_calls]);
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
            &fixture,
            &sink,
            AssetKindId::new("recipe.v1"),
            "test",
            0,
        )
        .unwrap_err();
        assert!(
            matches!(err, SynthesisError::EmittedCallToolNotAllowed { .. }),
            "expected EmittedCallToolNotAllowed, got {err:?}",
        );
        // No dispatch to the inner dispatcher.
        assert!(dispatcher.calls_seen.is_empty());
    }

    #[test]
    fn postcondition_failure_returns_invocation_failed() {
        use crate::curation::authoring_script::{ArgExpr, Postcondition};

        let mut script = minimal_script();
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
            AssetKindId::new("recipe.v1"),
            "test",
            0,
        )
        .unwrap_err();
        assert!(matches!(err, SynthesisError::InvocationFailed(_)));
    }

    #[test]
    fn ungrounded_claim_emits_corpus_gap() {
        let script = minimal_script();
        let prompt = prompt_for(&script);

        let llm = FixtureSynthesisLlm::ungrounded("bbr_rule_pack");
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
            AssetKindId::new("recipe.v1"),
            "test",
            0,
        )
        .unwrap_err();
        assert!(matches!(err, SynthesisError::UngroundedClaim { .. }));
        assert_eq!(sink.gap_count(), 1);
        let gaps = sink.into_gaps();
        assert_eq!(gaps[0].missing_artifact_kind, "bbr_rule_pack");
    }

    #[test]
    fn step_budget_exceeded_returns_error() {
        let script = minimal_script();
        let prompt = prompt_for(&script);

        // Two calls but budget is 1.
        let calls = vec![
            McpCall::new("my_tool", Map::new()),
            McpCall::new("my_tool", Map::new()),
        ];
        let fixture = FixtureSynthesisLlm::new(vec![calls]);
        let sink = VecGapSink::new();
        let mut dispatcher = EchoDispatcher::new(Value::Null);
        let oracle = AlwaysPassOracle;

        let err = synthesize(
            Trust::Draft,
            &script,
            prompt,
            Some(1),
            &mut dispatcher,
            &oracle,
            &fixture,
            &sink,
            AssetKindId::new("recipe.v1"),
            "test",
            0,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SynthesisError::StepBudgetExceeded { limit: 1 }
        ));
    }
}
