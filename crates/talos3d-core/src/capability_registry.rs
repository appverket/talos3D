use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    sync::Arc,
};

use bevy::{app::App, ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::authored_entity::BoxedEntity;
use crate::plugins::document_properties::DocumentProperties;
use crate::plugins::hosting_contracts::{
    HostingContractDescriptor, HostingContractInfo, HostingContractKindId,
};
use crate::plugins::identity::ElementId;
use crate::plugins::modeling::dependency_graph::EntityDependencies;
use crate::plugins::modeling::primitives::TriangleMesh;
use crate::plugins::refinement::{ClaimPath, ObligationId, RefinementState, SemanticRole};

pub const CAPABILITY_API_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy)]
pub struct HitCandidate {
    pub entity: Entity,
    pub distance: f32,
}

/// Stable generated face references exposed above raw topology where possible.
///
/// Raw `FaceId` can still exist as an internal topology artifact, but pointer
/// interaction and authored-edit routing should prefer these semantic refs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum GeneratedFaceRef {
    BoxFace { axis: u8, positive: bool },
    CylinderTop,
    CylinderBottom,
    CylinderSide,
    PlaneFace,
    ProfileTop,
    ProfileBottom,
    ProfileSideSegment(u32),
    ProfileSideArcSegment(u32),
    ProfileSideClosingSegment,
    FeatureCap,
    FeatureAnchor,
    FeatureSideSegment(u32),
    FeatureSideArcSegment(u32),
    FeatureSideClosingSegment,
}

impl GeneratedFaceRef {
    pub fn label(&self) -> String {
        match self {
            Self::BoxFace { axis, positive } => {
                let axis_label = match axis {
                    0 => "x",
                    1 => "y",
                    2 => "z",
                    _ => "axis",
                };
                let sign = if *positive { "+" } else { "-" };
                format!("{sign}{axis_label}")
            }
            Self::CylinderTop => "top".to_string(),
            Self::CylinderBottom => "bottom".to_string(),
            Self::CylinderSide => "side".to_string(),
            Self::PlaneFace => "surface".to_string(),
            Self::ProfileTop => "top".to_string(),
            Self::ProfileBottom => "bottom".to_string(),
            Self::ProfileSideSegment(index) => format!("side:{index}"),
            Self::ProfileSideArcSegment(index) => format!("side:arc:{index}"),
            Self::ProfileSideClosingSegment => "side:closing".to_string(),
            Self::FeatureCap => "cap".to_string(),
            Self::FeatureAnchor => "anchor".to_string(),
            Self::FeatureSideSegment(index) => format!("side:{index}"),
            Self::FeatureSideArcSegment(index) => format!("side:arc:{index}"),
            Self::FeatureSideClosingSegment => "side:closing".to_string(),
        }
    }
}

/// Identifies a specific face on an authored entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FaceId(pub u32);

impl FaceId {
    /// For box faces: index 0-5 maps to -X, +X, -Y, +Y, -Z, +Z.
    /// Returns the axis index (0=X, 1=Y, 2=Z) and sign (-1 or +1).
    pub fn box_axis_sign(&self) -> (usize, f32) {
        let axis = (self.0 / 2) as usize;
        let sign = if self.0.is_multiple_of(2) { -1.0 } else { 1.0 };
        (axis, sign)
    }
}

/// Result of a face-level hit test.
#[derive(Debug, Clone)]
pub struct FaceHitCandidate {
    pub entity: Entity,
    pub element_id: ElementId,
    pub distance: f32,
    pub face_id: FaceId,
    pub generated_face_ref: Option<GeneratedFaceRef>,
    pub normal: Vec3,
    pub centroid: Vec3,
}

#[derive(Debug, Clone)]
pub struct SnapPoint {
    pub position: Vec3,
    pub kind: crate::plugins::snap::SnapKind,
}

#[derive(Debug, Clone)]
pub struct ModelSummaryAccumulator {
    pub entity_counts: HashMap<String, usize>,
    pub assembly_counts: HashMap<String, usize>,
    pub relation_counts: HashMap<String, usize>,
    pub bounding_points: Vec<Vec3>,
    /// Domain-specific metrics contributed by capabilities.
    /// Keys are capability-defined (e.g. "total_wall_length", "wall_openings").
    pub metrics: HashMap<String, serde_json::Value>,
}

/// Describes an assembly type contributed by a capability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AssemblyTypeDescriptor {
    pub assembly_type: String,
    pub label: String,
    pub description: String,
    /// What entity or assembly types are expected as members.
    pub expected_member_types: Vec<String>,
    /// What roles are valid for members.
    pub expected_member_roles: Vec<String>,
    /// What relationship types are expected between members.
    pub expected_relation_types: Vec<String>,
    /// JSON Schema for assembly-level parameters.
    pub parameter_schema: serde_json::Value,
}

/// Describes a relationship type contributed by a capability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RelationTypeDescriptor {
    pub relation_type: String,
    pub label: String,
    pub description: String,
    /// Which entity or assembly types can be source. Empty = any.
    pub valid_source_types: Vec<String>,
    /// Which entity or assembly types can be target. Empty = any.
    pub valid_target_types: Vec<String>,
    /// JSON Schema for the parameters field.
    pub parameter_schema: serde_json::Value,
    /// Whether this relation participates in dependency/dirty propagation (ADR-007).
    /// Most semantic relations (adjacent_to, bounds) are query/validation-only.
    /// Some (hosted_on) may drive re-evaluation when the target changes.
    pub participates_in_dependency_graph: bool,
}

/// One ordered member/layer within a reusable assembly pattern.
///
/// The ids and roles are capability-defined, but the structure itself is
/// generic enough to be reused by any domain that needs ordered assemblies
/// with support/attachment semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AssemblyPatternLayerDescriptor {
    pub layer_id: String,
    pub label: String,
    pub role: String,
    /// Optional hint such as a material family, product family, or
    /// construction-system placeholder understood by the capability.
    pub material_hint: Option<String>,
    pub optional: bool,
}

/// One required relationship rule inside an assembly pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AssemblyPatternRelationRule {
    pub relation_type: String,
    pub source_layer_id: String,
    pub target_layer_id: String,
    pub required: bool,
    pub rationale: String,
}

/// Describes a reusable, capability-contributed assembly pattern.
///
/// This is intentionally more specific than `AssemblyTypeDescriptor`: it is not
/// just a type of assembly, but a concrete ordered pattern of members/layers
/// and the relations that should hold between them. It is designed to be a
/// consultable target for both shipped knowledge and future dynamic learning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AssemblyPatternDescriptor {
    pub pattern_id: String,
    pub label: String,
    pub description: String,
    /// Capability-defined class or assembly ids the pattern applies to.
    pub target_types: Vec<String>,
    /// Free-form orientation hint such as `exterior_to_interior` or
    /// `top_to_bottom`.
    pub axis: String,
    /// Layers or ordered members from one side of the assembly to the other.
    pub layers: Vec<AssemblyPatternLayerDescriptor>,
    /// Support/attachment/continuity-style relations expected within the pattern.
    pub relation_rules: Vec<AssemblyPatternRelationRule>,
    /// Which layers are considered support roots within the local pattern.
    pub root_layer_ids: Vec<String>,
    /// Whether all materialized members are expected to resolve to a broader
    /// stable support path outside the local pattern.
    pub requires_support_path: bool,
    /// Capability-defined tags such as climate/jurisdiction/system family hints.
    pub tags: Vec<String>,
    /// Pattern parameters that recipes or learning flows may expose.
    pub parameter_schema: serde_json::Value,
}

// ---------------------------------------------------------------------------
// PP71: ElementClassDescriptor and RecipeFamilyDescriptor
// ---------------------------------------------------------------------------

/// Newtype identifier for an element class (e.g. `"wall_assembly"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ElementClassId(pub String);

/// Newtype identifier for a recipe family (e.g. `"light_frame_exterior_wall"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RecipeFamilyId(pub String);

/// An obligation template — same shape as `Obligation` but without a live
/// status. When materialized on a concrete entity at promote-time the status
/// defaults to `Unresolved`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ObligationTemplate {
    pub id: ObligationId,
    pub role: SemanticRole,
    pub required_by_state: RefinementState,
}

/// Describes an element class contributed by a domain capability.
///
/// An element class names a category of designed element (e.g. `wall_assembly`)
/// and declares the semantic contract — roles, class-minimum obligations per
/// refinement state, and class-minimum promotion-critical claim paths — that any
/// recipe targeting the class must honour. Concrete content (recipes, catalog
/// rows, rule packs) registers separately; the descriptor is only the schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementClassDescriptor {
    /// Stable machine-readable identifier.
    pub id: ElementClassId,
    /// Human-readable name.
    pub label: String,
    /// Short description shown in MCP discovery tools.
    pub description: String,
    /// Semantic roles that entities of this class may play in assemblies.
    pub semantic_roles: Vec<SemanticRole>,
    /// Minimum set of obligations at each refinement state.
    /// Recipe specializations may *add* obligations; they may not *remove*
    /// class-minimum ones (except via `Waived` on a concrete instance).
    pub class_min_obligations: HashMap<RefinementState, Vec<ObligationTemplate>>,
    /// Claim paths that are promotion-critical at each refinement state.
    /// Consulted by `get_claim_grounding` to populate `is_promotion_critical`.
    pub class_min_promotion_critical_paths: HashMap<RefinementState, Vec<ClaimPath>>,
    /// JSON Schema describing the parameters understood by all recipes
    /// targeting this class.  May be extended by individual recipes.
    pub parameter_schema: serde_json::Value,
}

/// A parameter declared by a `RecipeFamilyDescriptor`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RecipeParameter {
    pub name: String,
    /// JSON Schema for the value (e.g. `{"type":"number","minimum":0}`).
    pub value_schema: serde_json::Value,
    /// Default value applied when the caller omits this parameter.
    pub default: Option<serde_json::Value>,
}

/// Input supplied to a recipe's `generate` function.
#[derive(Debug, Clone)]
pub struct GenerateInput {
    /// The entity being promoted (identified by element-id).
    pub element_id: u64,
    /// The refinement state the recipe is generating for.
    pub target_state: RefinementState,
    /// Merged parameter values (caller overrides + recipe defaults).
    pub parameters: HashMap<String, serde_json::Value>,
}

/// Output produced by a recipe's `generate` function.
#[derive(Debug, Default)]
pub struct GenerateOutput {
    /// Obligation satisfaction links: `(obligation_id, child_element_id)`.
    /// The promote machinery installs these as `SatisfiedBy(child_id)` on the
    /// parent entity's `ObligationSet`.
    pub satisfaction_links: Vec<(ObligationId, u64)>,
    /// Additional `ClaimGrounding` entries to install on the parent entity.
    pub grounding_updates: HashMap<ClaimPath, crate::plugins::refinement::ClaimRecord>,
}

/// A boxed recipe generation function.
///
/// Receives the `GenerateInput` and a `&mut World` so it can spawn child
/// entities and create refinement-linkage relations. Returns a `GenerateOutput`
/// mapping obligation ids to the child entity-ids that satisfy them.
pub type GenerateFn =
    Arc<dyn Fn(GenerateInput, &mut World) -> Result<GenerateOutput, String> + Send + Sync>;

// ---------------------------------------------------------------------------
// PP74: Constraint layer — ConstraintDescriptor, ValidatorFn, Finding, Findings
// ---------------------------------------------------------------------------

/// Opaque stable identifier for a registered constraint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ConstraintId(pub String);

/// Opaque stable identifier for a single emitted finding.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct FindingId(pub String);

/// Severity of a validator finding.
///
/// Re-exported alongside the legacy `FindingSeverity` alias in
/// `talos3d_core::plugins::refinement` for backward compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum Severity {
    /// Informational — consider addressing.
    Advice,
    /// Significant concern — should be addressed before advancing.
    Warning,
    /// Blocker — must be addressed.
    Error,
}

impl Severity {
    /// Human-readable lowercase label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advice => "advice",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// References a passage in a corpus document (e.g. `"BBR/9:2/table-1"`).
///
/// Re-exported from `refinement.rs`; this alias keeps imports tidy for PP74
/// code that only needs the constraint layer.
pub use crate::plugins::refinement::PassageRef;

/// A single finding produced by a validator function.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct Finding {
    /// Unique stable identifier for this finding instance.
    pub id: FindingId,
    /// The constraint that produced this finding.
    pub constraint_id: ConstraintId,
    /// The element-id of the entity this finding applies to (stored as `u64`).
    pub subject: u64,
    /// Severity of this finding (may differ from the descriptor's
    /// `default_severity` if the validator overrides it).
    pub severity: Severity,
    /// Human-readable message.
    pub message: String,
    /// Rationale explaining why this rule exists.
    pub rationale: String,
    /// Back-reference to a corpus passage (PP77 populates this; `None` for now).
    pub backlink: Option<PassageRef>,
    /// Unix timestamp (seconds) when this finding was emitted.
    pub emitted_at: i64,
    /// Role of the emitting constraint, denormalized onto the finding for
    /// O(1) filtering by role (ADR-042 §13). Stamped by
    /// `validation_sweep_system`. Old serialized payloads default to
    /// `Validation`.
    #[serde(default)]
    pub role: ConstraintRole,
}

/// Specifies which entities a validator applies to.
///
/// Both filters must match. An empty `element_classes` list matches all
/// classes. A `None` `required_state` matches any refinement state.
#[derive(Debug, Clone, Default)]
pub struct Applicability {
    /// Entity must have one of these element classes (empty = any class).
    pub element_classes: Vec<ElementClassId>,
    /// Entity must be at this state or higher (`None` = any state).
    pub required_state: Option<crate::plugins::refinement::RefinementState>,
}

impl Applicability {
    /// Matches any entity.
    pub fn any() -> Self {
        Self {
            element_classes: Vec::new(),
            required_state: None,
        }
    }
}

/// A validator function.
///
/// Receives the Bevy `Entity` of the subject and a shared `&World` reference.
/// The validator may walk `SemanticRelation`s, `ObligationSet`, or any other
/// component by querying the world directly — there is no separate accessor
/// struct (F4 from MCP validation notes).
pub type ValidatorFn = Arc<dyn Fn(Entity, &World) -> Vec<Finding> + Send + Sync>;

/// What role a constraint plays in the refinement lifecycle (ADR-042 §13).
///
/// Discovery, Validation, and Promotion are three orthogonal jobs. Generation
/// priors are tracked separately (see `RecipeArtifact` etc.).
///
/// - `Discovery` constraints ask clarification questions or surface design
///   judgment. Their findings are user-facing prompts; the validation sweep
///   suppresses emissions beyond the per-session budget kept in
///   [`DiscoveryFindingsBudget`].
/// - `Validation` constraints report issues in the current model. Severity is
///   the existing Advice/Warning/Error ladder. Default for backward
///   compatibility — every legacy constraint is `Validation`.
/// - `Promotion` constraints gate movement to a higher refinement state.
///   At promotion time the requester walks `findings_for_role(Promotion)`
///   for the entity; any finding with `severity >= Warning` blocks the
///   advancement. Helper:
///   [`crate::plugins::validation::entity_has_unresolved_promotion_findings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum ConstraintRole {
    Discovery,
    Validation,
    Promotion,
}

impl Default for ConstraintRole {
    fn default() -> Self {
        Self::Validation
    }
}

impl ConstraintRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discovery => "Discovery",
            Self::Validation => "Validation",
            Self::Promotion => "Promotion",
        }
    }
}

/// A registered constraint: a validator with its metadata.
///
/// Registered via `CapabilityRegistryAppExt::register_constraint` or
/// `CapabilityRegistry::register_constraint`. The orchestration engine
/// (`validation_sweep_system`) iterates registered constraints on each
/// `PostUpdate` pass.
pub struct ConstraintDescriptor {
    /// Stable machine-readable identifier.
    pub id: ConstraintId,
    /// Short human-readable name.
    pub label: String,
    /// Full description shown in `list_constraints`.
    pub description: String,
    /// Which entities this constraint applies to.
    pub applicability: Applicability,
    /// Default severity for findings emitted by this constraint. Validators
    /// may override the severity per-finding (F9 from MCP validation notes).
    pub default_severity: Severity,
    /// Rationale explaining why this constraint exists.
    pub rationale: String,
    /// Back-reference to the source corpus passage (`None` in PP74; PP77 fills in).
    pub source_backlink: Option<PassageRef>,
    /// Role of this constraint in the refinement lifecycle (ADR-042 §13).
    /// Defaults to `Validation` for backward compatibility.
    pub role: ConstraintRole,
    /// The validator function.
    pub validator: ValidatorFn,
}

impl std::fmt::Debug for ConstraintDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConstraintDescriptor")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("default_severity", &self.default_severity)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// PP75: Catalog provider layer
// ---------------------------------------------------------------------------

/// Stable identifier for a registered catalog provider.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CatalogProviderId(pub String);

/// Broad category of material or product in a catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum CatalogCategory {
    DimensionalLumber,
    StructuralSheetGoods,
    Other(String),
}

impl CatalogCategory {
    /// Human-readable lowercase string for serialising to MCP responses.
    pub fn as_str(&self) -> &str {
        match self {
            Self::DimensionalLumber => "dimensional_lumber",
            Self::StructuralSheetGoods => "structural_sheet_goods",
            Self::Other(s) => s.as_str(),
        }
    }
}

/// License or provenance tag attached to a catalog row or provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum LicenseTag {
    /// Creative Commons Zero (public domain dedication).
    Cc0,
    /// Boverket public-sector data (Swedish Building Regulations corpus).
    BoverketPublic,
    /// ICC document: citation only — must not reproduce text.
    IccCiteOnly,
    /// Vendor-specific EULA; the enclosed string names the vendor.
    VendorEula(String),
    /// Officially published public record.
    PublicRecord,
    /// Standards body (e.g. ISO, EN): citation permissible, reproduction requires license.
    StandardsBodyCitationOnly,
}

impl LicenseTag {
    /// Short machine-readable label for MCP responses.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Cc0 => "cc0",
            Self::BoverketPublic => "boverket_public",
            Self::IccCiteOnly => "icc_cite_only",
            Self::VendorEula(_) => "vendor_eula",
            Self::PublicRecord => "public_record",
            Self::StandardsBodyCitationOnly => "standards_body_citation_only",
        }
    }
}

/// Provenance metadata shared by a batch of rows ingested from one source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusProvenance {
    /// Human-readable source description (e.g. `"generic public knowledge"`).
    pub source: String,
    /// Version label for the source corpus (e.g. `"2026-Q1"`).
    pub source_version: String,
    /// ISO 3166-1 alpha-2 country or regional code (e.g. `"SE_EU"`), if applicable.
    pub jurisdiction: Option<String>,
    /// Unix timestamp (seconds) when this batch was ingested; `0` for static/built-in data.
    pub ingested_at: i64,
    /// License governing use of this data.
    pub license: LicenseTag,
    /// Back-reference to the specific corpus passage this row was derived from.
    pub backlink: Option<PassageRef>,
    /// Row IDs (in string form) this entry supersedes; empty for new rows.
    pub supersedes: Vec<String>,
}

/// A single row returned by a catalog provider query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogRow {
    /// Stable identifier for this row within the provider's namespace.
    pub row_id: crate::plugins::refinement::CatalogRowId,
    /// Category of this row.
    pub category: CatalogCategory,
    /// Arbitrary JSON payload: dimensions, grades, cost, etc.
    pub data: serde_json::Value,
    /// Provenance for this row.
    pub provenance: CorpusProvenance,
}

/// A boxed catalog query function.
///
/// Receives a raw JSON filter (PP75: no-op filter accepted) and returns all
/// matching rows. Real filtering and schema validation are follow-on work.
pub type CatalogQueryFn = Arc<dyn Fn(&serde_json::Value) -> Vec<CatalogRow> + Send + Sync>;

/// A registered catalog provider: metadata + the live query function.
///
/// Not `Serialize`/`Deserialize` — holds a function pointer (`CatalogQueryFn`).
/// Use `CatalogProviderInfo` (in `model_api`) for serialisable summaries.
pub struct CatalogProviderDescriptor {
    /// Stable machine-readable identifier.
    pub id: CatalogProviderId,
    /// Human-readable name.
    pub label: String,
    /// Short description shown in MCP discovery tools.
    pub description: String,
    /// Category of products this provider covers.
    pub category: CatalogCategory,
    /// Regional or jurisdictional scope (e.g. `"SE_EU"`), if applicable.
    pub region: Option<String>,
    /// License governing use of data from this provider.
    pub license: LicenseTag,
    /// Version label for the underlying source corpus.
    pub source_version: String,
    /// The query function: takes a JSON filter, returns matching rows.
    pub query_fn: CatalogQueryFn,
}

impl std::fmt::Debug for CatalogProviderDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogProviderDescriptor")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("category", &self.category)
            .finish_non_exhaustive()
    }
}

impl Clone for CatalogProviderDescriptor {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            label: self.label.clone(),
            description: self.description.clone(),
            category: self.category.clone(),
            region: self.region.clone(),
            license: self.license.clone(),
            source_version: self.source_version.clone(),
            query_fn: Arc::clone(&self.query_fn),
        }
    }
}

// ---------------------------------------------------------------------------
// PP76: Generation priors
// ---------------------------------------------------------------------------

/// Stable identifier for a registered generation prior.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PriorId(pub String);

/// Scope of a generation prior: what it ranks or defaults.
///
/// `RecipeSelection` priors contribute a weight for a specific recipe family
/// when `select_recipe` is called for the given element class.
/// `ParameterDefaulting` priors suggest a value for a specific claim path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PriorScope {
    /// Ranks a specific recipe family for the given element class.
    /// `recipe_family: None` means the prior targets *all* families for the class;
    /// supply `Some(id)` to narrow to one family.
    RecipeSelection {
        element_class: ElementClassId,
        recipe_family: Option<RecipeFamilyId>,
    },
    /// Suggests a default value for a typed claim path on the given element class.
    ParameterDefaulting {
        element_class: ElementClassId,
        claim_path: ClaimPath,
    },
}

/// Runtime context passed to a prior's evaluation function.
///
/// Derived from the caller's JSON context object (e.g. `select_recipe`'s
/// `context` argument). `extras` carries any additional fields not explicitly
/// modelled here, preserving forward-compatibility.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PriorContext {
    /// ISO 3166-1 jurisdiction code, e.g. `"SE"` or `"US"`.
    pub jurisdiction: Option<String>,
    /// Design-intent style token, e.g. `"modern_pavilion"`.
    pub style_intent: Option<String>,
    /// Site terrain slope as a percentage (rise/run × 100).
    pub terrain_slope_pct: Option<f64>,
    /// Catch-all for caller-supplied keys not yet modelled explicitly.
    #[serde(default)]
    pub extras: serde_json::Value,
}

impl PriorContext {
    /// Construct a `PriorContext` from a raw JSON object produced by a caller.
    ///
    /// Known fields are extracted; any remaining keys flow into `extras`.
    pub fn from_json(value: &serde_json::Value) -> Self {
        Self {
            jurisdiction: value
                .get("jurisdiction")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            style_intent: value
                .get("style_intent")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            terrain_slope_pct: value.get("terrain_slope_pct").and_then(|v| v.as_f64()),
            extras: value.clone(),
        }
    }
}

/// The outcome of evaluating a prior against a context.
///
/// `weight` is in `[0.0, 1.0]` where 1.0 = strong preference, 0.0 = veto.
/// `suggestion` carries an optional parameter value for `ParameterDefaulting`
/// priors. `rationale` is a short human-readable explanation for MCP callers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorEvaluation {
    /// Preference weight in \[0.0, 1.0\].
    pub weight: f32,
    /// Optional parameter suggestion (for `ParameterDefaulting` priors).
    pub suggestion: Option<serde_json::Value>,
    /// Human-readable explanation of why this weight was assigned.
    pub rationale: String,
}

impl PriorEvaluation {
    /// Neutral evaluation: weight 1.0, no suggestion.
    pub fn neutral(rationale: impl Into<String>) -> Self {
        Self {
            weight: 1.0,
            suggestion: None,
            rationale: rationale.into(),
        }
    }
}

/// A prior function: takes the evaluation context, returns a `PriorEvaluation`.
///
/// For `RecipeSelection` priors the function is called once per candidate
/// recipe family; for `ParameterDefaulting` priors it is called once per
/// entity whose parameter is being defaulted.
///
/// The `Arc` wrapper makes the descriptor cheaply `Clone`-able without
/// duplicating the closure allocation.
pub type PriorFn = Arc<dyn Fn(&PriorContext) -> PriorEvaluation + Send + Sync>;

/// Describes a registered generation prior.
///
/// A prior scores recipe families during `select_recipe` or supplies default
/// parameter values during recipe instantiation. Priors are separate from
/// validators: they run *before* entity creation to guide the authoring agent,
/// not *after* to check compliance.
///
/// Not `Serialize`/`Deserialize` — holds a `PriorFn` closure.
/// Use `GenerationPriorInfo` (in `model_api`) for serialisable summaries.
pub struct GenerationPriorDescriptor {
    /// Stable machine-readable identifier.
    pub id: PriorId,
    /// Short human-readable name.
    pub label: String,
    /// Full description shown in `list_generation_priors`.
    pub description: String,
    /// What this prior applies to (recipe selection or parameter defaulting).
    pub scope: PriorScope,
    /// Provenance of the knowledge encoded in this prior.
    pub source_provenance: CorpusProvenance,
    /// The evaluation function.
    pub prior_fn: PriorFn,
}

impl std::fmt::Debug for GenerationPriorDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenerationPriorDescriptor")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl Clone for GenerationPriorDescriptor {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            label: self.label.clone(),
            description: self.description.clone(),
            scope: self.scope.clone(),
            source_provenance: self.source_provenance.clone(),
            prior_fn: Arc::clone(&self.prior_fn),
        }
    }
}

/// Describes a recipe family: a parametric authoring contract for an element
/// class. A recipe family declares parameters, which refinement levels it
/// supports, how it specialises the class-minimum obligations, and a `generate`
/// function that materialises child entities on promotion.
pub struct RecipeFamilyDescriptor {
    /// Stable machine-readable identifier.
    pub id: RecipeFamilyId,
    /// The element class this recipe targets.
    pub target_class: ElementClassId,
    /// Human-readable name.
    pub label: String,
    /// Short description shown in MCP discovery tools.
    pub description: String,
    /// Parameters accepted by this recipe.
    pub parameters: Vec<RecipeParameter>,
    /// Which refinement states this recipe can generate for.
    pub supported_refinement_levels: Vec<RefinementState>,
    /// Additional obligation templates beyond the class minimum, keyed by state.
    pub obligation_specializations: HashMap<RefinementState, Vec<ObligationTemplate>>,
    /// Additional promotion-critical paths beyond the class minimum, keyed by state.
    pub promotion_critical_path_specializations: HashMap<RefinementState, Vec<ClaimPath>>,
    /// The generation function invoked at promote-time.
    pub generate: GenerateFn,
}

impl std::fmt::Debug for RecipeFamilyDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecipeFamilyDescriptor")
            .field("id", &self.id)
            .field("target_class", &self.target_class)
            .field("label", &self.label)
            .field(
                "supported_refinement_levels",
                &self.supported_refinement_levels,
            )
            .finish_non_exhaustive()
    }
}

/// Compute the effective merged obligation list for a given target state,
/// combining class-minimum obligations with recipe specialisations.
///
/// The class-minimum obligations are always included first; recipe
/// specialisations are appended. No de-duplication by id — callers must ensure
/// ids are unique across the two sets.
pub fn effective_obligations(
    class: &ElementClassDescriptor,
    recipe: Option<&RecipeFamilyDescriptor>,
    target_state: RefinementState,
) -> Vec<ObligationTemplate> {
    let mut out: Vec<ObligationTemplate> = class
        .class_min_obligations
        .get(&target_state)
        .cloned()
        .unwrap_or_default();
    if let Some(recipe) = recipe {
        if let Some(specializations) = recipe.obligation_specializations.get(&target_state) {
            out.extend_from_slice(specializations);
        }
    }
    out
}

/// Compute the effective merged promotion-critical paths for a given target
/// state, combining class-minimum paths with recipe specialisations.
pub fn effective_promotion_critical_paths(
    class: &ElementClassDescriptor,
    recipe: Option<&RecipeFamilyDescriptor>,
    target_state: RefinementState,
) -> Vec<ClaimPath> {
    let mut out: Vec<ClaimPath> = class
        .class_min_promotion_critical_paths
        .get(&target_state)
        .cloned()
        .unwrap_or_default();
    if let Some(recipe) = recipe {
        if let Some(specializations) = recipe
            .promotion_critical_path_specializations
            .get(&target_state)
        {
            out.extend_from_slice(specializations);
        }
    }
    out
}

/// ECS component that tags an entity with an element class and (optionally) the
/// active recipe family that generated it.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementClassAssignment {
    pub element_class: ElementClassId,
    pub active_recipe: Option<RecipeFamilyId>,
}

#[allow(clippy::wrong_self_convention)]
pub trait AuthoredEntityFactory: Send + Sync + 'static {
    fn type_name(&self) -> &'static str;

    fn capture_snapshot(&self, entity_ref: &EntityRef, world: &World) -> Option<BoxedEntity>;

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String>;

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String>;

    fn draw_selection(&self, _world: &World, _entity: Entity, _gizmos: &mut Gizmos, _color: Color) {
    }

    fn selection_line_count(&self, _world: &World, _entity: Entity) -> usize {
        0
    }

    fn hit_test(&self, _world: &World, _ray: Ray3d) -> Option<HitCandidate> {
        None
    }

    /// Test a ray against individual faces of entities of this type.
    /// Only called while in face-editing context for the given entity.
    fn hit_test_face(
        &self,
        _world: &World,
        _entity: Entity,
        _ray: Ray3d,
    ) -> Option<FaceHitCandidate> {
        None
    }

    fn collect_snap_points(&self, _world: &World, _out: &mut Vec<SnapPoint>) {}

    fn collect_inference_geometry(
        &self,
        _world: &World,
        _engine: &mut crate::plugins::inference::InferenceEngine,
    ) {
    }

    fn contribute_model_summary(&self, _world: &World, _summary: &mut ModelSummaryAccumulator) {}

    fn collect_delete_dependencies(
        &self,
        _world: &World,
        _requested_ids: &[ElementId],
        _out: &mut Vec<ElementId>,
    ) {
    }

    /// Declare outbound dependency edges for an entity owned by this
    /// factory. Called by [`sync_factory_declared_dependencies_system`]
    /// whenever the entity's [`DependencyEdgesStale`] marker is set.
    ///
    /// **Default**: no edges — entity is a leaf in the graph.
    ///
    /// Implementations must be deterministic and read-only against
    /// `world`. Read entity components via `world.get::<C>(entity)`.
    ///
    /// [`sync_factory_declared_dependencies_system`]:
    ///   crate::plugins::modeling::dependency_graph::sync_factory_declared_dependencies_system
    /// [`DependencyEdgesStale`]:
    ///   crate::plugins::modeling::dependency_graph::DependencyEdgesStale
    fn dependency_edges(&self, _world: &World, _entity: Entity) -> EntityDependencies {
        EntityDependencies::empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum CapabilityMaturity {
    Experimental,
    Preview,
    #[default]
    Stable,
    Deprecated,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum CapabilityDistribution {
    #[default]
    Bundled,
    ReferenceExtension,
    Community,
    Private,
    Commercial,
}

fn default_capability_api_version() -> u32 {
    CAPABILITY_API_VERSION
}

fn is_default_maturity(value: &CapabilityMaturity) -> bool {
    matches!(value, CapabilityMaturity::Stable)
}

fn is_default_distribution(value: &CapabilityDistribution) -> bool {
    matches!(value, CapabilityDistribution::Bundled)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    pub id: String,
    pub name: String,
    pub version: u32,
    #[serde(default = "default_capability_api_version")]
    pub api_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional_dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
    #[serde(default, skip_serializing_if = "is_default_maturity")]
    pub maturity: CapabilityMaturity,
    #[serde(default, skip_serializing_if = "is_default_distribution")]
    pub distribution: CapabilityDistribution,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

impl CapabilityDescriptor {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: 1,
            api_version: CAPABILITY_API_VERSION,
            description: None,
            dependencies: Vec::new(),
            optional_dependencies: Vec::new(),
            conflicts: Vec::new(),
            maturity: CapabilityMaturity::Stable,
            distribution: CapabilityDistribution::Bundled,
            license: None,
            repository: None,
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_dependencies<I, S>(mut self, dependencies: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.dependencies = dependencies.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_optional_dependencies<I, S>(mut self, dependencies: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.optional_dependencies = dependencies.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_conflicts<I, S>(mut self, conflicts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.conflicts = conflicts.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_distribution(mut self, distribution: CapabilityDistribution) -> Self {
        self.distribution = distribution;
        self
    }

    pub fn with_maturity(mut self, maturity: CapabilityMaturity) -> Self {
        self.maturity = maturity;
        self
    }

    pub fn with_license(mut self, license: impl Into<String>) -> Self {
        self.license = Some(license.into());
        self
    }

    pub fn with_repository(mut self, repository: impl Into<String>) -> Self {
        self.repository = Some(repository.into());
        self
    }
}

/// Metadata for a workbench: a curated user-facing workflow built from capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbenchDescriptor {
    pub id: String,
    pub name: String,
    #[serde(default = "default_workbench_version")]
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub capability_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional_capability_ids: Vec<String>,
}

fn default_workbench_version() -> u32 {
    1
}

impl WorkbenchDescriptor {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: 1,
            description: None,
            capability_ids: Vec::new(),
            optional_capability_ids: Vec::new(),
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_capabilities<I, S>(mut self, capability_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.capability_ids = capability_ids.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_optional_capabilities<I, S>(mut self, capability_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.optional_capability_ids = capability_ids.into_iter().map(Into::into).collect();
        self
    }
}

#[derive(Resource, Default)]
pub struct CapabilityRegistry {
    capabilities: Vec<CapabilityDescriptor>,
    capability_index: HashMap<String, usize>,
    workbenches: Vec<WorkbenchDescriptor>,
    ordered_factories: Vec<Arc<dyn AuthoredEntityFactory>>,
    factories_by_type: HashMap<&'static str, Arc<dyn AuthoredEntityFactory>>,
    assembly_type_descriptors: Vec<AssemblyTypeDescriptor>,
    assembly_pattern_descriptors: Vec<AssemblyPatternDescriptor>,
    assembly_pattern_index: HashMap<String, usize>,
    relation_type_descriptors: Vec<RelationTypeDescriptor>,
    hosting_contract_descriptors: Vec<HostingContractDescriptor>,
    hosting_contract_index: HashMap<String, usize>,
    // PP71
    element_class_descriptors: Vec<ElementClassDescriptor>,
    element_class_index: HashMap<String, usize>,
    recipe_family_descriptors: Vec<RecipeFamilyDescriptor>,
    recipe_family_index: HashMap<String, usize>,
    // PP74
    constraint_descriptors: Vec<ConstraintDescriptor>,
    constraint_index: HashMap<String, usize>,
    // PP75
    catalog_providers: Vec<CatalogProviderDescriptor>,
    catalog_provider_index: HashMap<String, usize>,
    // PP76
    generation_priors: Vec<GenerationPriorDescriptor>,
    generation_prior_index: HashMap<String, usize>,
}

impl CapabilityRegistry {
    pub fn register_capability(&mut self, descriptor: CapabilityDescriptor) {
        assert!(
            !self.capability_index.contains_key(descriptor.id.as_str()),
            "Capability '{}' was registered more than once",
            descriptor.id
        );
        let index = self.capabilities.len();
        self.capability_index.insert(descriptor.id.clone(), index);
        self.capabilities.push(descriptor);
    }

    pub fn capabilities(&self) -> &[CapabilityDescriptor] {
        &self.capabilities
    }

    pub fn capability(&self, id: &str) -> Option<&CapabilityDescriptor> {
        self.capability_index
            .get(id)
            .and_then(|index| self.capabilities.get(*index))
    }

    pub fn export_capabilities(&self) -> Value {
        serde_json::to_value(&self.capabilities).unwrap_or_default()
    }

    pub fn register_workbench(&mut self, descriptor: WorkbenchDescriptor) {
        assert!(
            self.workbenches.iter().all(|wb| wb.id != descriptor.id),
            "Workbench '{}' was registered more than once",
            descriptor.id
        );
        self.workbenches.push(descriptor);
    }

    pub fn workbenches(&self) -> &[WorkbenchDescriptor] {
        &self.workbenches
    }

    pub fn export_workbenches(&self) -> Value {
        serde_json::to_value(&self.workbenches).unwrap_or_default()
    }

    pub fn validate_dependencies(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for cap in &self.capabilities {
            if cap.api_version != CAPABILITY_API_VERSION {
                errors.push(format!(
                    "Capability '{}' targets API version {}, but Talos3D exposes version {}",
                    cap.id, cap.api_version, CAPABILITY_API_VERSION
                ));
            }
            for dep in &cap.dependencies {
                if !self.capability_index.contains_key(dep) {
                    errors.push(format!(
                        "Capability '{}' depends on '{}', which is not registered",
                        cap.id, dep
                    ));
                }
            }
            for conflict in &cap.conflicts {
                if self.capability_index.contains_key(conflict) {
                    errors.push(format!(
                        "Capability '{}' conflicts with '{}', but both are registered",
                        cap.id, conflict
                    ));
                }
            }
        }
        for workbench in &self.workbenches {
            for capability_id in &workbench.capability_ids {
                if !self.capability_index.contains_key(capability_id) {
                    errors.push(format!(
                        "Workbench '{}' references capability '{}', which is not registered",
                        workbench.id, capability_id
                    ));
                }
            }
        }
        errors
    }

    pub fn register_factory<F>(&mut self, factory: F)
    where
        F: AuthoredEntityFactory,
    {
        let factory = Arc::new(factory);
        self.factories_by_type
            .insert(factory.type_name(), factory.clone());
        self.ordered_factories.push(factory);
    }

    pub fn factories(&self) -> &[Arc<dyn AuthoredEntityFactory>] {
        &self.ordered_factories
    }

    pub fn factory_for(&self, type_name: &str) -> Option<Arc<dyn AuthoredEntityFactory>> {
        self.factories_by_type.get(type_name).cloned()
    }

    pub fn capture_snapshot(&self, entity_ref: &EntityRef, world: &World) -> Option<BoxedEntity> {
        self.ordered_factories
            .iter()
            .find_map(|factory| factory.capture_snapshot(entity_ref, world))
    }

    pub fn build_model_summary(&self, world: &World) -> ModelSummaryAccumulator {
        let mut summary = ModelSummaryAccumulator {
            entity_counts: HashMap::new(),
            assembly_counts: HashMap::new(),
            relation_counts: HashMap::new(),
            bounding_points: Vec::new(),
            metrics: HashMap::new(),
        };

        for factory in &self.ordered_factories {
            factory.contribute_model_summary(world, &mut summary);
        }

        summary
    }

    pub fn expand_delete_ids(&self, world: &World, requested_ids: &[ElementId]) -> Vec<ElementId> {
        let mut expanded = requested_ids.to_vec();
        for factory in &self.ordered_factories {
            factory.collect_delete_dependencies(world, requested_ids, &mut expanded);
        }
        expanded.sort_unstable_by_key(|element_id| element_id.0);
        expanded.dedup();
        expanded
    }

    pub fn register_assembly_type(&mut self, descriptor: AssemblyTypeDescriptor) {
        self.assembly_type_descriptors.push(descriptor);
    }

    pub fn register_assembly_pattern(&mut self, descriptor: AssemblyPatternDescriptor) {
        assert!(
            !self
                .assembly_pattern_index
                .contains_key(descriptor.pattern_id.as_str()),
            "AssemblyPattern '{}' was registered more than once",
            descriptor.pattern_id
        );
        let index = self.assembly_pattern_descriptors.len();
        self.assembly_pattern_index
            .insert(descriptor.pattern_id.clone(), index);
        self.assembly_pattern_descriptors.push(descriptor);
    }

    pub fn register_relation_type(&mut self, descriptor: RelationTypeDescriptor) {
        self.relation_type_descriptors.push(descriptor);
    }

    /// Register a hosting contract descriptor. Panics if the kind is already registered.
    pub fn register_hosting_contract(&mut self, descriptor: HostingContractDescriptor) {
        assert!(
            !self
                .hosting_contract_index
                .contains_key(descriptor.kind.0.as_str()),
            "HostingContract '{}' was registered more than once",
            descriptor.kind.0
        );
        let index = self.hosting_contract_descriptors.len();
        self.hosting_contract_index
            .insert(descriptor.kind.0.clone(), index);
        self.hosting_contract_descriptors.push(descriptor);
    }

    pub fn assembly_type_descriptors(&self) -> &[AssemblyTypeDescriptor] {
        &self.assembly_type_descriptors
    }

    pub fn assembly_pattern_descriptors(&self) -> &[AssemblyPatternDescriptor] {
        &self.assembly_pattern_descriptors
    }

    pub fn assembly_pattern_descriptor(
        &self,
        pattern_id: &str,
    ) -> Option<&AssemblyPatternDescriptor> {
        self.assembly_pattern_index
            .get(pattern_id)
            .and_then(|index| self.assembly_pattern_descriptors.get(*index))
    }

    pub fn relation_type_descriptors(&self) -> &[RelationTypeDescriptor] {
        &self.relation_type_descriptors
    }

    /// Return all registered hosting contract descriptor summaries.
    pub fn hosting_contract_descriptors(&self) -> Vec<HostingContractInfo> {
        self.hosting_contract_descriptors
            .iter()
            .map(HostingContractInfo::from)
            .collect()
    }

    /// Look up a hosting contract descriptor by kind.
    pub fn hosting_contract_descriptor(
        &self,
        kind: &HostingContractKindId,
    ) -> Option<&HostingContractDescriptor> {
        self.hosting_contract_index
            .get(kind.0.as_str())
            .and_then(|&i| self.hosting_contract_descriptors.get(i))
    }

    // --- PP71: Element class descriptors ---

    /// Register an element class descriptor. Panics if the id is already registered.
    pub fn register_element_class(&mut self, descriptor: ElementClassDescriptor) {
        assert!(
            !self
                .element_class_index
                .contains_key(descriptor.id.0.as_str()),
            "ElementClass '{}' was registered more than once",
            descriptor.id.0
        );
        let index = self.element_class_descriptors.len();
        self.element_class_index
            .insert(descriptor.id.0.clone(), index);
        self.element_class_descriptors.push(descriptor);
    }

    /// Return all registered element class descriptors.
    pub fn element_class_descriptors(&self) -> &[ElementClassDescriptor] {
        &self.element_class_descriptors
    }

    /// Look up a single element class descriptor by id.
    pub fn element_class_descriptor(&self, id: &ElementClassId) -> Option<&ElementClassDescriptor> {
        self.element_class_index
            .get(id.0.as_str())
            .and_then(|&i| self.element_class_descriptors.get(i))
    }

    // --- PP71: Recipe family descriptors ---

    /// Register a recipe family descriptor. Panics if the id is already registered.
    pub fn register_recipe_family(&mut self, descriptor: RecipeFamilyDescriptor) {
        assert!(
            !self
                .recipe_family_index
                .contains_key(descriptor.id.0.as_str()),
            "RecipeFamily '{}' was registered more than once",
            descriptor.id.0
        );
        let index = self.recipe_family_descriptors.len();
        self.recipe_family_index
            .insert(descriptor.id.0.clone(), index);
        self.recipe_family_descriptors.push(descriptor);
    }

    /// Return recipe family descriptors, optionally filtered to those targeting
    /// a specific element class.
    pub fn recipe_family_descriptors(
        &self,
        element_class: Option<&ElementClassId>,
    ) -> Vec<&RecipeFamilyDescriptor> {
        self.recipe_family_descriptors
            .iter()
            .filter(|d| element_class.is_none_or(|cls| &d.target_class == cls))
            .collect()
    }

    /// Look up a single recipe family descriptor by id.
    pub fn recipe_family_descriptor(&self, id: &RecipeFamilyId) -> Option<&RecipeFamilyDescriptor> {
        self.recipe_family_index
            .get(id.0.as_str())
            .and_then(|&i| self.recipe_family_descriptors.get(i))
    }

    // --- PP74: Constraint descriptors ---

    /// Register a constraint descriptor. Panics if the id is already registered.
    pub fn register_constraint(&mut self, descriptor: ConstraintDescriptor) {
        assert!(
            !self.constraint_index.contains_key(descriptor.id.0.as_str()),
            "Constraint '{}' was registered more than once",
            descriptor.id.0
        );
        let index = self.constraint_descriptors.len();
        self.constraint_index.insert(descriptor.id.0.clone(), index);
        self.constraint_descriptors.push(descriptor);
    }

    /// Return all registered constraint descriptors.
    pub fn constraint_descriptors(&self) -> &[ConstraintDescriptor] {
        &self.constraint_descriptors
    }

    /// Look up a single constraint descriptor by id.
    pub fn constraint_descriptor(&self, id: &ConstraintId) -> Option<&ConstraintDescriptor> {
        self.constraint_index
            .get(id.0.as_str())
            .and_then(|&i| self.constraint_descriptors.get(i))
    }

    // --- PP75: Catalog provider descriptors ---

    /// Register a catalog provider descriptor. Panics if the id is already registered.
    pub fn register_catalog_provider(&mut self, descriptor: CatalogProviderDescriptor) {
        assert!(
            !self
                .catalog_provider_index
                .contains_key(descriptor.id.0.as_str()),
            "CatalogProvider '{}' was registered more than once",
            descriptor.id.0
        );
        let index = self.catalog_providers.len();
        self.catalog_provider_index
            .insert(descriptor.id.0.clone(), index);
        self.catalog_providers.push(descriptor);
    }

    /// Return all registered catalog provider descriptors.
    pub fn catalog_provider_descriptors(&self) -> &[CatalogProviderDescriptor] {
        &self.catalog_providers
    }

    /// Look up a single catalog provider descriptor by id.
    pub fn catalog_provider_descriptor(
        &self,
        id: &CatalogProviderId,
    ) -> Option<&CatalogProviderDescriptor> {
        self.catalog_provider_index
            .get(id.0.as_str())
            .and_then(|&i| self.catalog_providers.get(i))
    }

    // --- PP76: Generation prior descriptors ---

    /// Register a generation prior descriptor. Panics if the id is already registered.
    pub fn register_generation_prior(&mut self, descriptor: GenerationPriorDescriptor) {
        assert!(
            !self
                .generation_prior_index
                .contains_key(descriptor.id.0.as_str()),
            "GenerationPrior '{}' was registered more than once",
            descriptor.id.0
        );
        let index = self.generation_priors.len();
        self.generation_prior_index
            .insert(descriptor.id.0.clone(), index);
        self.generation_priors.push(descriptor);
    }

    /// Return all registered generation prior descriptors, optionally filtered
    /// to those matching the given scope element class.
    ///
    /// Pass `element_class: None` to return all priors.
    pub fn generation_prior_descriptors(
        &self,
        element_class: Option<&ElementClassId>,
    ) -> Vec<&GenerationPriorDescriptor> {
        self.generation_priors
            .iter()
            .filter(|d| {
                let Some(cls) = element_class else {
                    return true;
                };
                match &d.scope {
                    PriorScope::RecipeSelection {
                        element_class: ec, ..
                    } => ec == cls,
                    PriorScope::ParameterDefaulting {
                        element_class: ec, ..
                    } => ec == cls,
                }
            })
            .collect()
    }

    /// Look up a single generation prior descriptor by id.
    pub fn generation_prior_descriptor(&self, id: &PriorId) -> Option<&GenerationPriorDescriptor> {
        self.generation_prior_index
            .get(id.0.as_str())
            .and_then(|&i| self.generation_priors.get(i))
    }
}

fn validate_capability_dependencies(registry: Res<CapabilityRegistry>) {
    let errors = registry.validate_dependencies();
    for error in &errors {
        warn!("{error}");
    }
}

/// Runtime activation state for registered capabilities.
///
/// Capability descriptors ([`CapabilityDescriptor`]) describe *what* a plugin
/// provides; this resource tracks *whether* the user currently wants that
/// functionality surfaced in the UI.
///
/// Menu and toolbar renderers consult [`CapabilityActivation::is_enabled`] to
/// decide whether a command tagged with a `capability_id` should be visible.
/// Capabilities are enabled by default — disabling is opt-in, so newly
/// registered plugins remain discoverable.
///
/// This is intentionally a session-scoped resource (not persisted with the
/// document), mirroring the way toolbar visibility is treated: a workspace
/// preference, not part of the authored model.
#[derive(Resource, Default, Debug, Clone)]
pub struct CapabilityActivation {
    disabled: HashSet<String>,
}

impl CapabilityActivation {
    pub fn is_enabled(&self, capability_id: &str) -> bool {
        !self.disabled.contains(capability_id)
    }

    pub fn is_disabled(&self, capability_id: &str) -> bool {
        self.disabled.contains(capability_id)
    }

    pub fn set_enabled(&mut self, capability_id: &str, enabled: bool) {
        if enabled {
            self.disabled.remove(capability_id);
        } else {
            self.disabled.insert(capability_id.to_string());
        }
    }

    pub fn toggle(&mut self, capability_id: &str) -> bool {
        let now_enabled = self.disabled.remove(capability_id);
        if !now_enabled {
            self.disabled.insert(capability_id.to_string());
        }
        now_enabled
    }

    pub fn disabled_ids(&self) -> impl Iterator<Item = &str> {
        self.disabled.iter().map(String::as_str)
    }
}

pub trait CapabilityRegistryAppExt {
    fn register_capability(&mut self, descriptor: CapabilityDescriptor) -> &mut Self;

    fn register_workbench(&mut self, descriptor: WorkbenchDescriptor) -> &mut Self;

    fn register_authored_entity_factory<F>(&mut self, factory: F) -> &mut Self
    where
        F: AuthoredEntityFactory;

    fn register_assembly_type(&mut self, descriptor: AssemblyTypeDescriptor) -> &mut Self;

    fn register_assembly_pattern(&mut self, descriptor: AssemblyPatternDescriptor) -> &mut Self;

    fn register_relation_type(&mut self, descriptor: RelationTypeDescriptor) -> &mut Self;

    /// Register a `HostingContractDescriptor` (PP-DHOST-1). Initialises the
    /// `CapabilityRegistry` resource if it does not yet exist.
    fn register_hosting_contract(&mut self, descriptor: HostingContractDescriptor) -> &mut Self;

    /// Register an `ElementClassDescriptor` (PP71). Initialises the
    /// `CapabilityRegistry` resource if it does not yet exist.
    fn register_element_class(&mut self, descriptor: ElementClassDescriptor) -> &mut Self;

    /// Register a `RecipeFamilyDescriptor` (PP71). Initialises the
    /// `CapabilityRegistry` resource if it does not yet exist.
    fn register_recipe_family(&mut self, descriptor: RecipeFamilyDescriptor) -> &mut Self;

    /// Register a `ConstraintDescriptor` (PP74). Initialises the
    /// `CapabilityRegistry` resource if it does not yet exist.
    fn register_constraint(&mut self, descriptor: ConstraintDescriptor) -> &mut Self;

    /// Register a `CatalogProviderDescriptor` (PP75). Initialises the
    /// `CapabilityRegistry` resource if it does not yet exist.
    fn register_catalog_provider(&mut self, descriptor: CatalogProviderDescriptor) -> &mut Self;

    /// Register a `GenerationPriorDescriptor` (PP76). Initialises the
    /// `CapabilityRegistry` resource if it does not yet exist.
    fn register_generation_prior(&mut self, descriptor: GenerationPriorDescriptor) -> &mut Self;
}

#[derive(Resource)]
struct CapabilityValidationScheduled;

impl CapabilityRegistryAppExt for App {
    fn register_capability(&mut self, descriptor: CapabilityDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }
        if !self.world().contains_resource::<CapabilityActivation>() {
            self.init_resource::<CapabilityActivation>();
        }
        if !self
            .world()
            .contains_resource::<CapabilityValidationScheduled>()
        {
            self.insert_resource(CapabilityValidationScheduled);
            self.add_systems(Startup, validate_capability_dependencies);
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_capability(descriptor);
        self
    }

    fn register_workbench(&mut self, descriptor: WorkbenchDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_workbench(descriptor);
        self
    }

    fn register_authored_entity_factory<F>(&mut self, factory: F) -> &mut Self
    where
        F: AuthoredEntityFactory,
    {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_factory(factory);
        self
    }

    fn register_assembly_type(&mut self, descriptor: AssemblyTypeDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_assembly_type(descriptor);
        self
    }

    fn register_assembly_pattern(&mut self, descriptor: AssemblyPatternDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_assembly_pattern(descriptor);
        self
    }

    fn register_relation_type(&mut self, descriptor: RelationTypeDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_relation_type(descriptor);
        self
    }

    fn register_hosting_contract(&mut self, descriptor: HostingContractDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_hosting_contract(descriptor);
        self
    }

    fn register_element_class(&mut self, descriptor: ElementClassDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_element_class(descriptor);
        self
    }

    fn register_recipe_family(&mut self, descriptor: RecipeFamilyDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_recipe_family(descriptor);
        self
    }

    fn register_constraint(&mut self, descriptor: ConstraintDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_constraint(descriptor);
        self
    }

    fn register_catalog_provider(&mut self, descriptor: CatalogProviderDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_catalog_provider(descriptor);
        self
    }

    fn register_generation_prior(&mut self, descriptor: GenerationPriorDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_generation_prior(descriptor);
        self
    }
}

pub trait DefaultsContributor: Send + Sync + 'static {
    fn contribute_defaults(&self, props: &mut DocumentProperties);
}

#[derive(Resource, Default)]
pub struct DefaultsRegistry {
    contributors: Vec<Box<dyn DefaultsContributor>>,
}

impl DefaultsRegistry {
    pub fn register<C: DefaultsContributor>(&mut self, contributor: C) {
        self.contributors.push(Box::new(contributor));
    }

    pub fn apply_all(&self, props: &mut DocumentProperties) {
        for contributor in &self.contributors {
            contributor.contribute_defaults(props);
        }
    }
}

pub trait DefaultsRegistryAppExt {
    fn register_defaults_contributor<C: DefaultsContributor>(
        &mut self,
        contributor: C,
    ) -> &mut Self;
}

impl DefaultsRegistryAppExt for App {
    fn register_defaults_contributor<C: DefaultsContributor>(
        &mut self,
        contributor: C,
    ) -> &mut Self {
        if !self.world().contains_resource::<DefaultsRegistry>() {
            self.init_resource::<DefaultsRegistry>();
        }

        self.world_mut()
            .resource_mut::<DefaultsRegistry>()
            .register(contributor);
        self
    }
}

pub trait TerrainProvider: Send + Sync + 'static {
    fn elevation_at(&self, world: &World, x: f32, z: f32) -> Option<f32>;

    fn surface_within_boundary(&self, world: &World, boundary: &[Vec2]) -> Option<TriangleMesh>;

    fn volume_above_datum(&self, world: &World, boundary: &[Vec2], datum_y: f32) -> Option<f64>;
}

#[derive(Resource, Default)]
pub struct TerrainProviderRegistry {
    provider: Option<Arc<dyn TerrainProvider>>,
}

impl TerrainProviderRegistry {
    pub fn register<T>(&mut self, provider: T)
    where
        T: TerrainProvider,
    {
        self.provider = Some(Arc::new(provider));
    }

    pub fn provider(&self) -> Option<Arc<dyn TerrainProvider>> {
        self.provider.clone()
    }
}

pub trait TerrainProviderRegistryAppExt {
    fn register_terrain_provider<T>(&mut self, provider: T) -> &mut Self
    where
        T: TerrainProvider;
}

impl TerrainProviderRegistryAppExt for App {
    fn register_terrain_provider<T>(&mut self, provider: T) -> &mut Self
    where
        T: TerrainProvider,
    {
        if !self.world().contains_resource::<TerrainProviderRegistry>() {
            self.init_resource::<TerrainProviderRegistry>();
        }

        self.world_mut()
            .resource_mut::<TerrainProviderRegistry>()
            .register(provider);
        self
    }
}

pub struct RequireWorkbench<T> {
    _marker: PhantomData<T>,
}

impl<T> RequireWorkbench<T> {
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<T> Default for RequireWorkbench<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Plugin for RequireWorkbench<T>
where
    T: Resource,
{
    fn build(&self, app: &mut App) {
        assert!(
            app.world().contains_resource::<T>(),
            "Required workbench resource '{}' is missing",
            std::any::type_name::<T>()
        );
    }
}

// ---------------------------------------------------------------------------
// Unit tests for PP71 registry additions
// ---------------------------------------------------------------------------

#[cfg(test)]
mod pp71_tests {
    use super::*;

    fn make_wall_class() -> ElementClassDescriptor {
        let mut class_min_obligations = HashMap::new();
        class_min_obligations.insert(
            RefinementState::Constructible,
            vec![
                ObligationTemplate {
                    id: ObligationId("structure".into()),
                    role: SemanticRole("primary_structure".into()),
                    required_by_state: RefinementState::Constructible,
                },
                ObligationTemplate {
                    id: ObligationId("thermal_layer".into()),
                    role: SemanticRole("thermal".into()),
                    required_by_state: RefinementState::Constructible,
                },
            ],
        );
        let mut class_min_promotion_critical_paths = HashMap::new();
        class_min_promotion_critical_paths.insert(
            RefinementState::Constructible,
            vec![ClaimPath("height_mm".into()), ClaimPath("length_mm".into())],
        );
        ElementClassDescriptor {
            id: ElementClassId("wall_assembly".into()),
            label: "Wall Assembly".into(),
            description: "A wall assembly".into(),
            semantic_roles: vec![SemanticRole("exterior_envelope".into())],
            class_min_obligations,
            class_min_promotion_critical_paths,
            parameter_schema: serde_json::json!({}),
        }
    }

    fn make_recipe(class_id: ElementClassId) -> RecipeFamilyDescriptor {
        let mut obligation_specializations = HashMap::new();
        obligation_specializations.insert(
            RefinementState::Constructible,
            vec![ObligationTemplate {
                id: ObligationId("lateral_bracing".into()),
                role: SemanticRole("bracing".into()),
                required_by_state: RefinementState::Constructible,
            }],
        );
        let mut promotion_critical_path_specializations = HashMap::new();
        promotion_critical_path_specializations.insert(
            RefinementState::Constructible,
            vec![ClaimPath("stud_spacing_mm".into())],
        );
        RecipeFamilyDescriptor {
            id: RecipeFamilyId("light_frame_exterior_wall".into()),
            target_class: class_id,
            label: "Light Frame Exterior Wall".into(),
            description: "Light-frame wall recipe".into(),
            parameters: vec![RecipeParameter {
                name: "length_mm".into(),
                value_schema: serde_json::json!({"type":"number","minimum":0}),
                default: None,
            }],
            supported_refinement_levels: vec![
                RefinementState::Conceptual,
                RefinementState::Schematic,
                RefinementState::Constructible,
            ],
            obligation_specializations,
            promotion_critical_path_specializations,
            generate: Arc::new(|_input, _world| Ok(GenerateOutput::default())),
        }
    }

    #[test]
    fn register_and_retrieve_element_class() {
        let mut registry = CapabilityRegistry::default();
        let descriptor = make_wall_class();
        registry.register_element_class(descriptor);

        let all = registry.element_class_descriptors();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id.0, "wall_assembly");

        let found = registry.element_class_descriptor(&ElementClassId("wall_assembly".into()));
        assert!(found.is_some());
        assert_eq!(found.unwrap().label, "Wall Assembly");

        assert!(registry
            .element_class_descriptor(&ElementClassId("unknown".into()))
            .is_none());
    }

    #[test]
    fn register_and_retrieve_recipe_family() {
        let mut registry = CapabilityRegistry::default();
        registry.register_element_class(make_wall_class());
        registry.register_recipe_family(make_recipe(ElementClassId("wall_assembly".into())));

        // Unfiltered
        let all = registry.recipe_family_descriptors(None);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id.0, "light_frame_exterior_wall");

        // Filtered to correct class
        let filtered =
            registry.recipe_family_descriptors(Some(&ElementClassId("wall_assembly".into())));
        assert_eq!(filtered.len(), 1);

        // Filtered to nonexistent class returns empty
        let empty = registry.recipe_family_descriptors(Some(&ElementClassId("roof_system".into())));
        assert!(empty.is_empty());

        // Direct lookup
        let found =
            registry.recipe_family_descriptor(&RecipeFamilyId("light_frame_exterior_wall".into()));
        assert!(found.is_some());

        assert!(registry
            .recipe_family_descriptor(&RecipeFamilyId("unknown".into()))
            .is_none());
    }

    #[test]
    fn effective_obligations_merges_class_min_and_recipe() {
        let class = make_wall_class();
        let recipe = make_recipe(ElementClassId("wall_assembly".into()));

        let obligations =
            effective_obligations(&class, Some(&recipe), RefinementState::Constructible);
        // class min has 2 + recipe adds 1
        assert_eq!(obligations.len(), 3);
        assert!(obligations.iter().any(|o| o.id.0 == "structure"));
        assert!(obligations.iter().any(|o| o.id.0 == "lateral_bracing"));
    }

    #[test]
    fn effective_obligations_without_recipe_returns_class_min_only() {
        let class = make_wall_class();
        let obligations = effective_obligations(&class, None, RefinementState::Constructible);
        assert_eq!(obligations.len(), 2);
    }

    #[test]
    fn effective_promotion_critical_paths_merges_class_and_recipe() {
        let class = make_wall_class();
        let recipe = make_recipe(ElementClassId("wall_assembly".into()));

        let paths = effective_promotion_critical_paths(
            &class,
            Some(&recipe),
            RefinementState::Constructible,
        );
        // class has 2 + recipe adds 1
        assert_eq!(paths.len(), 3);
        assert!(paths.iter().any(|p| p.0 == "stud_spacing_mm"));
    }

    #[test]
    #[should_panic(expected = "ElementClass 'wall_assembly' was registered more than once")]
    fn register_duplicate_element_class_panics() {
        let mut registry = CapabilityRegistry::default();
        registry.register_element_class(make_wall_class());
        registry.register_element_class(make_wall_class());
    }

    #[test]
    #[should_panic(
        expected = "RecipeFamily 'light_frame_exterior_wall' was registered more than once"
    )]
    fn register_duplicate_recipe_family_panics() {
        let mut registry = CapabilityRegistry::default();
        registry.register_element_class(make_wall_class());
        registry.register_recipe_family(make_recipe(ElementClassId("wall_assembly".into())));
        registry.register_recipe_family(make_recipe(ElementClassId("wall_assembly".into())));
    }
}
