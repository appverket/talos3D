//! Authoring guidance (Slice A of the `COMPONENT_STRUCTURE` proposal).
//!
//! Per `private/architecture_knowledge/COMPONENT_STRUCTURE.md`, Talos3D owns a
//! single canonical guidance document that tells authoring agents how to
//! choose between reusable `Definition`s, derived families, and singleton
//! entities, and how that decision composes with progressive refinement.
//!
//! This module defines:
//!
//! - [`AuthoringGuidance`] — the `Resource` that stores the active guidance
//! - [`ComponentStructurePolicy`] and its substructs — the structured form
//!   consumed by validators and future tooling
//! - [`AuthoringGuidancePlugin`] — installs an empty default at startup
//! - [`AuthoringGuidanceAppExt`] — capability-side setter used to provide the
//!   actual content (architecture crate contributes it)
//!
//! v1 is intentionally a single resource, not a registry; §6.1 of the
//! proposal.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Canonical authoring guidance document.
///
/// Consumed by the in-app assistant (when assembling its system prompt) and
/// by external agents via the `get_authoring_guidance` MCP tool. If
/// [`prompt_text`] and [`component_structure`] diverge, `prompt_text` wins —
/// the markdown is authoritative.
#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AuthoringGuidance {
    /// Stable handle for MCP clients. Reserved for a future registry.
    pub guidance_id: String,
    /// Monotonic version. Bumps on intentional content change.
    pub version: u32,
    /// Authoritative markdown consumed directly as system-prompt content.
    pub prompt_text: String,
    /// Structured form of the component-structure policy. Used by validators
    /// and tooling, never the sole contract.
    pub component_structure: ComponentStructurePolicy,
    /// Pointers the agent can follow up on without guessing — e.g. related
    /// MCP tools, recipe families, catalog ids.
    pub references: Vec<GuidanceReference>,
}

impl Default for AuthoringGuidance {
    /// Empty placeholder. Replaced by a capability via
    /// [`AuthoringGuidanceAppExt::set_authoring_guidance`] during `App::build`.
    fn default() -> Self {
        Self {
            guidance_id: "authoring.empty".to_string(),
            version: 0,
            prompt_text: String::new(),
            component_structure: ComponentStructurePolicy::default(),
            references: Vec::new(),
        }
    }
}

impl AuthoringGuidance {
    /// Whether any capability has contributed real guidance content.
    pub fn is_empty(&self) -> bool {
        self.version == 0 && self.prompt_text.is_empty()
    }
}

/// Structured form of the `COMPONENT_STRUCTURE` policy (§3 of the proposal).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ComponentStructurePolicy {
    pub reuse_rule: ReuseRule,
    pub derive_rule: DeriveRule,
    /// Per-refinement-stage expectations. Keyed by `RefinementState` label.
    pub stage_expectations: Vec<StageExpectation>,
    /// Patterns the validator should advise against.
    pub anti_patterns: Vec<AntiPattern>,
}

/// Rule A — reuse. §3.1 of the proposal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ReuseRule {
    /// Short operational statement embedded in the prompt.
    pub summary: String,
    /// Parameter names agents may override on `OccurrenceIdentity` without
    /// considering a new definition (placement, orientation, etc.).
    pub placement_override_allowlist: Vec<String>,
    /// Non-placement parameters declared reusable through the `Definition`
    /// interface. Anything outside this list should be treated as a
    /// derivation signal.
    pub family_parameter_allowlist: Vec<String>,
}

/// Rule B — derive. §3.2 of the proposal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct DeriveRule {
    pub summary: String,
    /// Advisory threshold (0.0–1.0) for fractional override variance on a
    /// non-placement parameter. Above this, the validator suggests deriving.
    pub variance_threshold: f32,
}

/// Per-stage expectation (§4.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct StageExpectation {
    /// `RefinementState` label: `conceptual`, `schematic`, `constructible`,
    /// `detailed`, `fabrication_ready`.
    pub refinement_state: String,
    /// Short imperative guidance shown to the agent for this stage.
    pub guidance: String,
}

/// An authoring pattern the validator should flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AntiPattern {
    pub id: String,
    pub summary: String,
}

/// Pointer the agent can follow for more context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct GuidanceReference {
    pub kind: String,
    pub target: String,
    pub note: Option<String>,
}

/// Lightweight role tag attached to authored entities so that
/// `COMPONENT_STRUCTURE` tooling (in particular the
/// `ComponentStructureQuality` validator in Slice C) can reason about
/// "entities with the same role but no shared `Definition`" without each
/// domain capability having to invent its own role component.
///
/// The value is an opaque, domain-chosen string (e.g. `"common_truss"`,
/// `"ridge_board"`, `"sheathing"`). Slice C's reuse check looks for 2+
/// entities under one refinement parent that share a role but no shared
/// `OccurrenceIdentity`.
#[derive(Debug, Clone, Component, Serialize, Deserialize)]
pub struct AuthoringRole(pub String);

impl AuthoringRole {
    pub fn new(role: impl Into<String>) -> Self {
        Self(role.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Installs an empty [`AuthoringGuidance`] so every downstream system can
/// depend on the resource being present. Capabilities override via
/// [`AuthoringGuidanceAppExt::set_authoring_guidance`].
pub struct AuthoringGuidancePlugin;

impl Plugin for AuthoringGuidancePlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<AuthoringGuidance>() {
            app.insert_resource(AuthoringGuidance::default());
        }
    }
}

/// Sugar for capability plugins contributing guidance content.
pub trait AuthoringGuidanceAppExt {
    /// Replace the active [`AuthoringGuidance`]. Last writer wins; v1 has a
    /// single canonical document so there is no merge step.
    fn set_authoring_guidance(&mut self, guidance: AuthoringGuidance) -> &mut Self;
}

impl AuthoringGuidanceAppExt for App {
    fn set_authoring_guidance(&mut self, guidance: AuthoringGuidance) -> &mut Self {
        self.insert_resource(guidance);
        self
    }
}
