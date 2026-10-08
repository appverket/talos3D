//! Opt-in, bounded evidence from the real input/presentation path. No planner
//! or domain behavior lives here. `present_ms` ends at Bevy's swapchain present
//! submission, not at physical display scanout (which this process cannot see).
use super::*;
use crate::plugins::{
    authored_edit_plan::{capture_snapshot_index, AuthoredEditPlan},
    history::{History, ModelRevision},
    transform::ActiveTransformPreview,
};
use bevy::render::{
    renderer::render_system, view::ExtractedWindows, Extract, ExtractSchedule, Render, RenderApp,
    RenderSystems,
};
use std::sync::{Arc, Mutex};

use crate::time::Instant;
const MAX_SAMPLES: usize = 128; // ux_drag admits at most 123 input steps.

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxDragTrace {
    pub sequence: u64,
    pub timing_boundary: String,
    pub samples: Vec<UxDragTraceSample>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UxDragTraceSample {
    pub index: usize,
    pub action: String,
    pub plan_id: Option<String>,
    pub digest: Option<String>,
    pub after_entity_ids: Vec<u64>,
    pub model_revision: ModelRevision,
    pub main_frame_ms: f64,
    /// None means a primary-window swapchain present was not observed.
    pub present_ms: Option<f64>,
    /// Present only for a release that had a captured candidate.
    pub committed_exact_candidate: Option<bool>,
    pub release_to_commit_ms: Option<f64>,
    pub preview_error: Option<String>,
}

#[derive(Resource, Default)]
struct TraceState {
    shared: Arc<Mutex<Option<UxDragTrace>>>,
    pending: Option<InputStamp>,
    frame: Option<RenderStamp>,
}
struct InputStamp {
    sequence: u64,
    started: Instant,
    action: &'static str,
    release_plan: Option<Arc<AuthoredEditPlan>>,
}
#[derive(Resource, Clone)]
struct RenderStamp {
    shared: Arc<Mutex<Option<UxDragTrace>>>,
    sequence: u64,
    index: usize,
    started: Instant,
    primary: Entity,
    had_surface: bool,
}

pub(super) fn install(app: &mut App) {
    app.init_resource::<TraceState>()
        .add_systems(Last, record_main_frame);
    if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
        render_app
            .add_systems(ExtractSchedule, extract_trace)
            .add_systems(
                Render,
                (
                    before_present
                        .in_set(RenderSystems::Render)
                        .before(render_system),
                    after_present
                        .in_set(RenderSystems::Render)
                        .after(render_system),
                ),
            );
    }
}
pub(super) fn begin(world: &mut World, sequence: u64) {
    let mut state = world.resource_mut::<TraceState>();
    *state.shared.lock().unwrap() = Some(UxDragTrace {
        sequence,
        timing_boundary: "Bevy input injection to primary-window swapchain present submission; excludes HTTP and physical display scanout".into(),
        samples: Vec::new(),
    });
    state.pending = None;
    state.frame = None;
}
pub(super) fn observe(world: &World) -> Option<UxDragTrace> {
    world
        .get_resource::<TraceState>()?
        .shared
        .lock()
        .unwrap()
        .clone()
}
pub(super) fn input(world: &mut World, step: &UxStep) {
    let Some(state) = world.get_resource::<TraceState>() else {
        return;
    };
    if !state
        .shared
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|t| t.sequence == step.sequence)
    {
        return;
    }
    let action = match step.action {
        UxStepAction::MovePointer(_) => "pointer_move",
        UxStepAction::MouseButton {
            state: ButtonState::Pressed,
            ..
        } => "press",
        UxStepAction::MouseButton {
            state: ButtonState::Released,
            ..
        } => "release",
        _ => "other",
    };
    let release_plan = (action == "release")
        .then(|| {
            world
                .get_resource::<ActiveTransformPreview>()
                .and_then(|p| p.plan.clone())
        })
        .flatten();
    world.resource_mut::<TraceState>().pending = Some(InputStamp {
        sequence: step.sequence,
        started: Instant::now(),
        action,
        release_plan,
    });
}
fn record_main_frame(world: &mut World) {
    world.resource_mut::<TraceState>().frame = None;
    let Some(stamp) = world.resource_mut::<TraceState>().pending.take() else {
        return;
    };
    let Some(history) = world.get_resource::<History>() else {
        return;
    };
    let revision = history.revision_token();
    let active = world.get_resource::<ActiveTransformPreview>();
    let plan = stamp
        .release_plan
        .clone()
        .or_else(|| active.and_then(|p| p.plan.clone()));
    let committed = stamp.release_plan.as_ref().map(|plan| {
        let captured =
            capture_snapshot_index(world, plan.after_snapshots().iter().map(|s| s.element_id()));
        revision.document_id == plan.base_model_revision().document_id
            && revision.revision == plan.base_model_revision().revision + 1
            && plan.after_snapshots().iter().all(|after| {
                captured
                    .get(&after.element_id())
                    .is_some_and(|(_, actual)| actual == after)
            })
    });
    let elapsed = stamp.started.elapsed().as_secs_f64() * 1000.;
    let sample = UxDragTraceSample {
        index: 0,
        action: stamp.action.into(),
        plan_id: plan.as_ref().map(|p| p.plan_id().0.clone()),
        digest: plan.as_ref().map(|p| p.digest().into()),
        after_entity_ids: plan
            .as_ref()
            .map(|p| {
                p.after_snapshots()
                    .iter()
                    .map(|s| s.element_id().0)
                    .collect()
            })
            .unwrap_or_default(),
        model_revision: revision,
        main_frame_ms: elapsed,
        present_ms: None,
        committed_exact_candidate: committed,
        release_to_commit_ms: (committed == Some(true)).then_some(elapsed),
        preview_error: active.and_then(|a| a.refusal.clone()),
    };
    let shared = world.resource::<TraceState>().shared.clone();
    let index = {
        let mut buffer = shared.lock().unwrap();
        let Some(trace) = buffer.as_mut() else { return };
        if trace.sequence != stamp.sequence || trace.samples.len() == MAX_SAMPLES {
            return;
        }
        let index = trace.samples.len();
        trace.samples.push(UxDragTraceSample { index, ..sample });
        index
    };
    if let Ok(primary) = primary_window_entity(world) {
        world.resource_mut::<TraceState>().frame = Some(RenderStamp {
            shared,
            sequence: stamp.sequence,
            index,
            started: stamp.started,
            primary,
            had_surface: false,
        });
    }
}
fn extract_trace(mut commands: Commands, state: Extract<Res<TraceState>>) {
    if let Some(frame) = &state.frame {
        commands.insert_resource(frame.clone());
    } else {
        commands.remove_resource::<RenderStamp>();
    }
}
fn before_present(stamp: Option<ResMut<RenderStamp>>, windows: Res<ExtractedWindows>) {
    if let Some(mut stamp) = stamp {
        stamp.had_surface = windows
            .get(&stamp.primary)
            .is_some_and(|w| w.swap_chain_texture.is_some());
    }
}
fn after_present(stamp: Option<Res<RenderStamp>>, windows: Res<ExtractedWindows>) {
    let Some(stamp) = stamp else { return };
    if !stamp.had_surface
        || !windows
            .get(&stamp.primary)
            .is_some_and(|w| w.swap_chain_texture.is_none())
    {
        return;
    }
    let mut shared = stamp.shared.lock().unwrap();
    if let Some(trace) = shared.as_mut().filter(|t| t.sequence == stamp.sequence) {
        if let Some(sample) = trace.samples.get_mut(stamp.index) {
            sample.present_ms = Some(stamp.started.elapsed().as_secs_f64() * 1000.);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        authored_entity::BoxedEntity,
        plugins::{
            authored_edit_plan::{queue_captured_plan, AuthoredEditPlanRegistry, PlanContext},
            history::{
                apply_pending_history_commands_for_test, PendingCommandQueue, SemanticEnforcement,
            },
            identity::ElementIdAllocator,
            modeling::{
                generic_factory::PrimitiveFactory,
                generic_snapshot::PrimitiveSnapshot,
                primitives::{BoxPrimitive, ShapeRotation},
            },
        },
    };

    #[test]
    fn release_trace_requires_exact_commit_and_does_not_invent_a_present() {
        for commit in [false, true] {
            let mut world = World::new();
            let mut registry = CapabilityRegistry::default();
            registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
            world.insert_resource(registry);
            world.init_resource::<History>();
            world.init_resource::<TraceState>();
            world.init_resource::<ActiveTransformPreview>();
            world.init_resource::<AuthoredEditPlanRegistry>();
            world.init_resource::<PendingCommandQueue>();
            world.init_resource::<SemanticEnforcement>();
            world.init_resource::<ElementIdAllocator>();
            let before: BoxedEntity = PrimitiveSnapshot {
                element_id: ElementId(1),
                primitive: BoxPrimitive {
                    centre: Vec3::ZERO,
                    half_extents: Vec3::splat(0.5),
                },
                rotation: ShapeRotation::default(),
                material_assignment: None,
                opening_context: None,
                subobject_display_overrides: None,
            }
            .into();
            before.apply_to(&mut world);
            let after = before.translate_by(Vec3::X);
            let plan = AuthoredEditPlan::capture(
                &world,
                None,
                world.resource::<History>().revision_token(),
                PlanContext::default(),
                vec![before],
                vec![after],
                Default::default(),
            )
            .unwrap();
            let plan = world
                .resource_mut::<AuthoredEditPlanRegistry>()
                .publish(plan)
                .unwrap();
            world.resource_mut::<ActiveTransformPreview>().plan = Some(plan.clone());
            begin(&mut world, 1);
            input(
                &mut world,
                &UxStep {
                    sequence: 1,
                    action: UxStepAction::MouseButton {
                        position: Vec2::ZERO,
                        button: MouseButton::Left,
                        state: ButtonState::Released,
                    },
                },
            );
            world.resource_mut::<ActiveTransformPreview>().plan = None;
            if commit {
                queue_captured_plan(&mut world, plan.plan_id()).unwrap();
                apply_pending_history_commands_for_test(&mut world);
            }
            record_main_frame(&mut world);
            let result = observe(&world).unwrap();
            assert_eq!(result.samples.len(), 1);
            let sample = &result.samples[0];
            assert_eq!(sample.plan_id.as_deref(), Some(plan.plan_id().0.as_str()));
            assert_eq!(sample.committed_exact_candidate, Some(commit));
            assert_eq!(sample.release_to_commit_ms.is_some(), commit);
            assert_eq!(
                sample.present_ms, None,
                "headless evidence cannot claim a rendered frame"
            );
            begin(&mut world, 2);
            input(
                &mut world,
                &UxStep {
                    sequence: 1,
                    action: UxStepAction::MovePointer(Vec2::ZERO),
                },
            );
            record_main_frame(&mut world);
            assert!(
                observe(&world).unwrap().samples.is_empty(),
                "old input cannot leak into a new trace"
            );
        }
    }
}
