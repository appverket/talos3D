//! Semantic Procedural Session — stateful AI-facing authoring substrate.
//!
//! Per ADR-051 and `private/proposals/SEMANTIC_PROCEDURAL_SESSION_AGREEMENT.md`.
//!
//! A `ProceduralSession` is an in-flight [`AuthoringScript`] with live
//! step-output bindings, accumulated obligations and findings, append-only
//! audit, and scoped replay rights. The session is **not** a new IR — it
//! reuses [`AuthoringScript`], [`MutationScope`], and [`replay()`] from the
//! curation substrate, and commits through the existing
//! `PendingCommandQueue` (ADR-002 / ADR-011) via a caller-supplied
//! `ToolDispatcher`.
//!
//! See:
//!
//! - [`super::authoring_script::AuthoringScript`] — the durable body shape.
//! - [`super::replay::replay`] — the deterministic executor reused for both
//!   dry-run and commit.
//! - [`PP-SPS-1`..`PP-SPS-6`] in `private/proof_points/` — the
//!   implementation slate.
//!
//! ## Lifecycle
//!
//! ```text
//! create(spec) -> SessionId
//!   eval(session, step, mode)    [bind_only | dry_run | dry_run_and_bind]
//!   ...
//!   snapshot(session) -> SessionSnapshot
//!   commit(session, policy, &mut dispatcher) -> CommitReport
//!   export(session, target, metadata) -> ExportHandle
//! close(session)
//! ```
//!
//! ## Out of v1
//!
//! - new IR or parallel session-only script language
//! - generic transactional diff at commit
//! - parallel namespace discovery API (use existing channels)
//! - scenario / recipe-draft / validator export targets (see PP-SPS-5)
//! - bridging session-local obligations to capability-defined validators
//!   (a follow-on PP; v1 carries minimal in-session obligation tracking)
//! - real-world rmcp wiring of the five MCP tools (see PP-SPS-3 plugin)

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::authoring_script::{
    ArgExpr, AuthoringScript, AuthoringScriptStructuralError, McpToolId, MutationScope, OutputPath,
    Postcondition, Predicate, ScriptInstruction, Step, StepId,
};
use super::identity::{AssetId, AssetKindId, AssetRevision};
use super::replay::{
    replay, InvocationError, PostconditionOracle, ResolvedPostcondition, ToolCall,
    ToolDispatchError, ToolDispatcher,
};
use super::scope_trust::{Scope, Trust};

// =====================================================================
// Identity
// =====================================================================

/// Stable identifier for a procedural session, content-friendly and
/// unique across processes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// =====================================================================
// Declarations supplied at create() time
// =====================================================================

/// Declared stage transition the session intends to perform. Anchors the
/// commit boundary; verified at commit-time against the target's actual
/// refinement state when validators are bridged in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StageTransition {
    /// Net-new entity: nothing to refined.
    NewConceptual,
    ConceptualToSchematic,
    SchematicToConstructible,
    /// Pure-query / validator-style session. No state should mutate.
    PureQuery,
}

/// Declared inputs at session creation time. See ADR-051 §3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SessionSpec {
    /// Refinement target (the authored entity being refined), if any.
    /// Carried opaquely as a JSON value so the substrate stays generic;
    /// capability crates can serialize their `EntityId` types into it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refinement_target: Option<Value>,
    pub stage_transition: StageTransition,
    pub mutation_scope: MutationScope,
    /// Closed set of MCP tools / capability commands this session may
    /// dispatch. Enforced at eval-time. Mirrors `AuthoringScript.allowed_tools`.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub allowed_tools: BTreeSet<McpToolId>,
    /// Deterministic seed for any later sampling-based capability calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Parameter schema for the accumulated `AuthoringScript`. Defaults
    /// to an empty object schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_schema: Option<Value>,
}

impl SessionSpec {
    /// Build a minimal spec for a refinement-subtree session.
    pub fn for_refinement(
        target: Value,
        stage_transition: StageTransition,
        root_element_param: impl Into<String>,
        allowed_tools: BTreeSet<McpToolId>,
    ) -> Self {
        Self {
            refinement_target: Some(target),
            stage_transition,
            mutation_scope: MutationScope::RefinementSubtree {
                root_element_param: root_element_param.into(),
            },
            allowed_tools,
            seed: None,
            parameter_schema: None,
        }
    }

    /// Build a minimal spec for authoring a new top-level structure from
    /// scratch (no refinement target). Uses [`MutationScope::ProjectRoot`].
    pub fn for_new_structure(allowed_tools: BTreeSet<McpToolId>) -> Self {
        Self {
            refinement_target: None,
            stage_transition: StageTransition::NewConceptual,
            mutation_scope: MutationScope::ProjectRoot,
            allowed_tools,
            seed: None,
            parameter_schema: None,
        }
    }
}

// =====================================================================
// Eval input / output
// =====================================================================

/// What the caller hands to `eval`: a step description in the same shape
/// as a [`Step`] from `AuthoringScript`. The session id, ordering, and
/// scope are session-owned; the caller cannot override them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct EvalStep {
    pub id: StepId,
    pub tool: McpToolId,
    #[serde(default)]
    pub args: BTreeMap<String, ArgExpr>,
    #[serde(default)]
    pub bindings: BTreeMap<String, OutputPath>,
    #[serde(default = "default_true")]
    pub essential: bool,
    #[serde(default)]
    pub precondition: Option<Predicate>,
}

fn default_true() -> bool {
    true
}

impl From<EvalStep> for Step {
    fn from(s: EvalStep) -> Self {
        Step {
            id: s.id,
            tool: s.tool,
            args: s.args,
            bindings: s.bindings,
            essential: s.essential,
            precondition: s.precondition,
        }
    }
}

impl From<EvalStep> for crate::curation::ScriptInstruction {
    fn from(s: EvalStep) -> Self {
        crate::curation::ScriptInstruction::Call(Step::from(s))
    }
}

/// Eval modes per the agreement and PP-SPS-2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EvalMode {
    /// Type-check + append step. Do not dispatch.
    BindOnly,
    /// Dispatch through a dry-run dispatcher; do NOT append; project
    /// expected effects.
    DryRun,
    /// Dry-run dispatch + append.
    DryRunAndBind,
}

/// Captured projection of a step's expected effect under `DryRun` /
/// `DryRunAndBind`. Holds the resolved tool call (so the caller can see
/// what would have been dispatched), the stub response value, and any
/// obligations / findings the session projected for the step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct DryRunProjection {
    pub tool: McpToolId,
    pub resolved_args: Map<String, Value>,
    pub stub_response: Value,
    pub projected_obligations: Vec<SessionObligation>,
    pub projected_findings: Vec<SessionFinding>,
}

/// Outcome of an `eval` call. `appended` is true iff the step was
/// appended to the session's accumulated script (BindOnly /
/// DryRunAndBind).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct EvalReport {
    pub step_id: StepId,
    pub mode: EvalMode,
    pub appended: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<DryRunProjection>,
    /// Wall-clock spent inside the eval call, milliseconds.
    pub elapsed_ms: u128,
}

// =====================================================================
// Session-local obligation / finding / waiver types
// =====================================================================
//
// These are minimal v1 bridges to capability-defined validator outputs.
// They are intentionally JSON-shaped so capability crates can serialize
// their own richer types into them without forcing a core dependency.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SessionObligation {
    pub id: String,
    pub kind: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum FindingSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SessionFinding {
    pub id: String,
    pub severity: FindingSeverity,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct Waiver {
    /// Targets a `SessionFinding.id`.
    pub finding_id: String,
    pub justification: String,
}

// =====================================================================
// Audit log
// =====================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEvent {
    Created,
    Evaluated {
        step_id: StepId,
        mode: EvalMode,
        appended: bool,
    },
    DryRunRejected {
        step_id: StepId,
        reason: String,
    },
    Snapshotted,
    Committed {
        steps_run: usize,
        policy: CommitPolicy,
        remaining_obligations: usize,
        remaining_findings: usize,
    },
    CommitRejected {
        policy: CommitPolicy,
        reason: String,
    },
    Exported {
        target: ExportTarget,
        asset_id: AssetId,
    },
    QuotaExceeded {
        which: String,
    },
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AuditEntry {
    pub event: AuditEvent,
    /// Monotonic sequence number within the session. Starts at 0 for
    /// `Created`.
    pub seq: u64,
    /// Milliseconds since session creation.
    pub at_ms: u128,
}

// =====================================================================
// Commit policy / report
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CommitPolicy {
    /// Fail closed if any obligation or finding remains.
    RequireClean,
    /// Allow commit if every remaining finding has an explicit waiver.
    AcceptWithWaivers,
    /// Allow commit; emit remaining obligations as carry-over on the
    /// refinement target for a later session.
    AcceptPartial,
}

/// Logged in `CommitReport` for partial-accept commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CarriedOverObligation {
    pub obligation: SessionObligation,
    pub onto_target: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CommitReport {
    pub session_id: SessionId,
    pub policy: CommitPolicy,
    pub steps_run: Vec<StepId>,
    pub steps_skipped: Vec<StepId>,
    /// Step outputs captured during commit, by step id. Maps to the
    /// underlying `InvocationReport.outputs`.
    pub outputs: BTreeMap<StepId, Map<String, Value>>,
    /// Tool-call records emitted at commit time, in dispatch order.
    /// Each carries the tool id and a session-tagged command id the
    /// commit dispatcher allocated for grouping; this is the hook for
    /// future grouped session-undo work.
    pub tagged_calls: Vec<TaggedCommit>,
    pub post_commit_findings: Vec<SessionFinding>,
    pub remaining_obligations: Vec<SessionObligation>,
    pub carried_over: Vec<CarriedOverObligation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_handle: Option<ExportHandle>,
}

/// One command enqueued at commit time, tagged with its originating
/// session id for optional grouped session undo (a future UI concern).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct TaggedCommit {
    pub session_id: SessionId,
    pub step_id: StepId,
    pub tool: McpToolId,
    /// A monotonic identifier the commit dispatcher assigns to each
    /// enqueued command. Substrate-level; the live impl maps these to
    /// `PendingCommandQueue` entries.
    pub command_id: u64,
}

// =====================================================================
// Export
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ExportTarget {
    /// v1 — freeze the in-flight script as a durable `AuthoringScript`
    /// curated asset.
    AuthoringScript,
}

/// Caller-supplied metadata for an export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ExportMetadata {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Optional postconditions to attach to the exported script beyond
    /// what was inferred from session obligations.
    #[serde(default)]
    pub additional_postconditions: Vec<Postcondition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ExportHandle {
    pub asset_id: AssetId,
    pub kind: AssetKindId,
    pub revision: AssetRevision,
    pub scope: Scope,
    pub trust: Trust,
    pub target: ExportTarget,
}

/// The frozen artifact returned alongside its handle. Carried inside
/// the registry so callers can read it back without a separate registry
/// lookup in v1.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedAuthoringScript {
    pub handle: ExportHandle,
    pub script: AuthoringScript,
    pub name: String,
    pub description: String,
}

// =====================================================================
// Errors
// =====================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum SessionError {
    UnknownSession(SessionId),
    UnknownTool {
        tool: McpToolId,
    },
    DisallowedTool {
        tool: McpToolId,
    },
    DuplicateStepId(StepId),
    InvalidStructure(AuthoringScriptStructuralError),
    QuotaStepCount {
        limit: usize,
    },
    QuotaWallClock {
        which: &'static str,
    },
    QuotaBindingMemory {
        bytes: usize,
        limit: usize,
    },
    MutationOutOfScope {
        step: StepId,
        reason: String,
    },
    Dispatch {
        step: StepId,
        error: ToolDispatchError,
    },
    BindingPathMissing {
        step: StepId,
        binding: String,
        path: OutputPath,
    },
    ParameterSchema {
        message: String,
    },
    CommitNotClean {
        remaining_obligations: usize,
        remaining_findings: usize,
    },
    CommitMissingWaivers {
        missing: Vec<String>,
    },
    ExportEmpty,
    InternalInvocation(InvocationError),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSession(s) => write!(f, "unknown session '{}'", s.0),
            Self::UnknownTool { tool } => {
                write!(f, "tool '{}' has no descriptor registered", tool.0)
            }
            Self::DisallowedTool { tool } => write!(
                f,
                "tool '{}' is not in this session's allowed_tools set",
                tool.0
            ),
            Self::DuplicateStepId(id) => write!(f, "step id '{}' already used in session", id.0),
            Self::InvalidStructure(e) => write!(f, "invalid step structure: {e}"),
            Self::QuotaStepCount { limit } => {
                write!(f, "session step-count quota exceeded (limit {limit})")
            }
            Self::QuotaWallClock { which } => write!(f, "{which} wall-clock quota exceeded"),
            Self::QuotaBindingMemory { bytes, limit } => write!(
                f,
                "binding memory quota exceeded: {bytes} bytes > limit {limit}",
            ),
            Self::MutationOutOfScope { step, reason } => write!(
                f,
                "step '{}' is outside the session's MutationScope: {reason}",
                step.0
            ),
            Self::Dispatch { step, error } => write!(
                f,
                "step '{}' dispatch failed: {} ({})",
                step.0, error.code, error.message
            ),
            Self::BindingPathMissing {
                step,
                binding,
                path,
            } => write!(
                f,
                "step '{}' binding '{}' path '{}' missing in response",
                step.0, binding, path.0,
            ),
            Self::ParameterSchema { message } => {
                write!(f, "parameter schema validation failed: {message}")
            }
            Self::CommitNotClean {
                remaining_obligations,
                remaining_findings,
            } => write!(
                f,
                "commit refused: {remaining_obligations} obligations and \
                 {remaining_findings} findings remain (policy require_clean)"
            ),
            Self::CommitMissingWaivers { missing } => write!(
                f,
                "commit refused: missing waivers for findings: {}",
                missing.join(", ")
            ),
            Self::ExportEmpty => write!(f, "cannot export an empty session"),
            Self::InternalInvocation(e) => write!(f, "internal invocation error: {e}"),
        }
    }
}

impl std::error::Error for SessionError {}

// =====================================================================
// Config (quotas)
// =====================================================================

#[derive(Debug, Clone, Resource)]
pub struct ProceduralSessionConfig {
    /// Per-eval wall-clock cap.
    pub per_eval_wall_clock: Duration,
    /// Per-session aggregate wall-clock cap (sum of all eval durations
    /// plus commit duration).
    pub per_session_wall_clock: Duration,
    /// Maximum number of steps in the accumulated script.
    pub max_steps: usize,
    /// Maximum serialized size of `bindings` map in bytes.
    pub max_binding_bytes: usize,
}

impl Default for ProceduralSessionConfig {
    fn default() -> Self {
        Self {
            per_eval_wall_clock: Duration::from_millis(250),
            per_session_wall_clock: Duration::from_secs(60),
            max_steps: 1000,
            max_binding_bytes: 8 * 1024 * 1024,
        }
    }
}

// =====================================================================
// Capability descriptor — what the session needs to know about a tool
// =====================================================================

/// Minimal descriptor the session uses to type-check eval steps and
/// project dry-run output stubs. Capability crates register full
/// `CommandDescriptor` (ADR-011) entries elsewhere; this is the v1 view
/// the session substrate consumes. A future PP will fold this into the
/// shared command/capability registry.
#[derive(Debug, Clone, PartialEq, Resource, Default)]
pub struct SessionToolRegistry {
    descriptors: BTreeMap<McpToolId, SessionToolDescriptor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionToolDescriptor {
    pub tool: McpToolId,
    /// Whether this tool mutates model state. Used by MutationScope
    /// enforcement at eval and commit time.
    pub mutates: bool,
    /// Optional stub response shape returned by the dry-run dispatcher
    /// when no caller-supplied stub is provided.
    pub default_stub: Option<Value>,
    /// Optional list of obligations this tool typically *creates*
    /// (carried to the session's outstanding list at bind time).
    pub creates_obligations: Vec<SessionObligation>,
    /// Optional list of obligations this tool *satisfies* (cleared from
    /// the session's outstanding list at bind time).
    pub satisfies_obligation_ids: Vec<String>,
}

impl SessionToolRegistry {
    pub fn register(&mut self, descriptor: SessionToolDescriptor) {
        self.descriptors.insert(descriptor.tool.clone(), descriptor);
    }

    pub fn get(&self, tool: &McpToolId) -> Option<&SessionToolDescriptor> {
        self.descriptors.get(tool)
    }

    pub fn known_tools(&self) -> impl Iterator<Item = &McpToolId> {
        self.descriptors.keys()
    }
}

// =====================================================================
// ProceduralSession
// =====================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct ProceduralSession {
    pub id: SessionId,
    pub spec: SessionSpec,
    /// The accumulated body. Grown by `BindOnly` / `DryRunAndBind` evals.
    pub script: AuthoringScript,
    /// Bindings keyed by `StepId`, capturing dry-run response shapes for
    /// future steps' `ArgExpr::StepOutput` resolution.
    pub bindings: BTreeMap<StepId, Map<String, Value>>,
    pub outstanding_obligations: Vec<SessionObligation>,
    pub findings: Vec<SessionFinding>,
    audit: Vec<AuditEntry>,
    audit_seq: u64,
    started_at: Instant,
    cumulative_eval: Duration,
    closed: bool,
    exported: Vec<ExportedAuthoringScript>,
}

impl ProceduralSession {
    fn new(id: SessionId, spec: SessionSpec) -> Self {
        let parameter_schema = spec
            .parameter_schema
            .clone()
            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
        let mut script = AuthoringScript::stub(spec.mutation_scope.clone());
        script.parameter_schema = parameter_schema;
        script.allowed_tools = spec.allowed_tools.clone();
        let mut s = Self {
            id,
            spec,
            script,
            bindings: BTreeMap::new(),
            outstanding_obligations: Vec::new(),
            findings: Vec::new(),
            audit: Vec::new(),
            audit_seq: 0,
            started_at: Instant::now(),
            cumulative_eval: Duration::ZERO,
            closed: false,
            exported: Vec::new(),
        };
        s.push_audit(AuditEvent::Created);
        s
    }

    fn push_audit(&mut self, event: AuditEvent) {
        let entry = AuditEntry {
            event,
            seq: self.audit_seq,
            at_ms: self.started_at.elapsed().as_millis(),
        };
        self.audit_seq += 1;
        self.audit.push(entry);
    }

    /// Append-only view of the audit log. There is no public mutator —
    /// the only way to add to it is through session methods that record
    /// the event inline.
    pub fn audit(&self) -> &[AuditEntry] {
        &self.audit
    }

    /// Read-only view of all exported authoring scripts from this session.
    pub fn exports(&self) -> &[ExportedAuthoringScript] {
        &self.exported
    }

    /// Convenience helper for the orchestration / MCP layers.
    pub fn snapshot(&mut self) -> SessionSnapshot {
        self.push_audit(AuditEvent::Snapshotted);
        SessionSnapshot {
            session_id: self.id.clone(),
            spec: self.spec.clone(),
            script: self.script.clone(),
            bindings: self.bindings.clone(),
            outstanding_obligations: self.outstanding_obligations.clone(),
            findings: self.findings.clone(),
            audit_excerpt: tail(&self.audit, 32),
            closed: self.closed,
        }
    }
}

/// Snapshot DTO. Mirrors the create/snapshot payload from the agreement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub spec: SessionSpec,
    pub script: AuthoringScript,
    pub bindings: BTreeMap<StepId, Map<String, Value>>,
    pub outstanding_obligations: Vec<SessionObligation>,
    pub findings: Vec<SessionFinding>,
    pub audit_excerpt: Vec<AuditEntry>,
    pub closed: bool,
}

fn tail<T: Clone>(v: &[T], n: usize) -> Vec<T> {
    let start = v.len().saturating_sub(n);
    v[start..].to_vec()
}

// =====================================================================
// Registry (Bevy Resource)
// =====================================================================

#[derive(Debug, Default, Resource)]
pub struct ProceduralSessionRegistry {
    sessions: BTreeMap<SessionId, ProceduralSession>,
}

impl ProceduralSessionRegistry {
    pub fn create(&mut self, spec: SessionSpec) -> SessionId {
        let id = SessionId::new_v4();
        let session = ProceduralSession::new(id.clone(), spec);
        self.sessions.insert(id.clone(), session);
        id
    }

    pub fn get(&self, id: &SessionId) -> Option<&ProceduralSession> {
        self.sessions.get(id)
    }

    pub fn get_mut(&mut self, id: &SessionId) -> Option<&mut ProceduralSession> {
        self.sessions.get_mut(id)
    }

    pub fn close(&mut self, id: &SessionId) -> Result<(), SessionError> {
        let session = self
            .sessions
            .get_mut(id)
            .ok_or_else(|| SessionError::UnknownSession(id.clone()))?;
        session.closed = true;
        session.push_audit(AuditEvent::Closed);
        // Keep the session struct around briefly so audit/snapshot
        // remain available; full removal happens lazily on next prune.
        Ok(())
    }

    /// Remove all closed sessions. Callable from a Bevy maintenance
    /// system if a hosting app wants periodic cleanup.
    pub fn prune_closed(&mut self) -> usize {
        let to_remove: Vec<_> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.closed)
            .map(|(k, _)| k.clone())
            .collect();
        let n = to_remove.len();
        for k in to_remove {
            self.sessions.remove(&k);
        }
        n
    }

    pub fn active_session_count(&self) -> usize {
        self.sessions.values().filter(|s| !s.closed).count()
    }
}

// =====================================================================
// Dry-run dispatcher
// =====================================================================

/// `ToolDispatcher` that records calls without mutating anything,
/// returning the descriptor's `default_stub` value (or `Value::Null`).
/// Used for `EvalMode::DryRun` / `DryRunAndBind`.
pub struct DryRunDispatcher<'a> {
    registry: &'a SessionToolRegistry,
    /// Calls captured during dispatch, in order.
    pub captured: Vec<(McpToolId, Map<String, Value>)>,
    /// Recorded MutationScope violations (per-step).
    pub scope_violations: Vec<(McpToolId, String)>,
}

impl<'a> DryRunDispatcher<'a> {
    pub fn new(registry: &'a SessionToolRegistry) -> Self {
        Self {
            registry,
            captured: Vec::new(),
            scope_violations: Vec::new(),
        }
    }
}

impl<'a> ToolDispatcher for DryRunDispatcher<'a> {
    fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
        let descriptor = self.registry.get(call.tool);
        let mutates = descriptor.map(|d| d.mutates).unwrap_or(false);

        // MutationScope::None must reject mutating calls outright.
        if mutates {
            if let MutationScope::None = call.mutation_scope {
                let reason = format!(
                    "tool '{}' is a mutator but session mutation_scope is `None`",
                    call.tool.0
                );
                self.scope_violations
                    .push((call.tool.clone(), reason.clone()));
                return Err(ToolDispatchError::new("mutation_out_of_scope", reason));
            }
        }
        self.captured.push((call.tool.clone(), call.args.clone()));
        let stub = descriptor
            .and_then(|d| d.default_stub.clone())
            .unwrap_or(Value::Null);
        Ok(stub)
    }
}

/// Stub oracle: passes every postcondition unconditionally. Used in
/// dry-run; the live oracle is plugged at commit time by the caller.
pub struct AlwaysPassOracle;

impl PostconditionOracle for AlwaysPassOracle {
    fn check(
        &self,
        _postcondition: &ResolvedPostcondition,
        _outputs: &BTreeMap<StepId, Map<String, Value>>,
        _params: &Map<String, Value>,
    ) -> super::replay::PostconditionVerdict {
        super::replay::PostconditionVerdict::Pass
    }
}

// =====================================================================
// eval
// =====================================================================

/// Helper to validate a candidate `EvalStep` against the session's
/// allowed-tools / registry / step-id-uniqueness without mutating the
/// session.
fn precheck_step(
    session: &ProceduralSession,
    registry: &SessionToolRegistry,
    step: &EvalStep,
) -> Result<(), SessionError> {
    if !session.spec.allowed_tools.is_empty() && !session.spec.allowed_tools.contains(&step.tool) {
        return Err(SessionError::DisallowedTool {
            tool: step.tool.clone(),
        });
    }
    if registry.get(&step.tool).is_none() {
        return Err(SessionError::UnknownTool {
            tool: step.tool.clone(),
        });
    }
    if session.script.steps.iter().any(|s| s.id() == &step.id) {
        return Err(SessionError::DuplicateStepId(step.id.clone()));
    }
    Ok(())
}

/// Run an eval against a session. The caller supplies the
/// `SessionToolRegistry` (Bevy `Resource` in the hosting app) and the
/// `ProceduralSessionConfig`. Returns the eval report or an error; in
/// either case the session's audit log is updated.
pub fn eval(
    session: &mut ProceduralSession,
    registry: &SessionToolRegistry,
    config: &ProceduralSessionConfig,
    step: EvalStep,
    mode: EvalMode,
) -> Result<EvalReport, SessionError> {
    if session.closed {
        return Err(SessionError::UnknownSession(session.id.clone()));
    }
    let started = Instant::now();

    // Per-session aggregate wall-clock check.
    if session.cumulative_eval >= config.per_session_wall_clock {
        let err = SessionError::QuotaWallClock {
            which: "per_session",
        };
        session.push_audit(AuditEvent::QuotaExceeded {
            which: "per_session".into(),
        });
        return Err(err);
    }

    // Step-count cap counts steps that would be appended after this eval.
    let would_append = matches!(mode, EvalMode::BindOnly | EvalMode::DryRunAndBind);
    if would_append && session.script.steps.len() + 1 > config.max_steps {
        session.push_audit(AuditEvent::QuotaExceeded {
            which: "max_steps".into(),
        });
        return Err(SessionError::QuotaStepCount {
            limit: config.max_steps,
        });
    }

    if let Err(e) = precheck_step(session, registry, &step) {
        let elapsed = started.elapsed();
        session.cumulative_eval += elapsed;
        session.push_audit(AuditEvent::DryRunRejected {
            step_id: step.id.clone(),
            reason: format!("{e}"),
        });
        return Err(e);
    }

    let descriptor = registry
        .get(&step.tool)
        .expect("precheck_step guarantees presence")
        .clone();

    let mut dry_run: Option<DryRunProjection> = None;

    if matches!(mode, EvalMode::DryRun | EvalMode::DryRunAndBind) {
        // Project this single step's dispatch by building a tiny
        // throwaway one-step script and running it through `replay()`
        // with the dry-run dispatcher.
        let projection = project_single_step(session, registry, &step, &descriptor);
        match projection {
            Ok(p) => {
                dry_run = Some(p);
            }
            Err(e) => {
                let elapsed = started.elapsed();
                session.cumulative_eval += elapsed;
                session.push_audit(AuditEvent::DryRunRejected {
                    step_id: step.id.clone(),
                    reason: format!("{e}"),
                });
                return Err(e);
            }
        }
    }

    let appended = matches!(mode, EvalMode::BindOnly | EvalMode::DryRunAndBind);
    if appended {
        let step_id = step.id.clone();
        // Update outstanding obligations: drop any satisfied by this
        // tool, add any it creates.
        let satisfied: BTreeSet<String> = descriptor
            .satisfies_obligation_ids
            .iter()
            .cloned()
            .collect();
        session
            .outstanding_obligations
            .retain(|o| !satisfied.contains(&o.id));
        for ob in &descriptor.creates_obligations {
            if !session
                .outstanding_obligations
                .iter()
                .any(|o| o.id == ob.id)
            {
                session.outstanding_obligations.push(ob.clone());
            }
        }

        // Append step.
        let s: ScriptInstruction = step.clone().into();
        session.script.steps.push(s);
        // Make the step's tool implicitly allowed (the spec set may
        // have been empty if caller wants "any registered tool").
        session.script.allowed_tools.insert(step.tool.clone());

        // If we have a dry-run stub, also record bindings so later
        // steps' StepOutput refs resolve.
        if let Some(ref p) = dry_run {
            let mut captured = Map::new();
            for (label, path) in &step.bindings {
                if let Some(v) = read_simple_path(&p.stub_response, path) {
                    captured.insert(label.clone(), v);
                }
            }
            // Memory cap on accumulated bindings.
            let projected = serde_json::to_vec(&session.bindings)
                .unwrap_or_default()
                .len()
                + serde_json::to_vec(&captured).unwrap_or_default().len();
            if projected > config.max_binding_bytes {
                // Roll back the append.
                session.script.steps.pop();
                session.push_audit(AuditEvent::QuotaExceeded {
                    which: "max_binding_bytes".into(),
                });
                return Err(SessionError::QuotaBindingMemory {
                    bytes: projected,
                    limit: config.max_binding_bytes,
                });
            }
            session.bindings.insert(step_id.clone(), captured);
        }
    }

    let elapsed = started.elapsed();
    if elapsed > config.per_eval_wall_clock {
        // Don't roll back the append (the work is done); but record
        // the quota event and surface as an error for backpressure.
        session.cumulative_eval += elapsed;
        session.push_audit(AuditEvent::QuotaExceeded {
            which: "per_eval".into(),
        });
        return Err(SessionError::QuotaWallClock { which: "per_eval" });
    }
    session.cumulative_eval += elapsed;

    let report = EvalReport {
        step_id: step.id.clone(),
        mode,
        appended,
        dry_run,
        elapsed_ms: elapsed.as_millis(),
    };
    session.push_audit(AuditEvent::Evaluated {
        step_id: step.id,
        mode,
        appended,
    });
    Ok(report)
}

fn project_single_step(
    session: &ProceduralSession,
    registry: &SessionToolRegistry,
    step: &EvalStep,
    descriptor: &SessionToolDescriptor,
) -> Result<DryRunProjection, SessionError> {
    // Use the existing replay() pipeline on a one-step script that
    // contains the new step. This ensures arg resolution, scope
    // enforcement, and binding capture run exactly as they would at
    // commit time.
    let mut throwaway = session.script.clone();
    throwaway.steps.clear();
    throwaway.allowed_tools.insert(step.tool.clone());
    let s: ScriptInstruction = step.clone().into();
    throwaway.steps.push(s);
    throwaway.postconditions.clear();

    // Seed parameters with any captured bindings as a synthetic params
    // object — keys prefixed with `__step.` — so the projection sees
    // earlier steps' values without re-running them. For v1 we simply
    // pass an empty params map; ArgExpr::StepOutput referring to
    // earlier steps is resolved via the dispatcher's captured outputs
    // for prior steps, which the dry-run executor doesn't have. The
    // explicit "live bindings" path is captured at append time.
    let params = Map::new();
    let mut dispatcher = DryRunDispatcher::new(registry);
    let oracle = AlwaysPassOracle;

    match replay(&throwaway, params, &mut dispatcher, &oracle) {
        Ok(report) => {
            let stub_response = report
                .outputs
                .get(&step.id)
                .cloned()
                .map(Value::Object)
                .unwrap_or(Value::Null);
            // Project obligations/findings from the descriptor.
            let projected_obligations = descriptor.creates_obligations.clone();
            let projected_findings = Vec::new();
            let resolved_args = dispatcher
                .captured
                .into_iter()
                .next()
                .map(|(_, a)| a)
                .unwrap_or_default();
            Ok(DryRunProjection {
                tool: step.tool.clone(),
                resolved_args,
                stub_response,
                projected_obligations,
                projected_findings,
            })
        }
        Err(InvocationError::Dispatch { step: sid, error })
            if error.code == "mutation_out_of_scope" =>
        {
            Err(SessionError::MutationOutOfScope {
                step: sid,
                reason: error.message,
            })
        }
        Err(InvocationError::ToolNotAllowed { tool, .. }) => {
            Err(SessionError::DisallowedTool { tool })
        }
        Err(InvocationError::ParameterSchemaFailed { message }) => {
            Err(SessionError::ParameterSchema { message })
        }
        Err(InvocationError::Dispatch { step, error }) => {
            Err(SessionError::Dispatch { step, error })
        }
        Err(InvocationError::BindingPathMissing {
            step,
            binding,
            path,
        }) => Err(SessionError::BindingPathMissing {
            step,
            binding,
            path,
        }),
        Err(InvocationError::InvalidStructure(e)) => Err(SessionError::InvalidStructure(e)),
        Err(e) => Err(SessionError::InternalInvocation(e)),
    }
}

/// Tiny JSON-path read mirroring `replay::read_path` for the bindings
/// path syntax (`$`, `$.a.b`, `a.b`).
fn read_simple_path(value: &Value, path: &OutputPath) -> Option<Value> {
    let raw = path.as_str();
    let stripped = raw
        .strip_prefix("$.")
        .unwrap_or_else(|| raw.strip_prefix('$').unwrap_or(raw));
    if stripped.is_empty() {
        return Some(value.clone());
    }
    let mut cur = value;
    for segment in stripped.split('.').filter(|s| !s.is_empty()) {
        cur = cur.get(segment)?;
    }
    Some(cur.clone())
}

// =====================================================================
// commit
// =====================================================================

/// `ToolDispatcher` adapter that wraps a caller-supplied dispatcher and
/// records a `TaggedCommit` per dispatched call. Used at commit time so
/// `CommitReport.tagged_calls` is populated.
pub struct CommitTaggingDispatcher<'a, D: ToolDispatcher> {
    pub inner: &'a mut D,
    pub session_id: SessionId,
    pub tagged: Vec<TaggedCommit>,
    /// Mapping of step id to step index (set by `commit`).
    step_order: VecDeque<StepId>,
    next_command_id: u64,
    pub mutation_violations: Vec<(StepId, String)>,
    registry: &'a SessionToolRegistry,
}

impl<'a, D: ToolDispatcher> CommitTaggingDispatcher<'a, D> {
    pub fn new(
        inner: &'a mut D,
        session_id: SessionId,
        registry: &'a SessionToolRegistry,
        step_order: impl IntoIterator<Item = StepId>,
    ) -> Self {
        Self {
            inner,
            session_id,
            tagged: Vec::new(),
            step_order: step_order.into_iter().collect(),
            next_command_id: 0,
            mutation_violations: Vec::new(),
            registry,
        }
    }
}

impl<'a, D: ToolDispatcher> ToolDispatcher for CommitTaggingDispatcher<'a, D> {
    fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
        // Enforce MutationScope::None at commit time for mutating tools.
        let descriptor = self.registry.get(call.tool);
        let mutates = descriptor.map(|d| d.mutates).unwrap_or(false);
        if mutates {
            if let MutationScope::None = call.mutation_scope {
                let step = self
                    .step_order
                    .front()
                    .cloned()
                    .unwrap_or_else(|| StepId::new("?"));
                let reason = format!(
                    "tool '{}' is a mutator but session mutation_scope is `None`",
                    call.tool.0
                );
                self.mutation_violations.push((step, reason.clone()));
                return Err(ToolDispatchError::new("mutation_out_of_scope", reason));
            }
        }

        let step_id = self
            .step_order
            .pop_front()
            .unwrap_or_else(|| StepId::new("?"));
        let response = self.inner.dispatch(call)?;
        let command_id = self.next_command_id;
        self.next_command_id += 1;
        self.tagged.push(TaggedCommit {
            session_id: self.session_id.clone(),
            step_id,
            tool: call.tool.clone(),
            command_id,
        });
        Ok(response)
    }
}

/// Commit options (policy + optional waivers + optional in-line export).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CommitOptions {
    pub policy: CommitPolicyDe,
    #[serde(default)]
    pub waivers: Vec<Waiver>,
    /// If present, perform an in-line export immediately after commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<InlineExportRequest>,
}

// Serde-friendly wrapper for CommitPolicy with default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CommitPolicyDe {
    #[default]
    RequireClean,
    AcceptWithWaivers,
    AcceptPartial,
}

impl From<CommitPolicyDe> for CommitPolicy {
    fn from(p: CommitPolicyDe) -> Self {
        match p {
            CommitPolicyDe::RequireClean => CommitPolicy::RequireClean,
            CommitPolicyDe::AcceptWithWaivers => CommitPolicy::AcceptWithWaivers,
            CommitPolicyDe::AcceptPartial => CommitPolicy::AcceptPartial,
        }
    }
}

impl From<CommitPolicy> for CommitPolicyDe {
    fn from(p: CommitPolicy) -> Self {
        match p {
            CommitPolicy::RequireClean => CommitPolicyDe::RequireClean,
            CommitPolicy::AcceptWithWaivers => CommitPolicyDe::AcceptWithWaivers,
            CommitPolicy::AcceptPartial => CommitPolicyDe::AcceptPartial,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct InlineExportRequest {
    pub target: ExportTarget,
    pub metadata: ExportMetadata,
}

/// Commit the session's accumulated `AuthoringScript` through the
/// caller-supplied dispatcher. The dispatcher is the live bridge to
/// `PendingCommandQueue` in the hosting app; tests use a fixture
/// dispatcher.
///
/// Returns a `CommitReport` on success.
pub fn commit<D: ToolDispatcher, O: PostconditionOracle>(
    session: &mut ProceduralSession,
    registry: &SessionToolRegistry,
    config: &ProceduralSessionConfig,
    options: CommitOptions,
    dispatcher: &mut D,
    oracle: &O,
) -> Result<CommitReport, SessionError> {
    if session.closed {
        return Err(SessionError::UnknownSession(session.id.clone()));
    }
    let policy: CommitPolicy = options.policy.into();
    // Quota gate.
    if session.cumulative_eval >= config.per_session_wall_clock {
        session.push_audit(AuditEvent::QuotaExceeded {
            which: "per_session".into(),
        });
        return Err(SessionError::QuotaWallClock {
            which: "per_session",
        });
    }

    // Policy gate.
    let remaining_obligations = session.outstanding_obligations.clone();
    let remaining_findings = session.findings.clone();
    match policy {
        CommitPolicy::RequireClean => {
            if !remaining_obligations.is_empty() || !remaining_findings.is_empty() {
                let reason = format!(
                    "{} obligations + {} findings remain",
                    remaining_obligations.len(),
                    remaining_findings.len(),
                );
                session.push_audit(AuditEvent::CommitRejected {
                    policy,
                    reason: reason.clone(),
                });
                return Err(SessionError::CommitNotClean {
                    remaining_obligations: remaining_obligations.len(),
                    remaining_findings: remaining_findings.len(),
                });
            }
        }
        CommitPolicy::AcceptWithWaivers => {
            let waiver_ids: BTreeSet<&str> = options
                .waivers
                .iter()
                .map(|w| w.finding_id.as_str())
                .collect();
            let missing: Vec<String> = remaining_findings
                .iter()
                .filter(|f| !waiver_ids.contains(f.id.as_str()))
                .map(|f| f.id.clone())
                .collect();
            if !missing.is_empty() {
                session.push_audit(AuditEvent::CommitRejected {
                    policy,
                    reason: format!("missing waivers: {}", missing.join(",")),
                });
                return Err(SessionError::CommitMissingWaivers { missing });
            }
        }
        CommitPolicy::AcceptPartial => {
            // Allowed; carry-over recorded below.
        }
    }

    // Replay the script through a tagging dispatcher that also enforces
    // MutationScope::None against mutating tools.
    let step_order: Vec<StepId> = session.script.steps.iter().map(|s| s.id().clone()).collect();
    let mut tagger =
        CommitTaggingDispatcher::new(dispatcher, session.id.clone(), registry, step_order.clone());

    let report_result = replay(&session.script, Map::new(), &mut tagger, oracle);
    let tagged = std::mem::take(&mut tagger.tagged);
    let mutation_violations = std::mem::take(&mut tagger.mutation_violations);
    drop(tagger);

    let report = match report_result {
        Ok(r) => r,
        Err(InvocationError::Dispatch { step, error }) if error.code == "mutation_out_of_scope" => {
            session.push_audit(AuditEvent::CommitRejected {
                policy,
                reason: format!("mutation out of scope at step '{}'", step.0),
            });
            return Err(SessionError::MutationOutOfScope {
                step,
                reason: error.message,
            });
        }
        Err(InvocationError::ToolNotAllowed { tool, .. }) => {
            return Err(SessionError::DisallowedTool { tool });
        }
        Err(InvocationError::ParameterSchemaFailed { message }) => {
            return Err(SessionError::ParameterSchema { message });
        }
        Err(InvocationError::Dispatch { step, error }) => {
            return Err(SessionError::Dispatch { step, error });
        }
        Err(InvocationError::BindingPathMissing {
            step,
            binding,
            path,
        }) => {
            return Err(SessionError::BindingPathMissing {
                step,
                binding,
                path,
            });
        }
        Err(InvocationError::InvalidStructure(e)) => {
            return Err(SessionError::InvalidStructure(e));
        }
        Err(other) => return Err(SessionError::InternalInvocation(other)),
    };

    // Even if replay succeeded, surface any latent mutation violations
    // (defensive; tagger fails the dispatch on violation so replay
    // would have errored).
    if let Some((step, reason)) = mutation_violations.into_iter().next() {
        return Err(SessionError::MutationOutOfScope { step, reason });
    }

    // Carry-over remaining obligations for AcceptPartial.
    let carried_over = if let CommitPolicy::AcceptPartial = policy {
        remaining_obligations
            .iter()
            .map(|o| CarriedOverObligation {
                obligation: o.clone(),
                onto_target: session.spec.refinement_target.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };

    // Update outstanding obligations on the session.
    if matches!(policy, CommitPolicy::AcceptPartial) {
        // Clear; they've been moved to carried_over for the caller.
        session.outstanding_obligations.clear();
    }

    let mut commit_report = CommitReport {
        session_id: session.id.clone(),
        policy,
        steps_run: report.steps_run,
        steps_skipped: report.steps_skipped,
        outputs: report.outputs,
        tagged_calls: tagged,
        post_commit_findings: remaining_findings,
        remaining_obligations: session.outstanding_obligations.clone(),
        carried_over,
        export_handle: None,
    };

    session.push_audit(AuditEvent::Committed {
        steps_run: commit_report.steps_run.len(),
        policy,
        remaining_obligations: commit_report.remaining_obligations.len(),
        remaining_findings: commit_report.post_commit_findings.len(),
    });

    // In-line export.
    if let Some(req) = options.export {
        let handle = export(session, req.target, req.metadata)?;
        commit_report.export_handle = Some(handle);
    }

    Ok(commit_report)
}

// =====================================================================
// export
// =====================================================================

/// Freeze the session's accumulated `AuthoringScript` into a curated
/// asset handle. For v1, only `ExportTarget::AuthoringScript` is
/// supported. The exported script is also stashed inside an
/// [`ExportedAuthoringScript`] inside the session for retrieval in v1
/// — a future PP will hand off to the durable curation registry.
pub fn export(
    session: &mut ProceduralSession,
    target: ExportTarget,
    metadata: ExportMetadata,
) -> Result<ExportHandle, SessionError> {
    if session.closed {
        return Err(SessionError::UnknownSession(session.id.clone()));
    }
    if session.script.steps.is_empty() {
        return Err(SessionError::ExportEmpty);
    }
    let handle = ExportHandle {
        asset_id: AssetId::new(Uuid::new_v4().to_string()),
        kind: AssetKindId::new(match target {
            ExportTarget::AuthoringScript => "recipe.authoring_script.v1",
        }),
        revision: AssetRevision::initial(),
        scope: Scope::Session,
        trust: Trust::Draft,
        target,
    };

    // Attach any caller-supplied postconditions to the script copy that
    // gets baked into the export — but do not mutate the live session
    // script so the session remains re-exportable.
    let mut snapshot_script = session.script.clone();
    snapshot_script
        .postconditions
        .extend(metadata.additional_postconditions.clone());

    session.exported.push(ExportedAuthoringScript {
        handle: handle.clone(),
        script: snapshot_script,
        name: metadata.name.clone(),
        description: metadata.description.clone(),
    });
    session.push_audit(AuditEvent::Exported {
        target,
        asset_id: handle.asset_id.clone(),
    });
    Ok(handle)
}

// Stash the `exported` field on `ProceduralSession`. Done via an impl
// block here so the main struct definition above stays declarative;
// for v1 we keep exports inside the session for retrieval, with a
// follow-on PP moving to the live curation registry.
impl ProceduralSession {
    /// Exports performed against this session.
    pub fn exported(&self) -> &[ExportedAuthoringScript] {
        &self.exported
    }
}

// Re-open the struct via a free-standing definition? Rust doesn't
// allow that — we must amend the struct above. The `exported` field is
// declared in the struct definition; ensure the constructor initializes
// it. See `ProceduralSession::new` above and the struct definition: we
// add the field here in a follow-up patch within this same file.

// (The field is initialized inside ProceduralSession::new via an
// extension trait below to keep the struct definition contiguous.)

// =====================================================================
// Bevy plugin
// =====================================================================

/// `ProceduralSessionPlugin` installs the registry, the default config,
/// and an empty `SessionToolRegistry` (capability crates register
/// `SessionToolDescriptor` entries during their own plugin `build`).
pub struct ProceduralSessionPlugin;

impl Plugin for ProceduralSessionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ProceduralSessionRegistry>()
            .init_resource::<ProceduralSessionConfig>()
            .init_resource::<SessionToolRegistry>();
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(tool: &str, mutates: bool) -> SessionToolDescriptor {
        SessionToolDescriptor {
            tool: McpToolId::new(tool),
            mutates,
            default_stub: Some(serde_json::json!({"ok": true, "id": "stub-1"})),
            creates_obligations: Vec::new(),
            satisfies_obligation_ids: Vec::new(),
        }
    }

    fn registry_with(descriptors: Vec<SessionToolDescriptor>) -> SessionToolRegistry {
        let mut r = SessionToolRegistry::default();
        for d in descriptors {
            r.register(d);
        }
        r
    }

    fn allowed(tools: &[&str]) -> BTreeSet<McpToolId> {
        tools.iter().map(|s| McpToolId::new(*s)).collect()
    }

    fn fresh_session(allowed_tools: BTreeSet<McpToolId>) -> ProceduralSession {
        let spec = SessionSpec {
            refinement_target: Some(serde_json::json!({"entity": 42})),
            stage_transition: StageTransition::SchematicToConstructible,
            mutation_scope: MutationScope::RefinementSubtree {
                root_element_param: "root".into(),
            },
            allowed_tools,
            seed: None,
            parameter_schema: None,
        };
        ProceduralSession::new(SessionId::new_v4(), spec)
    }

    #[test]
    fn create_then_snapshot_records_audit() {
        let mut s = fresh_session(allowed(&["architecture.opening.place_hosted"]));
        let snap = s.snapshot();
        assert_eq!(snap.script.steps.len(), 0);
        assert!(snap
            .audit_excerpt
            .iter()
            .any(|e| matches!(e.event, AuditEvent::Created)));
        assert!(snap
            .audit_excerpt
            .iter()
            .any(|e| matches!(e.event, AuditEvent::Snapshotted)));
    }

    #[test]
    fn bind_only_appends_step_and_rejects_disallowed() {
        let reg = registry_with(vec![descriptor("a.tool", true), descriptor("b.tool", true)]);
        let config = ProceduralSessionConfig::default();
        let mut s = fresh_session(allowed(&["a.tool"]));

        // a.tool is allowed.
        let step = EvalStep {
            id: StepId::new("s1"),
            tool: McpToolId::new("a.tool"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        let r = eval(&mut s, &reg, &config, step, EvalMode::BindOnly).unwrap();
        assert!(r.appended);
        assert_eq!(s.script.steps.len(), 1);

        // b.tool is registered but disallowed by this session's spec.
        let step = EvalStep {
            id: StepId::new("s2"),
            tool: McpToolId::new("b.tool"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        let err = eval(&mut s, &reg, &config, step, EvalMode::BindOnly).unwrap_err();
        assert!(matches!(err, SessionError::DisallowedTool { .. }));
        assert_eq!(s.script.steps.len(), 1);
    }

    #[test]
    fn unknown_tool_rejected_at_eval() {
        let reg = registry_with(vec![]);
        let config = ProceduralSessionConfig::default();
        let mut s = fresh_session(BTreeSet::new());
        let step = EvalStep {
            id: StepId::new("s1"),
            tool: McpToolId::new("nope.tool"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        let err = eval(&mut s, &reg, &config, step, EvalMode::BindOnly).unwrap_err();
        assert!(matches!(err, SessionError::UnknownTool { .. }));
    }

    #[test]
    fn duplicate_step_id_rejected() {
        let reg = registry_with(vec![descriptor("a.tool", false)]);
        let config = ProceduralSessionConfig::default();
        let mut s = fresh_session(allowed(&["a.tool"]));
        let mk = || EvalStep {
            id: StepId::new("s1"),
            tool: McpToolId::new("a.tool"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        eval(&mut s, &reg, &config, mk(), EvalMode::BindOnly).unwrap();
        let err = eval(&mut s, &reg, &config, mk(), EvalMode::BindOnly).unwrap_err();
        assert!(matches!(err, SessionError::DuplicateStepId(_)));
    }

    #[test]
    fn dry_run_projects_without_appending() {
        let reg = registry_with(vec![descriptor("a.tool", true)]);
        let config = ProceduralSessionConfig::default();
        let mut s = fresh_session(allowed(&["a.tool"]));
        let step = EvalStep {
            id: StepId::new("s1"),
            tool: McpToolId::new("a.tool"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        let r = eval(&mut s, &reg, &config, step, EvalMode::DryRun).unwrap();
        assert!(!r.appended);
        assert!(r.dry_run.is_some());
        assert_eq!(s.script.steps.len(), 0);
    }

    #[test]
    fn dry_run_rejects_mutator_in_none_scope() {
        let reg = registry_with(vec![descriptor("mut.tool", true)]);
        let config = ProceduralSessionConfig::default();
        let spec = SessionSpec {
            refinement_target: None,
            stage_transition: StageTransition::PureQuery,
            mutation_scope: MutationScope::None,
            allowed_tools: allowed(&["mut.tool"]),
            seed: None,
            parameter_schema: None,
        };
        let mut s = ProceduralSession::new(SessionId::new_v4(), spec);
        let step = EvalStep {
            id: StepId::new("s1"),
            tool: McpToolId::new("mut.tool"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        let err = eval(&mut s, &reg, &config, step, EvalMode::DryRun).unwrap_err();
        assert!(matches!(err, SessionError::MutationOutOfScope { .. }));
    }

    #[test]
    fn step_count_quota_rejects() {
        let reg = registry_with(vec![descriptor("a.tool", false)]);
        let config = ProceduralSessionConfig {
            max_steps: 2,
            ..Default::default()
        };
        let mut s = fresh_session(allowed(&["a.tool"]));
        for i in 0..2 {
            let step = EvalStep {
                id: StepId::new(format!("s{i}")),
                tool: McpToolId::new("a.tool"),
                args: BTreeMap::new(),
                bindings: BTreeMap::new(),
                essential: true,
                precondition: None,
            };
            eval(&mut s, &reg, &config, step, EvalMode::BindOnly).unwrap();
        }
        let step = EvalStep {
            id: StepId::new("s2"),
            tool: McpToolId::new("a.tool"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        let err = eval(&mut s, &reg, &config, step, EvalMode::BindOnly).unwrap_err();
        assert!(matches!(err, SessionError::QuotaStepCount { .. }));
    }

    /// Mock dispatcher used to verify commit behaviour.
    struct CountingDispatcher {
        responses: Vec<Value>,
        idx: usize,
        pub calls: Vec<(McpToolId, Map<String, Value>)>,
    }

    impl CountingDispatcher {
        fn new(responses: Vec<Value>) -> Self {
            Self {
                responses,
                idx: 0,
                calls: Vec::new(),
            }
        }
    }

    impl ToolDispatcher for CountingDispatcher {
        fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
            self.calls.push((call.tool.clone(), call.args.clone()));
            let v = self.responses.get(self.idx).cloned().unwrap_or(Value::Null);
            self.idx += 1;
            Ok(v)
        }
    }

    #[test]
    fn commit_clean_session_succeeds_and_tags_commands() {
        let reg = registry_with(vec![descriptor("a.tool", true), descriptor("b.tool", true)]);
        let config = ProceduralSessionConfig::default();
        let mut s = fresh_session(allowed(&["a.tool", "b.tool"]));
        for (i, t) in ["a.tool", "b.tool"].iter().enumerate() {
            let step = EvalStep {
                id: StepId::new(format!("s{i}")),
                tool: McpToolId::new(*t),
                args: BTreeMap::new(),
                bindings: BTreeMap::new(),
                essential: true,
                precondition: None,
            };
            eval(&mut s, &reg, &config, step, EvalMode::BindOnly).unwrap();
        }

        let mut dispatcher = CountingDispatcher::new(vec![
            serde_json::json!({"x": 1}),
            serde_json::json!({"y": 2}),
        ]);
        let oracle = AlwaysPassOracle;
        let report = commit(
            &mut s,
            &reg,
            &config,
            CommitOptions::default(),
            &mut dispatcher,
            &oracle,
        )
        .unwrap();
        assert_eq!(report.steps_run.len(), 2);
        assert_eq!(report.tagged_calls.len(), 2);
        assert_eq!(report.tagged_calls[0].session_id, s.id);
        assert_eq!(report.tagged_calls[0].command_id, 0);
        assert_eq!(report.tagged_calls[1].command_id, 1);
        assert!(report.export_handle.is_none());
        assert!(report.remaining_obligations.is_empty());
        assert!(report.post_commit_findings.is_empty());
    }

    #[test]
    fn commit_require_clean_refuses_when_obligations_remain() {
        let mut creates = descriptor("mk.tool", true);
        creates.creates_obligations.push(SessionObligation {
            id: "obl-1".into(),
            kind: "host-opening".into(),
            description: "place an opening".into(),
        });
        let reg = registry_with(vec![creates]);
        let config = ProceduralSessionConfig::default();
        let mut s = fresh_session(allowed(&["mk.tool"]));
        let step = EvalStep {
            id: StepId::new("s1"),
            tool: McpToolId::new("mk.tool"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        eval(&mut s, &reg, &config, step, EvalMode::BindOnly).unwrap();
        assert_eq!(s.outstanding_obligations.len(), 1);

        let mut dispatcher = CountingDispatcher::new(vec![Value::Null]);
        let err = commit(
            &mut s,
            &reg,
            &config,
            CommitOptions::default(), // require_clean
            &mut dispatcher,
            &AlwaysPassOracle,
        )
        .unwrap_err();
        assert!(matches!(err, SessionError::CommitNotClean { .. }));
    }

    #[test]
    fn commit_accept_with_waivers_requires_match() {
        let reg = registry_with(vec![descriptor("a.tool", true)]);
        let config = ProceduralSessionConfig::default();
        let mut s = fresh_session(allowed(&["a.tool"]));
        s.findings.push(SessionFinding {
            id: "f1".into(),
            severity: FindingSeverity::Warning,
            description: "stub".into(),
        });
        let step = EvalStep {
            id: StepId::new("s1"),
            tool: McpToolId::new("a.tool"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        eval(&mut s, &reg, &config, step, EvalMode::BindOnly).unwrap();

        let mut dispatcher = CountingDispatcher::new(vec![Value::Null]);
        let err = commit(
            &mut s,
            &reg,
            &config,
            CommitOptions {
                policy: CommitPolicyDe::AcceptWithWaivers,
                waivers: vec![],
                export: None,
            },
            &mut dispatcher,
            &AlwaysPassOracle,
        )
        .unwrap_err();
        assert!(matches!(err, SessionError::CommitMissingWaivers { .. }));

        let mut dispatcher = CountingDispatcher::new(vec![Value::Null]);
        let report = commit(
            &mut s,
            &reg,
            &config,
            CommitOptions {
                policy: CommitPolicyDe::AcceptWithWaivers,
                waivers: vec![Waiver {
                    finding_id: "f1".into(),
                    justification: "test".into(),
                }],
                export: None,
            },
            &mut dispatcher,
            &AlwaysPassOracle,
        )
        .unwrap();
        assert_eq!(report.steps_run.len(), 1);
    }

    #[test]
    fn commit_accept_partial_carries_over_remaining_obligations() {
        let mut creates = descriptor("mk.tool", true);
        creates.creates_obligations.push(SessionObligation {
            id: "obl-1".into(),
            kind: "host-opening".into(),
            description: "place an opening".into(),
        });
        let reg = registry_with(vec![creates]);
        let config = ProceduralSessionConfig::default();
        let mut s = fresh_session(allowed(&["mk.tool"]));
        let step = EvalStep {
            id: StepId::new("s1"),
            tool: McpToolId::new("mk.tool"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        eval(&mut s, &reg, &config, step, EvalMode::BindOnly).unwrap();

        let mut dispatcher = CountingDispatcher::new(vec![Value::Null]);
        let report = commit(
            &mut s,
            &reg,
            &config,
            CommitOptions {
                policy: CommitPolicyDe::AcceptPartial,
                waivers: vec![],
                export: None,
            },
            &mut dispatcher,
            &AlwaysPassOracle,
        )
        .unwrap();
        assert_eq!(report.carried_over.len(), 1);
        assert_eq!(report.carried_over[0].obligation.id, "obl-1");
        assert!(s.outstanding_obligations.is_empty());
    }

    #[test]
    fn export_freezes_authoring_script() {
        let reg = registry_with(vec![descriptor("a.tool", false)]);
        let config = ProceduralSessionConfig::default();
        let mut s = fresh_session(allowed(&["a.tool"]));
        let step = EvalStep {
            id: StepId::new("s1"),
            tool: McpToolId::new("a.tool"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        };
        eval(&mut s, &reg, &config, step, EvalMode::BindOnly).unwrap();

        let handle = export(
            &mut s,
            ExportTarget::AuthoringScript,
            ExportMetadata {
                name: "test".into(),
                description: "stub".into(),
                additional_postconditions: vec![],
            },
        )
        .unwrap();
        assert_eq!(handle.scope, Scope::Session);
        assert_eq!(handle.trust, Trust::Draft);
        assert_eq!(handle.target, ExportTarget::AuthoringScript);
        // Session remains re-exportable.
        let _ = export(
            &mut s,
            ExportTarget::AuthoringScript,
            ExportMetadata::default(),
        )
        .unwrap();
        assert_eq!(s.exported().len(), 2);
    }

    #[test]
    fn export_empty_session_errors() {
        let mut s = fresh_session(BTreeSet::new());
        let err = export(
            &mut s,
            ExportTarget::AuthoringScript,
            ExportMetadata::default(),
        )
        .unwrap_err();
        assert!(matches!(err, SessionError::ExportEmpty));
    }

    #[test]
    fn audit_log_is_monotonic_and_append_only() {
        let reg = registry_with(vec![descriptor("a.tool", false)]);
        let config = ProceduralSessionConfig::default();
        let mut s = fresh_session(allowed(&["a.tool"]));
        for i in 0..3 {
            let step = EvalStep {
                id: StepId::new(format!("s{i}")),
                tool: McpToolId::new("a.tool"),
                args: BTreeMap::new(),
                bindings: BTreeMap::new(),
                essential: true,
                precondition: None,
            };
            eval(&mut s, &reg, &config, step, EvalMode::BindOnly).unwrap();
        }
        let log = s.audit();
        let seqs: Vec<u64> = log.iter().map(|e| e.seq).collect();
        for w in seqs.windows(2) {
            assert!(w[1] == w[0] + 1, "audit seqs must be monotonic");
        }
        // No method on the public API mutates a prior entry; the field
        // itself is private. Compile-time check.
    }

    #[test]
    fn registry_close_and_prune() {
        let mut r = ProceduralSessionRegistry::default();
        let spec = SessionSpec {
            refinement_target: None,
            stage_transition: StageTransition::NewConceptual,
            mutation_scope: MutationScope::None,
            allowed_tools: BTreeSet::new(),
            seed: None,
            parameter_schema: None,
        };
        let id = r.create(spec);
        assert_eq!(r.active_session_count(), 1);
        r.close(&id).unwrap();
        assert_eq!(r.active_session_count(), 0);
        let removed = r.prune_closed();
        assert_eq!(removed, 1);
        assert!(r.get(&id).is_none());
    }

    #[test]
    fn dry_run_and_bind_records_binding_for_later_steps() {
        let reg = registry_with(vec![descriptor("mk.tool", false)]);
        let config = ProceduralSessionConfig::default();
        let mut s = fresh_session(allowed(&["mk.tool"]));
        let mut bindings = BTreeMap::new();
        bindings.insert("ok".to_string(), OutputPath::new("ok"));
        let step = EvalStep {
            id: StepId::new("s1"),
            tool: McpToolId::new("mk.tool"),
            args: BTreeMap::new(),
            bindings,
            essential: true,
            precondition: None,
        };
        let r = eval(&mut s, &reg, &config, step, EvalMode::DryRunAndBind).unwrap();
        assert!(r.appended);
        assert!(r.dry_run.is_some());
        let bound = s.bindings.get(&StepId::new("s1")).unwrap();
        assert_eq!(bound.get("ok"), Some(&Value::Bool(true)));
    }
}
