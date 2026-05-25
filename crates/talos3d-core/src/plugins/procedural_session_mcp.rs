//! World-side service layer for the Semantic Procedural Session MCP
//! surface (PP-SPS-3).
//!
//! Per ADR-051, the five MCP tools (`procedural_session.create`,
//! `procedural_session.eval`, `procedural_session.snapshot`,
//! `procedural_session.commit`, `procedural_session.export`) delegate
//! to the substrate in [`crate::curation::procedural_session`]. This
//! module owns the Bevy-side glue:
//!
//! - the world-level handlers ([`world_create`], [`world_eval`],
//!   [`world_snapshot`], [`world_commit`], [`world_export`]) that the
//!   MCP transport plugin in `model_api.rs` (or any in-process caller)
//!   dispatches to,
//! - the commit-time bridge that enqueues each session step as a real
//!   `EditorCommand` through [`PendingCommandQueue`] (ADR-002,
//!   ADR-011), so commits are undoable and history-tracked exactly like
//!   any other authored mutation,
//! - the request DTOs the transport accepts.
//!
//! The actual rmcp tool-router wiring lives next to the existing
//! `ModelApiServer` and is added in the same PP. This module is the
//! transport-neutral substrate it depends on.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::curation::procedural_session::{
    commit as session_commit, eval as session_eval, export as session_export, AlwaysPassOracle,
    CommitOptions, CommitReport, EvalMode, EvalReport, EvalStep, ExportHandle, ExportMetadata,
    ExportTarget, ProceduralSessionConfig, ProceduralSessionPlugin, ProceduralSessionRegistry,
    SessionError, SessionId, SessionSnapshot, SessionSpec, SessionToolRegistry,
};
use crate::curation::{McpToolId, StepId, ToolCall, ToolDispatchError, ToolDispatcher};

use super::history::{EditorCommand, PendingCommandQueue};

// ---------------------------------------------------------------------
// Request DTOs (transport-agnostic; rmcp wraps these in `Parameters`)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SessionCreateRequest {
    pub spec: SessionSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SessionEvalRequest {
    pub session_id: SessionId,
    pub step: EvalStep,
    pub mode: EvalMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SessionSnapshotRequest {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SessionCommitRequest {
    pub session_id: SessionId,
    pub options: CommitOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SessionExportRequest {
    pub session_id: SessionId,
    pub target: ExportTarget,
    pub metadata: ExportMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SessionCreateResponse {
    pub session_id: SessionId,
    pub snapshot: SessionSnapshot,
}

// ---------------------------------------------------------------------
// Session-tagged stub command — v1 commit placeholder
// ---------------------------------------------------------------------

/// `EditorCommand` enqueued by the commit dispatcher for each session
/// step. v1 has no semantic apply/undo because capability crates have
/// not yet wired their MCP tools to real `EditorCommand`s. The command
/// still flows through `PendingCommandQueue` and `History` — so undo,
/// redo, save-point tracking, and grouped session-undo can hook in
/// later without rewriting the substrate.
pub struct SessionStubCommand {
    pub session_id: SessionId,
    pub step_id: StepId,
    pub tool: McpToolId,
    pub args_summary: String,
}

impl EditorCommand for SessionStubCommand {
    fn label(&self) -> &'static str {
        "ProceduralSession.step"
    }

    fn apply(&mut self, _world: &mut World) {
        // v1: no-op semantic effect. Capability crates that register
        // real tools will (in a follow-on PP) provide a dispatcher that
        // produces their own concrete `EditorCommand`s instead of this
        // stub.
    }

    fn undo(&mut self, _world: &mut World) {
        // v1: paired no-op.
    }
}

// ---------------------------------------------------------------------
// Step executor — bridges committed session steps to real model mutation
// ---------------------------------------------------------------------

/// Translates a committed session step into a real model mutation against
/// the world, returning the tool's response JSON.
///
/// The substrate is capability-agnostic, so the concrete executor that
/// knows how to turn `create_box` / `create_entity` / etc. into real
/// `EditorCommand`s lives behind this trait. `model_api.rs` provides the
/// `model-api`-gated implementation that routes to the existing Model API
/// handlers; capability crates can provide their own. When no executor is
/// supplied, [`CommandQueueDispatcher`] falls back to a no-op
/// [`SessionStubCommand`] so the substrate is still exercised end-to-end.
pub trait SessionStepExecutor {
    fn execute(
        &mut self,
        world: &mut World,
        tool: &McpToolId,
        args: &Map<String, Value>,
    ) -> Result<Value, ToolDispatchError>;
}

// ---------------------------------------------------------------------
// Commit dispatcher that pushes session steps through PendingCommandQueue
// ---------------------------------------------------------------------

/// `ToolDispatcher` used at commit time. For each replayed step:
///
/// - if a [`SessionStepExecutor`] is wired, it performs the real model
///   mutation (which itself flows through `PendingCommandQueue` /
///   `History`) and returns the tool's real response, then a
///   [`TaggedCommit`]-tracking [`SessionStubCommand`] is *not* pushed
///   (the executor already enqueued the real command);
/// - otherwise a no-op [`SessionStubCommand`] is enqueued so the
///   substrate is exercised through `History` even without a capability
///   executor.
pub struct CommandQueueDispatcher<'w, 'e> {
    pub world: &'w mut World,
    pub session_id: SessionId,
    /// One entry per dispatched step, in order.
    pub step_order: std::collections::VecDeque<StepId>,
    /// Optional real-mutation executor. When `None`, falls back to the
    /// stub command.
    pub executor: Option<&'e mut dyn SessionStepExecutor>,
}

impl<'w, 'e> ToolDispatcher for CommandQueueDispatcher<'w, 'e> {
    fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
        let step_id = self
            .step_order
            .pop_front()
            .unwrap_or_else(|| StepId::new("?"));

        if let Some(executor) = self.executor.as_mut() {
            // Real mutation path: the executor turns the step into the
            // actual model command(s), which enqueue through
            // PendingCommandQueue / History inside the handler.
            return executor.execute(self.world, call.tool, call.args);
        }

        // Fallback: enqueue a no-op stub so the substrate is still
        // exercised through History when no executor is wired.
        let args_summary = format!("{} args", call.args.len());
        let cmd = Box::new(SessionStubCommand {
            session_id: self.session_id.clone(),
            step_id: step_id.clone(),
            tool: call.tool.clone(),
            args_summary,
        });
        let mut queue = self.world.resource_mut::<PendingCommandQueue>();
        queue.push_command(cmd);
        Ok(Value::Object({
            let mut m = Map::new();
            m.insert("ok".to_string(), Value::Bool(true));
            m.insert("step_id".to_string(), Value::String(step_id.0));
            m.insert("tool".to_string(), Value::String(call.tool.0.clone()));
            m
        }))
    }
}

// ---------------------------------------------------------------------
// World-level handlers
// ---------------------------------------------------------------------

pub fn world_create(world: &mut World, req: SessionCreateRequest) -> SessionCreateResponse {
    let mut registry = world.resource_mut::<ProceduralSessionRegistry>();
    let session_id = registry.create(req.spec);
    let snapshot = registry
        .get_mut(&session_id)
        .expect("session just created")
        .snapshot();
    SessionCreateResponse {
        session_id,
        snapshot,
    }
}

pub fn world_eval(world: &mut World, req: SessionEvalRequest) -> Result<EvalReport, SessionError> {
    let SessionEvalRequest {
        session_id,
        step,
        mode,
    } = req;
    let tool_registry = world.resource::<SessionToolRegistry>().clone();
    let config = world.resource::<ProceduralSessionConfig>().clone();
    let mut sessions = world.resource_mut::<ProceduralSessionRegistry>();
    let session = sessions
        .get_mut(&session_id)
        .ok_or_else(|| SessionError::UnknownSession(session_id.clone()))?;
    session_eval(session, &tool_registry, &config, step, mode)
}

pub fn world_snapshot(
    world: &mut World,
    req: SessionSnapshotRequest,
) -> Result<SessionSnapshot, SessionError> {
    let mut sessions = world.resource_mut::<ProceduralSessionRegistry>();
    let session = sessions
        .get_mut(&req.session_id)
        .ok_or_else(|| SessionError::UnknownSession(req.session_id.clone()))?;
    Ok(session.snapshot())
}

/// Commit with the no-op stub dispatcher (no real model mutation). Used
/// by tests and by callers that have not wired a [`SessionStepExecutor`].
pub fn world_commit(
    world: &mut World,
    req: SessionCommitRequest,
) -> Result<CommitReport, SessionError> {
    world_commit_with_executor(world, req, None)
}

/// Commit, optionally translating each step into a real model mutation
/// through the supplied [`SessionStepExecutor`]. The MCP transport passes
/// the `model-api`-backed executor so commits produce real geometry.
pub fn world_commit_with_executor(
    world: &mut World,
    req: SessionCommitRequest,
    executor: Option<&mut dyn SessionStepExecutor>,
) -> Result<CommitReport, SessionError> {
    // Reading-only resources first.
    let tool_registry = world.resource::<SessionToolRegistry>().clone();
    let config = world.resource::<ProceduralSessionConfig>().clone();

    // We need both `&mut ProceduralSessionRegistry` and `&mut World`
    // for the dispatcher. Take the session out of the registry, run
    // commit, then put it back. This avoids a borrow conflict.
    let mut session = {
        let sessions = world.resource::<ProceduralSessionRegistry>();
        sessions
            .get(&req.session_id)
            .ok_or_else(|| SessionError::UnknownSession(req.session_id.clone()))?
            .clone()
    };

    let step_order: std::collections::VecDeque<StepId> =
        session.script.steps.iter().map(|s| s.id.clone()).collect();
    let oracle = AlwaysPassOracle;
    let result = {
        let mut dispatcher = CommandQueueDispatcher {
            world,
            session_id: session.id.clone(),
            step_order,
            executor,
        };
        session_commit(
            &mut session,
            &tool_registry,
            &config,
            req.options,
            &mut dispatcher,
            &oracle,
        )
    };

    // Write the updated session state back.
    {
        let mut sessions = world.resource_mut::<ProceduralSessionRegistry>();
        if let Some(slot) = sessions.get_mut(&req.session_id) {
            *slot = session;
        }
    }
    result
}

pub fn world_export(
    world: &mut World,
    req: SessionExportRequest,
) -> Result<ExportHandle, SessionError> {
    let mut sessions = world.resource_mut::<ProceduralSessionRegistry>();
    let session = sessions
        .get_mut(&req.session_id)
        .ok_or_else(|| SessionError::UnknownSession(req.session_id.clone()))?;
    session_export(session, req.target, req.metadata)
}

// ---------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------

/// `ProceduralSessionMcpPlugin` registers the substrate plugin and the
/// world-side service hooks. Transport-level rmcp tool registration
/// lives in [`super::model_api`] (PP-SPS-3, same PP).
pub struct ProceduralSessionMcpPlugin;

impl Plugin for ProceduralSessionMcpPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<ProceduralSessionRegistry>() {
            app.add_plugins(ProceduralSessionPlugin);
        }
    }
}

// ---------------------------------------------------------------------
// Tests — exercise the full world-level path including
// `PendingCommandQueue` integration.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use bevy::prelude::*;

    use super::*;
    use crate::curation::procedural_session::{
        FindingSeverity, ProceduralSessionRegistry, SessionFinding, SessionToolDescriptor,
        SessionToolRegistry, StageTransition,
    };
    use crate::curation::{McpToolId, MutationScope, StepId};

    fn fresh_app() -> App {
        let mut app = App::new();
        app.init_resource::<PendingCommandQueue>();
        app.add_plugins(ProceduralSessionMcpPlugin);
        // Register one synthetic capability tool that mutates state.
        let mut reg = SessionToolRegistry::default();
        reg.register(SessionToolDescriptor {
            tool: McpToolId::new("architecture.opening.place_hosted"),
            mutates: true,
            default_stub: Some(serde_json::json!({"ok": true, "opening_id": 1})),
            creates_obligations: Vec::new(),
            satisfies_obligation_ids: Vec::new(),
        });
        app.insert_resource(reg);
        app
    }

    fn allowed(tools: &[&str]) -> BTreeSet<McpToolId> {
        tools.iter().map(|s| McpToolId::new(*s)).collect()
    }

    #[test]
    fn world_create_then_snapshot_round_trip() {
        let mut app = fresh_app();
        let response = world_create(
            app.world_mut(),
            SessionCreateRequest {
                spec: SessionSpec::for_refinement(
                    serde_json::json!({"entity": 7}),
                    StageTransition::SchematicToConstructible,
                    "root",
                    allowed(&["architecture.opening.place_hosted"]),
                ),
            },
        );
        assert_eq!(response.snapshot.spec.allowed_tools.len(), 1);

        let snap = world_snapshot(
            app.world_mut(),
            SessionSnapshotRequest {
                session_id: response.session_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(snap.session_id, response.session_id);
        assert_eq!(snap.script.steps.len(), 0);
    }

    #[test]
    fn world_eval_appends_step_and_commit_enqueues_through_pending_command_queue() {
        let mut app = fresh_app();
        let resp = world_create(
            app.world_mut(),
            SessionCreateRequest {
                spec: SessionSpec::for_refinement(
                    serde_json::json!({"entity": 7}),
                    StageTransition::SchematicToConstructible,
                    "root",
                    allowed(&["architecture.opening.place_hosted"]),
                ),
            },
        );

        // Two openings.
        for i in 0..2 {
            let report = world_eval(
                app.world_mut(),
                SessionEvalRequest {
                    session_id: resp.session_id.clone(),
                    step: EvalStep {
                        id: StepId::new(format!("open{i}")),
                        tool: McpToolId::new("architecture.opening.place_hosted"),
                        args: BTreeMap::new(),
                        bindings: BTreeMap::new(),
                        essential: true,
                        precondition: None,
                    },
                    mode: EvalMode::BindOnly,
                },
            )
            .unwrap();
            assert!(report.appended);
        }

        // Pre-commit: PendingCommandQueue is empty.
        assert_eq!(
            app.world().resource::<PendingCommandQueue>().commands.len(),
            0
        );

        let commit_report = world_commit(
            app.world_mut(),
            SessionCommitRequest {
                session_id: resp.session_id.clone(),
                options: CommitOptions::default(),
            },
        )
        .unwrap();
        assert_eq!(commit_report.steps_run.len(), 2);
        assert_eq!(commit_report.tagged_calls.len(), 2);

        // Post-commit: two SessionStubCommands sit in PendingCommandQueue.
        let queue_len = app.world().resource::<PendingCommandQueue>().commands.len();
        assert_eq!(
            queue_len, 2,
            "commit should enqueue one EditorCommand per session step"
        );
    }

    #[test]
    fn world_eval_unknown_session_errors() {
        let mut app = fresh_app();
        let err = world_eval(
            app.world_mut(),
            SessionEvalRequest {
                session_id: SessionId("does-not-exist".into()),
                step: EvalStep {
                    id: StepId::new("s1"),
                    tool: McpToolId::new("architecture.opening.place_hosted"),
                    args: BTreeMap::new(),
                    bindings: BTreeMap::new(),
                    essential: true,
                    precondition: None,
                },
                mode: EvalMode::BindOnly,
            },
        )
        .unwrap_err();
        assert!(matches!(err, SessionError::UnknownSession(_)));
    }

    #[test]
    fn world_commit_require_clean_refuses_with_findings() {
        let mut app = fresh_app();
        let resp = world_create(
            app.world_mut(),
            SessionCreateRequest {
                spec: SessionSpec::for_refinement(
                    serde_json::json!({"entity": 7}),
                    StageTransition::SchematicToConstructible,
                    "root",
                    allowed(&["architecture.opening.place_hosted"]),
                ),
            },
        );
        // Inject a finding directly.
        {
            let mut sessions = app.world_mut().resource_mut::<ProceduralSessionRegistry>();
            let session = sessions.get_mut(&resp.session_id).unwrap();
            session.findings.push(SessionFinding {
                id: "f1".into(),
                severity: FindingSeverity::Error,
                description: "host gap".into(),
            });
        }
        // Append a step so the script isn't empty.
        world_eval(
            app.world_mut(),
            SessionEvalRequest {
                session_id: resp.session_id.clone(),
                step: EvalStep {
                    id: StepId::new("s1"),
                    tool: McpToolId::new("architecture.opening.place_hosted"),
                    args: BTreeMap::new(),
                    bindings: BTreeMap::new(),
                    essential: true,
                    precondition: None,
                },
                mode: EvalMode::BindOnly,
            },
        )
        .unwrap();

        let err = world_commit(
            app.world_mut(),
            SessionCommitRequest {
                session_id: resp.session_id.clone(),
                options: CommitOptions::default(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, SessionError::CommitNotClean { .. }));
        // PendingCommandQueue must remain empty on a refused commit.
        assert_eq!(
            app.world().resource::<PendingCommandQueue>().commands.len(),
            0
        );
    }

    #[test]
    fn world_export_returns_handle() {
        let mut app = fresh_app();
        let resp = world_create(
            app.world_mut(),
            SessionCreateRequest {
                spec: SessionSpec {
                    refinement_target: None,
                    stage_transition: StageTransition::NewConceptual,
                    mutation_scope: MutationScope::None,
                    allowed_tools: allowed(&["architecture.opening.place_hosted"]),
                    seed: None,
                    parameter_schema: None,
                },
            },
        );
        // Make scope non-blocking by clearing the mutator descriptor's
        // mutates flag for this test.
        {
            let mut reg = app.world_mut().resource_mut::<SessionToolRegistry>();
            reg.register(SessionToolDescriptor {
                tool: McpToolId::new("architecture.opening.place_hosted"),
                mutates: false,
                default_stub: Some(serde_json::json!({"ok": true})),
                creates_obligations: Vec::new(),
                satisfies_obligation_ids: Vec::new(),
            });
        }
        world_eval(
            app.world_mut(),
            SessionEvalRequest {
                session_id: resp.session_id.clone(),
                step: EvalStep {
                    id: StepId::new("s1"),
                    tool: McpToolId::new("architecture.opening.place_hosted"),
                    args: BTreeMap::new(),
                    bindings: BTreeMap::new(),
                    essential: true,
                    precondition: None,
                },
                mode: EvalMode::BindOnly,
            },
        )
        .unwrap();

        let handle = world_export(
            app.world_mut(),
            SessionExportRequest {
                session_id: resp.session_id.clone(),
                target: ExportTarget::AuthoringScript,
                metadata: ExportMetadata {
                    name: "two-opening row".into(),
                    description: "demo".into(),
                    additional_postconditions: vec![],
                },
            },
        )
        .unwrap();
        assert_eq!(handle.target, ExportTarget::AuthoringScript);
        assert_eq!(handle.kind.as_str(), "recipe.authoring_script.v1");
    }
}
