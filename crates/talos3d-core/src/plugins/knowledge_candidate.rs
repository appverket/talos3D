//! Disposable candidate execution. No candidate registry or authored entity is
//! inserted into the user's document. Rendering consumes only derived meshes.
use crate::capability_registry::{CapabilityRegistry, ElementClassDescriptor};
use crate::plugins::modeling::{
    generic_factory::PrimitiveFactory,
    primitive_trait::{MeshGenerator, Primitive},
    primitives::ShapeRotation,
    profile::ProfileExtrusion,
};
use crate::plugins::{knowledge_cloud, model_api, parametric_mcp};
use crate::relational::registry::{ParametricStore, Placement};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Optional bounded native integration scenario in an empty disposable document.
    #[serde(default)]
    pub scenario: Option<scenario::Scenario>,
    pub asset: knowledge_cloud::Asset,
    #[serde(default)]
    pub drivers: BTreeMap<String, Value>,
    #[serde(default)]
    pub placement: Placement,
    /// Ordered edits exercise the same driver-update handler as ordinary MCP.
    #[serde(default)]
    pub edits: Vec<DriverEdit>,
    #[serde(skip)]
    #[schemars(skip)]
    inspection_snapshot: Option<String>,
}
#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverEdit {
    pub name: String,
    pub value: Value,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LegacyEvaluation {
    pub id: String,
    /// Exact snapshot_sha256 returned by get_legacy_knowledge_asset.
    pub snapshot_sha256: String,
    #[serde(default)]
    pub drivers: BTreeMap<String, Value>,
    #[serde(default)]
    pub placement: Placement,
    #[serde(default)]
    pub edits: Vec<DriverEdit>,
}
/// The caller supplies identity, never a fabricated publication envelope. The
/// exact server-owned legacy receipt is inspected without resolving its rights.
pub fn from_legacy(input: LegacyEvaluation, receipt: Value) -> Result<Request, String> {
    if receipt["kind"] != "parametric_type.v1"
        || receipt["id"] != input.id
        || receipt["snapshot_sha256"] != input.snapshot_sha256
    {
        return Err("Legacy snapshot changed or identity mismatched; retrieve it again".into());
    }
    Ok(Request {
        scenario: None,
        asset: knowledge_cloud::Asset {
            schema: knowledge_cloud::SCHEMA.into(),
            id: input.id,
            kind: "parametric_type.v1".into(),
            title: "Unreviewed legacy runtime inspection".into(),
            description: "Inspection-only snapshot; not a publication candidate".into(),
            element_classes: vec![],
            path_kind: "parametric".into(),
            required_tool_ids: vec![],
            payload: receipt["payload"].clone(),
            applicability: String::new(),
            limitations: vec![],
            evidence: vec![],
            redistribution_rights: String::new(),
            contains_private_context: false,
        },
        drivers: input.drivers,
        placement: input.placement,
        edits: input.edits,
        inspection_snapshot: Some(input.snapshot_sha256),
    })
}

pub struct Scene {
    pub report: Value,
    pub parts: Vec<(Mesh, Transform)>,
    pub bounds: crate::authored_entity::EntityBounds,
}

/// Deliberately limited to a portable parametric representation. Unsupported
/// asset kinds fail rather than substituting synthetic geometry.
#[path = "knowledge_candidate_scenario.rs"]
pub mod scenario;

#[derive(Default)]
pub struct Context {
    pub classes: Vec<ElementClassDescriptor>,
    pub assembly_types: Vec<crate::capability_registry::AssemblyTypeDescriptor>,
    pub relation_types: Vec<crate::capability_registry::RelationTypeDescriptor>,
}

pub fn evaluate(request: Request, classes: Vec<ElementClassDescriptor>) -> Result<Scene, String> {
    evaluate_in(
        request,
        Context {
            classes,
            ..Default::default()
        },
    )
}

pub fn evaluate_in(mut request: Request, context: Context) -> Result<Scene, String> {
    if let Some(scenario) = request.scenario.take() {
        if !request.edits.is_empty() {
            return Err(
                "Use scenario steps instead of combining a scenario with legacy driver edits"
                    .into(),
            );
        }
        return scenario::evaluate(request, scenario, context);
    }
    let classes = context.classes;
    let started = std::time::Instant::now();
    if request.asset.kind != "parametric_type.v1" {
        return Err("Isolated execution currently supports parametric_type.v1 only; other candidate runtimes are CapabilityGaps".into());
    }
    if request.edits.len() > 16 || request.drivers.len() > 64 {
        return Err("Candidate execution permits at most 16 edits and 64 driver overrides".into());
    }
    let check = knowledge_cloud::CandidateCheck {
        asset: request.asset.clone(),
        driver_cases: vec![request.drivers.clone()],
    };
    let checked = if request.inspection_snapshot.is_some() {
        knowledge_cloud::check_payload(check)?
    } else {
        knowledge_cloud::check_candidate(check)?
    };
    let count = checked["parametric_cases"][0]["members"]
        .as_array()
        .map_or(0, Vec::len);
    if !(1..=128).contains(&count) {
        return Err("Candidate scene requires 1..128 emitted members".into());
    }
    let revision = checked["content_hash"]
        .as_str()
        .ok_or("Candidate hash missing")?
        .to_owned();
    let type_id = request.asset.id.clone();
    let mut scratch = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<ProfileExtrusion>::new());
    for class in classes {
        registry.register_element_class(class);
    }
    scratch.insert_resource(registry);
    scratch.init_resource::<crate::plugins::identity::ElementIdAllocator>();
    scratch.init_resource::<ParametricStore>();
    // Candidate eligibility was checked above; legacy inspection deliberately
    // establishes no envelope, provenance or release authority.
    knowledge_cloud::install_payload(
        &mut scratch,
        knowledge_cloud::AdmittedAsset {
            scope_sha256: None,
            asset: request.asset,
            revision: revision.clone(),
        },
    )?;
    let created = parametric_mcp::world_create(
        &mut scratch,
        parametric_mcp::CreateParametricRequest {
            type_id,
            overrides: request.drivers,
            placement: Some(request.placement),
            ..default()
        },
    )?;
    let instance_id = created.snapshot.instance_id;
    let before = observations(&scratch, created.element_ids)?;
    let mut steps = Vec::new();
    let mut final_ids = before["element_ids"]
        .as_array()
        .ok_or("Missing created IDs")?
        .iter()
        .map(|id| id.as_u64().ok_or("Invalid created ID"))
        .collect::<Result<Vec<_>, _>>()?;
    for edit in request.edits {
        let outcome = parametric_mcp::world_set_driver(
            &mut scratch,
            parametric_mcp::SetParametricDriverRequest {
                instance_id,
                name: edit.name.clone(),
                value: edit.value.clone(),
            },
        )?;
        if !(1..=128).contains(&outcome.element_ids.len()) {
            return Err("Candidate scene member bound exceeded after edit".into());
        }
        final_ids = outcome.element_ids;
        steps.push(json!({"edit":edit,"observed":observations(&scratch, final_ids.clone())?}));
    }
    let final_observed = observations(&scratch, final_ids.clone())?;
    let snapshot = parametric_mcp::world_inspect(
        &mut scratch,
        parametric_mcp::InspectParametricRequest { instance_id },
    )?;
    let aabb = model_api::handle_get_world_aabb(
        &scratch,
        model_api::GetWorldAabbRequest {
            element_ids: final_ids,
        },
    )?;
    let combined = aabb.combined.ok_or("Candidate has no geometric bounds")?;
    let bounds = crate::authored_entity::EntityBounds {
        min: Vec3::from_array(combined.min.map(|v| v as f32)),
        max: Vec3::from_array(combined.max.map(|v| v as f32)),
    };
    if !bounds.min.is_finite()
        || !bounds.max.is_finite()
        || (bounds.max - bounds.min).length() > 100_000.0
    {
        return Err("Candidate scene bounds are nonfinite or exceed 100 km".into());
    }
    let mut parts = Vec::new();
    let mut query = scratch.query::<(&ProfileExtrusion, &ShapeRotation)>();
    let mut vertices = 0;
    for (shape, rotation) in query.iter(&scratch) {
        let mesh = shape.to_bevy_mesh(rotation.0);
        vertices += mesh.count_vertices();
        if vertices > 100_000 {
            return Err("Candidate scene exceeds 100000 vertices".into());
        }
        parts.push((mesh, shape.entity_transform(rotation.0)));
    }
    Ok(Scene {
        bounds,
        parts,
        report: json!({
            "schema":"talos3d.candidate-runtime-observation.v1", "content_hash":revision,
            "runtime":"isolated_parametric_handlers", "initial":before, "edits":steps,
        "legacy_inspection":request.inspection_snapshot.as_ref().map(|hash|json!({"snapshot_sha256":hash,"rights":"unresolved","publication_candidate":false})),
            "final":final_observed,"snapshot":snapshot,"mesh_vertices":vertices,
            "cpu_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "model_modified":false,"installed":false,"released":false,
            "limitations":["AABB checks are approximate; intersections may be intended joints.",
              "No source, rights, structural engineering or construction approval.",
              "No project hosting, undo/redo, native file round-trip or recipe/Definition execution qualification.",
              "A rendered candidate is not an accepted user-model retry."],
            "rendered":false
        }),
    })
}
fn observations(world: &World, ids: Vec<u64>) -> Result<Value, String> {
    let aabb = model_api::handle_get_world_aabb(
        world,
        model_api::GetWorldAabbRequest {
            element_ids: ids.clone(),
        },
    )?;
    if !aabb.errors.is_empty() {
        return Err("Candidate emitted members without geometric bounds".into());
    }
    let overlaps = model_api::handle_check_overlaps(
        world,
        model_api::CheckOverlapsRequest {
            element_ids: ids.clone(),
        },
    )?;
    let floating = model_api::handle_check_floating(
        world,
        model_api::CheckFloatingRequest {
            element_ids: ids.clone(),
            tolerance_m: None,
        },
    )?;
    Ok(json!({"element_ids":ids,"world_aabb":aabb,"overlaps":overlaps,"floating":floating}))
}

pub type RuntimeResult = Result<(Value, String), String>;
type Reply = std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<RuntimeResult>>>>;
#[derive(Resource)]
struct PendingRender {
    reply: Reply,
    deadline: std::time::Instant,
    entities: Vec<Entity>,
    meshes: Vec<Handle<Mesh>>,
    material: Handle<StandardMaterial>,
    target: Handle<Image>,
}
/// Dedicated derived-only render layer, disjoint from authored layer 0 and
/// Definition editor layer 1. Nothing here has an ElementId or an occurrence.
const CANDIDATE_LAYER: usize = 2;

pub fn register(app: &mut App) {
    app.add_systems(Update, cleanup_render);
}

pub fn start(
    world: &mut World,
    scene: Scene,
    response: tokio::sync::oneshot::Sender<RuntimeResult>,
) {
    if world.contains_resource::<PendingRender>() {
        let _ = response.send(Err(
            "Candidate renderer busy; only one bounded evaluation may run at a time".into(),
        ));
        return;
    }
    if !world.contains_resource::<Assets<Mesh>>()
        || !world.contains_resource::<Assets<Image>>()
        || !world.contains_resource::<Assets<StandardMaterial>>()
    {
        let _ = response.send(Err("Candidate rendering requires the product GPU renderer; computation alone is not visual acceptance".into()));
        return;
    }
    use crate::plugins::definition_preview_scene::PreviewOnly;
    use bevy::camera::{visibility::RenderLayers, RenderTarget};
    use bevy::render::{
        render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    };
    let layer = RenderLayers::layer(CANDIDATE_LAYER);
    let extent = Extent3d {
        width: 768,
        height: 768,
        depth_or_array_layers: 1,
    };
    let mut image = Image::new_fill(
        extent,
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        default(),
    );
    image.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT;
    let target = world.resource_mut::<Assets<Image>>().add(image);
    let material = world
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color: Color::srgb(0.65, 0.72, 0.82),
            perceptual_roughness: 0.8,
            ..default()
        });
    let mut entities = Vec::new();
    let mut meshes = Vec::new();
    for (mesh, transform) in scene.parts {
        let handle = world.resource_mut::<Assets<Mesh>>().add(mesh);
        entities.push(
            world
                .spawn((
                    Mesh3d(handle.clone()),
                    MeshMaterial3d(material.clone()),
                    transform,
                    layer.clone(),
                    PreviewOnly,
                ))
                .id(),
        );
        meshes.push(handle);
    }
    let center = (scene.bounds.min + scene.bounds.max) * 0.5;
    let radius = (scene.bounds.max - scene.bounds.min).length().max(0.01) * 0.5;
    let eye = center + Vec3::new(1.3, 1.0, 1.7).normalize() * radius * 3.5;
    entities.push(
        world
            .spawn((
                Camera3d::default(),
                Camera {
                    order: -200,
                    clear_color: ClearColorConfig::Custom(Color::srgb(0.06, 0.08, 0.11)),
                    ..default()
                },
                RenderTarget::Image(target.clone().into()),
                Transform::from_translation(eye).looking_at(center, Vec3::Y),
                Projection::Perspective(PerspectiveProjection {
                    near: (radius * 0.001).max(0.00001),
                    far: radius * 10.0,
                    ..default()
                }),
                layer.clone(),
                PreviewOnly,
            ))
            .id(),
    );
    entities.push(
        world
            .spawn((
                DirectionalLight {
                    illuminance: 15000.0,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.8, -0.6, 0.0)),
                layer,
                PreviewOnly,
            ))
            .id(),
    );
    let reply: Reply = std::sync::Arc::new(std::sync::Mutex::new(Some(response)));
    let observer_reply = reply.clone();
    let mut report = scene.report;
    report["rendered"] = json!(true);
    report["presentation"] = json!({"kind":"isolated_gpu_candidate","width":768,"height":768,"materials":"neutral inspection material, not candidate material qualification"});
    let screenshot = world
        // Marked like the rest of the candidate scene so a single
        // `Without<PreviewOnly>` filter excludes everything this path spawns.
        .spawn((Screenshot::image(target.clone()), PreviewOnly))
        .observe(move |event: On<ScreenshotCaptured>| {
            use base64::Engine;
            let result = (|| {
                let image = event
                    .image
                    .clone()
                    .try_into_dynamic()
                    .map_err(|e| e.to_string())?;
                if let Some(warning) =
                    crate::plugins::drawing_export::screenshot_quality_warning(&image.to_rgb8())
                {
                    return Err(format!(
                        "Candidate capture is not valid visual evidence: {warning}"
                    ));
                }
                let mut bytes = std::io::Cursor::new(Vec::new());
                image
                    .write_to(&mut bytes, image::ImageFormat::Png)
                    .map_err(|e| e.to_string())?;
                if bytes.get_ref().len() > 12 * 1024 * 1024 {
                    return Err("Candidate PNG exceeds response bound".into());
                }
                Ok((
                    report.clone(),
                    base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),
                ))
            })();
            if let Some(sender) = observer_reply.lock().unwrap().take() {
                let _ = sender.send(result);
            }
        })
        .id();
    entities.push(screenshot);
    world.insert_resource(PendingRender {
        reply,
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
        entities,
        meshes,
        material,
        target,
    });
}
fn cleanup_render(world: &mut World) {
    let Some(pending) = world.get_resource::<PendingRender>() else {
        return;
    };
    let finished = pending
        .reply
        .lock()
        .unwrap()
        .as_ref()
        .is_none_or(|sender| sender.is_closed());
    if !finished && std::time::Instant::now() < pending.deadline {
        return;
    }
    let pending = world.remove_resource::<PendingRender>().unwrap();
    if let Some(sender) = pending.reply.lock().unwrap().take() {
        let _ = sender.send(Err(
            "Candidate GPU capture timed out; no visual acceptance evidence".into(),
        ));
    }
    for entity in pending.entities {
        let _ = world.despawn(entity);
    }
    for mesh in pending.meshes {
        world.resource_mut::<Assets<Mesh>>().remove(mesh.id());
    }
    world
        .resource_mut::<Assets<StandardMaterial>>()
        .remove(pending.material.id());
    world
        .resource_mut::<Assets<Image>>()
        .remove(pending.target.id());
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Request {
        use crate::relational::{
            component::{ComponentParams, DriverPolicy},
            param_expr::{Quantity, ScalarExpr, Unit},
            registry::{ParametricMember, ParametricRepresentation, ParametricTypeDef},
            transform::TransformBindings,
        };
        let zero = || std::array::from_fn(|_| ScalarExpr::lit(Quantity::mm(0.0)));
        let definition = ParametricTypeDef {
            id: "test.candidate".into(),
            label: "Synthetic candidate fixture".into(),
            params: ComponentParams::default().driver("width", DriverPolicy::Editable),
            driver_units: BTreeMap::from([("width".into(), Unit::Mm)]),
            defaults: BTreeMap::from([("width".into(), 1000.0)]),
            derivations: default(),
            transform: TransformBindings::default(),
            public: true,
            representation: Some(ParametricRepresentation {
                members: vec![ParametricMember {
                    size: [
                        ScalarExpr::param("width"),
                        ScalarExpr::lit(Quantity::mm(500.0)),
                        ScalarExpr::lit(Quantity::mm(100.0)),
                    ],
                    translate: zero(),
                    rotate_euler_deg: zero(),
                    profile_xz: vec![],
                    label: None,
                    semantic: None,
                }],
            }),
        };
        let asset = knowledge_cloud::Asset {
            schema: knowledge_cloud::SCHEMA.into(),
            id: definition.id.clone(),
            kind: "parametric_type.v1".into(),
            title: "Synthetic fixture".into(),
            description: "Unit fixture only".into(),
            element_classes: vec![],
            path_kind: "parametric".into(),
            required_tool_ids: vec![],
            payload: json!({"definition":definition}),
            applicability: "Developer unit test".into(),
            limitations: vec![],
            evidence: vec!["Synthetic runtime test fixture".into()],
            redistribution_rights: "Synthetic unit fixture".into(),
            contains_private_context: false,
        };
        Request {
            scenario: None,
            asset,
            drivers: default(),
            placement: default(),
            inspection_snapshot: None,
            edits: vec![DriverEdit {
                name: "width".into(),
                value: json!(2000.0),
            }],
        }
    }
    #[test]
    fn native_scenario_retires_placeholder_and_preserves_assembly_through_codec_and_history() {
        let mut req = request();
        req.edits.clear();
        let call = |id: &str, tool: &str, args: Value| {
            json!({
                "id":id,"tool":tool,"args":args.as_object().unwrap().iter().map(|(k,v)| (k.clone(),json!({"expr":"literal","value":v}))).collect::<serde_json::Map<String,Value>>()
            })
        };
        req.scenario = Some(serde_json::from_value(json!({"script":{
            "parameter_schema":{"type":"object"},"allowed_tools":["create_box","parametric.create","create_assembly","delete_entities"],"mutation_scope":{"kind":"project_root"},
            "steps":[
                call("placeholder","create_box",json!({"centre":[10.,0.,0.],"half_extents":[0.1,0.1,0.1]})),
                call("candidate","parametric.create",json!({"type_id":req.asset.id})),
                call("assembly","create_assembly",json!({"assembly_type":"fixture","label":"fixture","members":[{"target":1,"role":"part"}],"metadata":{"unresolved":"deliberately retained"}})),
                call("retire","delete_entities",json!({"element_ids":[0]}))
            ]},"round_trip":true,"undo_redo":true})).unwrap());
        let context=Context {assembly_types:vec![serde_json::from_value(json!({"assembly_type":"fixture","label":"Fixture","description":"Synthetic regression","expected_member_types":[],"expected_member_roles":["part"],"expected_relation_types":[],"parameter_schema":{}})).unwrap()],..Default::default()};
        let scene = evaluate_in(req, context).unwrap();
        assert!(!scene.parts.is_empty());
        assert_eq!(
            scene.report["final_state"],
            scene.report["round_trip"]["after_reopen"]
        );
        assert_eq!(
            scene.report["before_persistence"],
            scene.report["history"]["after_redo"]
        );
        assert!(scene.report["history"]["after_undo"]["element_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(0)));
        assert!(!scene.report["final_state"]["element_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(0)));
        assert!(scene.report["final_state"]
            .to_string()
            .contains("deliberately retained"));
        assert_eq!(scene.report["model_modified"], false);
    }

    #[test]
    fn native_scenario_denies_host_routes_and_claimed_acceptance() {
        for tool in [
            "load_project",
            "save_project",
            "invoke_command",
            "install_knowledge_asset",
        ] {
            let mut req = request();
            req.edits.clear();
            req.scenario=Some(serde_json::from_value(json!({"script":{"parameter_schema":{"type":"object"},"allowed_tools":[tool],"mutation_scope":{"kind":"project_root"},"steps":[{"id":"deny","tool":tool,"args":{}}]}})).unwrap());
            assert!(evaluate(req, vec![]).err().unwrap().contains("unsupported"));
        }
    }

    #[test]
    fn native_scenario_rejects_unbounded_programs_and_false_postconditions() {
        let call = json!({"id":"step","tool":"model_summary","args":{}});
        for (steps, postconditions) in [
            (vec![call.clone(); 33], json!([])),
            (
                vec![
                    json!({"kind":"repeat","id":"loop","var":"i","count":{"expr":"literal","value":1000000},"body":[call.clone()]}),
                ],
                json!([]),
            ),
        ] {
            let mut req = request();
            req.edits.clear();
            req.scenario=Some(serde_json::from_value(json!({"script":{"parameter_schema":{"type":"object"},"allowed_tools":["model_summary"],"mutation_scope":{"kind":"project_root"},"steps":steps,"postconditions":postconditions}})).unwrap());
            assert!(evaluate(req, vec![])
                .err()
                .unwrap()
                .contains("1..32 flat calls"));
        }
        let mut req = request();
        req.edits.clear();
        req.scenario=Some(serde_json::from_value(json!({"script":{"parameter_schema":{"type":"object"},"allowed_tools":["create_box"],"mutation_scope":{"kind":"none"},"steps":[{"id":"mutate","tool":"create_box","args":{}}]}})).unwrap());
        assert!(evaluate(req, vec![]).err().unwrap().contains("no-mutation"));
    }

    #[test]
    fn legacy_inspection_binds_snapshot_without_inventing_rights() {
        use crate::relational::registry::{ParametricRegistry, ParametricTypeDef};
        let sample = request();
        let definition: ParametricTypeDef =
            serde_json::from_value(sample.asset.payload["definition"].clone()).unwrap();
        let mut world = World::new();
        world.init_resource::<ParametricRegistry>();
        world
            .resource_mut::<ParametricRegistry>()
            .register(definition.clone());
        let receipt = knowledge_cloud::legacy_get(
            &world,
            knowledge_cloud::LegacyRequest {
                id: definition.id.clone(),
                kind: "parametric_type.v1".into(),
            },
        )
        .unwrap();
        assert_eq!(
            receipt["runtime_inspection"]["next_tool"],
            "evaluate_legacy_knowledge_asset"
        );
        let hash = receipt["snapshot_sha256"].as_str().unwrap().to_owned();
        let input = || LegacyEvaluation {
            id: definition.id.clone(),
            snapshot_sha256: hash.clone(),
            drivers: default(),
            placement: default(),
            edits: vec![],
        };
        let inspected = from_legacy(input(), receipt.clone()).unwrap();
        assert!(inspected.asset.evidence.is_empty());
        assert!(inspected.asset.redistribution_rights.is_empty());
        assert!(
            knowledge_cloud::check_candidate(knowledge_cloud::CandidateCheck {
                asset: inspected.asset.clone(),
                driver_cases: vec![],
            })
            .is_err()
        );
        let forged = json!({"asset":inspected.asset,"inspection_snapshot":hash});
        assert!(serde_json::from_value::<Request>(forged).is_err());
        let scene = evaluate(inspected, vec![]).unwrap();
        assert_eq!(scene.report["legacy_inspection"]["rights"], "unresolved");
        assert_eq!(
            scene.report["legacy_inspection"]["publication_candidate"],
            false
        );
        let mut stale = input();
        stale.snapshot_sha256 = "0".repeat(64);
        assert!(from_legacy(stale, receipt)
            .err()
            .unwrap()
            .contains("snapshot changed"));
        world
            .resource_mut::<ParametricRegistry>()
            .remove(&definition.id);
        let mut private = definition;
        private.public = false;
        world.resource_mut::<ParametricRegistry>().register(private);
        assert!(knowledge_cloud::legacy_get(
            &world,
            knowledge_cloud::LegacyRequest {
                id: "test.candidate".into(),
                kind: "parametric_type.v1".into(),
            }
        )
        .is_err());
    }

    #[test]
    fn scratch_runtime_uses_real_geometry_and_driver_updates() {
        let scene = evaluate(request(), vec![]).unwrap();
        assert_eq!(scene.parts.len(), 1);
        assert_eq!(
            scene.report["initial"]["world_aabb"]["combined"]["max"][0],
            json!(0.5)
        );
        assert_eq!(
            scene.report["final"]["world_aabb"]["combined"]["max"][0],
            json!(1.0)
        );
        assert_eq!(scene.report["snapshot"]["drivers"]["width"], json!(2000.0));
        assert_eq!(scene.report["model_modified"], false);
        assert_eq!(scene.report["rendered"], false);
        assert!(scene.parts[0].0.count_vertices() > 0);
    }
    #[test]
    fn unsupported_and_oversized_execution_fail_without_stand_ins() {
        let mut r = request();
        r.asset.kind = "recipe.v1".into();
        assert!(evaluate(r, vec![])
            .err()
            .unwrap()
            .contains("supports parametric"));
        let mut r = request();
        r.edits = (0..17)
            .map(|_| DriverEdit {
                name: "width".into(),
                value: json!(2000.0),
            })
            .collect();
        assert!(evaluate(r, vec![]).err().unwrap().contains("at most 16"));
    }
    /// The isolation this scope's authorization rests on: a self-test runs in a
    /// scratch `World`, and the only entities it adds to the live one are
    /// derived render entities that no document query can reach.
    #[test]
    fn candidate_scene_cannot_enter_the_user_document() {
        use crate::plugins::definition_preview_scene::PreviewOnly;
        use crate::plugins::identity::ElementId;
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        let authored = world.spawn(ElementId(77)).id();
        let (sender, _receiver) = tokio::sync::oneshot::channel();

        let scene = evaluate(request(), vec![]).unwrap();
        // Evaluation alone spawns nothing: the geometry arrives as plain meshes.
        assert!(!scene.parts.is_empty());
        start(&mut world, scene, sender);

        let spawned = world.resource::<PendingRender>().entities.clone();
        assert!(spawned.len() >= 3, "expected the candidate scene entities");
        for entity in &spawned {
            assert!(
                world.get::<PreviewOnly>(*entity).is_some(),
                "{entity} is not marked PreviewOnly"
            );
            assert!(
                world.get::<ElementId>(*entity).is_none(),
                "{entity} carries a document identity"
            );
        }

        // Mirrors the single gate in `persistence::build_project_file`; the
        // authored entity is all a save, export or selection may see.
        let mut document =
            world.query_filtered::<Entity, (With<ElementId>, Without<PreviewOnly>)>();
        assert_eq!(document.iter(&world).collect::<Vec<_>>(), vec![authored]);
    }

    #[test]
    fn gpu_timeout_reclaims_only_candidate_resources() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        let authored = world.spawn(crate::plugins::identity::ElementId(77)).id();
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        start(&mut world, evaluate(request(), vec![]).unwrap(), sender);
        assert!(world.contains_resource::<PendingRender>());
        assert!(world
            .get::<crate::plugins::identity::ElementId>(authored)
            .is_some());
        world.resource_mut::<PendingRender>().deadline = std::time::Instant::now();
        cleanup_render(&mut world);
        assert!(receiver
            .try_recv()
            .unwrap()
            .unwrap_err()
            .contains("timed out"));
        assert!(world.get_entity(authored).is_ok());
        assert_eq!(world.resource::<Assets<Mesh>>().len(), 0);
        assert_eq!(world.resource::<Assets<Image>>().len(), 0);
        assert!(!world.contains_resource::<PendingRender>());
    }
}
