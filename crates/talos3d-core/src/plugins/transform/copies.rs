//! A transform copy uses the displayed edit plan's poses. Only identities change.
//! Repetition retains the signed angle (including full turns) and the original pivot.
use super::*;
use crate::plugins::identity::ElementIdAllocator;
use bevy::input::{
    keyboard::{Key, KeyboardInput},
    ButtonState,
};

const MAX_ARRAY_ENTITIES: usize = 2048;
const MAX_ARRAY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Resource)]
pub(super) struct LastCopy {
    revision: ModelRevision,
    state: TransformState,
    motion: TransformMotion,
    angle: f32,
    pivot: Vec3,
    bytes_per_copy: usize,
    pub(super) input: String,
}

fn validate_sources(world: &World, state: &TransformState) -> Result<(), String> {
    // Snapshot-only copying is safe for ordinary geometry. Reject sidecars
    // carrying domain contracts until their owning capability can duplicate them.
    for (entity, snapshot) in &state.initial_snapshots {
        let entity_ref = world
            .get_entity(*entity)
            .map_err(|_| "Copy source disappeared")?;
        if let Some(mut sidecars) =
            crate::plugins::persistence::capture_semantic_sidecars(&entity_ref)
        {
            if group_snapshot(snapshot).is_some()
                && sidecars
                    .layer_assignment
                    .as_ref()
                    .is_some_and(|l| l.layer != "Default")
            {
                return Err("Copy requires a group-layer duplication plan".into());
            }
            sidecars.layer_assignment = None;
            if serde_json::to_value(sidecars)
                .map_err(|e| e.to_string())?
                .as_object()
                .is_none_or(|fields| !fields.is_empty())
            {
                return Err(format!(
                    "Copy requires a duplication plan for the semantic data on {}",
                    snapshot.label()
                ));
            }
        }
    }
    if world
        .get_resource::<crate::plugins::modeling::group::GroupEditContext>()
        .is_some_and(|c| !c.is_root())
    {
        return Err("Leave the group edit context before placing independent copies".into());
    }
    if state.initial_snapshots.len() > MAX_ARRAY_ENTITIES {
        return Err("Copy exceeds the interactive entity limit".into());
    }
    Ok(())
}

pub(super) fn toggle(world: &mut World) {
    let state = world.resource::<TransformState>();
    if !matches!(state.mode, TransformMode::Moving | TransformMode::Rotating)
        || world.resource::<PushPullContext>().active_face.is_some()
    {
        return;
    }
    let enabled = !state.copy_mode;
    // Keep the previous snapshots until their live presentation has been restored.
    if let Err(error) = restore_captured_presentation(world) {
        world.resource_mut::<ActiveTransformPreview>().refusal = Some(error);
        return;
    }
    invalidate_transform_candidate(world, "Updating copy mode");
    world.resource_mut::<ActiveTransformPreview>().last_motion = None;
    let mut state = world.resource_mut::<TransformState>();
    state.copy_mode = enabled;
    state.copy_error = None;
    if !enabled {
        clear_originals(world);
        return;
    }
    let ids = allocate_ids(world, state_originals(world));
    world.resource_mut::<TransformState>().copy_ids = ids;
    if let Err(error) = validate_sources(world, world.resource::<TransformState>()) {
        world.resource_mut::<TransformState>().copy_error = Some(error);
        return;
    }
    // The usual move path presents the destination. Share the original GPU mesh
    // and material at the starting pose for the lifetime of the gesture.
    let originals = world
        .resource::<TransformState>()
        .initial_snapshots
        .iter()
        .filter_map(|(e, s)| {
            Some((
                world.get::<Mesh3d>(*e)?.clone(),
                world.get::<MeshMaterial3d<StandardMaterial>>(*e)?.clone(),
                s.preview_transform()?,
                world.get::<Visibility>(*e).copied().unwrap_or_default(),
            ))
        })
        .collect::<Vec<_>>();
    let ghosts = originals
        .into_iter()
        .map(|bundle| world.spawn(bundle).id())
        .collect();
    world.resource_mut::<TransformState>().copy_originals = ghosts;
}

pub(super) fn clear_originals(world: &mut World) {
    for entity in std::mem::take(&mut world.resource_mut::<TransformState>().copy_originals) {
        world.despawn(entity);
    }
}

fn state_originals(world: &World) -> impl Iterator<Item = &BoxedEntity> {
    world
        .resource::<TransformState>()
        .initial_snapshots
        .iter()
        .map(|(_, s)| s)
}

fn allocate_ids<'a>(
    world: &World,
    poses: impl Iterator<Item = &'a BoxedEntity>,
) -> HashMap<ElementId, ElementId> {
    let allocator = world.resource::<ElementIdAllocator>();
    poses
        .map(|s| (s.element_id(), allocator.next_id()))
        .collect()
}

fn copy_poses(
    poses: &[BoxedEntity],
    ids: &HashMap<ElementId, ElementId>,
) -> Result<Vec<BoxedEntity>, String> {
    poses
        .iter()
        .map(|s| {
            s.0.copy_with_ids(ids).ok_or_else(|| {
                format!(
                    "Copy requires a complete duplication plan for {}",
                    s.type_name()
                )
            })
        })
        .collect()
}

pub(super) fn candidate(
    state: &TransformState,
    draft: &EditPlanDraft,
) -> Result<Vec<BoxedEntity>, String> {
    if let Some(reason) = &state.copy_error {
        return Err(reason.clone());
    }
    validate_poses(state, &draft.after, &draft.semantic_intents)?;
    copy_poses(&draft.after, &state.copy_ids)
}

fn queue_copies(
    world: &mut World,
    revision: ModelRevision,
    after: Vec<BoxedEntity>,
) -> Result<(), String> {
    let context = PlanContext {
        planner_id: "core.transform.copy".into(),
        planner_version: 1,
        request_kind: "copy".into(),
        intent: "Copy transformed geometry".into(),
        mutation_scope: "copies".into(),
        ..default()
    };
    let plan = AuthoredEditPlan::capture(
        world,
        None,
        revision,
        context,
        Vec::new(),
        after,
        crate::semantics::SemanticPlan::none(),
    )
    .map_err(|e| e.to_string())?;
    let plan = world
        .resource_mut::<AuthoredEditPlanRegistry>()
        .publish(plan)
        .map_err(|e| e.to_string())?;
    queue_captured_plan(world, plan.plan_id()).map_err(|e| e.to_string())
}

pub(super) fn commit(
    world: &mut World,
    plan: &AuthoredEditPlan,
    active: &ActiveTransformPreview,
) -> Result<(), String> {
    let mut state = world.resource::<TransformState>().clone();

    let (motion, _) = active
        .last_motion
        .as_ref()
        .ok_or("Copy needs a displayed transform")?;
    let pivot =
        effective_pivot(&state, world.resource::<PivotPoint>()).ok_or("Copy needs a pivot")?;

    let mut revision = plan.base_model_revision().clone();
    queue_captured_plan(world, plan.plan_id()).map_err(|e| e.to_string())?;
    revision.revision += 1;
    state.copy_originals.clear();
    world.insert_resource(LastCopy {
        revision,
        state,
        motion: *motion,
        angle: active.display_value.unwrap_or_default(),
        pivot,
        bytes_per_copy: plan.retained_bytes(),
        input: String::new(),
    });
    world
        .resource_mut::<StatusBarData>()
        .set_feedback("Copy placed · type *N or /N and press Enter".into(), 5.);
    Ok(())
}

fn validate_poses(
    state: &TransformState,
    poses: &[BoxedEntity],
    intents: &crate::semantics::SemanticPlan,
) -> Result<(), String> {
    let ids: HashSet<_> = state
        .initial_snapshots
        .iter()
        .map(|(_, s)| s.element_id())
        .collect();
    if !intents.is_empty()
        || poses.len() != ids.len()
        || poses.iter().any(|s| !ids.contains(&s.element_id()))
    {
        return Err("Copy requires an independent selection and all of its dependencies".into());
    }
    Ok(())
}

pub(super) fn repeat(world: &mut World, last: &LastCopy) -> Result<(), String> {
    let (operator, digits) = last.input.split_at(1);
    let count: usize = digits
        .parse()
        .ok()
        .filter(|n| *n > 0)
        .ok_or("Enter a positive copy count")?;
    if count.saturating_mul(last.state.initial_snapshots.len()) > MAX_ARRAY_ENTITIES {
        return Err(format!(
            "Array exceeds {MAX_ARRAY_ENTITIES} authored entities"
        ));
    }
    if count.saturating_mul(last.bytes_per_copy) > MAX_ARRAY_BYTES {
        return Err("Array exceeds the interactive geometry memory limit".into());
    }
    let mut copies = Vec::new();
    for i in 1..count {
        let factor = if operator == "/" {
            i as f32 / count as f32
        } else {
            (i + 1) as f32
        };
        let motion = match last.motion {
            TransformMotion::Move(delta) => TransformMotion::Move(delta * factor),
            TransformMotion::Rotate(_) => TransformMotion::Rotate(rotation_quat(
                last.state.axis,
                (last.angle * factor).to_radians(),
            )),
            TransformMotion::Scale(_) => return Err("Scale has no copy array".into()),
        };
        let before: Vec<_> = last
            .state
            .initial_snapshots
            .iter()
            .map(|(_, s)| s.clone())
            .collect();
        let after = rigid_after(before.iter(), motion, last.pivot);
        let draft = prepare_transform_edit(world, &last.state, before, after);
        if !draft.context.refusals.is_empty() {
            return Err(draft.context.findings.join("; "));
        }
        validate_poses(&last.state, &draft.after, &draft.semantic_intents)?;
        let ids = allocate_ids(world, draft.after.iter());
        copies.extend(copy_poses(&draft.after, &ids)?);
    }
    if copies.is_empty() {
        return Ok(());
    }
    queue_copies(world, last.revision.clone(), copies)
}

pub(super) fn input(world: &mut World, events: &[KeyboardInput]) {
    if !world.contains_resource::<LastCopy>() {
        return;
    }
    let revision = world.resource::<History>().revision_token();
    let last = world.resource::<LastCopy>();
    if !world.resource::<InputOwnership>().is_idle()
        || last.revision.document_id != revision.document_id
        || revision.revision > last.revision.revision
        || world
            .get_resource::<ButtonInput<MouseButton>>()
            .is_some_and(|b| b.just_pressed(MouseButton::Left))
    {
        world.remove_resource::<LastCopy>();
        return;
    }
    if revision != last.revision {
        return;
    }
    for event in events.iter().filter(|e| e.state == ButtonState::Pressed) {
        let mut last = world.resource_mut::<LastCopy>();
        let text = event.text.as_deref().or_else(|| match &event.logical_key {
            Key::Character(s) => Some(s.as_str()),
            _ => None,
        });
        match (event.key_code, text) {
            (_, Some("*" | "/")) => last.input = text.unwrap().to_string(),
            (_, Some(t))
                if !last.input.is_empty() && t.len() == 1 && t.as_bytes()[0].is_ascii_digit() =>
            {
                if last.input.len() < 10 {
                    last.input.push_str(t);
                }
            }
            (KeyCode::Backspace, _) => {
                last.input.pop();
            }
            (KeyCode::Enter | KeyCode::NumpadEnter, _) if !last.input.is_empty() => {
                let last = world.remove_resource::<LastCopy>().unwrap();
                let result = repeat(world, &last);
                let message = match result {
                    Ok(()) => format!("Array {} placed", last.input),
                    Err(e) => e,
                };
                world
                    .resource_mut::<StatusBarData>()
                    .set_feedback(message, 4.);
                return;
            }
            (KeyCode::ShiftLeft | KeyCode::ShiftRight, _) => continue,
            _ => {
                world.remove_resource::<LastCopy>();
                return;
            }
        }
        let msg = format!("Array {} · Enter to place", last.input);
        world.resource_mut::<StatusBarData>().set_feedback(msg, 4.);
    }
}
