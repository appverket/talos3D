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

/// Transport each observed state once. References resolve within this same
/// response; equality is established from the actual complete native snapshots.
/// This changes no observation and grants no independent acceptance credit.
pub fn compact_scenario_report(mut report: Value) -> Value {
    if report["schema"] != "talos3d.candidate-scenario-observation.v1" {
        return report;
    }
    use sha2::{Digest, Sha256};
    let mut states = serde_json::Map::new();
    for path in [
        "/before_persistence",
        "/history/after_undo",
        "/history/after_redo",
        "/round_trip/after_reopen",
        "/final_state",
    ] {
        if let Some(value) = report.pointer_mut(path) {
            if !value.is_null() {
                let hash = hex::encode(Sha256::digest(
                    serde_json::to_vec(&value).expect("JSON state"),
                ));
                states.entry(hash.clone()).or_insert_with(|| value.clone());
                *value = json!({"state_ref":hash});
            }
        }
    }
    report["schema"] = json!("talos3d.candidate-scenario-observation.v2");
    report["observed_states"] = Value::Object(states);
    report["state_reference_contract"]=json!("Resolve each state_ref in observed_states in this response. Keys are SHA-256 of the complete serialized observed state. Reused references mean actual snapshots were identical, not that checks were skipped.");
    report
}

pub type RuntimeResult = Result<(Value, Option<String>), String>;
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
    if scene.parts.is_empty() {
        if scene.report["empty_scene"] == true
            && scene.report["execution_error"]
                .as_str()
                .is_some_and(|e| !e.is_empty())
            && scene.report["final_state"]["element_ids"] == json!([])
        {
            let mut report = scene.report;
            report["rendered"] = json!(false);
            report["presentation"] = json!({"kind":"expected_rejection_no_geometry",
                "notice":"No geometry remained after the expected rejection. No image or visual acceptance is claimed."});
            let _ = response.send(Ok((report, None)));
        } else {
            let _ = response.send(Err(
                "Empty candidate has no verified rejection observation".into()
            ));
        }
        return;
    }
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
                    Some(base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())),
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
    fn compact_observations_preserve_all_states_without_repeating_geometry() {
        let state = json!({"element_ids":[1],"entities":[{"element_id":1,"data":{"fixture":"x".repeat(4096)}}]});
        let report = json!({"schema":"talos3d.candidate-scenario-observation.v1","before_persistence":state,"final_state":state,"history":{"after_undo":{"element_ids":[],"entities":[]},"after_redo":state},"round_trip":{"after_reopen":state}});
        let compact = compact_scenario_report(report.clone());
        assert_eq!(
            compact["schema"],
            "talos3d.candidate-scenario-observation.v2"
        );
        assert_eq!(compact["observed_states"].as_object().unwrap().len(), 2);
        for path in [
            "/before_persistence",
            "/final_state",
            "/history/after_undo",
            "/history/after_redo",
            "/round_trip/after_reopen",
        ] {
            let key = compact.pointer(path).unwrap()["state_ref"]
                .as_str()
                .unwrap();
            assert_eq!(
                &compact["observed_states"][key],
                report.pointer(path).unwrap()
            );
        }
        assert!(
            serde_json::to_vec(&compact).unwrap().len() * 2
                < serde_json::to_vec(&report).unwrap().len()
        );
    }

    #[test]
    fn native_scenario_observes_reopened_geometry_without_allowing_post_reopen_edits() {
        let call = |id: &str, tool: &str, args: Value| {
            json!({
                "id":id,"tool":tool,"args":args.as_object().unwrap().iter()
                    .map(|(k,v)| (k.clone(),json!({"expr":"literal","value":v})))
                    .collect::<serde_json::Map<String,Value>>()
            })
        };
        let observation = json!({"parameter_schema":{"type":"object"},
            "allowed_tools":["inspect_element_geometry"],"mutation_scope":{"kind":"none"},
            "steps":[call("mesh","inspect_element_geometry",json!({"element_id":0}))]});
        let scenario = json!({"script":{"parameter_schema":{"type":"object"},
            "allowed_tools":["create_box","inspect_element_geometry"],
            "mutation_scope":{"kind":"project_root"},"steps":[
                call("body","create_box",json!({"centre":[2.,3.,4.],"half_extents":[0.5,0.6,0.7]})),
                call("before","inspect_element_geometry",json!({"element_id":0}))]},
            "round_trip":true,"after_reopen":observation});
        let run = |scenario: Value| {
            let mut req = request();
            req.edits.clear();
            req.scenario = Some(serde_json::from_value(scenario).unwrap());
            evaluate_in(req, Context::default())
        };
        let scene = run(scenario.clone()).unwrap();
        assert_eq!(
            scene.report["calls"][1]["result"],
            scene.report["round_trip"]["observations"][0]["result"]
        );
        assert_eq!(
            scene.report["round_trip"]["after_reopen"],
            scene.report["final_state"]
        );
        assert!(!scene.report["calls"][1]["result"].is_null());
        let mut invalid = scenario.clone();
        invalid["round_trip"] = json!(false);
        assert!(run(invalid).is_err());
        let mut invalid = scenario.clone();
        invalid["after_reopen"]["mutation_scope"] = json!({"kind":"project_root"});
        assert!(run(invalid).is_err());
        let mut invalid = scenario;
        invalid["after_reopen"]["allowed_tools"] = json!(["create_box"]);
        invalid["after_reopen"]["steps"] = json!([call(
            "edit",
            "create_box",
            json!({"centre":[0.,0.,0.],"half_extents":[1.,1.,1.]})
        )]);
        assert!(run(invalid).is_err());
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
    fn portable_recipe_scenario_uses_native_router_and_round_trip() {
        use crate::curation::provenance::{Confidence, Lineage, Provenance};
        use crate::curation::{AssetId, AssetKindId, CurationMeta, RecipeArtifact, RecipeBody};
        use crate::plugins::refinement::{AgentId, RefinementState};
        let mut req = request();
        req.edits.clear();
        let script = serde_json::from_value(json!({
            "parameter_schema":{"type":"object","required":["confirmed"]},
            "allowed_tools":["create_box"],"mutation_scope":{"kind":"project_root"},
            "steps":[{"id":"body","tool":"create_box","args":{
                "centre":{"expr":"literal","value":[0,0,0]},
                "half_extents":{"expr":"literal","value":[1,1,1]}
            }}]
        }))
        .unwrap();
        let recipe = RecipeArtifact {
            meta: CurationMeta::new(
                AssetId("fixture".into()),
                AssetKindId("recipe.v1".into()),
                Provenance {
                    author: AgentId("unit-test".into()),
                    confidence: Confidence::High,
                    lineage: Lineage::Freeform,
                    rationale: Some("Synthetic dispatch fixture".into()),
                    jurisdiction: None,
                    catalog_dependencies: vec![],
                    evidence: vec![],
                },
            ),
            body: RecipeBody::AuthoringScript { script },
            parameter_schema: json!({}),
            target_class: "fixture".into(),
            supported_refinement_states: vec![RefinementState::Schematic],
            tests: vec![],
        };
        req.asset.id = "fixture".into();
        req.asset.kind = "recipe.v1".into();
        req.asset.path_kind = "recipe".into();
        req.asset.payload = serde_json::to_value(recipe).unwrap();
        let mut registry_world = World::new();
        let admitted = |asset| knowledge_cloud::AdmittedAsset {
            scope_sha256: None,
            asset,
            revision: "fixture".into(),
        };
        knowledge_cloud::install_payload(&mut registry_world, admitted(req.asset.clone())).unwrap();
        let mut collision = req.asset.clone();
        collision.id = "recipe.v1/fixture".into();
        collision.payload["meta"]["id"] = json!(collision.id);
        assert!(
            knowledge_cloud::install_payload(&mut registry_world, admitted(collision))
                .unwrap_err()
                .contains("collides")
        );
        let checked = knowledge_cloud::check_candidate(knowledge_cloud::CandidateCheck {
            asset: req.asset.clone(),
            driver_cases: vec![],
        })
        .unwrap();
        assert_eq!(checked["execution"]["arguments"]["family_id"], "fixture");
        assert_eq!(
            checked["execution"]["required_parameters"],
            json!(["confirmed"])
        );

        req.scenario=Some(serde_json::from_value(json!({"script":{
            "parameter_schema":{},"allowed_tools":["instantiate_recipe"],"mutation_scope":{"kind":"project_root"},
            "steps":[{"id":"recipe","tool":"instantiate_recipe","args":{
                "family_id":{"expr":"literal","value":"fixture"},
                "target_class":{"expr":"literal","value":"fixture"},
                "target_state":{"expr":"literal","value":"Schematic"},
                "parameters":{"expr":"literal","value":{"confirmed":true}}
            }}]},"round_trip":true,"undo_redo":true})).unwrap());
        let context=Context {classes:vec![serde_json::from_value(json!({"id":"fixture","label":"Fixture","description":"Synthetic fixture","semantic_roles":[],"class_min_obligations":{},"class_min_promotion_critical_paths":{},"parameter_schema":{}})).unwrap()],..Default::default()};
        let mut negative_scenario = serde_json::to_value(req.scenario.as_ref().unwrap()).unwrap();
        negative_scenario["script"]["steps"][0]["args"]["parameters"]["value"] = json!({});
        negative_scenario["script"]["allowed_tools"]
            .as_array_mut()
            .unwrap()
            .push(json!("create_box"));
        negative_scenario["script"]["steps"].as_array_mut().unwrap().insert(0,json!({"id":"sentinel","tool":"create_box","args":{
            "centre":{"expr":"literal","value":[5,0,0]},"half_extents":{"expr":"literal","value":[1,1,1]}
        }}));
        negative_scenario["expect_failure"] = json!(true);
        negative_scenario["undo_redo"] = json!(false);
        let mut empty_scenario = negative_scenario.clone();
        empty_scenario["script"]["steps"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        let empty_request = Request {
            scenario: Some(serde_json::from_value(empty_scenario).unwrap()),
            asset: req.asset.clone(),
            drivers: default(),
            placement: default(),
            edits: vec![],
            inspection_snapshot: None,
        };
        let empty_context = Context {
            classes: context.classes.clone(),
            ..Default::default()
        };
        let empty = evaluate_in(empty_request, empty_context).unwrap();
        assert!(empty.parts.is_empty());
        assert_eq!(empty.report["empty_scene"], true);
        assert_eq!(empty.report["final_state"]["element_ids"], json!([]));
        assert!(empty.report["execution_error"]
            .as_str()
            .unwrap()
            .contains("confirmed"));
        let mut untouched = World::new();
        let initial_entity_count = untouched.entities().len();
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        start(&mut untouched, empty, sender);
        let (report, image) = receiver.try_recv().unwrap().unwrap();
        assert!(image.is_none());
        assert_eq!(report["rendered"], false);
        assert_eq!(
            report["presentation"]["kind"],
            "expected_rejection_no_geometry"
        );
        assert!(!untouched.contains_resource::<PendingRender>());
        assert_eq!(untouched.entities().len(), initial_entity_count);
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        let mut invalid = report;
        invalid["execution_error"] = Value::Null;
        start(
            &mut untouched,
            Scene {
                parts: vec![],
                bounds: crate::authored_entity::EntityBounds {
                    min: Vec3::ZERO,
                    max: Vec3::ONE,
                },
                report: invalid,
            },
            sender,
        );
        assert!(receiver.try_recv().unwrap().is_err());
        let negative = Request {
            scenario: Some(serde_json::from_value(negative_scenario).unwrap()),
            asset: req.asset.clone(),
            drivers: default(),
            placement: default(),
            edits: vec![],
            inspection_snapshot: None,
        };
        let negative_context = Context {
            classes: context.classes.clone(),
            ..Default::default()
        };
        let rejected = evaluate_in(negative, negative_context).unwrap();
        assert!(
            rejected.report["execution_error"]
                .as_str()
                .unwrap()
                .contains("confirmed"),
            "{}",
            rejected.report["execution_error"]
        );
        assert_eq!(
            rejected.report["before_persistence"]["element_ids"],
            json!([0]),
            "{}",
            rejected.report
        );
        assert_eq!(
            rejected.report["final_state"]["element_ids"],
            json!([0]),
            "{}",
            rejected.report
        );
        assert_eq!(
            rejected.report["final_state"],
            rejected.report["round_trip"]["after_reopen"]
        );
        let scene = evaluate_in(req, context).unwrap();
        assert_eq!(scene.report["calls"][0]["result"]["steps_run_count"], 1);
        assert_eq!(
            scene.report["history"]["after_undo"]["element_ids"],
            json!([])
        );
        assert_eq!(
            scene.report["before_persistence"],
            scene.report["history"]["after_redo"]
        );
        assert_eq!(
            scene.report["final_state"],
            scene.report["round_trip"]["after_reopen"]
        );
        assert!(!scene.parts.is_empty());
    }

    #[test]
    fn native_dispatch_failed_transaction_restores_parametric_store_and_geometry() {
        let req = request();
        let mut world = scenario::empty_world(Context::default());
        knowledge_cloud::install_payload(
            &mut world,
            knowledge_cloud::AdmittedAsset {
                scope_sha256: None,
                asset: req.asset,
                revision: "fixture".into(),
            },
        )
        .unwrap();
        let before = serde_json::to_value(world.resource::<ParametricStore>()).unwrap();
        let transaction = crate::plugins::history::HistoryTransaction::begin(&mut world).unwrap();
        let created = model_api::document_dispatch::execute(
            &mut world,
            "parametric.create",
            json!({"type_id":"test.candidate"}),
            false,
        )
        .unwrap();
        assert_eq!(created["element_ids"].as_array().unwrap().len(), 1);
        assert!(model_api::document_dispatch::execute(
            &mut world,
            "set_property",
            json!({"element_id":999,"property_name":"missing","value":0}),
            false
        )
        .is_err());
        transaction.finish(&mut world, false);
        assert_eq!(
            world
                .query::<&crate::plugins::identity::ElementId>()
                .iter(&world)
                .count(),
            0
        );
        assert_eq!(
            serde_json::to_value(world.resource::<ParametricStore>()).unwrap(),
            before
        );
        assert_eq!(
            world
                .resource::<crate::plugins::history::History>()
                .undo_stack_len(),
            0
        );
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
            assert!(evaluate(req, vec![])
                .err()
                .unwrap()
                .to_lowercase()
                .contains("unsupported"));
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
