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

use super::void_declaration::VoidDeclaration;

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
    /// Reference to a declared host-frame axis.
    AxisRef,
    /// Reference to a parameter on either the host or hosted side of a contract.
    ParameterRef { side: BindingSide },
}

/// Which side of a hosting contract a parameter reference targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingSide {
    Host,
    Hosted,
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParameterMutability {
    #[default]
    Input,
    Derived,
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

/// Higher-level semantic purpose of a representation within a
/// definition. Matches ADR-026 §5 `RepresentationKind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepresentationKind {
    /// The primary geometry used for rendering and analysis.
    PrimaryGeometry,
    /// A 2-D annotation layer.
    Annotation,
    /// A lightweight reference geometry (e.g. snap axis).
    Reference,
}

/// The per-representation geometric/export tag. Matches ADR-026 §5
/// `RepresentationRole`; export packs inspect this to pick the right
/// output (`Body` for the IFC `IfcShapeRepresentation` identifier
/// `"Body"`, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepresentationRole {
    /// Full volumetric body — the primary solid geometry.
    Body,
    /// Centre-line or reference axis.
    Axis,
    /// Horizontal footprint projection.
    Footprint,
    /// Axis-aligned bounding-box proxy.
    BoundingBox,
    /// 2D annotation symbol (line work, hatching, leaders, dimensions).
    /// Per ADR-026 §5; maps to the IFC `"Annotation"` representation
    /// identifier.
    Annotation,
    /// Centre-of-gravity point. Per ADR-026 §5; maps to the IFC
    /// `"CoG"` representation identifier.
    CoG,
    /// Vendor-, workflow-, or export-pack-specific representation tag.
    /// The string is opaque to core; export packs interpret it.
    Custom(String),
}

/// Level-of-detail hint for a representation. Coarse enum; export
/// packs map to format-specific LOD scales (LOD100…LOD500 in IFC,
/// 1:50/1:20/1:10 in dimensioned drawings, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LevelOfDetail {
    /// Conceptual-stage massing — schematic only.
    Conceptual,
    /// Schematic-stage representation. Default.
    #[default]
    Schematic,
    /// Detailed representation suitable for documentation drawings.
    Detailed,
    /// Fabrication-ready representation.
    Fabrication,
}

/// Declares how a representation should be (re-)evaluated when its
/// inputs change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UpdatePolicy {
    /// Re-evaluate eagerly on every parameter change. Default — matches
    /// existing behaviour for all representations registered before
    /// ADR-026 §5.
    #[default]
    Always,
    /// Re-evaluate on demand only (e.g. when the export pipeline
    /// requests it). Suitable for expensive or rarely-needed outputs
    /// like quantity takeoff or detailed shop drawings.
    OnDemand,
    /// Snapshot at first evaluation; never re-evaluate. Used for
    /// imported representations whose authoring upstream is unknown.
    Frozen,
}

/// Declaration pairing a higher-level `RepresentationKind`, an
/// export-facing `RepresentationRole`, an optional `LevelOfDetail`
/// hint, and an `UpdatePolicy`.
///
/// `lod` and `update_policy` are `Option`-defaulted so existing call
/// sites that only set `kind` and `role` continue to compile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepresentationDecl {
    pub kind: RepresentationKind,
    pub role: RepresentationRole,
    /// Optional LOD hint. `None` is treated as
    /// `LevelOfDetail::Schematic` by readers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lod: Option<LevelOfDetail>,
    /// Optional re-evaluation policy. `None` is treated as
    /// `UpdatePolicy::Always` by readers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_policy: Option<UpdatePolicy>,
}

impl RepresentationDecl {
    /// Construct a representation with default LOD (`Schematic`) and
    /// update policy (`Always`). Preserves backward compatibility with
    /// existing call sites.
    pub fn new(kind: RepresentationKind, role: RepresentationRole) -> Self {
        Self {
            kind,
            role,
            lod: None,
            update_policy: None,
        }
    }

    /// Construct a representation with explicit LOD + update policy.
    pub fn new_detailed(
        kind: RepresentationKind,
        role: RepresentationRole,
        lod: LevelOfDetail,
        update_policy: UpdatePolicy,
    ) -> Self {
        Self {
            kind,
            role,
            lod: Some(lod),
            update_policy: Some(update_policy),
        }
    }

    /// Effective LOD, with the documented `Schematic` default.
    pub fn effective_lod(&self) -> LevelOfDetail {
        self.lod.unwrap_or_default()
    }

    /// Effective update policy, with the documented `Always` default.
    pub fn effective_update_policy(&self) -> UpdatePolicy {
        self.update_policy.unwrap_or_default()
    }
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

// ---------------------------------------------------------------------------
// Slot multiplicity (PP-097 PP-DPROMOTE-3a slice 1)
// ---------------------------------------------------------------------------

/// Reference to a declared host-frame axis. Matches the
/// `ParamType::AxisRef` parameter convention; carrying a typed
/// newtype here keeps `SlotLayout` self-documenting without
/// forcing every layout author to remember the stringly-typed
/// convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisRef(pub String);

/// Reference to a parameter on either the host or hosted side of
/// a hosting contract. Same lift-out-of-`ParamType` motivation as
/// `AxisRef`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterRef {
    pub side: BindingSide,
    pub name: String,
}

/// How many children a `Collection` slot expands into. `Fixed`
/// resolves at definition time; `DerivedFromExpr` resolves at
/// occurrence-evaluation time and must yield a non-negative
/// integer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SlotCount {
    Fixed(u32),
    DerivedFromExpr(ExprNode),
}

/// Layout strategy for a `Collection` slot. PP-097 slice 1 ships
/// the data shape; the evaluation expansion (turning N children
/// into N evaluated occurrences with their resolved transforms)
/// lands in slice 2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SlotLayout {
    /// N children laid out along an axis with constant spacing.
    Linear {
        axis: AxisRef,
        spacing: ExprNode,
        #[serde(default)]
        origin: TransformBinding,
    },
    /// N×M children laid out on a 2D grid.
    Grid {
        axis_u: AxisRef,
        count_u: ExprNode,
        spacing_u: ExprNode,
        axis_v: AxisRef,
        count_v: ExprNode,
        spacing_v: ExprNode,
        #[serde(default)]
        origin: TransformBinding,
    },
    /// Children spaced according to a host-supplied parameter
    /// along an axis (e.g. truss spacing driven by a roof
    /// system's parameter).
    BySpacingFromHost {
        host_param: ParameterRef,
        axis: AxisRef,
    },
    /// Pattern-named layout (e.g. "3x2", "horizontal-3"). The
    /// pattern string is interpreted by domain-specific layout
    /// providers in slice 2.
    LitePattern { pattern: ExprNode },
}

/// Whether a child slot stands for a single child or a
/// deterministic collection of N children.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SlotMultiplicity {
    /// One child. The default; existing slots round-trip as
    /// `Single` after a deserialize-from-old-JSON without the
    /// `multiplicity` key.
    Single,
    /// N children laid out per a `SlotLayout`. The generated
    /// children expose stable indexed `slot_path`s of the form
    /// `slot_id[index]` (slice 2 — the evaluation expansion).
    Collection {
        layout: SlotLayout,
        count: SlotCount,
    },
}

impl Default for SlotMultiplicity {
    fn default() -> Self {
        Self::Single
    }
}

impl SlotMultiplicity {
    /// Returns `true` for any `Collection` variant.
    pub fn is_collection(&self) -> bool {
        matches!(self, Self::Collection { .. })
    }
}

/// Errors produced by [`ChildSlotDef::validate_multiplicity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotMultiplicityValidationError {
    /// `Collection { count: Fixed(0), .. }` is rejected at
    /// validation. Authors who want a slot to be empty should
    /// use the existing `suppression_expr` on a `Single` slot;
    /// `count: DerivedFromExpr(...)` is allowed to evaluate to 0
    /// at runtime per the agreement.
    FixedZeroCount { slot_id: String },
    /// A `Collection` slot must not have a `suppression_expr`.
    /// Authors should set the count instead.
    SuppressionOnCollection { slot_id: String },
}

impl std::fmt::Display for SlotMultiplicityValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FixedZeroCount { slot_id } => write!(
                f,
                "slot '{slot_id}': SlotCount::Fixed(0) is rejected; \
                 use a Single slot with `suppression_expr` for an empty slot, \
                 or DerivedFromExpr that evaluates to 0 at runtime",
            ),
            Self::SuppressionOnCollection { slot_id } => write!(
                f,
                "slot '{slot_id}': Collection slots cannot carry \
                 `suppression_expr`; set `count` instead",
            ),
        }
    }
}

impl std::error::Error for SlotMultiplicityValidationError {}

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
    /// PP-097 slice 1: whether this slot stands for a single child
    /// (default, backwards compatible) or a deterministic
    /// collection of N children. `#[serde(default)]` keeps
    /// pre-PP-097 project files and bundled libraries loading as
    /// `Single` without migration.
    #[serde(default)]
    pub multiplicity: SlotMultiplicity,
}

impl ChildSlotDef {
    /// Validate the multiplicity-related invariants spelled out
    /// in the PP-097 acceptance criteria.
    pub fn validate_multiplicity(&self) -> Result<(), SlotMultiplicityValidationError> {
        match &self.multiplicity {
            SlotMultiplicity::Single => Ok(()),
            SlotMultiplicity::Collection { count, .. } => {
                if matches!(count, SlotCount::Fixed(0)) {
                    return Err(SlotMultiplicityValidationError::FixedZeroCount {
                        slot_id: self.slot_id.clone(),
                    });
                }
                if self.suppression_expr.is_some() {
                    return Err(SlotMultiplicityValidationError::SuppressionOnCollection {
                        slot_id: self.slot_id.clone(),
                    });
                }
                Ok(())
            }
        }
    }
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
    /// Accepted `SemanticRelationTemplate`s harvested by PP-A2DB-2
    /// slice A's adapter classification. On Occurrence creation, the
    /// `materialize_relation_templates` helper walks these and spawns
    /// authored first-class `SemanticRelation` entities with
    /// endpoints resolved through the slot realization map.
    ///
    /// `#[serde(default, skip_serializing_if = "Vec::is_empty")]`
    /// keeps pre-PP-A2DB-2 project files loading and keeps Definitions
    /// without templates bit-stable in serialized form.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relation_templates:
        Vec<crate::plugins::promotion::SemanticRelationTemplate>,
}

// ---------------------------------------------------------------------------
// Interface
// ---------------------------------------------------------------------------

/// The public interface of a `Definition` — the set of parameters that
/// occurrences may query and override, plus any external-context
/// requirements harvested from boundary-spanning relations during
/// SemanticAssembly promotion.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Interface {
    pub parameters: ParameterSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub void_declaration: Option<VoidDeclaration>,
    /// External-context requirements (`HostContract`,
    /// `RequiredContext`, `AdvisoryContext`) produced by PP-A2DB-2
    /// slice B's classifier. Populated during emission via
    /// `with_external_context_requirements`. Slice C2 will consume
    /// these to bind `HostContract` requirements into the hosting-
    /// contract substrate; slice C4 will exercise their persistence
    /// round-trip end-to-end.
    ///
    /// `#[serde(default, skip_serializing_if = "Vec::is_empty")]`
    /// keeps old project files loading and keeps projects without
    /// any requirements bit-stable in the serialized output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_context_requirements:
        Vec<crate::plugins::promotion::ExternalContextRequirement>,
}

impl Interface {
    /// Builder helper for emitters: stamp the slice B requirements
    /// onto the Definition's interface during body construction.
    pub fn with_external_context_requirements(
        mut self,
        requirements: Vec<crate::plugins::promotion::ExternalContextRequirement>,
    ) -> Self {
        self.external_context_requirements = requirements;
        self
    }

    /// PP-A2DB-2 slice C2: filter
    /// `external_context_requirements` to entries whose
    /// `classification` is `HostContract`. Lets validators /
    /// instantiation paths walk just the hosting-contract subset
    /// without re-implementing the filter at every call site.
    pub fn iter_host_contract_requirements(
        &self,
    ) -> impl Iterator<Item = &crate::plugins::promotion::ExternalContextRequirement> + '_
    {
        self.external_context_requirements.iter().filter(|req| {
            matches!(
                req.classification,
                crate::plugins::promotion::ExternalRelationClassification::HostContract
            )
        })
    }
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
    /// Optional reusable base definition that this definition specializes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_definition_id: Option<DefinitionId>,
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
    /// Stamp `templates` onto the Definition's compound body. If the
    /// Definition was a leaf, this upgrades it to compound with an
    /// otherwise-empty body. Returns `self` so it composes with other
    /// builder helpers.
    pub fn with_relation_templates(
        mut self,
        templates: Vec<crate::plugins::promotion::SemanticRelationTemplate>,
    ) -> Self {
        let compound = self
            .compound
            .get_or_insert_with(CompoundDefinition::default);
        compound.relation_templates = templates;
        self
    }

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

        if self.base_definition_id.as_ref() == Some(&self.id) {
            return Err(format!(
                "Definition '{}' cannot inherit from itself",
                self.name
            ));
        }
        if let Some(base_definition_id) = &self.base_definition_id {
            if !has_definition(base_definition_id) {
                return Err(format!(
                    "Definition '{}' references missing base definition '{}'",
                    self.name, base_definition_id
                ));
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
    Bundled,
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

    /// Remove an override, returning its previous value if any.
    pub fn remove(&mut self, name: &str) -> Option<serde_json::Value> {
        self.0.remove(name)
    }

    /// Returns `true` if the map contains an override for `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
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
        def.validate_with(|id| id == &def.id || self.get(id).is_some())?;

        let mut preview = self.clone();
        preview.insert(def.clone());
        let _ = preview.effective_definition(&def.id)?;
        Ok(())
    }

    /// Validate occurrence overrides against the target definition.
    pub fn validate_overrides(
        &self,
        id: &DefinitionId,
        overrides: &OverrideMap,
    ) -> Result<(), String> {
        self.validate_param_overrides(id, overrides, false)
    }

    /// Validate bound parameter values produced by an owning definition.
    ///
    /// Internal definition bindings are allowed to drive locked child
    /// parameters because those values are part of the family definition, not
    /// user-authored occurrence overrides.
    pub fn validate_bound_overrides(
        &self,
        id: &DefinitionId,
        overrides: &OverrideMap,
    ) -> Result<(), String> {
        self.validate_param_overrides(id, overrides, true)
    }

    fn validate_param_overrides(
        &self,
        id: &DefinitionId,
        overrides: &OverrideMap,
        allow_locked: bool,
    ) -> Result<(), String> {
        let def = self.effective_definition(id)?;

        for (name, value) in &overrides.0 {
            let parameter =
                def.interface.parameters.get(name).ok_or_else(|| {
                    format!("Definition '{}' has no parameter '{}'", def.name, name)
                })?;

            if !allow_locked && parameter.override_policy == OverridePolicy::Locked {
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
        self.resolve_params_checked_internal(id, overrides, false)
    }

    /// Resolve effective parameter values when the inputs come from internal
    /// definition bindings instead of direct occurrence overrides.
    pub fn resolve_bound_params_checked(
        &self,
        id: &DefinitionId,
        overrides: &OverrideMap,
    ) -> Result<HashMap<String, ResolvedParam>, String> {
        self.resolve_params_checked_internal(id, overrides, true)
    }

    fn resolve_params_checked_internal(
        &self,
        id: &DefinitionId,
        overrides: &OverrideMap,
        allow_locked: bool,
    ) -> Result<HashMap<String, ResolvedParam>, String> {
        let def = self.effective_definition(id)?;
        self.validate_param_overrides(id, overrides, allow_locked)?;

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

    /// Resolve a definition together with any base-definition ancestry.
    pub fn effective_definition(&self, id: &DefinitionId) -> Result<Definition, String> {
        self.effective_definition_internal(id, &mut Vec::new())
    }

    fn effective_definition_internal(
        &self,
        id: &DefinitionId,
        stack: &mut Vec<DefinitionId>,
    ) -> Result<Definition, String> {
        if stack.contains(id) {
            let mut cycle = stack.iter().map(ToString::to_string).collect::<Vec<_>>();
            cycle.push(id.to_string());
            return Err(format!(
                "Definition inheritance cycle detected: {}",
                cycle.join(" -> ")
            ));
        }

        let definition = self
            .definitions
            .get(id)
            .cloned()
            .ok_or_else(|| format!("Definition '{}' not found", id))?;
        stack.push(id.clone());
        let effective = if let Some(base_definition_id) = &definition.base_definition_id {
            let base = self.effective_definition_internal(base_definition_id, stack)?;
            merge_definition(base, definition)
        } else {
            definition
        };
        stack.pop();
        Ok(effective)
    }
}

fn merge_definition(base: Definition, child: Definition) -> Definition {
    let Definition {
        id,
        base_definition_id,
        name,
        definition_kind,
        definition_version,
        interface,
        evaluators,
        representations,
        compound,
        domain_data,
    } = child;

    let interface = merge_interface(base.interface, interface);

    Definition {
        id,
        base_definition_id,
        name,
        definition_kind,
        definition_version,
        interface,
        evaluators: if evaluators.is_empty() {
            base.evaluators
        } else {
            evaluators
        },
        representations: if representations.is_empty() {
            base.representations
        } else {
            representations
        },
        compound: merge_compound_definition(base.compound, compound),
        domain_data: merge_json_values(base.domain_data, domain_data),
    }
}

fn merge_interface(base: Interface, child: Interface) -> Interface {
    // External-context requirements: the child wins when it has any;
    // otherwise inherit from the base. This keeps the "promotion
    // recorded these requirements on the parent and the derived
    // variant didn't override" case clean while still allowing a
    // derived definition to clear or replace the requirement set.
    let external_context_requirements = if !child.external_context_requirements.is_empty() {
        child.external_context_requirements
    } else {
        base.external_context_requirements
    };
    Interface {
        parameters: merge_parameter_schema(base.parameters, child.parameters),
        void_declaration: child.void_declaration.or(base.void_declaration),
        external_context_requirements,
    }
}

fn merge_parameter_schema(base: ParameterSchema, child: ParameterSchema) -> ParameterSchema {
    let mut merged = base.0;
    for parameter in child.0 {
        if let Some(existing) = merged.iter_mut().find(|entry| entry.name == parameter.name) {
            *existing = parameter;
        } else {
            merged.push(parameter);
        }
    }
    ParameterSchema(merged)
}

fn merge_compound_definition(
    base: Option<CompoundDefinition>,
    child: Option<CompoundDefinition>,
) -> Option<CompoundDefinition> {
    match (base, child) {
        (Some(base), Some(child)) => Some(CompoundDefinition {
            child_slots: merge_named_items(base.child_slots, child.child_slots, |slot| {
                slot.slot_id.clone()
            }),
            anchors: merge_named_items(base.anchors, child.anchors, |anchor| anchor.id.clone()),
            derived_parameters: merge_named_items(
                base.derived_parameters,
                child.derived_parameters,
                |derived| derived.name.clone(),
            ),
            constraints: merge_named_items(base.constraints, child.constraints, |constraint| {
                constraint.id.clone()
            }),
            // Same inherit-or-replace semantics as `merge_interface`'s
            // external_context_requirements: a derived definition with
            // a non-empty template list overrides; otherwise inherit
            // from base.
            relation_templates: if !child.relation_templates.is_empty() {
                child.relation_templates
            } else {
                base.relation_templates
            },
        }),
        (Some(base), None) => Some(base),
        (None, Some(child)) => Some(child),
        (None, None) => None,
    }
}

fn merge_named_items<T, F>(base: Vec<T>, child: Vec<T>, key: F) -> Vec<T>
where
    F: Fn(&T) -> String,
{
    let mut merged = base;
    for item in child {
        let item_key = key(&item);
        if let Some(existing) = merged.iter_mut().find(|entry| key(entry) == item_key) {
            *existing = item;
        } else {
            merged.push(item);
        }
    }
    merged
}

fn merge_json_values(base: Value, child: Value) -> Value {
    match (base, child) {
        (base, Value::Null) => base,
        (Value::Object(mut base_map), Value::Object(child_map)) => {
            for (key, value) in child_map {
                let merged_value = if let Some(existing) = base_map.remove(&key) {
                    merge_json_values(existing, value)
                } else {
                    value
                };
                base_map.insert(key, merged_value);
            }
            Value::Object(base_map)
        }
        (_, child) => child,
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
        ParamType::AxisRef if value.is_string() => Ok(()),
        ParamType::ParameterRef { .. } if value.is_string() => Ok(()),
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
        ParamType::AxisRef => Err(format!("{context} must be a host-frame axis reference")),
        ParamType::ParameterRef { side } => Err(format!(
            "{context} must be a parameter reference on the {side:?} side"
        )),
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

#[cfg(test)]
mod adr_026_phase_6c_tests {
    use super::*;

    #[test]
    fn representation_decl_new_uses_no_lod_or_policy_field() {
        let decl = RepresentationDecl::new(
            RepresentationKind::PrimaryGeometry,
            RepresentationRole::Body,
        );
        assert!(decl.lod.is_none());
        assert!(decl.update_policy.is_none());
    }

    #[test]
    fn representation_decl_effective_defaults_to_schematic_and_always() {
        let decl = RepresentationDecl::new(
            RepresentationKind::PrimaryGeometry,
            RepresentationRole::Body,
        );
        assert_eq!(decl.effective_lod(), LevelOfDetail::Schematic);
        assert_eq!(decl.effective_update_policy(), UpdatePolicy::Always);
    }

    #[test]
    fn representation_decl_new_detailed_carries_lod_and_policy() {
        let decl = RepresentationDecl::new_detailed(
            RepresentationKind::Reference,
            RepresentationRole::CoG,
            LevelOfDetail::Conceptual,
            UpdatePolicy::OnDemand,
        );
        assert_eq!(decl.lod, Some(LevelOfDetail::Conceptual));
        assert_eq!(decl.update_policy, Some(UpdatePolicy::OnDemand));
        assert_eq!(decl.effective_lod(), LevelOfDetail::Conceptual);
        assert_eq!(decl.effective_update_policy(), UpdatePolicy::OnDemand);
    }

    #[test]
    fn representation_role_new_variants_exist_and_serialize() {
        for kind in [
            RepresentationRole::Body,
            RepresentationRole::Axis,
            RepresentationRole::Footprint,
            RepresentationRole::BoundingBox,
            RepresentationRole::Annotation,
            RepresentationRole::CoG,
            RepresentationRole::Custom("vendor.qto".into()),
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let parsed: RepresentationRole = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn representation_decl_round_trips_with_optional_fields_omitted() {
        let decl = RepresentationDecl::new(
            RepresentationKind::Annotation,
            RepresentationRole::Annotation,
        );
        let json = serde_json::to_string(&decl).unwrap();
        // Optional fields should be skipped on serialize when None.
        assert!(!json.contains("\"lod\""));
        assert!(!json.contains("\"update_policy\""));
        let parsed: RepresentationDecl = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.kind, decl.kind);
        assert_eq!(parsed.role, decl.role);
        assert!(parsed.lod.is_none());
        assert!(parsed.update_policy.is_none());
    }

    #[test]
    fn representation_decl_round_trips_with_optional_fields_set() {
        let decl = RepresentationDecl::new_detailed(
            RepresentationKind::PrimaryGeometry,
            RepresentationRole::Footprint,
            LevelOfDetail::Detailed,
            UpdatePolicy::Frozen,
        );
        let json = serde_json::to_string(&decl).unwrap();
        let parsed: RepresentationDecl = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.lod, Some(LevelOfDetail::Detailed));
        assert_eq!(parsed.update_policy, Some(UpdatePolicy::Frozen));
    }

    #[test]
    fn level_of_detail_default_is_schematic() {
        let lod: LevelOfDetail = Default::default();
        assert_eq!(lod, LevelOfDetail::Schematic);
    }

    #[test]
    fn update_policy_default_is_always() {
        let pol: UpdatePolicy = Default::default();
        assert_eq!(pol, UpdatePolicy::Always);
    }
}

#[cfg(test)]
mod pp_dhost_param_type_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn existing_param_types_still_deserialize_from_library_json() {
        for (json_value, expected) in [
            (json!("Numeric"), ParamType::Numeric),
            (json!("Boolean"), ParamType::Boolean),
            (json!("StringVal"), ParamType::StringVal),
            (
                json!({"Enum": ["a", "b"]}),
                ParamType::Enum(vec!["a".into(), "b".into()]),
            ),
        ] {
            let parsed: ParamType = serde_json::from_value(json_value).unwrap();
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn hosting_reference_param_types_round_trip() {
        for param_type in [
            ParamType::AxisRef,
            ParamType::ParameterRef {
                side: BindingSide::Host,
            },
            ParamType::ParameterRef {
                side: BindingSide::Hosted,
            },
        ] {
            let json = serde_json::to_value(&param_type).unwrap();
            let parsed: ParamType = serde_json::from_value(json).unwrap();
            assert_eq!(parsed, param_type);
        }
    }

    #[test]
    fn hosting_reference_param_types_validate_string_references() {
        let axis = ParameterDef {
            name: "host_normal_axis".into(),
            param_type: ParamType::AxisRef,
            default_value: json!("host.normal"),
            override_policy: OverridePolicy::Locked,
            metadata: ParameterMetadata::default(),
        };
        axis.validate_value(&json!("host.normal"), "axis").unwrap();
        assert!(axis.validate_value(&json!(42), "axis").is_err());

        let parameter_ref = ParameterDef {
            name: "host_wall_thickness".into(),
            param_type: ParamType::ParameterRef {
                side: BindingSide::Host,
            },
            default_value: json!("wall_thickness"),
            override_policy: OverridePolicy::Locked,
            metadata: ParameterMetadata::default(),
        };
        parameter_ref
            .validate_value(&json!("wall_thickness"), "parameter reference")
            .unwrap();
        assert!(parameter_ref
            .validate_value(&json!(false), "parameter reference")
            .is_err());
    }
}

// === PP-A2DB-2 slice C1: external_context_requirements on Interface =======

#[cfg(test)]
mod pp_a2db_2_external_context_tests {
    use super::*;
    use crate::plugins::identity::ElementId;
    use crate::plugins::promotion::{
        ExternalContextRequirement, ExternalRelationClassification, RelationEndpoint,
    };
    use serde_json::json;

    fn sample_requirement() -> ExternalContextRequirement {
        ExternalContextRequirement {
            relation_type: "hosted_on_wall".into(),
            classification: ExternalRelationClassification::HostContract,
            endpoint_in_definition: RelationEndpoint::Slot("frame".into()),
            descriptor_id: Some("hosted_on_wall".into()),
            host_contract_kind: None,
            source_relation_id: ElementId(30),
        }
    }

    #[test]
    fn interface_default_has_empty_external_context_requirements() {
        let i = Interface::default();
        assert!(i.external_context_requirements.is_empty());
    }

    #[test]
    fn interface_with_external_context_requirements_builder_writes_field() {
        let req = sample_requirement();
        let i = Interface::default().with_external_context_requirements(vec![req.clone()]);
        assert_eq!(i.external_context_requirements, vec![req]);
    }

    #[test]
    fn interface_skips_serializing_empty_external_context_requirements() {
        let i = Interface::default();
        let json = serde_json::to_string(&i).unwrap();
        assert!(
            !json.contains("external_context_requirements"),
            "an empty list should not appear in the serialized form to keep \
             pre-PP-A2DB-2 project files bit-stable; got: {json}"
        );
    }

    #[test]
    fn interface_serializes_non_empty_external_context_requirements() {
        let i = Interface::default()
            .with_external_context_requirements(vec![sample_requirement()]);
        let json = serde_json::to_string(&i).unwrap();
        assert!(json.contains("external_context_requirements"));
        assert!(json.contains("hosted_on_wall"));
        // Round-trip.
        let back: Interface = serde_json::from_str(&json).unwrap();
        assert_eq!(back.external_context_requirements.len(), 1);
        assert_eq!(
            back.external_context_requirements[0].classification,
            ExternalRelationClassification::HostContract
        );
    }

    #[test]
    fn interface_loads_pre_pp_a2db_2_project_with_field_missing() {
        // Simulate an old project file that predates the new field.
        let legacy = json!({
            "parameters": [],
            "void_declaration": null,
        });
        let i: Interface = serde_json::from_value(legacy).unwrap();
        assert!(i.external_context_requirements.is_empty());
    }

    #[test]
    fn merge_interface_inherits_requirements_from_base_when_child_is_empty() {
        let base = Interface::default()
            .with_external_context_requirements(vec![sample_requirement()]);
        let child = Interface::default();
        let merged = merge_interface(base.clone(), child);
        assert_eq!(merged.external_context_requirements, base.external_context_requirements);
    }

    #[test]
    fn merge_interface_replaces_requirements_when_child_has_any() {
        let base = Interface::default()
            .with_external_context_requirements(vec![sample_requirement()]);
        let other = ExternalContextRequirement {
            relation_type: "near_room".into(),
            classification: ExternalRelationClassification::AdvisoryContext,
            endpoint_in_definition: RelationEndpoint::SelfRoot,
            descriptor_id: None,
            host_contract_kind: None,
            source_relation_id: ElementId(31),
        };
        let child = Interface::default()
            .with_external_context_requirements(vec![other.clone()]);
        let merged = merge_interface(base, child);
        assert_eq!(merged.external_context_requirements, vec![other]);
    }
}

// === PP-097 PP-DPROMOTE-3a slice 1: slot multiplicity data shape ============

#[cfg(test)]
mod pp_097_slot_multiplicity_tests {
    use super::*;
    use serde_json::json;

    fn slot_with(multiplicity: SlotMultiplicity) -> ChildSlotDef {
        ChildSlotDef {
            slot_id: "lite".to_string(),
            role: "lite".to_string(),
            definition_id: DefinitionId("pane".to_string()),
            parameter_bindings: Vec::new(),
            transform_binding: TransformBinding::default(),
            suppression_expr: None,
            multiplicity,
        }
    }

    #[test]
    fn slot_multiplicity_default_is_single() {
        let m = SlotMultiplicity::default();
        assert!(matches!(m, SlotMultiplicity::Single));
        assert!(!m.is_collection());
    }

    #[test]
    fn slot_multiplicity_collection_reports_is_collection() {
        let m = SlotMultiplicity::Collection {
            layout: SlotLayout::Linear {
                axis: AxisRef("x".into()),
                spacing: ExprNode::Literal { value: json!(1.0) },
                origin: TransformBinding::default(),
            },
            count: SlotCount::Fixed(3),
        };
        assert!(m.is_collection());
    }

    #[test]
    fn child_slot_default_loads_pre_pp097_json_as_single() {
        // Pre-PP-097 JSON: no `multiplicity` key.
        let legacy = json!({
            "slot_id": "frame",
            "role": "frame",
            "definition_id": "lib.frame",
            "parameter_bindings": [],
            "transform_binding": {},
            "suppression_expr": null,
        });
        let slot: ChildSlotDef = serde_json::from_value(legacy).unwrap();
        assert!(matches!(slot.multiplicity, SlotMultiplicity::Single));
    }

    #[test]
    fn child_slot_collection_round_trips_through_serde() {
        let original = slot_with(SlotMultiplicity::Collection {
            layout: SlotLayout::Linear {
                axis: AxisRef("u".into()),
                spacing: ExprNode::Literal { value: json!(0.5) },
                origin: TransformBinding::default(),
            },
            count: SlotCount::Fixed(4),
        });
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains("Collection"));
        let back: ChildSlotDef = serde_json::from_str(&json).unwrap();
        assert!(back.multiplicity.is_collection());
    }

    #[test]
    fn validate_rejects_fixed_zero_collection_count() {
        let slot = slot_with(SlotMultiplicity::Collection {
            layout: SlotLayout::Linear {
                axis: AxisRef("u".into()),
                spacing: ExprNode::Literal { value: json!(0.5) },
                origin: TransformBinding::default(),
            },
            count: SlotCount::Fixed(0),
        });
        let err = slot.validate_multiplicity().unwrap_err();
        assert!(matches!(
            err,
            SlotMultiplicityValidationError::FixedZeroCount { .. }
        ));
    }

    #[test]
    fn validate_rejects_collection_with_suppression_expr() {
        let mut slot = slot_with(SlotMultiplicity::Collection {
            layout: SlotLayout::Linear {
                axis: AxisRef("u".into()),
                spacing: ExprNode::Literal { value: json!(0.5) },
                origin: TransformBinding::default(),
            },
            count: SlotCount::Fixed(2),
        });
        slot.suppression_expr = Some(ExprNode::Literal { value: json!(false) });
        let err = slot.validate_multiplicity().unwrap_err();
        assert!(matches!(
            err,
            SlotMultiplicityValidationError::SuppressionOnCollection { .. }
        ));
    }

    #[test]
    fn validate_accepts_single_with_suppression_expr() {
        let mut slot = slot_with(SlotMultiplicity::Single);
        slot.suppression_expr = Some(ExprNode::Literal { value: json!(true) });
        assert!(slot.validate_multiplicity().is_ok());
    }

    #[test]
    fn validate_accepts_derived_count_zero_at_definition_time() {
        // DerivedFromExpr is evaluated at runtime; the validator
        // should not pre-reject expressions that may resolve to 0.
        // (The agreement requires runtime errors via
        // ConstraintSeverity::Error when the expression evaluates
        // to a negative or non-integer value; that's the
        // evaluation-time validator's job in slice 2.)
        let slot = slot_with(SlotMultiplicity::Collection {
            layout: SlotLayout::Linear {
                axis: AxisRef("u".into()),
                spacing: ExprNode::Literal { value: json!(0.5) },
                origin: TransformBinding::default(),
            },
            count: SlotCount::DerivedFromExpr(ExprNode::Literal { value: json!(0.0) }),
        });
        assert!(slot.validate_multiplicity().is_ok());
    }

    #[test]
    fn slot_layout_grid_round_trips() {
        let layout = SlotLayout::Grid {
            axis_u: AxisRef("u".into()),
            count_u: ExprNode::Literal { value: json!(3) },
            spacing_u: ExprNode::Literal { value: json!(0.5) },
            axis_v: AxisRef("v".into()),
            count_v: ExprNode::Literal { value: json!(2) },
            spacing_v: ExprNode::Literal { value: json!(0.4) },
            origin: TransformBinding::default(),
        };
        let json_str = serde_json::to_string(&layout).unwrap();
        let back: SlotLayout = serde_json::from_str(&json_str).unwrap();
        match back {
            SlotLayout::Grid {
                count_u, count_v, ..
            } => match (count_u, count_v) {
                (ExprNode::Literal { value: u }, ExprNode::Literal { value: v }) => {
                    assert_eq!(u, json!(3));
                    assert_eq!(v, json!(2));
                }
                other => panic!("count_u/count_v should round-trip as Literal: {other:?}"),
            },
            _ => panic!("expected Grid layout after round-trip"),
        }
    }

    #[test]
    fn slot_layout_by_spacing_from_host_round_trips() {
        let layout = SlotLayout::BySpacingFromHost {
            host_param: ParameterRef {
                side: BindingSide::Host,
                name: "truss_spacing_mm".into(),
            },
            axis: AxisRef("x".into()),
        };
        let json = serde_json::to_string(&layout).unwrap();
        let back: SlotLayout = serde_json::from_str(&json).unwrap();
        match back {
            SlotLayout::BySpacingFromHost { host_param, axis } => {
                assert_eq!(host_param.side, BindingSide::Host);
                assert_eq!(host_param.name, "truss_spacing_mm");
                assert_eq!(axis.0, "x");
            }
            _ => panic!("expected BySpacingFromHost after round-trip"),
        }
    }
}
