use std::{collections::HashMap, marker::PhantomData, sync::Arc};

use bevy::{app::App, ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::authored_entity::BoxedEntity;
use crate::plugins::document_properties::DocumentProperties;
use crate::plugins::identity::ElementId;
use crate::plugins::modeling::primitives::TriangleMesh;

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
    relation_type_descriptors: Vec<RelationTypeDescriptor>,
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

    pub fn register_relation_type(&mut self, descriptor: RelationTypeDescriptor) {
        self.relation_type_descriptors.push(descriptor);
    }

    pub fn assembly_type_descriptors(&self) -> &[AssemblyTypeDescriptor] {
        &self.assembly_type_descriptors
    }

    pub fn relation_type_descriptors(&self) -> &[RelationTypeDescriptor] {
        &self.relation_type_descriptors
    }
}

fn validate_capability_dependencies(registry: Res<CapabilityRegistry>) {
    let errors = registry.validate_dependencies();
    for error in &errors {
        warn!("{error}");
    }
}

pub trait CapabilityRegistryAppExt {
    fn register_capability(&mut self, descriptor: CapabilityDescriptor) -> &mut Self;

    fn register_workbench(&mut self, descriptor: WorkbenchDescriptor) -> &mut Self;

    fn register_authored_entity_factory<F>(&mut self, factory: F) -> &mut Self
    where
        F: AuthoredEntityFactory;

    fn register_assembly_type(&mut self, descriptor: AssemblyTypeDescriptor) -> &mut Self;

    fn register_relation_type(&mut self, descriptor: RelationTypeDescriptor) -> &mut Self;
}

#[derive(Resource)]
struct CapabilityValidationScheduled;

impl CapabilityRegistryAppExt for App {
    fn register_capability(&mut self, descriptor: CapabilityDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
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

    fn register_relation_type(&mut self, descriptor: RelationTypeDescriptor) -> &mut Self {
        if !self.world().contains_resource::<CapabilityRegistry>() {
            self.init_resource::<CapabilityRegistry>();
        }

        self.world_mut()
            .resource_mut::<CapabilityRegistry>()
            .register_relation_type(descriptor);
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
