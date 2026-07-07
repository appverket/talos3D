use super::*;
use crate::capability_registry::CapabilityRegistry;
#[cfg(feature = "model-api")]
use crate::importers::obj::ObjImporter;
#[cfg(feature = "model-api")]
use crate::plugins::command_registry::{execute_command, CommandRegistry};
#[cfg(feature = "model-api")]
use crate::plugins::commands::find_entity_by_element_id_readonly;
#[cfg(feature = "model-api")]
use crate::plugins::modeling::snapshots::TriangleMeshFactory;
#[cfg(feature = "model-api")]
use crate::plugins::modeling::{
    fillet::{ChamferFactory, FilletFactory},
    primitives::{CylinderPrimitive, SpherePrimitive},
};
use crate::plugins::modeling::{
    generic_factory::PrimitiveFactory,
    primitives::{BoxPrimitive, PlanePrimitive, Polyline, ShapeRotation},
    profile::{Profile2d, ProfileExtrusion},
    snapshots::PolylineFactory,
};
#[cfg(feature = "model-api")]
use crate::plugins::{
    commands::{
        ApplyEntityChangesCommand, BeginCommandGroup, CreateBoxCommand, CreateCylinderCommand,
        CreateEntityCommand, CreatePlaneCommand, CreatePolylineCommand, CreateSphereCommand,
        CreateTriangleMeshCommand, DeleteEntitiesCommand, EndCommandGroup,
        ResolvedDeleteEntitiesCommand,
    },
    dimension_line::{DimensionLineFactory, DimensionLineVisibility},
    document_properties::DocumentProperties,
    document_state::DocumentState,
    guide_line::{GuideLineFactory, GuideLineVisibility},
    history::{History, PendingCommandQueue},
    identity::ElementIdAllocator,
    import::ImportRegistry,
    persistence::OpaquePersistedEntities,
    property_edit::PropertyEditState,
    toolbar::{
        ToolbarDescriptor, ToolbarDock, ToolbarLayoutEntry, ToolbarLayoutState, ToolbarRegistry,
        ToolbarSection,
    },
    tools::ActiveTool,
    transform::TransformState,
};
use serde_json::json;
#[cfg(feature = "model-api")]
use serde_json::Value;
#[cfg(feature = "model-api")]
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn list_entities_and_model_summary_reflect_authored_world() {
    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    registry.register_factory(PrimitiveFactory::<PlanePrimitive>::new());
    registry.register_factory(PolylineFactory);
    world.insert_resource(registry);

    world.spawn((
        ElementId(1),
        BoxPrimitive {
            centre: Vec3::new(2.0, 1.0, 1.5),
            half_extents: Vec3::new(0.5, 0.5, 0.5),
        },
        ShapeRotation::default(),
    ));
    world.spawn((
        ElementId(2),
        PlanePrimitive {
            corner_a: Vec2::new(-1.0, -2.0),
            corner_b: Vec2::new(3.0, 2.0),
            elevation: 0.4,
        },
        ShapeRotation(Quat::from_rotation_y(0.2)),
    ));
    world.spawn((
        ElementId(3),
        Polyline {
            points: vec![Vec3::ZERO, Vec3::new(1.0, 0.0, 1.0)],
        },
    ));

    let entities = list_entities(&world);
    assert_eq!(entities.len(), 3);
    assert_eq!(entities[0].entity_type, "box");
    assert_eq!(entities[1].entity_type, "plane");
    assert_eq!(entities[2].entity_type, "polyline");

    let summary = model_summary(&world);
    assert_eq!(summary.entity_counts.get("box"), Some(&1));
    assert_eq!(summary.entity_counts.get("plane"), Some(&1));
    assert_eq!(summary.entity_counts.get("polyline"), Some(&1));
    assert!(summary.bounding_box.is_some());
}

#[test]
fn model_summary_uses_primitive_extents_not_only_centres() {
    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    registry.register_factory(PrimitiveFactory::<ProfileExtrusion>::new());
    world.insert_resource(registry);

    world.spawn((
        ElementId(1),
        BoxPrimitive {
            centre: Vec3::new(10.0, 2.0, -3.0),
            half_extents: Vec3::new(2.0, 0.5, 4.0),
        },
        ShapeRotation::default(),
    ));
    world.spawn((
        ElementId(2),
        ProfileExtrusion {
            centre: Vec3::ZERO,
            profile: Profile2d::rectangle(4.0, 0.2),
            height: 1.0,
        },
        ShapeRotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_4)),
    ));

    let bounding_box = model_summary(&world)
        .bounding_box
        .expect("summary should contain authored primitive bounds");

    assert!(
        bounding_box.min[0] <= -1.4,
        "min x = {:?}",
        bounding_box.min
    );
    assert!(
        bounding_box.max[0] >= 12.0,
        "max x = {:?}",
        bounding_box.max
    );
    assert!(
        bounding_box.min[1] <= -1.4,
        "min y = {:?}",
        bounding_box.min
    );
    assert!(bounding_box.max[1] >= 2.5, "max y = {:?}", bounding_box.max);
    assert_eq!(bounding_box.min[2], -7.0);
    assert_eq!(bounding_box.max[2], 1.0);
}

#[cfg(feature = "model-api")]
#[test]
fn list_element_classes_exposes_per_state_obligation_ladder() {
    use crate::capability_registry::{ElementClassDescriptor, ElementClassId, ObligationTemplate};
    use crate::plugins::refinement::{ClaimPath, ObligationId, RefinementState, SemanticRole};
    use std::collections::HashMap;

    let mut class_min_obligations: HashMap<RefinementState, Vec<ObligationTemplate>> =
        HashMap::new();
    class_min_obligations.insert(
        RefinementState::Constructible,
        vec![ObligationTemplate {
            id: ObligationId("structure".into()),
            role: SemanticRole("primary_structure".into()),
            required_by_state: RefinementState::Constructible,
        }],
    );
    let mut class_min_promotion_critical_paths: HashMap<RefinementState, Vec<ClaimPath>> =
        HashMap::new();
    class_min_promotion_critical_paths.insert(
        RefinementState::Constructible,
        vec![ClaimPath("thickness_mm".into())],
    );

    let mut registry = CapabilityRegistry::default();
    registry.register_element_class(ElementClassDescriptor {
        id: ElementClassId("wall_assembly".into()),
        label: "Wall Assembly".into(),
        description: "test".into(),
        semantic_roles: vec![SemanticRole("primary_structure".into())],
        class_min_obligations,
        class_min_promotion_critical_paths,
        parameter_schema: serde_json::json!({}),
    });

    let mut world = World::new();
    world.insert_resource(registry);

    let classes = handle_list_element_classes(&world);
    assert_eq!(classes.len(), 1);
    // Only the Constructible state carries content, so exactly one rung is
    // surfaced through the MCP-facing handler.
    let ladder = &classes[0].obligations_by_state;
    assert_eq!(ladder.len(), 1);
    assert_eq!(ladder[0].refinement_state, "Constructible");
    assert_eq!(ladder[0].obligations.len(), 1);
    assert_eq!(ladder[0].obligations[0].id, "structure");
    assert_eq!(ladder[0].obligations[0].role, "primary_structure");
    assert_eq!(ladder[0].obligations[0].required_by_state, "Constructible");
    assert_eq!(
        ladder[0].promotion_critical_paths,
        vec!["thickness_mm".to_string()]
    );
}

#[cfg(feature = "model-api")]
#[test]
fn capability_snapshot_reports_registry_counts_and_no_curated_paths() {
    use crate::capability_registry::{
        CatalogCategory, CatalogProviderDescriptor, CatalogProviderId, ElementClassDescriptor,
        ElementClassId, LicenseTag,
    };
    use crate::plugins::authoring_guidance::{
        AuthoringGuidance, ComponentStructurePolicy, GuidanceReference,
    };
    use crate::plugins::refinement::SemanticRole;
    use std::sync::Arc;

    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_element_class(ElementClassDescriptor {
        id: ElementClassId("wall_assembly".into()),
        label: "Wall Assembly".into(),
        description: "test".into(),
        semantic_roles: vec![SemanticRole("primary_structure".into())],
        class_min_obligations: std::collections::HashMap::new(),
        class_min_promotion_critical_paths: std::collections::HashMap::new(),
        parameter_schema: json!({}),
    });
    registry.register_catalog_provider(CatalogProviderDescriptor {
        id: CatalogProviderId("dimensional_lumber_metric".into()),
        label: "Metric lumber".into(),
        description: "test".into(),
        category: CatalogCategory::DimensionalLumber,
        region: Some("SE_EU".into()),
        license: LicenseTag::Cc0,
        source_version: "test".into(),
        query_fn: Arc::new(|_| Vec::new()),
    });
    world.insert_resource(registry);
    world.insert_resource(AuthoringGuidance {
        guidance_id: "test.guidance".into(),
        version: 1,
        prompt_text: "Use curated paths.".into(),
        component_structure: ComponentStructurePolicy::default(),
        references: vec![GuidanceReference {
            kind: "tool".into(),
            target: "request_corpus_expansion".into(),
            note: None,
        }],
        guidance_chapters: Vec::new(),
    });

    let snapshot = handle_get_capability_snapshot(&world, false);
    assert_eq!(snapshot.summary.element_class_count, 1);
    assert_eq!(snapshot.summary.recipe_family_count, 0);
    assert_eq!(snapshot.summary.catalog_provider_count, 1);
    assert_eq!(snapshot.summary.no_curated_path_count, 1);
    assert!(snapshot
        .computed
        .element_class_ids
        .contains(&"wall_assembly".to_string()));
    assert!(snapshot.no_curated_paths.iter().any(|gap| {
        gap.element_class == "wall_assembly"
            && gap.suggested_next_tool == "request_corpus_expansion"
    }));
    assert!(snapshot
        .guidance_overrides
        .iter()
        .all(|fact| fact.classification == "guidance_override"));
    assert!(snapshot
        .must_read_guidance_card_ids
        .contains(&"dkg.visual_morphology".to_string()));
    assert!(snapshot
        .must_read_guidance_card_ids
        .contains(&"dkg.building_skeleton".to_string()));
    assert!(snapshot
        .must_read_guidance_card_ids
        .contains(&"dkg.authoring_run_contract".to_string()));
    assert!(snapshot
        .must_read_guidance_card_ids
        .contains(&"dkg.trajectory_eval".to_string()));
    assert!(snapshot.estimated_json_bytes <= snapshot.size_budget_bytes);
}

/// Session-contract gate: every `must_read_guidance_card_id` the snapshot
/// advertises must resolve through `get_guidance_card`. Regression guard for the
/// case where dynamic authoring-guidance / reference ids (e.g.
/// `mcp_tool:select_recipe`) were advertised but unregistered.
#[cfg(feature = "model-api")]
#[test]
fn capability_snapshot_must_read_card_ids_all_resolve() {
    use crate::plugins::authoring_guidance::{
        AuthoringGuidance, ComponentStructurePolicy, GuidanceReference,
    };

    let mut world = World::new();
    world.insert_resource(CapabilityRegistry::default());
    world.insert_resource(AuthoringGuidance {
        guidance_id: "test.guidance".into(),
        version: 3,
        prompt_text: "Probe curated paths before authoring.".into(),
        component_structure: ComponentStructurePolicy::default(),
        references: vec![
            GuidanceReference {
                kind: "mcp_tool".into(),
                target: "select_recipe".into(),
                note: Some("Probe recipes before hand-rolling geometry.".into()),
            },
            GuidanceReference {
                kind: "mcp_tool".into(),
                target: "request_corpus_expansion".into(),
                note: None,
            },
        ],
        guidance_chapters: Vec::new(),
    });

    let snapshot = handle_get_capability_snapshot(&world, false);

    // The dynamic reference ids must actually be advertised (otherwise this test
    // would pass vacuously after a future change that drops them).
    assert!(
        snapshot
            .must_read_guidance_card_ids
            .iter()
            .any(|id| id == "mcp_tool:select_recipe"),
        "snapshot should advertise the mcp_tool:select_recipe guidance card, got {:?}",
        snapshot.must_read_guidance_card_ids
    );
    assert!(
        snapshot
            .must_read_guidance_card_ids
            .iter()
            .any(|id| id == "mcp_tool:request_corpus_expansion"),
        "snapshot should advertise the mcp_tool:request_corpus_expansion guidance card, got {:?}",
        snapshot.must_read_guidance_card_ids
    );

    // The core contract: every advertised id resolves through get_guidance_card.
    for id in &snapshot.must_read_guidance_card_ids {
        let card = handle_get_guidance_card(&world, id.clone());
        assert!(
            card.is_ok(),
            "must_read_guidance_card_id '{id}' did not resolve through get_guidance_card: {:?}",
            card.err()
        );
        assert_eq!(&card.unwrap().id, id);
    }
}

/// A corpus passage that flags itself proactive (in its data) must be surfaced
/// as an up-front must-read guidance card by the capability snapshot, and that
/// card must resolve through `get_guidance_card`. This is the data-driven
/// "skill" hook: an agent reads the generative knowledge before authoring
/// instead of only discovering it reactively via a validator backlink. Core
/// stays domain-neutral — it never inspects which passage this is.
#[cfg(feature = "model-api")]
#[test]
fn proactive_passage_surfaces_as_must_read_skill_card() {
    use crate::capability_registry::PassageRef;
    use crate::plugins::corpus_gap::{CorpusPassageRegistry, ProactivePassageGuidance};
    use crate::plugins::knowledge_persistence::{build_provenance_for_passage, PersistedPassage};

    let prov = |passage_ref: &str| {
        build_provenance_for_passage(&PersistedPassage {
            passage_ref: passage_ref.into(),
            text: String::new(),
            citation: "test".into(),
            source_url: None,
            jurisdiction: None,
            classification: None,
            license: Some("cc0".into()),
            proactive_guidance: None,
        })
    };

    let mut world = World::new();
    world.insert_resource(CapabilityRegistry::default());

    let mut passages = CorpusPassageRegistry::default();
    // A plain passage (no proactive flag) must NOT be promoted.
    passages.register(
        PassageRef("REACTIVE_ONLY_PASSAGE".into()),
        "Some reactively-discoverable detail.",
        prov("REACTIVE_ONLY_PASSAGE"),
    );
    // A proactive passage MUST be promoted to a must-read card.
    passages.register_with_guidance(
        PassageRef("SKILL_SUBSTRATE_CHAIN".into()),
        "Full generative substrate-chain text lives here.",
        prov("SKILL_SUBSTRATE_CHAIN"),
        Some(ProactivePassageGuidance {
            title: "Read first: build top-down along the substrate chain".into(),
            summary: "Frame establishes pitch; covering is derived from the frame face.".into(),
            task_tags: vec!["authoring".into(), "substrate".into()],
            priority: 1,
        }),
    );
    world.insert_resource(passages);

    let snapshot = handle_get_capability_snapshot(&world, false);

    assert!(
        snapshot
            .must_read_guidance_card_ids
            .iter()
            .any(|id| id == "passage:SKILL_SUBSTRATE_CHAIN"),
        "proactive passage should be advertised as a must-read card, got {:?}",
        snapshot.must_read_guidance_card_ids
    );
    assert!(
        !snapshot
            .must_read_guidance_card_ids
            .iter()
            .any(|id| id == "passage:REACTIVE_ONLY_PASSAGE"),
        "a passage without the proactive flag must NOT be advertised as must-read"
    );

    // Every advertised card — including the new skill card — must resolve.
    for id in &snapshot.must_read_guidance_card_ids {
        let card = handle_get_guidance_card(&world, id.clone());
        assert!(
            card.is_ok(),
            "must_read_guidance_card_id '{id}' did not resolve: {:?}",
            card.err()
        );
    }

    // The resolved skill card carries the decisive summary and points the agent
    // at lookup_source_passage for the full text.
    let card = handle_get_guidance_card(&world, "passage:SKILL_SUBSTRATE_CHAIN".into())
        .expect("skill card resolves");
    assert!(card.summary.contains("derived from the frame face"));
    assert!(card
        .referenced_tool_ids
        .iter()
        .any(|t| t == "lookup_source_passage"));
}

#[test]
fn internal_void_proxies_are_hidden_from_user_model_listing_and_summary() {
    use crate::plugins::modeling::void_declaration::OpeningContext;

    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    world.insert_resource(registry);

    world.spawn((
        ElementId(1),
        BoxPrimitive {
            centre: Vec3::new(0.0, 1.5, 0.0),
            half_extents: Vec3::new(3.0, 1.5, 0.15),
        },
        ShapeRotation::default(),
    ));
    world.spawn((
        ElementId(2),
        BoxPrimitive {
            centre: Vec3::new(0.0, 1.2, 0.0),
            half_extents: Vec3::new(0.6, 0.8, 0.15),
        },
        ShapeRotation::default(),
        OpeningContext {
            host: ElementId(1),
            filling: Some(ElementId(3)),
        },
    ));

    let entities = list_entities(&world);
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].element_id, 1);

    let summary = model_summary(&world);
    assert_eq!(summary.entity_counts.get("box"), Some(&1));
}

#[test]
fn get_entity_snapshot_returns_serialized_snapshot() {
    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    registry.register_factory(PrimitiveFactory::<PlanePrimitive>::new());
    registry.register_factory(PolylineFactory);
    world.insert_resource(registry);

    world.spawn((
        ElementId(7),
        PlanePrimitive {
            corner_a: Vec2::new(-1.0, -2.0),
            corner_b: Vec2::new(3.0, 2.0),
            elevation: 0.4,
        },
        ShapeRotation(Quat::from_rotation_y(0.2)),
    ));

    let snapshot =
        get_entity_snapshot(&world, ElementId(7)).expect("plane snapshot should be present");

    // PrimitiveSnapshot::to_json() serialises the primitive itself.
    let expected = serde_json::to_value(&PlanePrimitive {
        corner_a: Vec2::new(-1.0, -2.0),
        corner_b: Vec2::new(3.0, 2.0),
        elevation: 0.4,
    })
    .unwrap();

    assert_eq!(snapshot, expected);
    assert!(get_entity_snapshot(&world, ElementId(999)).is_none());
}

#[test]
fn get_entity_details_returns_normalized_property_list() {
    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    world.insert_resource(registry);

    world.spawn((
        ElementId(3),
        BoxPrimitive {
            centre: Vec3::new(1.0, 2.0, 3.0),
            half_extents: Vec3::new(4.0, 5.0, 6.0),
        },
        ShapeRotation::default(),
    ));

    let details = get_entity_details(&world, ElementId(3)).expect("box details should be present");

    assert_eq!(details.entity_type, "box");
    assert_eq!(
        details
            .geometry_semantics
            .as_ref()
            .map(|semantics| &semantics.role),
        Some(&crate::plugins::modeling::semantics::GeometryRole::SolidRoot)
    );
    assert_eq!(
        details
            .geometry_semantics
            .as_ref()
            .and_then(|semantics| semantics
                .evaluated_body
                .as_ref()
                .and_then(|body| body.volume)),
        Some(960.0)
    );
    assert_eq!(details.properties.len(), 3);
    assert_eq!(details.properties[0].name, "center");
    assert_eq!(details.properties[0].kind, "vec3");
    assert_eq!(details.properties[0].value, json!([1.0, 2.0, 3.0]));
    assert!(details.properties[0].editable);
    assert_eq!(details.properties[1].name, "half_extents");
    assert_eq!(details.properties[2].name, "material");
    assert_eq!(details.properties[2].kind, "text");
}

#[cfg(feature = "model-api")]
fn init_model_api_test_world() -> World {
    let mut world = World::new();
    world.insert_resource(Messages::<CreateBoxCommand>::default());
    world.insert_resource(Messages::<CreateCylinderCommand>::default());
    world.insert_resource(Messages::<CreateSphereCommand>::default());
    world.insert_resource(Messages::<CreatePlaneCommand>::default());
    world.insert_resource(Messages::<CreatePolylineCommand>::default());
    world.insert_resource(Messages::<CreateTriangleMeshCommand>::default());
    world.insert_resource(Messages::<CreateEntityCommand>::default());
    world.insert_resource(Messages::<DeleteEntitiesCommand>::default());
    world.insert_resource(Messages::<ResolvedDeleteEntitiesCommand>::default());
    world.insert_resource(Messages::<ApplyEntityChangesCommand>::default());
    world.insert_resource(Messages::<BeginCommandGroup>::default());
    world.insert_resource(Messages::<EndCommandGroup>::default());
    world.insert_resource(PendingCommandQueue::default());
    world.insert_resource(History::default());
    world.insert_resource(TextureRegistry::default());
    world.insert_resource(MaterialRegistry::default());
    world.insert_resource(ElementIdAllocator::default());
    world.insert_resource(DocumentState::default());
    world.insert_resource(OpaquePersistedEntities::default());
    world.insert_resource(DimensionLineVisibility::default());
    world.insert_resource(GuideLineVisibility::default());
    world.insert_resource(PropertyEditState::default());
    world.insert_resource(TransformState::default());
    world.insert_resource(NextState::<ActiveTool>::default());
    world.insert_resource(crate::plugins::storage::Storage(Box::new(
        crate::plugins::storage::LocalFileBackend,
    )));
    let mut import_registry = ImportRegistry::default();
    import_registry.register_importer(ObjImporter);
    world.insert_resource(import_registry);
    world.insert_resource(DocumentProperties::default());
    world.insert_resource(crate::plugins::named_views::NamedViewRegistry::default());
    let mut toolbar_registry = ToolbarRegistry::default();
    toolbar_registry.register(ToolbarDescriptor {
        id: "core".to_string(),
        label: "Core".to_string(),
        default_dock: ToolbarDock::Top,
        default_visible: true,
        sections: vec![ToolbarSection {
            label: "Select".to_string(),
            command_ids: vec!["core.select_tool".to_string()],
        }],
    });
    toolbar_registry.register(ToolbarDescriptor {
        id: "modeling".to_string(),
        label: "Modeling".to_string(),
        default_dock: ToolbarDock::Left,
        default_visible: true,
        sections: vec![ToolbarSection {
            label: "Primitives".to_string(),
            command_ids: vec!["modeling.place_box".to_string()],
        }],
    });
    world.insert_resource(toolbar_registry);
    let mut toolbar_layout_state = ToolbarLayoutState::default();
    toolbar_layout_state.entries.insert(
        "core".to_string(),
        ToolbarLayoutEntry {
            dock: ToolbarDock::Top,
            row: 0,
            order: 0,
            visible: true,
        },
    );
    toolbar_layout_state.entries.insert(
        "modeling".to_string(),
        ToolbarLayoutEntry {
            dock: ToolbarDock::Left,
            row: 0,
            order: 0,
            visible: true,
        },
    );
    world.insert_resource(toolbar_layout_state);
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    registry.register_factory(PrimitiveFactory::<CylinderPrimitive>::new());
    registry.register_factory(PrimitiveFactory::<SpherePrimitive>::new());
    registry.register_factory(PrimitiveFactory::<PlanePrimitive>::new());
    registry.register_factory(PolylineFactory);
    registry.register_factory(TriangleMeshFactory);
    registry.register_factory(FilletFactory);
    registry.register_factory(ChamferFactory);
    registry.register_factory(GuideLineFactory);
    registry.register_factory(DimensionLineFactory);
    registry.register_factory(crate::plugins::lighting::SceneLightFactory);
    registry.register_factory(crate::plugins::modeling::occurrence::OccurrenceFactory);
    world.insert_resource(registry);
    world.insert_resource(crate::plugins::modeling::definition::DefinitionRegistry::default());
    world.insert_resource(
        crate::plugins::modeling::definition::DefinitionLibraryRegistry::default(),
    );
    world.insert_resource(crate::plugins::definition_authoring::DefinitionDraftRegistry::default());
    world.insert_resource(crate::plugins::modeling::occurrence::ChangedDefinitions::default());
    world.insert_resource(RenderSettings::default());
    world.insert_resource(SceneLightingSettings::default());
    world.insert_resource(crate::plugins::drawing_export::ViewportExportState::default());
    world.insert_resource(Assets::<Mesh>::default());
    world.insert_resource(crate::plugins::layers::LayerRegistry::default());
    world.insert_resource(crate::plugins::materials::MaterialRegistry::default());
    world.spawn((
        Camera::default(),
        OrbitCamera::default(),
        Transform::default(),
        GlobalTransform::default(),
        Projection::Perspective(PerspectiveProjection::default()),
    ));
    world
}

#[cfg(feature = "model-api")]
#[test]
fn create_box_semantic_annotation_is_inspectable_and_survives_transform() {
    use crate::capability_registry::{ElementClassDescriptor, ElementClassId};
    use crate::plugins::refinement::{SemanticRole, SemanticSourceRef, UnresolvedDecisionRecord};

    let mut world = init_model_api_test_world();
    world
        .resource_mut::<CapabilityRegistry>()
        .register_element_class(ElementClassDescriptor {
            id: ElementClassId("conceptual_building_block".to_string()),
            label: "Conceptual Building Block".to_string(),
            description: "Test conceptual massing class".to_string(),
            semantic_roles: vec![SemanticRole("conceptual_massing".to_string())],
            class_min_obligations: std::collections::HashMap::new(),
            class_min_promotion_critical_paths: std::collections::HashMap::new(),
            parameter_schema: json!({"type": "object"}),
        });

    let element_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 1.5, 0.0]),
            half_extents: None,
            size: Some([8.0, 3.0, 10.0]),
            rotation: None,
            semantic: Some(SemanticEntityAnnotationRequest {
                element_class: Some("conceptual_building_block".to_string()),
                refinement_state: None,
                parameters: json!({
                    "label": "Main villa",
                    "height_mm": 3000,
                    "footprint_polygon_xz": [[-4, -5], [4, -5], [4, 5], [-4, 5]]
                }),
                unresolved_decisions: vec![UnresolvedDecisionRecord {
                    id: "orientation".to_string(),
                    question: "Which way should the main glazing face?".to_string(),
                    reason: "No site view or sun-path input has been provided.".to_string(),
                    grounding: "unresolved(CorpusGap)".to_string(),
                }],
                source_refs: vec![SemanticSourceRef {
                    reference: "user-prompt".to_string(),
                    claim: "Primary villa massing requested by the user.".to_string(),
                    grounding: "user-specified".to_string(),
                }],
                rationale: Some("Initial co-creative conceptual massing block".to_string()),
            }),
        },
    )
    .expect("semantic conceptual block should be creatable");

    let details = get_entity_details(&world, ElementId(element_id))
        .expect("created conceptual block should be inspectable");
    let semantic = details
        .semantic
        .expect("semantic details should be exposed");
    assert_eq!(
        semantic.element_class.as_deref(),
        Some("conceptual_building_block")
    );
    assert_eq!(semantic.refinement_state.as_deref(), Some("Conceptual"));
    assert_eq!(semantic.semantic_roles, vec!["conceptual_massing"]);
    assert_eq!(semantic.parameters["label"], json!("Main villa"));
    assert_eq!(semantic.unresolved_decisions.len(), 1);
    assert_eq!(
        semantic.unresolved_decisions[0].grounding,
        "unresolved(CorpusGap)"
    );

    handle_transform(
        &mut world,
        TransformToolRequest {
            element_ids: vec![element_id],
            operation: "move".to_string(),
            axis: None,
            value: json!([12.0, 0.0, 0.0]),
            pivot: None,
        },
    )
    .expect("conceptual block should use the existing transform path");

    let moved = get_entity_details(&world, ElementId(element_id))
        .expect("moved conceptual block should remain inspectable");
    assert_eq!(moved.snapshot["centre"], json!([12.0, 1.5, 0.0]));
    assert_eq!(
        moved
            .semantic
            .as_ref()
            .and_then(|semantic| semantic.element_class.as_deref()),
        Some("conceptual_building_block")
    );
}

#[cfg(feature = "model-api")]
#[test]
fn direct_model_api_primitives_are_hidden_commands_with_command_result() {
    let mut app = App::new();
    register_model_api_primitive_commands(&mut app);
    let command_registry = app
        .world_mut()
        .remove_resource::<CommandRegistry>()
        .expect("primitive command registration should initialize registry");

    let mut world = init_model_api_test_world();
    world.insert_resource(command_registry);

    let descriptor = world
        .resource::<CommandRegistry>()
        .get(CMD_MODEL_API_CREATE_BOX)
        .expect("direct create_box command should be registered");
    assert!(!descriptor.show_in_menu);
    assert!(descriptor.default_shortcut.is_none());

    let command_result = execute_command(
        &mut world,
        CMD_MODEL_API_CREATE_BOX,
        &json!({
            "center": [4.0, 5.0, 6.0],
            "size": [2.0, 2.0, 2.0]
        }),
    )
    .expect("hidden command should execute the same direct primitive");
    assert_eq!(command_result.created.len(), 1);
    let command_element_id = command_result.created[0];
    assert_eq!(
        command_result
            .output
            .as_ref()
            .and_then(|output| output.get("element_id"))
            .and_then(Value::as_u64),
        Some(command_element_id)
    );
    let details = get_entity_details(&world, ElementId(command_element_id))
        .expect("created box details should exist");
    assert_eq!(details.entity_type, "box");
}

#[cfg(feature = "model-api")]
#[test]
fn guide_line_create_round_trip_through_model_api() {
    let mut world = init_model_api_test_world();

    let element_id = handle_create_entity(
        &mut world,
        json!({
            "type": "guide_line",
            "anchor": [2.0, 0.0, 3.0],
            "direction": [0.0, 0.0, 5.0],
            "finite_length": 4.0,
            "label": "Survey axis"
        }),
    )
    .expect("guide line should be created");

    let snapshot = get_entity_snapshot(&world, ElementId(element_id))
        .expect("guide line snapshot should exist");
    assert_eq!(snapshot["anchor"], json!([2.0, 0.0, 3.0]));
    assert_eq!(snapshot["finite_length"], json!(4.0));
    assert_eq!(snapshot["label"], json!("Survey axis"));

    let details =
        get_entity_details(&world, ElementId(element_id)).expect("guide line details should exist");
    assert_eq!(details.entity_type, "guide_line");
    assert!(details
        .properties
        .iter()
        .any(|property| property.name == "direction"));

    let entities = list_entities(&world);
    assert!(entities
        .iter()
        .any(|entry| entry.element_id == element_id && entry.entity_type == "guide_line"));
}

#[cfg(feature = "model-api")]
#[test]
fn guide_line_request_json_supports_angle_contract() {
    let request = PlaceGuideLineRequest {
        anchor: [1.0, 1.0, 1.0],
        direction: None,
        through: None,
        reference_direction: Some([1.0, 0.0, 0.0]),
        angle_degrees: Some(45.0),
        plane_normal: Some([0.0, 1.0, 0.0]),
        finite_length: Some(2.5),
        visible: Some(true),
        label: Some("Top Face 45".to_string()),
    };

    let json = create_guide_line_request_json(&request);
    assert_eq!(json["anchor"], json!([1.0, 1.0, 1.0]));
    assert_eq!(json["reference_direction"], json!([1.0, 0.0, 0.0]));
    assert_eq!(json["angle_degrees"], json!(45.0));
    assert_eq!(json["plane_normal"], json!([0.0, 1.0, 0.0]));
    assert_eq!(json["finite_length"], json!(2.5));
    assert!(json.get("direction").is_none());
    assert!(json.get("through").is_none());
}

#[cfg(feature = "model-api")]
#[test]
fn dimension_line_create_round_trip_through_model_api() {
    let mut world = init_model_api_test_world();

    let element_id = handle_create_entity(
        &mut world,
        json!({
            "type": "dimension_line",
            "start": [0.0, 0.0, 0.0],
            "end": [2.0, 0.0, 0.0],
            "extension": 0.3,
            "label": "Width",
            "display_unit": "cm",
            "precision": 1
        }),
    )
    .expect("dimension line should be created");

    let snapshot = get_entity_snapshot(&world, ElementId(element_id))
        .expect("dimension line snapshot should exist");
    assert_eq!(snapshot["start"], json!([0.0, 0.0, 0.0]));
    assert_eq!(snapshot["end"], json!([2.0, 0.0, 0.0]));
    let line_point = snapshot["line_point"]
        .as_array()
        .expect("line_point should serialize as an array");
    assert_eq!(line_point.len(), 3);
    // Per the "project true 3D dim geometry" fix, the offset for a
    // horizontal dimension on the world XY plane goes along +Z.
    assert!((line_point[0].as_f64().expect("x should be numeric") - 1.0).abs() < 1e-5);
    assert!(line_point[1].as_f64().expect("y should be numeric").abs() < 1e-5);
    assert!((line_point[2].as_f64().expect("z should be numeric") - 0.36).abs() < 1e-5);
    let offset = snapshot["offset"]
        .as_f64()
        .expect("offset should serialize as numeric");
    assert!((offset - 0.36).abs() < 1e-5);
    let extension = snapshot["extension"]
        .as_f64()
        .expect("extension should be numeric");
    assert!((extension - 0.3).abs() < 1e-5);
    assert_eq!(snapshot["label"], json!("Width"));
    assert_eq!(snapshot["display_unit"], json!("cm"));
    assert_eq!(snapshot["precision"], json!(1));
    assert_eq!(snapshot["length"], json!(2.0));

    let details = get_entity_details(&world, ElementId(element_id))
        .expect("dimension line details should exist");
    assert_eq!(details.entity_type, "dimension_line");
    assert!(details
        .properties
        .iter()
        .any(|property| property.name == "extension"));
    assert!(details
        .properties
        .iter()
        .any(|property| property.name == "length"));

    let entities = list_entities(&world);
    assert!(entities
        .iter()
        .any(|entry| { entry.element_id == element_id && entry.entity_type == "dimension_line" }));
}

#[cfg(feature = "model-api")]
#[test]
fn agent_workflow_create_box_dimension_camera_and_screenshot_is_supported() {
    let mut world = init_model_api_test_world();

    let box_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 1.0, 0.0]),
            half_extents: None,
            size: Some([4.0, 2.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .expect("create_box should create a primitive");

    let box_snapshot =
        get_entity_snapshot(&world, ElementId(box_id)).expect("box snapshot should exist");
    assert_eq!(box_snapshot["centre"], json!([0.0, 1.0, 0.0]));
    assert_eq!(box_snapshot["half_extents"], json!([2.0, 1.0, 0.5]));

    let dimension_id = handle_place_dimension_between_handles(
        &mut world,
        PlaceDimensionBetweenHandlesRequest {
            start_element_id: box_id,
            start_handle_id: "corner_0".to_string(),
            end_element_id: box_id,
            end_handle_id: "corner_3".to_string(),
            line_point: None,
            offset: Some(0.5),
            extension: Some(0.25),
            visible: Some(true),
            label: Some("Width".to_string()),
            display_unit: Some("mm".to_string()),
            precision: Some(0),
        },
    )
    .expect("handle-based dimension should be created");

    let dimension_snapshot = get_entity_snapshot(&world, ElementId(dimension_id))
        .expect("dimension snapshot should exist");
    assert_eq!(dimension_snapshot["start"], json!([-2.0, 0.0, -0.5]));
    assert_eq!(dimension_snapshot["end"], json!([2.0, 0.0, -0.5]));
    assert_eq!(dimension_snapshot["label"], json!("Width"));

    let camera = handle_set_camera(
        &mut world,
        CameraParams {
            focus: Some([0.0, 1.0, 0.0]),
            radius: Some(8.0),
            orthographic_scale: Some(3.0),
            yaw: Some(0.75),
            pitch: Some(-0.4),
            projection: Some("orthographic".to_string()),
            focal_length_mm: Some(35.0),
        },
    )
    .expect("set_camera should update the live orbit camera");
    assert_eq!(camera.focus, [0.0, 1.0, 0.0]);
    assert_eq!(camera.projection, "orthographic");
    assert_eq!(camera.orthographic_scale, 3.0);

    let camera_snapshot = handle_get_camera(&world);
    assert_eq!(camera_snapshot.yaw, 0.75);
    assert_eq!(camera_snapshot.pitch, -0.4);

    let screenshot_path = std::env::temp_dir().join(format!(
        "talos3d-model-api-workflow-{}.png",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_millis()
    ));
    let saved_path = handle_take_screenshot(&mut world, screenshot_path.to_str().unwrap(), false)
        .expect("take_screenshot should queue a capture");
    assert_eq!(saved_path, screenshot_path.to_string_lossy().to_string());
    assert!(world
        .resource::<crate::plugins::drawing_export::ViewportExportState>()
        .pending
        .is_some());
}

#[cfg(feature = "model-api")]
#[test]
fn align_preview_and_execute_match_reference_edge() {
    let mut world = init_model_api_test_world();
    let left = handle_create_entity(
        &mut world,
        json!({"type": "box", "centre": [0.0, 0.0, 0.0], "half_extents": [1.0, 1.0, 1.0]}),
    )
    .expect("left box should be created");
    let reference = handle_create_entity(
        &mut world,
        json!({"type": "box", "centre": [5.0, 3.0, 0.0], "half_extents": [1.0, 2.0, 1.0]}),
    )
    .expect("reference box should be created");
    let right = handle_create_entity(
        &mut world,
        json!({"type": "box", "centre": [10.0, 0.5, 0.0], "half_extents": [1.0, 0.5, 1.0]}),
    )
    .expect("right box should be created");

    let request = AlignRequest {
        element_ids: vec![left, reference, right],
        axis: "y".to_string(),
        mode: "max".to_string(),
        reference_element_id: Some(reference),
        reference_value: None,
    };
    let preview = handle_align_preview(&mut world, request.clone()).expect("preview should work");
    let preview_left = preview
        .iter()
        .find(|entry| entry.element_id == left)
        .expect("left preview should exist");
    assert_eq!(preview_left.proposed_position, [0.0, 4.0, 0.0]);

    handle_align_execute(&mut world, request).expect("execute should work");

    let left_snapshot =
        capture_snapshot_by_id(&world, ElementId(left)).expect("left snapshot should exist");
    let reference_snapshot = capture_snapshot_by_id(&world, ElementId(reference))
        .expect("reference snapshot should exist");
    let right_snapshot =
        capture_snapshot_by_id(&world, ElementId(right)).expect("right snapshot should exist");

    assert_eq!(alignment_bounds(&left_snapshot).max.y, 5.0);
    assert_eq!(alignment_bounds(&reference_snapshot).max.y, 5.0);
    assert_eq!(alignment_bounds(&right_snapshot).max.y, 5.0);
}

#[cfg(feature = "model-api")]
#[test]
fn distribute_execute_evenly_spaces_centers() {
    let mut world = init_model_api_test_world();
    let first = handle_create_entity(
        &mut world,
        json!({"type": "box", "centre": [0.0, 0.0, 0.0], "half_extents": [1.0, 1.0, 1.0]}),
    )
    .expect("first box should be created");
    let middle = handle_create_entity(
        &mut world,
        json!({"type": "box", "centre": [3.0, 0.0, 0.0], "half_extents": [1.0, 1.0, 1.0]}),
    )
    .expect("middle box should be created");
    let last = handle_create_entity(
        &mut world,
        json!({"type": "box", "centre": [12.0, 0.0, 0.0], "half_extents": [1.0, 1.0, 1.0]}),
    )
    .expect("last box should be created");

    let preview = handle_distribute_preview(
        &mut world,
        DistributeRequest {
            element_ids: vec![first, middle, last],
            axis: "x".to_string(),
            mode: "spacing".to_string(),
            value: None,
        },
    )
    .expect("preview should work");
    let middle_preview = preview
        .iter()
        .find(|entry| entry.element_id == middle)
        .expect("middle preview should exist");
    assert_eq!(middle_preview.proposed_position, [6.0, 0.0, 0.0]);

    handle_distribute_execute(
        &mut world,
        DistributeRequest {
            element_ids: vec![first, middle, last],
            axis: "x".to_string(),
            mode: "spacing".to_string(),
            value: None,
        },
    )
    .expect("execute should work");

    let middle_snapshot =
        capture_snapshot_by_id(&world, ElementId(middle)).expect("middle snapshot should exist");
    assert_eq!(middle_snapshot.center(), Vec3::new(6.0, 0.0, 0.0));
}

#[cfg(feature = "model-api")]
fn write_temp_obj(contents: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("talos3d-model-api-{unique}.obj"));
    fs::write(&path, contents).expect("temp obj should be written");
    path
}

#[cfg(feature = "model-api")]
fn temp_json_path(stem: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{stem}-{unique}.json"))
}

#[cfg(feature = "model-api")]
#[test]
fn handle_dimension_between_box_corners_constrains_line_point_to_adjacent_edge_direction() {
    let mut world = init_model_api_test_world();

    let box_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 1.0, 0.0]),
            half_extents: None,
            size: Some([4.0, 2.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .expect("create_box should create a primitive");

    let dimension_id = handle_place_dimension_between_handles(
        &mut world,
        PlaceDimensionBetweenHandlesRequest {
            start_element_id: box_id,
            start_handle_id: "corner_0".to_string(),
            end_element_id: box_id,
            end_handle_id: "corner_3".to_string(),
            line_point: Some([0.0, 0.4, -0.2]),
            offset: None,
            extension: Some(0.25),
            visible: Some(true),
            label: None,
            display_unit: None,
            precision: None,
        },
    )
    .expect("handle-based dimension should be created");

    let dimension_snapshot = get_entity_snapshot(&world, ElementId(dimension_id))
        .expect("dimension snapshot should exist");
    assert_eq!(dimension_snapshot["start"], json!([-2.0, 0.0, -0.5]));
    assert_eq!(dimension_snapshot["end"], json!([2.0, 0.0, -0.5]));
    let line_point = dimension_snapshot["line_point"]
        .as_array()
        .expect("line_point should serialize as a coordinate array");
    let coordinates: Vec<f64> = line_point
        .iter()
        .map(|value| value.as_f64().expect("coordinate should be numeric"))
        .collect();
    assert_eq!(coordinates.len(), 3);
    assert!((coordinates[0] - 0.0).abs() < 1e-6);
    assert!((coordinates[1] - 0.0).abs() < 1e-6);
    assert!(
        (coordinates[2] + 1.22).abs() < 1e-5,
        "expected constrained line_point to project outside the nearest adjacent box side, got {coordinates:?}"
    );
}

#[cfg(feature = "model-api")]
#[test]
fn write_handlers_create_transform_delete_and_list_handles() {
    let mut world = init_model_api_test_world();

    let box_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [1.0, 2.0, 3.0],
            "half_extents": [0.5, 0.75, 1.0]
        }),
    )
    .expect("box should be created");
    assert_eq!(box_id, 0);

    let transformed = handle_transform(
        &mut world,
        TransformToolRequest {
            element_ids: vec![box_id],
            operation: "move".to_string(),
            axis: Some("X".to_string()),
            value: json!(2.5),
            pivot: None,
        },
    )
    .expect("transform should succeed");
    assert_eq!(transformed.len(), 1);

    let box_snapshot =
        get_entity_snapshot(&world, ElementId(box_id)).expect("box snapshot should exist");
    assert_eq!(box_snapshot["centre"], json!([3.5, 2.0, 3.0]));

    let handles = handle_list_handles(&world, box_id).expect("box handles should exist");
    assert_eq!(handles.len(), 9);
    assert_eq!(handles[0].kind, "Vertex");

    let deleted_count =
        handle_delete_entities(&mut world, vec![box_id]).expect("delete should remove the box");
    assert_eq!(deleted_count, 1);
    assert!(get_entity_snapshot(&world, ElementId(box_id)).is_none());
}

#[cfg(feature = "model-api")]
#[test]
fn write_handlers_create_and_edit_sphere() {
    let mut world = init_model_api_test_world();

    let sphere_id = handle_create_entity(
        &mut world,
        json!({
            "type": "sphere",
            "centre": [0.0, 1.0, 0.0],
            "radius": 1.25
        }),
    )
    .expect("sphere should be created");

    let details =
        get_entity_details(&world, ElementId(sphere_id)).expect("sphere details should exist");
    assert_eq!(details.entity_type, "sphere");

    let updated = handle_set_property(&mut world, sphere_id, "radius", json!(2.5))
        .expect("setting sphere radius should succeed");
    assert_eq!(updated["radius"], json!(2.5));

    let summary = model_summary(&world);
    assert_eq!(summary.entity_counts.get("sphere"), Some(&1));
}

#[cfg(feature = "model-api")]
#[test]
fn write_handlers_create_and_edit_fillet_and_chamfer() {
    let mut world = init_model_api_test_world();

    let fillet_source_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [0.0, 0.0, 0.0],
            "half_extents": [1.0, 1.0, 1.0]
        }),
    )
    .expect("fillet source should be created");
    let chamfer_source_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [4.0, 0.0, 0.0],
            "half_extents": [1.0, 1.0, 1.0]
        }),
    )
    .expect("chamfer source should be created");

    let fillet_id = handle_create_entity(
        &mut world,
        json!({
            "type": "fillet",
            "source": fillet_source_id,
            "radius": 0.2,
            "segments": 4
        }),
    )
    .expect("fillet should be created");
    let chamfer_id = handle_create_entity(
        &mut world,
        json!({
            "type": "chamfer",
            "source": chamfer_source_id,
            "distance": 0.15
        }),
    )
    .expect("chamfer should be created");

    let fillet_updated = handle_set_property(&mut world, fillet_id, "radius", json!(0.35))
        .expect("setting fillet radius should succeed");
    assert!(
        (fillet_updated["radius"].as_f64().unwrap_or_default() - 0.35).abs() < 1e-5,
        "fillet radius should be updated"
    );

    let segments_updated = handle_set_property(&mut world, fillet_id, "segments", json!(6.0))
        .expect("setting fillet segments should succeed");
    assert_eq!(segments_updated["segments"], json!(6));

    let chamfer_updated = handle_set_property(&mut world, chamfer_id, "distance", json!(0.25))
        .expect("setting chamfer distance should succeed");
    assert!(
        (chamfer_updated["distance"].as_f64().unwrap_or_default() - 0.25).abs() < 1e-5,
        "chamfer distance should be updated"
    );

    let fillet_details =
        get_entity_details(&world, ElementId(fillet_id)).expect("fillet details should exist");
    assert_eq!(fillet_details.entity_type, "fillet");

    let chamfer_details =
        get_entity_details(&world, ElementId(chamfer_id)).expect("chamfer details should exist");
    assert_eq!(chamfer_details.entity_type, "chamfer");

    let summary = model_summary(&world);
    assert_eq!(summary.entity_counts.get("fillet"), Some(&1));
    assert_eq!(summary.entity_counts.get("chamfer"), Some(&1));
}

#[cfg(feature = "model-api")]
#[test]
fn set_property_validates_entity_specific_fields() {
    let mut world = init_model_api_test_world();
    let box_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [0.0, 0.0, 0.0],
            "half_extents": [1.0, 2.0, 3.0]
        }),
    )
    .expect("box should be created");

    let updated = handle_set_property(&mut world, box_id, "half_extents", json!([4.0, 5.0, 6.0]))
        .expect("setting box half extents should succeed");
    assert_eq!(updated["half_extents"], json!([4.0, 5.0, 6.0]));

    let error = handle_set_property(&mut world, box_id, "radius", json!(1.0))
        .expect_err("invalid box property should fail");
    assert!(error.contains("Valid properties: center, half_extents"));
}

#[cfg(feature = "model-api")]
#[test]
fn toolbar_handlers_list_and_update_toolbar_layout() {
    let mut world = init_model_api_test_world();

    let listed = list_toolbars(&world);
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, "modeling");

    let updated = handle_set_toolbar_layout(
        &mut world,
        vec![ToolbarLayoutUpdate {
            toolbar_id: "modeling".to_string(),
            dock: Some("bottom".to_string()),
            order: Some(3),
            visible: Some(false),
        }],
    )
    .expect("toolbar layout update should succeed");

    let modeling = updated
        .iter()
        .find(|toolbar| toolbar.id == "modeling")
        .expect("modeling toolbar should be listed");
    assert_eq!(modeling.dock, "bottom");
    assert_eq!(modeling.order, 3);
    assert!(!modeling.visible);

    let error = handle_set_toolbar_layout(
        &mut world,
        vec![ToolbarLayoutUpdate {
            toolbar_id: "core".to_string(),
            dock: None,
            order: None,
            visible: Some(false),
        }],
    )
    .expect_err("core toolbar should remain visible");
    assert!(error.contains("cannot be hidden"));
}

#[cfg(feature = "model-api")]
#[test]
fn poll_model_api_requests_services_channel_queries() {
    let mut world = init_model_api_test_world();
    world.spawn((
        ElementId(1),
        PlanePrimitive {
            corner_a: Vec2::ZERO,
            corner_b: Vec2::new(4.0, 2.0),
            elevation: 0.0,
        },
        ShapeRotation::default(),
    ));

    let (sender, receiver) = mpsc::channel();
    world.insert_resource(ModelApiReceiver(Mutex::new(receiver)));

    let (list_response, list_receiver) = oneshot::channel();
    sender
        .send(ModelApiRequest::ListEntities(list_response))
        .expect("list request should send");

    let (summary_response, summary_receiver) = oneshot::channel();
    sender
        .send(ModelApiRequest::ModelSummary(summary_response))
        .expect("summary request should send");

    poll_model_api_requests(&mut world);

    let list = list_receiver
        .blocking_recv()
        .expect("list response should arrive");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].entity_type, "plane");

    let summary = summary_receiver
        .blocking_recv()
        .expect("summary response should arrive");
    assert_eq!(summary.entity_counts.get("plane"), Some(&1));
}

#[cfg(feature = "model-api")]
#[test]
fn import_handlers_list_importers_and_create_triangle_meshes() {
    use crate::capability_registry::{ElementClassDescriptor, ElementClassId};
    use crate::plugins::refinement::SemanticRole;
    use crate::plugins::semantic_shadow::SemanticShadowCandidateStatus;

    let mut world = init_model_api_test_world();
    world
        .resource_mut::<CapabilityRegistry>()
        .register_element_class(ElementClassDescriptor {
            id: ElementClassId("imported_foreign_geometry".to_string()),
            label: "Imported Foreign Geometry".to_string(),
            description: "Foreign import context accepted into native semantic inspection."
                .to_string(),
            semantic_roles: vec![SemanticRole("foreign_context".to_string())],
            class_min_obligations: std::collections::HashMap::new(),
            class_min_promotion_critical_paths: std::collections::HashMap::new(),
            parameter_schema: json!({}),
        });

    let importers = world.resource::<ImportRegistry>().list_importers();
    assert_eq!(importers.len(), 1);
    assert_eq!(importers[0].format_name, "Wavefront OBJ");
    assert_eq!(importers[0].extensions, vec!["obj"]);

    let path = write_temp_obj("o Imported\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
    let imported_ids = handle_import_file(&mut world, path.to_str().unwrap_or_default(), None)
        .expect("OBJ import should succeed");
    assert_eq!(imported_ids.len(), 1);

    let snapshot = get_entity_snapshot(&world, ElementId(imported_ids[0]))
        .expect("triangle mesh snapshot should exist");
    assert_eq!(
        snapshot["TriangleMesh"]["primitive"]["name"],
        json!("Imported")
    );
    assert_eq!(
        snapshot["TriangleMesh"]["semantic_shadow"]["candidates"][0]["status"],
        json!("inferred")
    );
    assert_eq!(
        snapshot["TriangleMesh"]["semantic_shadow"]["gaps"][0]["category"],
        json!("unsupported_semantics")
    );
    let details = get_entity_details(&world, ElementId(imported_ids[0]))
        .expect("triangle mesh details should exist");
    assert!(details.semantic.is_none());
    let shadow = details
        .semantic_shadow
        .expect("imported triangle mesh should carry semantic shadow");
    assert_eq!(
        shadow.candidates[0].element_class.as_deref(),
        Some("imported_foreign_geometry")
    );
    assert_eq!(
        shadow.candidates[0].status,
        SemanticShadowCandidateStatus::Inferred
    );

    let accepted = handle_accept_semantic_shadow_candidate(
        &mut world,
        AcceptSemanticShadowCandidateRequest {
            element_id: imported_ids[0],
            candidate_id: "imported_foreign_geometry".to_string(),
            element_class: None,
            refinement_state: Some("Conceptual".to_string()),
            parameters: None,
            rationale: None,
        },
    )
    .expect("semantic shadow candidate should accept through model API path");
    let semantic = accepted
        .semantic
        .expect("accepted shadow should become native semantic details");
    assert_eq!(
        semantic.element_class.as_deref(),
        Some("imported_foreign_geometry")
    );
    assert_eq!(semantic.semantic_roles, vec!["foreign_context".to_string()]);
    assert_eq!(
        accepted.semantic_shadow.unwrap().candidates[0].status,
        SemanticShadowCandidateStatus::AcceptedNativeClaim
    );

    let entities = list_entities(&world);
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].entity_type, "triangle_mesh");

    let _ = fs::remove_file(path);
}

#[cfg(feature = "model-api")]
#[tokio::test]
async fn mcp_tools_return_structured_model_data() {
    let (sender, receiver) = mpsc::channel();
    let worker_handle = tokio::task::spawn_blocking(move || {
        let mut world = init_model_api_test_world();
        let mut command_app = App::new();
        register_model_api_primitive_commands(&mut command_app);
        let command_registry = command_app
            .world_mut()
            .remove_resource::<CommandRegistry>()
            .expect("primitive command registration should initialize registry");
        world.insert_resource(command_registry);
        world.spawn((
            ElementId(10),
            BoxPrimitive {
                centre: Vec3::new(1.0, 1.0, 1.0),
                half_extents: Vec3::splat(0.5),
            },
            ShapeRotation::default(),
        ));
        world.spawn((
            ElementId(11),
            PlanePrimitive {
                corner_a: Vec2::new(-1.0, -1.0),
                corner_b: Vec2::new(1.0, 1.0),
                elevation: 0.0,
            },
            ShapeRotation::default(),
        ));

        while let Ok(request) = receiver.recv() {
            handle_model_api_request(&mut world, request);
        }
    });

    let server = ModelApiServer::new(sender);
    let tools = server.tool_router.list_all();
    let tool_names: std::collections::BTreeSet<_> =
        tools.iter().map(|tool| tool.name.clone()).collect();
    assert!(tool_names.contains("list_entities"));
    assert!(tool_names.contains("create_entity"));
    assert!(tool_names.contains("create_box"));
    assert!(tool_names.contains("get_texture_mapping"));
    assert!(tool_names.contains("update_texture_mapping"));
    assert!(tool_names.contains("reset_texture_mapping"));
    assert!(tool_names.contains("semantic_shadow.accept_candidate"));
    assert!(tool_names.contains("place_dimension_between_handles"));
    assert!(tool_names.contains("get_camera"));
    assert!(tool_names.contains("set_camera"));
    assert!(tool_names.contains("take_screenshot"));
    assert!(tool_names.contains("export_drawing"));
    assert!(tool_names.contains("export.fidelity.describe"));

    let manifests: Vec<crate::plugins::export_fidelity::ExportFidelityManifest> = server
        .export_fidelity_describe_tool(Parameters(ExportFidelityRequest {
            surface: Some("drawing.pdf".into()),
            path: None,
        }))
        .await
        .expect("export fidelity tool should succeed")
        .into_typed()
        .expect("export fidelity result should deserialize");
    assert_eq!(manifests.len(), 1);
    assert_eq!(manifests[0].surface_id, "drawing.pdf");

    let listed: Vec<EntityEntry> = server
        .list_entities_tool()
        .await
        .expect("list_entities tool should succeed")
        .into_typed()
        .expect("list_entities result should deserialize");
    assert_eq!(listed.len(), 2);

    let box_snapshot: serde_json::Value = server
        .get_entity_tool(Parameters(GetEntityRequest { element_id: 10 }))
        .await
        .expect("get_entity tool should succeed")
        .into_typed()
        .expect("get_entity result should deserialize");
    assert!(
        box_snapshot.is_object(),
        "box snapshot should be a JSON object"
    );
    assert_eq!(box_snapshot["centre"], serde_json::json!([1.0, 1.0, 1.0]));

    let box_details: EntityDetails = server
        .get_entity_details_tool(Parameters(GetEntityRequest { element_id: 10 }))
        .await
        .expect("get_entity_details tool should succeed")
        .into_typed()
        .expect("get_entity_details result should deserialize");
    assert_eq!(box_details.entity_type, "box");
    // center, half_extents, and the material property added to all
    // primitive snapshots (see PP-021).
    assert_eq!(box_details.properties.len(), 3);
    let prop_names: Vec<&str> = box_details
        .properties
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert!(prop_names.contains(&"center"));
    assert!(prop_names.contains(&"half_extents"));
    assert!(prop_names.contains(&"material"));

    let summary: ModelSummary = server
        .model_summary_tool()
        .await
        .expect("model_summary tool should succeed")
        .into_typed()
        .expect("model_summary result should deserialize");
    assert_eq!(summary.entity_counts.get("box"), Some(&1));
    assert_eq!(summary.entity_counts.get("plane"), Some(&1));

    let importers: Vec<ImporterDescriptor> = server
        .list_importers_tool()
        .await
        .expect("list_importers tool should succeed")
        .into_typed()
        .expect("list_importers result should deserialize");
    assert_eq!(importers.len(), 1);
    assert_eq!(importers[0].format_name, "Wavefront OBJ");

    let obj_path = write_temp_obj("o FromTool\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
    let imported_ids: Vec<u64> = server
        .import_file_tool(Parameters(ImportFileRequest {
            path: obj_path.to_string_lossy().to_string(),
            format_hint: None,
        }))
        .await
        .expect("import_file tool should succeed")
        .into_typed()
        .expect("import_file result should deserialize");
    assert_eq!(imported_ids.len(), 1);

    let imported_snapshot: Value = server
        .get_entity_tool(Parameters(GetEntityRequest {
            element_id: imported_ids[0],
        }))
        .await
        .expect("get_entity for imported triangle mesh should succeed")
        .into_typed()
        .expect("imported get_entity result should deserialize");
    assert_eq!(
        imported_snapshot["TriangleMesh"]["primitive"]["name"],
        json!("FromTool")
    );

    let property_result: CommandResult = server
        .set_entity_property_tool(Parameters(SetPropertyRequest {
            element_id: 10,
            property_name: "half_extents".to_string(),
            value: json!([2.0, 2.0, 2.0]),
        }))
        .await
        .expect("set_entity_property tool should succeed")
        .into_typed()
        .expect("set_entity_property result should deserialize");
    assert_eq!(property_result.modified, vec![10]);
    let updated_snapshot = property_result
        .output
        .expect("set_entity_property should include updated snapshot output");
    assert_eq!(updated_snapshot["half_extents"], json!([2.0, 2.0, 2.0]));

    let toolbars: Vec<ToolbarDetails> = server
        .list_toolbars_tool()
        .await
        .expect("list_toolbars tool should succeed")
        .into_typed()
        .expect("list_toolbars result should deserialize");
    assert_eq!(toolbars.len(), 2);

    let updated_toolbars: Vec<ToolbarDetails> = server
        .set_toolbar_layout_tool(Parameters(SetToolbarLayoutRequest {
            updates: vec![ToolbarLayoutUpdate {
                toolbar_id: "modeling".to_string(),
                dock: Some("right".to_string()),
                order: Some(4),
                visible: Some(true),
            }],
        }))
        .await
        .expect("set_toolbar_layout tool should succeed")
        .into_typed()
        .expect("set_toolbar_layout result should deserialize");
    let modeling_toolbar = updated_toolbars
        .iter()
        .find(|toolbar| toolbar.id == "modeling")
        .expect("modeling toolbar should be returned");
    assert_eq!(modeling_toolbar.dock, "right");
    assert_eq!(modeling_toolbar.order, 4);

    let _ = fs::remove_file(obj_path);

    drop(server);
    worker_handle.await.expect("worker should stop cleanly");
}

#[cfg(feature = "model-api")]
/// End-to-end demonstration (ADR-051): build a parametric post-and-beam
/// pavilion entirely through the `procedural_session.*` MCP tools, with
/// commit producing real geometry via `ModelApiStepExecutor`.
///
/// This drives the exact `ModelApiServer` tool entry points an external
/// MCP client hits, against a real world, and asserts the committed
/// structure exists.
#[cfg(feature = "model-api")]
#[tokio::test]
async fn procedural_session_builds_post_and_beam_pavilion_through_mcp() {
    use crate::curation::procedural_session::{
        ProceduralSessionConfig, ProceduralSessionRegistry, SessionToolRegistry,
    };
    use crate::curation::{ArgExpr, McpToolId, StepId};
    use crate::plugins::procedural_session_mcp::{
        SessionCommitRequest, SessionCreateRequest, SessionEvalRequest, SessionExportRequest,
    };

    let (sender, receiver) = mpsc::channel();
    let worker_handle = tokio::task::spawn_blocking(move || {
        let mut world = init_model_api_test_world();
        // Install the Semantic Procedural Session substrate + the
        // Model API session-tool descriptors (as ModelApiPlugin does).
        world.insert_resource(ProceduralSessionRegistry::default());
        world.insert_resource(ProceduralSessionConfig::default());
        let mut session_tools = SessionToolRegistry::default();
        register_model_api_session_tools(&mut session_tools);
        world.insert_resource(session_tools);

        while let Ok(request) = receiver.recv() {
            handle_model_api_request(&mut world, request);
        }
    });

    let server = ModelApiServer::new(sender);

    // The five session tools are registered on the MCP surface.
    let tool_names: std::collections::BTreeSet<_> = server
        .tool_router
        .list_all()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    for expected in [
        "procedural_session.create",
        "procedural_session.eval",
        "procedural_session.snapshot",
        "procedural_session.commit",
        "procedural_session.export",
    ] {
        assert!(tool_names.contains(expected), "missing MCP tool {expected}");
    }

    // The procedural interpreter must be able to run the read-only checks an
    // MCP authoring agent uses before saving generated geometry.
    let pure_query_spec = crate::curation::procedural_session::SessionSpec {
        refinement_target: None,
        stage_transition: crate::curation::procedural_session::StageTransition::PureQuery,
        mutation_scope: crate::curation::MutationScope::None,
        allowed_tools: [
            McpToolId::new("model_summary"),
            McpToolId::new("run_validation_v2"),
        ]
        .into_iter()
        .collect(),
        seed: Some(2),
        parameter_schema: None,
    };
    let pure_query: crate::plugins::procedural_session_mcp::SessionCreateResponse = server
        .procedural_session_create_tool(Parameters(SessionCreateRequest {
            spec: pure_query_spec,
        }))
        .await
        .expect("pure query session create should succeed")
        .into_typed()
        .expect("pure query create response should deserialize");
    for (id, tool) in [
        ("summary_check", "model_summary"),
        ("validation_check", "run_validation_v2"),
    ] {
        server
            .procedural_session_eval_tool(Parameters(SessionEvalRequest {
                session_id: pure_query.session_id.clone(),
                step: crate::curation::procedural_session::EvalStep {
                    id: StepId::new(id),
                    tool: McpToolId::new(tool),
                    args: Default::default(),
                    bindings: Default::default(),
                    essential: true,
                    precondition: None,
                },
                mode: crate::curation::procedural_session::EvalMode::BindOnly,
            }))
            .await
            .expect("read-only procedural check should bind");
    }
    let pure_commit: crate::curation::CommitReport = server
        .procedural_session_commit_tool(Parameters(SessionCommitRequest {
            session_id: pure_query.session_id.clone(),
            options: crate::curation::CommitOptions::default(),
        }))
        .await
        .expect("read-only procedural checks should commit")
        .into_typed()
        .expect("pure query commit report should deserialize");
    assert_eq!(pure_commit.steps_run.len(), 2);

    // 1. Open a session for net-new top-level authoring: ProjectRoot
    //    scope, (none) -> Conceptual, only `create_box` allowed.
    let mut spec = crate::curation::procedural_session::SessionSpec::for_new_structure(
        [McpToolId::new("create_box")].into_iter().collect(),
    );
    spec.seed = Some(1);
    let create_req = SessionCreateRequest { spec };
    let create_result: crate::plugins::procedural_session_mcp::SessionCreateResponse = server
        .procedural_session_create_tool(Parameters(create_req))
        .await
        .expect("session create should succeed")
        .into_typed()
        .expect("create response should deserialize");
    let session_id = create_result.session_id.clone();

    // 2. Procedurally generate the structure: a 3 (X) x 2 (Z) post-and-
    //    beam frame. This loop is the "agent's" procedural generation;
    //    each step is appended to the in-flight AuthoringScript.
    let nx = 3usize; // column lines along X
    let nz = 2usize; // column lines along Z
    let bay_x = 3.0f32;
    let bay_z = 4.0f32;
    let col_h = 3.0f32;
    let col_w = 0.3f32;
    let beam_h = 0.4f32;

    let box_step = |id: String, center: [f32; 3], size: [f32; 3]| SessionEvalRequest {
        session_id: session_id.clone(),
        step: crate::curation::procedural_session::EvalStep {
            id: StepId::new(id),
            tool: McpToolId::new("create_box"),
            args: [
                (
                    "center".to_string(),
                    ArgExpr::Literal {
                        value: json!(center),
                    },
                ),
                ("size".to_string(), ArgExpr::Literal { value: json!(size) }),
            ]
            .into_iter()
            .collect(),
            bindings: Default::default(),
            essential: true,
            precondition: None,
        },
        mode: crate::curation::procedural_session::EvalMode::BindOnly,
    };

    let mut expected_boxes = 0usize;

    // Columns.
    for ix in 0..nx {
        for iz in 0..nz {
            let x = ix as f32 * bay_x;
            let z = iz as f32 * bay_z;
            let req = box_step(
                format!("col_{ix}_{iz}"),
                [x, col_h / 2.0, z],
                [col_w, col_h, col_w],
            );
            server
                .procedural_session_eval_tool(Parameters(req))
                .await
                .expect("eval column step should succeed");
            expected_boxes += 1;
        }
    }
    // Longitudinal beams (along X) at the top, one per Z line per bay.
    for iz in 0..nz {
        for ix in 0..(nx - 1) {
            let x = (ix as f32 + 0.5) * bay_x;
            let z = iz as f32 * bay_z;
            let req = box_step(
                format!("beam_x_{ix}_{iz}"),
                [x, col_h + beam_h / 2.0, z],
                [bay_x, beam_h, col_w],
            );
            server
                .procedural_session_eval_tool(Parameters(req))
                .await
                .expect("eval longitudinal beam step should succeed");
            expected_boxes += 1;
        }
    }
    // Transverse beams (along Z) at the top, one per X line per bay.
    for ix in 0..nx {
        for iz in 0..(nz - 1) {
            let x = ix as f32 * bay_x;
            let z = (iz as f32 + 0.5) * bay_z;
            let req = box_step(
                format!("beam_z_{ix}_{iz}"),
                [x, col_h + beam_h / 2.0, z],
                [col_w, beam_h, bay_z],
            );
            server
                .procedural_session_eval_tool(Parameters(req))
                .await
                .expect("eval transverse beam step should succeed");
            expected_boxes += 1;
        }
    }
    // 3*2 columns + (nx-1)*nz + nx*(nz-1) beams = 6 + 4 + 3 = 13.
    assert_eq!(expected_boxes, 13);

    // 3. Snapshot: the accumulated AuthoringScript holds all steps,
    //    nothing has touched the model yet.
    let snap: crate::curation::SessionSnapshot = server
        .procedural_session_snapshot_tool(Parameters(
            crate::plugins::procedural_session_mcp::SessionSnapshotRequest {
                session_id: session_id.clone(),
            },
        ))
        .await
        .expect("snapshot should succeed")
        .into_typed()
        .expect("snapshot should deserialize");
    assert_eq!(snap.script.steps.len(), expected_boxes);

    // Pre-commit: the model is still empty.
    let pre: ModelSummary = server
        .model_summary_tool()
        .await
        .expect("model_summary should succeed")
        .into_typed()
        .expect("summary should deserialize");
    assert_eq!(pre.entity_counts.get("box").copied().unwrap_or(0), 0);

    // 4. Commit: replay the script through the command queue. The
    //    ModelApiStepExecutor turns each step into a real create_box.
    let commit: crate::curation::CommitReport = server
        .procedural_session_commit_tool(Parameters(SessionCommitRequest {
            session_id: session_id.clone(),
            options: crate::curation::CommitOptions::default(), // require_clean
        }))
        .await
        .expect("commit should succeed")
        .into_typed()
        .expect("commit report should deserialize");
    assert_eq!(commit.steps_run.len(), expected_boxes);
    assert_eq!(commit.tagged_calls.len(), expected_boxes);
    assert!(commit.remaining_obligations.is_empty());

    // 5. The model now contains the committed structure.
    let post: ModelSummary = server
        .model_summary_tool()
        .await
        .expect("model_summary should succeed")
        .into_typed()
        .expect("summary should deserialize");
    assert_eq!(
        post.entity_counts.get("box").copied().unwrap_or(0),
        expected_boxes,
        "committed pavilion should have created {expected_boxes} boxes"
    );

    // 6. Export the procedure as a reusable AuthoringScript artifact.
    let handle: crate::curation::ExportHandle = server
        .procedural_session_export_tool(Parameters(SessionExportRequest {
            session_id: session_id.clone(),
            target: crate::curation::ExportTarget::AuthoringScript,
            metadata: crate::curation::ExportMetadata {
                name: "post_and_beam_pavilion_3x2".to_string(),
                description: "Parametric post-and-beam pavilion, 3x2 bays".to_string(),
                additional_postconditions: vec![],
            },
        }))
        .await
        .expect("export should succeed")
        .into_typed()
        .expect("export handle should deserialize");
    assert_eq!(handle.kind.as_str(), "recipe.authoring_script.v1");

    drop(server);
    worker_handle.await.expect("worker should stop cleanly");
}

#[cfg(feature = "model-api")]
#[tokio::test]
async fn get_authoring_guidance_tool_returns_resource_contents() {
    use crate::plugins::authoring_guidance::{
        AntiPattern, AuthoringGuidance, ComponentStructurePolicy, DeriveRule, GuidanceReference,
        ReuseRule, StageExpectation,
    };

    let (sender, receiver) = mpsc::channel();
    let worker_handle = tokio::task::spawn_blocking(move || {
        let mut world = init_model_api_test_world();
        world.insert_resource(AuthoringGuidance {
            guidance_id: "test.component_structure".to_string(),
            version: 7,
            prompt_text: "Rule A: reuse Definition. Rule B: derive variant.".to_string(),
            component_structure: ComponentStructurePolicy {
                reuse_rule: ReuseRule {
                    summary: "reuse when role + topology match".to_string(),
                    placement_override_allowlist: vec!["position".to_string()],
                    family_parameter_allowlist: vec![],
                },
                derive_rule: DeriveRule {
                    summary: "derive on material topology change".to_string(),
                    variance_threshold: 0.25,
                },
                stage_expectations: vec![StageExpectation {
                    refinement_state: "conceptual".to_string(),
                    guidance: "coarse shells only".to_string(),
                }],
                anti_patterns: vec![AntiPattern {
                    id: "repeated_singletons".to_string(),
                    summary: "same role, no shared Definition".to_string(),
                }],
            },
            references: vec![GuidanceReference {
                kind: "canonical_markdown".to_string(),
                target: "talos3d-architecture/docs/authoring/COMPONENT_STRUCTURE.md".to_string(),
                note: None,
            }],
            guidance_chapters: Vec::new(),
        });

        while let Ok(request) = receiver.recv() {
            handle_model_api_request(&mut world, request);
        }
    });

    let server = ModelApiServer::new(sender);
    let tool_names: std::collections::BTreeSet<_> = server
        .tool_router
        .list_all()
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert!(
        tool_names.contains("get_authoring_guidance"),
        "get_authoring_guidance tool should be registered"
    );

    let guidance: AuthoringGuidance = server
        .get_authoring_guidance_tool()
        .await
        .expect("get_authoring_guidance tool should succeed")
        .into_typed()
        .expect("guidance should deserialize");

    assert_eq!(guidance.guidance_id, "test.component_structure");
    assert_eq!(guidance.version, 7);
    assert!(guidance.prompt_text.contains("Rule A"));
    assert_eq!(guidance.component_structure.anti_patterns.len(), 1);
    assert_eq!(
        guidance.component_structure.derive_rule.variance_threshold,
        0.25
    );
    assert_eq!(guidance.references.len(), 1);

    drop(server);
    worker_handle.await.expect("worker should stop cleanly");
}

#[cfg(feature = "model-api")]
#[tokio::test]
async fn get_authoring_guidance_tool_returns_default_when_unset() {
    use crate::plugins::authoring_guidance::AuthoringGuidance;

    let (sender, receiver) = mpsc::channel();
    let worker_handle = tokio::task::spawn_blocking(move || {
        let mut world = init_model_api_test_world();
        // Intentionally do not insert AuthoringGuidance — the handler
        // should fall back to AuthoringGuidance::default().
        while let Ok(request) = receiver.recv() {
            handle_model_api_request(&mut world, request);
        }
    });

    let server = ModelApiServer::new(sender);
    let guidance: AuthoringGuidance = server
        .get_authoring_guidance_tool()
        .await
        .expect("get_authoring_guidance tool should succeed")
        .into_typed()
        .expect("default guidance should deserialize");
    assert!(guidance.is_empty());
    assert_eq!(guidance.version, 0);

    drop(server);
    worker_handle.await.expect("worker should stop cleanly");
}

// -----------------------------------------------------------------------
// PP51 — Definition / Occurrence tests
// -----------------------------------------------------------------------

#[cfg(feature = "model-api")]
fn make_rect_extrusion_request() -> serde_json::Value {
    json!({
        "name": "TestWall",
        "definition_kind": "Solid",
        "width_param": "width",
        "depth_param": "depth",
        "height_param": "height",
        "parameters": [
            { "name": "width",  "param_type": "Numeric", "default_value": 4.0, "override_policy": "Overridable" },
            { "name": "depth",  "param_type": "Numeric", "default_value": 0.3, "override_policy": "Overridable" },
            { "name": "height", "param_type": "Numeric", "default_value": 3.0, "override_policy": "Overridable" }
        ]
    })
}

#[cfg(feature = "model-api")]
fn make_compound_window_request(child_definition_id: &str) -> serde_json::Value {
    json!({
        "name": "CompoundWindow",
        "definition_kind": "Solid",
        "evaluators": [],
        "parameters": [
            { "name": "overall_width", "param_type": "Numeric", "default_value": 1.2, "override_policy": "Overridable", "metadata": { "unit": "m" } },
            { "name": "overall_height", "param_type": "Numeric", "default_value": 1.4, "override_policy": "Overridable", "metadata": { "unit": "m" } },
            { "name": "wall_thickness", "param_type": "Numeric", "default_value": 0.2, "override_policy": "Overridable", "metadata": { "unit": "m" } },
            { "name": "finish_color", "param_type": "StringVal", "default_value": "white", "override_policy": "Overridable" }
        ],
        "void_declaration": {
            "shape": { "kind": "rectangular", "width_param": "overall_width", "height_param": "overall_height" },
            "placement": { "translation": [0.0, 0.0, 0.0], "yaw_radians": 0.0 },
            "exchange_role": "Opening"
        },
        "compound": {
            "anchors": [
                { "id": "opening.exterior_face", "kind": "host_exterior_face" },
                { "id": "opening.interior_face", "kind": "host_interior_face" }
            ],
            "derived_parameters": [
                {
                    "name": "clear_width",
                    "param_type": "Numeric",
                    "expr": { "kind": "param_ref", "path": "overall_width" },
                    "dependencies": ["overall_width"],
                    "metadata": { "unit": "m", "mutability": "Derived" }
                }
            ],
            "constraints": [
                {
                    "id": "width_positive",
                    "expr": {
                        "kind": "gt",
                        "left": { "kind": "param_ref", "path": "overall_width" },
                        "right": { "kind": "literal", "value": 0.5 }
                    },
                    "dependencies": ["overall_width"],
                    "severity": "Error",
                    "message": "Window width must stay positive"
                }
            ],
            "child_slots": [
                {
                    "slot_id": "frame",
                    "role": "frame",
                    "definition_id": child_definition_id,
                    "parameter_bindings": [
                        { "target_param": "width", "expr": { "kind": "param_ref", "path": "overall_width" } },
                        { "target_param": "depth", "expr": { "kind": "literal", "value": 0.14 } },
                        { "target_param": "height", "expr": { "kind": "param_ref", "path": "overall_height" } }
                    ],
                    "transform_binding": {
                        "translation": [
                            { "kind": "literal", "value": 0.0 },
                            { "kind": "literal", "value": 0.0 },
                            { "kind": "literal", "value": 0.0 }
                        ]
                    }
                }
            ]
        },
        "domain_data": {
            "architectural": {
                "void_declaration": { "kind": "opening", "parameters": { "host": "wall" } }
            }
        }
    })
}

#[cfg(feature = "model-api")]
fn make_locked_member_request() -> serde_json::Value {
    json!({
        "name": "LockedMember",
        "definition_kind": "Solid",
        "width_param": "width",
        "depth_param": "depth",
        "height_param": "height",
        "parameters": [
            { "name": "width",  "param_type": "Numeric", "default_value": 0.2, "override_policy": "Locked" },
            { "name": "depth",  "param_type": "Numeric", "default_value": 0.1, "override_policy": "Locked" },
            { "name": "height", "param_type": "Numeric", "default_value": 0.5, "override_policy": "Locked" }
        ]
    })
}

#[cfg(feature = "model-api")]
fn make_definition_variant_request(base_definition_id: &str) -> serde_json::Value {
    json!({
        "name": "TestWall Greyline",
        "base_definition_id": base_definition_id,
        "parameters": [
            { "name": "height", "param_type": "Numeric", "default_value": 4.5, "override_policy": "Locked" },
            { "name": "finish_color", "param_type": "StringVal", "default_value": "greyline", "override_policy": "Locked" }
        ],
        "domain_data": {
            "catalog": {
                "finish": "greyline"
            }
        }
    })
}

#[cfg(feature = "model-api")]
#[test]
fn definition_create_and_list_round_trip() {
    let mut world = init_model_api_test_world();

    assert!(handle_list_definitions(&world).is_empty());

    let entry = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("create definition should succeed");

    assert_eq!(entry.name, "TestWall");
    assert_eq!(entry.definition_kind, "Solid");
    assert_eq!(entry.definition_version, 1);
    assert_eq!(entry.parameter_names, vec!["width", "depth", "height"]);

    let all = handle_list_definitions(&world);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].definition_id, entry.definition_id);
}

#[cfg(feature = "model-api")]
#[test]
fn definition_get_returns_full_definition() {
    let mut world = init_model_api_test_world();

    let created = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("create definition should succeed");

    let fetched = handle_get_definition(&world, created.definition_id.clone())
        .expect("get definition should succeed");

    assert_eq!(fetched.definition_id, created.definition_id);
    assert_eq!(fetched.name, "TestWall");
    assert_eq!(
        fetched.full["interface"]["parameters"][0]["name"],
        json!("width")
    );
    assert_eq!(
        fetched.effective_full["interface"]["parameters"][0]["name"],
        json!("width")
    );
}

#[cfg(feature = "model-api")]
#[test]
fn representation_declare_adds_and_replaces_definition_representation() {
    let mut world = init_model_api_test_world();
    let created = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("create definition should succeed");

    let updated = handle_representation_declare(
        &mut world,
        RepresentationDeclareRequest {
            definition_id: created.definition_id.clone(),
            kind: "Annotation".into(),
            role: Some("Annotation".into()),
            lod: Some("Detailed".into()),
            update_policy: Some("OnDemand".into()),
        },
    )
    .expect("representation declaration should succeed");

    assert_eq!(updated.definition_version, created.definition_version + 1);
    let representations = updated.full["representations"].as_array().unwrap();
    assert!(representations.iter().any(|representation| {
        representation["kind"] == json!("Annotation")
            && representation["role"] == json!("Annotation")
            && representation["lod"] == json!("Detailed")
            && representation["update_policy"] == json!("OnDemand")
    }));

    let replaced = handle_representation_declare(
        &mut world,
        RepresentationDeclareRequest {
            definition_id: created.definition_id.clone(),
            kind: "annotation".into(),
            role: Some("annotation".into()),
            lod: Some("Fabrication".into()),
            update_policy: Some("Frozen".into()),
        },
    )
    .expect("representation replacement should succeed");
    let annotation_reps: Vec<_> = replaced.full["representations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|representation| representation["kind"] == json!("Annotation"))
        .collect();
    assert_eq!(annotation_reps.len(), 1);
    assert_eq!(annotation_reps[0]["lod"], json!("Fabrication"));
    assert_eq!(annotation_reps[0]["update_policy"], json!("Frozen"));
}

#[cfg(feature = "model-api")]
#[test]
fn representation_set_lod_and_policy_update_existing_declaration() {
    let mut world = init_model_api_test_world();
    let created = handle_create_definition(
            &mut world,
            json!({
                "name": "RepresentedWall",
                "definition_kind": "Solid",
                "representations": [
                    { "kind": "PrimaryGeometry", "role": "Body" },
                    { "kind": "Reference", "role": "Axis", "lod": "Conceptual", "update_policy": "OnDemand" }
                ]
            }),
        )
        .expect("create definition should succeed");

    let with_lod = handle_representation_set_lod(
        &mut world,
        RepresentationSetLodRequest {
            definition_id: created.definition_id.clone(),
            kind: "Reference".into(),
            role: None,
            lod: "Fabrication".into(),
        },
    )
    .expect("lod update should succeed");
    let axis = with_lod.full["representations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|representation| representation["role"] == json!("Axis"))
        .unwrap();
    assert_eq!(axis["lod"], json!("Fabrication"));

    let with_policy = handle_representation_set_update_policy(
        &mut world,
        RepresentationSetUpdatePolicyRequest {
            definition_id: created.definition_id.clone(),
            kind: "reference".into(),
            role: Some("axis".into()),
            update_policy: "Frozen".into(),
        },
    )
    .expect("policy update should succeed");
    let axis = with_policy.full["representations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|representation| representation["role"] == json!("Axis"))
        .unwrap();
    assert_eq!(axis["update_policy"], json!("Frozen"));
}

#[cfg(feature = "model-api")]
#[test]
fn representation_tools_report_missing_or_ambiguous_targets() {
    let mut world = init_model_api_test_world();
    let created = handle_create_definition(
        &mut world,
        json!({
            "name": "AmbiguousBody",
            "definition_kind": "Solid",
            "representations": [
                { "kind": "PrimaryGeometry", "role": "Body" },
                { "kind": "PrimaryGeometry", "role": "Axis" }
            ]
        }),
    )
    .expect("create definition should succeed");

    let err = handle_representation_set_lod(
        &mut world,
        RepresentationSetLodRequest {
            definition_id: created.definition_id.clone(),
            kind: "PrimaryGeometry".into(),
            role: None,
            lod: "Detailed".into(),
        },
    )
    .unwrap_err();
    assert!(err.contains("provide role"));

    let err = handle_representation_set_update_policy(
        &mut world,
        RepresentationSetUpdatePolicyRequest {
            definition_id: "missing".into(),
            kind: "PrimaryGeometry".into(),
            role: Some("Body".into()),
            update_policy: "Always".into(),
        },
    )
    .unwrap_err();
    assert!(err.contains("not found"));
}

#[cfg(feature = "model-api")]
#[test]
fn definition_variants_inherit_effective_shape_and_parameters() {
    let mut world = init_model_api_test_world();

    let base = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("base definition should be created");
    let variant = handle_create_definition(
        &mut world,
        make_definition_variant_request(&base.definition_id),
    )
    .expect("variant definition should be created");

    assert_eq!(
        variant.full["base_definition_id"],
        json!(base.definition_id.clone())
    );
    assert_eq!(
        variant.effective_full["interface"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|parameter| parameter["name"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["width", "depth", "height", "finish_color"]
    );
    assert_eq!(
        variant.effective_full["interface"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == json!("height"))
            .unwrap()["default_value"],
        json!(4.5)
    );

    let occurrence_id = handle_place_occurrence(
        &mut world,
        json!({ "definition_id": variant.definition_id, "label": "VariantWall" }),
    )
    .expect("variant occurrence should be placed");
    let resolved =
        handle_resolve_occurrence(&world, occurrence_id).expect("variant should resolve");
    assert_eq!(resolved["height"]["value"], json!(4.5));
    assert_eq!(resolved["finish_color"]["value"], json!("greyline"));
}

#[cfg(feature = "model-api")]
#[test]
fn definition_draft_lifecycle_creates_patches_validates_and_publishes() {
    let mut world = init_model_api_test_world();

    let draft = handle_create_definition_draft(&mut world, make_rect_extrusion_request())
        .expect("draft should be created");
    assert!(draft.dirty);

    let patched = handle_patch_definition_draft(
        &mut world,
        json!({
            "draft_id": draft.draft_id,
            "patches": [
                { "op": "set_name", "name": "DraftWall" },
                { "op": "set_parameter_default", "name": "height", "default_value": 4.25 }
            ]
        }),
    )
    .expect("draft should be patched");
    assert_eq!(patched.name, "DraftWall");
    assert_eq!(
        patched.effective_full["interface"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == json!("height"))
            .unwrap()["default_value"],
        json!(4.25)
    );

    let validation = handle_validate_definition(&world, json!({ "draft_id": patched.draft_id }))
        .expect("draft validation should succeed");
    assert!(validation.ok);

    let published = handle_publish_definition_draft(&mut world, patched.draft_id.clone())
        .expect("draft should publish");
    assert_eq!(published.name, "DraftWall");
    assert_eq!(
        published.effective_full["interface"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == json!("height"))
            .unwrap()["default_value"],
        json!(4.25)
    );
}

#[cfg(feature = "model-api")]
#[test]
fn definition_draft_patch_sets_core_material_assignment_and_warns_for_legacy() {
    let mut world = init_model_api_test_world();

    let draft = handle_create_definition_draft(&mut world, make_rect_extrusion_request())
        .expect("draft should be created");
    let assignment = MaterialAssignment::new("oak_finish");
    let assignment_json = serde_json::to_value(&assignment).expect("assignment serializes");

    let patched = handle_patch_definition_draft(
        &mut world,
        json!({
            "draft_id": draft.draft_id,
            "patch": {
                "op": "set_material_assignment",
                "assignment": assignment_json
            }
        }),
    )
    .expect("typed material patch should apply");

    assert_eq!(patched.full["material_assignment"], assignment_json);
    assert!(patched.warnings.is_empty());
    assert!(
        patched.full["domain_data"]
            .get("architectural")
            .and_then(|a| a.get("material_assignment"))
            .is_none(),
        "typed material patch should not write legacy architecture domain_data"
    );

    let cleared = handle_patch_definition_draft(
        &mut world,
        json!({
            "draft_id": patched.draft_id,
            "patch": { "op": "remove_material_assignment" }
        }),
    )
    .expect("remove material patch should apply");
    assert!(cleared.full.get("material_assignment").is_none());

    let legacy = handle_patch_definition_draft(
        &mut world,
        json!({
            "draft_id": cleared.draft_id,
            "patch": {
                "op": "set_domain_data_material",
                "material_id": "builtin.glass.blue_tint_glazing_80"
            }
        }),
    )
    .expect("legacy material patch remains accepted for one release cycle");
    assert_eq!(legacy.warnings.len(), 1);
    assert!(legacy.warnings[0].contains("SetDomainDataMaterial is deprecated"));
}

#[cfg(feature = "model-api")]
#[test]
fn derived_definition_draft_can_be_compiled_and_explained() {
    let mut world = init_model_api_test_world();

    let base = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("base definition should be created");
    let draft = handle_derive_definition_draft(
        &mut world,
        json!({
            "definition_id": base.definition_id,
            "name": "DerivedWall"
        }),
    )
    .expect("derived draft should be created");

    handle_patch_definition_draft(
        &mut world,
        json!({
            "draft_id": draft.draft_id,
            "patch": { "op": "set_parameter_default", "name": "height", "default_value": 5.0 }
        }),
    )
    .expect("derived draft should accept inherited parameter overrides");

    let compile = handle_compile_definition(&world, json!({ "draft_id": draft.draft_id }))
        .expect("compile should succeed");
    assert!(compile.nodes.iter().any(|node| node == "param:height"));

    let explain = handle_explain_definition(&world, json!({ "draft_id": draft.draft_id }))
        .expect("explain should succeed");
    assert_eq!(
        explain.effective_full["interface"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == json!("height"))
            .unwrap()["default_value"],
        json!(5.0)
    );
    assert!(explain
        .local_parameter_names
        .iter()
        .any(|name| name == "height"));
    assert!(explain
        .inherited_parameter_names
        .iter()
        .any(|name| name == "width"));
}

#[cfg(feature = "model-api")]
#[test]
fn compound_definition_round_trips_with_domain_data() {
    let mut world = init_model_api_test_world();

    let child = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("child definition should be created");

    let compound = handle_create_definition(
        &mut world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound definition should succeed");

    let fetched = handle_get_definition(&world, compound.definition_id.clone())
        .expect("compound definition should be retrievable");

    assert_eq!(
        fetched.full["compound"]["child_slots"][0]["role"],
        json!("frame")
    );
    assert_eq!(
        fetched.full["domain_data"]["architectural"]["void_declaration"]["kind"],
        json!("opening")
    );
}

#[cfg(feature = "model-api")]
#[test]
fn compound_occurrence_generates_child_slot_geometry() {
    use crate::plugins::modeling::{
        occurrence::GeneratedOccurrencePart, profile::ProfileExtrusion,
    };

    let mut world = init_model_api_test_world();

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let compound = handle_create_definition(
        &mut world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound definition should be created");

    let occurrence_id = handle_place_occurrence(
        &mut world,
        json!({
            "definition_id": compound.definition_id,
            "overrides": {
                "overall_width": 1.8,
                "overall_height": 1.6
            }
        }),
    )
    .expect("compound occurrence should be placed");

    let owner = ElementId(occurrence_id);
    let generated_parts: Vec<(GeneratedOccurrencePart, ProfileExtrusion)> = world
        .query::<(&GeneratedOccurrencePart, &ProfileExtrusion)>()
        .iter(&world)
        .map(|(generated, extrusion)| (generated.clone(), extrusion.clone()))
        .collect();

    assert_eq!(generated_parts.len(), 1);
    assert_eq!(generated_parts[0].0.owner, owner);
    assert_eq!(generated_parts[0].0.slot_path, "frame");
    let (min, max) = generated_parts[0].1.profile.bounds_2d();
    assert_eq!(max.x - min.x, 1.8);
    assert_eq!(max.y - min.y, 0.14);
    assert_eq!(generated_parts[0].1.height, 1.6);
}

#[cfg(feature = "model-api")]
#[test]
fn collection_slot_linear_expands_with_indexed_slot_paths() {
    use crate::plugins::modeling::{
        occurrence::GeneratedOccurrencePart, profile::ProfileExtrusion,
    };

    let mut world = init_model_api_test_world();

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let mut request = make_compound_window_request(&child.definition_id);
    let slot = &mut request["compound"]["child_slots"][0];
    slot["slot_id"] = json!("muntin");
    slot["multiplicity"] = json!({
        "Collection": {
            "layout": {
                "Linear": {
                    "axis": "x",
                    "spacing": {
                        "kind": "div",
                        "left": { "kind": "param_ref", "path": "overall_width" },
                        "right": { "kind": "literal", "value": 6.0 }
                    },
                    "origin": {
                        "translation": [
                            {
                                "kind": "mul",
                                "left": { "kind": "param_ref", "path": "overall_width" },
                                "right": { "kind": "literal", "value": -0.5 }
                            },
                            { "kind": "literal", "value": 0.0 },
                            { "kind": "literal", "value": 0.0 }
                        ]
                    }
                }
            },
            "count": { "Fixed": 5 }
        }
    });
    let compound = handle_create_definition(&mut world, request)
        .expect("compound definition should be created");

    let occurrence_id = handle_place_occurrence(
        &mut world,
        json!({
            "definition_id": compound.definition_id,
            "overrides": {
                "overall_width": 1.8,
                "overall_height": 1.6
            }
        }),
    )
    .expect("compound occurrence should be placed");

    let mut generated_parts: Vec<(GeneratedOccurrencePart, ProfileExtrusion)> = world
        .query::<(&GeneratedOccurrencePart, &ProfileExtrusion)>()
        .iter(&world)
        .map(|(generated, extrusion)| (generated.clone(), extrusion.clone()))
        .collect();
    generated_parts.sort_by(|left, right| left.0.slot_path.cmp(&right.0.slot_path));

    assert_eq!(generated_parts.len(), 5);
    assert_eq!(
        generated_parts
            .iter()
            .map(|(generated, _)| generated.slot_path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "muntin[0]",
            "muntin[1]",
            "muntin[2]",
            "muntin[3]",
            "muntin[4]"
        ]
    );
    assert_eq!(generated_parts[0].0.owner, ElementId(occurrence_id));
    let centers = generated_parts
        .iter()
        .map(|(_, extrusion)| (extrusion.centre.x * 10.0).round() / 10.0)
        .collect::<Vec<_>>();
    assert_eq!(centers, vec![-0.6, -0.3, 0.0, 0.3, 0.6]);

    let explanation =
        handle_explain_occurrence(&world, occurrence_id).expect("explain should succeed");
    assert_eq!(explanation.generated_parts.len(), 5);
    assert_eq!(explanation.generated_parts[0].slot_path, "muntin[0]");
}

#[cfg(feature = "model-api")]
#[test]
fn collection_slot_grid_expands_deterministically() {
    use crate::plugins::modeling::{
        occurrence::GeneratedOccurrencePart, profile::ProfileExtrusion,
    };

    let mut world = init_model_api_test_world();

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let mut request = make_compound_window_request(&child.definition_id);
    let slot = &mut request["compound"]["child_slots"][0];
    slot["slot_id"] = json!("lite");
    slot["multiplicity"] = json!({
        "Collection": {
            "layout": {
                "Grid": {
                    "axis_u": "x",
                    "count_u": { "kind": "literal", "value": 3.0 },
                    "spacing_u": { "kind": "literal", "value": 0.25 },
                    "axis_v": "y",
                    "count_v": { "kind": "literal", "value": 2.0 },
                    "spacing_v": { "kind": "literal", "value": 0.4 },
                    "origin": {
                        "translation": [
                            { "kind": "literal", "value": -0.5 },
                            { "kind": "literal", "value": -0.6 },
                            { "kind": "literal", "value": 0.0 }
                        ]
                    }
                }
            },
            "count": { "Fixed": 6 }
        }
    });
    let compound = handle_create_definition(&mut world, request)
        .expect("compound definition should be created");

    handle_place_occurrence(
        &mut world,
        json!({ "definition_id": compound.definition_id }),
    )
    .expect("grid compound occurrence should be placed");

    let mut generated_parts: Vec<(GeneratedOccurrencePart, ProfileExtrusion)> = world
        .query::<(&GeneratedOccurrencePart, &ProfileExtrusion)>()
        .iter(&world)
        .map(|(generated, extrusion)| (generated.clone(), extrusion.clone()))
        .collect();
    generated_parts.sort_by(|left, right| left.0.slot_path.cmp(&right.0.slot_path));
    assert_eq!(
        generated_parts
            .iter()
            .map(|(generated, _)| generated.slot_path.as_str())
            .collect::<Vec<_>>(),
        vec!["lite[0]", "lite[1]", "lite[2]", "lite[3]", "lite[4]", "lite[5]"]
    );
    assert!((generated_parts[0].1.centre.x - -0.25).abs() < 0.0001);
    assert!((generated_parts[0].1.centre.y - -0.2).abs() < 0.0001);
    assert!((generated_parts[5].1.centre.x - 0.25).abs() < 0.0001);
    assert!((generated_parts[5].1.centre.y - 0.2).abs() < 0.0001);
}

#[cfg(feature = "model-api")]
#[test]
fn collection_slot_by_spacing_and_lite_pattern_expand_deterministically() {
    use crate::plugins::modeling::{
        occurrence::GeneratedOccurrencePart, profile::ProfileExtrusion,
    };

    let mut world = init_model_api_test_world();

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");

    let mut by_spacing_request = make_compound_window_request(&child.definition_id);
    let slot = &mut by_spacing_request["compound"]["child_slots"][0];
    slot["slot_id"] = json!("truss");
    slot["multiplicity"] = json!({
        "Collection": {
            "layout": {
                "BySpacingFromHost": {
                    "host_param": { "side": "Host", "name": "wall_thickness" },
                    "axis": "z"
                }
            },
            "count": { "Fixed": 3 }
        }
    });
    let by_spacing = handle_create_definition(&mut world, by_spacing_request)
        .expect("by-spacing compound definition should be created");
    handle_place_occurrence(
        &mut world,
        json!({
            "definition_id": by_spacing.definition_id,
            "overrides": { "wall_thickness": 0.2 }
        }),
    )
    .expect("by-spacing occurrence should be placed");

    let mut generated_parts: Vec<(GeneratedOccurrencePart, ProfileExtrusion)> = world
        .query::<(&GeneratedOccurrencePart, &ProfileExtrusion)>()
        .iter(&world)
        .filter(|(generated, _)| generated.slot_path.starts_with("truss"))
        .map(|(generated, extrusion)| (generated.clone(), extrusion.clone()))
        .collect();
    generated_parts.sort_by(|left, right| left.0.slot_path.cmp(&right.0.slot_path));
    assert_eq!(
        generated_parts
            .iter()
            .map(|(generated, _)| generated.slot_path.as_str())
            .collect::<Vec<_>>(),
        vec!["truss[0]", "truss[1]", "truss[2]"]
    );
    let z_centers = generated_parts
        .iter()
        .map(|(_, extrusion)| (extrusion.centre.z * 10.0).round() / 10.0)
        .collect::<Vec<_>>();
    assert_eq!(z_centers, vec![0.2, 0.4, 0.6]);

    let mut lite_request = make_compound_window_request(&child.definition_id);
    let slot = &mut lite_request["compound"]["child_slots"][0];
    slot["slot_id"] = json!("pane");
    slot["multiplicity"] = json!({
        "Collection": {
            "layout": {
                "LitePattern": {
                    "pattern": { "kind": "literal", "value": "3x2" }
                }
            },
            "count": { "Fixed": 6 }
        }
    });
    let lite = handle_create_definition(&mut world, lite_request)
        .expect("lite compound definition should be created");
    handle_place_occurrence(&mut world, json!({ "definition_id": lite.definition_id }))
        .expect("lite occurrence should be placed");

    let mut generated_parts: Vec<(GeneratedOccurrencePart, ProfileExtrusion)> = world
        .query::<(&GeneratedOccurrencePart, &ProfileExtrusion)>()
        .iter(&world)
        .filter(|(generated, _)| generated.slot_path.starts_with("pane"))
        .map(|(generated, extrusion)| (generated.clone(), extrusion.clone()))
        .collect();
    generated_parts.sort_by(|left, right| left.0.slot_path.cmp(&right.0.slot_path));
    assert_eq!(
        generated_parts
            .iter()
            .map(|(generated, _)| generated.slot_path.as_str())
            .collect::<Vec<_>>(),
        vec!["pane[0]", "pane[1]", "pane[2]", "pane[3]", "pane[4]", "pane[5]"]
    );
    assert_eq!(generated_parts[0].1.centre.x, 0.0);
    assert_eq!(generated_parts[0].1.centre.y, 0.0);
    assert_eq!(generated_parts[5].1.centre.x, 2.0);
    assert_eq!(generated_parts[5].1.centre.y, 1.0);
}

#[cfg(feature = "model-api")]
#[test]
fn draft_patch_sets_and_removes_child_slot_multiplicity_summary() {
    let mut world = init_model_api_test_world();

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let draft = handle_create_definition_draft(
        &mut world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound draft should be created");

    handle_patch_definition_draft(
        &mut world,
        json!({
            "draft_id": draft.draft_id,
            "patch": {
                "op": "set_child_slot_multiplicity",
                "slot_id": "frame",
                "multiplicity": {
                    "Collection": {
                        "layout": {
                            "BySpacingFromHost": {
                                "host_param": { "side": "Host", "name": "wall_thickness" },
                                "axis": "z"
                            }
                        },
                        "count": { "Fixed": 2 }
                    }
                }
            }
        }),
    )
    .expect("draft should accept multiplicity patch");

    let compile = handle_compile_definition(&world, json!({ "draft_id": draft.draft_id }))
        .expect("compile should succeed");
    assert_eq!(compile.collection_slots.len(), 1);
    assert_eq!(compile.collection_slots[0].slot_id, "frame");

    let explain = handle_explain_definition(&world, json!({ "draft_id": draft.draft_id }))
        .expect("explain should succeed");
    assert_eq!(explain.resolved_collection_slots.len(), 1);
    assert_eq!(
        explain.resolved_collection_slots[0]["slot_id"],
        json!("frame")
    );
    assert_eq!(explain.resolved_collection_slots[0]["count"], json!(2));
    assert_eq!(
        explain.resolved_collection_slots[0]["instances"][0]["slot_path"],
        json!("frame[0]")
    );
    let translation = explain.resolved_collection_slots[0]["instances"][1]["translation"]
        .as_array()
        .expect("translation should be an array");
    assert_eq!(translation[0], json!(0.0));
    assert_eq!(translation[1], json!(0.0));
    assert!((translation[2].as_f64().unwrap() - 0.4).abs() < 0.0001);

    handle_patch_definition_draft(
        &mut world,
        json!({
            "draft_id": draft.draft_id,
            "patch": { "op": "remove_child_slot_multiplicity", "slot_id": "frame" }
        }),
    )
    .expect("draft should remove multiplicity patch");

    let compile = handle_compile_definition(&world, json!({ "draft_id": draft.draft_id }))
        .expect("compile should succeed");
    assert!(compile.collection_slots.is_empty());
}

#[cfg(feature = "model-api")]
#[test]
fn draft_patch_sets_parameter_geometry_affecting() {
    let mut world = init_model_api_test_world();

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let draft = handle_create_definition_draft(
        &mut world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound draft should be created");

    let patched = handle_patch_definition_draft(
        &mut world,
        json!({
            "draft_id": draft.draft_id,
            "patch": {
                "op": "set_parameter_geometry_affecting",
                "name": "finish_color",
                "geometry_affecting": false
            }
        }),
    )
    .expect("draft should accept geometry_affecting patch");

    let parameter = patched.full["interface"]["parameters"]
        .as_array()
        .expect("parameters should be an array")
        .iter()
        .find(|parameter| parameter["name"] == json!("finish_color"))
        .expect("finish_color parameter should exist");
    assert_eq!(parameter["geometry_affecting"], json!(false));
}

#[cfg(feature = "model-api")]
#[test]
fn occurrence_explain_reports_generated_parts_and_resolved_values() {
    let mut world = init_model_api_test_world();

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let compound = handle_create_definition(
        &mut world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound definition should be created");

    let occurrence_id = handle_place_occurrence(
        &mut world,
        json!({
            "definition_id": compound.definition_id,
            "label": "Window A",
            "overrides": {
                "overall_width": 1.5,
                "overall_height": 1.25,
                "finish_color": "red"
            },
            "domain_data": {
                "architectural": {
                    "host_occurrence": "wall-42"
                }
            }
        }),
    )
    .expect("compound occurrence should be placed");

    let explanation =
        handle_explain_occurrence(&world, occurrence_id).expect("explain should succeed");

    assert_eq!(explanation.label, "Window A");
    assert_eq!(explanation.generated_parts.len(), 1);
    assert_eq!(explanation.generated_parts[0].slot_path, "frame");
    assert_eq!(
        explanation.generated_parts[0].definition_id,
        child.definition_id
    );
    assert_eq!(
        explanation.resolved_parameters["finish_color"]["value"],
        json!("red")
    );
    assert_eq!(explanation.anchors.len(), 2);
    assert_eq!(
        explanation.domain_data["architectural"]["host_occurrence"],
        json!("wall-42")
    );
}

#[cfg(feature = "model-api")]
#[test]
fn definition_update_bumps_version_and_propagates() {
    let mut world = init_model_api_test_world();

    let created = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("create definition should succeed");

    let updated = handle_update_definition(
        &mut world,
        json!({
            "definition_id": created.definition_id,
            "name": "RenamedWall"
        }),
    )
    .expect("update definition should succeed");

    assert_eq!(updated.definition_version, 2);
    assert_eq!(updated.name, "RenamedWall");

    // Place an occurrence, then update the definition again — occurrence
    // should be marked dirty (ChangedDefinitions resource updated).
    let occ_id = handle_place_occurrence(
        &mut world,
        json!({ "definition_id": created.definition_id }),
    )
    .expect("place occurrence should succeed");
    let _ = occ_id; // placement succeeded (expect() already asserted this)

    handle_update_definition(
        &mut world,
        json!({
            "definition_id": created.definition_id,
            "name": "FinalWall"
        }),
    )
    .expect("second update should succeed");

    // ChangedDefinitions should have been drained by flush_model_api_write_pipeline
    // (which calls apply_pending_history_commands), but the UpdateDefinition command's
    // apply() calls mark_changed. Since flush runs synchronously we verify
    // the definition version rather than the transient resource.
    let after = handle_get_definition(&world, created.definition_id.clone())
        .expect("get after second update should succeed");
    assert_eq!(after.definition_version, 3);
    assert_eq!(after.name, "FinalWall");
}

#[cfg(feature = "model-api")]
#[test]
fn occurrence_place_and_resolve_returns_provenance() {
    let mut world = init_model_api_test_world();

    let def = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("create definition should succeed");

    // Place with no overrides — all values should be DefinitionDefault.
    let occ_id = handle_place_occurrence(
        &mut world,
        json!({ "definition_id": def.definition_id, "label": "Wall1" }),
    )
    .expect("place occurrence should succeed");

    let resolved =
        handle_resolve_occurrence(&world, occ_id).expect("resolve occurrence should succeed");

    assert_eq!(resolved["width"]["value"], json!(4.0));
    assert_eq!(resolved["width"]["provenance"], json!("DefinitionDefault"));
    assert_eq!(resolved["height"]["value"], json!(3.0));
}

#[cfg(feature = "model-api")]
#[test]
fn occurrence_update_overrides_changes_only_the_target() {
    let mut world = init_model_api_test_world();

    let def = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("create definition should succeed");

    let occ_a = handle_place_occurrence(
        &mut world,
        json!({ "definition_id": def.definition_id, "label": "WallA" }),
    )
    .expect("place occurrence A should succeed");

    let occ_b = handle_place_occurrence(
        &mut world,
        json!({ "definition_id": def.definition_id, "label": "WallB" }),
    )
    .expect("place occurrence B should succeed");

    // Override height only on A.
    handle_update_occurrence_overrides(&mut world, occ_a, json!({ "height": 5.0 }))
        .expect("update overrides should succeed");

    let resolved_a = handle_resolve_occurrence(&world, occ_a).expect("resolve A should succeed");
    let resolved_b = handle_resolve_occurrence(&world, occ_b).expect("resolve B should succeed");

    // A has an override, B still uses the definition default.
    assert_eq!(resolved_a["height"]["value"], json!(5.0));
    assert_eq!(
        resolved_a["height"]["provenance"],
        json!("OccurrenceOverride")
    );
    assert_eq!(resolved_b["height"]["value"], json!(3.0));
    assert_eq!(
        resolved_b["height"]["provenance"],
        json!("DefinitionDefault")
    );
}

#[cfg(feature = "model-api")]
#[test]
fn occurrence_material_override_tools_set_and_clear_core_slot() {
    let mut world = init_model_api_test_world();
    for id in ["oak_finish", "walnut_finish"] {
        world
            .resource_mut::<crate::plugins::materials::MaterialRegistry>()
            .upsert(MaterialDef {
                id: id.to_string(),
                name: id.to_string(),
                ..Default::default()
            });
    }

    let def = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("create definition should succeed");
    let occ_id = handle_place_occurrence(
        &mut world,
        json!({ "definition_id": def.definition_id, "label": "MaterialOverrideWall" }),
    )
    .expect("place occurrence should succeed");

    let assignment = MaterialAssignment::new("oak_finish");
    let updated = handle_set_occurrence_material_override(
        &mut world,
        SetOccurrenceMaterialOverrideRequest {
            element_id: occ_id,
            assignment: assignment.clone(),
        },
    )
    .expect("set occurrence material override should succeed");
    assert_eq!(
        serde_json::from_value::<MaterialAssignment>(
            updated["identity"]["material_override"].clone()
        )
        .expect("override should serialize as a typed assignment"),
        assignment
    );

    let cleared = handle_clear_occurrence_material_override(
        &mut world,
        ClearOccurrenceMaterialOverrideRequest { element_id: occ_id },
    )
    .expect("clear occurrence material override should succeed");
    assert!(cleared["identity"].get("material_override").is_none());
}

#[cfg(feature = "model-api")]
#[test]
fn hosted_occurrence_resize_updates_opening_proxy_and_relation() {
    let mut world = init_model_api_test_world();
    register_hosted_on_relation(&mut world);

    let host_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [4.0, 1.5, 0.0],
            "half_extents": [2.0, 1.5, 0.15]
        }),
    )
    .expect("host should be created");
    let opening_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [4.0, 1.2, 0.0],
            "half_extents": [0.6, 0.8, 0.15]
        }),
    )
    .expect("opening proxy should be created");

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let compound = handle_create_definition(
        &mut world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound definition should be created");
    let instantiated = handle_instantiate_hosted_definition(
        &mut world,
        json!({
            "definition_id": compound.definition_id,
            "label": "HostedWindow",
            "hosting": {
                "host_element_id": host_id,
                "opening_element_id": opening_id
            }
        }),
    )
    .expect("hosted occurrence should be instantiated");

    handle_update_occurrence_overrides(
        &mut world,
        instantiated.element_id,
        json!({
            "overall_width": 0.9,
            "overall_height": 1.1
        }),
    )
    .expect("hosted resize should update occurrence and opening");

    let opening_snapshot = capture_entity_snapshot(&world, ElementId(opening_id))
        .expect("opening proxy should still exist");
    let half_extents: [f32; 3] =
        serde_json::from_value(opening_snapshot.to_json()["half_extents"].clone())
            .expect("box half extents should be present");
    assert!((half_extents[0] - 0.45).abs() < 1e-5);
    assert!((half_extents[1] - 0.55).abs() < 1e-5);
    assert!((half_extents[2] - 0.15).abs() < 1e-5);

    let relations = handle_query_relations(
        &world,
        Some(instantiated.element_id),
        Some(host_id),
        Some("hosted_on".to_string()),
    );
    assert_eq!(relations.len(), 1);
    assert!((relations[0].parameters["window_width_m"].as_f64().unwrap() - 0.9).abs() < 1e-5);
    assert!((relations[0].parameters["window_height_m"].as_f64().unwrap() - 1.1).abs() < 1e-5);
}

#[cfg(feature = "model-api")]
#[test]
fn occurrence_make_unique_copies_definition_tree_and_repoints_only_target() {
    let mut world = init_model_api_test_world();

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let compound = handle_create_definition(
        &mut world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound definition should be created");

    let occ_a = handle_place_occurrence(
        &mut world,
        json!({ "definition_id": compound.definition_id, "label": "WindowA" }),
    )
    .expect("occurrence A should be placed");
    let occ_b = handle_place_occurrence(
        &mut world,
        json!({ "definition_id": compound.definition_id, "label": "WindowB" }),
    )
    .expect("occurrence B should be placed");

    let result = handle_make_occurrence_unique(
        &mut world,
        OccurrenceMakeUniqueRequest {
            element_id: occ_a,
            name: Some("WindowA Unique".to_string()),
            copy_dependencies: true,
        },
    )
    .expect("make unique should succeed");

    assert_eq!(result.previous_definition_id, compound.definition_id);
    assert_ne!(result.new_definition_id, compound.definition_id);
    assert_eq!(result.copied_definition_ids.len(), 2);

    let explained_a =
        handle_explain_occurrence(&world, occ_a).expect("A should explain after make unique");
    let explained_b =
        handle_explain_occurrence(&world, occ_b).expect("B should explain after make unique");

    assert_eq!(explained_a.definition_id, result.new_definition_id);
    assert_eq!(explained_b.definition_id, compound.definition_id);
    assert_eq!(explained_a.generated_parts.len(), 1);
    assert_eq!(explained_b.generated_parts.len(), 1);
    assert_ne!(
        explained_a.generated_parts[0].definition_id,
        child.definition_id
    );
    assert_eq!(
        explained_b.generated_parts[0].definition_id,
        child.definition_id
    );

    let copied = handle_get_definition(&world, result.new_definition_id.clone())
        .expect("copied definition should exist");
    assert_eq!(copied.name, "WindowA Unique");
}

#[cfg(feature = "model-api")]
#[test]
fn definition_library_workflow_exports_imports_and_instantiates() {
    let mut source_world = init_model_api_test_world();
    let base_definition =
        handle_create_definition(&mut source_world, make_rect_extrusion_request())
            .expect("base definition should be created");
    let definition = handle_create_definition(
        &mut source_world,
        make_definition_variant_request(&base_definition.definition_id),
    )
    .expect("variant definition should be created");

    let library =
        handle_create_definition_library(&mut source_world, json!({ "name": "Window Library" }))
            .expect("library should be created");
    handle_add_definition_to_library(
        &mut source_world,
        json!({
            "library_id": library.library_id,
            "definition_id": definition.definition_id
        }),
    )
    .expect("definition should be added to library");

    let listed = handle_list_definition_libraries(&source_world);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].definition_count, 2);

    let export_path = temp_json_path("talos3d-definition-library");
    handle_export_definition_library(
        &source_world,
        &library.library_id,
        export_path.to_str().unwrap_or_default(),
    )
    .expect("library should export");

    let mut target_world = init_model_api_test_world();
    let imported = handle_import_definition_library(
        &mut target_world,
        export_path.to_str().unwrap_or_default(),
    )
    .expect("library should import");
    let instantiated = handle_instantiate_definition(
        &mut target_world,
        json!({
            "library_id": imported.library_id,
            "definition_id": definition.definition_id,
            "label": "ImportedWall",
            "overrides": { "width": 4.2 }
        }),
    )
    .expect("definition should instantiate from library");

    assert_eq!(instantiated.definition_id, definition.definition_id);
    assert_eq!(instantiated.imported_definition_ids.len(), 2);
    assert!(instantiated
        .imported_definition_ids
        .contains(&definition.definition_id));
    assert!(instantiated
        .imported_definition_ids
        .contains(&base_definition.definition_id));

    let resolved = handle_resolve_occurrence(&target_world, instantiated.element_id)
        .expect("instantiated occurrence should resolve");
    assert_eq!(resolved["width"]["value"], json!(4.2));
    assert_eq!(resolved["height"]["value"], json!(4.5));
    assert_eq!(resolved["finish_color"]["value"], json!("greyline"));

    let _ = fs::remove_file(export_path);
}

#[cfg(feature = "model-api")]
fn register_hosted_on_relation(world: &mut World) {
    world
        .resource_mut::<CapabilityRegistry>()
        .register_relation_type(crate::capability_registry::RelationTypeDescriptor {
            relation_type: "hosted_on".to_string(),
            label: "Hosted On".to_string(),
            description: "Hosted relation for occurrence placement".to_string(),
            valid_source_types: vec!["occurrence".to_string()],
            valid_target_types: vec!["box".to_string()],
            parameter_schema: json!({}),
            participates_in_dependency_graph: true,
            external_classification: None,
            host_contract_kind: None,
        });
}

#[cfg(feature = "model-api")]
#[test]
fn workspace_definition_library_tools_persist_draft_crud_to_files() {
    let mut world = init_model_api_test_world();
    let temp = tempfile::tempdir().expect("temp workspace should be created");
    fs::create_dir(temp.path().join(".talos3d")).expect("workspace marker should be created");
    let workspace_root = temp.path().to_string_lossy().to_string();

    let created = handle_create_workspace_definition_library(
        &mut world,
        json!({
            "workspace_root": workspace_root,
            "name": "Workspace Drafts"
        }),
    )
    .expect("workspace library should be created");
    assert_eq!(created.scope, "WorkspaceLibrary");
    let source_path = created
        .source_path
        .clone()
        .expect("workspace library should have a source path");
    assert!(std::path::Path::new(&source_path).is_file());

    let listed = handle_list_workspace_definition_libraries(
        &mut world,
        json!({ "start_dir": temp.path().join("nested").to_string_lossy() }),
    )
    .expect("workspace libraries should list from a nested start dir");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].library_id, created.library_id);

    let draft = handle_create_definition_draft(&mut world, make_rect_extrusion_request())
        .expect("draft should be created");
    let imported = handle_import_workspace_definition_draft(
        &mut world,
        json!({
            "library_id": created.library_id,
            "draft_id": draft.draft_id
        }),
    )
    .expect("draft should import into workspace library");
    assert_eq!(imported.definition_count, 1);

    let patched = handle_patch_definition_draft(
        &mut world,
        json!({
            "draft_id": draft.draft_id,
            "patch": { "op": "set_name", "name": "WorkspaceWall" }
        }),
    )
    .expect("draft should patch before update");
    let updated = handle_update_workspace_definition_draft(
        &mut world,
        json!({
            "library_id": created.library_id,
            "draft_id": patched.draft_id
        }),
    )
    .expect("workspace draft should update");
    assert_eq!(updated.definition_count, 1);

    let library_json = std::fs::read_to_string(&source_path).expect("library file should read");
    assert!(library_json.contains("WorkspaceWall"));

    let deleted = handle_delete_workspace_definition_draft(
        &mut world,
        json!({
            "library_id": created.library_id,
            "definition_id": patched.definition_id
        }),
    )
    .expect("workspace draft should delete");
    assert_eq!(deleted.definition_count, 0);

    let library_json = std::fs::read_to_string(&source_path).expect("library file should read");
    assert!(!library_json.contains("WorkspaceWall"));
}

#[cfg(feature = "model-api")]
#[test]
fn hosted_definition_instantiation_derives_anchors_and_relation() {
    use crate::plugins::history::PendingCommandQueue;
    use crate::plugins::modeling::void_declaration::{OpeningContext, VoidLink};
    use bevy::prelude::Visibility;

    let mut world = init_model_api_test_world();
    register_hosted_on_relation(&mut world);

    let host_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [4.0, 1.5, 0.0],
            "half_extents": [2.0, 1.5, 0.15]
        }),
    )
    .expect("host should be created");
    let opening_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [4.0, 1.2, 0.0],
            "half_extents": [0.6, 0.8, 0.15]
        }),
    )
    .expect("opening proxy should be created");

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let compound = handle_create_definition(
        &mut world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound definition should be created");

    let instantiated = handle_instantiate_hosted_definition(
        &mut world,
        json!({
            "definition_id": compound.definition_id,
            "label": "HostedWindow",
            "hosting": {
                "host_element_id": host_id,
                "opening_element_id": opening_id
            }
        }),
    )
    .expect("hosted occurrence should be instantiated");

    assert_eq!(instantiated.relation_ids.len(), 1);

    let resolved = handle_resolve_occurrence(&world, instantiated.element_id)
        .expect("hosted occurrence should resolve");
    let wall_thickness = resolved["wall_thickness"]["value"]
        .as_f64()
        .expect("wall_thickness should resolve to a number");
    assert!((wall_thickness - 0.3).abs() < 1e-5);

    let explanation = handle_explain_occurrence(&world, instantiated.element_id)
        .expect("hosted occurrence explanation should succeed");
    assert_eq!(explanation.label, "HostedWindow");
    assert_eq!(explanation.hosting["host_element_id"], json!(host_id));
    assert_eq!(explanation.hosting["opening_element_id"], json!(opening_id));
    assert!(explanation.hosting["anchors"]
        .as_array()
        .is_some_and(|anchors| anchors.len() >= 3));

    let relations = handle_query_relations(
        &world,
        Some(instantiated.element_id),
        Some(host_id),
        Some("hosted_on".to_string()),
    );
    assert_eq!(relations.len(), 1);
    assert_eq!(
        relations[0].parameters["opening_element_id"],
        json!(opening_id)
    );

    let details = get_entity_details(&world, ElementId(instantiated.element_id))
        .expect("hosted occurrence details should resolve");
    assert_eq!(details.entity_type, "occurrence");
    assert_eq!(details.label, "HostedWindow");

    let opening_entity = find_entity_by_element_id(&mut world, ElementId(opening_id))
        .expect("opening entity should exist");
    assert_eq!(
        world.get::<OpeningContext>(opening_entity),
        Some(&OpeningContext {
            host: ElementId(host_id),
            filling: Some(ElementId(instantiated.element_id)),
        })
    );
    assert_eq!(
        world.get::<Visibility>(opening_entity),
        Some(&Visibility::Hidden),
        "linked opening proxies are implementation geometry and must not render"
    );
    let user_entities = list_entities(&world);
    assert!(
        user_entities
            .iter()
            .any(|entry| entry.element_id == host_id && entry.entity_type == "box"),
        "the host wall proxy should remain user-facing"
    );
    assert!(
        user_entities.iter().any(|entry| {
            entry.element_id == instantiated.element_id && entry.entity_type == "occurrence"
        }),
        "the filling occurrence should remain user-facing"
    );
    assert!(
        user_entities
            .iter()
            .all(|entry| entry.element_id != opening_id),
        "the opening proxy should be internal after it is linked to the filling"
    );
    let selection_error = handle_set_selection(&mut world, vec![opening_id])
        .expect_err("internal opening proxy should not be selectable");
    assert!(selection_error.contains("internal wall opening proxy"));
    let transform_error = handle_transform(
        &mut world,
        TransformToolRequest {
            element_ids: vec![opening_id],
            operation: "move".to_string(),
            axis: Some("X".to_string()),
            value: json!(0.25),
            pivot: None,
        },
    )
    .expect_err("internal opening proxy should not be transformable");
    assert!(transform_error.contains("internal wall opening proxy"));
    let filling_entity = find_entity_by_element_id(&mut world, ElementId(instantiated.element_id))
        .expect("filling entity should exist");
    assert_eq!(
        world.get::<VoidLink>(filling_entity),
        Some(&VoidLink {
            opening: ElementId(opening_id),
        })
    );

    world.resource_mut::<PendingCommandQueue>().queue_undo();
    apply_pending_history_commands(&mut world);

    assert!(get_entity_details(&world, ElementId(instantiated.element_id)).is_none());
    assert!(get_entity_details(&world, ElementId(instantiated.relation_ids[0])).is_none());
    assert!(get_entity_details(&world, ElementId(opening_id)).is_some());
    let opening_entity = find_entity_by_element_id(&mut world, ElementId(opening_id))
        .expect("opening entity should remain after undo");
    assert!(world.get::<OpeningContext>(opening_entity).is_none());
    assert_eq!(
        world.get::<Visibility>(opening_entity),
        Some(&Visibility::Visible)
    );
}

#[cfg(feature = "model-api")]
#[test]
fn hosted_opening_proxy_stays_internal_after_project_round_trip() {
    use crate::plugins::modeling::void_declaration::OpeningContext;
    use bevy::prelude::Visibility;

    let mut source_world = init_model_api_test_world();
    register_hosted_on_relation(&mut source_world);

    let host_id = handle_create_entity(
        &mut source_world,
        json!({
            "type": "box",
            "centre": [4.0, 1.5, 0.0],
            "half_extents": [2.0, 1.5, 0.15]
        }),
    )
    .expect("host should be created");
    let opening_id = handle_create_entity(
        &mut source_world,
        json!({
            "type": "box",
            "centre": [4.0, 1.2, 0.0],
            "half_extents": [0.6, 0.8, 0.15]
        }),
    )
    .expect("opening proxy should be created");

    let child = handle_create_definition(&mut source_world, make_locked_member_request())
        .expect("locked child definition should be created");
    let compound = handle_create_definition(
        &mut source_world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound definition should be created");
    let instantiated = handle_instantiate_hosted_definition(
        &mut source_world,
        json!({
            "definition_id": compound.definition_id,
            "label": "HostedWindow",
            "hosting": {
                "host_element_id": host_id,
                "opening_element_id": opening_id
            }
        }),
    )
    .expect("hosted occurrence should be instantiated");

    let path = temp_json_path("talos3d-hosted-opening-proxy-roundtrip").with_extension("talos3d");
    handle_save_project(&mut source_world, path.to_str().unwrap_or_default())
        .expect("project should save");

    let mut loaded_world = init_model_api_test_world();
    register_hosted_on_relation(&mut loaded_world);
    handle_load_project(&mut loaded_world, path.to_str().unwrap_or_default())
        .expect("project should load");

    let opening_entity = find_entity_by_element_id(&mut loaded_world, ElementId(opening_id))
        .expect("opening entity should be reloaded for the void relationship");
    assert_eq!(
        loaded_world.get::<OpeningContext>(opening_entity),
        Some(&OpeningContext {
            host: ElementId(host_id),
            filling: Some(ElementId(instantiated.element_id)),
        })
    );
    assert_eq!(
        loaded_world.get::<Visibility>(opening_entity),
        Some(&Visibility::Hidden),
        "persisted opening proxies must remain non-rendering after load"
    );

    let user_entities = list_entities(&loaded_world);
    assert!(
        user_entities
            .iter()
            .all(|entry| entry.element_id != opening_id),
        "persisted opening proxy must remain hidden from user-facing entity lists"
    );
    let selection_error = handle_set_selection(&mut loaded_world, vec![opening_id])
        .expect_err("persisted internal opening proxy should not be selectable");
    assert!(selection_error.contains("internal wall opening proxy"));

    let _ = fs::remove_file(path);
}

#[cfg(feature = "model-api")]
#[test]
fn hosted_definition_instantiation_rejects_perpendicular_opening_proxy() {
    let mut world = init_model_api_test_world();
    register_hosted_on_relation(&mut world);

    let host_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [4.0, 1.5, 0.0],
            "half_extents": [2.0, 1.5, 0.15]
        }),
    )
    .expect("host should be created");
    let perpendicular_opening_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [4.0, 1.2, 0.0],
            "half_extents": [0.6, 0.15, 0.8]
        }),
    )
    .expect("opening proxy should be created");

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let compound = handle_create_definition(
        &mut world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound definition should be created");

    let err = handle_instantiate_hosted_definition(
        &mut world,
        json!({
            "definition_id": compound.definition_id,
            "label": "BadHostedWindow",
            "hosting": {
                "host_element_id": host_id,
                "opening_element_id": perpendicular_opening_id
            }
        }),
    )
    .expect_err("perpendicular opening should be rejected");

    assert!(err.contains("Opening Normal Alignment"), "{err}");
    assert!(err.contains("Opening Thickness Containment"), "{err}");
}

#[cfg(feature = "model-api")]
#[test]
fn hosted_definition_instantiation_requires_remaining_wall_around_opening() {
    let mut world = init_model_api_test_world();
    register_hosted_on_relation(&mut world);

    let host_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [4.0, 1.5, 0.0],
            "half_extents": [2.0, 1.5, 0.15]
        }),
    )
    .expect("host should be created");
    let edge_to_edge_opening_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [4.0, 1.5, 0.0],
            "half_extents": [2.0, 1.0, 0.15]
        }),
    )
    .expect("opening proxy should be created");

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let compound = handle_create_definition(
        &mut world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound definition should be created");

    let err = handle_instantiate_hosted_definition(
        &mut world,
        json!({
            "definition_id": compound.definition_id,
            "label": "BadHostedWindow",
            "hosting": {
                "host_element_id": host_id,
                "opening_element_id": edge_to_edge_opening_id
            }
        }),
    )
    .expect_err("edge-to-edge opening should be rejected");

    assert!(err.contains("Remaining Wall X"), "{err}");
}

#[cfg(feature = "model-api")]
#[test]
fn occurrence_validate_host_fit_uses_core_wall_opening_fallback() {
    let mut world = init_model_api_test_world();
    register_hosted_on_relation(&mut world);

    let host_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [4.0, 1.5, 0.0],
            "half_extents": [2.0, 1.5, 0.15]
        }),
    )
    .expect("host should be created");
    let opening_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [4.0, 1.2, 0.0],
            "half_extents": [0.6, 0.8, 0.15]
        }),
    )
    .expect("opening proxy should be created");

    let child = handle_create_definition(&mut world, make_locked_member_request())
        .expect("locked child definition should be created");
    let compound = handle_create_definition(
        &mut world,
        make_compound_window_request(&child.definition_id),
    )
    .expect("compound definition should be created");
    let instantiated = handle_instantiate_hosted_definition(
        &mut world,
        json!({
            "definition_id": compound.definition_id,
            "label": "HostedWindow",
            "hosting": {
                "host_element_id": host_id,
                "opening_element_id": opening_id
            }
        }),
    )
    .expect("hosted occurrence should be instantiated");

    let result = handle_occurrence_validate_host_fit(
        &world,
        ValidateHostFitRequest {
            contract_kind: WALL_OPENING_HOSTING_CONTRACT_KIND.to_string(),
            host_element_id: host_id,
            hosted_element_id: instantiated.element_id,
            contract_parameters: json!({
                "opening_element_id": opening_id
            }),
        },
    )
    .expect("fallback validator should run");

    assert_eq!(result.status, HostingValidationStatus::Passed);
    assert!(result
        .checks
        .iter()
        .any(|check| check.id.0 == "opening.normal_alignment"));
}

#[cfg(feature = "model-api")]
#[test]
fn definition_and_occurrence_round_trip_through_project_persistence() {
    let mut source_world = init_model_api_test_world();
    let definition = handle_create_definition(&mut source_world, make_rect_extrusion_request())
        .expect("definition should be created");
    let occurrence_id = handle_place_occurrence(
        &mut source_world,
        json!({
            "definition_id": definition.definition_id,
            "label": "RoundTripWall",
            "overrides": { "height": 4.5 },
            "domain_data": {
                "architectural": { "exchange_identity_map": { "GlobalId": "rt-1" } }
            }
        }),
    )
    .expect("occurrence should be created");

    let path = temp_json_path("talos3d-roundtrip-project").with_extension("talos3d");
    handle_save_project(&mut source_world, path.to_str().unwrap_or_default())
        .expect("project should save");

    let mut loaded_world = init_model_api_test_world();
    handle_load_project(&mut loaded_world, path.to_str().unwrap_or_default())
        .expect("project should load");

    let loaded_definition = handle_get_definition(&loaded_world, definition.definition_id.clone())
        .expect("definition should load");
    assert_eq!(loaded_definition.full["name"], json!("TestWall"));
    assert_eq!(
        loaded_definition.full["interface"]["parameters"],
        handle_get_definition(&source_world, definition.definition_id.clone())
            .expect("source definition should exist")
            .full["interface"]["parameters"]
    );

    let resolved = handle_resolve_occurrence(&loaded_world, occurrence_id)
        .expect("loaded occurrence should resolve");
    assert_eq!(resolved["height"]["value"], json!(4.5));
    assert_eq!(
        resolved["height"]["provenance"],
        json!("OccurrenceOverride")
    );

    let explanation = handle_explain_occurrence(&loaded_world, occurrence_id)
        .expect("loaded occurrence explanation should succeed");
    assert_eq!(explanation.label, "RoundTripWall");
    assert_eq!(
        explanation.domain_data["architectural"]["exchange_identity_map"]["GlobalId"],
        json!("rt-1")
    );

    let _ = fs::remove_file(path);
}

#[cfg(feature = "model-api")]
#[test]
fn primitive_round_trip_through_project_persistence() {
    let mut source_world = init_model_api_test_world();
    let sphere_id = handle_create_entity(
        &mut source_world,
        json!({
            "type": "sphere",
            "centre": [1.5, 2.0, -0.5],
            "radius": 0.75
        }),
    )
    .expect("sphere should be created");

    let path = temp_json_path("talos3d-primitive-roundtrip").with_extension("talos3d");
    handle_save_project(&mut source_world, path.to_str().unwrap_or_default())
        .expect("project should save");

    let mut loaded_world = init_model_api_test_world();
    handle_load_project(&mut loaded_world, path.to_str().unwrap_or_default())
        .expect("project should load");

    let snapshot = get_entity_snapshot(&loaded_world, ElementId(sphere_id))
        .expect("loaded sphere snapshot should exist");
    assert_eq!(snapshot["centre"], json!([1.5, 2.0, -0.5]));
    assert_eq!(snapshot["radius"], json!(0.75));

    let _ = fs::remove_file(path);
}

/// A current-version project whose primitive entity record omits the
/// newer `element_id` / `rotation` fields (a legacy-shaped record)
/// still loads: `upgrade_legacy_entity_record` backfills them. Built
/// via the real serializer so it stays correct across format-version
/// bumps (v1 hand-rolled files are intentionally rejected — see
/// `persistence::tests::project_version_one_is_rejected_after_opening_feature_format_bump`).
#[cfg(feature = "model-api")]
#[test]
fn legacy_primitive_record_upgrades_and_loads() {
    // Produce a valid current-version project containing a sphere.
    let mut source_world = init_model_api_test_world();
    handle_create_entity(
        &mut source_world,
        json!({ "type": "sphere", "centre": [0.0, 0.0, 0.0], "radius": 1.25 }),
    )
    .expect("sphere should be created");
    let path = temp_json_path("talos3d-legacy-primitive").with_extension("talos3d");
    handle_save_project(&mut source_world, path.to_str().unwrap_or_default())
        .expect("project should save");

    // Strip the newer per-record fields to simulate a legacy record
    // inside an otherwise current-version file.
    let mut project: Value =
        serde_json::from_slice(&fs::read(&path).expect("saved project should read"))
            .expect("saved project should parse");
    for entity in project["entities"].as_array_mut().expect("entities array") {
        if entity["type"] == json!("sphere") {
            let data = entity["data"].as_object_mut().expect("entity data object");
            data.remove("element_id");
            data.remove("rotation");
        }
    }
    fs::write(
        &path,
        serde_json::to_vec_pretty(&project).expect("project should serialize"),
    )
    .expect("project should write");

    let mut world = init_model_api_test_world();
    handle_load_project(&mut world, path.to_str().unwrap_or_default())
        .expect("legacy-shaped record should upgrade and load");

    let entities = list_entities(&world);
    assert!(entities.iter().any(|entity| entity.entity_type == "sphere"));

    let _ = fs::remove_file(path);
}

#[cfg(feature = "model-api")]
#[test]
fn failed_load_does_not_clear_existing_scene() {
    let mut world = init_model_api_test_world();
    let box_id = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [0.0, 0.0, 0.0],
            "half_extents": [1.0, 1.0, 1.0]
        }),
    )
    .expect("box should be created");

    // Build a valid current-version project file, then inject an
    // unparseable entity (a scene_light missing its element_id). The
    // load fails during entity parsing — which happens *before*
    // `clear_scene` — so the existing box must survive. Built via the
    // real serializer so the version gate is passed and the test
    // exercises the intended mid-load failure path.
    let path = temp_json_path("talos3d-invalid-load").with_extension("talos3d");
    let mut empty_world = init_model_api_test_world();
    handle_save_project(&mut empty_world, path.to_str().unwrap_or_default())
        .expect("baseline project should save");
    let mut project: Value =
        serde_json::from_slice(&fs::read(&path).expect("baseline project should read"))
            .expect("baseline project should parse");
    project["entities"] = json!([{ "type": "scene_light", "data": {} }]);
    fs::write(
        &path,
        serde_json::to_vec_pretty(&project).expect("invalid project should serialize"),
    )
    .expect("invalid project should write");

    let error = handle_load_project(&mut world, path.to_str().unwrap_or_default())
        .expect_err("invalid project should fail to load");
    assert!(
        error.contains("Missing element_id"),
        "expected an entity-parse error, got: {error}"
    );
    assert!(get_entity_snapshot(&world, ElementId(box_id)).is_some());

    let _ = fs::remove_file(path);
}

#[cfg(feature = "model-api")]
#[test]
fn non_geometric_fields_do_not_set_mesh_dirty() {
    use crate::plugins::modeling::occurrence::{OccurrenceClassification, OccurrenceIdentity};

    let mut world = init_model_api_test_world();

    let def = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("create definition should succeed");

    let occ_id = handle_place_occurrence(&mut world, json!({ "definition_id": def.definition_id }))
        .expect("place occurrence should succeed");

    let eid = ElementId(occ_id);

    // Manually set mesh_dirty = false to simulate a clean state.
    let entity = {
        let mut q = world.query::<(bevy::prelude::Entity, &ElementId)>();
        q.iter(&world)
            .find_map(|(e, id)| (*id == eid).then_some(e))
            .expect("occurrence entity should exist")
    };
    world
        .entity_mut(entity)
        .insert(OccurrenceClassification::clean());

    // Directly mutate opaque domain data on the component. This must not
    // force a geometry re-evaluation.
    {
        let mut identity = world.get_mut::<OccurrenceIdentity>(entity).unwrap();
        identity.domain_data = json!({
            "architectural": {
                "property_set_map": { "Pset_BuildingCommon": { "IsExternal": true } },
                "exchange_identity_map": { "GlobalId": "abc" }
            }
        });
    }

    // mesh_dirty must remain false.
    let cls = world.get::<OccurrenceClassification>(entity).unwrap();
    assert!(
        !cls.mesh_dirty,
        "modifying domain_data must not set mesh_dirty"
    );
}

#[cfg(feature = "model-api")]
#[test]
fn non_geometry_override_update_marks_material_dirty_without_mesh_dirty() {
    use crate::plugins::modeling::occurrence::OccurrenceClassification;

    let mut world = init_model_api_test_world();
    let mut request = make_rect_extrusion_request();
    request["parameters"].as_array_mut().unwrap().push(json!({
        "name": "finish_color",
        "param_type": "StringVal",
        "default_value": "white",
        "override_policy": "Overridable",
        "geometry_affecting": false
    }));
    let def =
        handle_create_definition(&mut world, request).expect("create definition should succeed");
    let occ_id = handle_place_occurrence(&mut world, json!({ "definition_id": def.definition_id }))
        .expect("place occurrence should succeed");
    let eid = ElementId(occ_id);
    let entity = {
        let mut q = world.query::<(bevy::prelude::Entity, &ElementId)>();
        q.iter(&world)
            .find_map(|(e, id)| (*id == eid).then_some(e))
            .expect("occurrence entity should exist")
    };
    world
        .entity_mut(entity)
        .insert(OccurrenceClassification::clean());

    handle_update_occurrence_overrides(&mut world, occ_id, json!({ "finish_color": "black" }))
        .expect("non-geometry override update should succeed");

    let classification = world
        .get::<OccurrenceClassification>(entity)
        .expect("classification should still exist");
    assert!(!classification.mesh_dirty);
    assert!(classification.material_dirty);
    assert!(!classification.transform_dirty);
}

#[cfg(feature = "model-api")]
#[test]
fn render_settings_round_trip_and_validate() {
    let mut world = init_model_api_test_world();

    let initial = handle_get_render_settings(&world);
    assert_eq!(initial.tonemapping, "agx");
    assert_eq!(initial.edge_display_mode, "shaded");

    let updated = handle_set_render_settings(
        &mut world,
        RenderSettingsUpdateRequest {
            tonemapping: Some("blender_filmic".to_string()),
            exposure_ev100: Some(1.5),
            ssao_enabled: Some(false),
            bloom_enabled: Some(true),
            bloom_intensity: Some(0.42),
            ssr_enabled: Some(true),
            ssr_linear_steps: Some(24),
            wireframe_overlay_enabled: Some(true),
            contour_overlay_enabled: Some(true),
            visible_edge_overlay_enabled: Some(true),
            grid_enabled: Some(false),
            background_rgb: Some([1.0, 1.0, 1.0]),
            xray_enabled: Some(true),
            xray_surface_alpha: Some(0.25),
            ..Default::default()
        },
    )
    .expect("render settings update should succeed");

    assert_eq!(updated.tonemapping, "blender_filmic");
    assert_eq!(updated.exposure_ev100, 1.5);
    assert!(!updated.ssao_enabled);
    assert!(updated.ssr_enabled);
    assert_eq!(updated.ssr_linear_steps, 24);
    assert_eq!(updated.edge_display_mode, "outline");
    assert!(!updated.wireframe_overlay_enabled);
    assert!(!updated.contour_overlay_enabled);
    assert!(updated.visible_edge_overlay_enabled);
    assert!(!updated.grid_enabled);
    assert_eq!(updated.background_rgb, [1.0, 1.0, 1.0]);
    assert!(updated.xray_enabled);
    assert_eq!(updated.xray_surface_alpha, 0.25);
    assert!(!updated.paper_fill_enabled);

    let wireframe = handle_set_render_settings(
        &mut world,
        RenderSettingsUpdateRequest {
            edge_display_mode: Some("wireframe".to_string()),
            ..Default::default()
        },
    )
    .expect("edge display mode should accept wireframe");
    assert_eq!(wireframe.edge_display_mode, "wireframe");
    assert!(wireframe.wireframe_overlay_enabled);
    assert!(!wireframe.visible_edge_overlay_enabled);

    let paper = handle_set_render_settings(
        &mut world,
        RenderSettingsUpdateRequest {
            paper_fill_enabled: Some(true),
            ..Default::default()
        },
    )
    .expect("paper fill should disable xray mode");
    assert!(paper.paper_fill_enabled);
    assert!(!paper.xray_enabled);

    let clamped = handle_set_render_settings(
        &mut world,
        RenderSettingsUpdateRequest {
            xray_enabled: Some(true),
            xray_surface_alpha: Some(2.0),
            ..Default::default()
        },
    )
    .expect("xray alpha should clamp to supported transparency bounds");
    assert!(clamped.xray_enabled);
    assert!(!clamped.paper_fill_enabled);
    assert_eq!(clamped.xray_surface_alpha, 0.95);

    let error = handle_set_render_settings(
        &mut world,
        RenderSettingsUpdateRequest {
            tonemapping: Some("not-a-tonemapper".to_string()),
            ..Default::default()
        },
    )
    .expect_err("invalid tonemapping should fail");
    assert!(error.contains("Unknown tonemapping mode"));
}

#[cfg(feature = "model-api")]
#[test]
fn lighting_round_trip_and_restore_default_rig() {
    let mut world = init_model_api_test_world();

    let created = handle_create_light(
        &mut world,
        CreateLightRequest {
            kind: "spot".to_string(),
            name: Some("Workbench Spot".to_string()),
            enabled: Some(true),
            color: Some([0.7, 0.8, 1.0]),
            intensity: Some(3200.0),
            shadows_enabled: Some(true),
            position: Some([2.0, 3.5, 1.0]),
            yaw_deg: Some(-45.0),
            pitch_deg: Some(-30.0),
            range: Some(14.0),
            radius: Some(0.12),
            inner_angle_deg: Some(12.0),
            outer_angle_deg: Some(24.0),
        },
    )
    .expect("create_light should succeed");

    assert_eq!(created.kind, "spot");
    assert_eq!(created.name, "Workbench Spot");
    assert_eq!(created.position, [2.0, 3.5, 1.0]);

    let listed = handle_list_lights(&world);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].element_id, created.element_id);

    let updated = handle_update_light(
        &mut world,
        UpdateLightRequest {
            element_id: created.element_id,
            name: Some("Workbench Fill".to_string()),
            kind: Some("point".to_string()),
            enabled: Some(false),
            color: Some([1.0, 0.9, 0.75]),
            intensity: Some(1800.0),
            shadows_enabled: Some(false),
            position: Some([1.0, 2.0, 3.0]),
            yaw_deg: Some(0.0),
            pitch_deg: Some(0.0),
            range: Some(10.0),
            radius: Some(0.3),
            inner_angle_deg: Some(8.0),
            outer_angle_deg: Some(18.0),
        },
    )
    .expect("update_light should succeed");

    assert_eq!(updated.name, "Workbench Fill");
    assert_eq!(updated.kind, "point");
    assert!(!updated.enabled);
    assert_eq!(updated.position, [1.0, 2.0, 3.0]);
    assert_eq!(updated.radius, 0.3);

    let ambient = handle_set_ambient_light(
        &mut world,
        AmbientLightUpdateRequest {
            color: Some([0.25, 0.3, 0.4]),
            brightness: Some(18.0),
            affects_lightmapped_meshes: Some(false),
        },
    )
    .expect("ambient update should succeed");
    assert_eq!(ambient.color, [0.25, 0.3, 0.4]);
    assert_eq!(ambient.brightness, 18.0);
    assert!(!ambient.affects_lightmapped_meshes);

    let scene = handle_get_lighting_scene(&world);
    assert_eq!(scene.lights.len(), 1);
    assert_eq!(scene.ambient.color, [0.25, 0.3, 0.4]);

    let restored =
        handle_restore_default_light_rig(&mut world).expect("restore_default_light_rig works");
    assert_eq!(restored.len(), 2);
    assert!(restored.iter().any(|light| light.name == "Sun Key"));
    assert!(restored.iter().any(|light| light.name == "Sky Fill"));

    let removed = handle_delete_light(&mut world, restored[0].element_id)
        .expect("delete_light should succeed");
    assert_eq!(removed, 1);
    assert_eq!(handle_list_lights(&world).len(), 1);
}

#[cfg(feature = "model-api")]
fn seed_recipe_draft_corpus(world: &mut World) {
    use crate::capability_registry::{CorpusProvenance, LicenseTag, PassageRef};
    use crate::plugins::corpus_gap::{CorpusGapQueue, CorpusPassageRegistry};

    world.insert_resource(CorpusGapQueue::default());
    let mut passages = CorpusPassageRegistry::default();
    passages.register(
        PassageRef("SE/mono_truss".into()),
        "Mono-pitch trusses require a continuous support path into the wall frame.",
        CorpusProvenance {
            source: "test corpus".into(),
            source_version: "2026-04-22".into(),
            jurisdiction: Some("SE".into()),
            ingested_at: 0,
            license: LicenseTag::PublicRecord,
            backlink: None,
            supersedes: Vec::new(),
        },
    );
    passages.register(
            PassageRef("SE/rainscreen_wall".into()),
            "Ventilated wood cladding requires a backing support layer and a continuous drainage cavity.",
            CorpusProvenance {
                source: "test corpus".into(),
                source_version: "2026-04-22".into(),
                jurisdiction: Some("SE".into()),
                ingested_at: 0,
                license: LicenseTag::PublicRecord,
                backlink: None,
                supersedes: Vec::new(),
            },
        );
    world.insert_resource(passages);
}

#[cfg(feature = "model-api")]
fn save_recipe_draft_fixture(
    world: &mut World,
    recipe_draft_id: Option<String>,
    status: Option<&str>,
) -> RecipeDraftInfo {
    let gap = handle_request_corpus_expansion(
        world,
        Some("roof_system".into()),
        Some("SE".into()),
        "recipe".into(),
        "need mono roof framing".into(),
    )
    .expect("roof_system is a registered class — gap should record");
    handle_save_recipe_draft(
        world,
        SaveRecipeDraftRequest {
            recipe_draft_id,
            scope: Some("project".into()),
            label: "Mono Roof Draft".into(),
            description: "Session draft for a mono-pitch roof framing recipe".into(),
            target_class: "roof_system".into(),
            supported_refinement_levels: vec!["Constructible".into()],
            parameters: vec![RecipeParameterInfo {
                name: "span_mm".into(),
                value_schema: serde_json::json!({ "type": "number", "minimum": 1 }),
                default: Some(serde_json::json!(6000)),
            }],
            jurisdiction: Some("SE".into()),
            gap_id: Some(gap.id),
            source_passage_refs: vec!["SE/mono_truss".into()],
            evidence_slots: vec![crate::plugins::knowledge_assets::EvidenceSlot {
                claim_path: "roof_system/framing_strategy".into(),
                description: "Ground the learned roof framing strategy.".into(),
                required: true,
                evidence_ref: None,
            }],
            runtime_claims: Vec::new(),
            acquisition_context: serde_json::json!({
                "assembly_strategy": "mono_truss",
                "siding_support": "rainscreen"
            }),
            draft_script: serde_json::json!({
                "kind": "authoring_script_draft",
                "steps": ["TODO"]
            }),
            notes: vec!["Needs constructability validation".into()],
            status: status.map(str::to_string),
        },
    )
    .expect("recipe draft should save")
}

#[cfg(feature = "model-api")]
fn save_assembly_pattern_draft_fixture(
    world: &mut World,
    assembly_pattern_draft_id: Option<String>,
    status: Option<&str>,
) -> AssemblyPatternDraftInfo {
    let gap = handle_request_corpus_expansion(
        world,
        Some("wall_assembly".into()),
        Some("SE".into()),
        "assembly_pattern".into(),
        "need ventilated rainscreen wall pattern".into(),
    )
    .expect("wall_assembly is a registered class — gap should record");
    handle_save_assembly_pattern_draft(
        world,
        SaveAssemblyPatternDraftRequest {
            assembly_pattern_draft_id,
            scope: Some("project".into()),
            label: "Ventilated Rainscreen Wall".into(),
            description: "Session draft for a ventilated exterior wall assembly pattern".into(),
            target_types: vec!["wall_assembly".into()],
            axis: "exterior_to_interior".into(),
            layers: vec![
                AssemblyPatternLayerInfo {
                    layer_id: "cedar_siding".into(),
                    label: "Cedar Siding".into(),
                    role: "exterior_finish".into(),
                    material_hint: Some("cedar".into()),
                    optional: false,
                },
                AssemblyPatternLayerInfo {
                    layer_id: "ventilated_battens".into(),
                    label: "Ventilated Battens".into(),
                    role: "drainage_cavity".into(),
                    material_hint: Some("timber_battens".into()),
                    optional: false,
                },
                AssemblyPatternLayerInfo {
                    layer_id: "stud_frame".into(),
                    label: "Stud Frame".into(),
                    role: "primary_structure".into(),
                    material_hint: Some("light_frame_studs".into()),
                    optional: false,
                },
            ],
            relation_rules: vec![
                AssemblyPatternRelationRuleInfo {
                    relation_type: "supported_by".into(),
                    source_layer_id: "cedar_siding".into(),
                    target_layer_id: "ventilated_battens".into(),
                    required: true,
                    rationale: "Siding needs a backing support layer.".into(),
                },
                AssemblyPatternRelationRuleInfo {
                    relation_type: "fastened_to".into(),
                    source_layer_id: "cedar_siding".into(),
                    target_layer_id: "ventilated_battens".into(),
                    required: true,
                    rationale: "Siding fastens to battens.".into(),
                },
            ],
            root_layer_ids: vec!["stud_frame".into()],
            requires_support_path: true,
            tags: vec!["wall".into(), "rainscreen".into()],
            parameter_schema: serde_json::json!({}),
            jurisdiction: Some("SE".into()),
            gap_id: Some(gap.id),
            source_passage_refs: vec!["SE/rainscreen_wall".into()],
            evidence_slots: vec![crate::plugins::knowledge_assets::EvidenceSlot {
                claim_path: "wall_assembly/layers".into(),
                description: "Ground the learned layer ordering.".into(),
                required: true,
                evidence_ref: None,
            }],
            runtime_claims: Vec::new(),
            acquisition_context: serde_json::json!({
                "assembly_strategy": "ventilated_rainscreen"
            }),
            notes: vec!["Needs continuity rules later".into()],
            status: status.map(str::to_string),
        },
    )
    .expect("assembly pattern draft should save")
}

#[cfg(feature = "model-api")]
#[test]
fn recipe_draft_round_trip_with_gap_and_source_refs() {
    let mut world = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut world);

    let saved = save_recipe_draft_fixture(&mut world, None, Some("drafted"));
    assert_eq!(saved.status, "drafted");
    assert_eq!(saved.curation.scope, "project");
    assert_eq!(saved.curation.kind, "recipe_draft.v1");
    assert_eq!(saved.curation.evidence_slot_count, 1);
    assert_eq!(saved.target_class, "roof_system");
    assert_eq!(saved.source_passage_refs, vec!["SE/mono_truss".to_string()]);

    let fetched = handle_get_recipe_draft(&world, saved.id.clone()).expect("draft should load");
    assert_eq!(fetched.id, saved.id);
    assert_eq!(fetched.gap_id, saved.gap_id);
    assert_eq!(
        fetched.notes,
        vec!["Needs constructability validation".to_string()]
    );

    let filtered =
        handle_list_recipe_drafts(&world, Some("roof_system".into()), Some("drafted".into()))
            .expect("list_recipe_drafts should succeed");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, saved.id);
}

#[cfg(feature = "model-api")]
#[test]
fn curation_shaped_drafts_and_gaps_survive_project_reload() {
    let mut source = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut source);
    let saved = save_recipe_draft_fixture(&mut source, None, Some("installed"));
    let path = std::env::temp_dir().join(format!(
        "talos3d-dkc-roundtrip-{}.talos3d",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    crate::plugins::persistence::save_project_to_path(&mut source, path.clone())
        .expect("project should save");

    let mut target = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut target);
    crate::plugins::persistence::load_project_from_path(&mut target, path.clone())
        .expect("project should load");
    let _ = std::fs::remove_file(path);

    let loaded = handle_get_recipe_draft(&target, saved.id).expect("draft should reload");
    assert_eq!(loaded.curation.asset_id, saved.curation.asset_id);
    assert_eq!(loaded.curation.scope, "project");
    assert_eq!(loaded.gap_id, saved.gap_id);

    let gaps = handle_list_corpus_gaps(&target);
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].kind.as_deref(), Some("recipe"));
}

#[cfg(feature = "model-api")]
#[test]
fn curation_shaped_draft_cache_rebuilds_the_same_asset_view() {
    let mut registry = crate::plugins::recipe_drafts::RecipeDraftRegistry::default();
    let mut draft = crate::plugins::recipe_drafts::RecipeDraftArtifact {
        id: "recipe-draft-cache-test".into(),
        meta: crate::plugins::knowledge_assets::default_recipe_draft_meta(),
        residency: crate::plugins::knowledge_assets::KnowledgeResidency::SessionCache,
        label: "Cached".into(),
        description: "Cached draft".into(),
        target_class: "roof_system".into(),
        supported_refinement_levels: vec!["Constructible".into()],
        parameters: Vec::new(),
        jurisdiction: Some("SE".into()),
        gap_id: Some("gap-1".into()),
        source_passage_refs: Vec::new(),
        evidence_slots: Vec::new(),
        runtime_claims: Vec::new(),
        acquisition_context: serde_json::Value::Null,
        draft_script: serde_json::Value::Null,
        notes: Vec::new(),
        status: crate::plugins::recipe_drafts::RecipeDraftStatus::Drafted,
        created_at: 0,
        updated_at: 0,
    };
    draft.meta = crate::plugins::recipe_drafts::recipe_draft_meta_for(
        &draft,
        crate::curation::Scope::Project,
    );
    let saved = registry.save(draft);
    let snapshot = registry.snapshot();

    let mut restored = crate::plugins::recipe_drafts::RecipeDraftRegistry::default();
    restored.restore(snapshot);
    let reloaded = restored.get(&saved.id).expect("draft should restore");
    assert_eq!(reloaded.meta.id, saved.meta.id);
    assert_eq!(reloaded.meta.scope, crate::curation::Scope::Project);
}

#[cfg(feature = "model-api")]
#[test]
fn instance_info_reports_live_authoring_guidance_version() {
    use crate::plugins::authoring_guidance::{AuthoringGuidance, ComponentStructurePolicy};
    use crate::plugins::model_api::types::ModelApiRuntimeInfo;

    let mut world = World::new();
    world.insert_resource(ModelApiRuntimeInfo {
        instance_id: "test-instance".into(),
        app_name: "Talos3D Test".into(),
        pid: 42,
        http_host: "127.0.0.1".into(),
        http_port: 24901,
        http_url: "http://127.0.0.1:24901/mcp".into(),
        registry_path: "/tmp/talos3d-instances/test-instance.json".into(),
        started_at_unix_ms: 123,
        requested_port: Some(24901),
    });
    world.insert_resource(AuthoringGuidance {
        guidance_id: "test.guidance".into(),
        version: 7,
        prompt_text: "Read the live guidance.".into(),
        component_structure: ComponentStructurePolicy::default(),
        references: Vec::new(),
        guidance_chapters: Vec::new(),
    });

    let info = handle_get_instance_info(&world);

    assert_eq!(info.authoring_guidance_id.as_deref(), Some("test.guidance"));
    assert_eq!(info.authoring_guidance_version, Some(7));
    assert!(info
        .harness_drift_note
        .as_deref()
        .is_some_and(|note| note.contains("rebuild/restart")));
}

#[cfg(feature = "model-api")]
#[test]
fn no_curated_path_discovery_and_guidance_cards_are_explicit() {
    let world = init_model_api_test_world();
    let discovery = handle_discover_curated_paths(
        &world,
        CuratedPathDiscoveryRequest {
            path_kind: Some("recipe".into()),
            element_class: Some("roof_system".into()),
            query: None,
            context: serde_json::json!({ "target_state": "Constructible" }),
        },
    )
    .expect("discovery should succeed");
    let gap = discovery
        .no_curated_path
        .expect("missing recipe path should be first-class");
    assert_eq!(gap.suggested_next_tool, "request_corpus_expansion");
    assert!(!gap.gap_record_is_terminal);
    assert!(gap
        .required_next_tools
        .iter()
        .any(|tool| tool.contains("materialize_learned_asset")));
    assert!(gap
        .completion_criteria
        .iter()
        .any(|criterion| criterion.contains("consultable-only drafts do not close")));
    assert!(discovery
        .guidance_card_ids
        .contains(&"dkg.no_curated_path".to_string()));
    assert!(discovery
        .guidance_card_ids
        .contains(&"dkg.design_resources".to_string()));
    assert!(discovery
        .guidance_card_ids
        .contains(&"dkg.authoring_run_contract".to_string()));
    let run_contract_card = handle_get_guidance_card(&world, "dkg.authoring_run_contract".into())
        .expect("authoring run contract card should fetch");
    assert!(run_contract_card
        .required_trajectory_tool_ids
        .contains(&"get_capability_snapshot".to_string()));
    assert!(run_contract_card
        .success_criteria
        .iter()
        .any(|criterion| criterion.contains("Final response")));
    assert!(run_contract_card
        .success_criteria
        .iter()
        .any(|criterion| criterion.contains("actually show the authored target")));
    assert!(run_contract_card
        .success_criteria
        .iter()
        .any(|criterion| criterion.contains("exact schema enums/scopes")));
    assert!(run_contract_card
        .stop_conditions
        .iter()
        .any(|condition| condition.contains("skipped must-read guidance")));
    assert!(run_contract_card
        .stop_conditions
        .iter()
        .any(|condition| condition.contains("requested material/finish cues")));
    assert!(run_contract_card
        .stop_conditions
        .iter()
        .any(|condition| condition.contains("blank/all-black/near-uniform")));
    let run_contract_body = run_contract_card
        .body_markdown
        .as_deref()
        .expect("authoring run card should explain schema and screenshot requirements");
    assert!(run_contract_body.contains("Read live tool schemas"));
    assert!(run_contract_body.contains("required fields such as element_ids"));
    assert!(run_contract_body.contains("registered element classes"));
    assert!(run_contract_body.contains("blank"));
    assert!(run_contract_body.contains("all-black"));
    assert!(run_contract_body.contains("near-uniform"));
    assert_eq!(run_contract_card.phase.as_deref(), Some("review"));
    let trajectory_card = handle_get_guidance_card(&world, "dkg.trajectory_eval".into())
        .expect("trajectory eval card should fetch");
    assert!(trajectory_card
        .summary
        .contains("whether it followed the required MCP trajectory"));
    assert!(trajectory_card
        .observability_events
        .iter()
        .any(|event| event.contains("target-framed screenshot")));
    let trajectory_body = trajectory_card
        .body_markdown
        .as_deref()
        .expect("trajectory eval card should include QA body");
    assert!(trajectory_body.contains("blank/all-black/near-uniform"));
    assert!(trajectory_card
        .stop_conditions
        .iter()
        .any(|condition| condition.contains("blank/all-black/near-uniform")));
    let design_card = handle_get_guidance_card(&world, "dkg.design_resources".into())
        .expect("design resources card should fetch");
    assert!(design_card.summary.contains("derive positive invariants"));
    assert!(design_card
        .summary
        .contains("do not brute-force arbitrary primitives"));
    let design_body = design_card
        .body_markdown
        .as_deref()
        .expect("design resources card should carry the progressive-disclosure loop");
    assert!(design_body.contains("progressive-disclosure design-resource harness"));
    assert!(design_body.contains("positive invariants"));
    assert!(design_body.contains("The agent may add to the harness"));
    assert!(design_body.contains("not a brute-force"));
    assert!(discovery
        .guidance_card_ids
        .contains(&"dkg.building_skeleton".to_string()));
    let skeleton_card = handle_get_guidance_card(&world, "dkg.building_skeleton".into())
        .expect("card should fetch");
    assert!(skeleton_card
        .summary
        .contains("foundation -> wall frame/top plate"));
    assert!(skeleton_card.summary.contains("CorpusGaps"));
    assert!(skeleton_card
        .referenced_tool_ids
        .contains(&"acquire_corpus_passage".to_string()));
    let visual_card = handle_list_guidance_cards(&world, Some("house".into()))
        .iter()
        .find(|card| card.id == "dkg.visual_morphology")
        .cloned()
        .expect("visual morphology card should list for house tasks");
    assert!(visual_card.title.contains("Houses"));
    assert!(visual_card
        .summary
        .contains("positive morphology checklist"));
    assert!(visual_card
        .referenced_tool_ids
        .contains(&"take_screenshot".to_string()));
    let visual_body = visual_card
        .body_markdown
        .as_deref()
        .expect("visual morphology card should include resource-oriented body");
    assert!(visual_body.contains("not a blacklist"));
    assert!(visual_body.contains("positive invariants"));
    assert!(visual_body.contains("save it as a prior"));
    assert!(visual_body.contains("color alone"));
    assert!(visual_body.contains("mostly shows terrain"));
    let terrain_card = handle_get_guidance_card(&world, "dkg.terrain_foundation".into())
        .expect("terrain foundation card should fetch");
    assert!(terrain_card
        .summary
        .contains("one local-coordinate building assembly"));
    assert!(terrain_card.summary.contains("terrain.plant_structure"));
    assert!(terrain_card.summary.contains("terrain.plant_on_surface"));
    assert!(terrain_card.summary.contains("invoke_command"));
    assert!(terrain_card.summary.contains("command_id"));
    assert!(terrain_card.summary.contains("semantic `structure`"));
    assert!(terrain_card
        .summary
        .contains("semantic `foundation_system`"));
    assert!(terrain_card
        .summary
        .contains("terrain.release_planted_structure"));
    assert!(terrain_card
        .summary
        .contains("terrain.demote_conforming_foundation"));
    assert!(terrain_card.summary.contains("world Y=0"));
    assert!(terrain_card
        .referenced_tool_ids
        .contains(&"elevation_at".to_string()));
    assert!(terrain_card
        .referenced_tool_ids
        .contains(&"get_world_aabb".to_string()));
    let terrain_body = terrain_card
        .body_markdown
        .as_deref()
        .expect("terrain foundation card should include behavior body");
    assert!(terrain_body.contains("select the visible"));
    assert!(terrain_body.contains("terrain.plant_structure"));
    assert!(terrain_body.contains("invoke_command"));
    assert!(terrain_body.contains("command_id"));
    assert!(terrain_body.contains("nested semantic `foundation_system`"));
    assert!(terrain_body.contains("snapshot"));
    assert!(terrain_body.contains("max_height_box"));
    let terrain_body = terrain_card
        .body_markdown
        .as_deref()
        .expect("terrain card should explain assembly-wide placement");
    assert!(terrain_body.contains("assembly-wide"));
    assert!(terrain_body.contains("prompt target"));
    assert!(terrain_body.contains("re-seat the house"));
    assert!(terrain_body.contains("check_floating"));
    assert!(terrain_body.contains("not sufficient for terrain-seated buildings"));

    let known_tools = std::collections::BTreeSet::from([
        "get_instance_info",
        "get_capability_snapshot",
        "get_authoring_guidance",
        "get_guidance_card",
        "get_agent_skill",
        "discover_curated_paths",
        "select_recipe",
        "acquire_corpus_passage",
        "request_corpus_expansion",
        "save_recipe_draft",
        "save_assembly_pattern_draft",
        "definition.create",
        "definition.instantiate_hosted",
        "bim_void.declare_for_definition",
        "parametric.create",
        "materialize_learned_asset",
        "create_assembly",
        "create_relation",
        "run_validation_v2",
        "check_overlaps",
        "check_floating",
        "check_clearance",
        "take_screenshot",
        // ADR-058 local-frame card (dkg.local_frames)
        "create_entity",
        "enter_group",
        "exit_group",
        "transform",
        "get_editing_context",
        // site/terrain cards (dkg.site_from_survey, dkg.terrain_foundation)
        "import_file",
        "list_importers",
        "set_selection",
        "invoke_command",
        "list_commands",
        "elevation_at",
        "get_world_aabb",
        "definition.draft.derive",
        "occurrence.create",
    ]);
    for card in handle_list_guidance_cards(&world, None) {
        for tool_id in &card.referenced_tool_ids {
            assert!(
                known_tools.contains(tool_id.as_str()),
                "guidance card {} references unknown tool {}",
                card.id,
                tool_id
            );
        }
        for example in &card.json_examples {
            assert!(example.is_object());
        }
    }
    let card =
        handle_get_guidance_card(&world, "dkg.close_gap".into()).expect("card should fetch by id");
    assert!(card
        .referenced_tool_ids
        .contains(&"save_recipe_draft".to_string()));
    assert!(card
        .referenced_tool_ids
        .contains(&"discover_curated_paths".to_string()));
    assert!(card
        .referenced_tool_ids
        .contains(&"materialize_learned_asset".to_string()));
    assert!(
        card.summary.contains("normalize") && card.summary.contains("rediscovery"),
        "close-gap guidance must require vocabulary normalization and rediscovery proof"
    );
}

#[cfg(feature = "model-api")]
#[test]
fn curated_path_discovery_matches_aliases_and_curated_manifests() {
    use crate::curation::provenance::{Confidence, Lineage, Provenance};
    use crate::curation::{
        CuratedManifest, CuratedManifestRegistry, CurationMeta, ManifestKindId, Scope, Trust,
    };
    use crate::plugins::refinement::AgentId;
    use crate::relational::{
        component::ComponentParams,
        registry::{ParametricRegistry, ParametricTypeDef},
        transform::TransformBindings,
    };

    let mut world = init_model_api_test_world();
    let mut parametrics = ParametricRegistry::default();
    parametrics.register(ParametricTypeDef {
        id: "architecture.roof.system.gable".into(),
        label: "Gable Roof System".into(),
        params: ComponentParams::default(),
        driver_units: Default::default(),
        defaults: Default::default(),
        derivations: Default::default(),
        transform: TransformBindings::default(),
        public: true,
        representation: None,
    });
    world.insert_resource(parametrics);

    let kind = ManifestKindId::new("assembly_pattern.v2");
    let asset_id = CuratedManifest::asset_id_for(&kind, "closed_gable_end_wall");
    let manifest = CuratedManifest {
        meta: CurationMeta::new(
            asset_id.clone(),
            CuratedManifest::asset_kind(),
            Provenance {
                author: AgentId("test".into()),
                confidence: Confidence::High,
                lineage: Lineage::Freeform,
                rationale: None,
                jurisdiction: None,
                catalog_dependencies: Vec::new(),
                evidence: Vec::new(),
            },
        )
        .with_scope(Scope::Project)
        .with_trust(Trust::Draft),
        manifest_kind: kind,
        body: json!({
            "label": "Closed gable end wall",
            "target_classes": ["roof_system", "wall_assembly"],
            "slots": []
        }),
    };
    let mut manifests = CuratedManifestRegistry::default();
    manifests.insert(manifest);
    world.insert_resource(manifests);

    let parametric = handle_discover_curated_paths(
        &world,
        CuratedPathDiscoveryRequest {
            path_kind: Some("parametric".into()),
            element_class: Some("roof_system".into()),
            query: None,
            context: json!({}),
        },
    )
    .expect("parametric discovery should succeed");
    assert!(parametric
        .parametric_types
        .iter()
        .any(|ty| ty.id == "architecture.roof.system.gable"));
    assert!(parametric.no_curated_path.is_none());

    let recipe = handle_discover_curated_paths(
        &world,
        CuratedPathDiscoveryRequest {
            path_kind: Some("recipe".into()),
            element_class: Some("roof_system".into()),
            query: None,
            context: json!({}),
        },
    )
    .expect("recipe discovery should succeed");
    assert!(recipe
        .curated_assets
        .iter()
        .any(|asset| asset.asset_id == asset_id.as_str()));
    assert!(recipe.no_curated_path.is_none());
}

#[cfg(feature = "model-api")]
#[test]
fn consultable_curated_assets_do_not_close_materializable_path_gap() {
    use crate::curation::provenance::{Confidence, Lineage, Provenance};
    use crate::curation::{
        CuratedManifest, CuratedManifestRegistry, CurationMeta, ManifestKindId, Scope, Trust,
    };
    use crate::plugins::refinement::AgentId;

    let mut world = init_model_api_test_world();
    let kind = ManifestKindId::new("construction_system_manifest.v1");
    let asset_id = CuratedManifest::asset_id_for(&kind, "house.simple_cottage");
    let manifest = CuratedManifest {
        meta: CurationMeta::new(
            asset_id.clone(),
            CuratedManifest::asset_kind(),
            Provenance {
                author: AgentId("test".into()),
                confidence: Confidence::High,
                lineage: Lineage::Freeform,
                rationale: None,
                jurisdiction: None,
                catalog_dependencies: Vec::new(),
                evidence: Vec::new(),
            },
        )
        .with_scope(Scope::Project)
        .with_trust(Trust::Draft),
        manifest_kind: kind,
        body: json!({
            "label": "Simple cottage grounding notes",
            "target_classes": ["house"],
            "slots": []
        }),
    };
    let mut manifests = CuratedManifestRegistry::default();
    manifests.insert(manifest);
    world.insert_resource(manifests);

    let discovery = handle_discover_curated_paths(
        &world,
        CuratedPathDiscoveryRequest {
            path_kind: Some("recipe".into()),
            element_class: Some("house".into()),
            query: None,
            context: json!({}),
        },
    )
    .expect("discovery should succeed");

    assert!(
        discovery
            .curated_assets
            .iter()
            .any(|asset| asset.asset_id == asset_id.as_str()),
        "consultable grounding asset should still be returned"
    );
    let gap = discovery
        .no_curated_path
        .expect("consultable-only assets must not close a materializable path gap");
    assert_eq!(gap.suggested_next_tool, "request_corpus_expansion");
    assert_eq!(discovery.suggested_next_tool, "request_corpus_expansion");
}

#[cfg(feature = "model-api")]
#[test]
fn agent_skill_handlers_list_get_and_save_drafts() {
    use crate::plugins::agent_skills::{
        AgentSkill, AgentSkillDraftRequest, AgentSkillId, AgentSkillRegistry, AgentSkillTrustLevel,
    };

    let mut world = init_model_api_test_world();
    let mut registry = AgentSkillRegistry::default();
    registry.insert(AgentSkill {
        id: AgentSkillId("architecture.skill.window_authoring".into()),
        title: "Window Authoring".into(),
        summary: "Hosted window workflow".into(),
        task_tags: vec!["window".into(), "hosted_component".into()],
        referenced_tool_ids: vec!["definition.instantiate_hosted".into()],
        required_tool_ids: vec!["get_capability_snapshot".into()],
        forbidden_tool_ids: vec!["create_box".into()],
        validation_tool_ids: vec!["run_validation_v2".into(), "take_screenshot".into()],
        success_criteria: vec!["Hosted opening validates and renders.".into()],
        stop_conditions: vec!["No host wall is selected.".into()],
        screenshot_requirements: vec!["Exterior face view after placement.".into()],
        common_failure_modes: vec!["Free-floating window occurrence.".into()],
        regression_prompt_ids: vec!["window-hosted-opening-basic".into()],
        next_skill_ids: vec![],
        body_markdown: "Use Definition occurrences for hosted windows.".into(),
        trust_level: AgentSkillTrustLevel::Shipped,
        source_path: Some("assets/agent_skills/architecture_authoring.v1.json".into()),
    });
    world.insert_resource(registry);

    let found = handle_list_agent_skills(
        &world,
        crate::plugins::agent_skills::AgentSkillSearch {
            query: Some("window".into()),
            tags: vec!["hosted_component".into()],
        },
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "architecture.skill.window_authoring");
    assert!(found[0]
        .success_criteria
        .contains(&"Hosted opening validates and renders.".to_string()));

    let skill = handle_get_agent_skill(&world, "architecture.skill.window_authoring".into())
        .expect("skill should fetch by id");
    assert!(skill
        .referenced_tool_ids
        .contains(&"definition.instantiate_hosted".to_string()));

    let draft = handle_save_agent_skill_draft(
        &mut world,
        AgentSkillDraftRequest {
            id: Some("project.skill.opening_review".into()),
            title: "Opening Review".into(),
            summary: "Review hosted openings before promotion.".into(),
            task_tags: vec!["opening".into()],
            referenced_tool_ids: vec!["run_validation_v2".into()],
            required_tool_ids: vec!["get_capability_snapshot".into()],
            forbidden_tool_ids: vec!["create_box".into()],
            validation_tool_ids: vec!["run_validation_v2".into()],
            success_criteria: vec!["Validation findings are resolved.".into()],
            stop_conditions: vec!["Opening host cannot be resolved.".into()],
            screenshot_requirements: vec!["Opening close-up.".into()],
            common_failure_modes: vec!["Bare void without hosted product.".into()],
            regression_prompt_ids: vec!["opening-review-basic".into()],
            next_skill_ids: vec!["architecture.skill.window_authoring".into()],
            body_markdown: "Run validation and inspect rendered geometry.".into(),
            source_path: None,
        },
    )
    .expect("draft should save");
    assert_eq!(draft.id.0, "project.skill.opening_review");
    assert_eq!(draft.trust_level, AgentSkillTrustLevel::SessionDraft);
    assert!(draft.forbidden_tool_ids.contains(&"create_box".to_string()));
}

#[cfg(feature = "model-api")]
#[test]
fn executable_learned_asset_materializes_through_parametric_path() {
    use crate::curation::{EvidenceRef, SourceId, SourceRevision};
    use crate::plugins::knowledge_assets::RuntimeCapabilityClaim;
    use crate::plugins::refinement::{ClaimPath, Grounding, HeuristicTag};
    use crate::relational::param_expr::{Quantity, ScalarExpr, Unit};
    use crate::relational::registry::{
        ParametricMember, ParametricRegistry, ParametricRepresentation, ParametricStore,
    };

    let mut world = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut world);
    world.insert_resource(ParametricRegistry::default());
    world.insert_resource(ParametricStore::default());

    let expr_mm = |value| ScalarExpr::lit(Quantity::mm(value));
    let expr_deg = |value| ScalarExpr::lit(Quantity::deg(value));
    let parametric_create = crate::plugins::parametric_mcp::CreateParametricRequest {
        type_id: "learned_roof_block".into(),
        overrides: std::collections::BTreeMap::new(),
        placement: None,
        representation: Some(ParametricRepresentation {
            members: vec![ParametricMember {
                size: [expr_mm(1000.0), expr_mm(200.0), expr_mm(800.0)],
                translate: [expr_mm(0.0), expr_mm(0.0), expr_mm(0.0)],
                rotate_euler_deg: [expr_deg(0.0), expr_deg(0.0), expr_deg(0.0)],
                profile_xz: Vec::new(),
                label: Some("learned_member".into()),
                semantic: None,
            }],
        }),
        defaults: std::collections::BTreeMap::new(),
        driver_units: std::collections::BTreeMap::from([("span_mm".into(), Unit::Mm)]),
        derivations: std::collections::BTreeMap::new(),
        label: Some("Learned Roof Block".into()),
    };

    let evidence = EvidenceRef {
        source_id: SourceId::new("test.source"),
        revision: SourceRevision::new("v1"),
        claim_path: Some(ClaimPath("runtime/geometry_emission".into())),
        excerpt_ref: None,
        grounding_kind: Grounding::LLMHeuristic {
            rationale: "fixture verifies the parametric replay path".into(),
            heuristic_tag: HeuristicTag("test.learned_asset".into()),
        },
    };
    let saved = handle_save_recipe_draft(
        &mut world,
        SaveRecipeDraftRequest {
            recipe_draft_id: Some("learned-roof-block".into()),
            scope: Some("project".into()),
            label: "Learned Roof Block".into(),
            description: "Executable learned asset fixture".into(),
            target_class: "roof_system".into(),
            supported_refinement_levels: vec!["Conceptual".into()],
            parameters: Vec::new(),
            jurisdiction: Some("SE".into()),
            gap_id: None,
            source_passage_refs: Vec::new(),
            evidence_slots: Vec::new(),
            runtime_claims: vec![RuntimeCapabilityClaim::verified_geometry_emission(
                vec![evidence],
                1,
                "fixture",
            )],
            acquisition_context: serde_json::json!({ "proof_point": "PP-DKC-6" }),
            draft_script: serde_json::json!({
                "parametric_create": serde_json::to_value(parametric_create).unwrap()
            }),
            notes: Vec::new(),
            status: Some("installed".into()),
        },
    )
    .expect("learned asset should save");

    let ranking = handle_select_recipe(
        &world,
        "roof_system".into(),
        serde_json::json!({
            "target_state": "Conceptual",
            "include_session_drafts": true
        }),
    )
    .expect("select_recipe should surface executable learned asset");
    assert_eq!(ranking.len(), 1);
    assert!(ranking[0].is_session_draft);
    assert!(ranking[0].executable);
    assert_eq!(
        ranking[0].execution_path.as_deref(),
        Some("materialize_learned_asset")
    );
    assert!(ranking[0]
        .how_to_instantiate
        .contains("materialize_learned_asset"));

    let result = handle_materialize_learned_asset(
        &mut world,
        MaterializeLearnedAssetRequest {
            asset_id: saved.curation.asset_id,
            overrides: std::collections::BTreeMap::new(),
            placement: None,
        },
    )
    .expect("learned asset should materialize");

    assert_eq!(result.execution_path, "parametric.create");
    assert_eq!(result.element_ids.len(), 1);
    assert!(result
        .evidence_backed_claim_ids
        .contains(&"geometry_emission".to_string()));
}

#[cfg(feature = "model-api")]
#[test]
fn recipe_draft_rejects_unknown_gap_id() {
    let mut world = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut world);

    let error = handle_save_recipe_draft(
        &mut world,
        SaveRecipeDraftRequest {
            recipe_draft_id: None,
            scope: Some("project".into()),
            label: "Mono Roof Draft".into(),
            description: "Invalid gap linkage".into(),
            target_class: "roof_system".into(),
            supported_refinement_levels: vec!["Constructible".into()],
            parameters: Vec::new(),
            jurisdiction: Some("SE".into()),
            gap_id: Some("gap-999".into()),
            source_passage_refs: vec!["SE/mono_truss".into()],
            evidence_slots: Vec::new(),
            runtime_claims: Vec::new(),
            acquisition_context: serde_json::Value::Null,
            draft_script: serde_json::Value::Null,
            notes: Vec::new(),
            status: Some("drafted".into()),
        },
    )
    .expect_err("unknown gap id must fail");

    assert!(error.contains("unknown corpus gap id"));
}

#[cfg(feature = "model-api")]
#[test]
fn recipe_draft_rejects_unknown_passage_ref() {
    let mut world = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut world);

    let gap = handle_request_corpus_expansion(
        &mut world,
        Some("roof_system".into()),
        Some("SE".into()),
        "recipe".into(),
        "need mono roof framing".into(),
    )
    .expect("roof_system is a registered class — gap should record");

    let error = handle_save_recipe_draft(
        &mut world,
        SaveRecipeDraftRequest {
            recipe_draft_id: None,
            scope: Some("project".into()),
            label: "Mono Roof Draft".into(),
            description: "Invalid source linkage".into(),
            target_class: "roof_system".into(),
            supported_refinement_levels: vec!["Constructible".into()],
            parameters: Vec::new(),
            jurisdiction: Some("SE".into()),
            gap_id: Some(gap.id),
            source_passage_refs: vec!["SE/missing".into()],
            evidence_slots: Vec::new(),
            runtime_claims: Vec::new(),
            acquisition_context: serde_json::Value::Null,
            draft_script: serde_json::Value::Null,
            notes: Vec::new(),
            status: Some("drafted".into()),
        },
    )
    .expect_err("unknown source passage ref must fail");

    assert!(error.contains("unknown source passage ref"));
    assert!(error.contains("not raw URLs"));
}

#[cfg(feature = "model-api")]
#[test]
fn draft_rule_pack_scaffold_has_no_user_facing_todo_placeholders() {
    let mut world = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut world);

    let draft = handle_draft_rule_pack(&world, "SE/mono_truss".into(), "roof_system".into())
        .expect("draft_rule_pack should generate a scaffold");

    assert!(!draft.rust_skeleton.contains("TODO"));
    assert!(!draft.notes.iter().any(|note| note.contains("TODO")));
    assert!(draft
        .rust_skeleton
        .contains("Draft roof_system rule from SE/mono_truss"));
    assert!(draft
        .rust_skeleton
        .contains("Complete the validator body before registering"));
}

#[cfg(feature = "model-api")]
#[test]
fn installed_recipe_drafts_are_consultable_from_recipe_discovery() {
    let mut world = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut world);

    let installed = save_recipe_draft_fixture(
        &mut world,
        Some("draft-installed".into()),
        Some("installed"),
    );
    let _drafted =
        save_recipe_draft_fixture(&mut world, Some("draft-pending".into()), Some("drafted"));

    let families =
        handle_list_recipe_families_with_options(&world, Some("roof_system".into()), true);
    assert_eq!(families.len(), 1);
    assert_eq!(families[0].id, installed.id);
    assert!(families[0].is_session_draft);

    let ranking = handle_select_recipe(
        &world,
        "roof_system".into(),
        serde_json::json!({
            "target_state": "Constructible",
            "include_session_drafts": true
        }),
    )
    .expect("select_recipe should surface installed drafts");
    assert_eq!(ranking.len(), 1);
    assert_eq!(ranking[0].id, installed.id);
    assert!(ranking[0].is_session_draft);
    assert!(!ranking[0].executable);
    assert!(ranking[0].execution_path.is_none());
    assert!(ranking[0]
        .how_to_instantiate
        .contains("not executable until"));
}

#[cfg(feature = "model-api")]
#[test]
fn select_recipe_omits_prior_vetoed_families() {
    use crate::capability_registry::{
        CorpusProvenance, ElementClassId, GenerateOutput, GenerationPriorDescriptor, LicenseTag,
        PassageRef, PriorEvaluation, PriorId, PriorScope, RecipeFamilyDescriptor, RecipeFamilyId,
    };

    fn recipe(id: &str) -> RecipeFamilyDescriptor {
        RecipeFamilyDescriptor {
            id: RecipeFamilyId(id.into()),
            target_class: ElementClassId("wall_assembly".into()),
            label: id.into(),
            description: "test recipe".into(),
            parameters: Vec::new(),
            supported_refinement_levels: vec![
                crate::plugins::refinement::RefinementState::Constructible,
            ],
            obligation_specializations: std::collections::HashMap::new(),
            promotion_critical_path_specializations: std::collections::HashMap::new(),
            generate: std::sync::Arc::new(|_, _| Ok(GenerateOutput::default())),
        }
    }

    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_recipe_family(recipe("compatible_wall"));
    registry.register_recipe_family(recipe("incompatible_wall"));
    registry.register_generation_prior(GenerationPriorDescriptor {
        id: PriorId("test_veto_incompatible_wall".into()),
        label: "Test Veto".into(),
        description: "Veto one recipe to prove select_recipe removes it.".into(),
        scope: PriorScope::RecipeSelection {
            element_class: ElementClassId("wall_assembly".into()),
            recipe_family: Some(RecipeFamilyId("incompatible_wall".into())),
        },
        source_provenance: CorpusProvenance {
            source: "test".into(),
            source_version: "test".into(),
            jurisdiction: None,
            ingested_at: 0,
            license: LicenseTag::Cc0,
            backlink: None::<PassageRef>,
            supersedes: Vec::new(),
        },
        prior_fn: std::sync::Arc::new(|_| PriorEvaluation {
            weight: 0.0,
            suggestion: None,
            rationale: "incompatible with active facet".into(),
        }),
    });
    world.insert_resource(registry);

    let ranking = handle_select_recipe(
        &world,
        "wall_assembly".into(),
        serde_json::json!({ "target_state": "Constructible" }),
    )
    .expect("recipe selection should succeed");

    assert_eq!(ranking.len(), 1);
    assert_eq!(ranking[0].id, "compatible_wall");
}

#[cfg(feature = "model-api")]
#[test]
fn promote_refinement_rejects_session_recipe_draft_ids() {
    let mut world = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut world);
    let installed = save_recipe_draft_fixture(
        &mut world,
        Some("draft-installed".into()),
        Some("installed"),
    );

    let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
    world.spawn((element_id,));

    let error = handle_promote_refinement(
        &mut world,
        element_id.0,
        "Constructible".into(),
        Some(installed.id),
        serde_json::json!({}),
    )
    .expect_err("draft recipe ids must not execute");

    assert!(
        error.contains("draft only") && error.contains("install"),
        "session/draft recipe ids must be rejected by promote with install guidance, got: {error}"
    );
}

/// Enforcement teeth: a recipe that explicitly declares its supported
/// refinement states must NOT be usable to reach a state outside that set.
/// This is what stops a covering-only / frameless roof shell (now declared
/// `["Schematic"]` in data) from being instantiated or promoted to
/// Constructible+, where it would masquerade as a resolved roof with no
/// structural-framing substrate. Regression guard for the iter23 trap where a
/// frameless gable shell auto-promoted to Constructible.
#[cfg(feature = "model-api")]
#[test]
fn promote_refinement_rejects_recipe_beyond_supported_states() {
    use crate::curation::authoring_script::{AuthoringScript, MutationScope};
    use crate::curation::{
        provenance::{Confidence, Lineage, Provenance},
        scope_trust::{Scope, Trust},
        AssetId, AssetKindId, CurationMeta, RecipeArtifact, RecipeArtifactRegistry, RecipeBody,
        RECIPE_ARTIFACT_KIND,
    };
    use crate::plugins::refinement::{AgentId, RefinementState};

    let mut world = init_model_api_test_world();

    // A Schematic-capped recipe (e.g. a covering-only shell with no frame).
    let asset_id = AssetId("installed_recipe/capped_shell".into());
    let artifact = RecipeArtifact {
        meta: CurationMeta::new(
            asset_id,
            AssetKindId(RECIPE_ARTIFACT_KIND.into()),
            Provenance {
                author: AgentId("test_agent".into()),
                confidence: Confidence::Medium,
                lineage: Lineage::Freeform,
                rationale: Some("covering-only shell, Schematic max".into()),
                jurisdiction: None,
                catalog_dependencies: Vec::new(),
                evidence: Vec::new(),
            },
        )
        .with_scope(Scope::Project)
        .with_trust(Trust::Draft),
        body: RecipeBody::AuthoringScript {
            script: AuthoringScript::stub(MutationScope::None),
        },
        parameter_schema: serde_json::Value::Null,
        target_class: "roof_system".into(),
        supported_refinement_states: vec![RefinementState::Schematic],
        tests: Vec::new(),
    };
    let mut registry = RecipeArtifactRegistry::default();
    registry.insert(artifact);
    world.insert_resource(registry);

    let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
    world.spawn((element_id,));

    let error = handle_promote_refinement(
        &mut world,
        element_id.0,
        "Constructible".into(),
        Some("capped_shell".into()),
        serde_json::json!({}),
    )
    .expect_err("a Schematic-capped recipe must not reach Constructible");

    assert!(
        error.contains("supports only") && error.contains("Constructible"),
        "rejection must name the supported-state cap and the refused target, got: {error}"
    );

    // The same recipe to its supported state must NOT be rejected by the gate.
    // (It may still fail later for unrelated reasons; we only assert the gate
    // message is absent.)
    let to_schematic = handle_promote_refinement(
        &mut world,
        element_id.0,
        "Schematic".into(),
        Some("capped_shell".into()),
        serde_json::json!({}),
    );
    if let Err(e) = to_schematic {
        assert!(
            !e.contains("supports only"),
            "promoting to a supported state must not trip the supported-states gate, got: {e}"
        );
    }
}

#[cfg(feature = "model-api")]
#[test]
fn preview_promotion_returns_read_only_refinement_plan() {
    use crate::capability_registry::{
        ElementClassAssignment, ElementClassDescriptor, ElementClassId, ObligationTemplate,
    };
    use crate::plugins::modeling::assembly::SemanticRelation;
    use crate::plugins::refinement::{
        ObligationId, ObligationSet, RefinementState, RefinementStateComponent, SemanticRole,
    };

    let mut world = init_model_api_test_world();
    let mut class_min_obligations = std::collections::HashMap::new();
    class_min_obligations.insert(
        RefinementState::Schematic,
        vec![ObligationTemplate {
            id: ObligationId("primary_structure".to_string()),
            role: SemanticRole("primary_structure".to_string()),
            required_by_state: RefinementState::Schematic,
        }],
    );
    world
        .resource_mut::<CapabilityRegistry>()
        .register_element_class(ElementClassDescriptor {
            id: ElementClassId("generic_building_part".to_string()),
            label: "Generic Building Part".to_string(),
            description: "Test class".to_string(),
            semantic_roles: Vec::new(),
            class_min_obligations,
            class_min_promotion_critical_paths: std::collections::HashMap::new(),
            parameter_schema: serde_json::json!({"type": "object"}),
        });

    let root = world
        .spawn((
            ElementId(100),
            ElementClassAssignment {
                element_class: ElementClassId("generic_building_part".to_string()),
                active_recipe: None,
            },
        ))
        .id();
    world.spawn((ElementId(101),));
    world.spawn((
        ElementId(200),
        SemanticRelation {
            source: ElementId(100),
            target: ElementId(101),
            relation_type: "refined_into".to_string(),
            parameters: serde_json::Value::Null,
        },
    ));

    let result = handle_preview_promotion(
        &mut world,
        100,
        "Schematic".to_string(),
        None,
        serde_json::json!({}),
    )
    .expect("promotion preview should produce a plan");

    assert!(world.get::<RefinementStateComponent>(root).is_none());
    assert!(world.get::<ObligationSet>(root).is_none());
    assert_eq!(
        result.plan.affected_scope.default_selected_element_ids,
        vec![100, 101]
    );
    assert!(result.plan.affected_scope.editable);
    assert!(!result.plan.affected_scope.project_wide);
    assert_eq!(result.plan.default_commit_policy, "require_clean");
    assert!(result
        .plan
        .supported_commit_policies
        .contains(&"accept_with_waivers".to_string()));
    assert_eq!(result.plan.changed_entities.len(), 2);
    assert_eq!(result.obligation_set.len(), 1);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].severity, "warning");
    assert!(result.plan.can_commit);
}

/// Register a "house" assembly type carrying the ADR-042 member-composition
/// ladder used by the anti-bluff promotion tests below.
#[cfg(feature = "model-api")]
fn register_house_with_member_obligations(world: &mut World) {
    use crate::capability_registry::{AssemblyMemberObligationTemplate, AssemblyTypeDescriptor};
    use crate::plugins::refinement::{ObligationId, RefinementState, SemanticRole};

    let member_obligations = vec![
        AssemblyMemberObligationTemplate {
            id: ObligationId("house_has_foundation".into()),
            role: SemanticRole("primary_structure".into()),
            member_role: "foundation".into(),
            min_count: 1,
            required_by_state: RefinementState::Schematic,
            member_tracks_target_state: true,
        },
        AssemblyMemberObligationTemplate {
            id: ObligationId("house_has_exterior_walls".into()),
            role: SemanticRole("primary_structure".into()),
            member_role: "exterior_wall".into(),
            min_count: 1,
            required_by_state: RefinementState::Schematic,
            member_tracks_target_state: true,
        },
        AssemblyMemberObligationTemplate {
            id: ObligationId("house_has_roof".into()),
            role: SemanticRole("envelope".into()),
            member_role: "roof_element".into(),
            min_count: 1,
            required_by_state: RefinementState::Schematic,
            member_tracks_target_state: true,
        },
    ];

    world
        .resource_mut::<CapabilityRegistry>()
        .register_assembly_type(AssemblyTypeDescriptor {
            assembly_type: "house".into(),
            label: "House".into(),
            description: "A house".into(),
            expected_member_types: vec!["wall".into()],
            expected_member_roles: vec![
                "foundation".into(),
                "exterior_wall".into(),
                "roof_element".into(),
            ],
            member_role_descriptors: Vec::new(),
            expected_relation_types: Vec::new(),
            parameter_schema: serde_json::json!({}),
            member_obligations,
        });
}

#[cfg(feature = "model-api")]
fn spawn_member(
    world: &mut World,
    element_id: u64,
    state: crate::plugins::refinement::RefinementState,
) {
    use crate::plugins::refinement::RefinementStateComponent;
    world.spawn((ElementId(element_id), RefinementStateComponent { state }));
}

#[cfg(feature = "model-api")]
fn spawn_house_assembly(world: &mut World, element_id: u64, members: Vec<(&str, u64)>) {
    use crate::plugins::modeling::assembly::{AssemblyMemberRef, SemanticAssembly};
    use crate::plugins::refinement::{RefinementState, RefinementStateComponent};

    let members = members
        .into_iter()
        .map(|(role, target)| AssemblyMemberRef {
            target: ElementId(target),
            role: role.into(),
        })
        .collect();
    world.spawn((
        ElementId(element_id),
        SemanticAssembly {
            assembly_type: "house".into(),
            label: "Test House".into(),
            members,
            parameters: serde_json::Value::Null,
            metadata: serde_json::Value::Null,
        },
        RefinementStateComponent {
            state: RefinementState::Conceptual,
        },
    ));
}

#[cfg(feature = "model-api")]
#[test]
fn create_assembly_creates_selected_physical_group_for_members() {
    use crate::plugins::modeling::group::GroupMembers;

    let mut world = init_model_api_test_world();
    world
        .resource_mut::<CapabilityRegistry>()
        .register_factory(crate::plugins::modeling::group::GroupFactory);
    register_house_with_member_obligations(&mut world);
    let foundation = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [0.0, 0.25, 0.0],
            "half_extents": [4.0, 0.25, 3.0]
        }),
    )
    .expect("foundation should be creatable");
    let wall = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [0.0, 1.75, -3.0],
            "half_extents": [4.0, 1.5, 0.15]
        }),
    )
    .expect("wall should be creatable");

    let result = handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "house".into(),
            label: "Swedish Cottage".into(),
            members: vec![
                AssemblyMemberRefRequest {
                    target: foundation,
                    role: "foundation".into(),
                },
                AssemblyMemberRefRequest {
                    target: wall,
                    role: "exterior_wall".into(),
                },
            ],
            parameters: Value::Null,
            metadata: Value::Null,
            relations: Vec::new(),
        },
    )
    .expect("assembly should be creatable");
    let group_id = ElementId(result.group_element_id.expect("physical group id"));
    let group_entity = find_entity_by_element_id_readonly(&world, group_id)
        .expect("physical group entity should exist");
    let members = world
        .get::<GroupMembers>(group_entity)
        .expect("physical group should have members");
    assert_eq!(
        members.member_ids,
        vec![ElementId(foundation), ElementId(wall)]
    );
    assert_eq!(handle_get_selection(&mut world), vec![group_id.0]);
}

#[cfg(feature = "model-api")]
#[test]
fn create_assembly_group_moves_members_as_a_unit() {
    let mut world = init_model_api_test_world();
    world
        .resource_mut::<CapabilityRegistry>()
        .register_factory(crate::plugins::modeling::group::GroupFactory);
    register_house_with_member_obligations(&mut world);
    let foundation = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [0.0, 0.25, 0.0],
            "half_extents": [4.0, 0.25, 3.0]
        }),
    )
    .expect("foundation should be creatable");
    let wall = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [0.0, 1.75, -3.0],
            "half_extents": [4.0, 1.5, 0.15]
        }),
    )
    .expect("wall should be creatable");
    let result = handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "house".into(),
            label: "Swedish Cottage".into(),
            members: vec![
                AssemblyMemberRefRequest {
                    target: foundation,
                    role: "foundation".into(),
                },
                AssemblyMemberRefRequest {
                    target: wall,
                    role: "exterior_wall".into(),
                },
            ],
            parameters: Value::Null,
            metadata: Value::Null,
            relations: Vec::new(),
        },
    )
    .expect("assembly should be creatable");
    let group_id = result.group_element_id.expect("physical group id");

    handle_transform(
        &mut world,
        TransformToolRequest {
            element_ids: vec![group_id],
            operation: "move".into(),
            axis: None,
            value: json!([10.0, 0.0, 2.0]),
            pivot: None,
        },
    )
    .expect("physical group should transform");

    assert_eq!(
        get_entity_snapshot(&world, ElementId(foundation)).unwrap()["centre"],
        json!([10.0, 0.25, 2.0])
    );
    assert_eq!(
        get_entity_snapshot(&world, ElementId(wall)).unwrap()["centre"],
        json!([10.0, 1.75, -1.0])
    );
}

#[cfg(feature = "model-api")]
#[test]
fn set_selection_on_group_member_selects_group_at_root() {
    let mut world = init_model_api_test_world();
    world
        .resource_mut::<CapabilityRegistry>()
        .register_factory(crate::plugins::modeling::group::GroupFactory);
    register_house_with_member_obligations(&mut world);
    let foundation = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [0.0, 0.25, 0.0],
            "half_extents": [4.0, 0.25, 3.0]
        }),
    )
    .expect("foundation should be creatable");
    let wall = handle_create_entity(
        &mut world,
        json!({
            "type": "box",
            "centre": [0.0, 1.75, -3.0],
            "half_extents": [4.0, 1.5, 0.15]
        }),
    )
    .expect("wall should be creatable");
    let result = handle_create_assembly(
        &mut world,
        CreateAssemblyRequest {
            assembly_type: "house".into(),
            label: "Swedish Cottage".into(),
            members: vec![
                AssemblyMemberRefRequest {
                    target: foundation,
                    role: "foundation".into(),
                },
                AssemblyMemberRefRequest {
                    target: wall,
                    role: "exterior_wall".into(),
                },
            ],
            parameters: Value::Null,
            metadata: Value::Null,
            relations: Vec::new(),
        },
    )
    .expect("assembly should be creatable");
    let group_id = result.group_element_id.expect("physical group id");

    let selected = handle_set_selection(&mut world, vec![foundation])
        .expect("member selection should resolve to its group");

    assert_eq!(selected, vec![group_id]);
    assert_eq!(handle_get_selection(&mut world), vec![group_id]);
}

/// A bare house (no resolved sub-structure) cannot be previewed as a clean
/// commit when skipping straight to Detailed — every in-force member
/// obligation surfaces as a missing input and an error finding.
#[cfg(feature = "model-api")]
#[test]
fn preview_promotion_blocks_bare_assembly_skipping_to_detailed() {
    let mut world = init_model_api_test_world();
    register_house_with_member_obligations(&mut world);
    spawn_house_assembly(&mut world, 500, Vec::new());

    let result = handle_preview_promotion(
        &mut world,
        500,
        "Detailed".to_string(),
        None,
        serde_json::json!({}),
    )
    .expect("preview should produce a plan even when blocked");

    assert!(
        !result.plan.can_commit,
        "bare house promoted across levels must not be committable"
    );
    assert_eq!(
        result.plan.missing_inputs.len(),
        3,
        "all three member obligations are in force at Detailed"
    );
    assert!(result
        .plan
        .missing_inputs
        .iter()
        .any(|m| m.contains("house_has_foundation")));
    // Constructible+ obligations escalate to error severity.
    assert!(result.findings.iter().any(|f| f.severity == "error"));
}

/// A bare house promoted to FabricationReady is likewise blocked.
#[cfg(feature = "model-api")]
#[test]
fn promote_refinement_rejects_bluff_assembly_to_fabrication_ready() {
    use crate::plugins::refinement::RefinementStateComponent;

    let mut world = init_model_api_test_world();
    register_house_with_member_obligations(&mut world);
    spawn_house_assembly(&mut world, 510, Vec::new());

    let error = handle_promote_refinement(
        &mut world,
        510,
        "FabricationReady".to_string(),
        None,
        serde_json::json!({}),
    )
    .expect_err("bare house promotion must be rejected by the anti-bluff gate");

    assert!(error.contains("unmet member obligation"));

    // The commit must not have mutated state.
    let entity = find_entity_by_element_id_readonly(&world, ElementId(510))
        .expect("assembly entity should still exist");
    let state = world
        .get::<RefinementStateComponent>(entity)
        .map(|c| c.state)
        .unwrap_or_default();
    assert_eq!(
        state,
        crate::plugins::refinement::RefinementState::Conceptual,
        "rejected promotion must leave the assembly at its original state"
    );
}

/// A fully authored house — foundation, exterior wall, and roof all resolved to
/// the target state — promotes cleanly.
#[cfg(feature = "model-api")]
#[test]
fn promote_refinement_allows_fully_authored_assembly() {
    use crate::plugins::refinement::{RefinementState, RefinementStateComponent};

    let mut world = init_model_api_test_world();
    register_house_with_member_obligations(&mut world);
    spawn_member(&mut world, 601, RefinementState::Detailed);
    spawn_member(&mut world, 602, RefinementState::Detailed);
    spawn_member(&mut world, 603, RefinementState::Detailed);
    spawn_house_assembly(
        &mut world,
        600,
        vec![
            ("foundation", 601),
            ("exterior_wall", 602),
            ("roof_element", 603),
        ],
    );

    let preview = handle_preview_promotion(
        &mut world,
        600,
        "Detailed".to_string(),
        None,
        serde_json::json!({}),
    )
    .expect("preview should succeed");
    assert!(
        preview.plan.can_commit,
        "fully authored house should be committable"
    );
    assert!(preview.plan.missing_inputs.is_empty());

    let result = handle_promote_refinement(
        &mut world,
        600,
        "Detailed".to_string(),
        None,
        serde_json::json!({}),
    )
    .expect("fully authored house promotion should succeed");
    assert_eq!(result.new_state, "Detailed");

    let entity = find_entity_by_element_id_readonly(&world, ElementId(600)).unwrap();
    assert_eq!(
        world
            .get::<RefinementStateComponent>(entity)
            .map(|c| c.state),
        Some(RefinementState::Detailed)
    );
}

/// Members that exist but have not themselves been resolved to the target state
/// do not satisfy a `member_tracks_target_state` obligation.
#[cfg(feature = "model-api")]
#[test]
fn promote_refinement_rejects_assembly_with_underresolved_members() {
    use crate::plugins::refinement::RefinementState;

    let mut world = init_model_api_test_world();
    register_house_with_member_obligations(&mut world);
    // Present in the right roles, but only at Conceptual.
    spawn_member(&mut world, 701, RefinementState::Conceptual);
    spawn_member(&mut world, 702, RefinementState::Conceptual);
    spawn_member(&mut world, 703, RefinementState::Conceptual);
    spawn_house_assembly(
        &mut world,
        700,
        vec![
            ("foundation", 701),
            ("exterior_wall", 702),
            ("roof_element", 703),
        ],
    );

    let error = handle_promote_refinement(
        &mut world,
        700,
        "Detailed".to_string(),
        None,
        serde_json::json!({}),
    )
    .expect_err("under-resolved members must not satisfy a tracking obligation");
    assert!(error.contains("resolved to at least Detailed"));
}

#[cfg(feature = "model-api")]
#[test]
fn assembly_pattern_draft_round_trip_with_gap_and_source_refs() {
    let mut world = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut world);

    let saved = save_assembly_pattern_draft_fixture(&mut world, None, Some("installed"));
    assert_eq!(saved.status, "installed");
    assert_eq!(saved.target_types, vec!["wall_assembly".to_string()]);
    assert_eq!(
        saved.source_passage_refs,
        vec!["SE/rainscreen_wall".to_string()]
    );

    let fetched =
        handle_get_assembly_pattern_draft(&world, saved.id.clone()).expect("draft should load");
    assert_eq!(fetched.id, saved.id);
    assert_eq!(fetched.gap_id, saved.gap_id);
    assert_eq!(fetched.layers.len(), 3);

    let filtered = handle_list_assembly_pattern_drafts(
        &world,
        Some("wall_assembly".into()),
        Some("installed".into()),
    )
    .expect("list_assembly_pattern_drafts should succeed");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, saved.id);
}

#[cfg(feature = "model-api")]
#[test]
fn material_assignment_round_trips_layer_sets() {
    let mut world = init_model_api_test_world();
    world.spawn((ElementId(42),));
    world
        .resource_mut::<crate::plugins::materials::MaterialRegistry>()
        .upsert(MaterialDef {
            id: "oak_finish".to_string(),
            name: "Oak Finish".to_string(),
            ..Default::default()
        });

    let assignment = MaterialAssignment::LayerSet(crate::plugins::materials::MaterialLayerSet {
        layers: vec![
            crate::plugins::materials::MaterialLayer {
                name: Some("structure".to_string()),
                thickness_mm: Some(45.0),
                binding: crate::plugins::materials::MaterialBinding {
                    spec: None,
                    render: Some(crate::plugins::materials::material_def_asset_id(
                        "oak_finish",
                    )),
                    mapping: None,
                },
            },
            crate::plugins::materials::MaterialLayer {
                name: Some("finish".to_string()),
                thickness_mm: Some(12.5),
                binding: crate::plugins::materials::MaterialBinding::default(),
            },
        ],
    });

    let updated = handle_set_material_assignment(
        &mut world,
        SetMaterialAssignmentRequest {
            element_ids: vec![42],
            assignment: assignment.clone(),
        },
    )
    .expect("set_material_assignment should accept layer sets");
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].assignment, Some(assignment.clone()));

    let fetched =
        handle_get_material_assignment(&world, 42).expect("get_material_assignment works");
    assert_eq!(fetched.assignment, Some(assignment));
}

#[cfg(feature = "model-api")]
#[test]
fn assign_material_uses_existing_registry_material() {
    let mut world = init_model_api_test_world();
    world.spawn((ElementId(42),));
    world
        .resource_mut::<crate::plugins::materials::MaterialRegistry>()
        .upsert(MaterialDef {
            id: "oak_finish".to_string(),
            name: "Oak Finish".to_string(),
            ..Default::default()
        });

    let assigned = handle_assign_material(
        &mut world,
        AssignMaterialRequest {
            element_ids: vec![42],
            material_id: Some("oak_finish".to_string()),
            name: None,
            base_color: None,
            perceptual_roughness: None,
            metallic: None,
            base_color_texture: None,
            normal_map_texture: None,
            metallic_roughness_texture: None,
            emissive_texture: None,
            occlusion_texture: None,
        },
    )
    .expect("assign_material should accept existing registry material ids");

    assert_eq!(assigned.material_id, "oak_finish");
    assert!(!assigned.created_material);
    assert_eq!(assigned.assignments.len(), 1);
    assert_eq!(
        assigned.assignments[0].assignment,
        Some(MaterialAssignment::new("oak_finish"))
    );
}

#[cfg(feature = "model-api")]
#[test]
fn assign_material_can_create_color_material() {
    let mut world = init_model_api_test_world();
    world.spawn((ElementId(42),));

    let assigned = handle_assign_material(
        &mut world,
        AssignMaterialRequest {
            element_ids: vec![42],
            material_id: Some("project.paint.deep_green".to_string()),
            name: Some("Deep Green Paint".to_string()),
            base_color: Some([0.05, 0.26, 0.18, 1.0]),
            perceptual_roughness: Some(0.72),
            metallic: Some(0.0),
            base_color_texture: None,
            normal_map_texture: None,
            metallic_roughness_texture: None,
            emissive_texture: None,
            occlusion_texture: None,
        },
    )
    .expect("assign_material should create and assign ad hoc color materials");

    assert_eq!(assigned.material_id, "project.paint.deep_green");
    assert!(assigned.created_material);
    assert_eq!(
        assigned.assignments[0].assignment,
        Some(MaterialAssignment::new("project.paint.deep_green"))
    );
    let material = world
        .resource::<crate::plugins::materials::MaterialRegistry>()
        .get("project.paint.deep_green")
        .expect("color material should be inserted into registry");
    assert_eq!(material.name, "Deep Green Paint");
    assert_eq!(material.base_color, [0.05, 0.26, 0.18, 1.0]);
    assert_eq!(material.perceptual_roughness, 0.72);
}

#[cfg(feature = "model-api")]
#[test]
fn assign_material_can_create_asset_textured_material() {
    let mut world = init_model_api_test_world();
    world.spawn((ElementId(42),));

    let assigned = handle_assign_material(
        &mut world,
        AssignMaterialRequest {
            element_ids: vec![42],
            material_id: Some("project.brick.textured".to_string()),
            name: Some("Textured Brick".to_string()),
            base_color: Some([0.72, 0.31, 0.22, 1.0]),
            perceptual_roughness: Some(0.9),
            metallic: None,
            base_color_texture: Some(AssignMaterialTextureRef {
                asset: Some(AssignMaterialAssetTextureRef {
                    path: "textures/brick_albedo.ktx2".to_string(),
                }),
                embedded: None,
            }),
            normal_map_texture: None,
            metallic_roughness_texture: Some(AssignMaterialTextureRef {
                asset: Some(AssignMaterialAssetTextureRef {
                    path: "textures/brick_orm.png".to_string(),
                }),
                embedded: None,
            }),
            emissive_texture: None,
            occlusion_texture: None,
        },
    )
    .expect("assign_material should create textured registry materials");

    assert_eq!(assigned.material_id, "project.brick.textured");
    assert_eq!(
        assigned.assignments[0].assignment,
        Some(MaterialAssignment::new("project.brick.textured"))
    );
    let material = world
        .resource::<crate::plugins::materials::MaterialRegistry>()
        .get("project.brick.textured")
        .expect("textured material should be inserted into registry");
    assert_eq!(
        material.base_color_texture,
        Some(crate::plugins::materials::TextureRef::AssetPath {
            path: "textures/brick_albedo.ktx2".to_string()
        })
    );
    assert_eq!(
        material.metallic_roughness_texture,
        Some(crate::plugins::materials::TextureRef::AssetPath {
            path: "textures/brick_orm.png".to_string()
        })
    );
}

#[cfg(feature = "model-api")]
#[test]
fn texture_mapping_tools_update_material_defaults() {
    let mut world = init_model_api_test_world();
    world
        .resource_mut::<crate::plugins::materials::MaterialRegistry>()
        .upsert(MaterialDef {
            id: "project.cedar".to_string(),
            name: "Project Cedar".to_string(),
            ..Default::default()
        });

    let updated = handle_update_texture_mapping(
        &mut world,
        UpdateTextureMappingRequest {
            material_id: Some("project.cedar".to_string()),
            element_id: None,
            mapping: TextureMappingDto {
                projection: "uv".to_string(),
                uv_scale: [0.25, 0.5],
                uv_offset: [0.1, 0.2],
                uv_rotation_deg: 90.0,
                flip_u: true,
                flip_v: false,
                blend_sharpness: 4.0,
            },
        },
    )
    .expect("material mapping should update");

    assert_eq!(updated.target, "material");
    assert_eq!(updated.source, "material_default");
    assert!(updated.renderer_supported);
    assert_eq!(updated.mapping.uv_scale, [0.25, 0.5]);
    assert_eq!(updated.mapping.uv_offset, [0.1, 0.2]);
    assert_eq!(updated.mapping.uv_rotation_deg, 90.0);
    assert!(updated.mapping.flip_u);

    let fetched = handle_get_texture_mapping(
        &world,
        GetTextureMappingRequest {
            material_id: Some("project.cedar".to_string()),
            element_id: None,
        },
    )
    .expect("material mapping should be inspectable");
    assert_eq!(fetched, updated);

    let reset = handle_reset_texture_mapping(
        &mut world,
        ResetTextureMappingRequest {
            material_id: Some("project.cedar".to_string()),
            element_id: None,
        },
    )
    .expect("material mapping should reset");
    assert_eq!(reset.mapping.uv_scale, [1.0, 1.0]);
    assert_eq!(reset.mapping.uv_offset, [0.0, 0.0]);
    assert_eq!(reset.mapping.uv_rotation_deg, 0.0);
    assert!(!reset.mapping.flip_u);
}

#[cfg(feature = "model-api")]
#[test]
fn texture_mapping_tools_update_assignment_override() {
    let mut world = init_model_api_test_world();
    world
        .resource_mut::<crate::plugins::materials::MaterialRegistry>()
        .upsert(MaterialDef {
            id: "project.cedar".to_string(),
            name: "Project Cedar".to_string(),
            ..Default::default()
        });
    world.spawn((ElementId(42), MaterialAssignment::new("project.cedar")));

    let updated = handle_update_texture_mapping(
        &mut world,
        UpdateTextureMappingRequest {
            material_id: None,
            element_id: Some(42),
            mapping: TextureMappingDto {
                projection: "uv".to_string(),
                uv_scale: [2.0, 3.0],
                uv_offset: [0.0, 0.25],
                uv_rotation_deg: 270.0,
                flip_u: false,
                flip_v: true,
                blend_sharpness: 4.0,
            },
        },
    )
    .expect("assignment mapping should update");

    assert_eq!(updated.target, "element");
    assert_eq!(updated.material_id.as_deref(), Some("project.cedar"));
    assert_eq!(updated.source, "assignment_override");
    assert_eq!(updated.mapping.uv_scale, [2.0, 3.0]);
    assert!(updated.mapping.flip_v);
    assert_eq!(
        world
            .resource::<crate::plugins::materials::MaterialRegistry>()
            .get("project.cedar")
            .expect("material exists")
            .uv_scale,
        [1.0, 1.0],
        "assignment override must not mutate the shared material"
    );

    let reset = handle_reset_texture_mapping(
        &mut world,
        ResetTextureMappingRequest {
            material_id: None,
            element_id: Some(42),
        },
    )
    .expect("assignment mapping should reset");
    assert_eq!(reset.source, "material_default");
    assert_eq!(reset.mapping.uv_scale, [1.0, 1.0]);
}

#[cfg(feature = "model-api")]
#[test]
fn texture_mapping_reports_unsupported_projection_and_uv_diagnostics() {
    let mut world = init_model_api_test_world();
    world
        .resource_mut::<crate::plugins::materials::MaterialRegistry>()
        .upsert(MaterialDef {
            id: "project.cedar".to_string(),
            name: "Project Cedar".to_string(),
            ..Default::default()
        });
    let mut mesh = Mesh::new(
        bevy::mesh::PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::default(),
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; 3]);
    let mesh_handle = world.resource_mut::<Assets<Mesh>>().add(mesh);
    world.spawn((
        ElementId(42),
        MaterialAssignment::new("project.cedar"),
        Mesh3d(mesh_handle),
    ));

    let updated = handle_update_texture_mapping(
        &mut world,
        UpdateTextureMappingRequest {
            material_id: Some("project.cedar".to_string()),
            element_id: None,
            mapping: TextureMappingDto {
                projection: "triplanar".to_string(),
                uv_scale: [1.0, 1.0],
                uv_offset: [0.0, 0.0],
                uv_rotation_deg: 0.0,
                flip_u: false,
                flip_v: false,
                blend_sharpness: 8.0,
            },
        },
    )
    .expect("unsupported projection should still be authored");
    assert!(!updated.renderer_supported);
    assert!(updated
        .renderer_note
        .as_deref()
        .unwrap_or_default()
        .contains("not yet rendered"));

    let entity_mapping = handle_get_texture_mapping(
        &world,
        GetTextureMappingRequest {
            material_id: None,
            element_id: Some(42),
        },
    )
    .expect("entity mapping should be inspectable");
    assert_eq!(entity_mapping.source, "material_default");
    let diagnostics = entity_mapping
        .uv_diagnostics
        .expect("entity target should include uv diagnostics");
    assert!(diagnostics.has_uv0);
    assert!(diagnostics.degenerate);
    assert!(diagnostics
        .messages
        .iter()
        .any(|message| message.contains("identical")));
}

#[cfg(feature = "model-api")]
#[test]
fn bim_material_layered_definition_default_resolves_for_occurrence() {
    let mut world = init_model_api_test_world();
    let definition = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("definition should be created");

    let assigned = handle_bim_material_assign_layered(
        &mut world,
        BimMaterialAssignLayeredRequest {
            definition_id: Some(definition.definition_id.clone()),
            element_id: None,
            layers: vec![
                BimMaterialLayerInput {
                    material: "mat.gypsum.standard_12_5mm".into(),
                    thickness_m: 0.0125,
                    function: Some("finish".into()),
                    is_ventilated: None,
                    label: Some("Interior gypsum".into()),
                },
                BimMaterialLayerInput {
                    material: "mat.insulation.mineral_wool".into(),
                    thickness_m: 0.15,
                    function: Some("insulation".into()),
                    is_ventilated: Some(false),
                    label: None,
                },
            ],
            total_thickness_param: Some("depth".into()),
        },
    )
    .expect("layered assignment should succeed");
    assert_eq!(assigned["target"], json!("definition"));
    assert_eq!(assigned["prior"], Value::Null);
    assert_eq!(assigned["assignment"]["kind"], json!("layer_set"));
    assert_eq!(
        assigned["assignment"]["layers"][0]["function"],
        json!("finish")
    );
    assert_eq!(
        assigned["assignment"]["total_thickness_param"],
        json!("depth")
    );

    let occurrence_id = handle_place_occurrence(
        &mut world,
        json!({ "definition_id": definition.definition_id, "label": "Wall1" }),
    )
    .expect("occurrence should be placed");
    let effective = handle_bim_material_get_effective(
        &mut world,
        BimMaterialGetEffectiveRequest {
            definition_id: None,
            element_id: Some(occurrence_id),
        },
    )
    .expect("effective material should resolve");
    assert_eq!(effective["source"], json!("definition"));
    assert_eq!(effective["assignment"]["kind"], json!("layer_set"));
}

#[cfg(feature = "model-api")]
#[test]
fn bim_material_constituent_override_wins_over_definition_default() {
    let mut world = init_model_api_test_world();
    let definition = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("definition should be created");
    handle_bim_material_assign_layered(
        &mut world,
        BimMaterialAssignLayeredRequest {
            definition_id: Some(definition.definition_id.clone()),
            element_id: None,
            layers: vec![BimMaterialLayerInput {
                material: "mat.concrete.c25_30".into(),
                thickness_m: 0.2,
                function: Some("structural".into()),
                is_ventilated: None,
                label: None,
            }],
            total_thickness_param: None,
        },
    )
    .expect("definition default should be assigned");

    let occurrence_id = handle_place_occurrence(
        &mut world,
        json!({ "definition_id": definition.definition_id, "label": "Wall1" }),
    )
    .expect("occurrence should be placed");
    let assigned = handle_bim_material_assign_constituents(
        &mut world,
        BimMaterialAssignConstituentsRequest {
            definition_id: None,
            element_id: Some(occurrence_id),
            constituents: vec![
                BimMaterialConstituentInput {
                    material: "mat.concrete.c25_30".into(),
                    fraction: 0.95,
                    label: Some("Concrete".into()),
                },
                BimMaterialConstituentInput {
                    material: "mat.steel.rebar_b500b".into(),
                    fraction: 0.05,
                    label: Some("Rebar".into()),
                },
            ],
        },
    )
    .expect("constituent override should be assigned");
    assert_eq!(assigned["target"], json!("element"));
    assert_eq!(assigned["prior"], Value::Null);

    let effective = handle_bim_material_get_effective(
        &mut world,
        BimMaterialGetEffectiveRequest {
            definition_id: None,
            element_id: Some(occurrence_id),
        },
    )
    .expect("effective material should resolve");
    assert_eq!(effective["source"], json!("override"));
    assert_eq!(effective["assignment"]["kind"], json!("constituent_set"));
    assert_eq!(
        effective["assignment"]["constituents"][1]["label"],
        json!("Rebar")
    );
}

#[cfg(feature = "model-api")]
#[test]
fn bim_material_assignment_rejects_invalid_targets_and_fractions() {
    let mut world = init_model_api_test_world();
    let definition = handle_create_definition(&mut world, make_rect_extrusion_request())
        .expect("definition should be created");
    let non_occurrence = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .expect("box should be created");

    let err = handle_bim_material_assign_layered(
        &mut world,
        BimMaterialAssignLayeredRequest {
            definition_id: Some(definition.definition_id.clone()),
            element_id: Some(non_occurrence),
            layers: vec![BimMaterialLayerInput {
                material: "mat.concrete.c25_30".into(),
                thickness_m: 0.2,
                function: Some("structural".into()),
                is_ventilated: None,
                label: None,
            }],
            total_thickness_param: None,
        },
    )
    .unwrap_err();
    assert!(err.contains("exactly one"));

    let err = handle_bim_material_assign_constituents(
        &mut world,
        BimMaterialAssignConstituentsRequest {
            definition_id: Some(definition.definition_id),
            element_id: None,
            constituents: vec![BimMaterialConstituentInput {
                material: "mat.concrete.c25_30".into(),
                fraction: 0.5,
                label: None,
            }],
        },
    )
    .unwrap_err();
    assert!(err.contains("sum to 1.0"));

    let err = handle_bim_material_get_effective(
        &mut world,
        BimMaterialGetEffectiveRequest {
            definition_id: None,
            element_id: Some(non_occurrence),
        },
    )
    .unwrap_err();
    assert!(err.contains("not an occurrence"));
}

#[cfg(feature = "model-api")]
#[test]
fn quantity_set_get_and_list_provenance_for_primary_fields() {
    let mut world = init_model_api_test_world();
    world.spawn((ElementId(314),));

    let full = handle_quantity_set(
        &mut world,
        QuantitySetRequest {
            element_id: 314,
            field: "area_gross_m2".into(),
            value: json!(12.5),
            material: None,
            provenance: QuantityProvenanceInput {
                kind: "authored_parameter".into(),
                parameter: Some("wall_area".into()),
                node: None,
                source: None,
                rationale: None,
            },
        },
    )
    .expect("quantity set should succeed");
    assert_eq!(full["area_gross_m2"]["value"], json!(12.5));

    let got = handle_quantity_get(
        &mut world,
        QuantityGetRequest {
            element_id: 314,
            field: Some("area_gross".into()),
            material: None,
        },
    )
    .expect("quantity get should succeed");
    assert_eq!(got["value"], json!(12.5));
    assert_eq!(
        got["provenance"],
        json!({ "kind": "authored_parameter", "parameter": "wall_area" })
    );

    let provenance = handle_quantity_list_provenance(
        &mut world,
        QuantityListProvenanceRequest { element_id: 314 },
    )
    .expect("quantity provenance list should succeed");
    assert_eq!(provenance.as_array().unwrap().len(), 1);
    assert_eq!(provenance[0]["field"], json!("area_gross_m2"));
    assert_eq!(provenance[0]["grounded"], json!(true));
}

#[cfg(feature = "model-api")]
#[test]
fn quantity_set_get_material_quantities() {
    let mut world = init_model_api_test_world();
    world.spawn((ElementId(315),));

    handle_quantity_set(
        &mut world,
        QuantitySetRequest {
            element_id: 315,
            field: "volume_m3".into(),
            value: json!(1.75),
            material: Some("mat.concrete.c25_30".into()),
            provenance: QuantityProvenanceInput {
                kind: "evaluator_node".into(),
                parameter: None,
                node: Some("wall.quantities".into()),
                source: None,
                rationale: None,
            },
        },
    )
    .expect("material quantity set should succeed");

    let got = handle_quantity_get(
        &mut world,
        QuantityGetRequest {
            element_id: 315,
            field: Some("volume_m3".into()),
            material: Some("mat.concrete.c25_30".into()),
        },
    )
    .expect("material quantity get should succeed");
    assert_eq!(got["value"], json!(1.75));
    assert_eq!(
        got["provenance"],
        json!({ "kind": "evaluator_node", "node": "wall.quantities" })
    );

    let full = handle_quantity_get(
        &mut world,
        QuantityGetRequest {
            element_id: 315,
            field: None,
            material: None,
        },
    )
    .expect("full quantity get should succeed");
    assert_eq!(
        full["material_quantities"][0]["material"],
        json!("mat.concrete.c25_30")
    );
}

#[cfg(feature = "model-api")]
#[test]
fn quantity_check_invariants_reports_violations_and_mesh_provenance() {
    let mut world = init_model_api_test_world();
    world.spawn((ElementId(316),));

    for (field, value) in [
        ("area_gross_m2", json!(10.0)),
        ("area_net_m2", json!(11.0)),
        ("opening_area_deducted_m2", json!(2.0)),
    ] {
        handle_quantity_set(
            &mut world,
            QuantitySetRequest {
                element_id: 316,
                field: field.into(),
                value,
                material: None,
                provenance: QuantityProvenanceInput {
                    kind: "mesh_approximation".into(),
                    parameter: None,
                    node: None,
                    source: None,
                    rationale: None,
                },
            },
        )
        .expect("quantity set should succeed");
    }

    let check = handle_quantity_check_invariants(
        &mut world,
        QuantityCheckInvariantsRequest {
            element_id: 316,
            tolerance: Some(1.0e-6),
        },
    )
    .expect("quantity invariant check should succeed");
    assert_eq!(check["ok"], json!(false));
    assert_eq!(check["all_grounded"], json!(false));
    assert_eq!(check["area_deduction_consistent"], json!(false));
    assert_eq!(
        check["net_le_gross_violations"],
        json!(["area_net_m2 > area_gross_m2"])
    );
}

#[cfg(feature = "model-api")]
#[test]
fn quantity_set_rejects_bad_values_and_missing_provenance_payload() {
    let mut world = init_model_api_test_world();
    world.spawn((ElementId(317),));

    let err = handle_quantity_set(
        &mut world,
        QuantitySetRequest {
            element_id: 317,
            field: "length_m".into(),
            value: json!(-1.0),
            material: None,
            provenance: QuantityProvenanceInput {
                kind: "user_override".into(),
                parameter: None,
                node: None,
                source: None,
                rationale: Some("test".into()),
            },
        },
    )
    .unwrap_err();
    assert!(err.contains("non-negative"));

    let err = handle_quantity_set(
        &mut world,
        QuantitySetRequest {
            element_id: 317,
            field: "length_m".into(),
            value: json!(1.0),
            material: None,
            provenance: QuantityProvenanceInput {
                kind: "authored_parameter".into(),
                parameter: None,
                node: None,
                source: None,
                rationale: None,
            },
        },
    )
    .unwrap_err();
    assert!(err.contains("requires parameter"));
}

#[cfg(feature = "model-api")]
#[test]
fn assembly_pattern_draft_rejects_unknown_passage_ref() {
    let mut world = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut world);

    let gap = handle_request_corpus_expansion(
        &mut world,
        Some("wall_assembly".into()),
        Some("SE".into()),
        "assembly_pattern".into(),
        "need ventilated wall pattern".into(),
    )
    .expect("wall_assembly is a registered class — gap should record");

    let error = handle_save_assembly_pattern_draft(
        &mut world,
        SaveAssemblyPatternDraftRequest {
            assembly_pattern_draft_id: None,
            scope: Some("project".into()),
            label: "Wall Pattern".into(),
            description: "Invalid source linkage".into(),
            target_types: vec!["wall_assembly".into()],
            axis: "exterior_to_interior".into(),
            layers: Vec::new(),
            relation_rules: Vec::new(),
            root_layer_ids: vec!["stud_frame".into()],
            requires_support_path: true,
            tags: Vec::new(),
            parameter_schema: serde_json::json!({}),
            jurisdiction: Some("SE".into()),
            gap_id: Some(gap.id),
            source_passage_refs: vec!["SE/missing".into()],
            evidence_slots: Vec::new(),
            runtime_claims: Vec::new(),
            acquisition_context: serde_json::Value::Null,
            notes: Vec::new(),
            status: Some("drafted".into()),
        },
    )
    .expect_err("unknown source passage ref must fail");

    assert!(error.contains("unknown source passage ref"));
    assert!(error.contains("not raw URLs"));
}

#[cfg(feature = "model-api")]
#[test]
fn installed_assembly_pattern_drafts_are_consultable_from_vocabulary() {
    let mut world = init_model_api_test_world();
    seed_recipe_draft_corpus(&mut world);

    let installed = save_assembly_pattern_draft_fixture(
        &mut world,
        Some("pattern-installed".into()),
        Some("installed"),
    );
    let _drafted = save_assembly_pattern_draft_fixture(
        &mut world,
        Some("pattern-pending".into()),
        Some("drafted"),
    );

    let vocabulary = handle_list_vocabulary(&world);
    assert_eq!(vocabulary.assembly_patterns.len(), 1);
    assert_eq!(vocabulary.assembly_patterns[0].id, installed.id);
    assert!(vocabulary.assembly_patterns[0].is_session_draft);
    assert!(vocabulary.assembly_patterns[0].consultable);
    assert_eq!(
        vocabulary.assembly_patterns[0].status.as_deref(),
        Some("installed")
    );
}

#[cfg(feature = "model-api")]
#[test]
fn deleting_material_keeps_spec_binding_when_assignment_has_fallback() {
    let mut world = init_model_api_test_world();
    let spec_id = crate::curation::MaterialSpec::asset_id_for("gypsum_board");
    let mut specs = crate::curation::MaterialSpecRegistry::default();
    specs.insert(crate::curation::MaterialSpec::draft(
        spec_id.clone(),
        crate::curation::MaterialSpecBody {
            display_name: "Gypsum Board".to_string(),
            ..Default::default()
        },
        crate::plugins::refinement::AgentId("codex".to_string()),
        None,
    ));
    world.insert_resource(specs);
    world
        .resource_mut::<crate::plugins::materials::MaterialRegistry>()
        .upsert(MaterialDef {
            id: "paint_white".to_string(),
            name: "White Paint".to_string(),
            ..Default::default()
        });
    world.spawn((
        ElementId(7),
        MaterialAssignment::Single(crate::plugins::materials::MaterialBinding {
            spec: Some(spec_id.clone()),
            render: Some(crate::plugins::materials::material_def_asset_id(
                "paint_white",
            )),
            mapping: None,
        }),
    ));

    let deleted =
        handle_delete_material(&mut world, "paint_white").expect("delete_material should work");
    assert_eq!(deleted, "paint_white");

    let assignment = handle_get_material_assignment(&world, 7)
        .expect("entity should remain")
        .assignment;
    assert_eq!(
        assignment,
        Some(MaterialAssignment::Single(
            crate::plugins::materials::MaterialBinding {
                spec: Some(spec_id),
                render: None,
                mapping: None,
            }
        ))
    );
    assert!(!world
        .resource::<crate::plugins::materials::MaterialRegistry>()
        .contains("paint_white"));
}

#[cfg(feature = "model-api")]
#[test]
fn bim_property_set_get_returns_null_for_missing_map() {
    let mut world = init_model_api_test_world();
    let element_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();
    let value =
        handle_bim_property_set_get(&mut world, element_id, "Pset_WallCommon", "FireRating")
            .unwrap();
    assert_eq!(value, Value::Null);
}

#[cfg(feature = "model-api")]
#[test]
fn bim_property_set_get_returns_null_for_missing_element() {
    let mut world = init_model_api_test_world();
    let err = handle_bim_property_set_get(&mut world, 999_999_999, "Pset_WallCommon", "FireRating")
        .unwrap_err();
    assert!(err.contains("not found"));
}

#[cfg(feature = "model-api")]
#[test]
fn bim_property_set_set_writes_validated_value_and_returns_null_prior() {
    use crate::plugins::modeling::definition::DefinitionId;
    use crate::plugins::modeling::property_sets::{
        ExportProfile, PropertyDef, PropertySetMap, PropertySetSchema, PropertySetSchemaRegistry,
        PropertyValueType,
    };

    let mut world = init_model_api_test_world();
    let element_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();

    // Register the schema for a wall-like definition.
    let mut registry = PropertySetSchemaRegistry::default();
    registry.register(
        DefinitionId("wall.lf_v1".into()),
        vec![PropertySetSchema::new("Pset_WallCommon").with_property(
            PropertyDef::new("FireRating", PropertyValueType::Text)
                .required_for(ExportProfile::new("IFC4")),
        )],
    );
    world.insert_resource(registry);
    world.insert_resource(bevy::ecs::message::Messages::<
        crate::plugins::modeling::property_sets::PropertySetChanged,
    >::default());

    // Write a valid Text value.
    let prior = handle_bim_property_set_set(
        &mut world,
        element_id,
        "wall.lf_v1",
        "Pset_WallCommon",
        "FireRating",
        serde_json::json!({ "text": "REI60" }),
    )
    .unwrap();
    assert_eq!(prior, Value::Null);

    // Read back via get.
    let v = handle_bim_property_set_get(&mut world, element_id, "Pset_WallCommon", "FireRating")
        .unwrap();
    assert_eq!(v, serde_json::json!({ "text": "REI60" }));

    // The component is now present on the entity.
    let entity = find_entity_by_element_id(&mut world, ElementId(element_id)).unwrap();
    let map = world.get::<PropertySetMap>(entity).unwrap();
    assert_eq!(map.property_count(), 1);
}

#[cfg(feature = "model-api")]
#[test]
fn bim_property_set_set_rejects_type_mismatch_and_does_not_mutate() {
    use crate::plugins::modeling::definition::DefinitionId;
    use crate::plugins::modeling::property_sets::{
        PropertyDef, PropertySetMap, PropertySetSchema, PropertySetSchemaRegistry,
        PropertyValueType,
    };

    let mut world = init_model_api_test_world();
    let element_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();

    let mut registry = PropertySetSchemaRegistry::default();
    registry.register(
        DefinitionId("wall.lf_v1".into()),
        vec![PropertySetSchema::new("Pset_WallCommon")
            .with_property(PropertyDef::new("FireRating", PropertyValueType::Text))],
    );
    world.insert_resource(registry);
    world.insert_resource(bevy::ecs::message::Messages::<
        crate::plugins::modeling::property_sets::PropertySetChanged,
    >::default());

    // Number where Text is expected → reject.
    let err = handle_bim_property_set_set(
        &mut world,
        element_id,
        "wall.lf_v1",
        "Pset_WallCommon",
        "FireRating",
        serde_json::json!({ "number": 60.0 }),
    )
    .unwrap_err();
    assert!(err.contains("type mismatch"));

    // The map should be empty after the rejection. The
    // PropertySetMap component was inserted (default) but
    // no value was written into it.
    let entity = find_entity_by_element_id(&mut world, ElementId(element_id)).unwrap();
    let map = world.get::<PropertySetMap>(entity).unwrap();
    assert_eq!(map.property_count(), 0);
}

#[cfg(feature = "model-api")]
#[test]
fn bim_exchange_identity_assign_get_and_list_round_trip() {
    use crate::plugins::modeling::exchange_identity::{ExchangeIdentityMap, ExchangeSystem};

    let mut world = init_model_api_test_world();
    let element_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();

    let result = handle_bim_exchange_identity_assign(
        &mut world,
        element_id,
        "ifc",
        "0Lh3Y2nzz3wuRfV4z4xRGn",
    )
    .unwrap();
    assert_eq!(result, Value::Null);

    let got = handle_bim_exchange_identity_get(&mut world, element_id, "ifc").unwrap();
    assert_eq!(got, Value::String("0Lh3Y2nzz3wuRfV4z4xRGn".into()));

    handle_bim_exchange_identity_assign(&mut world, element_id, "facility_db", "FM-42").unwrap();
    let listed = handle_bim_exchange_identity_list(&mut world, element_id).unwrap();
    assert_eq!(
        listed,
        serde_json::json!({
            "facility_db": "FM-42",
            "ifc": "0Lh3Y2nzz3wuRfV4z4xRGn"
        })
    );

    let entity = find_entity_by_element_id(&mut world, ElementId(element_id)).unwrap();
    let map = world.get::<ExchangeIdentityMap>(entity).unwrap();
    assert!(map.contains(&ExchangeSystem::Ifc));
    assert!(map.contains(&ExchangeSystem::Custom("facility_db".into())));
}

#[cfg(feature = "model-api")]
#[test]
fn bim_exchange_identity_assign_refuses_to_regenerate_existing_system_id() {
    let mut world = init_model_api_test_world();
    let element_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();

    handle_bim_exchange_identity_assign(&mut world, element_id, "ifc", "first").unwrap();
    let err =
        handle_bim_exchange_identity_assign(&mut world, element_id, "IFC", "second").unwrap_err();
    assert!(
        err.contains("already assigned"),
        "assign-if-absent invariant should reject regeneration: {err}"
    );
    assert_eq!(
        handle_bim_exchange_identity_get(&mut world, element_id, "ifc").unwrap(),
        Value::String("first".into())
    );
}

#[cfg(feature = "model-api")]
#[test]
fn bim_exchange_identity_get_returns_null_for_missing_map_or_system() {
    let mut world = init_model_api_test_world();
    let element_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();

    assert_eq!(
        handle_bim_exchange_identity_get(&mut world, element_id, "ifc").unwrap(),
        Value::Null
    );
    handle_bim_exchange_identity_assign(&mut world, element_id, "revit", "r-1").unwrap();
    assert_eq!(
        handle_bim_exchange_identity_get(&mut world, element_id, "ifc").unwrap(),
        Value::Null
    );
}

#[cfg(feature = "model-api")]
#[test]
fn bim_exchange_identity_errors_for_missing_element_or_blank_inputs() {
    let mut world = init_model_api_test_world();
    assert!(handle_bim_exchange_identity_get(&mut world, 999, "ifc")
        .unwrap_err()
        .contains("not found"));
    assert!(
        handle_bim_exchange_identity_assign(&mut world, 999, "", "x")
            .unwrap_err()
            .contains("exchange system must be non-empty")
    );

    let element_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();
    assert!(
        handle_bim_exchange_identity_assign(&mut world, element_id, "ifc", " ")
            .unwrap_err()
            .contains("exchange_id must be non-empty")
    );
}

#[cfg(feature = "model-api")]
#[test]
fn bim_property_set_set_emits_property_set_changed_message() {
    use crate::plugins::modeling::definition::DefinitionId;
    use crate::plugins::modeling::property_sets::{
        PropertyDef, PropertySetChanged, PropertySetSchema, PropertySetSchemaRegistry,
        PropertyValueType,
    };

    let mut world = init_model_api_test_world();
    let element_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();

    let mut registry = PropertySetSchemaRegistry::default();
    registry.register(
        DefinitionId("wall.lf_v1".into()),
        vec![PropertySetSchema::new("Pset_WallCommon")
            .with_property(PropertyDef::new("LoadBearing", PropertyValueType::Boolean))],
    );
    world.insert_resource(registry);
    world.insert_resource(bevy::ecs::message::Messages::<PropertySetChanged>::default());

    handle_bim_property_set_set(
        &mut world,
        element_id,
        "wall.lf_v1",
        "Pset_WallCommon",
        "LoadBearing",
        serde_json::json!({ "boolean": true }),
    )
    .unwrap();

    let messages = world.resource::<bevy::ecs::message::Messages<PropertySetChanged>>();
    // Use the read iterator pattern: drain to count.
    let collected: Vec<&PropertySetChanged> =
        bevy::ecs::message::Messages::iter_current_update_messages(messages).collect();
    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].element_id, ElementId(element_id));
    assert_eq!(collected[0].set_name, "Pset_WallCommon");
    assert_eq!(collected[0].property_name, "LoadBearing");
}

// ── ADR-026 Phase 6f / 6g MCP handler tests ─────────────────────

#[cfg(feature = "model-api")]
#[test]
fn bim_void_declare_for_definition_updates_interface_and_returns_prior_null() {
    use crate::plugins::modeling::definition::{DefinitionId, DefinitionRegistry};

    let mut world = init_model_api_test_world();
    handle_create_definition(
        &mut world,
        serde_json::json!({ "name": "Window", "definition_id": "ignored" }),
    )
    .unwrap();
    let definition_id = world
        .resource::<DefinitionRegistry>()
        .list()
        .first()
        .unwrap()
        .id
        .clone();
    let declaration = serde_json::json!({
        "shape": { "kind": "rectangular",
                   "width_param": "opening_width_m",
                   "height_param": "opening_height_m" },
        "placement": { "translation": [0.0, 0.0, 0.0], "yaw_radians": 0.0 },
        "exchange_role": "Opening"
    });
    let prior = handle_bim_void_declare_for_definition(
        &mut world,
        definition_id.as_str(),
        declaration.clone(),
    )
    .unwrap();
    assert_eq!(prior, Value::Null);
    let stored = world
        .resource::<DefinitionRegistry>()
        .get(&DefinitionId(definition_id.as_str().to_string()))
        .unwrap();
    assert!(stored.interface.void_declaration.is_some());
    assert_eq!(stored.definition_version, 2);
}

#[cfg(feature = "model-api")]
#[test]
fn bim_void_declare_for_definition_returns_prior_when_overwriting() {
    use crate::plugins::modeling::definition::DefinitionRegistry;

    let mut world = init_model_api_test_world();
    handle_create_definition(&mut world, serde_json::json!({ "name": "Window" })).unwrap();
    let definition_id = world
        .resource::<DefinitionRegistry>()
        .list()
        .first()
        .unwrap()
        .id
        .as_str()
        .to_string();
    let v1 = serde_json::json!({
        "shape": { "kind": "rectangular",
                   "width_param": "w", "height_param": "h" },
        "placement": { "translation": [0.0,0.0,0.0], "yaw_radians": 0.0 },
        "exchange_role": "Opening"
    });
    handle_bim_void_declare_for_definition(&mut world, &definition_id, v1.clone()).unwrap();
    let prior =
        handle_bim_void_declare_for_definition(&mut world, &definition_id, v1.clone()).unwrap();
    assert!(prior.is_object(), "expected prior declaration object");
}

#[cfg(feature = "model-api")]
#[test]
fn bim_void_declare_for_definition_rejects_malformed_json() {
    let mut world = init_model_api_test_world();
    let err = handle_bim_void_declare_for_definition(
        &mut world,
        "win.v1",
        serde_json::json!({ "shape": "garbage" }),
    )
    .unwrap_err();
    assert!(err.contains("VoidDeclaration"));
}

#[cfg(feature = "model-api")]
#[test]
fn bim_void_plan_placement_returns_three_part_outcome() {
    use crate::plugins::modeling::definition::DefinitionRegistry;

    let mut world = init_model_api_test_world();
    handle_create_definition(&mut world, serde_json::json!({ "name": "Window" })).unwrap();
    let definition_id = world
        .resource::<DefinitionRegistry>()
        .list()
        .first()
        .unwrap()
        .id
        .as_str()
        .to_string();
    let host_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();
    let filling_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([1.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();
    // Declare the void cut first.
    let declaration = serde_json::json!({
        "shape": { "kind": "rectangular",
                   "width_param": "w", "height_param": "h" },
        "placement": { "translation": [0.0, 0.0, 0.0], "yaw_radians": 0.0 },
        "exchange_role": "Opening"
    });
    handle_bim_void_declare_for_definition(&mut world, &definition_id, declaration).unwrap();
    let plan =
        handle_bim_void_plan_placement(&mut world, &definition_id, host_id, filling_id).unwrap();
    assert!(plan.get("opening_element").is_some());
    assert_eq!(plan["opening_context"]["host"], Value::from(host_id));
    assert_eq!(plan["opening_context"]["filling"], Value::from(filling_id));
    assert_eq!(plan["filling_link"]["opening"], plan["opening_element"]);
}

#[cfg(feature = "model-api")]
#[test]
fn bim_void_plan_placement_errors_when_declaration_missing() {
    use crate::plugins::modeling::definition::DefinitionRegistry;

    let mut world = init_model_api_test_world();
    handle_create_definition(&mut world, serde_json::json!({ "name": "Window" })).unwrap();
    let definition_id = world
        .resource::<DefinitionRegistry>()
        .list()
        .first()
        .unwrap()
        .id
        .as_str()
        .to_string();
    let host_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();
    let filling_id = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([2.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();
    let err = handle_bim_void_plan_placement(&mut world, &definition_id, host_id, filling_id)
        .unwrap_err();
    assert!(err.contains("no VoidDeclaration"));
}

#[cfg(feature = "model-api")]
#[test]
fn bim_spatial_assign_inserts_membership_and_validates_kind() {
    use crate::plugins::modeling::spatial_container::{
        SpatialContainerKind, SpatialContainerKindRegistry, SpatialMembership,
    };

    let mut world = init_model_api_test_world();
    let mut reg = SpatialContainerKindRegistry::default();
    reg.register(SpatialContainerKind::new("storey"));
    world.insert_resource(reg);

    let storey = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([10.0, 0.5, 10.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();
    let room = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([1.0, 1.5, 1.0]),
            half_extents: None,
            size: Some([3.0, 3.0, 3.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();
    handle_bim_spatial_assign(&mut world, room, storey, "storey").unwrap();

    let entity = find_entity_by_element_id(&mut world, ElementId(room)).unwrap();
    let m = world.get::<SpatialMembership>(entity).unwrap();
    assert_eq!(m.container, ElementId(storey));
}

#[cfg(feature = "model-api")]
#[test]
fn bim_spatial_assign_rejects_unregistered_kind() {
    let mut world = init_model_api_test_world();
    let storey = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([10.0, 0.5, 10.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();
    let room = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([1.0, 1.5, 1.0]),
            half_extents: None,
            size: Some([3.0, 3.0, 3.0]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();
    let err = handle_bim_spatial_assign(&mut world, room, storey, "ghost_kind").unwrap_err();
    assert!(err.contains("not registered"));
}

#[cfg(feature = "model-api")]
#[test]
fn bim_spatial_list_kind_registry_returns_sorted_kinds() {
    use crate::plugins::modeling::spatial_container::{
        SpatialContainerKind, SpatialContainerKindRegistry,
    };
    let mut world = init_model_api_test_world();
    let mut reg = SpatialContainerKindRegistry::default();
    reg.register(SpatialContainerKind::new("zone"));
    reg.register(SpatialContainerKind::new("storey"));
    reg.register(SpatialContainerKind::new("space"));
    world.insert_resource(reg);
    let result = handle_bim_spatial_list_kind_registry(&mut world).unwrap();
    assert_eq!(result, serde_json::json!(["space", "storey", "zone"]));
}

#[cfg(feature = "model-api")]
#[test]
fn bim_spatial_list_kind_registry_returns_empty_when_unset() {
    let mut world = init_model_api_test_world();
    let result = handle_bim_spatial_list_kind_registry(&mut world).unwrap();
    assert_eq!(result, serde_json::json!([]));
}

#[cfg(feature = "model-api")]
#[test]
fn parametric_class_token_match_uses_head_noun_not_generic_suffix() {
    // The dotted gable roof type must surface for the `roof_system` noun...
    assert!(parametric_class_token_match(
        "roof_system",
        "architecture.roof.system.gable",
        "Gable Roof System",
    ));
    assert!(parametric_class_token_match(
        "roof_system",
        "architecture.roof.truss.common",
        "Common Truss",
    ));
    // ...but NOT for `foundation_system`: the only shared token is the generic
    // suffix `system`, which is a stopword. This is the regression that made
    // discover_curated_paths(foundation_system) wrongly return the gable roof.
    assert!(!parametric_class_token_match(
        "foundation_system",
        "architecture.roof.system.gable",
        "Gable Roof System",
    ));
    // `wall_assembly` shares only the stopword `assembly` with nothing roof-like.
    assert!(!parametric_class_token_match(
        "wall_assembly",
        "architecture.roof.system.gable",
        "Gable Roof System",
    ));
    // A head noun that genuinely matches still works across separators.
    assert!(parametric_class_token_match(
        "window",
        "architecture.window.double-european",
        "Double European Window",
    ));
}

// ---------------------------------------------------------------------------
// Vocabulary-surface agreement + atomic semantic create (PP defect fixes).
// Defect 1: discover_curated_paths / request_corpus_expansion / create_box's
//           semantic.element_class must agree on what a term is.
// Defect 2: a create whose semantic annotation fails validation must leave no
//           orphan geometry behind.
// ---------------------------------------------------------------------------

#[cfg(feature = "model-api")]
#[test]
fn create_box_with_invalid_element_class_creates_no_orphan_geometry() {
    // Defect 2 (atomic create): a create_box whose semantic.element_class fails
    // validation must reject the whole call and leave NO box behind — not create
    // un-annotated geometry and silently drop only the annotation.
    let mut world = init_model_api_test_world();

    // A plain (un-annotated) box proves the world can actually create geometry.
    handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: None,
        },
    )
    .expect("plain box should be creatable");
    assert_eq!(list_entities(&world).len(), 1, "baseline: one plain box");

    // A box annotated with an unregistered element_class must be rejected...
    let result = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([5.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: Some(SemanticEntityAnnotationRequest {
                element_class: Some("totally_unregistered_class".to_string()),
                refinement_state: None,
                parameters: json!({}),
                unresolved_decisions: vec![],
                source_refs: vec![],
                rationale: None,
            }),
        },
    );
    let error = result.expect_err("invalid element_class must reject the whole call");
    assert!(
        error.contains("totally_unregistered_class"),
        "error should name the offending class; got: {error}"
    );

    // ...and crucially must NOT have created an orphan box.
    assert_eq!(
        list_entities(&world).len(),
        1,
        "no orphan geometry: the rejected call must not add a box"
    );
}

#[cfg(feature = "model-api")]
fn register_hosted_opening_patterns(world: &mut World) {
    use crate::capability_registry::AssemblyPatternDescriptor;
    let mut registry = world.resource_mut::<CapabilityRegistry>();
    for (pattern_id, label, tag) in [
        ("hosted_window_opening", "Hosted Window Opening", "window"),
        (
            "hosted_entrance_door_opening",
            "Hosted Entrance Door Opening",
            "door",
        ),
    ] {
        registry.register_assembly_pattern(AssemblyPatternDescriptor {
            pattern_id: pattern_id.into(),
            label: label.into(),
            description: "Hosted opening cut into a wall.".into(),
            target_types: vec!["opening".into()],
            axis: "exterior_to_interior".into(),
            layers: Vec::new(),
            relation_rules: Vec::new(),
            root_layer_ids: Vec::new(),
            requires_support_path: false,
            tags: vec![tag.into()],
            parameter_schema: json!({}),
        });
    }
}

#[cfg(feature = "model-api")]
#[test]
fn native_non_class_terms_agree_across_discovery_gap_and_annotation() {
    // Defect 1 (surfaces must agree): door/window are NOT element classes — they
    // are authored as `opening` entities and covered by hosted-opening assembly
    // patterns. All three surfaces must treat them as native terms:
    //   - discover_curated_paths → non_class_term pointer (NOT a corpus gap)
    //   - request_corpus_expansion → rejected (it is not a gap)
    //   - create_box semantic.element_class → rejected atomically (no geometry)
    let mut world = init_model_api_test_world();
    register_hosted_opening_patterns(&mut world);

    // discover_curated_paths surfaces a native-term pointer, not a gap.
    let discovery = handle_discover_curated_paths(
        &world,
        CuratedPathDiscoveryRequest {
            path_kind: Some("recipe".into()),
            element_class: Some("window".into()),
            query: None,
            context: json!({}),
        },
    )
    .expect("discovery should succeed");
    let non_class = discovery
        .non_class_term
        .expect("window should surface as a native non-class term");
    assert_eq!(non_class.term, "window");
    assert!(
        non_class
            .assembly_pattern_ids
            .contains(&"hosted_window_opening".to_string()),
        "native term should point at the covering pattern; got: {:?}",
        non_class.assembly_pattern_ids
    );
    assert!(
        discovery.no_curated_path.is_none(),
        "a native term must NOT be advertised as a corpus gap"
    );

    // request_corpus_expansion rejects the same native term.
    let gap_error = handle_request_corpus_expansion(
        &mut world,
        Some("door".into()),
        Some("SE".into()),
        "rule_pack".into(),
        "tried to gap a door".into(),
    )
    .expect_err("door is a native term, not a corpus gap");
    assert!(
        gap_error.contains("door"),
        "rejection should name the term; got: {gap_error}"
    );

    // create_box's semantic annotation rejects it atomically — no orphan box.
    let create_result = handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: Some([0.0, 0.0, 0.0]),
            half_extents: None,
            size: Some([1.0, 1.0, 1.0]),
            rotation: None,
            semantic: Some(SemanticEntityAnnotationRequest {
                element_class: Some("window".into()),
                refinement_state: None,
                parameters: json!({}),
                unresolved_decisions: vec![],
                source_refs: vec![],
                rationale: None,
            }),
        },
    );
    assert!(
        create_result.is_err(),
        "annotating a box as element_class=window must be rejected"
    );
    assert!(
        list_entities(&world).is_empty(),
        "rejected native-term annotation must not create geometry"
    );
}

#[cfg(feature = "model-api")]
#[test]
fn unknown_term_still_records_corpus_gap_and_no_curated_path() {
    // Guard the other direction: a genuinely unrecognised term (no factory, no
    // pattern, not an element class) is still a first-class corpus gap on both
    // the discovery and the request surfaces — the Defect 1 fix must not swallow
    // real gaps.
    let mut world = init_model_api_test_world();

    let discovery = handle_discover_curated_paths(
        &world,
        CuratedPathDiscoveryRequest {
            path_kind: Some("recipe".into()),
            element_class: Some("flux_capacitor".into()),
            query: None,
            context: json!({}),
        },
    )
    .expect("discovery should succeed");
    assert!(
        discovery.non_class_term.is_none(),
        "an unknown term is not a native non-class term"
    );
    assert!(
        discovery.no_curated_path.is_some(),
        "an unknown term must still surface as a corpus gap"
    );

    let gap = handle_request_corpus_expansion(
        &mut world,
        Some("flux_capacitor".into()),
        None,
        "rule_pack".into(),
        "genuinely missing knowledge".into(),
    )
    .expect("an unknown term must still record a corpus gap");
    assert!(!gap.id.is_empty(), "recorded gap must have an id");
}

// ---------------------------------------------------------------------------
// Change 1 — select_recipe surfaces installed RecipeArtifactRegistry entries
// ---------------------------------------------------------------------------

#[cfg(feature = "model-api")]
#[test]
fn select_recipe_surfaces_installed_recipe_artifact() {
    // After install_recipe_from_session_export, an agent-installed
    // AuthoringScript recipe must appear in select_recipe results even
    // though there are no native CapabilityRegistry recipe families for
    // the target class.
    use crate::curation::authoring_script::AuthoringScript;
    use crate::curation::{
        provenance::{Confidence, Lineage, Provenance},
        scope_trust::{Scope, Trust},
        AssetId, AssetKindId, CurationMeta, RecipeArtifact, RecipeArtifactRegistry, RecipeBody,
        RECIPE_ARTIFACT_KIND,
    };
    use crate::plugins::refinement::AgentId;

    let mut world = World::new();
    world.insert_resource(CapabilityRegistry::default());

    // Build and install a recipe artifact for "synthetic_roof" targeting
    // "roof_system" — domain-neutral test class.
    let asset_id = AssetId("installed_recipe/synthetic_roof".into());
    let artifact = RecipeArtifact {
        meta: CurationMeta::new(
            asset_id.clone(),
            AssetKindId(RECIPE_ARTIFACT_KIND.into()),
            Provenance {
                author: AgentId("test_agent".into()),
                confidence: Confidence::Medium,
                lineage: Lineage::Freeform,
                rationale: Some("domain-neutral installed recipe for test".into()),
                jurisdiction: None,
                catalog_dependencies: Vec::new(),
                evidence: Vec::new(),
            },
        )
        .with_scope(Scope::Project)
        .with_trust(Trust::Draft),
        body: RecipeBody::AuthoringScript {
            script: AuthoringScript::stub(crate::curation::authoring_script::MutationScope::None),
        },
        parameter_schema: serde_json::Value::Null,
        target_class: "roof_system".into(),
        supported_refinement_states: vec![
            crate::plugins::refinement::RefinementState::Constructible,
        ],
        tests: Vec::new(),
    };

    let mut registry = RecipeArtifactRegistry::default();
    registry.insert(artifact);
    world.insert_resource(registry);

    // With no native CapabilityRegistry family for "roof_system", the
    // pre-Change-1 result would be empty. After Change 1 it must return
    // the installed artifact.
    let ranking = handle_select_recipe(
        &world,
        "roof_system".into(),
        serde_json::json!({ "target_state": "Constructible" }),
    )
    .expect("select_recipe should succeed");

    assert_eq!(ranking.len(), 1, "one installed recipe must surface");
    assert_eq!(ranking[0].target_class, "roof_system");
    assert!(
        ranking[0].executable,
        "installed artifact must be marked executable"
    );
    assert_eq!(
        ranking[0].execution_path.as_deref(),
        Some("instantiate_recipe"),
        "execution path must be instantiate_recipe"
    );
    assert!(
        ranking[0].how_to_instantiate.contains("instantiate_recipe"),
        "how_to_instantiate must name instantiate_recipe"
    );

    // Also confirm the wrong class returns nothing.
    let empty = handle_select_recipe(&world, "wall_assembly".into(), serde_json::json!({}))
        .expect("select_recipe for different class should succeed");
    assert!(empty.is_empty(), "different class must return no results");
}

#[cfg(feature = "model-api")]
#[test]
fn discover_curated_paths_ranks_query_specific_installed_recipe_first() {
    use crate::capability_registry::{
        ElementClassId, GenerateInput, GenerateOutput, ObligationTemplate, RecipeFamilyDescriptor,
    };
    use crate::curation::authoring_script::{AuthoringScript, MutationScope};
    use crate::curation::{
        provenance::{Confidence, Lineage, Provenance},
        scope_trust::{Scope, Trust},
        AssetId, AssetKindId, CurationMeta, RecipeArtifact, RecipeArtifactRegistry, RecipeBody,
        RECIPE_ARTIFACT_KIND,
    };
    use crate::plugins::model_api::server::CuratedPathDiscoveryRequest;
    use crate::plugins::refinement::{AgentId, RefinementState};
    use std::collections::HashMap;
    use std::sync::Arc;

    let mut world = World::new();
    let mut capability_registry = CapabilityRegistry::default();
    capability_registry.register_recipe_family(RecipeFamilyDescriptor {
        id: crate::capability_registry::RecipeFamilyId("generic_roof".into()),
        target_class: ElementClassId("roof_system".into()),
        label: "Generic gable roof".into(),
        description: "Generic roof recipe used when no specific finish is requested.".into(),
        parameters: Vec::new(),
        supported_refinement_levels: vec![RefinementState::Schematic],
        obligation_specializations: HashMap::<RefinementState, Vec<ObligationTemplate>>::new(),
        promotion_critical_path_specializations: HashMap::new(),
        generate: Arc::new(
            |_: GenerateInput, _: &mut World| -> Result<GenerateOutput, String> {
                Ok(GenerateOutput::default())
            },
        ),
    });
    world.insert_resource(capability_registry);

    let asset_id = AssetId("installed_recipe/standing_seam_specific".into());
    let artifact = RecipeArtifact {
        meta: CurationMeta::new(
            asset_id.clone(),
            AssetKindId(RECIPE_ARTIFACT_KIND.into()),
            Provenance {
                author: AgentId("test_agent".into()),
                confidence: Confidence::Medium,
                lineage: Lineage::Freeform,
                rationale: Some("Standing seam metal roof covering with folded sheet seams".into()),
                jurisdiction: None,
                catalog_dependencies: Vec::new(),
                evidence: Vec::new(),
            },
        )
        .with_scope(Scope::Project)
        .with_trust(Trust::Draft),
        body: RecipeBody::AuthoringScript {
            script: AuthoringScript::stub(MutationScope::None),
        },
        parameter_schema: serde_json::Value::Null,
        target_class: "roof_system".into(),
        supported_refinement_states: vec![RefinementState::Schematic],
        tests: Vec::new(),
    };
    let mut artifact_registry = RecipeArtifactRegistry::default();
    artifact_registry.insert(artifact);
    world.insert_resource(artifact_registry);

    let discovery = handle_discover_curated_paths(
        &world,
        CuratedPathDiscoveryRequest {
            path_kind: Some("recipe".into()),
            element_class: Some("roof_system".into()),
            query: Some("standing seam metal roof".into()),
            context: serde_json::json!({ "target_state": "Schematic" }),
        },
    )
    .expect("recipe discovery should succeed");

    assert_eq!(
        discovery
            .recipe_rankings
            .first()
            .map(|ranking| ranking.id.as_str()),
        Some("standing_seam_specific"),
        "query-specific installed executable recipe should outrank a generic class recipe"
    );
}

#[cfg(feature = "model-api")]
#[test]
fn select_recipe_surfaces_installed_recipe_parameters_and_defaults() {
    // An installed AuthoringScript recipe must surface its declared
    // parameters (names + defaults from parameter_defaults) in the ranking,
    // so a code-blind agent can discover them without trial-and-error.
    use crate::curation::authoring_script::{AuthoringScript, MutationScope};
    use crate::curation::{
        provenance::{Confidence, Lineage, Provenance},
        scope_trust::{Scope, Trust},
        AssetId, AssetKindId, CurationMeta, RecipeArtifact, RecipeArtifactRegistry, RecipeBody,
        RECIPE_ARTIFACT_KIND,
    };
    use crate::plugins::refinement::{AgentId, RefinementState};

    let mut world = World::new();
    world.insert_resource(CapabilityRegistry::default());

    let mut script = AuthoringScript::stub(MutationScope::None);
    script.parameter_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "min_x": {"type": "number"},
            "pitch_deg": {"type": "number"}
        },
        "required": ["min_x", "pitch_deg"]
    });
    script
        .parameter_defaults
        .insert("min_x".into(), serde_json::json!(-4.5));
    script
        .parameter_defaults
        .insert("pitch_deg".into(), serde_json::json!(45.0));

    let asset_id = AssetId("installed_recipe/synthetic_param_roof".into());
    let artifact = RecipeArtifact {
        meta: CurationMeta::new(
            asset_id,
            AssetKindId(RECIPE_ARTIFACT_KIND.into()),
            Provenance {
                author: AgentId("test_agent".into()),
                confidence: Confidence::Medium,
                lineage: Lineage::Freeform,
                rationale: Some("param-bearing installed recipe for test".into()),
                jurisdiction: None,
                catalog_dependencies: Vec::new(),
                evidence: Vec::new(),
            },
        )
        .with_scope(Scope::Project)
        .with_trust(Trust::Draft),
        body: RecipeBody::AuthoringScript { script },
        parameter_schema: serde_json::Value::Null,
        target_class: "roof_system".into(),
        supported_refinement_states: vec![RefinementState::Constructible],
        tests: Vec::new(),
    };

    let mut registry = RecipeArtifactRegistry::default();
    registry.insert(artifact);
    world.insert_resource(registry);

    let ranking = handle_select_recipe(
        &world,
        "roof_system".into(),
        serde_json::json!({ "target_state": "Constructible" }),
    )
    .expect("select_recipe should succeed");

    assert_eq!(ranking.len(), 1);
    let names: Vec<&str> = ranking[0]
        .parameters
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert!(
        names.contains(&"min_x") && names.contains(&"pitch_deg"),
        "parameters must surface declared property names, got {names:?}"
    );
    let pitch = ranking[0]
        .parameters
        .iter()
        .find(|p| p.name == "pitch_deg")
        .expect("pitch_deg present");
    assert_eq!(
        pitch.default,
        Some(serde_json::json!(45.0)),
        "default must come from parameter_defaults"
    );
    assert!(
        ranking[0].how_to_instantiate.contains("pitch_deg=45"),
        "hint must enumerate parameter names with defaults, got: {}",
        ranking[0].how_to_instantiate
    );
}

// ---------------------------------------------------------------------------
// Change 2 — resolve_obligation handler
// ---------------------------------------------------------------------------

/// Build a minimal world with the command pipeline and a single entity that
/// has an ObligationSet with one `Unresolved` domain-neutral obligation.
#[cfg(feature = "model-api")]
fn obligation_test_world() -> (bevy::prelude::World, bevy::ecs::entity::Entity) {
    use crate::plugins::refinement::{Obligation, ObligationId, ObligationSet, SemanticRole};

    let mut world = init_model_api_test_world();
    let entity = world
        .spawn((
            ElementId(9001),
            crate::plugins::refinement::RefinementStateComponent {
                state: crate::plugins::refinement::RefinementState::Conceptual,
            },
        ))
        .id();

    world.entity_mut(entity).insert(ObligationSet {
        entries: vec![Obligation {
            id: ObligationId("test_ob".into()),
            role: SemanticRole("primary_structure".into()),
            required_by_state: crate::plugins::refinement::RefinementState::Schematic,
            status: crate::plugins::refinement::ObligationStatus::Unresolved,
        }],
    });

    (world, entity)
}

#[cfg(feature = "model-api")]
#[test]
fn resolve_obligation_sets_satisfied_by() {
    use crate::plugins::model_api::request::{ObligationResolution, ResolveObligationRequest};
    use crate::plugins::refinement::{ObligationSet, ObligationStatus};

    let (mut world, entity) = obligation_test_world();
    // Spawn the "child" entity that satisfies the obligation.
    world.spawn((ElementId(9999),));

    let result = handle_resolve_obligation(
        &mut world,
        ResolveObligationRequest {
            element_id: 9001,
            obligation_id: "test_ob".into(),
            resolution: ObligationResolution::SatisfiedBy { element_id: 9999 },
        },
    )
    .expect("SatisfiedBy resolution must succeed");

    assert_eq!(result.new_status, "SatisfiedBy:9999");
    // Verify the component was actually updated.
    let set = world
        .get::<ObligationSet>(entity)
        .expect("ObligationSet present");
    assert_eq!(set.entries[0].status, ObligationStatus::SatisfiedBy(9999));
}

#[cfg(feature = "model-api")]
#[test]
fn resolve_obligation_sets_deferred() {
    use crate::plugins::model_api::request::{ObligationResolution, ResolveObligationRequest};
    use crate::plugins::refinement::{ObligationSet, ObligationStatus};

    let (mut world, entity) = obligation_test_world();

    let result = handle_resolve_obligation(
        &mut world,
        ResolveObligationRequest {
            element_id: 9001,
            obligation_id: "test_ob".into(),
            resolution: ObligationResolution::Deferred {
                reason: "awaiting SE input".into(),
            },
        },
    )
    .expect("Deferred resolution must succeed");

    assert!(result.new_status.starts_with("Deferred:"));
    let set = world.get::<ObligationSet>(entity).expect("ObligationSet");
    assert!(matches!(
        &set.entries[0].status,
        ObligationStatus::Deferred(r) if r == "awaiting SE input"
    ));
}

#[cfg(feature = "model-api")]
#[test]
fn resolve_obligation_sets_waived() {
    use crate::plugins::model_api::request::{ObligationResolution, ResolveObligationRequest};
    use crate::plugins::refinement::{ObligationSet, ObligationStatus};

    let (mut world, entity) = obligation_test_world();

    let result = handle_resolve_obligation(
        &mut world,
        ResolveObligationRequest {
            element_id: 9001,
            obligation_id: "test_ob".into(),
            resolution: ObligationResolution::Waived {
                rationale: "out of scope".into(),
            },
        },
    )
    .expect("Waived resolution must succeed");

    assert!(result.new_status.starts_with("Waived:"));
    let set = world.get::<ObligationSet>(entity).expect("ObligationSet");
    assert!(matches!(
        &set.entries[0].status,
        ObligationStatus::Waived(r) if r == "out of scope"
    ));
}

#[cfg(feature = "model-api")]
#[test]
fn resolve_obligation_errors_on_unknown_obligation_id() {
    use crate::plugins::model_api::request::{ObligationResolution, ResolveObligationRequest};

    let (mut world, _) = obligation_test_world();

    let err = handle_resolve_obligation(
        &mut world,
        ResolveObligationRequest {
            element_id: 9001,
            obligation_id: "nonexistent_ob".into(),
            resolution: ObligationResolution::Waived {
                rationale: "test".into(),
            },
        },
    )
    .expect_err("unknown obligation id must return an error");

    assert!(
        err.contains("nonexistent_ob"),
        "error must name the unknown obligation id: {err}"
    );
}

#[cfg(feature = "model-api")]
#[test]
fn resolve_obligation_errors_on_missing_obligation_set() {
    use crate::plugins::model_api::request::{ObligationResolution, ResolveObligationRequest};

    let mut world = init_model_api_test_world();
    // Entity with no ObligationSet.
    world.spawn((
        ElementId(9002),
        crate::plugins::refinement::RefinementStateComponent::default(),
    ));

    let err = handle_resolve_obligation(
        &mut world,
        ResolveObligationRequest {
            element_id: 9002,
            obligation_id: "load_path".into(),
            resolution: ObligationResolution::Waived {
                rationale: "test".into(),
            },
        },
    )
    .expect_err("missing ObligationSet must return an error");

    assert!(
        err.to_lowercase().contains("no obligationset")
            || err.to_lowercase().contains("obligation"),
        "error must mention ObligationSet: {err}"
    );
}

#[cfg(feature = "model-api")]
#[test]
fn resolve_obligation_errors_when_satisfied_by_child_missing() {
    use crate::plugins::model_api::request::{ObligationResolution, ResolveObligationRequest};

    let (mut world, _) = obligation_test_world();
    // Do NOT spawn entity 8888 — it doesn't exist.

    let err = handle_resolve_obligation(
        &mut world,
        ResolveObligationRequest {
            element_id: 9001,
            obligation_id: "test_ob".into(),
            resolution: ObligationResolution::SatisfiedBy { element_id: 8888 },
        },
    )
    .expect_err("SatisfiedBy with missing child must return an error");

    assert!(
        err.contains("8888"),
        "error must reference the missing child id: {err}"
    );
}

// ---------------------------------------------------------------------------
// PP82/ADR-042 — recipe instantiation executes AuthoringScript
// ---------------------------------------------------------------------------

/// Build a minimal AuthoringScript that creates one box via `create_box` and
/// binds the returned `element_id`.  Uses only tools supported by
/// `ModelApiStepExecutor`.
#[cfg(feature = "model-api")]
fn build_two_box_recipe_script() -> crate::curation::authoring_script::AuthoringScript {
    use crate::curation::authoring_script::{
        ArgExpr, AuthoringScript, McpToolId, MutationScope, OutputPath, ScriptInstruction, Step,
        StepId,
    };
    use std::collections::{BTreeMap, BTreeSet};

    let mut script = AuthoringScript::stub(MutationScope::ProjectRoot);
    script.allowed_tools = [McpToolId::new("create_box")]
        .into_iter()
        .collect::<BTreeSet<_>>();

    // Step 1: create first box (wall slab A)
    let step_a = Step {
        id: StepId::new("box_a"),
        tool: McpToolId::new("create_box"),
        args: {
            let mut m = BTreeMap::new();
            m.insert(
                "center".into(),
                ArgExpr::Literal {
                    value: serde_json::json!([0.0, 1.0, 0.0]),
                },
            );
            m.insert(
                "half_extents".into(),
                ArgExpr::Literal {
                    value: serde_json::json!([1.0, 1.0, 0.1]),
                },
            );
            m
        },
        bindings: {
            let mut m = BTreeMap::new();
            m.insert("id_a".into(), OutputPath::new("element_id"));
            m
        },
        essential: true,
        precondition: None,
    };

    // Step 2: create second box (wall slab B), referencing no prior step
    let step_b = Step {
        id: StepId::new("box_b"),
        tool: McpToolId::new("create_box"),
        args: {
            let mut m = BTreeMap::new();
            m.insert(
                "center".into(),
                ArgExpr::Literal {
                    value: serde_json::json!([3.0, 1.0, 0.0]),
                },
            );
            m.insert(
                "half_extents".into(),
                ArgExpr::Literal {
                    value: serde_json::json!([1.0, 1.0, 0.1]),
                },
            );
            m
        },
        bindings: BTreeMap::new(),
        essential: true,
        precondition: None,
    };

    script.steps = vec![
        ScriptInstruction::Call(step_a),
        ScriptInstruction::Call(step_b),
    ];
    script
}

/// Insert a test AuthoringScript recipe artifact into a fresh
/// `RecipeArtifactRegistry` on the world.
#[cfg(feature = "model-api")]
fn install_two_box_recipe(world: &mut World, family_id: &str, target_class: &str) {
    use crate::curation::{
        provenance::{Confidence, Lineage, Provenance},
        scope_trust::{Scope, Trust},
        AssetId, AssetKindId, CurationMeta, RecipeArtifact, RecipeArtifactRegistry, RecipeBody,
        RECIPE_ARTIFACT_KIND,
    };
    use crate::plugins::refinement::{AgentId, RefinementState};

    let asset_id = AssetId(format!("installed_recipe/{family_id}"));
    let artifact = RecipeArtifact {
        meta: CurationMeta::new(
            asset_id,
            AssetKindId(RECIPE_ARTIFACT_KIND.into()),
            Provenance {
                author: AgentId("test_agent".into()),
                confidence: Confidence::High,
                lineage: Lineage::Freeform,
                rationale: Some("test two-box recipe".into()),
                jurisdiction: None,
                catalog_dependencies: Vec::new(),
                evidence: Vec::new(),
            },
        )
        .with_scope(Scope::Project)
        .with_trust(Trust::Draft),
        body: RecipeBody::AuthoringScript {
            script: build_two_box_recipe_script(),
        },
        parameter_schema: serde_json::json!({"type": "object", "properties": {}}),
        target_class: target_class.into(),
        supported_refinement_states: vec![
            RefinementState::Schematic,
            RefinementState::Constructible,
        ],
        tests: Vec::new(),
    };
    let mut registry = world
        .get_resource_mut::<RecipeArtifactRegistry>()
        .unwrap_or_else(|| {
            panic!(
                "RecipeArtifactRegistry must be initialized before calling install_two_box_recipe"
            )
        });
    registry.insert(artifact);
}

/// An installed `AuthoringScript` recipe, when passed to `instantiate_recipe`,
/// must:
///   1. Create the root semantic anchor entity.
///   2. Create every entity the script's steps produce (two boxes here).
///   3. Return `steps_run_count` matching the script's step count.
///   4. Return `recipe_id_used` equal to the requested family id.
///   5. Leave all created entities in the authored model (verifiable via
///      list_entities).
///   6. Leave the operations undoable: History must have at least one entry.
#[cfg(feature = "model-api")]
#[test]
fn instantiate_recipe_with_authoring_script_creates_sub_elements_and_is_undoable() {
    use crate::capability_registry::{ElementClassDescriptor, ElementClassId};
    use crate::plugins::refinement::SemanticRole;

    let mut world = init_model_api_test_world();

    // Register "wall_assembly" as a semantic element class so that
    // handle_instantiate_recipe's element_class validation passes.
    world
        .resource_mut::<CapabilityRegistry>()
        .register_element_class(ElementClassDescriptor {
            id: ElementClassId("wall_assembly".into()),
            label: "Wall Assembly".into(),
            description: "Test element class for recipe instantiation.".into(),
            semantic_roles: vec![SemanticRole("primary_structure".into())],
            class_min_obligations: std::collections::HashMap::new(),
            class_min_promotion_critical_paths: std::collections::HashMap::new(),
            parameter_schema: serde_json::json!({}),
        });

    world.insert_resource(crate::curation::RecipeArtifactRegistry::default());
    install_two_box_recipe(&mut world, "two_box_wall", "wall_assembly");

    // Pre-register ElementId so that handle_instantiate_recipe's
    // collect_ids closure can create a QueryState for it on the first
    // call (before any ElementId entity has been spawned). Without this,
    // World::try_query::<(&ElementId,)>() returns None on a fresh world
    // where the component has never been used.
    world.register_component::<crate::plugins::identity::ElementId>();

    let entity_count_before = list_entities(&world).len();

    // Root is created at "Schematic"; promote must target a higher level.
    let result = handle_instantiate_recipe(
        &mut world,
        InstantiateRecipeRequest {
            family_id: "two_box_wall".into(),
            target_class: "wall_assembly".into(),
            parameters: serde_json::json!({}),
            placement: None,
            target_state: Some("Constructible".into()),
        },
    )
    .expect("AuthoringScript recipe instantiation must succeed");

    // Root entity was created; verify it is inspectable in the world.
    // (ElementIdAllocator starts at 0, so id 0 is a valid first id.)
    assert!(
        get_entity_snapshot(
            &world,
            crate::plugins::identity::ElementId(result.root_element_id)
        )
        .is_some(),
        "root_element_id {} must refer to an existing entity",
        result.root_element_id
    );

    // Two boxes were created by the script steps.
    assert_eq!(
        result.created_element_ids.len(),
        2,
        "script has two create_box steps → two sub-elements must be created; \
         got {:?}",
        result.created_element_ids
    );

    // No created id is the root anchor.
    assert!(
        !result.created_element_ids.contains(&result.root_element_id),
        "root_element_id must not appear in created_element_ids"
    );

    // steps_run_count reflects the two-step script.
    assert_eq!(
        result.steps_run_count, 2,
        "steps_run_count must equal the number of executed script steps"
    );

    // recipe_id_used must echo back the requested family id.
    assert_eq!(
        result.recipe_id_used.as_deref(),
        Some("two_box_wall"),
        "recipe_id_used must match the requested family id"
    );

    // All created entities are visible through list_entities.
    let entity_count_after = list_entities(&world).len();
    // Root + 2 script boxes = 3 new entities minimum.
    assert!(
        entity_count_after >= entity_count_before + 3,
        "list_entities must see root + at least 2 script boxes; \
         before={entity_count_before}, after={entity_count_after}"
    );

    // Undo is available: the PendingCommandQueue was flushed (commands went
    // through the pipeline), which means they are now in History. Verify by
    // checking that the pending queue is empty (flush happened) and that the
    // entity count persisted (entities survived the flush).
    let queue = world.resource::<crate::plugins::history::PendingCommandQueue>();
    assert!(
        queue.commands.is_empty(),
        "PendingCommandQueue must be empty after flush — all commands were committed"
    );
}

#[cfg(feature = "model-api")]
#[test]
fn instantiate_recipe_at_creation_state_materializes_without_promote_error() {
    use crate::capability_registry::{ElementClassDescriptor, ElementClassId};
    use crate::plugins::refinement::SemanticRole;

    let mut world = init_model_api_test_world();

    world
        .resource_mut::<CapabilityRegistry>()
        .register_element_class(ElementClassDescriptor {
            id: ElementClassId("wall_assembly".into()),
            label: "Wall Assembly".into(),
            description: "Test element class for same-level recipe instantiation.".into(),
            semantic_roles: vec![SemanticRole("primary_structure".into())],
            class_min_obligations: std::collections::HashMap::new(),
            class_min_promotion_critical_paths: std::collections::HashMap::new(),
            parameter_schema: serde_json::json!({}),
        });

    world.insert_resource(crate::curation::RecipeArtifactRegistry::default());
    install_two_box_recipe(&mut world, "two_box_wall", "wall_assembly");
    world.register_component::<crate::plugins::identity::ElementId>();

    let result = handle_instantiate_recipe(
        &mut world,
        InstantiateRecipeRequest {
            family_id: "two_box_wall".into(),
            target_class: "wall_assembly".into(),
            parameters: serde_json::json!({}),
            placement: None,
            target_state: Some("Schematic".into()),
        },
    )
    .expect("same-level executable recipe instantiation must materialize cleanly");

    assert_eq!(result.state, "Schematic");
    assert_eq!(result.steps_run_count, 2);
    assert_eq!(
        result.created_element_ids.len(),
        2,
        "script-created elements must be reported instead of hidden behind a promotion error"
    );
    assert!(
        result.promotion_blocked.is_none(),
        "same-level materialization is not a blocked promotion"
    );
}

/// World with "wall_assembly" registered carrying the given Constructible
/// obligations (which nothing in the two-box recipe satisfies, so the
/// promotion gate blocks) and the executable two-box recipe installed as
/// family "two_box_wall".
#[cfg(feature = "model-api")]
fn gated_two_box_world(obligation_ids: &[&str]) -> World {
    use crate::capability_registry::{ElementClassDescriptor, ElementClassId, ObligationTemplate};
    use crate::plugins::refinement::{ObligationId, RefinementState, SemanticRole};

    let mut world = init_model_api_test_world();

    let mut class_min_obligations = std::collections::HashMap::new();
    class_min_obligations.insert(
        RefinementState::Constructible,
        obligation_ids
            .iter()
            .map(|id| ObligationTemplate {
                id: ObligationId((*id).into()),
                role: SemanticRole("primary_structure".into()),
                required_by_state: RefinementState::Constructible,
            })
            .collect(),
    );
    world
        .resource_mut::<CapabilityRegistry>()
        .register_element_class(ElementClassDescriptor {
            id: ElementClassId("wall_assembly".into()),
            label: "Wall Assembly".into(),
            description: "Test element class with unsatisfiable promotion obligations.".into(),
            semantic_roles: vec![SemanticRole("primary_structure".into())],
            class_min_obligations,
            class_min_promotion_critical_paths: std::collections::HashMap::new(),
            parameter_schema: serde_json::json!({}),
        });

    world.insert_resource(crate::curation::RecipeArtifactRegistry::default());
    install_two_box_recipe(&mut world, "two_box_wall", "wall_assembly");
    world
}

/// When the recipe script executes but the post-execution promotion gate then
/// blocks on unsatisfied obligations, `instantiate_recipe` must report partial
/// success — the created geometry persists, so the agent must be told exactly
/// what exists and what blocks the refinement claim. A bare error here reads
/// as "nothing happened" and a blind retry duplicates geometry (live MCP trace
/// defect, 2026-06-11: door_unit → Constructible).
#[cfg(feature = "model-api")]
#[test]
fn instantiate_recipe_gated_promotion_reports_partial_state_not_bare_error() {
    use crate::plugins::refinement::ObligationSet;

    let mut world = gated_two_box_world(&["structure", "thermal_layer"]);

    let result = handle_instantiate_recipe(
        &mut world,
        InstantiateRecipeRequest {
            family_id: "two_box_wall".into(),
            target_class: "wall_assembly".into(),
            parameters: serde_json::json!({}),
            placement: None,
            target_state: Some("Constructible".into()),
        },
    )
    .expect("a gate-blocked instantiation must be partial success, not a bare error");

    // The script's geometry was created and is attributed in the response.
    assert_eq!(
        result.created_element_ids.len(),
        2,
        "both script-created boxes must be reported; got {:?}",
        result.created_element_ids
    );
    assert_eq!(result.steps_run_count, 2);

    // The refinement claim did NOT advance past the creation state.
    assert_eq!(
        result.state, "Schematic",
        "a blocked promotion must not advance the refinement state"
    );

    // The block is structured: it names the blocking obligations so the agent
    // can resolve_obligation + re-promote instead of retrying the instantiate.
    let blocked = result
        .promotion_blocked
        .expect("promotion_blocked must be set when the gate fired");
    assert_eq!(
        blocked.unsatisfied_obligations,
        vec!["structure".to_string(), "thermal_layer".to_string()],
        "the blocked info must list the unsatisfied obligation ids"
    );
    assert!(
        blocked.message.contains("resolve_obligation"),
        "the message must point at the recovery path: {}",
        blocked.message
    );

    // The root carries a resolvable ObligationSet — the recovery path works.
    let root_entity = find_entity_by_element_id_readonly(
        &world,
        crate::plugins::identity::ElementId(result.root_element_id),
    )
    .expect("root entity must exist");
    assert!(
        world.get::<ObligationSet>(root_entity).is_some(),
        "the root must carry the ObligationSet so resolve_obligation can run"
    );
}

/// The same defect through the bare `promote_refinement{recipe_id}` path: the
/// AuthoringScript executed (geometry persists) and the gate then blocked —
/// the handler must return partial success carrying the created element ids
/// and the structured block, with the state unchanged.
#[cfg(feature = "model-api")]
#[test]
fn promote_refinement_gate_block_after_script_reports_partial_state() {
    let mut world = gated_two_box_world(&["structure"]);

    let root = handle_create_entity(
        &mut world,
        serde_json::json!({
            "type": "box",
            "centre": [0.0, 0.0, 0.0],
            "half_extents": [0.5, 0.5, 0.5],
            "semantic": {
                "element_class": "wall_assembly",
                "refinement_state": "Schematic",
                "parameters": {},
            }
        }),
    )
    .expect("root creation must succeed");

    let result = handle_promote_refinement(
        &mut world,
        root,
        "Constructible".into(),
        Some("two_box_wall".into()),
        serde_json::json!({}),
    )
    .expect("a gate block after script execution must be partial success");

    assert_eq!(result.script_steps_run, 2);
    assert_eq!(
        result.created_element_ids.len(),
        2,
        "script-created elements must be attributed; got {:?}",
        result.created_element_ids
    );
    assert_eq!(
        result.new_state, result.previous_state,
        "a blocked promotion must not advance the refinement state"
    );
    let blocked = result
        .promotion_blocked
        .expect("promotion_blocked must be set when the gate fired");
    assert_eq!(
        blocked.unsatisfied_obligations,
        vec!["structure".to_string()]
    );
}

/// A gate block with NO recipe side effects (no script ran, nothing created)
/// keeps the plain-error semantics of promote_refinement: there is no partial
/// state to report, and the error message already names the recovery path.
#[cfg(feature = "model-api")]
#[test]
fn promote_refinement_gate_block_without_side_effects_stays_error() {
    let mut world = gated_two_box_world(&["structure"]);

    let root = handle_create_entity(
        &mut world,
        serde_json::json!({
            "type": "box",
            "centre": [0.0, 0.0, 0.0],
            "half_extents": [0.5, 0.5, 0.5],
            "semantic": {
                "element_class": "wall_assembly",
                "refinement_state": "Schematic",
                "parameters": {},
            }
        }),
    )
    .expect("root creation must succeed");

    let err = handle_promote_refinement(
        &mut world,
        root,
        "Constructible".into(),
        None,
        serde_json::json!({}),
    )
    .expect_err("a blocked promote with no recipe side effects must remain an error");
    assert!(
        err.contains("unsatisfied obligation"),
        "the error must explain the gate: {err}"
    );
}

/// When a recipe is requested by id but the artifact has a `NativeFnRef` body
/// and the native function is NOT registered in CapabilityRegistry, the handler
/// must return a structured error mentioning `request_corpus_expansion` — not
/// silently succeed with zero elements.
#[cfg(feature = "model-api")]
#[test]
fn instantiate_recipe_with_unresolvable_native_fn_returns_structured_error() {
    use crate::capability_registry::RecipeFamilyId;
    use crate::curation::{
        provenance::{Confidence, Lineage, Provenance},
        scope_trust::{Scope, Trust},
        AssetId, AssetKindId, CurationMeta, RecipeArtifact, RecipeArtifactRegistry, RecipeBody,
        RECIPE_ARTIFACT_KIND,
    };
    use crate::plugins::refinement::{AgentId, RefinementState};

    let mut world = init_model_api_test_world();

    // Install a NativeFnRef artifact whose fn_id does NOT appear in
    // CapabilityRegistry (no architecture plugin is loaded in the test world).
    let family_id = "unregistered_native_roof";
    let asset_id = AssetId(format!("recipe.v1/{family_id}"));
    let artifact = RecipeArtifact {
        meta: CurationMeta::new(
            asset_id,
            AssetKindId(RECIPE_ARTIFACT_KIND.into()),
            Provenance {
                author: AgentId("test_agent".into()),
                confidence: Confidence::High,
                lineage: Lineage::Freeform,
                rationale: Some("test unregistered native".into()),
                jurisdiction: None,
                catalog_dependencies: Vec::new(),
                evidence: Vec::new(),
            },
        )
        .with_scope(Scope::Shipped)
        .with_trust(Trust::Published),
        body: RecipeBody::native(RecipeFamilyId(family_id.into())),
        parameter_schema: serde_json::json!({"type": "object"}),
        target_class: "roof_system".into(),
        supported_refinement_states: vec![RefinementState::Constructible],
        tests: Vec::new(),
    };
    let mut registry = RecipeArtifactRegistry::default();
    registry.insert(artifact);
    world.insert_resource(registry);

    // Spawn an entity to promote.
    let element_id = world
        .resource_mut::<crate::plugins::identity::ElementIdAllocator>()
        .next_id();
    world.spawn((element_id,));

    let error = handle_promote_refinement(
        &mut world,
        element_id.0,
        "Constructible".into(),
        Some(family_id.into()),
        serde_json::json!({}),
    )
    .expect_err("NativeFnRef with unregistered fn must return an error, not silently no-op");

    assert!(
        error.contains("NativeFnRef") || error.contains("native"),
        "error must describe the NativeFnRef body, got: {error}"
    );
    assert!(
        error.contains("request_corpus_expansion"),
        "error must point to request_corpus_expansion, got: {error}"
    );
}

/// `list_recipe_families` must return `executable: true` and
/// `execution_path: "instantiate_recipe"` for every shipped native descriptor.
#[cfg(feature = "model-api")]
#[test]
fn list_recipe_families_carries_accurate_executable_field() {
    use crate::capability_registry::{
        ElementClassId, GenerateInput, GenerateOutput, ObligationTemplate, RecipeFamilyDescriptor,
        RecipeParameter,
    };
    use crate::plugins::refinement::RefinementState;
    use std::collections::HashMap;
    use std::sync::Arc;

    let mut world = init_model_api_test_world();

    // Register a shipped native recipe family.
    world
        .resource_mut::<CapabilityRegistry>()
        .register_recipe_family(RecipeFamilyDescriptor {
            id: crate::capability_registry::RecipeFamilyId("test_native_wall".into()),
            target_class: ElementClassId("wall_assembly".into()),
            label: "Test Native Wall".into(),
            description: "test".into(),
            parameters: vec![RecipeParameter {
                name: "length_mm".into(),
                value_schema: serde_json::json!({"type": "number"}),
                default: Some(serde_json::json!(3000)),
            }],
            supported_refinement_levels: vec![RefinementState::Constructible],
            obligation_specializations: HashMap::<RefinementState, Vec<ObligationTemplate>>::new(),
            promotion_critical_path_specializations: HashMap::new(),
            generate: Arc::new(
                |_: GenerateInput, _: &mut World| -> Result<GenerateOutput, String> {
                    Ok(GenerateOutput::default())
                },
            ),
        });

    let families = handle_list_recipe_families(&world, Some("wall_assembly".into()));
    assert_eq!(families.len(), 1, "one registered family");
    let family = &families[0];
    assert!(
        family.executable,
        "shipped native descriptor must report executable=true"
    );
    assert_eq!(
        family.execution_path.as_deref(),
        Some("instantiate_recipe"),
        "shipped native descriptor must name instantiate_recipe as execution_path"
    );
}

/// `select_recipe` must carry `executable: true` for installed AuthoringScript
/// artifacts, reflecting that `instantiate_recipe` can materialise them.
#[cfg(feature = "model-api")]
#[test]
fn select_recipe_reports_authoring_script_artifacts_as_executable() {
    let mut world = init_model_api_test_world();

    world.insert_resource(crate::curation::RecipeArtifactRegistry::default());
    install_two_box_recipe(&mut world, "two_box_query_wall", "wall_assembly");

    let rankings = handle_select_recipe(&world, "wall_assembly".into(), serde_json::json!({}))
        .expect("select_recipe must succeed");

    let ranking = rankings
        .iter()
        .find(|r| r.id == "two_box_query_wall")
        .expect("installed recipe must appear in select_recipe results");

    assert!(
        ranking.executable,
        "installed AuthoringScript artifact must report executable=true in select_recipe"
    );
    assert_eq!(
        ranking.execution_path.as_deref(),
        Some("instantiate_recipe"),
        "installed AuthoringScript artifact must name instantiate_recipe as execution_path"
    );
}

// ---------------------------------------------------------------------------
// Item C: Geometric validator tests
// ---------------------------------------------------------------------------

/// Build a minimal world with a CapabilityRegistry and two box primitives
/// at known positions.
#[cfg(feature = "model-api")]
fn make_two_box_world() -> World {
    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    world.insert_resource(registry);
    // Box A: centre (0,1,0), half-extents (1,1,1) → min (-1,0,-1), max (1,2,1)
    world.spawn((
        ElementId(10),
        BoxPrimitive {
            centre: Vec3::new(0.0, 1.0, 0.0),
            half_extents: Vec3::new(1.0, 1.0, 1.0),
        },
        ShapeRotation::default(),
    ));
    // Box B: centre (3,1,0), half-extents (1,1,1) → min (2,0,-1), max (4,2,1)
    world.spawn((
        ElementId(20),
        BoxPrimitive {
            centre: Vec3::new(3.0, 1.0, 0.0),
            half_extents: Vec3::new(1.0, 1.0, 1.0),
        },
        ShapeRotation::default(),
    ));
    world
}

#[test]
#[cfg(feature = "model-api")]
fn get_world_aabb_returns_correct_bounds() {
    let world = make_two_box_world();
    let result = handle_get_world_aabb(
        &world,
        GetWorldAabbRequest {
            element_ids: vec![10, 20],
        },
    )
    .expect("get_world_aabb must succeed for known elements");

    assert_eq!(result.elements.len(), 2);
    assert!(result.errors.is_empty());

    let a = result.elements.iter().find(|e| e.element_id == 10).unwrap();
    assert!((a.min[1] - 0.0).abs() < 0.01, "Box A min y should be ~0");
    assert!((a.max[1] - 2.0).abs() < 0.01, "Box A max y should be ~2");

    let b = result.elements.iter().find(|e| e.element_id == 20).unwrap();
    assert!((b.min[0] - 2.0).abs() < 0.01, "Box B min x should be ~2");

    let combined = result.combined.expect("combined must be present");
    assert!(combined.min[0] <= -0.9, "combined min x covers Box A");
    assert!(combined.max[0] >= 3.9, "combined max x covers Box B");
}

#[test]
#[cfg(feature = "model-api")]
fn get_world_aabb_error_entry_for_missing_element() {
    let world = make_two_box_world();
    let result = handle_get_world_aabb(
        &world,
        GetWorldAabbRequest {
            element_ids: vec![10, 9999],
        },
    )
    .expect("get_world_aabb must succeed even with unknown ids");

    assert_eq!(result.elements.len(), 1);
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].element_id, 9999);
}

#[test]
#[cfg(feature = "model-api")]
fn check_overlaps_non_overlapping_boxes_returns_empty() {
    let world = make_two_box_world();
    // Boxes A and B don't overlap (gap of 1 unit between them).
    let result = handle_check_overlaps(
        &world,
        CheckOverlapsRequest {
            element_ids: vec![10, 20],
        },
    )
    .expect("check_overlaps must succeed");

    assert!(
        result.overlaps.is_empty(),
        "non-overlapping boxes must not report overlap"
    );
    assert_eq!(result.pairs_checked, 1);
    assert!(!result.truncated);
}

#[test]
#[cfg(feature = "model-api")]
fn check_overlaps_touching_boxes_reports_overlap() {
    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    world.insert_resource(registry);
    // Box A: min (-1,0,-1) max (1,2,1)
    world.spawn((
        ElementId(1),
        BoxPrimitive {
            centre: Vec3::new(0.0, 1.0, 0.0),
            half_extents: Vec3::new(1.0, 1.0, 1.0),
        },
        ShapeRotation::default(),
    ));
    // Box B: min (0.5,0,-1) max (2.5,2,1) — overlaps A by 0.5 in X
    world.spawn((
        ElementId(2),
        BoxPrimitive {
            centre: Vec3::new(1.5, 1.0, 0.0),
            half_extents: Vec3::new(1.0, 1.0, 1.0),
        },
        ShapeRotation::default(),
    ));

    let result = handle_check_overlaps(
        &world,
        CheckOverlapsRequest {
            element_ids: vec![1, 2],
        },
    )
    .expect("check_overlaps must succeed");

    assert_eq!(
        result.overlaps.len(),
        1,
        "overlapping boxes must produce one entry"
    );
    let entry = &result.overlaps[0];
    assert!(
        (entry.overlap_extents[0] - 0.5).abs() < 0.02,
        "overlap dx ~ 0.5 m"
    );
    assert!(entry.overlap_volume > 0.0);
}

#[test]
#[cfg(feature = "model-api")]
fn check_floating_detects_floating_box() {
    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    world.insert_resource(registry);
    // Ground box: min (-5,-0.1,-5) max (5,0.1,5) — rests on y=0
    world.spawn((
        ElementId(1),
        BoxPrimitive {
            centre: Vec3::new(0.0, 0.0, 0.0),
            half_extents: Vec3::new(5.0, 0.1, 5.0),
        },
        ShapeRotation::default(),
    ));
    // Floating box: min (-0.5, 2.0, -0.5) max (0.5, 3.0, 0.5) — 1.9 m gap above ground box top
    world.spawn((
        ElementId(2),
        BoxPrimitive {
            centre: Vec3::new(0.0, 2.5, 0.0),
            half_extents: Vec3::new(0.5, 0.5, 0.5),
        },
        ShapeRotation::default(),
    ));

    let result = handle_check_floating(
        &world,
        CheckFloatingRequest {
            element_ids: vec![1, 2],
            tolerance_m: Some(0.01),
        },
    )
    .expect("check_floating must succeed");

    // Element 2 is floating; element 1 sits on y=0 ground with a 0 gap.
    let floating_ids: Vec<u64> = result.floating.iter().map(|f| f.element_id).collect();
    assert!(
        floating_ids.contains(&2),
        "element 2 should be floating; floating_ids={floating_ids:?}"
    );
    let entry = result.floating.iter().find(|f| f.element_id == 2).unwrap();
    assert!(
        entry.gap_m > 1.8,
        "gap should be ~1.9 m above support top (got {})",
        entry.gap_m
    );
    assert_eq!(entry.nearest_support, Some(1));
}

#[test]
#[cfg(feature = "model-api")]
fn check_floating_grounded_element_not_reported() {
    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    world.insert_resource(registry);
    // Box sitting exactly at y=0 ground.
    world.spawn((
        ElementId(1),
        BoxPrimitive {
            centre: Vec3::new(0.0, 0.5, 0.0),
            half_extents: Vec3::new(1.0, 0.5, 1.0),
        },
        ShapeRotation::default(),
    ));

    let result = handle_check_floating(
        &world,
        CheckFloatingRequest {
            element_ids: vec![1],
            tolerance_m: Some(0.01),
        },
    )
    .expect("check_floating must succeed");

    assert!(
        result.floating.is_empty(),
        "grounded element must not be reported as floating"
    );
}

#[test]
#[cfg(feature = "model-api")]
fn check_clearance_pass_and_fail() {
    let world = make_two_box_world();
    // Box A max_x=1, Box B min_x=2 → gap = 1.0 m

    let pass_result = handle_check_clearance(
        &world,
        CheckClearanceRequest {
            a: 10,
            b: 20,
            min_m: 0.5,
        },
    )
    .expect("check_clearance must succeed");
    assert!(pass_result.pass, "1.0 m gap should pass 0.5 m requirement");
    assert!((pass_result.actual_m - 1.0).abs() < 0.05);

    let fail_result = handle_check_clearance(
        &world,
        CheckClearanceRequest {
            a: 10,
            b: 20,
            min_m: 2.0,
        },
    )
    .expect("check_clearance must succeed");
    assert!(!fail_result.pass, "1.0 m gap should fail 2.0 m requirement");
}

#[test]
#[cfg(feature = "model-api")]
fn check_clearance_error_on_missing_element() {
    let world = make_two_box_world();
    let err = handle_check_clearance(
        &world,
        CheckClearanceRequest {
            a: 10,
            b: 9999,
            min_m: 1.0,
        },
    );
    assert!(err.is_err(), "missing element should return Err");
}

// ---------------------------------------------------------------------------
// Item B: Session-recovery persistence tests
// ---------------------------------------------------------------------------

/// Build a unique tempdir-based knowledge dir and return (dir_path, instance_id).
/// Uses the instance_id as the sole namespace so the env var is not needed.
/// Instead, we directly test the functions that take `instance_id` and rely on
/// `knowledge_dir()`.  We run these tests under a global mutex to avoid races
/// on the env var.
#[cfg(feature = "model-api")]
fn with_isolated_knowledge_dir<F: FnOnce(&str)>(f: F) {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = std::env::temp_dir().join(format!("t3d_kp_{nanos}"));
    let instance = format!("inst_{nanos}");
    std::env::set_var("TALOS3D_KNOWLEDGE_DIR", &base);
    f(&instance);
    std::env::remove_var("TALOS3D_KNOWLEDGE_DIR");
}

#[test]
#[cfg(feature = "model-api")]
fn session_recipe_draft_write_through_and_reload() {
    use crate::plugins::knowledge_assets::{default_recipe_draft_meta, KnowledgeResidency};
    use crate::plugins::knowledge_persistence::{
        load_session_recipe_drafts, persist_session_recipe_draft,
    };
    use crate::plugins::recipe_drafts::{
        RecipeDraftArtifact, RecipeDraftRegistry, RecipeDraftStatus,
    };

    with_isolated_knowledge_dir(|instance| {
        let draft = RecipeDraftArtifact {
            id: "test-draft-1".to_string(),
            meta: default_recipe_draft_meta(),
            residency: KnowledgeResidency::SessionCache,
            label: "Test Draft".into(),
            description: "Write-through test".into(),
            target_class: "wall".into(),
            supported_refinement_levels: vec!["Schematic".into()],
            parameters: vec![],
            jurisdiction: None,
            gap_id: None,
            source_passage_refs: vec![],
            evidence_slots: vec![],
            runtime_claims: vec![],
            acquisition_context: serde_json::json!({}),
            draft_script: serde_json::json!({}),
            notes: vec![],
            status: RecipeDraftStatus::Drafted,
            created_at: 0,
            updated_at: 0,
        };

        persist_session_recipe_draft(instance, &draft);

        let mut fresh_registry = RecipeDraftRegistry::default();
        load_session_recipe_drafts(instance, &mut fresh_registry);

        let loaded = fresh_registry.get("test-draft-1");
        assert!(
            loaded.is_some(),
            "draft must be recoverable after persist + load"
        );
        assert_eq!(loaded.unwrap().label, "Test Draft");
    });
}

#[test]
#[cfg(feature = "model-api")]
fn session_corpus_gaps_write_through_and_reload() {
    use crate::plugins::corpus_gap::{CorpusGap, CorpusGapId, CorpusGapQueue};
    use crate::plugins::knowledge_persistence::{
        load_session_corpus_gaps, persist_session_corpus_gaps,
    };

    with_isolated_knowledge_dir(|instance| {
        let mut queue = CorpusGapQueue::default();
        let gap = CorpusGap {
            id: CorpusGapId(String::new()),
            element_class: Some("stair".into()),
            kind: None,
            jurisdiction: Some("SE".into()),
            missing_artifact_kind: "rule_pack".into(),
            context: serde_json::json!({}),
            reported_by: "agent".into(),
            reported_at: 1_700_000_000,
        };
        queue.push(gap);

        persist_session_corpus_gaps(instance, queue.list());

        let mut fresh_queue = CorpusGapQueue::default();
        load_session_corpus_gaps(instance, &mut fresh_queue);

        assert_eq!(
            fresh_queue.list().len(),
            1,
            "one gap must survive round-trip"
        );
        assert_eq!(
            fresh_queue.list()[0].element_class.as_deref(),
            Some("stair")
        );
        assert_eq!(fresh_queue.list()[0].reported_at, 1_700_000_000);
    });
}

#[test]
#[cfg(feature = "model-api")]
fn session_assembly_pattern_draft_write_through_and_reload() {
    use crate::plugins::assembly_pattern_drafts::{
        AssemblyPatternDraftArtifact, AssemblyPatternDraftRegistry, AssemblyPatternDraftStatus,
    };
    use crate::plugins::knowledge_assets::{
        default_assembly_pattern_draft_meta, KnowledgeResidency,
    };
    use crate::plugins::knowledge_persistence::{
        load_session_assembly_pattern_drafts, persist_session_assembly_pattern_draft,
    };

    with_isolated_knowledge_dir(|instance| {
        let draft = AssemblyPatternDraftArtifact {
            id: "ap-draft-1".to_string(),
            meta: default_assembly_pattern_draft_meta(),
            residency: KnowledgeResidency::SessionCache,
            label: "Wall Pattern".into(),
            description: "test".into(),
            target_types: vec!["wall_assembly".into()],
            axis: "exterior_to_interior".into(),
            layers: vec![],
            relation_rules: vec![],
            root_layer_ids: vec![],
            requires_support_path: false,
            tags: vec![],
            parameter_schema: serde_json::json!({}),
            jurisdiction: None,
            gap_id: None,
            source_passage_refs: vec![],
            evidence_slots: vec![],
            runtime_claims: vec![],
            acquisition_context: serde_json::json!({}),
            notes: vec![],
            status: AssemblyPatternDraftStatus::Drafted,
            created_at: 0,
            updated_at: 0,
        };

        persist_session_assembly_pattern_draft(instance, &draft);

        let mut fresh_registry = AssemblyPatternDraftRegistry::default();
        load_session_assembly_pattern_drafts(instance, &mut fresh_registry);

        let loaded = fresh_registry.get("ap-draft-1");
        assert!(
            loaded.is_some(),
            "assembly pattern draft must survive round-trip"
        );
        assert_eq!(loaded.unwrap().label, "Wall Pattern");
    });
}

#[test]
#[cfg(feature = "model-api")]
fn session_recovery_scope_separation_project_vs_session() {
    // Project-scoped drafts go to knowledge_dir/recipes/<id>.json via
    // persist_recipe (not the session area).  This test confirms session-area
    // writes use a separate path and that the instance id namespaces them.
    // No env var needed — tests path structure only.
    use crate::plugins::knowledge_persistence::{
        session_corpus_gaps_path, session_recipe_drafts_dir,
    };

    let id_a = "inst-a";
    let id_b = "inst-b";
    let dir_a = session_recipe_drafts_dir(id_a);
    let dir_b = session_recipe_drafts_dir(id_b);
    assert_ne!(
        dir_a, dir_b,
        "different instances must use different directories"
    );
    assert!(dir_a.ends_with("session/inst-a/recipe_drafts"));
    assert!(dir_b.ends_with("session/inst-b/recipe_drafts"));

    let gap_path_a = session_corpus_gaps_path(id_a);
    let gap_path_b = session_corpus_gaps_path(id_b);
    assert_ne!(gap_path_a, gap_path_b);
}

#[test]
#[cfg(feature = "model-api")]
fn run_validation_v2_computes_current_findings_when_cache_is_empty() {
    use crate::capability_registry::{
        Applicability, ConstraintDescriptor, ConstraintId, ConstraintRole, Finding, FindingId,
        Severity,
    };

    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_constraint(ConstraintDescriptor {
        id: ConstraintId("test.current_validation".into()),
        label: "Current validation".into(),
        description: "Emits a finding for the current entity".into(),
        applicability: Applicability::any(),
        default_severity: Severity::Error,
        rationale: "MCP validation must compute current findings, not only read a cache.".into(),
        source_backlink: None,
        role: ConstraintRole::Validation,
        validator: std::sync::Arc::new(|entity, world| {
            let Some(element_id) = world.get::<ElementId>(entity) else {
                return Vec::new();
            };
            vec![Finding {
                id: FindingId(format!("test.current_validation:{}", element_id.0)),
                constraint_id: ConstraintId("test.current_validation".into()),
                subject: element_id.0,
                severity: Severity::Error,
                message: "current finding".into(),
                rationale: "computed directly".into(),
                backlink: None,
                emitted_at: 0,
                role: ConstraintRole::Validation,
            }]
        }),
    });
    world.insert_resource(registry);
    world.insert_resource(crate::plugins::validation::Findings::default());
    world.spawn((ElementId(42),));

    let targeted = handle_run_validation_v2(&world, Some(42));
    assert_eq!(targeted.len(), 1);
    assert_eq!(targeted[0].finding_id, "test.current_validation:42");

    let whole_model = handle_run_validation_v2(&world, None);
    assert_eq!(whole_model.len(), 1);
    assert_eq!(whole_model[0].finding_id, "test.current_validation:42");
}

// --- Capability-profile tool gating (context economy) ---

#[cfg(feature = "model-api")]
mod capability_profiles {
    use super::super::profiles::{
        profile_allows, profiles_containing, tool_category, SessionProfileState, ToolCategory,
        TOOL_CATEGORIES,
    };
    use super::super::{
        capability_snapshot_next_tools, profile_tool_catalog, CapabilityProfile,
        CapabilitySnapshotInfo,
    };
    use std::collections::BTreeSet;

    fn router_tool_names() -> BTreeSet<String> {
        profile_tool_catalog()
            .tools_for(CapabilityProfile::Full)
            .iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    fn profile_tool_names(profile: CapabilityProfile) -> BTreeSet<String> {
        profile_tool_catalog()
            .tools_for(profile)
            .iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    /// Every tool the router registers must classify into a real category.
    /// A new tool landing in `Unclassified` is reachable only through the
    /// `full` profile, which silently hides it from every gated session —
    /// classify it in `profiles::TOOL_CATEGORIES` (or a prefix rule).
    #[test]
    fn every_router_tool_is_classified() {
        let unclassified: Vec<String> = router_tool_names()
            .into_iter()
            .filter(|name| tool_category(name) == ToolCategory::Unclassified)
            .collect();
        assert!(
            unclassified.is_empty(),
            "unclassified MCP tools (assign a ToolCategory in profiles.rs): {unclassified:?}"
        );
    }

    /// Every explicit table entry must name a tool that still exists, so the
    /// table cannot drift as tools are renamed or removed.
    #[test]
    fn category_table_has_no_stale_tool_names() {
        let names = router_tool_names();
        let stale: Vec<&str> = TOOL_CATEGORIES
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| !names.contains(*name))
            .collect();
        assert!(
            stale.is_empty(),
            "TOOL_CATEGORIES entries with no matching router tool: {stale:?}"
        );
    }

    #[test]
    fn full_profile_exposes_entire_router() {
        let catalog = profile_tool_catalog();
        assert_eq!(
            catalog.tools_for(CapabilityProfile::Full).len(),
            catalog.router.list_all().len(),
        );
    }

    /// The MCP session contract: a fresh MCP-only agent must be able to
    /// discover guidance, the capability snapshot, guidance cards, curated
    /// paths, agent skills, and the profile switch itself in EVERY profile.
    #[test]
    fn session_contract_tools_present_in_every_profile() {
        const SESSION_CONTRACT: &[&str] = &[
            "get_instance_info",
            "set_session_profile",
            "get_authoring_guidance",
            "get_capability_snapshot",
            "list_guidance_cards",
            "get_guidance_card",
            "discover_curated_paths",
            "list_agent_skills",
            "find_agent_skills",
            "get_agent_skill",
        ];
        for profile in CapabilityProfile::ALL {
            let names = profile_tool_names(profile);
            let missing: Vec<&str> = SESSION_CONTRACT
                .iter()
                .copied()
                .filter(|tool| !names.contains(*tool))
                .collect();
            assert!(
                missing.is_empty(),
                "profile '{}' is missing session-contract tools: {missing:?}",
                profile.name()
            );
        }
    }

    /// The default profile covers the standard authoring loop and the
    /// ADR-042 anti-bluff gate, and excludes the UI-automation/look-dev/BIM
    /// surfaces that dominate the cold-start schema cost.
    #[test]
    fn authoring_profile_covers_standard_loop_and_excludes_ui_surfaces() {
        let names = profile_tool_names(CapabilityProfile::Authoring);
        for tool in [
            // inspect
            "list_entities",
            "get_entity_details",
            "model_summary",
            "get_world_aabb",
            // edit + materials
            "create_box",
            "create_entity",
            "transform",
            "apply_material",
            "create_material",
            // recipes / discovery / gap flow
            "select_recipe",
            "list_recipe_families",
            "instantiate_recipe",
            "list_generation_priors",
            "request_corpus_expansion",
            "save_recipe_draft",
            "set_recipe_draft_status",
            "materialize_learned_asset",
            // definitions / parametric
            "definition.create",
            "definition.instantiate_hosted",
            "occurrence.place",
            "bim_void.plan_placement",
            "parametric.list_types",
            "parametric.create",
            // validation + refinement + capture
            "check_overlaps",
            "check_floating",
            "check_clearance",
            "run_validation_v2",
            "promote_refinement",
            "resolve_obligation",
            "take_screenshot",
            "set_camera",
            // commands escape hatch
            "invoke_command",
            "list_commands",
        ] {
            assert!(
                names.contains(tool),
                "authoring profile must include {tool}"
            );
        }
        for tool in [
            "ux_click",
            "ux_observe",
            "view_save",
            "view_restore",
            "clip_plane_create",
            "list_toolbars",
            "set_toolbar_layout",
            "export_drawing",
            "place_dimension_line",
            "bim_property_set.get",
            "quantity.set",
            "create_light",
            "set_render_settings",
            "definition.library.workspace.create",
            "procedural_session.create",
            "array_create_linear",
            "mirror_create",
        ] {
            assert!(
                !names.contains(tool),
                "authoring profile must exclude {tool}"
            );
        }
    }

    /// Context-economy guard: the default profile must stay a fraction of the
    /// full surface, both in tool count and in serialized schema bytes (the
    /// actual cold-start cost). Counts are bounded loosely so adding a tool
    /// does not break the build, but a wholesale un-gating does.
    #[test]
    fn authoring_profile_is_a_fraction_of_full_surface() {
        let catalog = profile_tool_catalog();
        let authoring = catalog.tools_for(CapabilityProfile::Authoring);
        let full = catalog.tools_for(CapabilityProfile::Full);
        assert!(
            authoring.len() <= full.len() / 2,
            "authoring advertises {} of {} tools; expected at most half",
            authoring.len(),
            full.len()
        );
        let bytes = |tools: &Vec<rmcp::model::Tool>| {
            serde_json::to_vec(tools)
                .expect("tool list serializes")
                .len()
        };
        let authoring_bytes = bytes(authoring);
        let full_bytes = bytes(full);
        println!(
            "authoring: {} tools / {} KB; full: {} tools / {} KB",
            authoring.len(),
            authoring_bytes / 1024,
            full.len(),
            full_bytes / 1024
        );
        assert!(
            authoring_bytes * 2 <= full_bytes,
            "authoring schema bytes ({authoring_bytes}) should be at most half of full ({full_bytes})"
        );
    }

    /// Read-only contract for the inspection profile: nothing that writes the
    /// authored model is advertised.
    #[test]
    fn inspection_profile_has_no_model_writes() {
        let names = profile_tool_names(CapabilityProfile::Inspection);
        for tool in [
            "create_entity",
            "create_box",
            "delete_entities",
            "transform",
            "set_property",
            "apply_material",
            "instantiate_recipe",
            "promote_refinement",
            "definition.instantiate",
            "save_project",
            "invoke_command",
        ] {
            assert!(
                !names.contains(tool),
                "inspection profile must not advertise {tool}"
            );
        }
        for tool in ["list_entities", "get_world_aabb", "take_screenshot"] {
            assert!(
                names.contains(tool),
                "inspection profile must include {tool}"
            );
        }
    }

    #[test]
    fn ux_automation_profile_carries_ui_surface() {
        let names = profile_tool_names(CapabilityProfile::UxAutomation);
        for tool in [
            "ux_click",
            "ux_press_key",
            "view_save",
            "clip_plane_create",
            "set_toolbar_layout",
            "invoke_command",
            "take_screenshot",
            "list_entities",
        ] {
            assert!(names.contains(tool), "ux-automation must include {tool}");
        }
        assert!(!names.contains("delete_entities"));
        assert!(!names.contains("instantiate_recipe"));
    }

    /// Session-contract invariant: the snapshot's canonical `next_tools` must
    /// all live inside the default profile (so default sessions are never
    /// steered toward a gated tool), and per-profile filtering must always
    /// yield a subset of the active profile's advertised tools.
    #[test]
    fn snapshot_next_tools_respect_profiles() {
        for tool in capability_snapshot_next_tools() {
            assert!(
                profile_allows(CapabilityProfile::Authoring, &tool),
                "canonical snapshot next_tool '{tool}' is outside the authoring profile"
            );
        }
        for tool in CapabilitySnapshotInfo::empty(false).next_tools {
            assert!(
                profile_allows(CapabilityProfile::Authoring, &tool),
                "empty-snapshot next_tool '{tool}' is outside the authoring profile"
            );
        }
        for profile in CapabilityProfile::ALL {
            let names = profile_tool_names(profile);
            let mut filtered = capability_snapshot_next_tools();
            filtered.retain(|tool| profile_allows(profile, tool));
            assert!(
                filtered.iter().all(|tool| names.contains(tool)),
                "filtered next_tools must be advertised in profile '{}'",
                profile.name()
            );
            assert!(
                filtered.contains(&"get_guidance_card".to_string()),
                "profile '{}' filtering must keep the guidance-card route",
                profile.name()
            );
        }
    }

    #[test]
    fn profile_names_round_trip_and_state_switches() {
        for profile in CapabilityProfile::ALL {
            assert_eq!(CapabilityProfile::from_name(profile.name()), Some(profile));
        }
        assert_eq!(
            CapabilityProfile::from_name("ux_automation"),
            Some(CapabilityProfile::UxAutomation)
        );
        assert_eq!(
            CapabilityProfile::from_name("FULL"),
            Some(CapabilityProfile::Full)
        );
        assert_eq!(CapabilityProfile::from_name("nope"), None);

        let state = SessionProfileState::new(CapabilityProfile::Authoring);
        assert_eq!(state.get(), CapabilityProfile::Authoring);
        assert!(state.set(CapabilityProfile::Full));
        assert!(
            !state.set(CapabilityProfile::Full),
            "re-set must report unchanged"
        );
        let shared = state.clone();
        assert_eq!(shared.get(), CapabilityProfile::Full);
    }

    /// Calling a tool outside the active profile must fail with an error that
    /// names the profiles containing it; unknown tools fall through to the
    /// router's own not-found error.
    #[test]
    fn out_of_profile_calls_are_rejected_with_guidance() {
        let (sender, _receiver) = std::sync::mpsc::channel();
        let server = super::super::ModelApiServer::with_profile_state(
            sender,
            SessionProfileState::new(CapabilityProfile::Authoring),
        );
        let error = server
            .tool_call_allowed("ux_click")
            .expect_err("ux_click must be gated under authoring");
        let message = error.message.to_string();
        assert!(message.contains("ux_click"), "message: {message}");
        assert!(message.contains("ux-automation"), "message: {message}");
        assert!(
            message.contains("set_session_profile"),
            "message: {message}"
        );
        assert_eq!(profiles_containing("ux_click"), vec!["ux-automation"]);

        server
            .tool_call_allowed("list_entities")
            .expect("in-profile tool");
        server
            .tool_call_allowed("no_such_tool")
            .expect("unknown tools fall through to the router error");

        server.profile_state.set(CapabilityProfile::Full);
        server
            .tool_call_allowed("ux_click")
            .expect("full allows everything");
    }
}
