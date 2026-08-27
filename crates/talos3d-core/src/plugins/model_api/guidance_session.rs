//! Stateful, agent-neutral guidance for one MCP endpoint.
//!
//! The Model API already sees every Talos3D tool call, so it owns the durable
//! guidance state. Client lifecycle hooks are optional projections over this
//! store; they are never the semantic enforcement boundary.

use super::profiles::{tool_category, ToolCategory};
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

const REQUIRED_DISCOVERY_PATHS: [&str; 4] = ["recipe", "parametric", "definition", "prior"];
const REQUIRED_VERIFICATION_TOOLS: [&str; 5] = [
    "get_world_aabb",
    "check_overlaps",
    "check_floating",
    "check_clearance",
    "run_validation_v2",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GuidancePhase {
    Bootstrap,
    Discover,
    Plan,
    Validate,
    Inspect,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct GuidanceObligation {
    pub id: String,
    pub reason: String,
    pub blocking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct GuidanceNextAction {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    pub reason: String,
}

/// Compact, progressive-disclosure state returned to every MCP client and
/// suitable for hook context rehydration after compaction.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AgentGuidanceState {
    pub contract: String,
    pub contract_version: u32,
    pub guidance_session_id: String,
    pub revision_anchor: String,
    pub phase: GuidancePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub compact_context: String,
    pub required_invariants: Vec<String>,
    pub satisfied_obligations: Vec<String>,
    pub blocking_obligations: Vec<GuidanceObligation>,
    pub next_actions: Vec<GuidanceNextAction>,
    pub authored_since_negotiation: bool,
    pub verification_tools_observed: Vec<String>,
    pub rendered_output_observed: bool,
}

/// Machine-readable description of the optional Codex lifecycle projection.
/// The configuration is deliberately rule-free: all decisions come back to
/// the live MCP guidance tools.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct LifecycleHookProjection {
    pub dialect: String,
    pub server_name: String,
    pub events: Vec<LifecycleHookEvent>,
    pub trust_required: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct LifecycleHookEvent {
    pub event: String,
    pub matcher: String,
    pub tool: String,
    pub purpose: String,
}

impl LifecycleHookProjection {
    pub fn codex() -> Self {
        Self {
            dialect: "codex-hooks/v1".to_string(),
            server_name: "talos3d".to_string(),
            events: vec![
                LifecycleHookEvent {
                    event: "PreToolUse".to_string(),
                    matcher: "^mcp__talos3d__.*$".to_string(),
                    tool: "agent_guidance_preflight".to_string(),
                    purpose: "Give just-in-time guidance before Talos3D calls and deny premature model mutation."
                        .to_string(),
                },
                LifecycleHookEvent {
                    event: "PostCompact".to_string(),
                    matcher: "manual|auto".to_string(),
                    tool: "agent_guidance_after_compact".to_string(),
                    purpose: "Rehydrate the compact live guidance state after context compaction."
                        .to_string(),
                },
                LifecycleHookEvent {
                    event: "Stop".to_string(),
                    matcher: String::new(),
                    tool: "agent_guidance_completion_check".to_string(),
                    purpose: "Continue an authored turn when required validation or rendered evidence is missing."
                        .to_string(),
                },
            ],
            trust_required: true,
            note: "Hooks are an optional client adapter. Talos3D repeats mutation admission server-side, so disabled, unavailable, or untrusted hooks cannot bypass semantic guidance."
                .to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct GuidanceStateRequest {
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub client_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuidancePreflightRequest {
    pub proposed_tool: String,
    #[serde(default)]
    pub proposed_input: Value,
    #[serde(default)]
    pub client_session_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct GuidanceCompletionRequest {
    #[serde(default)]
    pub client_session_id: Option<String>,
    #[serde(default)]
    pub stop_hook_active: bool,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuidanceBlock {
    pub kind: String,
    pub reason: String,
    pub guidance_state: AgentGuidanceState,
}

#[derive(Debug, Clone)]
pub(super) struct GuidanceSessionStore(Arc<Mutex<Option<GuidanceSession>>>);

#[derive(Debug, Clone)]
struct GuidanceSession {
    id: String,
    revision_anchor: String,
    task: Option<String>,
    required_invariants: Vec<String>,
    required_cards: Vec<String>,
    read_cards: HashSet<String>,
    required_skills: Vec<String>,
    read_skills: HashSet<String>,
    guidance_loaded: bool,
    guidance_unavailable: bool,
    discovery_paths: HashSet<String>,
    authored: bool,
    verification_tools: HashSet<String>,
    rendered_output: bool,
}

impl Default for GuidanceSessionStore {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }
}

impl GuidanceSessionStore {
    pub fn start(
        &self,
        revision_anchor: String,
        task: Option<String>,
        required_cards: Vec<String>,
        required_skills: Vec<String>,
        required_invariants: Vec<String>,
    ) -> AgentGuidanceState {
        let session = GuidanceSession {
            id: format!("gs_{}", uuid::Uuid::new_v4().simple()),
            revision_anchor,
            task: task.filter(|task| !task.trim().is_empty()),
            required_invariants,
            required_cards,
            read_cards: HashSet::new(),
            required_skills,
            read_skills: HashSet::new(),
            guidance_loaded: false,
            guidance_unavailable: false,
            discovery_paths: HashSet::new(),
            authored: false,
            verification_tools: HashSet::new(),
            rendered_output: false,
        };
        let snapshot = session.snapshot();
        *self.0.lock().expect("guidance session lock poisoned") = Some(session);
        snapshot
    }

    pub fn snapshot(&self) -> Option<AgentGuidanceState> {
        self.0
            .lock()
            .expect("guidance session lock poisoned")
            .as_ref()
            .map(GuidanceSession::snapshot)
    }

    pub fn required_cards(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("guidance session lock poisoned")
            .as_ref()
            .map(|session| session.required_cards.clone())
            .unwrap_or_default()
    }

    pub fn required_skills(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("guidance session lock poisoned")
            .as_ref()
            .map(|session| session.required_skills.clone())
            .unwrap_or_default()
    }

    pub fn mark_guidance_packet_loaded(&self, authoritative_guidance_available: bool) {
        let mut guard = self.0.lock().expect("guidance session lock poisoned");
        let Some(session) = guard.as_mut() else {
            return;
        };
        session.guidance_loaded = authoritative_guidance_available;
        session.guidance_unavailable = !authoritative_guidance_available;
        session
            .read_cards
            .extend(session.required_cards.iter().cloned());
        session
            .read_skills
            .extend(session.required_skills.iter().cloned());
    }

    /// Server-side admission. Legacy clients that never negotiate remain
    /// compatible; once negotiation activates a session, premature mutation is
    /// rejected regardless of whether a client installed hooks.
    pub fn router_preflight(&self, tool: &str) -> Option<GuidanceBlock> {
        let guard = self.0.lock().expect("guidance session lock poisoned");
        let session = guard.as_ref()?;
        session.preflight(tool)
    }

    /// Hook-side admission is stricter before negotiation so a configured
    /// lifecycle adapter can steer a cold agent to the one required first call.
    pub fn hook_preflight(&self, tool: &str) -> Option<GuidanceBlock> {
        let guard = self.0.lock().expect("guidance session lock poisoned");
        match guard.as_ref() {
            Some(session) => session.preflight(tool),
            None if requires_guidance(tool) && tool != "negotiate_agent_session" => {
                let state = inactive_guidance_state();
                Some(GuidanceBlock {
                    kind: "GuidanceBlock".to_string(),
                    reason: "No active Talos3D guidance session. Call negotiate_agent_session before model mutation."
                        .to_string(),
                    guidance_state: state,
                })
            }
            None => None,
        }
    }

    pub fn observe_success(&self, tool: &str, arguments: Option<&Map<String, Value>>) {
        let mut guard = self.0.lock().expect("guidance session lock poisoned");
        let Some(session) = guard.as_mut() else {
            return;
        };

        match tool {
            "get_authoring_guidance" => session.guidance_loaded = true,
            "get_guidance_card" => {
                if let Some(card_id) = arguments
                    .and_then(|arguments| arguments.get("card_id"))
                    .and_then(Value::as_str)
                {
                    session.read_cards.insert(card_id.to_string());
                }
            }
            "discover_curated_paths" => {
                if let Some(path_kind) = arguments
                    .and_then(|arguments| arguments.get("path_kind"))
                    .and_then(Value::as_str)
                {
                    session.discovery_paths.insert(path_kind.to_string());
                }
            }
            "get_agent_skill" => {
                if let Some(skill_id) = arguments
                    .and_then(|arguments| arguments.get("skill_id"))
                    .and_then(Value::as_str)
                {
                    session.read_skills.insert(skill_id.to_string());
                }
            }
            "take_screenshot" if session.authored => session.rendered_output = true,
            tool if REQUIRED_VERIFICATION_TOOLS.contains(&tool) && session.authored => {
                session.verification_tools.insert(tool.to_string());
            }
            tool if changes_authored_model(tool) => {
                session.authored = true;
                // Evidence must describe the latest authored state, not a
                // snapshot captured before the most recent mutation.
                session.verification_tools.clear();
                session.rendered_output = false;
            }
            _ => {}
        }
    }

    pub fn completion_block(&self) -> Option<GuidanceBlock> {
        let guard = self.0.lock().expect("guidance session lock poisoned");
        let session = guard.as_ref()?;
        if !session.authored || session.phase() == GuidancePhase::Complete {
            return None;
        }
        let state = session.snapshot();
        Some(GuidanceBlock {
            kind: "GuidanceBlock".to_string(),
            reason: format!(
                "Talos3D authoring evidence is incomplete. {}",
                state.compact_context
            ),
            guidance_state: state,
        })
    }
}

impl GuidanceSession {
    fn bootstrap_complete(&self) -> bool {
        self.guidance_loaded
            && self
                .required_cards
                .iter()
                .all(|card| self.read_cards.contains(card))
            && self
                .required_skills
                .iter()
                .all(|skill| self.read_skills.contains(skill))
    }

    fn discovery_complete(&self) -> bool {
        self.task.is_none()
            || REQUIRED_DISCOVERY_PATHS
                .iter()
                .all(|path| self.discovery_paths.contains(*path))
    }

    fn verification_complete(&self) -> bool {
        REQUIRED_VERIFICATION_TOOLS
            .iter()
            .all(|tool| self.verification_tools.contains(*tool))
    }

    fn phase(&self) -> GuidancePhase {
        if !self.bootstrap_complete() {
            GuidancePhase::Bootstrap
        } else if !self.discovery_complete() {
            GuidancePhase::Discover
        } else if !self.authored {
            GuidancePhase::Plan
        } else if !self.verification_complete() {
            GuidancePhase::Validate
        } else if !self.rendered_output {
            GuidancePhase::Inspect
        } else {
            GuidancePhase::Complete
        }
    }

    fn preflight(&self, tool: &str) -> Option<GuidanceBlock> {
        if !requires_guidance(tool) || (self.bootstrap_complete() && self.discovery_complete()) {
            return None;
        }
        let state = self.snapshot();
        Some(GuidanceBlock {
            kind: "GuidanceBlock".to_string(),
            reason: format!(
                "Tool '{tool}' would mutate Talos3D before the live guidance bootstrap is complete. {}",
                state.compact_context
            ),
            guidance_state: state,
        })
    }

    fn snapshot(&self) -> AgentGuidanceState {
        let phase = self.phase();
        let mut satisfied = Vec::new();
        let mut blocking = Vec::new();
        let mut next_actions = Vec::new();

        if self.guidance_loaded {
            satisfied.push("authoring-guidance-loaded".to_string());
        } else if self.guidance_unavailable {
            blocking.push(GuidanceObligation {
                id: "authoritative-guidance-unavailable".to_string(),
                reason: "This running app composition has no authoritative authoring guidance. Do not mutate the model; connect to a capability composition that serves guidance or initialize one during onboarding."
                    .to_string(),
                blocking: true,
            });
        } else {
            blocking.push(GuidanceObligation {
                id: "load-authoring-guidance".to_string(),
                reason: "Load the authoritative contract served by the running instance."
                    .to_string(),
                blocking: true,
            });
        }

        for card in &self.required_cards {
            if self.read_cards.contains(card) {
                satisfied.push(format!("guidance-card:{card}"));
            } else {
                blocking.push(GuidanceObligation {
                    id: format!("read-guidance-card:{card}"),
                    reason: "Resolve a must-read card named by the live capability snapshot."
                        .to_string(),
                    blocking: true,
                });
            }
        }

        for skill in &self.required_skills {
            if self.read_skills.contains(skill) {
                satisfied.push(format!("agent-skill:{skill}"));
            } else {
                blocking.push(GuidanceObligation {
                    id: format!("read-agent-skill:{skill}"),
                    reason: "Resolve a must-read operating procedure named by the live capability snapshot."
                        .to_string(),
                    blocking: true,
                });
            }
        }

        if !self.bootstrap_complete() && !self.guidance_unavailable {
            next_actions.push(GuidanceNextAction {
                tool: "get_agent_guidance_packet".to_string(),
                arguments: None,
                reason:
                    "Load the authoritative guidance, every must-read card, and every must-read skill in one bounded call."
                        .to_string(),
            });
        }

        if self.bootstrap_complete() && self.task.is_some() {
            for path in REQUIRED_DISCOVERY_PATHS {
                if self.discovery_paths.contains(path) {
                    satisfied.push(format!("curated-path-probed:{path}"));
                } else {
                    blocking.push(GuidanceObligation {
                        id: format!("discover-curated-path:{path}"),
                        reason: "Probe the task-relevant curated path before authoring; an empty result is a first-class CorpusGap."
                            .to_string(),
                        blocking: true,
                    });
                    next_actions.push(GuidanceNextAction {
                        tool: "discover_curated_paths".to_string(),
                        arguments: Some(json!({
                            "path_kind": path,
                            "query": self.task.as_deref().unwrap_or_default(),
                        })),
                        reason: format!("Probe the {path} path before authoring."),
                    });
                }
            }
        }

        if self.authored {
            satisfied.push("model-authored".to_string());
            for tool in REQUIRED_VERIFICATION_TOOLS {
                if self.verification_tools.contains(tool) {
                    satisfied.push(format!("verification:{tool}"));
                } else {
                    blocking.push(GuidanceObligation {
                        id: format!("verify:{tool}"),
                        reason: "Verify the latest authored state with structured evidence."
                            .to_string(),
                        blocking: true,
                    });
                    next_actions.push(GuidanceNextAction {
                        tool: tool.to_string(),
                        arguments: None,
                        reason: "Run structured verification against the latest authored state."
                            .to_string(),
                    });
                }
            }
            if self.rendered_output {
                satisfied.push("rendered-output-captured".to_string());
            } else if self.verification_complete() {
                blocking.push(GuidanceObligation {
                    id: "inspect-rendered-output".to_string(),
                    reason: "Capture and actually inspect rendered geometry before completion."
                        .to_string(),
                    blocking: true,
                });
                next_actions.push(GuidanceNextAction {
                    tool: "take_screenshot".to_string(),
                    arguments: None,
                    reason: "Capture the verified authored state for visual inspection."
                        .to_string(),
                });
            }
        }

        let next = next_actions
            .first()
            .map(|action| format!("Next: {}.", action.tool))
            .unwrap_or_else(|| {
                if phase == GuidancePhase::Plan {
                    "Ready for a semantic edit plan using the discovered curated path.".to_string()
                } else if !blocking.is_empty() {
                    "Stop: the running instance has no executable action that can satisfy its remaining guidance obligation."
                        .to_string()
                } else {
                    "No outstanding guidance obligations.".to_string()
                }
            });
        let compact_context = format!(
            "Guidance session {} is in phase {:?} with {} blocking obligation(s). {} Treat revision {} as current.",
            self.id,
            phase,
            blocking.len(),
            next,
            self.revision_anchor,
        );

        AgentGuidanceState {
            contract: "talos3d.agent-guidance-state".to_string(),
            contract_version: 1,
            guidance_session_id: self.id.clone(),
            revision_anchor: self.revision_anchor.clone(),
            phase,
            task: self.task.clone(),
            compact_context,
            required_invariants: self.required_invariants.clone(),
            satisfied_obligations: satisfied,
            blocking_obligations: blocking,
            next_actions,
            authored_since_negotiation: self.authored,
            verification_tools_observed: REQUIRED_VERIFICATION_TOOLS
                .iter()
                .filter(|tool| self.verification_tools.contains(**tool))
                .map(|tool| (*tool).to_string())
                .collect(),
            rendered_output_observed: self.rendered_output,
        }
    }
}

fn normalize_hook_tool_name(tool: &str) -> &str {
    tool.rsplit("__").next().unwrap_or(tool)
}

pub(super) fn hook_tool_name(tool: &str) -> &str {
    normalize_hook_tool_name(tool)
}

fn read_only_name(tool: &str) -> bool {
    let leaf = tool.rsplit(['.', '_']).next().unwrap_or(tool);
    [
        "get", "list", "find", "query", "check", "validate", "describe", "explain", "inspect",
        "preview", "select", "discover", "summary", "info", "details",
    ]
    .iter()
    .any(|verb| tool.starts_with(verb) || leaf == *verb)
}

fn requires_guidance(tool: &str) -> bool {
    let tool = normalize_hook_tool_name(tool);
    if matches!(
        tool,
        "set_selection"
            | "set_subobject_selection"
            | "expand_subobject_selection"
            | "enter_group"
            | "exit_group"
    ) {
        return false;
    }
    match tool_category(tool) {
        ToolCategory::SessionContract
        | ToolCategory::Inspection
        | ToolCategory::Validation
        | ToolCategory::Capture
        | ToolCategory::UxAutomation => false,
        ToolCategory::ProjectIo => tool == "save_project" || tool.starts_with("import"),
        ToolCategory::Discovery => {
            tool.starts_with("instantiate")
                || tool.starts_with("promote")
                || tool.starts_with("save")
                || tool.starts_with("request_corpus")
        }
        ToolCategory::Unclassified => false,
        _ => !read_only_name(tool),
    }
}

fn changes_authored_model(tool: &str) -> bool {
    let tool = normalize_hook_tool_name(tool);
    if !requires_guidance(tool) {
        return false;
    }
    match tool_category(tool) {
        ToolCategory::Editing
        | ToolCategory::Commands
        | ToolCategory::Materials
        | ToolCategory::Refinement
        | ToolCategory::Definitions
        | ToolCategory::Parametric
        | ToolCategory::ModelingExtended
        | ToolCategory::Drafting2d
        | ToolCategory::BimExtended => true,
        ToolCategory::Discovery => tool.starts_with("instantiate") || tool.starts_with("promote"),
        _ => false,
    }
}

fn inactive_guidance_state() -> AgentGuidanceState {
    AgentGuidanceState {
        contract: "talos3d.agent-guidance-state".to_string(),
        contract_version: 1,
        guidance_session_id: "not-negotiated".to_string(),
        revision_anchor: "not-negotiated".to_string(),
        phase: GuidancePhase::Bootstrap,
        task: None,
        compact_context: "No active guidance session. Call negotiate_agent_session first."
            .to_string(),
        required_invariants: Vec::new(),
        satisfied_obligations: Vec::new(),
        blocking_obligations: vec![GuidanceObligation {
            id: "negotiate-agent-session".to_string(),
            reason: "Establish the live instance, task, capability, and guidance contract."
                .to_string(),
            blocking: true,
        }],
        next_actions: vec![GuidanceNextAction {
            tool: "negotiate_agent_session".to_string(),
            arguments: None,
            reason: "Start the Talos3D-owned guidance session.".to_string(),
        }],
        authored_since_negotiation: false,
        verification_tools_observed: Vec::new(),
        rendered_output_observed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_store(task: Option<&str>) -> GuidanceSessionStore {
        let store = GuidanceSessionStore::default();
        store.start(
            "revision-1".to_string(),
            task.map(str::to_string),
            vec!["card.required".to_string()],
            Vec::new(),
            vec!["Use semantic edits.".to_string()],
        );
        store.observe_success("get_authoring_guidance", None);
        store.observe_success(
            "get_guidance_card",
            Some(&Map::from_iter([(
                "card_id".to_string(),
                json!("card.required"),
            )])),
        );
        if task.is_some() {
            for path in REQUIRED_DISCOVERY_PATHS {
                store.observe_success(
                    "discover_curated_paths",
                    Some(&Map::from_iter([("path_kind".to_string(), json!(path))])),
                );
            }
        }
        store
    }

    #[test]
    fn missing_authoritative_guidance_is_a_stable_mutation_block() {
        let store = GuidanceSessionStore::default();
        store.start(
            "revision-1".to_string(),
            Some("author a wall".to_string()),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        store.mark_guidance_packet_loaded(false);

        let state = store.snapshot().unwrap();
        assert_eq!(state.phase, GuidancePhase::Bootstrap);
        assert!(state
            .blocking_obligations
            .iter()
            .any(|obligation| obligation.id == "authoritative-guidance-unavailable"));
        assert!(state.next_actions.is_empty());
        assert!(store.router_preflight("create_box").is_some());
    }

    #[test]
    fn negotiated_session_blocks_mutation_until_guidance_and_discovery_are_complete() {
        let store = GuidanceSessionStore::default();
        let initial = store.start(
            "revision-1".to_string(),
            Some("author a roof".to_string()),
            vec!["card.required".to_string()],
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(initial.phase, GuidancePhase::Bootstrap);
        assert!(store.router_preflight("create_box").is_some());

        store.observe_success("get_authoring_guidance", None);
        store.observe_success(
            "get_guidance_card",
            Some(&Map::from_iter([(
                "card_id".to_string(),
                json!("card.required"),
            )])),
        );
        assert_eq!(store.snapshot().unwrap().phase, GuidancePhase::Discover);
        assert!(store.router_preflight("create_box").is_some());

        for path in REQUIRED_DISCOVERY_PATHS {
            store.observe_success(
                "discover_curated_paths",
                Some(&Map::from_iter([("path_kind".to_string(), json!(path))])),
            );
        }
        assert_eq!(store.snapshot().unwrap().phase, GuidancePhase::Plan);
        assert!(store.router_preflight("create_box").is_none());
    }

    #[test]
    fn verification_evidence_is_invalidated_by_later_mutation() {
        let store = ready_store(None);
        store.observe_success("create_box", None);
        for tool in REQUIRED_VERIFICATION_TOOLS {
            store.observe_success(tool, None);
        }
        store.observe_success("take_screenshot", None);
        assert_eq!(store.snapshot().unwrap().phase, GuidancePhase::Complete);

        store.observe_success("transform", None);
        let state = store.snapshot().unwrap();
        assert_eq!(state.phase, GuidancePhase::Validate);
        assert!(state.verification_tools_observed.is_empty());
        assert!(!state.rendered_output_observed);
    }

    #[test]
    fn cold_hook_preflight_requires_negotiation_but_legacy_router_remains_compatible() {
        let store = GuidanceSessionStore::default();
        assert!(store.hook_preflight("mcp__talos3d__create_box").is_some());
        assert!(store.router_preflight("create_box").is_none());
        assert!(store
            .hook_preflight("mcp__talos3d__negotiate_agent_session")
            .is_none());
    }
}
