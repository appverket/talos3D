//! Bounded native document scenarios. No user document, storage or network is
//! available here; commands and the native codec are the production handlers.
use super::*;
use crate::curation::{
    authoring_script::{AuthoringScript, MutationScope, ScriptInstruction},
    replay::{ToolCall, ToolDispatchError, ToolDispatcher},
};
use crate::plugins::procedural_session_mcp::SessionStepExecutor;
use crate::plugins::{
    history::{History, PendingCommandQueue},
    identity::{ElementId, ElementIdAllocator},
    modeling::{
        assembly::{AssemblyFactory, RelationFactory},
        definition::{DefinitionLibraryRegistry, DefinitionRegistry},
        group::GroupFactory,
        occurrence::{ChangedDefinitions, OccurrenceFactory, RepresentationCache},
        primitives::BoxPrimitive,
    },
    persistence,
};

/// An observation scenario, not independent acceptance. Uses the existing
/// AuthoringScript argument/binding format, at most 32 flat calls. Supported:
/// create_box, create_entity (box/profile_extrusion only), parametric.create,
/// parametric.set_driver, create_assembly, delete_entities, set_property,
/// model_summary. No files, imports, native function references or publication.
#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub script: AuthoringScript,
    #[serde(default)]
    pub parameters: serde_json::Map<String, Value>,
    /// Native encode/decode, entirely in memory, after the scenario.
    #[serde(default)]
    pub round_trip: bool,
    /// Undo then redo the final command and report both actual states.
    #[serde(default)]
    pub undo_redo: bool,
}

fn empty_world(context: Context) -> World {
    use crate::plugins::*;
    let mut w = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<ProfileExtrusion>::new());
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    registry.register_factory(AssemblyFactory);
    registry.register_factory(RelationFactory);
    registry.register_factory(GroupFactory);
    registry.register_factory(OccurrenceFactory);
    for c in context.classes {
        registry.register_element_class(c);
    }
    for c in context.assembly_types {
        registry.register_assembly_type(c);
    }
    for c in context.relation_types {
        registry.register_relation_type(c);
    }
    w.insert_resource(registry);
    w.init_resource::<ElementIdAllocator>();
    w.init_resource::<ParametricStore>();
    w.init_resource::<History>();
    w.init_resource::<PendingCommandQueue>();
    use crate::plugins::commands::*;
    macro_rules! messages { ($($t:ty),*) => { $(w.init_resource::<Messages<$t>>();)* }; }
    messages!(
        BeginCommandGroup,
        EndCommandGroup,
        CreateEntityCommand,
        CreateBoxCommand,
        CreateCylinderCommand,
        CreateSphereCommand,
        CreatePlaneCommand,
        CreatePolylineCommand,
        CreateTriangleMeshCommand,
        DeleteEntitiesCommand,
        ResolvedDeleteEntitiesCommand,
        ApplyEntityChangesCommand
    );
    w.init_resource::<document_state::DocumentState>();
    w.init_resource::<document_properties::DocumentProperties>();
    w.init_resource::<persistence::OpaquePersistedEntities>();
    w.init_resource::<layers::LayerRegistry>();
    w.init_resource::<materials::MaterialRegistry>();
    w.init_resource::<materials::TextureRegistry>();
    w.init_resource::<DefinitionRegistry>();
    w.init_resource::<DefinitionLibraryRegistry>();
    w.init_resource::<ChangedDefinitions>();
    w.init_resource::<RepresentationCache>();
    w.init_resource::<named_views::NamedViewRegistry>();
    w.init_resource::<property_edit::PropertyEditState>();
    w.init_resource::<transform::TransformState>();
    w.init_resource::<NextState<tools::ActiveTool>>();
    w.init_resource::<Assets<Mesh>>();
    w.init_resource::<Assets<StandardMaterial>>();
    w.init_resource::<authored_edit_plan::AuthoredEditPlanRegistry>();
    w.init_resource::<authored_edit_plan::requests::EditRequestRegistry>();
    w
}

fn ids(w: &mut World) -> Vec<u64> {
    let mut ids: Vec<_> = w.query::<&ElementId>().iter(w).map(|e| e.0).collect();
    ids.sort_unstable();
    ids
}
fn state(w: &mut World) -> Result<Value, String> {
    let element_ids = ids(w);
    if element_ids.len() > 128 {
        return Err("Scenario exceeds 128 authored entities".into());
    }
    let registry = w.resource::<CapabilityRegistry>();
    let entities: Vec<_> = w.iter_entities().filter_map(|e| {
        let id = e.get::<ElementId>()?;
        let snapshot = registry.capture_edit_snapshot(&e, w)?;
        Some(json!({"element_id":id.0,"type":snapshot.type_name(),"data":snapshot.to_persisted_json(),"semantic":persistence::capture_semantic_sidecars(&e)}))
    }).collect();
    Ok(json!({"element_ids":element_ids,"entities":entities}))
}

struct Dispatcher<'a> {
    world: &'a mut World,
    calls: Vec<Value>,
}
impl ToolDispatcher for Dispatcher<'_> {
    fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
        let error = |s: String| ToolDispatchError::new("scenario_step_failed", s);
        if self.calls.len() >= 32 {
            return Err(error("Scenario permits 32 calls".into()));
        }
        if !matches!(
            call.mutation_scope,
            MutationScope::ProjectRoot | MutationScope::None
        ) {
            return Err(error("Scenario scope must be project_root or none".into()));
        }
        if matches!(call.mutation_scope, MutationScope::None)
            && call.tool.as_str() != "model_summary"
        {
            return Err(error("A no-mutation scenario cannot dispatch edits".into()));
        }
        let args = Value::Object(call.args.clone());
        let result: Result<Value, String> = (|| match call.tool.as_str() {
            "create_entity" => {
                if !matches!(
                    args.get("type").and_then(Value::as_str),
                    Some("box" | "profile_extrusion")
                ) {
                    return Err("Scenario create_entity supports box and profile_extrusion; other factories are capability gaps".into());
                }
                model_api::ModelApiStepExecutor
                    .execute(self.world, call.tool, call.args)
                    .map_err(|e| e.message)
            }
            "create_box" | "set_property" | "model_summary" => model_api::ModelApiStepExecutor
                .execute(self.world, call.tool, call.args)
                .map_err(|e| e.message),
            "parametric.create" => {
                let req: parametric_mcp::CreateParametricRequest =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                if req.representation.is_some() {
                    return Err("Scenario creates only the staged candidate; inline type definitions are denied".into());
                }
                serde_json::to_value(parametric_mcp::world_create(self.world, req)?)
                    .map_err(|e| e.to_string())
            }
            "parametric.set_driver" => {
                let req = serde_json::from_value(args).map_err(|e| e.to_string())?;
                serde_json::to_value(parametric_mcp::world_set_driver(self.world, req)?)
                    .map_err(|e| e.to_string())
            }
            "create_assembly" => {
                let req = serde_json::from_value(args).map_err(|e| e.to_string())?;
                serde_json::to_value(model_api::handle_create_assembly(self.world, req)?)
                    .map_err(|e| e.to_string())
            }
            "delete_entities" => {
                let elements = serde_json::from_value(
                    args.get("element_ids")
                        .cloned()
                        .ok_or("element_ids required")?,
                )
                .map_err(|e| e.to_string())?;
                Ok(json!({"deleted":model_api::handle_delete_entities(self.world, elements)?}))
            }
            _ => Err(format!(
                "Scenario tool {} is unsupported",
                call.tool.as_str()
            )),
        })();
        let result = result.map_err(error)?;
        let observed_ids = ids(self.world);
        if observed_ids.len() > 128 {
            return Err(error("Scenario exceeds 128 authored entities".into()));
        }
        self.calls
            .push(json!({"tool":call.tool.as_str(),"result":result,"element_ids":observed_ids}));
        Ok(result)
    }
}

pub fn evaluate(request: Request, scenario: Scenario, context: Context) -> Result<Scene, String> {
    let started = std::time::Instant::now();
    if request.inspection_snapshot.is_some() {
        return Err(
            "Legacy scenarios require candidate admission; legacy inspection is not admission"
                .into(),
        );
    }
    if request.asset.kind != "parametric_type.v1" {
        return Err("Native scenarios currently support parametric_type.v1 candidates; other kinds remain capability gaps".into());
    }
    if scenario.script.steps.len() > 32
        || scenario.script.steps.is_empty()
        || scenario
            .script
            .steps
            .iter()
            .any(|s| !matches!(s, ScriptInstruction::Call(_)))
        || !scenario.script.postconditions.is_empty()
    {
        return Err("Scenario requires 1..32 flat calls and no claimed postconditions; observations are not acceptance".into());
    }
    if serde_json::to_vec(&request.asset)
        .map_err(|e| e.to_string())?
        .len()
        > 1_048_576
    {
        return Err("Candidate exceeds scenario byte bound".into());
    }
    if serde_json::to_vec(&scenario)
        .map_err(|e| e.to_string())?
        .len()
        > 1_048_576
    {
        return Err("Scenario exceeds 1 MiB".into());
    }
    let checked = knowledge_cloud::check_candidate(knowledge_cloud::CandidateCheck {
        asset: request.asset.clone(),
        driver_cases: vec![request.drivers.clone()],
    })?;
    let members = checked["parametric_cases"][0]["members"]
        .as_array()
        .map_or(0, Vec::len);
    if !(1..=128).contains(&members) {
        return Err("Candidate requires 1..128 members".into());
    }
    let revision = checked["content_hash"]
        .as_str()
        .ok_or("Candidate hash missing")?
        .to_string();
    let mut w = empty_world(context);
    knowledge_cloud::install_payload(
        &mut w,
        knowledge_cloud::AdmittedAsset {
            scope_sha256: None,
            asset: request.asset,
            revision: revision.clone(),
        },
    )?;
    let mut dispatcher = Dispatcher {
        world: &mut w,
        calls: vec![],
    };
    crate::curation::replay::replay(
        &scenario.script,
        scenario.parameters,
        &mut dispatcher,
        &crate::curation::procedural_session::AlwaysPassOracle,
    )
    .map_err(|e| e.to_string())?;
    let calls = dispatcher.calls;
    let before = state(&mut w)?;
    let mut history = Value::Null;
    if scenario.undo_redo {
        if w.resource::<History>().undo_stack_len() == 0 {
            return Err("Scenario has no undoable command".into());
        }
        w.resource_mut::<PendingCommandQueue>().queue_undo();
        crate::plugins::commands::flush_queued_commands(&mut w);
        let undone = state(&mut w)?;
        w.resource_mut::<PendingCommandQueue>().queue_redo();
        crate::plugins::commands::flush_queued_commands(&mut w);
        history = json!({"after_undo":undone,"after_redo":state(&mut w)?});
    }
    let mut roundtrip = Value::Null;
    if scenario.round_trip {
        let bytes = persistence::capture_project_file_bytes(&mut w)?;
        if bytes.len() > 4_194_304 {
            return Err("Scenario native snapshot exceeds 4 MiB".into());
        }
        persistence::open_project_bytes(&mut w, &bytes)?;
        roundtrip = json!({"bytes":bytes.len(),"after_reopen":state(&mut w)?});
    }
    let final_state = state(&mut w)?;
    let mut parts = vec![];
    fn meshes<P: Primitive + MeshGenerator>(w: &mut World, parts: &mut Vec<(Mesh, Transform)>) {
        let mut q = w.query::<(&P, Option<&ShapeRotation>)>();
        for (p, r) in q.iter(w) {
            let r = r.copied().unwrap_or_default().0;
            parts.push((p.to_bevy_mesh(r), p.entity_transform(r)));
        }
    }
    meshes::<BoxPrimitive>(&mut w, &mut parts);
    meshes::<ProfileExtrusion>(&mut w, &mut parts);
    let vertices: usize = parts.iter().map(|(m, _)| m.count_vertices()).sum();
    if parts.is_empty() || vertices > 100_000 {
        return Err("Scenario must produce 1..100000 render vertices".into());
    }
    let geometry_ids: Vec<_> = w
        .query::<(&ElementId, Option<&BoxPrimitive>, Option<&ProfileExtrusion>)>()
        .iter(&w)
        .filter(|(_, b, p)| b.is_some() || p.is_some())
        .map(|(id, _, _)| id.0)
        .collect();
    let observed = super::observations(&w, geometry_ids.clone())?;
    let aabb = model_api::handle_get_world_aabb(
        &w,
        model_api::GetWorldAabbRequest {
            element_ids: geometry_ids,
        },
    )?
    .combined
    .ok_or("Scenario bounds missing")?;
    let bounds = crate::authored_entity::EntityBounds {
        min: Vec3::from_array(aabb.min.map(|v| v as f32)),
        max: Vec3::from_array(aabb.max.map(|v| v as f32)),
    };
    if !bounds.min.is_finite()
        || !bounds.max.is_finite()
        || (bounds.max - bounds.min).length() > 100_000.
    {
        return Err("Scenario bounds invalid".into());
    }
    Ok(Scene {
        parts,
        bounds,
        report: json!({"schema":"talos3d.candidate-scenario-observation.v1","content_hash":revision,"calls":calls,"before_persistence":before,"history":history,"round_trip":roundtrip,"final_state":final_state,"geometry":observed,"mesh_vertices":vertices,"cpu_elapsed_ms":started.elapsed().as_secs_f64()*1000.,"model_modified":false,"installed":false,"released":false,"rendered":false,"limitations":["Curator-created synthetic scenario, not private-project evidence or independent qualification.","Only the advertised native tool and geometry subset is supported.","AABB overlaps and floating checks are approximate; no structural engineering approval."]}),
    })
}
