//! `AuthoringScript` — normalized stored body schema for dynamic recipes.
//!
//! Per ADR-041, a recipe body is either:
//!
//! - `NativeFnRef` — a pointer to a shipped `GenerateFn` in the
//!   `CapabilityRegistry` (covered by PP81), or
//! - `AuthoringScript` — a parameterized sequence of MCP tool calls over
//!   the existing Model API surface. This module defines that schema.
//!
//! The body is deliberately a **flat typed list of calls over the
//! already-safe Model API**. No new semantic language is introduced; the
//! "Recipe IR" that early drafts contemplated is the Model API itself.
//!
//! Schema version: `SchemaVersion { kind: "recipe.v1", version: 1 }` —
//! carried on `CurationMeta.compatibility.body_schema` when an artifact
//! declares this body shape.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::identity::AssetKindId;
use super::provenance::GroundingKind;
use crate::plugins::refinement::{ClaimPath, PassageRef as _PassageRefUnused, RecipeId};

/// Step identifier, unique within a single script. Conventionally short
/// dotted or kebab-cased labels like `"create_def"`, `"place_stud.3"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct StepId(pub String);

impl StepId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identifier of an MCP tool accepted as a step target. Checked against
/// `AuthoringScript.allowed_tools` at replay time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct McpToolId(pub String);

impl McpToolId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where an output of an earlier step should be read from. Currently a
/// dotted/slashed JSON path; evaluated by the replay executor against
/// the captured step output map.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct OutputPath(pub String);

impl OutputPath {
    pub fn new(p: impl Into<String>) -> Self {
        Self(p.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Expression forming a tool argument value. Resolved by the replay
/// executor against `params` + captured step outputs + claim state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "expr", rename_all = "snake_case")]
pub enum ArgExpr {
    /// Literal JSON value; used verbatim.
    Literal { value: Value },
    /// Read from a named script parameter.
    Param { name: String },
    /// Read from the output map of an earlier step, at a JSON path.
    StepOutput { step_id: StepId, path: OutputPath },
    /// Read a claim value from the current element's `ClaimGrounding`
    /// by `ClaimPath`.
    ClaimRef { path: ClaimPath },
    /// Small arithmetic expression string evaluated at replay time.
    ///
    /// Variables resolve from script parameter bindings and captured step
    /// outputs. Supports the four arithmetic operators, unary minus,
    /// parentheses, and the following built-in functions:
    /// `tan`, `sin`, `cos`, `sqrt`, `abs`, `floor`, `ceil`, `round`.
    /// The constant `pi` is always available.
    ///
    /// Example: `"eave_y + (width / 2.0) * tan(pitch_rad)"`.
    Expr { formula: String },
    /// Compose a JSON array whose elements are themselves `ArgExpr`s,
    /// resolved recursively at replay time. This is what lets a recipe
    /// argument that must be an array — e.g. a `centre: [x, y, z]` vector or
    /// a `vertices: [[x,y,z], ...]` mesh buffer — have *computed* components
    /// rather than fixed literals. Each element may be a `Param`, an `Expr`
    /// formula, a nested `Array`, etc.
    ///
    /// Example (a box centre derived from footprint params):
    /// `{ "expr": "array", "items": [
    ///     { "expr": "expr", "formula": "(min_x + max_x) / 2.0" },
    ///     { "expr": "expr", "formula": "top_datum_m - slab_thickness_m / 2.0" },
    ///     { "expr": "expr", "formula": "(min_z + max_z) / 2.0" } ] }`.
    Array { items: Vec<ArgExpr> },
    /// Compose a JSON object whose values are themselves `ArgExpr`s, resolved
    /// recursively at replay time. Enables computed structured arguments such
    /// as `footprint: { kind: "polyline", vertices: [...] }` where the
    /// vertices are derived from parameters.
    Object { entries: BTreeMap<String, ArgExpr> },
}

/// Scope within which an `AuthoringScript` is permitted to mutate world
/// state. The replay executor wraps each dispatched call in a scope
/// guard; calls that would mutate outside the scope are rejected before
/// the call fires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MutationScope {
    /// Everything under the refinement subtree rooted at the element
    /// identified by the named parameter. The value of the parameter
    /// must be an element id.
    RefinementSubtree { root_element_param: String },
    /// New top-level authored entities in the active document/project.
    /// Used by scripts and procedural sessions that author a fresh
    /// structure from scratch rather than refining an existing entity.
    /// Permits creating and mutating top-level document content; it does
    /// **not** grant `DefinitionRegistry` / org-library writes (use
    /// `OrgLibraryDefinitions` for that) and is distinct from refining an
    /// existing subtree (`RefinementSubtree`).
    ProjectRoot,
    /// Writes to the `DefinitionRegistry` at org-library scope only.
    /// Used by scripts that author shared `Definition`s.
    OrgLibraryDefinitions,
    /// Degenerate: script is not permitted to mutate anything (pure
    /// query). Rare but useful for validators-as-scripts.
    None,
}

/// Boolean predicate over `ArgExpr`s, evaluated by the replay executor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Predicate {
    /// `lhs` and `rhs` evaluate to JSON-equal values.
    Equals { lhs: ArgExpr, rhs: ArgExpr },
    /// `expr` evaluates to a non-null value (parameter present, step
    /// output non-null, etc.).
    Defined { expr: ArgExpr },
    /// All contained predicates are true.
    And { children: Vec<Predicate> },
    /// At least one contained predicate is true.
    Or { children: Vec<Predicate> },
    /// Negation.
    Not { child: Box<Predicate> },
}

/// A single step in an `AuthoringScript`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct Step {
    pub id: StepId,
    pub tool: McpToolId,
    /// Positional/named arguments as JSON object keys. Values are
    /// `ArgExpr`s.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, ArgExpr>,
    /// Outputs this step exposes to later steps, keyed by a local label.
    /// The `OutputPath` picks a value out of the tool's response JSON.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, OutputPath>,
    /// If `true`, failing this step's `precondition` or failing to
    /// dispatch the step is a hard error. If `false`, a failed
    /// precondition silently skips the step.
    #[serde(default = "default_true")]
    pub essential: bool,
    /// Optional condition; if present and false, the step is skipped
    /// (soft skip for non-essential, hard error for essential).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition: Option<Predicate>,
}

fn default_true() -> bool {
    true
}

/// A single instruction in an `AuthoringScript`'s step sequence.
///
/// Most instructions are plain `Call` steps; the `Repeat` and `CallRecipe`
/// variants add looping and sub-recipe delegation without introducing a
/// new IR layer — they are still executed entirely through the same
/// `ToolDispatcher` and `PostconditionOracle` interfaces as `Call` steps.
///
/// Serde: uses `untagged` so existing plain `Step` JSON continues to
/// deserialise as `ScriptInstruction::Call`.  New variants use a
/// discriminator field (`kind = "repeat"` / `kind = "call_recipe"`) that
/// the `untagged` deserialiser detects via field presence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ScriptInstruction {
    /// Delegating sub-recipe call. Resolved against `RecipeArtifactRegistry`
    /// at replay time; up to `MAX_CALL_RECIPE_DEPTH` nested levels allowed.
    ///
    /// Placed before `Repeat` in the untagged discriminator order so that a
    /// JSON object containing `kind = "call_recipe"` is matched here before
    /// falling through to `Call`.
    CallRecipe {
        /// Unique identifier within the enclosing script.
        id: StepId,
        /// Must equal `"call_recipe"` in serialised form.
        #[serde(rename = "kind")]
        _kind: CallRecipeKindTag,
        /// Recipe family id as registered in `RecipeArtifactRegistry`.
        family_id: String,
        /// Parameter expressions evaluated at replay time.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        parameters: BTreeMap<String, ArgExpr>,
        /// If `Some`, bind the called recipe's root entity_id to this name
        /// so that later steps (or postconditions) can reference it via
        /// `ArgExpr::StepOutput { step_id: <this step's id>, path: "entity_id" }`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        binding: Option<String>,
    },
    /// Loop a block of sub-instructions a computed number of times.
    ///
    /// At replay time `count` is evaluated to a non-negative integer.  The
    /// `body` is then executed `count` times with `var` bound to the current
    /// loop index (0-based).  Nested `Repeat` blocks are allowed; the depth
    /// limit matches `MAX_CALL_RECIPE_DEPTH`.  Postconditions on body steps
    /// are checked per-iteration.
    Repeat {
        /// Unique identifier within the enclosing script.
        id: StepId,
        /// Must equal `"repeat"` in serialised form.
        #[serde(rename = "kind")]
        _kind: RepeatKindTag,
        /// Name of the loop-index variable (integer, 0-based) injected into
        /// the parameter environment while executing `body`.
        var: String,
        /// Expression that evaluates to the loop count (non-negative integer).
        count: ArgExpr,
        /// Instructions to execute on each iteration.
        body: Vec<ScriptInstruction>,
    },
    /// A single MCP tool dispatch — the original and most common instruction.
    Call(Step),
}

/// Discriminator for `ScriptInstruction::Repeat`.  Always serialises as
/// the string `"repeat"`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RepeatKindTag {
    #[default]
    Repeat,
}

/// Discriminator for `ScriptInstruction::CallRecipe`.  Always serialises as
/// the string `"call_recipe"`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CallRecipeKindTag {
    #[default]
    CallRecipe,
}

/// Maximum nesting depth for `ScriptInstruction::CallRecipe` and
/// `ScriptInstruction::Repeat` blocks.  Deep nesting almost certainly
/// indicates an authoring mistake; this limit also guards against cycles.
pub const MAX_CALL_RECIPE_DEPTH: usize = 16;

impl ScriptInstruction {
    /// Return the instruction's `StepId`.
    pub fn id(&self) -> &StepId {
        match self {
            Self::Call(s) => &s.id,
            Self::Repeat { id, .. } => id,
            Self::CallRecipe { id, .. } => id,
        }
    }

    /// Return the inner `Step` if this is a `Call` instruction.
    pub fn as_call(&self) -> Option<&Step> {
        if let Self::Call(s) = self {
            Some(s)
        } else {
            None
        }
    }

    /// Construct a plain `Call` instruction.
    pub fn call(step: Step) -> Self {
        Self::Call(step)
    }

    /// Collect all `StepId`s referenced by `ArgExpr::StepOutput` inside
    /// this instruction (recursing into `Repeat.body`).
    pub fn collect_step_refs(&self, out: &mut BTreeSet<StepId>) {
        match self {
            Self::Call(step) => {
                for arg in step.args.values() {
                    collect_step_refs(arg, out);
                }
                if let Some(p) = &step.precondition {
                    collect_pred_step_refs(p, out);
                }
                for pc in step.args.values() {
                    collect_step_refs(pc, out);
                }
            }
            Self::Repeat { count, body, .. } => {
                collect_step_refs(count, out);
                for instr in body {
                    instr.collect_step_refs(out);
                }
            }
            Self::CallRecipe { parameters, .. } => {
                for expr in parameters.values() {
                    collect_step_refs(expr, out);
                }
            }
        }
    }

    /// Collect all parameter names referenced by `ArgExpr::Param` inside
    /// this instruction (recursing into `Repeat.body`).
    pub fn collect_param_refs(&self, out: &mut BTreeSet<String>) {
        match self {
            Self::Call(step) => {
                for arg in step.args.values() {
                    collect_param_refs(arg, out);
                }
                if let Some(p) = &step.precondition {
                    collect_pred_param_refs(p, out);
                }
            }
            Self::Repeat {
                count, body, var, ..
            } => {
                collect_param_refs(count, out);
                // The loop variable is locally introduced — remove it from
                // the set of external params if it shadowed one.
                let mut body_params = BTreeSet::new();
                for instr in body {
                    instr.collect_param_refs(&mut body_params);
                }
                body_params.remove(var);
                out.extend(body_params);
            }
            Self::CallRecipe { parameters, .. } => {
                for expr in parameters.values() {
                    collect_param_refs(expr, out);
                }
            }
        }
    }

    /// Collect all `McpToolId`s used by `Call` steps in this instruction
    /// (recursing into `Repeat.body`).  Used by `allowed_tools` validation.
    pub fn collect_tools(&self, out: &mut BTreeSet<McpToolId>) {
        match self {
            Self::Call(step) => {
                out.insert(step.tool.clone());
            }
            Self::Repeat { body, .. } => {
                for instr in body {
                    instr.collect_tools(out);
                }
            }
            Self::CallRecipe { .. } => {
                // CallRecipe dispatches to a sub-script; no direct tool used
                // at this level.
            }
        }
    }

    /// Collect all `StepId`s declared by this instruction and its children.
    pub fn collect_declared_ids(&self, out: &mut BTreeSet<StepId>) {
        out.insert(self.id().clone());
        if let Self::Repeat { body, .. } = self {
            for instr in body {
                instr.collect_declared_ids(out);
            }
        }
    }
}

impl From<Step> for ScriptInstruction {
    fn from(step: Step) -> Self {
        Self::Call(step)
    }
}

/// Statements the script promises to establish post-run. Verified by
/// the replay executor after the last step; any unmet postcondition is
/// a hard error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "postcondition", rename_all = "snake_case")]
pub enum Postcondition {
    /// A relation of the given kind must exist between the entities
    /// resolved by `from` and `to`.
    Relation {
        relation_kind: String,
        from: ArgExpr,
        to: ArgExpr,
    },
    /// A claim at `path` must exist on the asset with a grounding
    /// compatible with `grounding`.
    Claim {
        path: ClaimPath,
        grounding: GroundingKind,
    },
    /// An obligation resolved from `obligation_id_expr` must be marked
    /// satisfied by the entity produced by the named step's primary
    /// output.
    ObligationSatisfied {
        obligation_id_expr: ArgExpr,
        by_step: StepId,
    },
}

/// Schema version this module implements. Carried on
/// `CurationMeta.compatibility.body_schema` when an artifact declares a
/// `RecipeBody::AuthoringScript` body with this shape.
pub const AUTHORING_SCRIPT_SCHEMA_VERSION: u32 = 1;

/// The full `AuthoringScript` body.
///
/// Parameters are validated against `parameter_schema` (JSON Schema
/// object) at replay time; defaults are merged from
/// `parameter_defaults`. Steps dispatch in order; each step's outputs
/// are captured by its `bindings` and available to later steps via
/// `ArgExpr::StepOutput`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AuthoringScript {
    /// JSON Schema for the parameter object.
    pub parameter_schema: Value,
    /// Default values applied when a parameter is omitted by the
    /// caller. Merged onto the caller's `params` before validation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameter_defaults: BTreeMap<String, Value>,
    /// Closed set of tools this script may dispatch. Enforced by the
    /// replay executor.
    pub allowed_tools: BTreeSet<McpToolId>,
    /// Where mutations are permitted.
    pub mutation_scope: MutationScope,
    /// The ordered list of instructions.  Plain `Call` steps are the common
    /// case; `Repeat` and `CallRecipe` are control-flow extensions.
    pub steps: Vec<ScriptInstruction>,
    /// Statements the script promises to establish.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub postconditions: Vec<Postcondition>,
}

impl AuthoringScript {
    /// Build an empty-ish template (useful in tests and when an author
    /// starts a fresh draft).
    pub fn stub(mutation_scope: MutationScope) -> Self {
        Self {
            parameter_schema: serde_json::json!({"type": "object", "properties": {}}),
            parameter_defaults: BTreeMap::new(),
            allowed_tools: BTreeSet::new(),
            mutation_scope,
            steps: Vec::new(),
            postconditions: Vec::new(),
        }
    }

    /// Reference the schema-version identifier for `CompatibilityRef`.
    pub fn schema_version_for(kind: AssetKindId) -> super::compatibility::SchemaVersion {
        super::compatibility::SchemaVersion {
            kind,
            version: AUTHORING_SCRIPT_SCHEMA_VERSION,
        }
    }

    /// Return every `StepId` referenced as a source in `ArgExpr::
    /// StepOutput`. Useful for cycle detection and dead-step analysis.
    pub fn referenced_step_ids(&self) -> BTreeSet<StepId> {
        let mut out = BTreeSet::new();
        for instr in &self.steps {
            instr.collect_step_refs(&mut out);
        }
        for pc in &self.postconditions {
            match pc {
                Postcondition::Relation { from, to, .. } => {
                    collect_step_refs(from, &mut out);
                    collect_step_refs(to, &mut out);
                }
                Postcondition::ObligationSatisfied {
                    obligation_id_expr, ..
                } => collect_step_refs(obligation_id_expr, &mut out),
                Postcondition::Claim { .. } => {}
            }
        }
        out
    }

    /// Structural validation: every `StepOutput` reference resolves to
    /// a known `step.id`, step ids are unique, steps that use each
    /// tool declared the tool in `allowed_tools`. Does not execute
    /// anything.
    pub fn validate_structure(&self) -> Result<(), AuthoringScriptStructuralError> {
        use AuthoringScriptStructuralError::*;
        let mut ids = BTreeSet::new();
        // Collect all declared ids and validate Call tools.
        self.validate_instructions(&self.steps, &mut ids)?;
        let referenced = self.referenced_step_ids();
        for r in &referenced {
            if !ids.contains(r) {
                return Err(UnknownStepReference(r.clone()));
            }
        }
        // Check parameter defaults are all referenced by at least one
        // ArgExpr::Param somewhere — not strictly required, but catches
        // stale defaults. Produce a structural warning-as-error to
        // keep scripts tidy.
        let referenced_params = self.referenced_params();
        for key in self.parameter_defaults.keys() {
            if !referenced_params.contains(key) {
                return Err(UnreferencedParameterDefault(key.clone()));
            }
        }
        Ok(())
    }

    fn validate_instructions(
        &self,
        instrs: &[ScriptInstruction],
        ids: &mut BTreeSet<StepId>,
    ) -> Result<(), AuthoringScriptStructuralError> {
        use AuthoringScriptStructuralError::*;
        for instr in instrs {
            let id = instr.id().clone();
            if !ids.insert(id.clone()) {
                return Err(DuplicateStepId(id));
            }
            match instr {
                ScriptInstruction::Call(step) => {
                    if !self.allowed_tools.contains(&step.tool) {
                        return Err(ToolNotInAllowedSet {
                            step: step.id.clone(),
                            tool: step.tool.clone(),
                        });
                    }
                }
                ScriptInstruction::Repeat { body, .. } => {
                    self.validate_instructions(body, ids)?;
                }
                ScriptInstruction::CallRecipe { .. } => {
                    // family_id resolution is deferred to replay time;
                    // no static tool-set check needed here.
                }
            }
        }
        Ok(())
    }

    fn referenced_params(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for instr in &self.steps {
            instr.collect_param_refs(&mut out);
        }
        for pc in &self.postconditions {
            match pc {
                Postcondition::Relation { from, to, .. } => {
                    collect_param_refs(from, &mut out);
                    collect_param_refs(to, &mut out);
                }
                Postcondition::ObligationSatisfied {
                    obligation_id_expr, ..
                } => collect_param_refs(obligation_id_expr, &mut out),
                Postcondition::Claim { .. } => {}
            }
        }
        // Also treat MutationScope::RefinementSubtree { root_element_param }
        // as a referenced param so that scripts can legitimately declare
        // defaults for it.
        if let MutationScope::RefinementSubtree { root_element_param } = &self.mutation_scope {
            out.insert(root_element_param.clone());
        }
        out
    }
}

fn collect_step_refs(expr: &ArgExpr, out: &mut BTreeSet<StepId>) {
    match expr {
        ArgExpr::StepOutput { step_id, .. } => {
            out.insert(step_id.clone());
        }
        ArgExpr::Array { items } => {
            for item in items {
                collect_step_refs(item, out);
            }
        }
        ArgExpr::Object { entries } => {
            for v in entries.values() {
                collect_step_refs(v, out);
            }
        }
        _ => {}
    }
}

fn collect_param_refs(expr: &ArgExpr, out: &mut BTreeSet<String>) {
    match expr {
        ArgExpr::Param { name } => {
            out.insert(name.clone());
        }
        ArgExpr::Expr { formula } => {
            // Formula-driven steps reference parameters by name inside the
            // expression string (evaluated by the relational expr engine),
            // never as `ArgExpr::Param`. Treat every identifier token in the
            // formula as a potential parameter reference. This is an
            // over-approximation (built-in names such as `sin`/`pi` are also
            // collected) which is harmless here: the only consumer is the
            // unreferenced-default tidiness check, which must not flag a
            // default that a formula actually uses.
            collect_formula_identifiers(formula, out);
        }
        ArgExpr::Array { items } => {
            for item in items {
                collect_param_refs(item, out);
            }
        }
        ArgExpr::Object { entries } => {
            for v in entries.values() {
                collect_param_refs(v, out);
            }
        }
        _ => {}
    }
}

/// Extract every identifier token (`[A-Za-z_][A-Za-z0-9_]*`) from an
/// expression formula. Domain-neutral lexical scan; does not interpret the
/// formula. Used to recognise parameter references embedded in formulas.
fn collect_formula_identifiers(formula: &str, out: &mut BTreeSet<String>) {
    let bytes = formula.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'_' || c.is_ascii_alphabetic() {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            out.insert(formula[start..i].to_string());
        } else {
            i += 1;
        }
    }
}

fn collect_pred_step_refs(pred: &Predicate, out: &mut BTreeSet<StepId>) {
    match pred {
        Predicate::Equals { lhs, rhs } => {
            collect_step_refs(lhs, out);
            collect_step_refs(rhs, out);
        }
        Predicate::Defined { expr } => collect_step_refs(expr, out),
        Predicate::And { children } | Predicate::Or { children } => {
            for c in children {
                collect_pred_step_refs(c, out);
            }
        }
        Predicate::Not { child } => collect_pred_step_refs(child, out),
    }
}

fn collect_pred_param_refs(pred: &Predicate, out: &mut BTreeSet<String>) {
    match pred {
        Predicate::Equals { lhs, rhs } => {
            collect_param_refs(lhs, out);
            collect_param_refs(rhs, out);
        }
        Predicate::Defined { expr } => collect_param_refs(expr, out),
        Predicate::And { children } | Predicate::Or { children } => {
            for c in children {
                collect_pred_param_refs(c, out);
            }
        }
        Predicate::Not { child } => collect_pred_param_refs(child, out),
    }
}

/// Structural validation failures detected by
/// [`AuthoringScript::validate_structure`].
#[derive(Debug, Clone, PartialEq)]
pub enum AuthoringScriptStructuralError {
    DuplicateStepId(StepId),
    ToolNotInAllowedSet { step: StepId, tool: McpToolId },
    UnknownStepReference(StepId),
    UnreferencedParameterDefault(String),
}

impl std::fmt::Display for AuthoringScriptStructuralError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateStepId(id) => write!(f, "duplicate step id '{}'", id.0),
            Self::ToolNotInAllowedSet { step, tool } => write!(
                f,
                "step '{}' uses tool '{}' which is not in allowed_tools",
                step.0, tool.0,
            ),
            Self::UnknownStepReference(id) => write!(f, "reference to unknown step '{}'", id.0),
            Self::UnreferencedParameterDefault(name) => {
                write!(f, "parameter_defaults entry '{name}' is never referenced")
            }
        }
    }
}

impl std::error::Error for AuthoringScriptStructuralError {}

// Suppress warning — kept visible to future readers that the shipped
// `PassageRef` and recipe-id types are available in this module's
// import paths when scripts need to cite corpus passages.
#[allow(dead_code)]
fn _touch_reexports() -> (_PassageRefUnused, RecipeId) {
    (_PassageRefUnused("".into()), RecipeId("".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> McpToolId {
        McpToolId::new(name)
    }

    fn call_step(id: &str, tool_name: &str) -> Step {
        Step {
            id: StepId::new(id),
            tool: tool(tool_name),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        }
    }

    // Convenience: returns a ScriptInstruction::Call wrapping a Step.
    fn step(id: &str, tool_name: &str) -> ScriptInstruction {
        ScriptInstruction::Call(call_step(id, tool_name))
    }

    fn minimal_script_with(
        steps: Vec<ScriptInstruction>,
        tools: Vec<McpToolId>,
    ) -> AuthoringScript {
        let mut s = AuthoringScript::stub(MutationScope::None);
        for t in tools {
            s.allowed_tools.insert(t);
        }
        s.steps = steps;
        s
    }

    #[test]
    fn stub_has_empty_step_list() {
        let s = AuthoringScript::stub(MutationScope::None);
        assert!(s.steps.is_empty());
        assert!(s.allowed_tools.is_empty());
        assert!(s.postconditions.is_empty());
        assert_eq!(
            s.parameter_schema,
            serde_json::json!({"type": "object", "properties": {}})
        );
    }

    #[test]
    fn schema_version_matches_constant() {
        let sv = AuthoringScript::schema_version_for(AssetKindId::new("recipe.v1"));
        assert_eq!(sv.version, AUTHORING_SCRIPT_SCHEMA_VERSION);
        assert_eq!(sv.version, 1);
        assert_eq!(sv.kind.as_str(), "recipe.v1");
    }

    #[test]
    fn validate_structure_rejects_duplicate_step_ids() {
        let s = minimal_script_with(vec![step("a", "t"), step("a", "t")], vec![tool("t")]);
        assert!(matches!(
            s.validate_structure(),
            Err(AuthoringScriptStructuralError::DuplicateStepId(_))
        ));
    }

    #[test]
    fn validate_structure_rejects_tool_not_in_allowed_set() {
        let s = minimal_script_with(vec![step("a", "t")], Vec::new()); // allowed_tools empty
        assert!(matches!(
            s.validate_structure(),
            Err(AuthoringScriptStructuralError::ToolNotInAllowedSet { .. })
        ));
    }

    #[test]
    fn validate_structure_rejects_unknown_step_reference() {
        let mut a = call_step("a", "t");
        a.args.insert(
            "x".into(),
            ArgExpr::StepOutput {
                step_id: StepId::new("missing"),
                path: OutputPath::new("$"),
            },
        );
        let s = minimal_script_with(vec![ScriptInstruction::Call(a)], vec![tool("t")]);
        assert!(matches!(
            s.validate_structure(),
            Err(AuthoringScriptStructuralError::UnknownStepReference(_))
        ));
    }

    #[test]
    fn validate_structure_accepts_valid_script_with_step_ref() {
        let mut a = call_step("a", "t");
        a.bindings.insert("out".into(), OutputPath::new("$.id"));
        let mut b = call_step("b", "t");
        b.args.insert(
            "source".into(),
            ArgExpr::StepOutput {
                step_id: StepId::new("a"),
                path: OutputPath::new("out"),
            },
        );
        let s = minimal_script_with(
            vec![ScriptInstruction::Call(a), ScriptInstruction::Call(b)],
            vec![tool("t")],
        );
        assert!(s.validate_structure().is_ok());
    }

    #[test]
    fn validate_structure_detects_unreferenced_parameter_default() {
        let mut s = minimal_script_with(vec![step("a", "t")], vec![tool("t")]);
        s.parameter_defaults
            .insert("orphan".into(), Value::from(42));
        assert!(matches!(
            s.validate_structure(),
            Err(AuthoringScriptStructuralError::UnreferencedParameterDefault(_))
        ));
    }

    #[test]
    fn parameter_default_referenced_only_inside_expr_formula_is_accepted() {
        // A formula-driven step references `eave_y_m` only inside an
        // `ArgExpr::Expr` formula string (never as `ArgExpr::Param`). Its
        // default must be recognised as referenced, so validation passes.
        let mut a = call_step("a", "t");
        a.args.insert(
            "center".into(),
            ArgExpr::Expr {
                formula: "(eave_y_m + member_t_m/2.0)".into(),
            },
        );
        let mut s = minimal_script_with(vec![ScriptInstruction::Call(a)], vec![tool("t")]);
        s.parameter_defaults
            .insert("eave_y_m".into(), Value::from(2.7));
        s.parameter_defaults
            .insert("member_t_m".into(), Value::from(0.09));
        assert!(
            s.validate_structure().is_ok(),
            "formula-referenced parameter defaults must not be flagged unreferenced"
        );
    }

    #[test]
    fn referenced_step_ids_walks_preconditions_and_postconditions() {
        let mut a = call_step("a", "t");
        a.bindings
            .insert("out".into(), OutputPath::new("$.element_id"));
        let mut b = call_step("b", "t");
        b.precondition = Some(Predicate::Defined {
            expr: ArgExpr::StepOutput {
                step_id: StepId::new("a"),
                path: OutputPath::new("out"),
            },
        });
        let mut s = minimal_script_with(
            vec![ScriptInstruction::Call(a), ScriptInstruction::Call(b)],
            vec![tool("t")],
        );
        s.postconditions.push(Postcondition::Relation {
            relation_kind: "bears_on".into(),
            from: ArgExpr::StepOutput {
                step_id: StepId::new("a"),
                path: OutputPath::new("out"),
            },
            to: ArgExpr::Literal {
                value: Value::from("anchor"),
            },
        });
        let refs = s.referenced_step_ids();
        assert!(refs.contains(&StepId::new("a")));
        assert!(!refs.contains(&StepId::new("b"))); // b is a step, not a ref target
    }

    #[test]
    fn arg_expr_round_trips_all_variants() {
        let variants = vec![
            ArgExpr::Literal {
                value: Value::from(42),
            },
            ArgExpr::Param {
                name: "thickness_mm".into(),
            },
            ArgExpr::StepOutput {
                step_id: StepId::new("s"),
                path: OutputPath::new("id"),
            },
            ArgExpr::ClaimRef {
                path: ClaimPath("stair/riser_height_mm".into()),
            },
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let parsed: ArgExpr = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, v);
        }
    }

    #[test]
    fn predicate_round_trips() {
        let p = Predicate::And {
            children: vec![
                Predicate::Equals {
                    lhs: ArgExpr::Param { name: "x".into() },
                    rhs: ArgExpr::Literal {
                        value: Value::from(1),
                    },
                },
                Predicate::Not {
                    child: Box::new(Predicate::Defined {
                        expr: ArgExpr::Param { name: "opt".into() },
                    }),
                },
            ],
        };
        let json = serde_json::to_string(&p).unwrap();
        let parsed: Predicate = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, p);
    }

    #[test]
    fn mutation_scope_variants_round_trip() {
        for ms in [
            MutationScope::RefinementSubtree {
                root_element_param: "element_id".into(),
            },
            MutationScope::OrgLibraryDefinitions,
            MutationScope::None,
        ] {
            let json = serde_json::to_string(&ms).unwrap();
            let parsed: MutationScope = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, ms);
        }
    }

    #[test]
    fn authoring_script_full_round_trips() {
        let mut s = AuthoringScript::stub(MutationScope::RefinementSubtree {
            root_element_param: "element_id".into(),
        });
        s.allowed_tools.insert(McpToolId::new("create_definition"));
        s.parameter_schema = serde_json::json!({
            "type": "object",
            "properties": { "length_mm": { "type": "number" } },
            "required": ["length_mm"]
        });
        s.parameter_defaults.insert("length_mm".into(), 2400.into());
        let mut step_a = call_step("create_def", "create_definition");
        step_a.args.insert(
            "length_mm".into(),
            ArgExpr::Param {
                name: "length_mm".into(),
            },
        );
        step_a
            .bindings
            .insert("def_id".into(), OutputPath::new("$.definition_id"));
        s.steps.push(ScriptInstruction::Call(step_a));
        s.postconditions.push(Postcondition::Claim {
            path: ClaimPath("length_mm".into()),
            grounding: GroundingKind::ExplicitRule(crate::plugins::refinement::RuleId(
                "length_matches_param".into(),
            )),
        });

        let json = serde_json::to_string(&s).unwrap();
        let parsed: AuthoringScript = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, s);
    }

    #[test]
    fn referenced_params_includes_mutation_scope_root_param() {
        let s = AuthoringScript::stub(MutationScope::RefinementSubtree {
            root_element_param: "element_id".into(),
        });
        let referenced = s.referenced_params();
        assert!(referenced.contains("element_id"));
    }

    #[test]
    fn postcondition_variants_round_trip() {
        let variants = vec![
            Postcondition::Relation {
                relation_kind: "bears_on".into(),
                from: ArgExpr::StepOutput {
                    step_id: StepId::new("a"),
                    path: OutputPath::new("id"),
                },
                to: ArgExpr::Param {
                    name: "host".into(),
                },
            },
            Postcondition::Claim {
                path: ClaimPath("stair/riser_height_mm".into()),
                grounding: GroundingKind::LLMHeuristic {
                    rationale: "inherited from default".into(),
                    heuristic_tag: crate::plugins::refinement::HeuristicTag("default".into()),
                },
            },
            Postcondition::ObligationSatisfied {
                obligation_id_expr: ArgExpr::ClaimRef {
                    path: ClaimPath("obligation_id".into()),
                },
                by_step: StepId::new("create_def"),
            },
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let parsed: Postcondition = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, v);
        }
    }

    #[test]
    fn excerpt_ref_still_available_from_provenance() {
        // Smoke test that the `ExcerptRef` re-export path in the
        // curation module is intact for postconditions that cite
        // excerpts; not used in the shape here directly but kept
        // importable via `super`.
        let _: crate::curation::provenance::ExcerptRef =
            crate::curation::provenance::ExcerptRef::new("§8:22");
    }

    // ---- Change-4: ArgExpr::Expr ----

    #[test]
    fn arg_expr_expr_round_trips() {
        let e = ArgExpr::Expr {
            formula: "eave_y + (width / 2.0) * tan(pitch_rad)".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        let parsed: ArgExpr = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, e);
    }

    // ---- Change-5 & 6: ScriptInstruction round-trips ----

    #[test]
    fn script_instruction_call_round_trips() {
        let instr = ScriptInstruction::Call(Step {
            id: StepId::new("create_box"),
            tool: McpToolId::new("create_box"),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        });
        let json = serde_json::to_string(&instr).unwrap();
        let parsed: ScriptInstruction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, instr);
    }

    #[test]
    fn script_instruction_repeat_round_trips() {
        let instr = ScriptInstruction::Repeat {
            id: StepId::new("place_studs"),
            _kind: RepeatKindTag::Repeat,
            var: "i".into(),
            count: ArgExpr::Param {
                name: "count".into(),
            },
            body: vec![ScriptInstruction::Call(Step {
                id: StepId::new("place_stud"),
                tool: McpToolId::new("create_box"),
                args: BTreeMap::new(),
                bindings: BTreeMap::new(),
                essential: true,
                precondition: None,
            })],
        };
        let json = serde_json::to_string(&instr).unwrap();
        let parsed: ScriptInstruction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, instr);
    }

    #[test]
    fn script_instruction_call_recipe_round_trips() {
        let instr = ScriptInstruction::CallRecipe {
            id: StepId::new("call_stud"),
            _kind: CallRecipeKindTag::CallRecipe,
            family_id: "stud_recipe".into(),
            parameters: {
                let mut m = BTreeMap::new();
                m.insert(
                    "height_mm".into(),
                    ArgExpr::Param {
                        name: "stud_height".into(),
                    },
                );
                m
            },
            binding: Some("stud_entity_id".into()),
        };
        let json = serde_json::to_string(&instr).unwrap();
        let parsed: ScriptInstruction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, instr);
    }
}
