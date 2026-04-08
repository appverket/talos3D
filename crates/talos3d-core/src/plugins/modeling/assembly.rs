use std::any::Any;

use bevy::{ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    authored_entity::{
        invalid_property_error, property_field, read_only_property_field, AuthoredEntity,
        BoxedEntity, EntityBounds, HandleInfo, PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{AuthoredEntityFactory, CapabilityRegistry, ModelSummaryAccumulator},
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        identity::{ElementId, ElementIdAllocator},
    },
};

/// Check if an entity with the given ElementId exists (works with &World).
fn element_exists(world: &World, id: ElementId) -> bool {
    let Some(mut q) = world.try_query::<&ElementId>() else {
        return false;
    };
    q.iter(world).any(|eid| *eid == id)
}

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

/// A reference to a member of an assembly, with its semantic role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssemblyMemberRef {
    /// ElementId of the member (entity or sub-assembly).
    pub target: ElementId,
    /// Capability-defined role within this assembly.
    pub role: String,
}

/// A semantic assembly — a higher-order domain object composed of
/// members with roles. Standalone authored entity, not a Group.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticAssembly {
    /// Capability-defined assembly type (e.g. "house", "room", "storey").
    pub assembly_type: String,
    /// Human-readable label.
    pub label: String,
    /// Typed membership references with roles.
    pub members: Vec<AssemblyMemberRef>,
    /// Assembly-level parameters (not derivable from members).
    #[serde(default)]
    pub parameters: serde_json::Value,
    /// Optional domain metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// A typed, directed relationship between two authored entities or assemblies.
/// Each relationship is its own ECS entity with an ElementId.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticRelation {
    /// The entity/assembly this relationship originates from.
    pub source: ElementId,
    /// The entity/assembly this relationship points to.
    pub target: ElementId,
    /// Capability-defined relationship type (e.g. "hosted_on", "bounds").
    pub relation_type: String,
    /// Relationship-specific parameters.
    #[serde(default)]
    pub parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssemblySnapshot {
    pub element_id: ElementId,
    pub assembly: SemanticAssembly,
}

impl From<AssemblySnapshot> for BoxedEntity {
    fn from(snapshot: AssemblySnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum AssemblySnapshotJson {
    Assembly(AssemblySnapshot),
}

impl AuthoredEntity for AssemblySnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "semantic_assembly"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        self.assembly.label.clone()
    }

    fn center(&self) -> Vec3 {
        Vec3::ZERO
    }

    fn bounds(&self) -> Option<EntityBounds> {
        None
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        self.clone().into()
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        self.clone().into()
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        self.clone().into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            read_only_property_field(
                "assembly_type",
                "type",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.assembly.assembly_type.clone())),
            ),
            property_field(
                "label",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.assembly.label.clone())),
            ),
            read_only_property_field(
                "member_count",
                "members",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.assembly.members.len() as f32)),
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "label" => {
                snapshot.assembly.label = value
                    .as_str()
                    .ok_or_else(|| "Expected string value for label".to_string())?
                    .to_string();
            }
            _ => return Err(invalid_property_error("semantic_assembly", &["label"])),
        }
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        Vec::new()
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(AssemblySnapshotJson::Assembly(self.clone())).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            if let Some(mut assembly) = world.get_mut::<SemanticAssembly>(entity) {
                *assembly = self.assembly.clone();
            }
        } else {
            world.spawn((self.element_id, self.assembly.clone()));
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {}

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }
}

// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationSnapshot {
    pub element_id: ElementId,
    pub relation: SemanticRelation,
}

impl From<RelationSnapshot> for BoxedEntity {
    fn from(snapshot: RelationSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum RelationSnapshotJson {
    Relation(RelationSnapshot),
}

impl AuthoredEntity for RelationSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "semantic_relation"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!(
            "{} ({} → {})",
            self.relation.relation_type, self.relation.source.0, self.relation.target.0
        )
    }

    fn center(&self) -> Vec3 {
        Vec3::ZERO
    }

    fn bounds(&self) -> Option<EntityBounds> {
        None
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        self.clone().into()
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        self.clone().into()
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        self.clone().into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            read_only_property_field(
                "relation_type",
                "type",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.relation.relation_type.clone())),
            ),
            read_only_property_field(
                "source",
                "source",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.relation.source.0 as f32)),
            ),
            read_only_property_field(
                "target",
                "target",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.relation.target.0 as f32)),
            ),
        ]
    }

    fn set_property_json(
        &self,
        _property_name: &str,
        _value: &Value,
    ) -> Result<BoxedEntity, String> {
        Err(invalid_property_error("semantic_relation", &[]))
    }

    fn handles(&self) -> Vec<HandleInfo> {
        Vec::new()
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(RelationSnapshotJson::Relation(self.clone())).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            if let Some(mut relation) = world.get_mut::<SemanticRelation>(entity) {
                *relation = self.relation.clone();
            }
        } else {
            world.spawn((self.element_id, self.relation.clone()));
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {}

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }
}

// ---------------------------------------------------------------------------
// Factories
// ---------------------------------------------------------------------------

pub struct AssemblyFactory;

impl AuthoredEntityFactory for AssemblyFactory {
    fn type_name(&self) -> &'static str {
        "semantic_assembly"
    }

    fn capture_snapshot(&self, entity_ref: &EntityRef, _world: &World) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let assembly = entity_ref.get::<SemanticAssembly>()?;
        Some(
            AssemblySnapshot {
                element_id,
                assembly: assembly.clone(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        match serde_json::from_value::<AssemblySnapshotJson>(data.clone())
            .map_err(|error| error.to_string())?
        {
            AssemblySnapshotJson::Assembly(snapshot) => Ok(snapshot.into()),
        }
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;

        let assembly_type = object
            .get("assembly_type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing 'assembly_type'".to_string())?
            .to_string();

        // Validate assembly_type against registered vocabulary.
        let registry = world.resource::<CapabilityRegistry>();
        let valid_types: Vec<&str> = registry
            .assembly_type_descriptors()
            .iter()
            .map(|d| d.assembly_type.as_str())
            .collect();
        if !valid_types.contains(&assembly_type.as_str()) {
            return Err(format!(
                "Unknown assembly type '{}'. Registered types: {}",
                assembly_type,
                valid_types.join(", ")
            ));
        }

        let label = object
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let members: Vec<AssemblyMemberRef> = object
            .get("members")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        let obj = v.as_object()?;
                        Some(AssemblyMemberRef {
                            target: ElementId(obj.get("target")?.as_u64()?),
                            role: obj.get("role")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Validate member targets exist.
        for member in &members {
            if !element_exists(world, member.target) {
                return Err(format!("Member target {} does not exist", member.target.0));
            }
        }

        let parameters = object
            .get("parameters")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        let metadata = object
            .get("metadata")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        Ok(AssemblySnapshot {
            element_id: world.resource::<ElementIdAllocator>().next_id(),
            assembly: SemanticAssembly {
                assembly_type,
                label,
                members,
                parameters,
                metadata,
            },
        }
        .into())
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let mut q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in q.iter(world) {
            let Some(assembly) = entity_ref.get::<SemanticAssembly>() else {
                continue;
            };
            if entity_ref.get::<ElementId>().is_none() {
                continue;
            }
            *summary
                .assembly_counts
                .entry(assembly.assembly_type.clone())
                .or_insert(0) += 1;
        }
    }

    fn collect_delete_dependencies(
        &self,
        _world: &World,
        _requested_ids: &[ElementId],
        _out: &mut Vec<ElementId>,
    ) {
        // Assemblies do NOT cascade-delete their members.
        // Member cleanup (pruning stale refs) is handled by the shared delete pipeline.
    }
}

// ---------------------------------------------------------------------------

pub struct RelationFactory;

impl AuthoredEntityFactory for RelationFactory {
    fn type_name(&self) -> &'static str {
        "semantic_relation"
    }

    fn capture_snapshot(&self, entity_ref: &EntityRef, _world: &World) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let relation = entity_ref.get::<SemanticRelation>()?;
        Some(
            RelationSnapshot {
                element_id,
                relation: relation.clone(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        match serde_json::from_value::<RelationSnapshotJson>(data.clone())
            .map_err(|error| error.to_string())?
        {
            RelationSnapshotJson::Relation(snapshot) => Ok(snapshot.into()),
        }
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;

        let source = ElementId(
            object
                .get("source")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| "Missing 'source'".to_string())?,
        );

        let target = ElementId(
            object
                .get("target")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| "Missing 'target'".to_string())?,
        );

        let relation_type = object
            .get("relation_type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing 'relation_type'".to_string())?
            .to_string();

        // Validate relation_type against registered vocabulary.
        let registry = world.resource::<CapabilityRegistry>();
        let valid_types: Vec<&str> = registry
            .relation_type_descriptors()
            .iter()
            .map(|d| d.relation_type.as_str())
            .collect();
        if !valid_types.contains(&relation_type.as_str()) {
            return Err(format!(
                "Unknown relation type '{}'. Registered types: {}",
                relation_type,
                valid_types.join(", ")
            ));
        }

        // Validate source and target entities exist.
        if !element_exists(world, source) {
            return Err(format!("Relation source {} does not exist", source.0));
        }
        if !element_exists(world, target) {
            return Err(format!("Relation target {} does not exist", target.0));
        }

        let parameters = object
            .get("parameters")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        Ok(RelationSnapshot {
            element_id: world.resource::<ElementIdAllocator>().next_id(),
            relation: SemanticRelation {
                source,
                target,
                relation_type,
                parameters,
            },
        }
        .into())
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let mut q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in q.iter(world) {
            let Some(relation) = entity_ref.get::<SemanticRelation>() else {
                continue;
            };
            if entity_ref.get::<ElementId>().is_none() {
                continue;
            }
            *summary
                .relation_counts
                .entry(relation.relation_type.clone())
                .or_insert(0) += 1;
        }
    }

    fn collect_delete_dependencies(
        &self,
        world: &World,
        requested_ids: &[ElementId],
        out: &mut Vec<ElementId>,
    ) {
        // When an entity/assembly that is source or target of a relation is deleted,
        // cascade-delete the relation.
        let mut q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in q.iter(world) {
            let (Some(element_id), Some(relation)) = (
                entity_ref.get::<ElementId>(),
                entity_ref.get::<SemanticRelation>(),
            ) else {
                continue;
            };
            if (requested_ids.contains(&relation.source)
                || requested_ids.contains(&relation.target))
                && !out.contains(element_id)
            {
                out.push(*element_id);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Remove a member from all assemblies that reference it.
/// Mutates SemanticAssembly components directly. Used by the shared delete pipeline.
pub fn remove_member_from_assemblies(world: &mut World, member_id: ElementId) {
    // Collect entities to modify first to avoid borrow conflicts.
    let affected: Vec<Entity> = {
        let mut q = world.try_query::<(Entity, &SemanticAssembly)>().unwrap();
        q.iter(world)
            .filter(|(_, assembly)| assembly.members.iter().any(|m| m.target == member_id))
            .map(|(entity, _)| entity)
            .collect()
    };
    for entity in affected {
        if let Some(mut assembly) = world.get_mut::<SemanticAssembly>(entity) {
            assembly.members.retain(|m| m.target != member_id);
        }
    }
}

/// Find all assembly ElementIds that contain the given member.
pub fn find_assemblies_for_member(world: &World, member_id: ElementId) -> Vec<ElementId> {
    let mut q = world.try_query::<EntityRef>().unwrap();
    q.iter(world)
        .filter_map(|entity_ref| {
            let assembly = entity_ref.get::<SemanticAssembly>()?;
            if assembly.members.iter().any(|m| m.target == member_id) {
                Some(*entity_ref.get::<ElementId>()?)
            } else {
                None
            }
        })
        .collect()
}
