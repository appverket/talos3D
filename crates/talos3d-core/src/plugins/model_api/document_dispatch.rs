//! In-process adapter to the generated MCP router. No parallel tool semantics.
//! The positive capability list excludes host I/O, network, publication, session
//! mutation and open-ended command invocation. It is also the discovery source.
use super::*;
use std::future::Future;
use std::task::{Context, Poll, Wake, Waker};

const ATOMIC_WRITES: &[&str] = &[
    "create_box",
    "create_entity",
    "set_property",
    "resolve_obligation",
    "delete_entities",
    "create_assembly",
    "definition.create",
    "definition.instantiate",
    "occurrence.place",
    "parametric.create",
    "parametric.set_driver",
];
const READS: &[&str] = &[
    "get_entity_creation_schema",
    "inspect_element_geometry",
    "model_summary",
    "get_obligations",
    "get_refinement_state",
    "get_claim_grounding",
    "get_authoring_provenance",
    "run_validation_v2",
    "get_world_aabb",
    "check_overlaps",
    "check_floating",
    "check_clearance",
    "get_assembly",
    "list_assemblies",
    "list_assembly_members",
    "query_relations",
];
const SCENARIO_ONLY: &[&str] = &["instantiate_recipe", "promote_refinement"];

pub(crate) fn atomic_step(name: &str) -> bool {
    ATOMIC_WRITES.contains(&name) || READS.contains(&name)
}
pub(crate) fn read_only(name: &str) -> bool {
    READS.contains(&name)
}

/// One scenario shares this budget with all nested recipe calls.
#[derive(Resource)]
pub(crate) struct DocumentCallBudget(pub usize);

fn server() -> (server::ModelApiServer, mpsc::Receiver<ModelApiRequest>) {
    let (sender, receiver) = mpsc::channel();
    let mut server = server::ModelApiServer::new(ModelApiRequestSender::new(sender));
    server.document_execution = true;
    server.profile_state = SessionProfileState::new(CapabilityProfile::Full);
    (server, receiver)
}

pub(crate) fn register_session_tools(
    registry: &mut crate::curation::procedural_session::SessionToolRegistry,
) {
    for name in ATOMIC_WRITES.iter().chain(READS) {
        let tool = crate::curation::McpToolId::new(*name);
        if registry.get(&tool).is_none() {
            registry.register(crate::curation::procedural_session::SessionToolDescriptor {
                tool,
                mutates: !read_only(name),
                default_stub: None,
                creates_obligations: Vec::new(),
                satisfies_obligation_ids: Vec::new(),
            });
        }
    }
}

pub(crate) fn contract() -> Value {
    let (server, _) = server();
    let tools: Vec<_> = browser_tool_definitions(&server)
        .into_iter()
        .filter(|t| {
            t["name"]
                .as_str()
                .is_some_and(|n| atomic_step(n) || SCENARIO_ONLY.contains(&n))
        })
        .collect();
    json!({"tools":tools,"atomic_recipe_tools":ATOMIC_WRITES,"read_only_tools":READS,
        "scenario_only_tools":SCENARIO_ONLY,
        "result_contract":"Native MCP JSON; legacy script create_box/create_entity/set_property expose CommandResult.output; occurrence.place exposes element_id.",
        "limits":{"scenario_steps":32,"nested_native_calls":128},
        "notice":"Only native document operations. No host I/O, remote calls, publication or arbitrary commands. Scenario observations are not independent acceptance."})
}

pub(crate) fn initialize_commands(world: &mut World) {
    if !world.contains_resource::<crate::plugins::command_registry::CommandRegistry>() {
        let mut app = App::new();
        register_model_api_primitive_commands(&mut app);
        world.insert_resource(
            app.world_mut()
                .remove_resource::<crate::plugins::command_registry::CommandRegistry>()
                .unwrap(),
        );
    }
}

pub(crate) fn execute(
    world: &mut World,
    name: &str,
    args: Value,
    scenario: bool,
) -> Result<Value, String> {
    if !atomic_step(name) && !(scenario && SCENARIO_ONLY.contains(&name)) {
        return Err(format!(
            "Unsupported native document tool '{name}'; inspect the runtime execution contract"
        ));
    }
    if let Some(mut budget) = world.get_resource_mut::<DocumentCallBudget>() {
        if budget.0 == 0 {
            return Err("Scenario nested native call budget exhausted".into());
        }
        budget.0 -= 1;
    }
    initialize_commands(world);
    let (server, receiver) = server();
    // Poll locally: all admitted handlers are channel round-trips, never I/O.
    // Entering (rather than block_on) supports nested recipe dispatch and works
    // on both the app thread and the candidate blocking worker.
    #[cfg(not(target_arch = "wasm32"))]
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    #[cfg(not(target_arch = "wasm32"))]
    let _enter = RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("native dispatch timer context")
        })
        .enter();
    struct NoWake;
    impl Wake for NoWake {
        fn wake(self: Arc<Self>) {}
    }
    let waker = Waker::from(Arc::new(NoWake));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(server.call_without_peer(name, args));
    for _ in 0..16 {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(result) => {
                let result = result.map_err(|e| e.to_string())?;
                if result.is_error == Some(true) {
                    return Err(format!(
                        "Native tool rejected operation: {:?}",
                        result.content
                    ));
                }
                let value: Value = result.into_typed().map_err(|e| e.to_string())?;
                return Ok(match name {
                    "create_box" | "create_entity" | "set_property" => {
                        value.get("output").cloned().unwrap_or(value)
                    }
                    "occurrence.place" if value.is_u64() => json!({"element_id":value}),
                    _ => value,
                });
            }
            Poll::Pending => {
                let mut dispatched = false;
                while let Ok(request) = receiver.try_recv() {
                    dispatched = true;
                    request::handle_model_api_request(world, request);
                }
                if !dispatched {
                    return Err("Native document tool requested asynchronous external work; unavailable in bounded execution".into());
                }
            }
        }
    }
    Err("Native document dispatch round-trip bound exceeded".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_and_dispatch_share_one_bounded_capability_set() {
        let c = contract();
        let names: std::collections::BTreeSet<_> = c["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        let expected: std::collections::BTreeSet<_> = ATOMIC_WRITES
            .iter()
            .chain(READS)
            .chain(SCENARIO_ONLY)
            .copied()
            .collect();
        assert_eq!(names, expected);
        for tool in c["tools"].as_array().unwrap() {
            assert!(tool["inputSchema"].is_object());
        }
        let mut w = World::new();
        let before = w.entities().len();
        for denied in [
            "save_project",
            "load_project",
            "import_file",
            "invoke_command",
            "install_knowledge_asset",
            "set_session_profile",
        ] {
            assert!(execute(&mut w, denied, json!({}), true)
                .unwrap_err()
                .contains("Unsupported"));
        }
        assert!(!w.contains_resource::<crate::plugins::command_registry::CommandRegistry>());
        w.insert_resource(DocumentCallBudget(0));
        assert!(execute(&mut w, "create_box", json!({}), true)
            .unwrap_err()
            .contains("budget exhausted"));
        assert_eq!(w.entities().len(), before);
    }
}
