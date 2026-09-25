//! GPU selection tint/stipple over the actual selected surfaces, including group
//! members. Reuses each source Mesh handle; never copies vertices or emits an
//! edge/dot per frame. Overlays are children of the render source, so interactive
//! transform previews and inherited visibility follow the same presentation.
use std::collections::{HashMap, HashSet};

use bevy::{
    camera::visibility::RenderLayers,
    light::{NotShadowCaster, NotShadowReceiver},
    picking::Pickable,
    prelude::*,
};

use crate::plugins::{
    drawing_export::{HiddenDuringViewportExport, ViewportExportState},
    face_edit::FaceEditContext,
    identity::ElementId,
    modeling::{group::GroupMembers, occurrence::GeneratedOccurrencePart},
    selection::Selected,
    subobject_overlay::FaceStippleMaterial,
    tools::Preview,
};

#[derive(Component)]
pub(crate) struct SelectedObjectOverlay(pub Entity);

#[derive(Resource, Default)]
struct ObjectSelectionOverlayState {
    material: Option<Handle<FaceStippleMaterial>>,
}

pub(crate) fn install(app: &mut App) {
    app.init_resource::<ObjectSelectionOverlayState>()
        .add_systems(Update, sync_object_selection_overlay);
}

// Selection/group traversal is O(authored entities), with no geometry reads.
// Idle frames do no mesh/material allocation and mutate no existing overlay.
fn sync_object_selection_overlay(world: &mut World) {
    let suppressed = world
        .get_resource::<FaceEditContext>()
        .is_some_and(|c| c.is_active())
        || world
            .get_resource::<ViewportExportState>()
            .is_some_and(|s| s.ui_suppressed());
    let mut ids: HashSet<ElementId> = if suppressed {
        HashSet::new()
    } else {
        world
            .query_filtered::<&ElementId, With<Selected>>()
            .iter(world)
            .copied()
            .collect()
    };
    let groups: HashMap<ElementId, Vec<ElementId>> = if ids.is_empty() {
        HashMap::new()
    } else {
        world
            .query::<(&ElementId, &GroupMembers)>()
            .iter(world)
            .map(|(id, g)| (*id, g.member_ids.clone()))
            .collect()
    };
    let mut pending: Vec<_> = ids.iter().copied().collect();
    while let Some(id) = pending.pop() {
        if let Some(members) = groups.get(&id) {
            for member in members {
                if ids.insert(*member) {
                    pending.push(*member);
                }
            }
        }
    }
    let desired: HashMap<Entity, (Handle<Mesh>, RenderLayers)> = if ids.is_empty() {
        HashMap::new()
    } else {
        world
            .query_filtered::<(
                Entity,
                &Mesh3d,
                Option<&ElementId>,
                Option<&GeneratedOccurrencePart>,
                Option<&RenderLayers>,
            ), Without<SelectedObjectOverlay>>()
            .iter(world)
            .filter(|(_, _, id, generated, _)| {
                id.is_some_and(|id| ids.contains(id))
                    || generated.is_some_and(|g| ids.contains(&g.owner))
            })
            .map(|(entity, mesh, _, _, layers)| {
                (
                    entity,
                    (mesh.0.clone(), layers.cloned().unwrap_or_default()),
                )
            })
            .collect()
    };
    let existing: HashMap<Entity, Entity> = world
        .query::<(Entity, &SelectedObjectOverlay)>()
        .iter(world)
        .map(|(entity, overlay)| (overlay.0, entity))
        .collect();
    for (source, overlay) in &existing {
        if !desired.contains_key(source) {
            world.despawn(*overlay);
        }
    }
    if desired.is_empty() {
        return;
    }
    let material = match world
        .resource::<ObjectSelectionOverlayState>()
        .material
        .clone()
    {
        Some(handle) => handle,
        None => {
            let handle =
                world
                    .resource_mut::<Assets<FaceStippleMaterial>>()
                    .add(FaceStippleMaterial {
                        color: LinearRgba::new(0.08, 0.32, 1.0, 0.72),
                        // Blue wash between dots keeps even small/far selected parts readable.
                        params: Vec4::new(5.0, 1.0, 0.14, 0.0),
                    });
            world.resource_mut::<ObjectSelectionOverlayState>().material = Some(handle.clone());
            handle
        }
    };
    for (source, (mesh, layers)) in desired {
        if let Some(overlay) = existing.get(&source) {
            if world.get::<Mesh3d>(*overlay).is_none_or(|m| m.0 != mesh) {
                world.entity_mut(*overlay).insert(Mesh3d(mesh));
            }
            if world.get::<RenderLayers>(*overlay) != Some(&layers) {
                world.entity_mut(*overlay).insert(layers);
            }
        } else {
            world.spawn((
                SelectedObjectOverlay(source),
                Mesh3d(mesh),
                MeshMaterial3d(material.clone()),
                ChildOf(source),
                Transform::IDENTITY,
                Visibility::Inherited,
                layers,
                Pickable::IGNORE,
                NotShadowCaster,
                NotShadowReceiver,
                HiddenDuringViewportExport,
                Preview,
                Name::new("Selected object highlight"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::uuid_handle;

    #[test]
    fn nested_selection_reuses_mesh_and_tracks_preview_visibility_and_replacement() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            TransformPlugin,
            bevy::camera::visibility::VisibilityPlugin,
        ));
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<bevy::mesh::skinning::SkinnedMeshInverseBindposes>>();
        app.init_resource::<ObjectSelectionOverlayState>()
            .init_resource::<Assets<FaceStippleMaterial>>();
        let mesh = uuid_handle!("1d1b983f-9ccf-4c91-9d87-7cb96977d603");
        let source = app
            .world_mut()
            .spawn((
                ElementId(1),
                Mesh3d(mesh.clone()),
                Transform::from_xyz(2.0, 3.0, 4.0),
                Visibility::Visible,
            ))
            .id();
        let group = app
            .world_mut()
            .spawn((
                ElementId(2),
                Selected,
                GroupMembers {
                    name: "veranda".into(),
                    member_ids: vec![ElementId(1)],
                    frame: default(),
                    linked_model: None,
                },
            ))
            .id();
        sync_object_selection_overlay(app.world_mut());
        app.update();
        let overlay = app
            .world_mut()
            .query::<(Entity, &SelectedObjectOverlay)>()
            .single(app.world())
            .unwrap()
            .0;
        assert_eq!(app.world().get::<Mesh3d>(overlay).unwrap().0, mesh);
        assert!(app.world().get::<ElementId>(overlay).is_none());
        assert!(app
            .world()
            .get::<HiddenDuringViewportExport>(overlay)
            .is_some());
        assert_eq!(
            app.world()
                .get::<GlobalTransform>(overlay)
                .unwrap()
                .translation(),
            Vec3::new(2.0, 3.0, 4.0)
        );
        app.world_mut()
            .get_mut::<Transform>(source)
            .unwrap()
            .translation
            .x = 7.0;
        sync_object_selection_overlay(app.world_mut());
        app.update();
        assert_eq!(
            app.world()
                .get::<GlobalTransform>(overlay)
                .unwrap()
                .translation()
                .x,
            7.0
        );
        assert_eq!(
            app.world().get::<ChildOf>(overlay).unwrap().parent(),
            source
        );
        app.world_mut()
            .entity_mut(source)
            .insert(Visibility::Hidden);
        app.update();
        assert!(!app
            .world()
            .get::<InheritedVisibility>(overlay)
            .unwrap()
            .get());
        app.world_mut()
            .entity_mut(source)
            .insert(Visibility::Visible);
        app.update();
        assert!(app
            .world()
            .get::<InheritedVisibility>(overlay)
            .unwrap()
            .get());
        let replacement = uuid_handle!("225e6e83-2089-42b9-97e8-116b4d27f0d3");
        app.world_mut()
            .entity_mut(source)
            .insert(Mesh3d(replacement.clone()));
        sync_object_selection_overlay(app.world_mut());
        assert_eq!(app.world().get::<Mesh3d>(overlay).unwrap().0, replacement);
        // Idle sync keeps the same derived entity and shared material.
        assert_eq!(
            app.world().resource::<Assets<FaceStippleMaterial>>().len(),
            1
        );
        app.world_mut().entity_mut(group).remove::<Selected>();
        sync_object_selection_overlay(app.world_mut());
        assert!(app.world().get_entity(overlay).is_err());
        app.world_mut().entity_mut(group).insert(Selected);
        sync_object_selection_overlay(app.world_mut());
        assert_eq!(
            app.world_mut()
                .query::<&SelectedObjectOverlay>()
                .iter(app.world())
                .count(),
            1
        );
        app.world_mut().despawn(source);
        sync_object_selection_overlay(app.world_mut());
        assert_eq!(
            app.world_mut()
                .query::<&SelectedObjectOverlay>()
                .iter(app.world())
                .count(),
            0
        );
    }
}
