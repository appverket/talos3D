//! GPU overlay for the selected face.
//!
//! SketchUp indicates the selected face with a fine screen-space dot raster.
//! The obvious way to do that with immediate-mode gizmos — emit one line or one
//! point per dot — is exactly the unbounded per-frame CPU path the platform
//! rules forbid: a face covering a 1200×800 viewport at a 4 px pitch is 60 000
//! primitives every frame, growing with zoom.
//!
//! So the raster is a fragment shader. The overlay is a single derived mesh
//! (the selected face polygon, ear-clipped) drawn with one alpha-blended
//! material whose fragment stage discards every pixel that is not on the dot
//! lattice. Cost is one draw call and is independent of zoom, face size and
//! tessellation. The mesh is rebuilt only when the selected face or its
//! geometry actually changes, so orbiting and idling cost nothing.
//!
//! The overlay carries no [`ElementId`]: like every other highlight it is a
//! derived artifact, invisible to the authored model, to persistence and to
//! the Model API.
//!
//! [`ElementId`]: crate::plugins::identity::ElementId

use bevy::{
    asset::{load_internal_asset, uuid_handle, RenderAssetUsages},
    light::{NotShadowCaster, NotShadowReceiver},
    material::AlphaMode,
    mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology},
    pbr::{MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    render::render_resource::{
        AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
    },
    shader::ShaderRef,
};

use crate::{
    capability_registry::FaceId,
    plugins::{
        drawing_export::HiddenDuringViewportExport,
        face_edit::{face_vertices_for_entity, FaceDrawingContext, FaceEditContext},
        modeling::triangulate::ear_clip_triangulate,
        tools::Preview,
    },
};

const FACE_STIPPLE_SHADER: Handle<Shader> = uuid_handle!("68b76a08-e5ba-46f5-aed0-a00938f2f758");

/// Matches the green the selected-face outline is drawn in.
const FACE_STIPPLE_COLOR: LinearRgba = LinearRgba::new(0.2, 1.0, 0.4, 0.85);
/// Dot lattice pitch and dot size, in physical pixels.
const FACE_STIPPLE_PITCH_PX: f32 = 4.0;
const FACE_STIPPLE_DOT_PX: f32 = 1.0;
/// Lift off the surface so the raster is not eaten by depth-test ties with the
/// face it decorates. Matches the offset the previous gizmo hatch used.
const FACE_STIPPLE_SURFACE_OFFSET: f32 = 0.004;

/// Screen-space dot raster fill.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct FaceStippleMaterial {
    #[uniform(0)]
    pub color: LinearRgba,
    /// `x` = dot pitch in physical pixels, `y` = dot size in physical pixels.
    #[uniform(1)]
    pub params: Vec4,
}

impl Default for FaceStippleMaterial {
    fn default() -> Self {
        Self {
            color: FACE_STIPPLE_COLOR,
            params: Vec4::new(FACE_STIPPLE_PITCH_PX, FACE_STIPPLE_DOT_PX, 0.0, 0.0),
        }
    }
}

impl Material for FaceStippleMaterial {
    fn fragment_shader() -> ShaderRef {
        FACE_STIPPLE_SHADER.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn depth_bias(&self) -> f32 {
        // Sorts the raster after other transparent geometry at the same depth.
        // Actually clearing the surface is the job of the geometric offset in
        // `face_stipple_mesh`; this only settles ties in the transparent phase.
        1.0
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The face polygon is emitted with the authored winding, which faces
        // away from the camera for half the faces of any solid. The highlight
        // must be visible on whichever face the user actually selected.
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

/// Marks the derived stipple overlay entity.
#[derive(Component, Debug, Clone, Copy)]
pub struct SelectedFaceStipple;

/// Identifies the geometry currently baked into the overlay mesh, so a frame
/// that changes nothing rebuilds nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FaceStippleKey {
    entity: Entity,
    face_id: FaceId,
    vertices: Vec<[i64; 3]>,
}

#[derive(Resource, Default)]
struct FaceStippleOverlay {
    entity: Option<Entity>,
    key: Option<FaceStippleKey>,
    material: Option<Handle<FaceStippleMaterial>>,
    mesh: Option<Handle<Mesh>>,
}

pub struct SubobjectOverlayPlugin;

impl Plugin for SubobjectOverlayPlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(
            app,
            FACE_STIPPLE_SHADER,
            "face_stipple.wgsl",
            Shader::from_wgsl
        );
        app.add_plugins(MaterialPlugin::<FaceStippleMaterial>::default())
            .init_resource::<FaceStippleOverlay>()
            .add_systems(Update, sync_selected_face_stipple);
    }
}

/// Keeps the derived stipple overlay in step with the selected face.
///
/// Exclusive because face geometry is resolved through `&World` (the face may
/// live on a primitive, an editable mesh, a CSG operand or a drawn feature) and
/// the overlay assets are written in the same step. Every path except "the
/// selected face changed" is an early return.
fn sync_selected_face_stipple(world: &mut World) {
    let target = selected_face_target(world);

    let Some((render_entity, face_id, normal)) = target else {
        clear_face_stipple(world);
        return;
    };

    let Some(vertices) = face_vertices_for_entity(world, render_entity, face_id) else {
        clear_face_stipple(world);
        return;
    };

    let key = FaceStippleKey {
        entity: render_entity,
        face_id,
        vertices: vertices.iter().map(quantize_vertex).collect(),
    };
    if world.resource::<FaceStippleOverlay>().key.as_ref() == Some(&key) {
        return;
    }

    let Some(mesh) = face_stipple_mesh(&vertices, normal) else {
        clear_face_stipple(world);
        return;
    };

    let mesh_handle = world.resource_mut::<Assets<Mesh>>().add(mesh);
    let material_handle = match world.resource::<FaceStippleOverlay>().material.clone() {
        Some(handle) => handle,
        None => world
            .resource_mut::<Assets<FaceStippleMaterial>>()
            .add(FaceStippleMaterial::default()),
    };

    let previous_mesh = world.resource::<FaceStippleOverlay>().mesh.clone();
    let overlay_entity = world.resource::<FaceStippleOverlay>().entity;
    let overlay_entity = match overlay_entity.filter(|entity| world.get_entity(*entity).is_ok()) {
        Some(entity) => {
            world
                .entity_mut(entity)
                .insert(Mesh3d(mesh_handle.clone()))
                .insert(MeshMaterial3d(material_handle.clone()));
            entity
        }
        None => world
            .spawn((
                Mesh3d(mesh_handle.clone()),
                MeshMaterial3d(material_handle.clone()),
                Transform::IDENTITY,
                Visibility::Visible,
                NotShadowCaster,
                NotShadowReceiver,
                SelectedFaceStipple,
                // The raster is instrument, not model: a clean viewport export
                // must not contain it, exactly as it does not contain gizmos.
                HiddenDuringViewportExport,
                // Derived, never authored: cleared with the rest of the
                // transient scene when a project is loaded.
                Preview,
                Name::new("Selected face stipple"),
            ))
            .id(),
    };

    if let Some(previous_mesh) = previous_mesh {
        if previous_mesh != mesh_handle {
            world.resource_mut::<Assets<Mesh>>().remove(&previous_mesh);
        }
    }

    let mut overlay = world.resource_mut::<FaceStippleOverlay>();
    overlay.entity = Some(overlay_entity);
    overlay.key = Some(key);
    overlay.material = Some(material_handle);
    overlay.mesh = Some(mesh_handle);
}

/// The face the overlay should currently be showing, if any.
fn selected_face_target(world: &World) -> Option<(Entity, FaceId, Vec3)> {
    if world.resource::<FaceDrawingContext>().active {
        // While drawing on a face the raster would compete with the sketch
        // preview, exactly as the previous hatch was suppressed.
        return None;
    }
    let face_context = world.resource::<FaceEditContext>();
    let selected = face_context.selected_face.as_ref()?;
    let render_entity = face_context
        .csg_operand_target
        .map(|(entity, _)| entity)
        .or(face_context.entity)?;
    Some((render_entity, selected.face_id, selected.normal))
}

fn clear_face_stipple(world: &mut World) {
    let (entity, mesh) = {
        let overlay = world.resource::<FaceStippleOverlay>();
        (overlay.entity, overlay.mesh.clone())
    };
    if entity.is_none() && mesh.is_none() {
        return;
    }
    if let Some(entity) = entity {
        if let Ok(entity_mut) = world.get_entity_mut(entity) {
            entity_mut.despawn();
        }
    }
    if let Some(mesh) = mesh {
        world.resource_mut::<Assets<Mesh>>().remove(&mesh);
    }
    let mut overlay = world.resource_mut::<FaceStippleOverlay>();
    overlay.entity = None;
    overlay.key = None;
    overlay.mesh = None;
}

/// Quantise to 0.1 mm so float noise in re-evaluated geometry does not look
/// like a change and rebuild the mesh every frame.
fn quantize_vertex(vertex: &Vec3) -> [i64; 3] {
    [
        (vertex.x as f64 * 10_000.0).round() as i64,
        (vertex.y as f64 * 10_000.0).round() as i64,
        (vertex.z as f64 * 10_000.0).round() as i64,
    ]
}

/// Builds the overlay mesh for a face polygon given in world space.
pub(crate) fn face_stipple_mesh(vertices: &[Vec3], normal: Vec3) -> Option<Mesh> {
    if vertices.len() < 3 {
        return None;
    }
    let normal = normal.normalize_or_zero();
    if normal == Vec3::ZERO {
        return None;
    }

    let (tangent, bitangent) = plane_basis(normal);
    let origin = vertices[0];
    let planar: Vec<Vec2> = vertices
        .iter()
        .map(|vertex| {
            let offset = *vertex - origin;
            Vec2::new(offset.dot(tangent), offset.dot(bitangent))
        })
        .collect();

    // Ear clipping wants counter-clockwise input; the face may be wound either
    // way relative to the basis, and the overlay is drawn without backface
    // culling, so flipping the index order is enough.
    let mut triangles = ear_clip_triangulate(&planar);
    if triangles.is_empty() {
        let reversed: Vec<Vec2> = planar.iter().rev().copied().collect();
        let last = (planar.len() - 1) as u32;
        triangles = ear_clip_triangulate(&reversed)
            .into_iter()
            .map(|[a, b, c]| [last - a, last - b, last - c])
            .collect();
    }
    if triangles.is_empty() {
        return None;
    }

    let positions: Vec<[f32; 3]> = vertices
        .iter()
        .map(|vertex| (*vertex + normal * FACE_STIPPLE_SURFACE_OFFSET).to_array())
        .collect();
    let normals = vec![normal.to_array(); positions.len()];
    let indices: Vec<u32> = triangles.into_iter().flatten().collect();

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
}

fn plane_basis(normal: Vec3) -> (Vec3, Vec3) {
    let reference = if normal.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
    let tangent = normal.cross(reference).normalize();
    (tangent, tangent.cross(normal).normalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        plugins::{
            face_edit::SelectedFace,
            identity::ElementId,
            modeling::primitives::{BoxPrimitive, ShapeRotation},
        },
    };

    /// The real sync system driving a real face-edit selection — the overlay is
    /// presentation machinery, so it is tested through the system that runs it,
    /// not only through the mesh builder.
    fn overlay_app() -> (App, Entity) {
        let mut app = App::new();
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<FaceStippleMaterial>>()
            .init_resource::<FaceEditContext>()
            .init_resource::<FaceDrawingContext>()
            .init_resource::<FaceStippleOverlay>()
            .add_systems(Update, sync_selected_face_stipple);

        let entity = app
            .world_mut()
            .spawn((
                BoxPrimitive {
                    centre: Vec3::ZERO,
                    half_extents: Vec3::splat(1.0),
                },
                ShapeRotation::default(),
            ))
            .id();
        let mut face_context = app.world_mut().resource_mut::<FaceEditContext>();
        face_context.entity = Some(entity);
        face_context.element_id = Some(ElementId(11));
        (app, entity)
    }

    fn select_top_face(app: &mut App) {
        app.world_mut()
            .resource_mut::<FaceEditContext>()
            .selected_face = Some(SelectedFace {
            // Box face 3 is +Y.
            face_id: FaceId(3),
            generated_face_ref: None,
            normal: Vec3::Y,
            centroid: Vec3::Y,
        });
    }

    fn overlay_entities(app: &mut App) -> Vec<Entity> {
        app.world_mut()
            .query_filtered::<Entity, With<SelectedFaceStipple>>()
            .iter(app.world())
            .collect()
    }

    #[test]
    fn selecting_a_face_spawns_one_stipple_overlay_covering_it() {
        let (mut app, _) = overlay_app();
        app.update();
        assert!(
            overlay_entities(&mut app).is_empty(),
            "no selected face, no overlay"
        );

        select_top_face(&mut app);
        app.update();

        let overlays = overlay_entities(&mut app);
        assert_eq!(overlays.len(), 1);

        let mesh_handle = app
            .world()
            .get::<Mesh3d>(overlays[0])
            .expect("overlay carries a mesh")
            .0
            .clone();
        let meshes = app.world().resource::<Assets<Mesh>>();
        let mesh = meshes.get(&mesh_handle).expect("overlay mesh exists");
        assert_eq!(triangle_count(mesh), 2, "the top of a box is one quad");
    }

    #[test]
    fn the_overlay_is_not_an_authored_element() {
        let (mut app, _) = overlay_app();
        select_top_face(&mut app);
        app.update();

        let overlay = overlay_entities(&mut app)[0];
        assert!(
            app.world().get::<ElementId>(overlay).is_none(),
            "the highlight must stay invisible to the authored model and the Model API"
        );
    }

    #[test]
    fn a_steady_selection_does_not_rebuild_the_overlay_mesh() {
        let (mut app, _) = overlay_app();
        select_top_face(&mut app);
        app.update();
        let first = app.world().resource::<FaceStippleOverlay>().mesh.clone();

        app.update();
        app.update();

        assert_eq!(
            app.world().resource::<FaceStippleOverlay>().mesh,
            first,
            "orbiting and idling must not re-triangulate the face"
        );
    }

    #[test]
    fn moving_the_face_rebuilds_the_overlay() {
        let (mut app, entity) = overlay_app();
        select_top_face(&mut app);
        app.update();
        let first = app.world().resource::<FaceStippleOverlay>().mesh.clone();

        app.world_mut()
            .entity_mut(entity)
            .get_mut::<BoxPrimitive>()
            .expect("box primitive")
            .half_extents = Vec3::new(1.0, 3.0, 1.0);
        app.update();

        assert_ne!(
            app.world().resource::<FaceStippleOverlay>().mesh,
            first,
            "a push/pull must drag the highlight along with the face"
        );
    }

    #[test]
    fn deselecting_removes_the_overlay_and_its_mesh() {
        let (mut app, _) = overlay_app();
        select_top_face(&mut app);
        app.update();
        let mesh_handle = app
            .world()
            .resource::<FaceStippleOverlay>()
            .mesh
            .clone()
            .expect("overlay mesh");

        app.world_mut()
            .resource_mut::<FaceEditContext>()
            .selected_face = None;
        app.update();

        assert!(overlay_entities(&mut app).is_empty());
        assert!(
            app.world().resource::<Assets<Mesh>>().get(&mesh_handle).is_none(),
            "the derived mesh must not outlive the highlight"
        );
    }

    #[test]
    fn drawing_on_a_face_suppresses_the_raster() {
        let (mut app, _) = overlay_app();
        select_top_face(&mut app);
        app.update();
        assert_eq!(overlay_entities(&mut app).len(), 1);

        app.world_mut()
            .resource_mut::<FaceDrawingContext>()
            .active = true;
        app.update();

        assert!(
            overlay_entities(&mut app).is_empty(),
            "the sketch preview owns the face while drawing"
        );
    }

    fn triangle_count(mesh: &Mesh) -> usize {
        match mesh.indices() {
            Some(Indices::U32(indices)) => indices.len() / 3,
            _ => 0,
        }
    }

    #[test]
    fn quad_face_becomes_two_triangles_lifted_off_the_surface() {
        let quad = [
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(2.0, 1.0, 3.0),
            Vec3::new(0.0, 1.0, 3.0),
        ];
        let mesh = face_stipple_mesh(&quad, Vec3::Y).expect("a quad is triangulable");
        assert_eq!(triangle_count(&mesh), 2);

        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) => positions.clone(),
            _ => panic!("overlay mesh must carry positions"),
        };
        assert_eq!(positions.len(), 4);
        for position in positions {
            assert!(
                (position[1] - (1.0 + FACE_STIPPLE_SURFACE_OFFSET)).abs() < 1e-6,
                "overlay must sit just off the face plane, got {position:?}"
            );
        }
    }

    #[test]
    fn clockwise_face_still_triangulates() {
        // Faces are wound consistently with their outward normal, so half the
        // faces of any solid arrive clockwise in the plane basis.
        let quad = [
            Vec3::new(0.0, 1.0, 3.0),
            Vec3::new(2.0, 1.0, 3.0),
            Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let mesh = face_stipple_mesh(&quad, Vec3::Y).expect("winding must not matter");
        assert_eq!(triangle_count(&mesh), 2);
    }

    #[test]
    fn concave_face_is_fully_covered() {
        // An L-shaped cap, as produced by a drawn profile extrusion.
        let polygon = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(3.0, 0.0, 0.0),
            Vec3::new(3.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 3.0),
            Vec3::new(0.0, 0.0, 3.0),
        ];
        let mesh = face_stipple_mesh(&polygon, Vec3::Y).expect("concave caps are triangulable");
        assert_eq!(
            triangle_count(&mesh),
            polygon.len() - 2,
            "a simple polygon triangulates into n-2 triangles"
        );
    }

    #[test]
    fn degenerate_faces_produce_no_overlay() {
        assert!(face_stipple_mesh(&[Vec3::ZERO, Vec3::X], Vec3::Y).is_none());
        assert!(face_stipple_mesh(
            &[Vec3::ZERO, Vec3::X, Vec3::X * 2.0],
            Vec3::Y
        )
        .is_none());
    }
}
