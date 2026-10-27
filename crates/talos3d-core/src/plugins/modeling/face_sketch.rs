//! Agent-first commands for planar profiles. Pointer tools call the same builders.
use super::{
    planar_body::PlanarBody,
    profile::{Profile2d, ProfileSegment},
    profile_feature::{make_face_profile_feature_snapshot, FaceProfileFeatureFactory},
};
use crate::capability_registry::AuthoredEntityFactory;
use crate::{
    authored_entity::scalar_from_json,
    capability_registry::{FaceHitCandidate, FaceId, SubobjectSelection},
    plugins::{
        command_registry::{
            CommandCategory, CommandDescriptor, CommandHandler, CommandRegistryAppExt,
            CommandResult,
        },
        commands::{
            enqueue_apply_entity_changes, enqueue_create_world_entity,
            find_entity_by_element_id_readonly, ApplyEntityChangesCommand,
        },
        cursor::DrawingPlane,
        face_edit::{face_vertices_for_entity, FaceDrawingContext, FaceEditContext, SelectedFace},
        identity::{ElementId, ElementIdAllocator},
        selection::Selected,
    },
};
use bevy::prelude::*;
use serde_json::{json, Value};

#[derive(Resource, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum FaceSketchShape {
    #[default]
    Polyline,
    Circle,
    Rectangle,
}
#[derive(Resource, Default)]
pub struct PendingFaceRegion(pub Option<ElementId>);

pub fn register(app: &mut App) {
    app.init_resource::<FaceSketchShape>()
        .init_resource::<PendingFaceRegion>();
    let commands: [(&str,&str,&str,CommandHandler,bool);6] = [
        ("inspect_faces","Inspect Planar Faces","Inspect an authored object's planar surfaces and drawing frames before selecting or drawing.", inspect_faces,false),
        ("select_face","Select Face","Select an object's planar face by element_id and face_id; establishes the drawing plane.", select_face,false),
        ("draw_face_circle","Draw Circle on Face","Click centre and radius on the selected face, or supply centre [u,v] and radius in its drawing frame. Creates an authored zero-depth circular region.", circle,true),
        ("draw_face_rectangle","Draw Rectangle on Face","Click opposite corners, or supply a and b in selected face coordinates. Creates an authored zero-depth region.", rectangle,true),
        ("draw_face_polyline","Draw Lines on Face","Draw a face-constrained outline. Supply points [u,v]; closed profiles can be pushed or pulled.", polyline,true),
        ("set_face_depth","Set Face Region Depth","Set absolute signed depth in metres: positive protrusion, zero flush, negative recess or through-cut. Preserves supporting authored object.", set_depth,false),
    ];
    for (id, label, description, handler, menu) in commands {
        let properties = match id {
            "inspect_faces" => json!({"element_id":{"type":"integer"}}),
            "select_face" => json!({"element_id":{"type":"integer"},"face_id":{"type":"integer"}}),
            "set_face_depth" => json!({"element_id":{"type":"integer"},"depth":{"type":"number"}}),
            "draw_face_circle" => {
                json!({"element_id":{"type":"integer"},"face_id":{"type":"integer"},"centre":{"type":"array","items":{"type":"number"},"minItems":2,"maxItems":2},"radius":{"type":"number","exclusiveMinimum":0}})
            }
            "draw_face_rectangle" => {
                json!({"element_id":{"type":"integer"},"face_id":{"type":"integer"},"a":{"type":"array","items":{"type":"number"},"minItems":2,"maxItems":2},"b":{"type":"array","items":{"type":"number"},"minItems":2,"maxItems":2}})
            }
            _ => {
                json!({"element_id":{"type":"integer"},"face_id":{"type":"integer"},"points":{"type":"array","minItems":2,"maxItems":257,"items":{"type":"array","items":{"type":"number"},"minItems":2,"maxItems":2}}})
            }
        };
        app.register_command(
            CommandDescriptor {
                id: format!("modeling.{id}"),
                label: label.into(),
                description: description.into(),
                category: if menu {
                    CommandCategory::Create
                } else {
                    CommandCategory::Edit
                },
                parameters: Some(
                    json!({"type":"object","properties":properties,"additionalProperties":false}),
                ),
                default_shortcut: None,
                icon: None,
                hint: Some(description.into()),
                requires_selection: false,
                show_in_menu: menu,
                version: 1,
                activates_tool: None,
                capability_id: Some("modeling".into()),
            },
            handler,
        );
    }
}

pub fn select_hit(world: &mut World, hit: FaceHitCandidate) {
    world.init_resource::<SubobjectSelection>();
    world.init_resource::<DrawingPlane>();
    world
        .resource_mut::<FaceEditContext>()
        .enter(hit.entity, hit.element_id);
    world.resource_mut::<FaceEditContext>().selected_face = Some(SelectedFace {
        face_id: hit.face_id,
        generated_face_ref: hit.generated_face_ref.clone(),
        normal: hit.normal,
        centroid: hit.centroid,
    });
    world.resource_mut::<SubobjectSelection>().refs =
        crate::plugins::face_edit::face_selection_refs(hit.element_id, &hit, 1);
    *world.resource_mut::<DrawingPlane>() = DrawingPlane::from_face(hit.centroid, hit.normal);
    world.entity_mut(hit.entity).remove::<Selected>();
}

fn target(world: &World, args: &Value) -> Result<(Entity, ElementId), String> {
    let id = args
        .get("element_id")
        .and_then(Value::as_u64)
        .map(ElementId)
        .or_else(|| {
            world
                .get_resource::<FaceEditContext>()
                .and_then(|c| c.element_id)
        })
        .ok_or("Supply element_id or select an object face")?;
    Ok((
        find_entity_by_element_id_readonly(world, id).ok_or("Object no longer exists")?,
        id,
    ))
}
fn inspect_faces(world: &mut World, args: &Value) -> Result<CommandResult, String> {
    let (entity, id) = target(world, args)?;
    let mut faces = Vec::new();
    // Capabilities supply cached planar topology; primitives retain their semantic faces.
    let count = world
        .get::<PlanarBody>(entity)
        .map(|b| b.surfaces.len())
        .unwrap_or(256);
    for i in 0..count {
        if let Some(vertices) = face_vertices_for_entity(world, entity, FaceId(i as u32)) {
            if vertices.len() < 3 {
                continue;
            }
            let normal = world
                .get::<PlanarBody>(entity)
                .and_then(|b| b.surfaces.get(i))
                .map(|f| f.normal)
                .unwrap_or_else(|| {
                    (vertices[1] - vertices[0])
                        .cross(vertices[2] - vertices[0])
                        .normalize_or_zero()
                });
            if normal == Vec3::ZERO {
                continue;
            }
            let origin = world
                .get::<PlanarBody>(entity)
                .and_then(|b| b.surfaces.get(i))
                .map(|f| f.origin)
                .unwrap_or(vertices[0]);
            let (origin, normal) = if i == 0 {
                if let Some(f) = world.get::<super::profile_feature::FaceProfileFeature>(entity) {
                    let n = world
                        .get::<super::primitives::ShapeRotation>(entity)
                        .copied()
                        .unwrap_or_default()
                        .0
                        * Vec3::Y;
                    (f.anchor_origin + n * f.depth, n)
                } else {
                    (origin, normal)
                }
            } else {
                (origin, normal)
            };
            let plane = DrawingPlane::from_face(origin, normal);
            faces.push(json!({"face_id":i,"surface_ref":world.get::<PlanarBody>(entity).and_then(|b|b.surfaces.get(i)).and_then(|s|s.reference.clone()),"origin":plane.origin.to_array(),"normal":plane.normal.to_array(),"boundary":vertices.iter().map(|p|plane.project_to_2d(*p).to_array()).collect::<Vec<_>>(),"u_axis":plane.tangent.to_array(),"v_axis":plane.bitangent.to_array(),"next_command":"modeling.select_face"}));
        }
    }
    Ok(CommandResult {
        output: Some(json!({"element_id":id.0,"faces":faces})),
        ..Default::default()
    })
}
fn select_face(world: &mut World, args: &Value) -> Result<CommandResult, String> {
    let (entity, id) = target(world, args)?;
    let face = FaceId(
        args.get("face_id")
            .and_then(Value::as_u64)
            .ok_or("Supply face_id from modeling.inspect_faces")? as u32,
    );
    let vertices = face_vertices_for_entity(world, entity, face)
        .ok_or("Unsupported or missing planar face")?;
    if vertices.len() < 3 {
        return Err("Face has no planar region".into());
    }
    let normal = world
        .get::<PlanarBody>(entity)
        .and_then(|b| b.surfaces.get(face.0 as usize))
        .map(|s| s.normal)
        .unwrap_or_else(|| {
            (vertices[1] - vertices[0])
                .cross(vertices[2] - vertices[0])
                .normalize_or_zero()
        });
    if normal == Vec3::ZERO {
        return Err("Degenerate face".into());
    }
    let centroid = world
        .get::<PlanarBody>(entity)
        .and_then(|b| b.surfaces.get(face.0 as usize))
        .map(|f| f.origin)
        .unwrap_or(vertices[0]);
    let feature = world.get::<super::profile_feature::FaceProfileFeature>(entity);
    let generated_face_ref = feature
        .and_then(|f| f.generated_face_ref(face))
        .or_else(|| {
            world
                .get::<PlanarBody>(entity)
                .and_then(|b| b.surfaces.get(face.0 as usize))
                .and_then(|s| s.reference.clone())
        });
    let normal = if feature.is_some() && face == FaceId(0) {
        world
            .get::<super::primitives::ShapeRotation>(entity)
            .copied()
            .unwrap_or_default()
            .0
            * Vec3::Y
    } else {
        normal
    };
    let centroid = feature
        .filter(|_| face == FaceId(0))
        .map(|f| f.anchor_origin + normal * f.depth)
        .unwrap_or(centroid);
    select_hit(
        world,
        FaceHitCandidate {
            entity,
            element_id: id,
            distance: 0.,
            face_id: face,
            generated_face_ref,
            normal,
            centroid,
        },
    );
    Ok(CommandResult {
        output: Some(
            json!({"selected":id.0,"face_id":face.0,"next_commands":["modeling.draw_face_circle","modeling.draw_face_rectangle","modeling.draw_face_polyline"]}),
        ),
        ..Default::default()
    })
}
fn prepare(world: &mut World, args: &Value, shape: FaceSketchShape) -> Result<bool, String> {
    if args.get("element_id").is_some() && args.get("face_id").is_none() {
        return Err("Supply face_id with the support element_id".into());
    }
    if args.get("face_id").is_some() {
        select_face(world, args)?;
    }
    if world.resource::<FaceEditContext>().selected_face.is_none() {
        return Err("Select a planar face first".into());
    }
    *world.resource_mut::<FaceSketchShape>() = shape;
    let interactive =
        args.get("centre").is_none() && args.get("a").is_none() && args.get("points").is_none();
    if interactive {
        *world.resource_mut::<FaceDrawingContext>() = FaceDrawingContext {
            active: true,
            ..Default::default()
        };
    }
    Ok(interactive)
}
fn point(v: &Value) -> Result<Vec2, String> {
    let a = v
        .as_array()
        .filter(|a| a.len() == 2)
        .ok_or("Point must contain two finite coordinates")?;
    Ok(Vec2::new(
        scalar_from_json(&a[0])?,
        scalar_from_json(&a[1])?,
    ))
}
pub fn shape_profile(shape: FaceSketchShape, a: Vec2, b: Vec2) -> Result<Profile2d, String> {
    if !a.is_finite() || !b.is_finite() {
        return Err("Coordinates must be finite".into());
    }
    match shape {
        FaceSketchShape::Circle => {
            let r = a.distance(b);
            if !r.is_finite() || r < 0.0001 {
                return Err("Circle radius must be positive".into());
            }
            // Four analytic quarter arcs, not an authored polygon approximation.
            let bulge = (std::f32::consts::PI / 8.).tan();
            Ok(Profile2d {
                start: a + Vec2::X * r,
                segments: vec![
                    ProfileSegment::ArcTo {
                        to: a + Vec2::Y * r,
                        bulge,
                    },
                    ProfileSegment::ArcTo {
                        to: a - Vec2::X * r,
                        bulge,
                    },
                    ProfileSegment::ArcTo {
                        to: a - Vec2::Y * r,
                        bulge,
                    },
                    ProfileSegment::ArcTo {
                        to: a + Vec2::X * r,
                        bulge,
                    },
                ],
            })
        }
        FaceSketchShape::Rectangle => {
            let size = (b - a).abs();
            if size.min_element() < 0.0001 {
                return Err("Rectangle needs two distinct opposite corners".into());
            }
            Ok(Profile2d::rectangle(size.x, size.y).translated((a + b) * 0.5))
        }
        FaceSketchShape::Polyline => Err("Polyline requires points".into()),
    }
}
fn circle(world: &mut World, args: &Value) -> Result<CommandResult, String> {
    if prepare(world, args, FaceSketchShape::Circle)? {
        return Ok(CommandResult::default());
    }
    let a = point(&args["centre"])?;
    let r = scalar_from_json(&args["radius"])?;
    if r <= 0. {
        return Err("Radius must be positive".into());
    }
    commit_profile(
        world,
        shape_profile(FaceSketchShape::Circle, a, a + Vec2::X * r)?,
    )
}
fn rectangle(world: &mut World, args: &Value) -> Result<CommandResult, String> {
    if prepare(world, args, FaceSketchShape::Rectangle)? {
        return Ok(CommandResult::default());
    }
    commit_profile(
        world,
        shape_profile(
            FaceSketchShape::Rectangle,
            point(&args["a"])?,
            point(&args["b"])?,
        )?,
    )
}
fn polyline(world: &mut World, args: &Value) -> Result<CommandResult, String> {
    if prepare(world, args, FaceSketchShape::Polyline)? {
        return Ok(CommandResult::default());
    }
    let points = args["points"]
        .as_array()
        .ok_or("Supply points")?
        .iter()
        .map(point)
        .collect::<Result<Vec<_>, _>>()?;
    if points.len() < 2 {
        return Err("At least two points required".into());
    }
    commit_profile(
        world,
        Profile2d {
            start: points[0],
            segments: points[1..]
                .iter()
                .map(|p| ProfileSegment::LineTo { to: *p })
                .collect(),
        },
    )
}
pub fn commit_profile(world: &mut World, profile: Profile2d) -> Result<CommandResult, String> {
    super::profile_feature::validate_face_profile(&profile, 0.)?;
    let context = world.resource::<FaceEditContext>().clone();
    let selected = context.selected_face.ok_or("Select a face first")?;
    let plane = DrawingPlane::from_face(selected.centroid, selected.normal);
    if let Some(surface) = context
        .entity
        .and_then(|e| world.get::<PlanarBody>(e))
        .and_then(|b| b.surfaces.get(selected.face_id.0 as usize))
    {
        let points = profile.tessellate(64);
        for i in 0..points.len() {
            for point in [
                points[i],
                (points[i] + points[(i + 1) % points.len()]) * 0.5,
            ] {
                let p = plane.to_world(point);
                let inside = surface.triangles.iter().any(|t| {
                    let n = surface.normal;
                    (0..3).all(|j| (t[(j + 1) % 3] - t[j]).cross(p - t[j]).dot(n) >= -1e-5)
                });
                if !inside {
                    return Err(
                        "Keep the outline within the selected face and clear of existing openings"
                            .into(),
                    );
                }
            }
        }
    }

    let mut parent = context.element_id.ok_or("Missing support object")?;
    // Add subsequent regions to the current derived body, preserving the
    // ordered authored feature dependency rather than overlapping host copies.
    let mut visited = std::collections::HashSet::new();
    while visited.insert(parent) {
        let Some(entity) = find_entity_by_element_id_readonly(world, parent) else {
            break;
        };
        let Some(operand) = world.get::<super::profile_feature::FeatureOperand>(entity) else {
            break;
        };
        parent = operand.owner;
    }
    let id = world.resource::<ElementIdAllocator>().next_id();
    let rotation = Quat::from_mat3(&Mat3::from_cols(
        plane.tangent,
        plane.normal,
        plane.bitangent,
    ));
    let (min, max) = profile.bounds_2d();
    let centre = (min + max) * 0.5;
    let snapshot = make_face_profile_feature_snapshot(
        id,
        parent,
        profile.translated(-centre),
        plane.to_world(centre),
        rotation,
        selected.generated_face_ref,
    );
    crate::plugins::face_edit::consume_drawing_completion_click(world);
    enqueue_create_world_entity(world, snapshot.into());
    *world.resource_mut::<FaceDrawingContext>() = FaceDrawingContext::default();
    world.resource_mut::<PendingFaceRegion>().0 = Some(id);
    Ok(CommandResult {
        output: Some(
            json!({"element_id":id.0,"parent":parent.0,"depth":0,"next_command":"modeling.set_face_depth"}),
        ),
        ..Default::default()
    })
}
fn set_depth(world: &mut World, args: &Value) -> Result<CommandResult, String> {
    let (entity, id) = target(world, args)?;
    let before = FaceProfileFeatureFactory
        .capture_snapshot(&world.entity(entity), world)
        .ok_or("Target must be an authored face profile feature")?;
    let depth = scalar_from_json(&args["depth"])?;
    let after = before.set_property_json("depth", &json!(depth))?;
    enqueue_apply_entity_changes(
        world,
        ApplyEntityChangesCommand {
            label: "Push/pull face region".into(),
            before: vec![before],
            after: vec![after],
        },
    );
    Ok(CommandResult {
        output: Some(
            json!({"element_id":id.0,"depth":depth,"operation":if depth>0. {"add"}else if depth<0. {"cut"}else{"flush"}}),
        ),
        ..Default::default()
    })
}
pub fn select_pending_region(world: &mut World) {
    let Some(id) = world.resource::<PendingFaceRegion>().0 else {
        return;
    };
    let Some(entity) = find_entity_by_element_id_readonly(world, id) else {
        return;
    };
    let Some(feature) = world.get::<super::profile_feature::FaceProfileFeature>(entity) else {
        return;
    };
    let rotation = world
        .get::<super::primitives::ShapeRotation>(entity)
        .copied()
        .unwrap_or_default();
    let normal = rotation.0 * Vec3::Y;
    let centroid = feature.anchor_origin + normal * feature.depth;
    select_hit(
        world,
        FaceHitCandidate {
            entity,
            element_id: id,
            distance: 0.,
            face_id: FaceId(0),
            generated_face_ref: Some(crate::capability_registry::GeneratedFaceRef::FeatureCap),
            normal,
            centroid,
        },
    );
    world.resource_mut::<PendingFaceRegion>().0 = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authored_entity::AuthoredEntity;
    use crate::plugins::modeling::{
        bsp_csg::{self, CsgTriangle},
        primitive_trait::Primitive,
        primitives::BoxPrimitive,
        profile_feature::evaluate_profile_on_triangles,
        snapshots::ray_triangle_intersection,
    };
    fn triangles(e: &super::super::profile_feature::EvaluatedFeature) -> Vec<CsgTriangle> {
        e.indices
            .chunks_exact(3)
            .map(|t| {
                CsgTriangle::new(
                    e.vertices[t[0] as usize],
                    e.vertices[t[1] as usize],
                    e.vertices[t[2] as usize],
                )
            })
            .collect()
    }
    fn volume(ts: &[CsgTriangle]) -> f32 {
        ts.iter()
            .map(|t| t.vertices[0].dot(t.vertices[1].cross(t.vertices[2])) / 6.)
            .sum::<f32>()
            .abs()
    }
    #[test]
    fn analytic_circle_round_trip_and_signed_evaluation_preserve_host() {
        let host = BoxPrimitive {
            centre: Vec3::ZERO,
            half_extents: Vec3::new(2., 1.5, 0.15),
        };
        let base =
            bsp_csg::triangles_from_editable_mesh(&host.to_editable_mesh(Quat::IDENTITY).unwrap());
        let profile =
            shape_profile(FaceSketchShape::Circle, Vec2::ZERO, Vec2::new(0.3, 0.)).unwrap();
        assert_eq!(profile.segments.len(), 4);
        for point in profile.tessellate(64) {
            assert!(
                (point.length() - 0.3).abs() < 1e-6,
                "circle radius {point:?}"
            );
        }
        assert!(profile
            .segments
            .iter()
            .all(|s| matches!(s, ProfileSegment::ArcTo { .. })));
        let mut snap = make_face_profile_feature_snapshot(
            ElementId(2),
            ElementId(1),
            profile,
            Vec3::new(0., 0., 0.15),
            Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            None,
        );
        let original = base.clone();
        let mut volumes = Vec::new();
        for depth in [0., 0.4, 0., -0.1, -0.4] {
            snap.feature.depth = depth;
            let eval = evaluate_profile_on_triangles(&base, &snap.feature, snap.rotation.0);
            let result = triangles(&eval);
            volumes.push(volume(&result));
            assert!(
                super::super::semantics::body_summary_from_triangles(&result).is_closed_manifold,
                "open shell at depth {depth}"
            );
            let ray = Ray3d::new(Vec3::new(0., 0., 2.), Dir3::NEG_Z);
            let hit = result.iter().any(|t| {
                ray_triangle_intersection(ray, t.vertices[0], t.vertices[1], t.vertices[2])
                    .is_some()
            });
            assert_eq!(hit, depth > -0.3, "depth {depth}");
            let restored = FaceProfileFeatureFactory
                .from_persisted_json(&snap.to_json())
                .unwrap();
            assert_eq!(restored.to_json(), snap.to_json());
            assert_eq!(snap.feature.parent, ElementId(1));
        }
        assert!((volumes[0] - 3.6).abs() < 0.001);
        assert!(volumes[1] > volumes[0] + 0.1);
        assert_eq!(volumes[2], volumes[0]);
        assert!(volumes[3] < volumes[0]);
        assert!(volumes[4] < volumes[3]);
        assert_eq!(base[0].vertices, original[0].vertices);
    }
    #[test]
    fn rectangle_after_circular_cut_preserves_closed_shell() {
        let host = BoxPrimitive {
            centre: Vec3::new(13.058414, 1.1, -8.157),
            half_extents: Vec3::new(0.115, 1.1, 0.9338),
        };
        let base =
            bsp_csg::triangles_from_editable_mesh(&host.to_editable_mesh(Quat::IDENTITY).unwrap());
        let rotation = Quat::from_xyzw(0.5, 0.5, 0.5, -0.5);
        let mut circle = make_face_profile_feature_snapshot(
            ElementId(2),
            ElementId(1),
            shape_profile(
                FaceSketchShape::Circle,
                Vec2::ZERO,
                Vec2::new(0.2348976, 0.),
            )
            .unwrap(),
            Vec3::new(13.173414, 1.1434436, -8.098276),
            rotation,
            None,
        );
        circle.feature.depth = -0.3;
        let cut = triangles(&evaluate_profile_on_triangles(
            &base,
            &circle.feature,
            rotation,
        ));
        let mut rectangle = make_face_profile_feature_snapshot(
            ElementId(3),
            ElementId(2),
            Profile2d::rectangle(0.3, 0.3),
            Vec3::new(13.173414, 0.35, -8.740799),
            rotation,
            None,
        );
        rectangle.feature.depth = 0.1;
        let result = triangles(&evaluate_profile_on_triangles(
            &cut,
            &rectangle.feature,
            rotation,
        ));
        assert!(super::super::semantics::body_summary_from_triangles(&result).is_closed_manifold);
    }

    #[test]
    fn circle_rejects_zero_radius_and_rectangle_is_plane_local() {
        assert!(shape_profile(FaceSketchShape::Circle, Vec2::ZERO, Vec2::ZERO).is_err());
        let profile = shape_profile(
            FaceSketchShape::Rectangle,
            Vec2::new(3., 4.),
            Vec2::new(1., 2.),
        )
        .unwrap();
        let (a, b) = profile.bounds_2d();
        assert_eq!(a, Vec2::new(1., 2.));
        assert_eq!(b, Vec2::new(3., 4.));
    }
}
