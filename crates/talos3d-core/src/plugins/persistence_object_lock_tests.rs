use super::*;
use crate::plugins::{
    modeling::{
        generic_factory::PrimitiveFactory,
        group::{GroupFactory, GroupMembers},
        primitives::BoxPrimitive,
    },
    object_locks::{refresh_object_locks, DocumentLocks, ObjectLocks},
};

fn lock_world() -> World {
    let mut world = shell_world();
    world.init_resource::<PropertyEditState>();
    world.init_resource::<TransformState>();
    world.init_resource::<NextState<ActiveTool>>();
    world.init_resource::<Assets<Mesh>>();
    world.init_resource::<Assets<StandardMaterial>>();
    world.init_resource::<ObjectLocks>();
    world
        .resource_mut::<CapabilityRegistry>()
        .register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    world
        .resource_mut::<CapabilityRegistry>()
        .register_factory(GroupFactory);
    world
}

#[test]
fn object_locks_round_trip_real_project_and_clear_on_new_or_legacy_document() {
    let mut source = lock_world();
    source.spawn((
        ElementId(1),
        BoxPrimitive {
            centre: Vec3::ZERO,
            half_extents: Vec3::ONE,
        },
    ));
    source.spawn((
        ElementId(2),
        GroupMembers {
            name: "Terrain reference".into(),
            member_ids: vec![ElementId(1)],
            frame: Default::default(),
            linked_model: None,
        },
    ));
    source
        .resource_mut::<ObjectLocks>()
        .locked
        .extend([ElementId(2), ElementId(999)]);
    let bytes = capture_project_file_bytes(&mut source).unwrap();
    let mut target = lock_world();
    open_project_bytes(&mut target, &bytes).unwrap();
    assert_eq!(
        target.resource::<ObjectLocks>().locked,
        BTreeSet::from([ElementId(2)])
    );
    assert!(target
        .resource::<DocumentLocks>()
        .locked
        .contains(&ElementId(1)));
    let leaf = crate::plugins::commands::find_entity_by_element_id_readonly(&target, ElementId(1))
        .unwrap();
    assert_ne!(target.get::<Visibility>(leaf), Some(&Visibility::Hidden));

    // Older projects have no lock field and must replace, not retain, old locks.
    let mut legacy: serde_json::Value = crate::plugins::project_codec::decode(&bytes).unwrap();
    legacy.as_object_mut().unwrap().remove("locked_objects");
    open_project_bytes(&mut target, &serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(target.resource::<ObjectLocks>().locked.is_empty());
    assert!(target.resource::<DocumentLocks>().locked.is_empty());
    target
        .resource_mut::<ObjectLocks>()
        .locked
        .insert(ElementId(2));
    refresh_object_locks(&mut target);
    new_document(&mut target);
    assert!(target.resource::<ObjectLocks>().locked.is_empty());
    assert!(target.resource::<DocumentLocks>().locked.is_empty());
}
