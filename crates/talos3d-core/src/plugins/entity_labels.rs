//! Shared `ElementId` → display-label resolution.
//!
//! Several read-only views (Outliner, Layers panel, Dependency-graph panel and
//! their model-api mirrors) need a human label for each authored entity. They
//! all derive it the same way: through the lightweight
//! [`CapabilityRegistry::display_label`] path so any authored entity kind
//! contributes its own `label()` without the caller knowing concrete types.
//! This module owns that single derivation so the surfaces never drift.

use std::collections::HashMap;

use bevy::{ecs::world::EntityRef, prelude::*};

use crate::capability_registry::CapabilityRegistry;
use crate::plugins::identity::ElementId;

/// Resolve a display label for one entity via the capability metadata path.
/// Returns `None` when there is no `CapabilityRegistry`, no label provider for the
/// entity, or its label is empty.
pub fn entity_label(world: &World, entity: Entity) -> Option<String> {
    let registry = world.get_resource::<CapabilityRegistry>()?;
    let entity_ref: EntityRef = world.get_entity(entity).ok()?;
    let label = registry.display_label(&entity_ref, world)?;
    (!label.is_empty()).then_some(label)
}

/// Build an `ElementId.0` → label map for every authored entity in the world.
/// Entities without a resolvable label are simply absent from the map; callers
/// fall back to a synthetic `#id` form.
pub fn collect_entity_labels(world: &mut World) -> HashMap<u64, String> {
    let ids: Vec<(u64, Entity)> = {
        let mut query = world.query::<(Entity, &ElementId)>();
        query
            .iter(world)
            .map(|(entity, element_id)| (element_id.0, entity))
            .collect()
    };
    let mut labels = HashMap::with_capacity(ids.len());
    for (eid, entity) in ids {
        if let Some(label) = entity_label(world, entity) {
            labels.insert(eid, label);
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        authored_entity::BoxedEntity,
        capability_registry::{AuthoredEntityFactory, SnapshotCaptureRole},
        plugins::modeling::{
            group::{GroupFactory, GroupMembers},
            primitives::TriangleMesh,
            snapshots::TriangleMeshFactory,
        },
    };
    use serde_json::Value;

    struct MetadataOnlyFactory {
        role: SnapshotCaptureRole,
        label: &'static str,
    }
    impl AuthoredEntityFactory for MetadataOnlyFactory {
        fn type_name(&self) -> &'static str {
            self.label
        }
        fn display_label(&self, _: &EntityRef, _: &World) -> Option<String> {
            Some(self.label.into())
        }
        fn capture_role(&self, _: &EntityRef, _: &World) -> SnapshotCaptureRole {
            self.role
        }
        fn capture_snapshot(&self, _: &EntityRef, _: &World) -> Option<BoxedEntity> {
            panic!("browsing metadata must not capture geometry")
        }
        fn from_persisted_json(&self, _: &Value) -> Result<BoxedEntity, String> {
            unreachable!()
        }
        fn from_create_request(&self, _: &World, _: &Value) -> Result<BoxedEntity, String> {
            unreachable!()
        }
    }

    #[test]
    fn labels_use_metadata_without_geometry_and_preserve_primary_precedence() {
        let mut world = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(MetadataOnlyFactory {
            role: SnapshotCaptureRole::DerivedGeometry,
            label: "derived",
        });
        registry.register_factory(MetadataOnlyFactory {
            role: SnapshotCaptureRole::PrimaryAuthored,
            label: "authored",
        });
        world.insert_resource(registry);
        let entity = world.spawn(ElementId(1)).id();
        assert_eq!(entity_label(&world, entity).as_deref(), Some("authored"));
    }

    #[test]
    fn imported_group_and_mesh_labels_do_not_traverse_members() {
        let mut world = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(GroupFactory);
        registry.register_factory(TriangleMeshFactory);
        registry.register_factory(MetadataOnlyFactory {
            role: SnapshotCaptureRole::PrimaryAuthored,
            label: "uncaptured child",
        });
        world.insert_resource(registry);
        world.spawn(ElementId(2));
        let group = world
            .spawn((
                ElementId(1),
                GroupMembers {
                    name: "Roof".into(),
                    member_ids: vec![ElementId(2)],
                    frame: Default::default(),
                    linked_model: None,
                },
            ))
            .id();
        let mesh = world
            .spawn((
                ElementId(3),
                TriangleMesh {
                    name: Some("Siding".into()),
                    vertices: vec![],
                    faces: vec![],
                    normals: None,
                },
            ))
            .id();
        assert_eq!(entity_label(&world, group).as_deref(), Some("Roof"));
        assert_eq!(entity_label(&world, mesh).as_deref(), Some("Siding"));
        world.get_mut::<TriangleMesh>(mesh).unwrap().name = Some("Renamed siding".into());
        assert_eq!(
            entity_label(&world, mesh).as_deref(),
            Some("Renamed siding")
        );
        // Exercise the real panel builders: both used to clone all geometry
        // and recursively compute every group's bounds just to paint names.
        use crate::plugins::{
            layers::{LayerRegistry, LayerState},
            layers_panel::{build_layers_panel_data, LayersPanelData, LayersPanelState},
            outliner::{build_outliner_tree, OutlinerTree, OutlinerWindowState},
        };
        world.init_resource::<LayerRegistry>();
        world.init_resource::<LayerState>();
        world.init_resource::<LayersPanelData>();
        world.insert_resource(LayersPanelState {
            visible: true,
            ..Default::default()
        });
        build_layers_panel_data(&mut world);
        assert_eq!(
            world.resource::<LayersPanelData>().layers[0].member_count,
            3
        );
        world.init_resource::<OutlinerTree>();
        world.insert_resource(OutlinerWindowState::default());
        world.resource_mut::<OutlinerWindowState>().visible = true;
        build_outliner_tree(&mut world);
        assert_eq!(world.resource::<OutlinerTree>().nodes.len(), 3);
    }
}
