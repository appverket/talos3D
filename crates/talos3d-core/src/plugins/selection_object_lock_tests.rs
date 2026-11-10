use super::*;
use crate::plugins::{
    history::{apply_pending_history_commands_for_test, History, PendingCommandQueue},
    object_locks::{execute_set_object_locked, DocumentLocks, ObjectLockPlugin, ObjectLocks},
};

fn lock(app: &mut App, ids: &[u64], locked: bool) {
    execute_set_object_locked(
        app.world_mut(),
        &serde_json::json!({"element_ids":ids,"locked":locked}),
    )
    .unwrap();
    apply_pending_history_commands_for_test(app.world_mut());
}

fn with_locks(app: &mut App) {
    app.init_resource::<History>()
        .init_resource::<PendingCommandQueue>()
        .add_plugins(ObjectLockPlugin);
}

#[test]
fn object_locks_selected_command_locks_all_targets_in_one_undo_step() {
    let (mut app, _) = native_selection_app();
    with_locks(&mut app);
    let group = crate::plugins::commands::find_entity_by_element_id(app.world_mut(), ElementId(30))
        .unwrap();
    let other = app.world_mut().spawn((ElementId(90), Selected)).id();
    app.world_mut().entity_mut(group).insert(Selected);
    crate::plugins::command_registry::execute_command(
        app.world_mut(),
        "view.lock_selected",
        &serde_json::Value::Null,
    )
    .unwrap();
    apply_pending_history_commands_for_test(app.world_mut());
    assert_eq!(
        app.world().resource::<ObjectLocks>().locked,
        [ElementId(30), ElementId(90)].into()
    );
    assert!(app.world().get::<Selected>(group).is_none());
    assert!(app.world().get::<Selected>(other).is_none());
    app.world_mut()
        .resource_mut::<PendingCommandQueue>()
        .queue_undo();
    apply_pending_history_commands_for_test(app.world_mut());
    assert!(app.world().resource::<ObjectLocks>().locked.is_empty());
}

#[test]
fn object_locks_native_click_passes_through_locked_group_and_undo_restores_picking() {
    use crate::plugins::modeling::primitives::BoxPrimitive;
    use bevy::input::ButtonState::{Pressed, Released};
    let (mut app, window) = native_selection_app();
    with_locks(&mut app);
    let behind = app
        .world_mut()
        .spawn((
            ElementId(50),
            BoxPrimitive {
                centre: Vec3::new(0., 0., -2.),
                half_extents: Vec3::splat(0.5),
            },
        ))
        .id();
    lock(&mut app, &[30], true);
    queue_native_edges(&mut app, window, &[Pressed, Released]);
    app.update();
    assert!(app.world().get::<Selected>(behind).is_some());
    assert!(app.world().resource::<GroupEditContext>().is_root());
    assert!(app
        .world()
        .resource::<DocumentLocks>()
        .locked
        .contains(&ElementId(10)));
    app.world_mut()
        .resource_mut::<PendingCommandQueue>()
        .queue_undo();
    apply_pending_history_commands_for_test(app.world_mut());
    let ray = Ray3d::new(Vec3::Z * 5., Dir3::NEG_Z);
    let front = selection_hit_entity(app.world_mut(), ray, Vec2::new(400., 300.)).unwrap();
    assert_eq!(app.world().get::<ElementId>(front), Some(&ElementId(10)));
    app.world_mut()
        .resource_mut::<PendingCommandQueue>()
        .queue_redo();
    apply_pending_history_commands_for_test(app.world_mut());
    assert_eq!(
        selection_hit_entity(app.world_mut(), ray, Vec2::new(400., 300.)),
        Some(behind)
    );
}

#[test]
fn object_locks_marquee_excludes_nested_children_and_unlock_preserves_child_lock() {
    let (mut app, window, feature) = marquee_app(false);
    with_locks(&mut app);
    lock(&mut app, &[40], true);
    lock(&mut app, &[20], true);
    marquee_drag(
        &mut app,
        window,
        Vec2::new(300., 200.),
        Vec2::new(500., 400.),
        0,
    );
    assert!(app.world().get::<Selected>(feature).is_none());
    lock(&mut app, &[20], false);
    assert!(app
        .world()
        .resource::<DocumentLocks>()
        .locked
        .contains(&ElementId(40)));
    assert!(!app
        .world()
        .resource::<DocumentLocks>()
        .locked
        .contains(&ElementId(10)));
    lock(&mut app, &[40], false);
    app.world_mut().resource_mut::<GroupEditContext>().stack = vec![ElementId(30), ElementId(20)];
    marquee_drag(
        &mut app,
        window,
        Vec2::new(300., 200.),
        Vec2::new(500., 400.),
        0,
    );
    assert!(app.world().get::<Selected>(feature).is_some());
}

#[test]
fn object_locks_clear_selection_face_edit_and_follow_membership_changes() {
    let (mut app, _, feature) = marquee_app(true);
    with_locks(&mut app);
    app.world_mut().entity_mut(feature).insert(Selected);
    lock(&mut app, &[20], true);
    assert!(app.world().get::<Selected>(feature).is_none());
    assert!(!app.world().resource::<FaceEditContext>().is_active());
    let added = app.world_mut().spawn(ElementId(77)).id();
    let group = find_entity_by_element_id_readonly(app.world(), ElementId(20)).unwrap();
    app.world_mut()
        .get_mut::<GroupMembers>(group)
        .unwrap()
        .member_ids
        .push(ElementId(77));
    app.update();
    assert_eq!(retain_selected_entity(app.world(), added), None);
    app.world_mut()
        .get_mut::<GroupMembers>(group)
        .unwrap()
        .member_ids
        .retain(|id| id.0 != 77);
    app.update();
    assert!(!crate::plugins::object_locks::entity_locked(
        app.world(),
        added
    ));
}

#[test]
fn object_locks_generated_parts_and_selection_proxies_inherit_owner_lock() {
    let (mut app, _) = native_selection_app();
    with_locks(&mut app);
    let part = app
        .world_mut()
        .spawn(GeneratedOccurrencePart {
            owner: ElementId(10),
            slot_path: "surface".into(),
            definition_id: crate::plugins::modeling::definition::DefinitionId("test".into()),
        })
        .id();
    let proxy = app
        .world_mut()
        .spawn((ElementId(90), SelectionProxy(ElementId(10))))
        .id();
    lock(&mut app, &[30], true);
    assert_eq!(resolve_entity_for_selection(app.world(), part), None);
    assert_eq!(resolve_entity_for_selection(app.world(), proxy), None);
    assert!(crate::plugins::object_locks::entity_locked(
        app.world(),
        part
    ));
    assert!(crate::plugins::object_locks::entity_locked(
        app.world(),
        proxy
    ));
    assert!(execute_set_object_locked(
        app.world_mut(),
        &serde_json::json!({"element_ids":[30,999],"locked":false})
    )
    .is_err());
    assert!(app
        .world()
        .resource::<ObjectLocks>()
        .locked
        .contains(&ElementId(30)));
}

#[test]
fn object_locks_outliner_keeps_unlock_control_and_reports_inherited_state() {
    use crate::plugins::outliner::{
        build_outliner_tree, outline_tree_json, OutlinerTree, OutlinerWindowState,
    };
    let (mut app, _) = native_selection_app();
    with_locks(&mut app);
    app.init_resource::<OutlinerTree>()
        .init_resource::<OutlinerWindowState>();
    app.world_mut()
        .resource_mut::<OutlinerWindowState>()
        .visible = true;
    lock(&mut app, &[30], true);
    build_outliner_tree(app.world_mut());
    let tree = app.world().resource::<OutlinerTree>();
    let root = tree.nodes.iter().find(|node| node.node_id == 30).unwrap();
    assert!(root.explicitly_locked && root.effectively_locked);
    assert_eq!(root.visibility_id, Some(30));
    assert_eq!(root.select_entity, None);
    let child = tree.nodes.iter().find(|node| node.node_id == 10).unwrap();
    assert!(!child.explicitly_locked && child.effectively_locked);
    assert_eq!(child.select_entity, None);
    let json = outline_tree_json(app.world_mut());
    assert_eq!(json["roots"][0]["locked"], true);
    assert_eq!(json["roots"][0]["children"][0]["explicitly_locked"], false);
}

#[test]
fn object_locks_between_native_press_and_release_cannot_reselect_target() {
    use bevy::input::ButtonState::{Pressed, Released};
    let (mut app, window) = native_selection_app();
    with_locks(&mut app);
    queue_native_edges(&mut app, window, &[Pressed]);
    app.update();
    lock(&mut app, &[30], true);
    queue_native_edges(&mut app, window, &[Released]);
    app.update();
    assert_eq!(
        app.world_mut()
            .query_filtered::<Entity, With<Selected>>()
            .iter(app.world())
            .count(),
        0
    );
    assert!(app.world().resource::<GroupEditContext>().is_root());
}

#[test]
fn object_locks_cover_evaluated_features_of_a_locked_body() {
    let (mut app, _, feature) = marquee_app(true);
    with_locks(&mut app);
    lock(&mut app, &[10], true);
    assert!(crate::plugins::object_locks::entity_locked(
        app.world(),
        feature
    ));
    assert!(!app.world().resource::<FaceEditContext>().is_active());
    lock(&mut app, &[10], false);
    assert!(!crate::plugins::object_locks::entity_locked(
        app.world(),
        feature
    ));
}
