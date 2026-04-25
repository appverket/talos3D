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
    /// The ordered list of steps.
    pub steps: Vec<Step>,
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
        for step in &self.steps {
            for arg in step.args.values() {
                collect_step_refs(arg, &mut out);
            }
            if let Some(p) = &step.precondition {
                collect_pred_step_refs(p, &mut out);
            }
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
        for step in &self.steps {
            if !ids.insert(step.id.clone()) {
                return Err(DuplicateStepId(step.id.clone()));
            }
            if !self.allowed_tools.contains(&step.tool) {
                return Err(ToolNotInAllowedSet {
                    step: step.id.clone(),
                    tool: step.tool.clone(),
                });
            }
        }
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

    fn referenced_params(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for step in &self.steps {
            for arg in step.args.values() {
                collect_param_refs(arg, &mut out);
            }
            if let Some(p) = &step.precondition {
                collect_pred_param_refs(p, &mut out);
            }
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
    if let ArgExpr::StepOutput { step_id, .. } = expr {
        out.insert(step_id.clone());
    }
}

fn collect_param_refs(expr: &ArgExpr, out: &mut BTreeSet<String>) {
    if let ArgExpr::Param { name } = expr {
        out.insert(name.clone());
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

    fn step(id: &str, tool_name: &str) -> Step {
        Step {
            id: StepId::new(id),
            tool: tool(tool_name),
            args: BTreeMap::new(),
            bindings: BTreeMap::new(),
            essential: true,
            precondition: None,
        }
    }

    fn minimal_script_with(steps: Vec<Step>, tools: Vec<McpToolId>) -> AuthoringScript {
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
        let mut a = step("a", "t");
        a.args.insert(
            "x".into(),
            ArgExpr::StepOutput {
                step_id: StepId::new("missing"),
                path: OutputPath::new("$"),
            },
        );
        let s = minimal_script_with(vec![a], vec![tool("t")]);
        assert!(matches!(
            s.validate_structure(),
            Err(AuthoringScriptStructuralError::UnknownStepReference(_))
        ));
    }

    #[test]
    fn validate_structure_accepts_valid_script_with_step_ref() {
        let mut a = step("a", "t");
        a.bindings.insert("out".into(), OutputPath::new("$.id"));
        let mut b = step("b", "t");
        b.args.insert(
            "source".into(),
            ArgExpr::StepOutput {
                step_id: StepId::new("a"),
                path: OutputPath::new("out"),
            },
        );
        let s = minimal_script_with(vec![a, b], vec![tool("t")]);
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
    fn referenced_step_ids_walks_preconditions_and_postconditions() {
        let mut a = step("a", "t");
        a.bindings
            .insert("out".into(), OutputPath::new("$.element_id"));
        let mut b = step("b", "t");
        b.precondition = Some(Predicate::Defined {
            expr: ArgExpr::StepOutput {
                step_id: StepId::new("a"),
                path: OutputPath::new("out"),
            },
        });
        let mut s = minimal_script_with(vec![a, b], vec![tool("t")]);
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
        let mut step_a = step("create_def", "create_definition");
        step_a.args.insert(
            "length_mm".into(),
            ArgExpr::Param {
                name: "length_mm".into(),
            },
        );
        step_a
            .bindings
            .insert("def_id".into(), OutputPath::new("$.definition_id"));
        s.steps.push(step_a);
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
        let _: crate::curation::provenance::ExcerptRef = crate::curation::provenance::ExcerptRef::new("§8:22");
    }
}
