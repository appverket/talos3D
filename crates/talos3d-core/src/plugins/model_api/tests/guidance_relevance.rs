//! TALOS-cyczbfsk: the onboarding must-read set is selected for the negotiated
//! task from declarative tags, ids and discovery aliases. Nothing leaves the
//! required set silently: every non-selected resource is listed as optional
//! with its retrieval tool. All vocabulary below is synthetic test data.
use super::*;
use crate::plugins::agent_skills::{AgentSkill, AgentSkillId, AgentSkillTrustLevel};
use crate::plugins::authoring_guidance::{
    AuthoringGuidance, DiscoveryAliases, GuidanceCardInfo, GuidanceCardRegistry,
};

const SUBSTRATE: [&str; 4] = [
    "dkg.snapshot.start",
    "dkg.authoring_run_contract",
    "dkg.trajectory_eval",
    "dkg.no_curated_path",
];

fn owned(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

/// Cards: `(id, tags)`. Skills: `(id, tags, must_read_at_bootstrap)`.
fn relevance_world(cards: &[(&str, &[&str])], skills: &[(&str, &[&str], bool)]) -> World {
    let mut world = init_model_api_test_world();
    world.insert_resource(ModelApiRuntimeInfo {
        instance_id: "guidance-relevance".into(),
        app_name: "test".into(),
        pid: 1,
        http_host: "127.0.0.1".into(),
        http_port: 24845,
        http_url: "http://127.0.0.1:24845/mcp".into(),
        registry_path: String::new(),
        started_at_unix_ms: 1,
        requested_port: None,
    });
    let mut aliases = DiscoveryAliases::default();
    aliases.register("alpha_system", &["alpha", "apex"]);
    world.insert_resource(aliases);
    let mut registry = GuidanceCardRegistry::default();
    for (id, tags) in cards {
        registry.insert(GuidanceCardInfo {
            id: id.to_string(),
            title: id.to_string(),
            task_tags: owned(tags),
            summary: format!("Card {id}."),
            ..Default::default()
        });
    }
    world.insert_resource(registry);
    let mut registry = AgentSkillRegistry::default();
    for (id, tags, mandatory) in skills {
        registry.insert(AgentSkill {
            id: AgentSkillId(id.to_string()),
            title: id.to_string(),
            summary: format!("Skill {id}."),
            task_tags: owned(tags),
            must_read_at_bootstrap: *mandatory,
            body_markdown: "Follow the procedure.".into(),
            trust_level: AgentSkillTrustLevel::Shipped,
            source_path: None,
            referenced_tool_ids: vec![],
            required_tool_ids: vec![],
            forbidden_tool_ids: vec![],
            validation_tool_ids: vec![],
            success_criteria: vec![],
            stop_conditions: vec![],
            screenshot_requirements: vec![],
            common_failure_modes: vec![],
            regression_prompt_ids: vec![],
            next_skill_ids: vec![],
        });
    }
    world.insert_resource(registry);
    world.insert_resource(AuthoringGuidance {
        guidance_id: "domainx.contract".into(),
        version: 1,
        prompt_text: "Canonical rule.".into(),
        ..Default::default()
    });
    world
}

fn serve(world: World) -> (ModelApiServer, tokio::task::JoinHandle<()>) {
    let (sender, receiver) = mpsc::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let mut world = world;
        while let Ok(request) = receiver.recv() {
            handle_model_api_request(&mut world, request);
        }
    });
    (
        ModelApiServer::new(ModelApiRequestSender::new(sender)),
        worker,
    )
}

async fn negotiate(server: &ModelApiServer, task: Option<&str>) -> AgentWelcome {
    server
        .negotiate_agent_session_tool(Parameters(AgentHello {
            task: task.map(str::to_string),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_typed()
        .unwrap()
}

fn optional(welcome: &AgentWelcome) -> Vec<(String, String, OptionalReason)> {
    welcome
        .capability_snapshot
        .optional_guidance
        .iter()
        .map(|entry| (entry.kind.clone(), entry.id.clone(), entry.reason))
        .collect()
}

fn registry_part(welcome: &AgentWelcome) -> Vec<String> {
    welcome
        .required_guidance_card_ids
        .iter()
        .filter(|id| !SUBSTRATE.contains(&id.as_str()))
        .cloned()
        .collect()
}

const MIXED_CARDS: &[(&str, &[&str])] = &[
    ("c.alpha", &["alpha_system"]),
    ("c.beta", &["beta"]),
    ("c.domain", &["domainx", "curation"]),
    ("c.untagged", &[]),
];
const MIXED_SKILLS: &[(&str, &[&str], bool)] = &[
    ("s.alpha", &["apex"], true),
    ("s.beta", &["beta"], true),
    ("s.untagged", &[], true),
    ("s.recommended", &["beta"], false),
];

#[tokio::test]
async fn task_selects_relevant_cards_and_skills_and_lists_the_rest_as_optional() {
    let (server, worker) = serve(relevance_world(MIXED_CARDS, MIXED_SKILLS));
    let welcome = negotiate(&server, Some("build a small apex structure")).await;

    // Substrate stays first and required whatever the task says.
    assert_eq!(
        &welcome.required_guidance_card_ids[..4],
        &SUBSTRATE.map(String::from)
    );
    // `apex` reaches `alpha_system` through the domain's aliases; the domain
    // namespace tag and the untagged card apply to every task.
    assert_eq!(
        registry_part(&welcome),
        owned(&["c.alpha", "c.domain", "c.untagged"])
    );
    assert_eq!(
        welcome.capability_snapshot.must_read_agent_skill_ids,
        owned(&["s.alpha", "s.untagged"])
    );
    assert_eq!(
        optional(&welcome),
        vec![
            (
                "agent_skill".into(),
                "s.beta".into(),
                OptionalReason::NotSelectedForTask
            ),
            (
                "guidance_card".into(),
                "c.beta".into(),
                OptionalReason::NotSelectedForTask
            ),
        ]
    );

    // The packet delivers only the required set, lists the optional resources
    // with their retrieval tool, and they block nothing.
    let (pages, resources) = drain_guidance_packet(&server).await;
    let manifest = pages[0]["manifest"].as_array().unwrap();
    let entry = |id: &str| {
        manifest
            .iter()
            .find(|entry| entry["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from manifest"))
            .clone()
    };
    assert_eq!(entry("c.beta")["required"], false);
    assert_eq!(entry("c.beta")["reason"], "not_selected_for_task");
    assert_eq!(
        entry("c.beta")["retrieval"],
        json!({"tool": "get_guidance_card", "arguments": {"card_id": "c.beta"}})
    );
    assert_eq!(
        entry("s.beta")["retrieval"],
        json!({"tool": "get_agent_skill", "arguments": {"skill_id": "s.beta"}})
    );
    assert_eq!(entry("c.alpha")["required"], true);
    assert!(!resources.contains_key(&card_key("c.beta")));
    assert!(!resources.contains_key(&skill_key("s.beta")));
    assert!(resources.contains_key(&card_key("c.alpha")));
    assert_eq!(
        server.guidance_state.snapshot().unwrap().phase,
        GuidancePhase::Discover
    );

    // The snapshot tool keeps the session's selection and still lists the
    // optional resources; an optional card remains retrievable on demand.
    let snapshot: CapabilitySnapshotInfo = server
        .get_capability_snapshot_tool(Parameters(CapabilitySnapshotRequest { expanded: false }))
        .await
        .unwrap()
        .into_typed()
        .unwrap();
    assert!(snapshot.must_read_guidance_card_ids.is_empty());
    assert_eq!(
        snapshot.already_read_guidance_card_ids,
        welcome.required_guidance_card_ids
    );
    assert_eq!(
        snapshot.optional_guidance,
        welcome.capability_snapshot.optional_guidance
    );
    let card: serde_json::Value = server
        .get_guidance_card_tool(Parameters(GetGuidanceCardRequest {
            card_id: "c.beta".into(),
            cursor: None,
        }))
        .await
        .unwrap()
        .into_typed()
        .unwrap();
    assert!(card.to_string().contains("Card c.beta."));
    drop(server);
    worker.await.unwrap();
}

#[tokio::test]
async fn without_a_task_every_item_stays_required() {
    for task in [None, Some(""), Some("a b")] {
        let (server, worker) = serve(relevance_world(MIXED_CARDS, MIXED_SKILLS));
        let welcome = negotiate(&server, task).await;
        assert_eq!(
            registry_part(&welcome),
            owned(&["c.alpha", "c.beta", "c.domain", "c.untagged"]),
            "task {task:?}"
        );
        assert_eq!(
            welcome.capability_snapshot.must_read_agent_skill_ids,
            owned(&["s.alpha", "s.beta", "s.untagged"])
        );
        assert!(optional(&welcome).is_empty());
        drop(server);
        worker.await.unwrap();
    }
}

#[tokio::test]
async fn untagged_content_is_required_for_every_task() {
    let cards: &[(&str, &[&str])] = &[("u.one", &[]), ("u.two", &[])];
    let skills: &[(&str, &[&str], bool)] = &[("s.one", &[], true)];
    let mut selections = Vec::new();
    for task in [
        None,
        Some("build a small apex structure"),
        Some("zzz unrelated words"),
    ] {
        let (server, worker) = serve(relevance_world(cards, skills));
        let welcome = negotiate(&server, task).await;
        assert!(optional(&welcome).is_empty(), "task {task:?}");
        selections.push((
            welcome.required_guidance_card_ids.clone(),
            welcome
                .capability_snapshot
                .must_read_agent_skill_ids
                .clone(),
        ));
        drop(server);
        worker.await.unwrap();
    }
    assert!(selections.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(selections[0].1, owned(&["s.one"]));
}

/// The base budget used to truncate registry cards alphabetically, so a
/// relevant card sorting late was silently dropped. It now ranks first and the
/// overflow is listed.
#[tokio::test]
async fn budget_keeps_the_most_relevant_cards_and_lists_the_overflow() {
    let ids = (0..10)
        .map(|index| format!("a.untagged.{index}"))
        .collect::<Vec<_>>();
    let mut cards = ids
        .iter()
        .map(|id| (id.as_str(), &[][..]))
        .collect::<Vec<(&str, &[&str])>>();
    cards.push(("z.relevant", &["alpha"]));
    let (server, worker) = serve(relevance_world(&cards, &[]));
    let welcome = negotiate(&server, Some("an apex task")).await;
    let registry = registry_part(&welcome);
    assert_eq!(registry.first().map(String::as_str), Some("z.relevant"));
    assert_eq!(
        registry.len(),
        8,
        "base budget is 12 including 4 substrate cards"
    );
    let overflow = optional(&welcome);
    assert_eq!(overflow.len(), 3);
    assert!(overflow
        .iter()
        .all(|(kind, _, reason)| kind == "guidance_card"
            && *reason == OptionalReason::BootstrapBudget));
    // Every installed card is either required or listed.
    let mut covered = registry;
    covered.extend(overflow.into_iter().map(|(_, id, _)| id));
    covered.sort();
    let mut expected = ids.clone();
    expected.push("z.relevant".into());
    assert_eq!(covered, expected);
    drop(server);
    worker.await.unwrap();
}
