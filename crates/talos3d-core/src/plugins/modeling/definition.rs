//! Reusable Definition Foundation (PP51).
//!
//! A `Definition` is a parameterised, versioned template for a reusable modeled element.
//! A `DefinitionRegistry` holds all live definitions and can resolve parameter
//! values for a given set of occurrence overrides.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Global counters
// ---------------------------------------------------------------------------

static DEFINITION_COUNTER: AtomicU64 = AtomicU64::new(0);
static DEFINITION_LIBRARY_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Identifier helpers
// ---------------------------------------------------------------------------

fn allocate_stable_id(prefix: &str, counter: &AtomicU64) -> String {
    let counter = counter.fetch_add(1, Ordering::Relaxed);
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    format!("{prefix}-{timestamp_ms}-{counter}")
}

// ---------------------------------------------------------------------------
// DefinitionId
// ---------------------------------------------------------------------------

/// Unique, stable string identifier for a `Definition`.
///
/// Generated from a millisecond-precision timestamp combined with a
/// process-global monotonic counter, giving uniqueness across restarts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DefinitionId(pub String);

impl DefinitionId {
    /// Allocate a new, globally unique `DefinitionId`.
    pub fn new() -> Self {
        Self(allocate_stable_id("def", &DEFINITION_COUNTER))
    }

    /// Borrow the raw string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for DefinitionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DefinitionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// DefinitionLibraryId
// ---------------------------------------------------------------------------

/// Unique identifier for a reusable definition library.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DefinitionLibraryId(pub String);

impl DefinitionLibraryId {
    pub fn new() -> Self {
        Self(allocate_stable_id("lib", &DEFINITION_LIBRARY_COUNTER))
    }
}

impl Default for DefinitionLibraryId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DefinitionLibraryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// DefinitionVersion
// ---------------------------------------------------------------------------

/// Monotonically increasing version stamp on a `Definition`.
pub type DefinitionVersion = u32;

// ---------------------------------------------------------------------------
// DefinitionKind
// ---------------------------------------------------------------------------

/// Broad category of what a `Definition` produces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefinitionKind {
    /// A volumetric solid element (walls, slabs, columns, …).
    Solid,
    /// A 2-D annotation or symbol.
    Annotation,
}

// ---------------------------------------------------------------------------
// ParamType
// ---------------------------------------------------------------------------

/// The value type of a definition parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamType {
    /// Floating-point numeric value.
    Numeric,
    /// Boolean flag.
    Boolean,
    /// One of a fixed set of string variants.
    Enum(Vec<String>),
    /// Arbitrary UTF-8 text.
    StringVal,
}

// ---------------------------------------------------------------------------
// OverridePolicy
// ---------------------------------------------------------------------------

/// Governs how an occurrence may modify a parameter's default value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverridePolicy {
    /// The occurrence may not change this value.
    Locked,
    /// The occurrence may freely change this value.
    Overridable,
    /// The occurrence must supply a value (no built-in default is meaningful).
    Required,
}

// ---------------------------------------------------------------------------
// Parameter metadata
// ---------------------------------------------------------------------------

/// Distinguishes direct author inputs from system-computed parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParameterMutability {
    Input,
    Derived,
}

impl Default for ParameterMutability {
    fn default() -> Self {
        Self::Input
    }
}

/// Extended metadata used by agents and UI when authoring parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParameterMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default)]
    pub mutability: ParameterMutability,
}

// ---------------------------------------------------------------------------
// ParameterDef
// ---------------------------------------------------------------------------

/// Declaration of a single typed parameter within a `Definition`'s interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterDef {
    /// Machine-readable name, used as the key in override maps.
    pub name: String,
    /// Value type.
    pub param_type: ParamType,
    /// Default value used when no occurrence override is present.
    pub default_value: serde_json::Value,
    /// Whether and how occurrences may override this parameter.
    pub override_policy: OverridePolicy,
    /// Optional metadata for authoring and validation.
    #[serde(default)]
    pub metadata: ParameterMetadata,
}

impl ParameterDef {
    pub fn validate_value(&self, value: &Value, context: &str) -> Result<(), String> {
        validate_param_type(&self.param_type, value, context)?;

        if let Some(min) = &self.metadata.min {
            validate_numeric_bound(value, min, true, context)?;
        }
        if let Some(max) = &self.metadata.max {
            validate_numeric_bound(value, max, false, context)?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ParameterSchema
// ---------------------------------------------------------------------------

/// Ordered list of `ParameterDef`s forming the full interface of a `Definition`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParameterSchema(pub Vec<ParameterDef>);

impl ParameterSchema {
    /// Look up a parameter definition by name.
    pub fn get(&self, name: &str) -> Option<&ParameterDef> {
        self.0.iter().find(|p| p.name == name)
    }
}

// ---------------------------------------------------------------------------
// RepresentationKind / RepresentationRole / RepresentationDecl
// ---------------------------------------------------------------------------

/// The geometric role a representation plays in space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepresentationKind {
    /// Full volumetric body.
    Body,
    /// Centre-line or reference axis.
    Axis,
    /// Horizontal footprint projection.
    Footprint,
    /// Axis-aligned bounding box proxy.
    BoundingBox,
}

/// Semantic purpose of a representation within a definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepresentationRole {
    /// The primary geometry used for rendering and analysis.
    PrimaryGeometry,
    /// A 2-D annotation layer.
    Annotation,
    /// A lightweight reference geometry (e.g. snap axis).
    Reference,
}

/// Declaration pairing a `RepresentationKind` with a `RepresentationRole`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepresentationDecl {
    pub kind: RepresentationKind,
    pub role: RepresentationRole,
}

// ---------------------------------------------------------------------------
// Evaluators
// ---------------------------------------------------------------------------

/// Parameters needed to evaluate a rectangular prism via `ProfileExtrusion`.
///
/// Each field names the corresponding parameter in the definition's
/// `ParameterSchema` — the values are resolved at evaluation time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RectangularExtrusionEvaluator {
    /// Parameter name for the X dimension (width).
    pub width_param: String,
    /// Parameter name for the Y dimension (depth / length).
    pub depth_param: String,
    /// Parameter name for the Z dimension (height).
    pub height_param: String,
}

/// Discriminated union of all supported evaluator strategies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvaluatorDecl {
    /// Extrude a rectangular `Profile2d` to produce a box-like solid.
    RectangularExtrusion(RectangularExtrusionEvaluator),
}

// ---------------------------------------------------------------------------
// Compound-definition graph
// ---------------------------------------------------------------------------

/// Minimal expression tree used for derived values, bindings, and constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExprNode {
    Literal {
        value: Value,
    },
    ParamRef {
        path: String,
    },
    Add {
        left: Box<ExprNode>,
        right: Box<ExprNode>,
    },
    Sub {
        left: Box<ExprNode>,
        right: Box<ExprNode>,
    },
    Mul {
        left: Box<ExprNode>,
        right: Box<ExprNode>,
    },
    Div {
        left: Box<ExprNode>,
        right: Box<ExprNode>,
    },
    Min {
        left: Box<ExprNode>,
        right: Box<ExprNode>,
    },
    Max {
        left: Box<ExprNode>,
        right: Box<ExprNode>,
    },
    Eq {
        left: Box<ExprNode>,
        right: Box<ExprNode>,
    },
    Gt {
        left: Box<ExprNode>,
        right: Box<ExprNode>,
    },
    Lt {
        left: Box<ExprNode>,
        right: Box<ExprNode>,
    },
    And {
        nodes: Vec<ExprNode>,
    },
    IfElse {
        condition: Box<ExprNode>,
        when_true: Box<ExprNode>,
        when_false: Box<ExprNode>,
    },
}

/// Machine-readable anchor name exposed by a definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorDef {
    pub id: String,
    pub kind: String,
}

/// A derived parameter computed from other parameters in the definition graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedParameterDef {
    pub name: String,
    pub param_type: ParamType,
    pub expr: ExprNode,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub metadata: ParameterMetadata,
}

/// Severity level for machine-checkable definition invariants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstraintSeverity {
    Warning,
    Error,
}

/// A named invariant over inputs, derived parameters, or child slots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstraintDef {
    pub id: String,
    pub expr: ExprNode,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub severity: ConstraintSeverity,
    pub message: String,
}

/// Binding from a parent expression into a child slot parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterBinding {
    pub target_param: String,
    pub expr: ExprNode,
}

/// Placement binding for a child slot relative to its parent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransformBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation: Option<Vec<ExprNode>>,
}

/// A reusable child slot within a compound definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildSlotDef {
    pub slot_id: String,
    pub role: String,
    pub definition_id: DefinitionId,
    #[serde(default)]
    pub parameter_bindings: Vec<ParameterBinding>,
    #[serde(default)]
    pub transform_binding: TransformBinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppression_expr: Option<ExprNode>,
}

/// Composition graph attached to a root definition.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompoundDefinition {
    #[serde(default)]
    pub child_slots: Vec<ChildSlotDef>,
    #[serde(default)]
    pub anchors: Vec<AnchorDef>,
    #[serde(default)]
    pub derived_parameters: Vec<DerivedParameterDef>,
    #[serde(default)]
    pub constraints: Vec<ConstraintDef>,
}

// ---------------------------------------------------------------------------
// Interface
// ---------------------------------------------------------------------------

/// The public interface of a `Definition` — the set of parameters that
/// occurrences may query and override.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Interface {
    pub parameters: ParameterSchema,
}

// ---------------------------------------------------------------------------
// Definition
// ---------------------------------------------------------------------------

/// A fully described, versioned template for a reusable modeled element.
///
/// `Definition`s are immutable once published; the `definition_version` is
/// bumped on every edit and used to propagate changes to all occurrences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Definition {
    /// Globally unique identifier.
    pub id: DefinitionId,
    /// Human-readable name displayed in the UI.
    pub name: String,
    /// Broad category of what this definition produces.
    pub definition_kind: DefinitionKind,
    /// Monotonically increasing version; bump whenever the definition changes.
    pub definition_version: DefinitionVersion,
    /// Typed parameter interface exposed to occurrences.
    pub interface: Interface,
    /// Ordered list of evaluation strategies used to produce geometry.
    pub evaluators: Vec<EvaluatorDecl>,
    /// Declared geometry representations.
    pub representations: Vec<RepresentationDecl>,
    /// Optional composition graph for compound definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compound: Option<CompoundDefinition>,
    /// Domain-specific extension payload owned by higher-level products.
    ///
    /// Core treats this as opaque JSON and round-trips it unchanged.
    #[serde(default)]
    pub domain_data: Value,
}

impl Definition {
    /// Validate internal structure and cross-definition references.
    pub fn validate_with<F>(&self, mut has_definition: F) -> Result<(), String>
    where
        F: FnMut(&DefinitionId) -> bool,
    {
        let mut parameter_names = HashSet::new();
        for param in &self.interface.parameters.0 {
            if !parameter_names.insert(param.name.clone()) {
                return Err(format!(
                    "Definition '{}' contains duplicate parameter '{}'",
                    self.name, param.name
                ));
            }

            if !(param.override_policy == OverridePolicy::Required && param.default_value.is_null())
            {
                param.validate_value(
                    &param.default_value,
                    &format!("default value for parameter '{}'", param.name),
                )?;
            }
        }

        if let Some(compound) = &self.compound {
            let mut anchor_ids = HashSet::new();
            for anchor in &compound.anchors {
                if !anchor_ids.insert(anchor.id.clone()) {
                    return Err(format!(
                        "Definition '{}' contains duplicate anchor '{}'",
                        self.name, anchor.id
                    ));
                }
            }

            let mut derived_names = HashSet::new();
            for derived in &compound.derived_parameters {
                if parameter_names.contains(&derived.name)
                    || !derived_names.insert(derived.name.clone())
                {
                    return Err(format!(
                        "Definition '{}' contains duplicate derived parameter '{}'",
                        self.name, derived.name
                    ));
                }
            }

            let mut constraint_ids = HashSet::new();
            for constraint in &compound.constraints {
                if !constraint_ids.insert(constraint.id.clone()) {
                    return Err(format!(
                        "Definition '{}' contains duplicate constraint '{}'",
                        self.name, constraint.id
                    ));
                }
            }

            let mut child_slot_ids = HashSet::new();
            for slot in &compound.child_slots {
                if !child_slot_ids.insert(slot.slot_id.clone()) {
                    return Err(format!(
                        "Definition '{}' contains duplicate child slot '{}'",
                        self.name, slot.slot_id
                    ));
                }
                if slot.definition_id == self.id {
                    return Err(format!(
                        "Definition '{}' cannot reference itself in child slot '{}'",
                        self.name, slot.slot_id
                    ));
                }
                if !has_definition(&slot.definition_id) {
                    return Err(format!(
                        "Definition '{}' child slot '{}' references missing definition '{}'",
                        self.name, slot.slot_id, slot.definition_id
                    ));
                }
                if let Some(translation) = &slot.transform_binding.translation {
                    if translation.len() != 3 {
                        return Err(format!(
                            "Definition '{}' child slot '{}' translation must contain exactly 3 expressions",
                            self.name, slot.slot_id
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Libraries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefinitionLibraryScope {
    DocumentLocal,
    ExternalFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionLibrary {
    pub id: DefinitionLibraryId,
    pub name: String,
    pub scope: DefinitionLibraryScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub definitions: HashMap<DefinitionId, Definition>,
}

impl DefinitionLibrary {
    pub fn summary(&self) -> DefinitionLibrarySummary {
        DefinitionLibrarySummary {
            library_id: self.id.to_string(),
            name: self.name.clone(),
            scope: format!("{:?}", self.scope),
            definition_count: self.definitions.len(),
            source_path: self.source_path.clone(),
        }
    }

    pub fn get(&self, definition_id: &DefinitionId) -> Option<&Definition> {
        self.definitions.get(definition_id)
    }

    pub fn insert(&mut self, definition: Definition) {
        self.definitions.insert(definition.id.clone(), definition);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionLibrarySummary {
    pub library_id: String,
    pub name: String,
    pub scope: String,
    pub definition_count: usize,
    pub source_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionLibraryFile {
    pub version: u32,
    pub library: DefinitionLibrary,
}

impl DefinitionLibraryFile {
    pub const VERSION: u32 = 1;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Resource)]
pub struct DefinitionLibraryRegistry {
    libraries: HashMap<DefinitionLibraryId, DefinitionLibrary>,
}

impl DefinitionLibraryRegistry {
    pub fn insert(&mut self, library: DefinitionLibrary) {
        self.libraries.insert(library.id.clone(), library);
    }

    pub fn create_library(
        &mut self,
        name: impl Into<String>,
        scope: DefinitionLibraryScope,
        source_path: Option<String>,
    ) -> DefinitionLibraryId {
        let library = DefinitionLibrary {
            id: DefinitionLibraryId::new(),
            name: name.into(),
            scope,
            source_path,
            tags: vec![],
            definitions: HashMap::new(),
        };
        let id = library.id.clone();
        self.insert(library);
        id
    }

    pub fn get(&self, id: &DefinitionLibraryId) -> Option<&DefinitionLibrary> {
        self.libraries.get(id)
    }

    pub fn get_mut(&mut self, id: &DefinitionLibraryId) -> Option<&mut DefinitionLibrary> {
        self.libraries.get_mut(id)
    }

    pub fn list(&self) -> Vec<&DefinitionLibrary> {
        self.libraries.values().collect()
    }

    pub fn add_definition(
        &mut self,
        library_id: &DefinitionLibraryId,
        definition: Definition,
    ) -> Result<(), String> {
        let library = self
            .libraries
            .get_mut(library_id)
            .ok_or_else(|| format!("Definition library '{}' not found", library_id))?;
        library.insert(definition);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Override resolution types
// ---------------------------------------------------------------------------

/// Records where a resolved parameter value came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ValueProvenance {
    /// Value came from the definition's default.
    DefinitionDefault,
    /// Value was supplied by the occurrence as an override.
    OccurrenceOverride,
}

/// A fully resolved parameter value together with its provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedParam {
    /// The resolved JSON value.
    pub value: serde_json::Value,
    /// Whether the value came from the definition or an occurrence override.
    pub provenance: ValueProvenance,
}

/// A map of parameter name → override value supplied by an occurrence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverrideMap(pub HashMap<String, serde_json::Value>);

impl OverrideMap {
    /// Return the override for the named parameter, if any.
    pub fn get(&self, name: &str) -> Option<&serde_json::Value> {
        self.0.get(name)
    }

    /// Insert or update an override.
    pub fn set(&mut self, name: impl Into<String>, value: serde_json::Value) {
        self.0.insert(name.into(), value);
    }
}

// ---------------------------------------------------------------------------
// DefinitionRegistry
// ---------------------------------------------------------------------------

/// Bevy resource holding all live `Definition`s indexed by `DefinitionId`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Resource)]
pub struct DefinitionRegistry {
    definitions: HashMap<DefinitionId, Definition>,
}

impl DefinitionRegistry {
    /// Insert or replace a definition.
    pub fn insert(&mut self, def: Definition) {
        self.definitions.insert(def.id.clone(), def);
    }

    /// Look up a definition by id.
    pub fn get(&self, id: &DefinitionId) -> Option<&Definition> {
        self.definitions.get(id)
    }

    /// Return references to all registered definitions in arbitrary order.
    pub fn list(&self) -> Vec<&Definition> {
        self.definitions.values().collect()
    }

    /// Remove a definition, returning it if it existed.
    pub fn remove(&mut self, id: &DefinitionId) -> Option<Definition> {
        self.definitions.remove(id)
    }

    /// Validate a definition against the current registry.
    pub fn validate_definition(&self, def: &Definition) -> Result<(), String> {
        def.validate_with(|id| self.get(id).is_some())
    }

    /// Validate occurrence overrides against the target definition.
    pub fn validate_overrides(
        &self,
        id: &DefinitionId,
        overrides: &OverrideMap,
    ) -> Result<(), String> {
        let def = self
            .get(id)
            .ok_or_else(|| format!("Definition '{}' not found", id))?;

        for (name, value) in &overrides.0 {
            let parameter =
                def.interface.parameters.get(name).ok_or_else(|| {
                    format!("Definition '{}' has no parameter '{}'", def.name, name)
                })?;

            if parameter.override_policy == OverridePolicy::Locked {
                return Err(format!(
                    "Parameter '{}' on definition '{}' is locked and cannot be overridden",
                    name, def.name
                ));
            }

            parameter.validate_value(value, &format!("override for parameter '{}'", name))?;
        }

        Ok(())
    }

    /// Resolve the effective parameter values for an occurrence and fail on
    /// invalid or missing required parameters.
    pub fn resolve_params_checked(
        &self,
        id: &DefinitionId,
        overrides: &OverrideMap,
    ) -> Result<HashMap<String, ResolvedParam>, String> {
        let def = self
            .definitions
            .get(id)
            .ok_or_else(|| format!("Definition '{}' not found", id))?;
        self.validate_overrides(id, overrides)?;

        let mut resolved = HashMap::new();
        for param_def in &def.interface.parameters.0 {
            let (value, provenance) = if let Some(override_value) = overrides.get(&param_def.name) {
                (override_value.clone(), ValueProvenance::OccurrenceOverride)
            } else {
                if param_def.override_policy == OverridePolicy::Required
                    && param_def.default_value.is_null()
                {
                    return Err(format!(
                        "Definition '{}' requires an override for parameter '{}'",
                        def.name, param_def.name
                    ));
                }
                (
                    param_def.default_value.clone(),
                    ValueProvenance::DefinitionDefault,
                )
            };

            if !(param_def.override_policy == OverridePolicy::Required && value.is_null()) {
                param_def
                    .validate_value(&value, &format!("resolved value for '{}'", param_def.name))?;
            }

            resolved.insert(param_def.name.clone(), ResolvedParam { value, provenance });
        }

        Ok(resolved)
    }

    /// Resolve the effective parameter values for an occurrence.
    pub fn resolve_params(
        &self,
        id: &DefinitionId,
        overrides: &OverrideMap,
    ) -> Option<HashMap<String, ResolvedParam>> {
        self.resolve_params_checked(id, overrides).ok()
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn validate_param_type(param_type: &ParamType, value: &Value, context: &str) -> Result<(), String> {
    match param_type {
        ParamType::Numeric if value.is_number() => Ok(()),
        ParamType::Boolean if value.is_boolean() => Ok(()),
        ParamType::StringVal if value.is_string() => Ok(()),
        ParamType::Enum(variants) => {
            let Some(string_value) = value.as_str() else {
                return Err(format!("{context} must be a string enum value"));
            };
            if variants.iter().any(|variant| variant == string_value) {
                Ok(())
            } else {
                Err(format!(
                    "{context} must be one of [{}]",
                    variants.join(", ")
                ))
            }
        }
        ParamType::Numeric => Err(format!("{context} must be numeric")),
        ParamType::Boolean => Err(format!("{context} must be boolean")),
        ParamType::StringVal => Err(format!("{context} must be a string")),
    }
}

fn validate_numeric_bound(
    value: &Value,
    bound: &Value,
    is_min: bool,
    context: &str,
) -> Result<(), String> {
    let Some(value) = value.as_f64() else {
        return Ok(());
    };
    let Some(bound) = bound.as_f64() else {
        return Ok(());
    };

    let is_valid = if is_min {
        value >= bound
    } else {
        value <= bound
    };
    if is_valid {
        Ok(())
    } else if is_min {
        Err(format!("{context} must be >= {bound}"))
    } else {
        Err(format!("{context} must be <= {bound}"))
    }
}
