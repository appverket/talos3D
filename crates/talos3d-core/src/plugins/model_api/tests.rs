use super::*;
use crate::capability_registry::CapabilityRegistry;
#[cfg(feature = "model-api")]
use crate::importers::obj::ObjImporter;
#[cfg(feature = "model-api")]
use crate::plugins::command_registry::{execute_command, CommandRegistry};
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
    assert!(snapshot.estimated_json_bytes <= snapshot.size_budget_bytes);
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
    let saved_path = handle_take_screenshot(&mut world, screenshot_path.to_str().unwrap())
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
    let mut world = init_model_api_test_world();

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
    assert!(tool_names.contains("place_dimension_between_handles"));
    assert!(tool_names.contains("get_camera"));
    assert!(tool_names.contains("set_camera"));
    assert!(tool_names.contains("take_screenshot"));
    assert!(tool_names.contains("export_drawing"));

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
            paper_fill_enabled: Some(true),
            ..Default::default()
        },
    )
    .expect("render settings update should succeed");

    assert_eq!(updated.tonemapping, "blender_filmic");
    assert_eq!(updated.exposure_ev100, 1.5);
    assert!(!updated.ssao_enabled);
    assert!(updated.ssr_enabled);
    assert_eq!(updated.ssr_linear_steps, 24);
    assert!(updated.wireframe_overlay_enabled);
    assert!(updated.contour_overlay_enabled);
    assert!(updated.visible_edge_overlay_enabled);
    assert!(!updated.grid_enabled);
    assert_eq!(updated.background_rgb, [1.0, 1.0, 1.0]);
    assert!(updated.paper_fill_enabled);

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
    );
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
    );
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
fn no_curated_path_discovery_and_guidance_cards_are_explicit() {
    let world = init_model_api_test_world();
    let discovery = handle_discover_curated_paths(
        &world,
        CuratedPathDiscoveryRequest {
            path_kind: Some("recipe".into()),
            element_class: Some("roof_system".into()),
            context: serde_json::json!({ "target_state": "Constructible" }),
        },
    )
    .expect("discovery should succeed");
    let gap = discovery
        .no_curated_path
        .expect("missing recipe path should be first-class");
    assert_eq!(gap.suggested_next_tool, "request_corpus_expansion");
    assert!(discovery
        .guidance_card_ids
        .contains(&"dkg.no_curated_path".to_string()));

    let known_tools = std::collections::BTreeSet::from([
        "get_capability_snapshot",
        "discover_curated_paths",
        "request_corpus_expansion",
        "save_recipe_draft",
        "save_assembly_pattern_draft",
        "parametric.create",
        "take_screenshot",
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
    );

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

    assert!(error.contains("consultable but not executable yet"));
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
    );

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
