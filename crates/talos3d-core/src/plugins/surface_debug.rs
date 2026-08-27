//! Surface debug render: a flat false-colour pass over the authored envelope.
//!
//! Shaded renders are a poor instrument for the one question that matters most
//! when reviewing a building: *does every visible surface carry the finish it
//! should?* An unfinished surface renders in the default near-white material,
//! and against pale cladding under a pale roof it is close to invisible — which
//! is exactly how a bare gable field survives review by an agent looking at its
//! own screenshot.
//!
//! This mode removes the judgment call. Every mesh is painted a flat unlit
//! colour derived from the material it actually resolves to, and anything with
//! no resolved material is painted **magenta**. The convention is borrowed from
//! game-engine debug views for the same reason it works there: the eye cannot
//! miss it, and neither can a vision model.
//!
//! It is a presentation mode only. No authored data is touched; the original
//! material handle is stashed per-entity and restored on exit.

use bevy::{platform::collections::HashMap, prelude::*};

use crate::curation::material_specs::MaterialSpecRegistry;
use crate::plugins::materials::MaterialAssignment;

/// Colour for geometry that resolves to no material at all. Deliberately a
/// colour no real building finish would ever be.
const UNFINISHED_COLOR: Color = Color::srgb(1.0, 0.0, 1.0);

/// Toggle for the false-colour envelope pass.
#[derive(Resource, Debug, Clone, Default)]
pub struct SurfaceDebugRender {
    pub enabled: bool,
}

/// Cache of flat debug materials, keyed by resolved material id (or the empty
/// string for "no material").
#[derive(Resource, Default)]
struct SurfaceDebugPalette(HashMap<String, Handle<StandardMaterial>>);

/// The entity's real material, stashed while the debug pass is active.
#[derive(Component)]
struct SurfaceDebugOriginal(Option<MeshMaterial3d<StandardMaterial>>);

pub struct SurfaceDebugPlugin;

impl Plugin for SurfaceDebugPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SurfaceDebugRender>()
            .init_resource::<SurfaceDebugPalette>()
            .add_systems(Update, (enter_surface_debug, exit_surface_debug));
    }
}

fn enter_surface_debug(
    mut commands: Commands,
    mode: Res<SurfaceDebugRender>,
    mut palette: ResMut<SurfaceDebugPalette>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    specs: Option<Res<MaterialSpecRegistry>>,
    entering: Query<
        (
            Entity,
            Option<&MaterialAssignment>,
            Option<&MeshMaterial3d<StandardMaterial>>,
        ),
        (With<Mesh3d>, Without<SurfaceDebugOriginal>),
    >,
) {
    if !mode.is_changed() || !mode.enabled {
        return;
    }

    for (entity, assignment, current) in entering.iter() {
        let key = assignment
            .and_then(|assignment| assignment.render_material_id(specs.as_deref()))
            .unwrap_or_default();
        let handle = palette
            .0
            .entry(key.clone())
            .or_insert_with(|| std_materials.add(flat_material(&key)))
            .clone();
        commands
            .entity(entity)
            .insert(SurfaceDebugOriginal(current.cloned()))
            .insert(MeshMaterial3d::<StandardMaterial>(handle));
    }
}

/// Restoring authored materials needs mutable access to `MaterialAssignment`,
/// which cannot share a system with the immutable read above (Bevy B0001), so
/// the exit path is its own system.
fn exit_surface_debug(
    mut commands: Commands,
    mode: Res<SurfaceDebugRender>,
    leaving: Query<(Entity, &SurfaceDebugOriginal)>,
    mut assignments: Query<&mut MaterialAssignment>,
    primitive_material: Option<Res<crate::plugins::modeling::mesh_generation::PrimitiveMaterial>>,
) {
    if !mode.is_changed() || mode.enabled {
        return;
    }

    for (entity, original) in leaving.iter() {
        let mut entity_commands = commands.entity(entity);
        entity_commands.remove::<SurfaceDebugOriginal>();
        match &original.0 {
            Some(material) => {
                entity_commands.insert(material.clone());
            }
            // No stashed handle: fall back to the default primitive material
            // rather than removing the component. A mesh with no material at
            // all renders single-sided, so camera-facing faces cull and the
            // solid reads as see-through — a worse artefact than the one this
            // mode exists to reveal.
            None => match &primitive_material {
                Some(default_material) => {
                    entity_commands.insert(MeshMaterial3d::<StandardMaterial>(
                        default_material.0.clone(),
                    ));
                }
                None => {
                    entity_commands.remove::<MeshMaterial3d<StandardMaterial>>();
                }
            },
        }
    }
    // Materials assigned while the debug pass was active never reached the
    // renderer. Touching every assignment makes `apply_material_assignments`
    // recompute them on the next frame, so leaving debug mode always lands on
    // the authored state rather than on the stash.
    for mut assignment in assignments.iter_mut() {
        assignment.set_changed();
    }
}

/// A flat, unlit material for one resolved material id. Unlit so that shading
/// cannot blend two adjacent surfaces into looking like one.
fn flat_material(material_id: &str) -> StandardMaterial {
    let base_color = if material_id.is_empty() {
        UNFINISHED_COLOR
    } else {
        distinct_color(material_id)
    };
    StandardMaterial {
        base_color,
        unlit: true,
        double_sided: true,
        cull_mode: None,
        ..default()
    }
}

/// Deterministic, well-spread hue per material id, so the same model always
/// produces the same debug image and two different finishes never collide by
/// accident in the common case.
fn distinct_color(material_id: &str) -> Color {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in material_id.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Golden-ratio hue stepping keeps successive ids far apart on the wheel.
    let hue = ((hash % 360) as f32 * 0.618_034) % 1.0 * 360.0;
    Color::hsl(hue, 0.72, 0.55)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_material_is_magenta() {
        let material = flat_material("");
        assert_eq!(material.base_color, UNFINISHED_COLOR);
        assert!(material.unlit, "debug surfaces must not be shaded");
    }

    #[test]
    fn distinct_ids_get_distinct_colors() {
        let a = flat_material("falu_red");
        let b = flat_material("white_paint");
        assert_ne!(a.base_color, b.base_color);
        assert_ne!(a.base_color, UNFINISHED_COLOR);
        assert_ne!(b.base_color, UNFINISHED_COLOR);
    }

    #[test]
    fn colors_are_stable_across_runs() {
        assert_eq!(
            flat_material("falu_red").base_color,
            flat_material("falu_red").base_color
        );
    }

    // --- system-level behaviour ------------------------------------------
    //
    // The colour helpers above were green while the pass was visibly broken in
    // the app twice over: once because `apply_material_assignments` re-applied
    // authored materials over the debug colours, and once because the exit path
    // removed `MeshMaterial3d` outright and left solids see-through. Both live
    // in the ECS layer, so that is where they have to be tested.

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(bevy::asset::AssetPlugin::default())
            .init_asset::<StandardMaterial>()
            .init_asset::<Mesh>()
            .add_plugins(SurfaceDebugPlugin);
        app
    }

    fn spawn_mesh(app: &mut App, material: Option<&str>) -> Entity {
        let handle = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let mesh = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Mesh::from(bevy::math::primitives::Cuboid::new(
                1.0, 1.0, 1.0,
            )));
        let mut entity = app
            .world_mut()
            .spawn((Mesh3d(mesh), MeshMaterial3d(handle)));
        if let Some(material_id) = material {
            entity.insert(MaterialAssignment::new(material_id));
        }
        entity.id()
    }

    fn material_handle(app: &App, entity: Entity) -> Option<Handle<StandardMaterial>> {
        app.world()
            .get::<MeshMaterial3d<StandardMaterial>>(entity)
            .map(|material| material.0.clone())
    }

    fn set_enabled(app: &mut App, enabled: bool) {
        app.world_mut().resource_mut::<SurfaceDebugRender>().enabled = enabled;
        app.update();
    }

    #[test]
    fn enabling_repaints_finished_and_unfinished_meshes_differently() {
        let mut app = test_app();
        let finished = spawn_mesh(&mut app, Some("falu_red"));
        let unfinished = spawn_mesh(&mut app, None);
        app.update();

        set_enabled(&mut app, true);

        let finished_handle = material_handle(&app, finished).expect("finished keeps a material");
        let unfinished_handle =
            material_handle(&app, unfinished).expect("unfinished keeps a material");
        assert_ne!(
            finished_handle, unfinished_handle,
            "a finished and an unfinished surface must not share a debug colour"
        );

        let materials = app.world().resource::<Assets<StandardMaterial>>();
        assert_eq!(
            materials.get(&unfinished_handle).unwrap().base_color,
            UNFINISHED_COLOR,
            "an unfinished surface must be painted magenta"
        );
        assert!(materials.get(&finished_handle).unwrap().unlit);
    }

    #[test]
    fn disabling_restores_every_original_material() {
        let mut app = test_app();
        let finished = spawn_mesh(&mut app, Some("falu_red"));
        let unfinished = spawn_mesh(&mut app, None);
        app.update();
        let before = [
            material_handle(&app, finished).unwrap(),
            material_handle(&app, unfinished).unwrap(),
        ];

        set_enabled(&mut app, true);
        set_enabled(&mut app, false);

        assert_eq!(
            [
                material_handle(&app, finished).unwrap(),
                material_handle(&app, unfinished).unwrap(),
            ],
            before,
            "leaving the debug pass must restore the exact prior materials"
        );
    }

    /// Regression: an unfinished mesh must never come back from the debug pass
    /// with no material at all. A material-less mesh renders single-sided, so
    /// camera-facing faces cull and the solid reads as see-through.
    #[test]
    fn disabling_never_leaves_a_mesh_without_a_material() {
        let mut app = test_app();
        let unfinished = spawn_mesh(&mut app, None);
        app.update();

        set_enabled(&mut app, true);
        set_enabled(&mut app, false);

        assert!(
            app.world()
                .get::<MeshMaterial3d<StandardMaterial>>(unfinished)
                .is_some(),
            "restoring must leave a material behind, not strip the component"
        );
        assert!(
            app.world()
                .get::<SurfaceDebugOriginal>(unfinished)
                .is_none(),
            "the stash must be cleared on exit"
        );
    }
}
