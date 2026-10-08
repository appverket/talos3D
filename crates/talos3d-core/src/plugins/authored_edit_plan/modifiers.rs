//! Shared ordered planning stage. Capability callbacks mutate a draft before
//! immutable capture; they never apply snapshots to the authored world.
use super::*;
use std::any::Any;

/// Ephemeral planner input. The typed payload belongs to the invoking
/// capability; this is neither a persisted representation nor a second IR.
pub struct EditModifierRequest<'a> {
    pub kind: &'a str,
    payload: &'a (dyn Any + Send + Sync),
}
impl<'a> EditModifierRequest<'a> {
    pub fn new(kind: &'a str, payload: &'a (dyn Any + Send + Sync)) -> Self {
        Self { kind, payload }
    }
    pub fn payload<T: Any>(&self) -> Option<&T> {
        self.payload.downcast_ref()
    }
}

/// Mutable only during planning. Publication captures its final contents in
/// the single immutable `AuthoredEditPlan` carrier.
pub struct EditPlanDraft {
    pub source_artifacts: Vec<crate::plugins::foreign_source::SourceArtifactChange>,
    pub context: PlanContext,
    pub before: Vec<BoxedEntity>,
    pub after: Vec<BoxedEntity>,
    pub semantic_intents: SemanticPlan,
}
impl EditPlanDraft {
    pub fn capture(
        self,
        world: &World,
        interaction: Option<InteractionId>,
        base: ModelRevision,
    ) -> Result<AuthoredEditPlan, PlanError> {
        AuthoredEditPlan::capture(
            world,
            interaction,
            base,
            self.context,
            self.before,
            self.after,
            self.semantic_intents,
        )?
        .with_source_artifacts(world, self.source_artifacts)
    }
}

type ModifierCallback = dyn Fn(&World, &EditModifierRequest<'_>, &mut EditPlanDraft) + Send + Sync;

#[derive(Clone)]
pub struct OrderedEditModifier {
    id: String,
    priority: i32,
    request_kind: String,
    callback: Arc<ModifierCallback>,
}
impl OrderedEditModifier {
    pub fn new(
        id: impl Into<String>,
        priority: i32,
        request_kind: impl Into<String>,
        callback: impl Fn(&World, &EditModifierRequest<'_>, &mut EditPlanDraft) + Send + Sync + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            priority,
            request_kind: request_kind.into(),
            callback: Arc::new(callback),
        }
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn priority(&self) -> i32 {
        self.priority
    }
}

/// Higher priority first; ties preserve registration order. When merging a
/// compatibility registry, generic callbacks precede legacy callbacks at the
/// same priority, then each registry preserves its own registration order.
#[derive(Resource, Default, Clone)]
pub struct EditPlanModifiers {
    entries: Vec<OrderedEditModifier>,
}
impl EditPlanModifiers {
    pub fn register(&mut self, modifier: OrderedEditModifier) -> Result<(), String> {
        if self.entries.iter().any(|entry| entry.id == modifier.id) {
            return Err(format!("duplicate edit modifier: {}", modifier.id));
        }
        self.entries.push(modifier);
        self.entries
            .sort_by_key(|entry| std::cmp::Reverse(entry.priority));
        Ok(())
    }
    pub fn entries(&self) -> &[OrderedEditModifier] {
        &self.entries
    }
}

/// Execute one deterministic merged stage without allocating a new callback
/// list each frame. Compatibility entries must already be priority-sorted.
pub fn apply_edit_modifiers(
    world: &World,
    request: &EditModifierRequest<'_>,
    draft: &mut EditPlanDraft,
    compatibility: &[OrderedEditModifier],
) {
    let generic = world
        .get_resource::<EditPlanModifiers>()
        .map(EditPlanModifiers::entries)
        .unwrap_or_default();
    let mut generic = generic.iter().peekable();
    let mut legacy = compatibility.iter().peekable();
    loop {
        let entry = match (generic.peek(), legacy.peek()) {
            (Some(a), Some(b)) if a.priority >= b.priority => generic.next(),
            (Some(_), Some(_)) => legacy.next(),
            (Some(_), None) => generic.next(),
            (None, Some(_)) => legacy.next(),
            (None, None) => break,
        }
        .expect("peeked modifier");
        if entry.request_kind == request.kind {
            (entry.callback)(world, request, draft);
        }
    }
    capture_added_originals(world, draft);
}

/// A modifier can add dependent entities to the result. Preserve their authored
/// originals for undo from the same factory contract used by normal captures.
fn capture_added_originals(world: &World, draft: &mut EditPlanDraft) {
    let before_ids: BTreeSet<_> = draft.before.iter().map(BoxedEntity::element_id).collect();
    let needed: BTreeSet<_> = draft
        .after
        .iter()
        .map(BoxedEntity::element_id)
        .filter(|id| !before_ids.contains(id))
        .collect();
    if needed.is_empty() {
        return;
    }
    let Some(registry) = world.get_resource::<CapabilityRegistry>() else {
        return;
    };
    let Some(mut query) = world.try_query::<(Entity, &ElementId)>() else {
        return;
    };
    let mut originals: BTreeMap<_, _> = query
        .iter(world)
        .filter(|(_, id)| needed.contains(id))
        .filter_map(|(entity, id)| {
            registry
                .capture_snapshot(&world.entity(entity), world)
                .map(|snapshot| (*id, snapshot))
        })
        .collect();
    for snapshot in &draft.after {
        if let Some(original) = originals.remove(&snapshot.element_id()) {
            draft.before.push(original);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn modifier(id: &str, priority: i32, kind: &str) -> OrderedEditModifier {
        let name = id.to_string();
        OrderedEditModifier::new(id, priority, kind, move |_, request, draft| {
            assert_eq!(request.payload::<u32>(), Some(&42));
            draft.context.derived_impacts.push(name.clone());
        })
    }
    #[test]
    fn merged_order_is_stable_and_unrelated_requests_are_skipped() {
        let mut world = World::new();
        let mut registry = EditPlanModifiers::default();
        registry.register(modifier("generic-a", 0, "test")).unwrap();
        registry.register(modifier("generic-b", 0, "test")).unwrap();
        registry.register(modifier("other", 200, "other")).unwrap();
        assert!(registry.register(modifier("generic-a", 0, "test")).is_err());
        world.insert_resource(registry);
        let mut draft = EditPlanDraft {
            source_artifacts: Vec::new(),
            context: Default::default(),
            before: vec![],
            after: vec![],
            semantic_intents: SemanticPlan::none(),
        };
        apply_edit_modifiers(
            &world,
            &EditModifierRequest::new("test", &42u32),
            &mut draft,
            &[
                modifier("legacy-high", 100, "test"),
                modifier("legacy-a", 0, "test"),
                modifier("legacy-b", 0, "test"),
            ],
        );
        assert_eq!(
            draft.context.derived_impacts,
            [
                "legacy-high",
                "generic-a",
                "generic-b",
                "legacy-a",
                "legacy-b"
            ]
        );
    }

    /// Isolates the added registry/original-capture stage from domain planning
    /// and presentation. Budget is fixed before measurement: p95 < 2 ms for
    /// 512 existing snapshots, 20 callbacks. Full interaction budgets are a
    /// separate gate; this result cannot be used as frame-time evidence.
    #[test]
    #[ignore = "explicit local microbenchmark; wall-clock budget is not a CI assertion"]
    fn modifier_stage_budget() {
        use crate::plugins::modeling::{
            generic_snapshot::PrimitiveSnapshot,
            primitives::{BoxPrimitive, ShapeRotation},
        };
        use crate::time::Instant;
        let mut world = World::new();
        let mut registry = EditPlanModifiers::default();
        for index in 0..20 {
            registry
                .register(OrderedEditModifier::new(
                    format!("noop-{index}"),
                    index,
                    "bench",
                    |_, _, _| {},
                ))
                .unwrap();
        }
        world.insert_resource(registry);
        let snapshots: Vec<BoxedEntity> = (0..512)
            .map(|id| {
                PrimitiveSnapshot {
                    element_id: ElementId(id),
                    primitive: BoxPrimitive {
                        centre: Vec3::new(id as f32, 0., 0.),
                        half_extents: Vec3::ONE,
                    },
                    rotation: ShapeRotation::default(),
                    material_assignment: None,
                    opening_context: None,
                    subobject_display_overrides: None,
                }
                .into()
            })
            .collect();
        let mut samples = Vec::new();
        for _ in 0..250 {
            let mut draft = EditPlanDraft {
                source_artifacts: Vec::new(),
                context: Default::default(),
                before: snapshots.clone(),
                after: snapshots.clone(),
                semantic_intents: SemanticPlan::none(),
            };
            let start = Instant::now();
            apply_edit_modifiers(
                &world,
                &EditModifierRequest::new("bench", &()),
                &mut draft,
                &[],
            );
            samples.push(start.elapsed());
        }
        samples.sort();
        let p95 = samples[237];
        eprintln!(
            "modifier stage: 512 snapshots / 20 callbacks / 250 samples, p95={p95:?}, limit=2ms"
        );
        assert!(p95 < std::time::Duration::from_millis(2));
    }
}
