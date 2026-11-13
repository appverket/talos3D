//! Export the model's 3D geometry to an interchange format.
//!
//! The Exchange menu could only produce 2D drawings (PNG/SVG/DXF/PDF), so a
//! model could be authored here but never handed to another 3D tool. This adds
//! the 3D direction.
//!
//! Geometry is taken from the generated render meshes rather than re-derived
//! from snapshots, so what is exported is exactly what the viewport shows —
//! including anything a capability produced procedurally.
//!
//! Wavefront OBJ is the first format: plain text, no dependencies, and read by
//! every 3D tool. The writer is factored so another format only has to supply a
//! serializer over the same collected [`ExportMesh`] list.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use serde_json::Value;

use crate::plugins::{
    command_registry::{CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult},
    definition_preview_scene::PreviewOnly,
    identity::ElementId,
    layers::entity_document_hidden,
    modeling::occurrence::GeneratedOccurrencePart,
    ui::StatusBarData,
};

pub mod glb;

pub struct ModelExportPlugin;

impl Plugin for ModelExportPlugin {
    fn build(&self, app: &mut App) {
        app.register_command(
            CommandDescriptor {
                id: "core.export_model_obj".to_string(),
                label: "Export Model as OBJ...".to_string(),
                description: "Export the model's 3D geometry as a Wavefront OBJ file.".to_string(),
                category: CommandCategory::File,
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Destination .obj path. Defaults to the document \
                                            name beside the current document."
                        }
                    }
                })),
                default_shortcut: None,
                icon: None,
                hint: Some("Export 3D geometry as Wavefront OBJ".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: None,
            },
            execute_export_model_obj,
        );
    }
}

/// One mesh in world space, ready to serialize.
#[derive(Debug, Clone)]
pub struct ExportMesh {
    /// Object name in the exported file — the element id keeps it traceable
    /// back to the authored model.
    pub name: String,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    /// Triangle indices into `positions`.
    pub indices: Vec<[u32; 3]>,
}

/// Collect every visible render mesh, baked into world space.
pub fn collect_export_meshes(world: &mut World) -> Vec<ExportMesh> {
    let entries: Vec<(Option<ElementId>, Handle<Mesh>, GlobalTransform)> = {
        let mut query = world.query::<(
            Option<&ElementId>,
            &Mesh3d,
            &GlobalTransform,
            &ViewVisibility,
        )>();
        query
            .iter(world)
            .filter(|(_, _, _, visibility)| visibility.get())
            .map(|(element_id, mesh, transform, _)| {
                (element_id.copied(), mesh.0.clone(), *transform)
            })
            .collect()
    };

    let assets = world.resource::<Assets<Mesh>>();
    entries
        .into_iter()
        .filter_map(|(element_id, handle, transform)| {
            let mesh = assets.get(&handle)?;
            let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION)? {
                VertexAttributeValues::Float32x3(values) => values,
                _ => return None,
            };
            let normals = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
                Some(VertexAttributeValues::Float32x3(values)) => values.clone(),
                _ => Vec::new(),
            };
            let indices: Vec<u32> = match mesh.indices()? {
                Indices::U16(values) => values.iter().map(|index| *index as u32).collect(),
                Indices::U32(values) => values.clone(),
            };
            let affine = transform.affine();
            Some(ExportMesh {
                name: element_id
                    .map(|id| format!("element_{}", id.0))
                    .unwrap_or_else(|| "mesh".to_string()),
                positions: positions
                    .iter()
                    .map(|p| affine.transform_point3(Vec3::from_array(*p)))
                    .collect(),
                normals: normals
                    .iter()
                    .map(|n| {
                        affine
                            .transform_vector3(Vec3::from_array(*n))
                            .normalize_or_zero()
                    })
                    .collect(),
                indices: indices
                    .chunks_exact(3)
                    .map(|chunk| [chunk[0], chunk[1], chunk[2]])
                    .collect(),
            })
        })
        .filter(|mesh| !mesh.indices.is_empty())
        .collect()
}

/// Authored elements whose generated geometry may leave the app: visible, not
/// excluded by the document, and not a preview. Owners may be logical nodes
/// with no inherited visibility component, so absence counts as visible.
fn visible_authored_owners(world: &mut World) -> HashSet<ElementId> {
    world
        .query::<(Entity, &ElementId, Option<&InheritedVisibility>)>()
        .iter(world)
        .filter(|(entity, _, visibility)| {
            visibility.is_none_or(|visibility| visibility.get())
                && !entity_document_hidden(world, *entity)
                && world.get::<PreviewOnly>(*entity).is_none()
        })
        .map(|(_, id, _)| *id)
        .collect()
}

/// The visible authored element that owns render entity `entity`, found
/// through generated-part ownership and then ancestry. `None` for helpers
/// (grid, grips, previews) and for hidden elements. Compound occurrences emit
/// detached meshes that carry their owner in [`GeneratedOccurrencePart`].
fn visible_authored_owner(
    world: &World,
    entity: Entity,
    visible: &HashSet<ElementId>,
) -> Option<ElementId> {
    let mut current = Some(entity);
    while let Some(entity) = current {
        if world.get::<PreviewOnly>(entity).is_some() {
            return None;
        }
        if let Some(id) = world
            .get::<GeneratedOccurrencePart>(entity)
            .map(|part| part.owner)
            .or_else(|| world.get::<ElementId>(entity).copied())
        {
            return visible.contains(&id).then_some(id);
        }
        current = world.get::<ChildOf>(entity).map(|parent| parent.parent());
    }
    None
}

/// Every visible render mesh of one authored element, merged and baked into
/// world space (Y-up, metres, f64). Domain-neutral: formats that need to know
/// *which element* a triangle belongs to (IFC) consume this; OBJ keeps using
/// [`collect_export_meshes`].
#[derive(Debug, Clone, PartialEq)]
pub struct ElementMesh {
    pub owner: ElementId,
    pub positions: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
    /// sRGB base colour of the first part that has a standard material.
    pub color: Option<[f32; 3]>,
}

/// Collect geometry grouped by owning authored element, ordered by element id.
///
/// Fails rather than silently dropping geometry when a mesh asset is missing or
/// malformed, so an export can never be quietly incomplete.
pub fn collect_element_meshes(world: &mut World) -> Result<Vec<ElementMesh>, String> {
    let visible = visible_authored_owners(world);
    let entries: Vec<_> = world
        .query::<(
            Entity,
            &Mesh3d,
            &GlobalTransform,
            &InheritedVisibility,
            Option<&MeshMaterial3d<StandardMaterial>>,
        )>()
        .iter(world)
        .filter(|(_, _, _, visibility, _)| visibility.get())
        .map(|(entity, mesh, transform, _, material)| {
            (
                entity,
                mesh.0.clone(),
                *transform,
                material.map(|material| material.0.clone()),
            )
        })
        .collect();

    let meshes = world.resource::<Assets<Mesh>>();
    let materials = world.get_resource::<Assets<StandardMaterial>>();
    let mut by_owner: BTreeMap<u64, ElementMesh> = BTreeMap::new();
    for (entity, handle, transform, material) in entries {
        let Some(owner) = visible_authored_owner(world, entity, &visible) else {
            continue;
        };
        let Some(mesh) = meshes.get(&handle) else {
            return Err("Model geometry is still loading".to_string());
        };
        if mesh.primitive_topology() != PrimitiveTopology::TriangleList {
            continue;
        }
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            continue;
        };
        let indices: Vec<u32> = match mesh.indices() {
            Some(Indices::U16(values)) => values.iter().map(|index| u32::from(*index)).collect(),
            Some(Indices::U32(values)) => values.clone(),
            None => (0..positions.len() as u32).collect(),
        };
        if indices.is_empty() {
            continue;
        }
        if indices.len() % 3 != 0
            || indices
                .iter()
                .any(|index| *index as usize >= positions.len())
        {
            return Err("Invalid model triangles".to_string());
        }
        let affine = transform.affine();
        // A mirroring transform flips winding; undo it so faces stay outward.
        let mirrored = affine.matrix3.determinant() < 0.0;
        let merged = by_owner.entry(owner.0).or_insert_with(|| ElementMesh {
            owner,
            positions: Vec::new(),
            triangles: Vec::new(),
            color: None,
        });
        if merged.color.is_none() {
            merged.color = material
                .as_ref()
                .and_then(|handle| materials.and_then(|assets| assets.get(handle)))
                .map(|material| {
                    let color = material.base_color.to_srgba();
                    [color.red, color.green, color.blue]
                });
        }
        let base = merged.positions.len() as u32;
        for position in positions {
            let point = affine.transform_point3(Vec3::from_array(*position));
            if !point.is_finite() {
                return Err("Cannot export non-finite geometry".to_string());
            }
            merged
                .positions
                .push([f64::from(point.x), f64::from(point.y), f64::from(point.z)]);
        }
        for triangle in indices.chunks_exact(3) {
            let [a, b, c] = [triangle[0], triangle[1], triangle[2]];
            merged.triangles.push(if mirrored {
                [base + a, base + c, base + b]
            } else {
                [base + a, base + b, base + c]
            });
        }
    }
    Ok(by_owner.into_values().collect())
}

/// Serialize meshes as Wavefront OBJ.
///
/// OBJ indices are 1-based and file-global, so each mesh's vertices are offset
/// by the running total rather than restarting per object.
pub fn write_obj(meshes: &[ExportMesh]) -> String {
    let mut out = String::from("# Exported by Talos3D\n");
    let mut position_base: u32 = 1;
    let mut normal_base: u32 = 1;
    for mesh in meshes {
        let _ = writeln!(out, "o {}", mesh.name);
        for position in &mesh.positions {
            let _ = writeln!(out, "v {} {} {}", position.x, position.y, position.z);
        }
        for normal in &mesh.normals {
            let _ = writeln!(out, "vn {} {} {}", normal.x, normal.y, normal.z);
        }
        let has_normals = mesh.normals.len() == mesh.positions.len();
        for [a, b, c] in &mesh.indices {
            if has_normals {
                let _ = writeln!(
                    out,
                    "f {}//{} {}//{} {}//{}",
                    position_base + a,
                    normal_base + a,
                    position_base + b,
                    normal_base + b,
                    position_base + c,
                    normal_base + c
                );
            } else {
                let _ = writeln!(
                    out,
                    "f {} {} {}",
                    position_base + a,
                    position_base + b,
                    position_base + c
                );
            }
        }
        position_base += mesh.positions.len() as u32;
        normal_base += mesh.normals.len() as u32;
    }
    out
}

pub fn export_model_obj_to_path(world: &mut World, path: PathBuf) -> Result<PathBuf, String> {
    let meshes = collect_export_meshes(world);
    if meshes.is_empty() {
        return Err("The model has no 3D geometry to export".to_string());
    }
    let contents = write_obj(&meshes);
    std::fs::write(&path, contents).map_err(|error| error.to_string())?;
    Ok(path)
}

fn execute_export_model_obj(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let path = match parameters.get("path").and_then(Value::as_str) {
        Some(path) => PathBuf::from(path),
        None => default_export_path(world)?,
    };
    let written = export_model_obj_to_path(world, path)?;
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        status.set_feedback(format!("Exported {}", written.display()), 3.0);
    }
    Ok(CommandResult {
        output: Some(serde_json::json!({ "path": written.to_string_lossy() })),
        ..Default::default()
    })
}

/// Beside the current document, with the extension swapped — so a menu click
/// with no arguments still lands somewhere predictable.
fn default_export_path(world: &World) -> Result<PathBuf, String> {
    let doc_state = world
        .get_resource::<crate::plugins::document_state::DocumentState>()
        .ok_or_else(|| "No document state".to_string())?;
    let current = doc_state
        .current_path
        .as_ref()
        .ok_or_else(|| "Save the document first, or pass an explicit path".to_string())?;
    Ok(with_obj_extension(current))
}

fn with_obj_extension(path: &Path) -> PathBuf {
    let mut path = path.to_path_buf();
    path.set_extension("obj");
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> ExportMesh {
        ExportMesh {
            name: "element_7".to_string(),
            positions: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
            ],
            normals: vec![Vec3::Z, Vec3::Z, Vec3::Z],
            indices: vec![[0, 1, 2]],
        }
    }

    #[test]
    fn obj_faces_are_one_based() {
        let obj = write_obj(&[quad()]);
        assert!(obj.contains("o element_7"));
        assert!(obj.contains("v 0 0 0"));
        assert!(
            obj.contains("f 1//1 2//2 3//3"),
            "OBJ indices start at 1, got:\n{obj}"
        );
    }

    /// OBJ indices are file-global, so a second object must continue numbering
    /// rather than restart — the classic way an exporter produces a mangled file.
    #[test]
    fn second_object_continues_the_global_vertex_numbering() {
        let mut second = quad();
        second.name = "element_8".to_string();
        let obj = write_obj(&[quad(), second]);
        assert!(obj.contains("f 4//4 5//5 6//6"), "got:\n{obj}");
    }

    #[test]
    fn meshes_without_normals_emit_position_only_faces() {
        let mut mesh = quad();
        mesh.normals.clear();
        let obj = write_obj(&[mesh]);
        assert!(obj.contains("f 1 2 3"), "got:\n{obj}");
        assert!(!obj.contains("//"));
    }

    #[test]
    fn export_path_swaps_the_document_extension() {
        assert_eq!(
            with_obj_extension(Path::new("/projects/house.talos3d")),
            PathBuf::from("/projects/house.obj")
        );
    }
    fn centered_outward(mesh: &ElementMesh) -> bool {
        let count = mesh.positions.len() as f64;
        let center = mesh.positions.iter().fold([0.0; 3], |mut acc, p| {
            for axis in 0..3 {
                acc[axis] += p[axis] / count;
            }
            acc
        });
        mesh.triangles.iter().all(|[a, b, c]| {
            let (a, b, c) = (
                mesh.positions[*a as usize],
                mesh.positions[*b as usize],
                mesh.positions[*c as usize],
            );
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let outward = [
                (a[0] + b[0] + c[0]) / 3.0 - center[0],
                (a[1] + b[1] + c[1]) / 3.0 - center[1],
                (a[2] + b[2] + c[2]) / 3.0 - center[2],
            ];
            n[0] * outward[0] + n[1] * outward[1] + n[2] * outward[2] > 0.0
        })
    }

    #[test]
    fn element_meshes_group_by_authored_owner_in_world_space() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<StandardMaterial>>();
        let mesh = world
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(2., 2., 2.));
        let material = world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color: Color::srgb(0.2, 0.4, 0.6),
                ..default()
            });
        let owner = world.spawn(ElementId(42)).id();
        for x in [0., 4.] {
            world.spawn((
                ChildOf(owner),
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                GlobalTransform::from(Transform::from_xyz(x, 2., 0.)),
                InheritedVisibility::VISIBLE,
            ));
        }
        // Helper without an authored ancestor, and a hidden element: both excluded.
        world.spawn((
            Mesh3d(mesh.clone()),
            GlobalTransform::IDENTITY,
            InheritedVisibility::VISIBLE,
        ));
        world.spawn((
            ElementId(99),
            Mesh3d(mesh),
            GlobalTransform::IDENTITY,
            InheritedVisibility::HIDDEN,
        ));

        let meshes = collect_element_meshes(&mut world).unwrap();
        assert_eq!(meshes.len(), 1);
        let merged = &meshes[0];
        assert_eq!(merged.owner, ElementId(42));
        assert_eq!(merged.positions.len(), 48);
        assert_eq!(merged.triangles.len(), 24);
        let max_x = merged
            .positions
            .iter()
            .map(|p| p[0])
            .fold(f64::MIN, f64::max);
        assert_eq!(max_x, 5.0);
        let color = merged.color.expect("material colour");
        assert!((color[0] - 0.2).abs() < 1e-4 && (color[2] - 0.6).abs() < 1e-4);
    }

    #[test]
    fn mirrored_transforms_keep_faces_outward() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        let mesh = world
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(2., 2., 2.));
        world.spawn((
            ElementId(7),
            Mesh3d(mesh),
            GlobalTransform::from(Transform::from_scale(Vec3::new(-1., 1., 1.))),
            InheritedVisibility::VISIBLE,
        ));
        let meshes = collect_element_meshes(&mut world).unwrap();
        assert!(centered_outward(&meshes[0]));
    }

    #[test]
    fn a_missing_mesh_asset_is_an_error_not_a_silent_gap() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.spawn((
            ElementId(1),
            Mesh3d(Handle::<Mesh>::default()),
            GlobalTransform::IDENTITY,
            InheritedVisibility::VISIBLE,
        ));
        assert!(collect_element_meshes(&mut world)
            .unwrap_err()
            .contains("still loading"));
    }
}
