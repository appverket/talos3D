//! One disposable preparation path for parametric commands and candidate execution.
//! Expressions and semantic annotations complete before the authored world changes.
use super::*;
use crate::authored_entity::BoxedEntity;
use crate::capability_registry::{AuthoredEntityFactory, CapabilityRegistry};
use crate::plugins::history::{self, EditorCommand, History, PendingCommandQueue};
use crate::plugins::modeling::generic_factory::PrimitiveFactory;
use crate::plugins::persistence::{
    apply_semantic_sidecars, capture_semantic_sidecars, PersistedSemanticSidecars,
};
use crate::relational::registry::ParametricInstance;
use crate::semantics::{PlanIntent, SemanticGraph, SemanticPlan};

#[derive(Clone)]
struct Member {
    shape: BoxedEntity,
    semantics: Option<PersistedSemanticSidecars>,
    membership: ParametricInstanceRef,
}
#[derive(Clone)]
struct State {
    instance: ParametricInstance,
    members: Vec<Member>,
}
fn capture(world: &World, id: u64) -> Result<State, String> {
    let instance = world
        .resource::<ParametricStore>()
        .get(id)
        .cloned()
        .ok_or("Unknown parametric instance")?;
    let wanted: std::collections::HashSet<_> = instance.geometry.iter().copied().collect();
    let factory = PrimitiveFactory::<ProfileExtrusion>::new();
    let mut members = Vec::with_capacity(wanted.len());
    let mut q = world
        .try_query::<EntityRef>()
        .ok_or("Cannot inspect parametric members")?;
    for entity in q.iter(world) {
        let Some(eid) = entity.get::<ElementId>() else {
            continue;
        };
        if !wanted.contains(&eid.0) {
            continue;
        }
        let membership = entity
            .get::<ParametricInstanceRef>()
            .cloned()
            .ok_or("Parametric member lost its instance association")?;
        if membership.instance_id != id {
            return Err("Parametric membership conflict".into());
        }
        members.push(Member {
            shape: factory
                .capture_snapshot(&entity, world)
                .ok_or("Parametric member has unsupported geometry")?,
            semantics: capture_semantic_sidecars(&entity),
            membership,
        });
    }
    if members.len() != wanted.len() {
        return Err("Parametric instance has missing members".into());
    }
    members.sort_by_key(|m| m.membership.member_index);
    Ok(State { instance, members })
}
fn install(world: &mut World, target: Option<&State>, previous: Option<&State>, id: u64) {
    let target_ids: std::collections::HashSet<_> = target
        .into_iter()
        .flat_map(|s| s.instance.geometry.iter().copied())
        .collect();
    if let Some(previous) = previous {
        for member in &previous.members {
            if !target_ids.contains(&member.shape.element_id().0) {
                member.shape.remove_from(world);
            }
        }
    }
    if let Some(target) = target {
        for member in &target.members {
            let old = previous.and_then(|s| {
                s.members
                    .iter()
                    .find(|m| m.shape.element_id() == member.shape.element_id())
            });
            member
                .shape
                .apply_with_previous(world, old.map(|m| &m.shape));
            if let Some(semantics) = &member.semantics {
                apply_semantic_sidecars(world, member.shape.element_id(), semantics);
            }
            crate::plugins::modeling::dependency_graph::stamp_authored_entity_dependencies(
                world,
                &member.shape,
            );
        }
        let refs: std::collections::HashMap<_, _> = target
            .members
            .iter()
            .map(|m| (m.shape.element_id(), m.membership.clone()))
            .collect();
        let entities: Vec<_> = world
            .query::<(Entity, &ElementId)>()
            .iter(world)
            .filter_map(|(entity, id)| refs.get(id).cloned().map(|r| (entity, r)))
            .collect();
        for (entity, membership) in entities {
            world.entity_mut(entity).insert(membership);
        }
    }
    world.init_resource::<ParametricStore>();
    world
        .resource_mut::<ParametricStore>()
        .restore_instance(id, target.map(|s| s.instance.clone()));
}
struct Command {
    before: Option<State>,
    after: State,
    next_element_id: u64,
}
impl EditorCommand for Command {
    fn label(&self) -> &'static str {
        if self.before.is_some() {
            "Edit parametric instance"
        } else {
            "Create parametric instance"
        }
    }
    fn apply(&mut self, world: &mut World) {
        install(
            world,
            Some(&self.after),
            self.before.as_ref(),
            self.after.instance.instance_id,
        );
        let mut allocator = world.resource_mut::<ElementIdAllocator>();
        let next = allocator.next_value().max(self.next_element_id);
        allocator.set_next(next);
    }
    fn undo(&mut self, world: &mut World) {
        install(
            world,
            self.before.as_ref(),
            Some(&self.after),
            self.after.instance.instance_id,
        );
    }
    fn semantic_plan(&self, _world: &World) -> SemanticPlan {
        self.after
            .members
            .iter()
            .filter_map(|m| {
                m.semantics
                    .as_ref()?
                    .concept
                    .as_ref()
                    .map(|c| PlanIntent::AssignConcept {
                        entity: m.shape.element_id(),
                        concept: c.concept.clone(),
                    })
            })
            .collect()
    }
}
fn prepare(world: &mut World, id: Option<u64>) -> Result<(World, Option<State>), String> {
    if world.contains_resource::<crate::plugins::document_state::ReadOnlyDocument>() {
        return Err("Read-only document".into());
    }
    if world.contains_resource::<History>() && world.contains_resource::<PendingCommandQueue>() {
        history::apply_pending_history_commands(world);
    }
    let before = id.map(|id| capture(world, id)).transpose()?;
    if let Some(before) = &before {
        let ids: std::collections::HashSet<_> = before.instance.geometry.iter().copied().collect();
        let mut q = world.query::<(Entity, &ElementId)>();
        if q.iter(world).any(|(entity, id)| {
            ids.contains(&id.0) && crate::plugins::object_locks::entity_locked(world, entity)
        }) {
            return Err("Parametric instance contains a locked member".into());
        }
    }
    let mut scratch = World::new();
    let mut capabilities = CapabilityRegistry::default();
    capabilities.register_factory(PrimitiveFactory::<ProfileExtrusion>::new());
    if let Some(source) = world.get_resource::<CapabilityRegistry>() {
        for class in source.element_class_descriptors() {
            capabilities.register_element_class(class.clone());
        }
    }
    scratch.insert_resource(capabilities);
    if let Some(graph) = world.get_resource::<SemanticGraph>() {
        scratch.insert_resource(graph.clone());
    }
    scratch.insert_resource(
        world
            .get_resource::<ParametricRegistry>()
            .cloned()
            .unwrap_or_default(),
    );
    scratch.insert_resource(
        world
            .get_resource::<ParametricStore>()
            .map(|s| s.isolated(id))
            .unwrap_or_default(),
    );
    let mut allocator = ElementIdAllocator::default();
    allocator.set_next(
        world
            .get_resource::<ElementIdAllocator>()
            .map_or(0, ElementIdAllocator::next_value),
    );
    scratch.insert_resource(allocator);
    if let Some(before) = &before {
        install(
            &mut scratch,
            Some(before),
            None,
            before.instance.instance_id,
        );
    }
    Ok((scratch, before))
}
fn commit(
    world: &mut World,
    scratch: &World,
    before: Option<State>,
    id: u64,
) -> Result<(), String> {
    let mut after = capture(scratch, id)?;
    // Retain authored presentation and semantic bindings on stable members.
    // The portable definition controls geometry, not later user material choices.
    if let Some(before) = &before {
        for member in &mut after.members {
            if let Some(old) = before
                .members
                .iter()
                .find(|old| old.shape.element_id() == member.shape.element_id())
            {
                member.semantics = old.semantics.clone();
                let mut value = member.shape.to_persisted_json();
                let old_value = old.shape.to_persisted_json();
                for key in [
                    "layer",
                    "material_assignment",
                    "opening_context",
                    "subobject_display_overrides",
                ] {
                    if let Some(v) = old_value.get(key) {
                        value[key] = v.clone();
                    }
                }
                member.shape =
                    PrimitiveFactory::<ProfileExtrusion>::new().from_persisted_json(&value)?;
            }
        }
    }
    let mut command = Command {
        before,
        after,
        next_element_id: scratch.resource::<ElementIdAllocator>().next_value(),
    };
    if world.contains_resource::<History>() {
        world.init_resource::<PendingCommandQueue>();
        world.init_resource::<history::SemanticEnforcement>();
        let revision = world.resource::<History>().model_revision();
        world
            .resource_mut::<PendingCommandQueue>()
            .push_command(Box::new(command));
        history::apply_pending_history_commands(world);
        if world.resource::<History>().model_revision() == revision {
            return Err(world
                .resource::<history::SemanticEnforcement>()
                .last_refusal()
                .map(|r| r.summary())
                .unwrap_or_else(|| "Parametric command was not applied".into()));
        }
    } else {
        // Offline candidate/test worlds consume the identical plan and gate.
        match history::evaluate_command_admissibility(world, &command) {
            crate::semantics::Verdict::Refuse(refusals) => {
                return Err(refusals
                    .iter()
                    .map(|r| r.summary())
                    .collect::<Vec<_>>()
                    .join("; "))
            }
            crate::semantics::Verdict::AdmitWithObligation(obligations) => {
                world.init_resource::<history::SemanticEnforcement>();
                world
                    .resource_mut::<history::SemanticEnforcement>()
                    .obligations
                    .extend(obligations);
            }
            crate::semantics::Verdict::Admit => {}
        }
        command.apply(world);
    }
    Ok(())
}
pub(super) fn create(
    world: &mut World,
    req: CreateParametricRequest,
) -> Result<CreateParametricResponse, String> {
    if req
        .overrides
        .values()
        .any(|v| !v.as_f64().is_some_and(f64::is_finite))
    {
        return Err("Parametric driver overrides must be finite numbers".into());
    }
    let (mut scratch, before) = prepare(world, None)?;
    let result = direct_create(&mut scratch, req)?;
    commit(world, &scratch, before, result.snapshot.instance_id)?;
    Ok(result)
}
pub(super) fn set_driver(
    world: &mut World,
    req: SetParametricDriverRequest,
) -> Result<SetDriverResponse, String> {
    if !req.value.as_f64().is_some_and(f64::is_finite) {
        return Err("Parametric driver value must be a finite number".into());
    }
    let id = req.instance_id;
    let (mut scratch, before) = prepare(world, Some(id))?;
    let result = direct_set_driver(&mut scratch, req)?;
    commit(world, &scratch, before, id)?;
    Ok(result)
}
pub(super) fn transform(
    world: &mut World,
    req: ParametricTransformRequest,
) -> Result<TransformOutcome, String> {
    let id = req.instance_id;
    let (mut scratch, before) = prepare(world, Some(id))?;
    let result = direct_transform(&mut scratch, req)?;
    if matches!(result, TransformOutcome::DriverEdit { new_value, .. } if !new_value.is_finite()) {
        return Err("Transform produced a nonfinite parametric driver".into());
    }
    if matches!(result, TransformOutcome::DriverEdit { .. }) {
        commit(world, &scratch, before, id)?;
    }
    Ok(result)
}
