use super::*;
use crate::plugins::{
    authored_edit_plan::{queue_captured_plan, requests},
    foreign_source::{self, GetForeignSourcesRequest, SourceArtifacts},
};
const OBJ: &[u8] = include_bytes!("../../../../tests/fixtures/foreign_source/two-blocks.obj");
fn request(bytes: &[u8], format: &str) -> Value {
    json!({"metadata":{"name":"two-blocks","format":format,"format_release":"OBJ text","producer":"Talos3D test fixture","producer_version":"1","license":"CC0","provenance":"Synthetic input authored for regression testing"},"bytes":bytes,
        "placement":{"length_unit":"millimetre","frame":"z_up_right_handed","world_translation_m":[10.,2.,3.],"world_rotation_xyzw":[0.,0.,0.,1.]}})
}
fn fixture() -> World {
    let mut world = init_model_api_test_world();
    register_model_api_edit_requests(&mut world);
    world
}
fn preview(
    world: &mut World,
    value: Value,
) -> std::sync::Arc<crate::plugins::authored_edit_plan::AuthoredEditPlan> {
    requests::preview(world, "core.foreign_reference", value).unwrap()
}
fn apply(world: &mut World, plan: &crate::plugins::authored_edit_plan::AuthoredEditPlan) {
    queue_captured_plan(world, plan.plan_id()).unwrap();
    flush_model_api_write_pipeline(world);
}
fn sources(world: &World) -> Value {
    foreign_source::inspect_sources(
        world,
        GetForeignSourcesRequest {
            digest: None,
            include_bytes: false,
        },
    )
    .unwrap()
}
fn native_request(reference: u64, definition: &str) -> Value {
    json!({"reference_element_id":reference,"definition_id":definition,"overrides":{"width":1.,"depth":1.,"height":1.},"label":"Interpreted block","world_translation_m":[10.5,2.5,2.5],"world_rotation_xyzw":[0.,0.,0.,1.],"rationale":"Explicit new rectangular block interpretation of the synthetic reference","uncertainty":["No construction meaning or original feature history established"]})
}
#[test]
fn retained_bytes_units_frames_preview_and_atomic_history() {
    let mut world = fixture();
    let revision = world.resource::<History>().revision_token();
    let next = world.resource::<ElementIdAllocator>().next_value();
    let plan = preview(&mut world, request(OBJ, "obj"));
    assert_eq!(world.resource::<History>().revision_token(), revision);
    assert_eq!(world.resource::<ElementIdAllocator>().next_value(), next);
    assert!(world.resource::<SourceArtifacts>().0.is_empty());
    assert_eq!(plan.created_ids().len(), 2);
    let manifest = &plan.content()["source_artifact_changes"][0]["after"];
    assert_eq!(manifest["byte_count"], OBJ.len());
    assert!(
        manifest.get("bytes").is_none(),
        "preview must not dump retained input"
    );
    apply(&mut world, &plan);
    let artifact = world
        .resource::<SourceArtifacts>()
        .0
        .values()
        .next()
        .unwrap();
    assert_eq!(artifact.bytes, OBJ);
    assert_eq!(artifact.digest, blake3::hash(OBJ).to_hex().as_str());
    let digest = artifact.digest.clone();
    let id = plan.created_ids()[0];
    let entity = find_entity_by_element_id_readonly(&world, id).unwrap();
    let mesh = world
        .get::<crate::plugins::modeling::primitives::TriangleMesh>(entity)
        .unwrap();
    let min = mesh
        .vertices
        .iter()
        .fold(Vec3::splat(f32::INFINITY), |a, b| a.min(*b));
    let max = mesh
        .vertices
        .iter()
        .fold(Vec3::splat(f32::NEG_INFINITY), |a, b| a.max(*b));
    assert!(min.abs_diff_eq(Vec3::new(10., 2., 2.), 1e-5));
    assert!(max.abs_diff_eq(Vec3::new(11., 3., 3.), 1e-5));
    let exact = foreign_source::inspect_sources(
        &world,
        GetForeignSourcesRequest {
            digest: Some(digest),
            include_bytes: true,
        },
    )
    .unwrap();
    assert_eq!(exact["sources"][0]["bytes"], json!(OBJ));
    assert!(queue_captured_plan(&mut world, plan.plan_id()).is_err());
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    flush_model_api_write_pipeline(&mut world);
    assert!(world.resource::<SourceArtifacts>().0.is_empty());
    assert!(get_entity_snapshot(&world, id).is_none());
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(
        world
            .resource::<SourceArtifacts>()
            .0
            .values()
            .next()
            .unwrap()
            .bytes,
        OBJ
    );
    assert!(get_entity_snapshot(&world, id).is_some());
}
#[test]
fn failed_and_unknown_adapters_retain_source_without_partial_geometry() {
    for (bytes, format, reason) in [
        (&b"opaque binary\0\xff"[..], "unknown", "no_adapter"),
        (
            &b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\no bad\nf 999 2 3\n"[..],
            "obj",
            "adapter_failed",
        ),
    ] {
        let mut world = fixture();
        let plan = preview(&mut world, request(bytes, format));
        assert!(plan.created_ids().is_empty());
        assert!(plan.content().to_string().contains(reason));
        apply(&mut world, &plan);
        assert_eq!(
            world
                .resource::<SourceArtifacts>()
                .0
                .values()
                .next()
                .unwrap()
                .bytes,
            bytes
        );
        let path = temp_json_path("foreign-source-only").with_extension("talos3d");
        handle_save_project(&mut world, path.to_str().unwrap()).unwrap();
        let mut loaded = fixture();
        handle_load_project(&mut loaded, path.to_str().unwrap()).unwrap();
        assert_eq!(sources(&world), sources(&loaded));
        assert_eq!(
            world.resource::<SourceArtifacts>(),
            loaded.resource::<SourceArtifacts>()
        );
        fs::remove_file(path).unwrap();
    }
}
#[test]
fn native_partial_acceptance_preserves_other_part_source_and_uncertainty_across_reload() {
    let mut world = fixture();
    let definition = handle_create_definition(&mut world, make_rect_extrusion_request())
        .unwrap()
        .definition_id;
    let reference = preview(&mut world, request(OBJ, "obj"));
    apply(&mut world, &reference);
    let first = reference.created_ids()[0];
    let other = reference.created_ids()[1];
    let other_before = get_entity_snapshot(&world, other).unwrap();
    let first_before = get_entity_snapshot(&world, first).unwrap();
    let source_before = sources(&world);
    let candidate = requests::preview(
        &mut world,
        "core.foreign_native_mapping",
        native_request(first.0, &definition),
    )
    .unwrap();
    assert_eq!(get_entity_snapshot(&world, first).unwrap(), first_before);
    assert_eq!(sources(&world), source_before);
    assert_eq!(candidate.removed_ids(), &[first]);
    apply(&mut world, &candidate);
    let native = candidate.created_ids()[0];
    assert!(get_entity_snapshot(&world, first).is_none());
    assert_eq!(get_entity_snapshot(&world, other).unwrap(), other_before);
    assert_eq!(sources(&world), source_before);
    let details = get_entity_snapshot(&world, native).unwrap();
    let explanation = serde_json::to_value(
        crate::plugins::design_explanation::explain_design(&world, native.0).unwrap(),
    )
    .unwrap();
    assert!(explanation.to_string().contains("No construction meaning"));
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(get_entity_snapshot(&world, first).unwrap(), first_before);
    assert!(get_entity_snapshot(&world, native).is_none());
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(get_entity_snapshot(&world, native).unwrap(), details);
    let stale = requests::preview(
        &mut world,
        "core.foreign_native_mapping",
        native_request(other.0, &definition),
    )
    .unwrap();
    let path = temp_json_path("foreign-native-round-trip").with_extension("talos3d");
    handle_save_project(&mut world, path.to_str().unwrap()).unwrap();
    handle_load_project(&mut world, path.to_str().unwrap()).unwrap();
    assert!(queue_captured_plan(&mut world, stale.plan_id()).is_err());
    assert_eq!(sources(&world), source_before);
    assert_eq!(get_entity_snapshot(&world, native).unwrap(), details);
    assert_eq!(get_entity_snapshot(&world, other).unwrap(), other_before);
    handle_update_occurrence_overrides(&mut world, native.0, json!({"width":2.})).unwrap();
    assert_eq!(
        handle_resolve_occurrence(&world, native.0).unwrap()["width"]["value"],
        2.
    );
    assert!(serde_json::to_value(
        crate::plugins::design_explanation::explain_design(&world, native.0).unwrap()
    )
    .unwrap()
    .to_string()
    .contains("No construction meaning"));
    fs::remove_file(path).unwrap();
}
#[test]
fn refusal_rejection_and_stale_preview_do_not_mutate_or_consume_ids() {
    let mut world = fixture();
    let next = world.resource::<ElementIdAllocator>().next_value();
    let rejected = preview(&mut world, request(OBJ, "obj"));
    drop(rejected);
    assert_eq!(world.resource::<ElementIdAllocator>().next_value(), next);
    assert!(world.resource::<SourceArtifacts>().0.is_empty());
    let mut invalid = request(OBJ, "obj");
    invalid["placement"]["length_unit"] = json!("unknown");
    assert!(requests::preview(&mut world, "core.foreign_reference", invalid).is_err());
    let mut invalid = request(OBJ, "obj");
    invalid["placement"]["world_rotation_xyzw"] = json!([0., 0., 0., 0.]);
    assert!(requests::preview(&mut world, "core.foreign_reference", invalid).is_err());
    let mut invalid = request(OBJ, "obj");
    invalid["bytes"] = json!(vec![42; 65537]);
    assert!(requests::preview(&mut world, "core.foreign_reference", invalid).is_err());
    let plan = preview(&mut world, request(OBJ, "obj"));
    handle_create_entity(
        &mut world,
        json!({"type":"box","centre":[8.,8.,8.],"half_extents":[1.,1.,1.]}),
    )
    .unwrap();
    assert!(queue_captured_plan(&mut world, plan.plan_id()).is_err());
    assert!(world.resource::<SourceArtifacts>().0.is_empty());
}
#[test]
fn immutable_sources_refuse_conflicting_provenance_and_check_legacy_resource_writers() {
    let mut world = fixture();
    let plan = preview(&mut world, request(OBJ, "obj"));
    // A legacy writer that forgets history must still invalidate captured resource state.
    let draft = foreign_source::reference_draft(
        &world,
        serde_json::from_value(request(OBJ, "obj")).unwrap(),
    )
    .unwrap();
    draft.source_artifacts[0].apply(&mut world, false);
    assert!(queue_captured_plan(&mut world, plan.plan_id()).is_err());
    let mut conflict = request(OBJ, "obj");
    conflict["metadata"]["license"] = json!("different asserted license");
    assert!(
        requests::preview(&mut world, "core.foreign_reference", conflict)
            .unwrap_err()
            .contains("different metadata")
    );
}
#[test]
fn corrupt_retained_bytes_refuse_load_before_replacing_live_model() {
    let mut world = fixture();
    let plan = preview(&mut world, request(OBJ, "obj"));
    apply(&mut world, &plan);
    let before = get_entity_snapshot(&world, plan.created_ids()[0]).unwrap();
    let path = temp_json_path("foreign-corrupt-source").with_extension("talos3d");
    handle_save_project(&mut world, path.to_str().unwrap()).unwrap();
    let mut json: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let sources = json["foreign_sources"].as_object_mut().unwrap();
    sources.values_mut().next().unwrap()["bytes"][0] = json!(0);
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(handle_load_project(&mut world, path.to_str().unwrap())
        .unwrap_err()
        .contains("digest"));
    assert_eq!(
        get_entity_snapshot(&world, plan.created_ids()[0]).unwrap(),
        before
    );
    fs::remove_file(path).unwrap();
}
#[test]
fn new_document_clears_sources_and_excessive_exact_query_is_refused() {
    let mut world = fixture();
    let plan = preview(&mut world, request(OBJ, "obj"));
    apply(&mut world, &plan);
    assert!(foreign_source::inspect_sources(
        &world,
        GetForeignSourcesRequest {
            digest: None,
            include_bytes: true
        }
    )
    .is_err());
    crate::plugins::persistence::new_document(&mut world);
    assert!(world.resource::<SourceArtifacts>().0.is_empty());
}

#[test]
fn unknown_native_controls_and_claimed_references_refuse_without_losing_input() {
    let mut world = fixture();
    let definition = handle_create_definition(&mut world, make_rect_extrusion_request())
        .unwrap()
        .definition_id;
    let plan = preview(&mut world, request(OBJ, "obj"));
    apply(&mut world, &plan);
    let id = plan.created_ids()[0];
    let before = get_entity_snapshot(&world, id).unwrap();
    let next = world.resource::<ElementIdAllocator>().next_value();
    let mut bad = native_request(id.0, &definition);
    bad["overrides"]["invented_control"] = json!(1.);
    assert!(requests::preview(&mut world, "core.foreign_native_mapping", bad).is_err());
    let mut bad = native_request(id.0, &definition);
    bad["uncertainty"] = json!([]);
    assert!(requests::preview(&mut world, "core.foreign_native_mapping", bad).is_err());
    assert_eq!(get_entity_snapshot(&world, id).unwrap(), before);
    assert_eq!(world.resource::<ElementIdAllocator>().next_value(), next);
    let entity = find_entity_by_element_id_readonly(&world, id).unwrap();
    world
        .entity_mut(entity)
        .insert(crate::plugins::refinement::RefinementStateComponent::default());
    assert!(requests::preview(
        &mut world,
        "core.foreign_native_mapping",
        native_request(id.0, &definition)
    )
    .unwrap_err()
    .contains("claim migration"));
}
#[test]
fn unsupported_records_are_reported_and_reusing_source_does_not_remove_it_on_undo() {
    let mut world = fixture();
    let mut bytes = OBJ.to_vec();
    bytes.extend_from_slice(b"\nmtllib absent.mtl\nvt 0 0\n");
    let plan = preview(&mut world, request(&bytes, "obj"));
    apply(&mut world, &plan);
    let before = sources(&world);
    assert!(before.to_string().contains("unsupported_records"));
    let duplicate = preview(&mut world, request(&bytes, "obj"));
    let content = duplicate.content();
    assert_eq!(
        content["source_artifact_changes"][0]["before"],
        content["source_artifact_changes"][0]["after"]
    );
    apply(&mut world, &duplicate);
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(sources(&world), before);
    assert!(get_entity_snapshot(&world, plan.created_ids()[0]).is_some());
}
#[test]
fn source_link_integrity_and_subset_export_keep_only_referenced_artifacts() {
    let mut world = fixture();
    let plan = preview(&mut world, request(OBJ, "obj"));
    apply(&mut world, &plan);
    let opaque = preview(&mut world, request(b"other input", "unsupported"));
    apply(&mut world, &opaque);
    let id = plan.created_ids()[0];
    let snapshot = crate::plugins::authored_edit_plan::capture_snapshot(&world, id).unwrap();
    let record = crate::plugins::persistence::PersistedEntityRecord {
        type_name: snapshot.type_name().into(),
        data: snapshot.to_json(),
        semantic: None,
    };
    let subset =
        crate::plugins::persistence::serialize_entity_records_as_project(&world, 10, vec![record])
            .unwrap();
    assert!(
        crate::plugins::persistence::deserialize_project_entity_records_with_assets(&subset)
            .err()
            .unwrap()
            .contains("asset migration")
    );
    let exported: Value = serde_json::from_slice(&subset).unwrap();
    assert_eq!(exported["foreign_sources"].as_object().unwrap().len(), 1);
    let path = temp_json_path("foreign-missing-source").with_extension("talos3d");
    handle_save_project(&mut world, path.to_str().unwrap()).unwrap();
    let mut project: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    project.as_object_mut().unwrap().remove("foreign_sources");
    fs::write(&path, serde_json::to_vec(&project).unwrap()).unwrap();
    let original = get_entity_snapshot(&world, id).unwrap();
    assert!(handle_load_project(&mut world, path.to_str().unwrap())
        .unwrap_err()
        .contains("missing"));
    assert_eq!(get_entity_snapshot(&world, id).unwrap(), original);
    fs::remove_file(path).unwrap();
}

#[test]
fn reused_source_is_a_freshness_condition_and_source_bytes_count_toward_plan_capacity() {
    let mut world = fixture();
    let plan = preview(&mut world, request(OBJ, "obj"));
    apply(&mut world, &plan);
    let reused = preview(&mut world, request(OBJ, "obj"));
    world
        .resource_mut::<SourceArtifacts>()
        .0
        .values_mut()
        .next()
        .unwrap()
        .metadata
        .provenance = "changed outside history".into();
    assert!(queue_captured_plan(&mut world, reused.plan_id()).is_err());
    let mut world = fixture();
    world.insert_resource(
        crate::plugins::authored_edit_plan::AuthoredEditPlanRegistry::with_limits(8, 4096),
    );
    assert!(requests::preview(
        &mut world,
        "core.foreign_reference",
        request(&vec![42; 65536], "unsupported")
    )
    .unwrap_err()
    .contains("Capacity"));
    assert!(world.resource::<SourceArtifacts>().0.is_empty());
}
#[test]
fn an_empty_native_body_cannot_silently_replace_a_visible_reference() {
    let mut world = fixture();
    let definition = handle_create_definition(
        &mut world,
        json!({"name":"Empty fixture","definition_kind":"Solid","parameters":[],"evaluators":[]}),
    )
    .unwrap()
    .definition_id;
    let plan = preview(&mut world, request(OBJ, "obj"));
    apply(&mut world, &plan);
    let id = plan.created_ids()[0];
    let mut request = native_request(id.0, &definition);
    request["overrides"] = json!({});
    assert!(
        requests::preview(&mut world, "core.foreign_native_mapping", request)
            .unwrap_err()
            .contains("visible geometry")
    );
    assert!(get_entity_snapshot(&world, id).is_some());
}
