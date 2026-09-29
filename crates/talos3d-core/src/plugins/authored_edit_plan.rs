//! ADR-065's transient, immutable mutation carrier. AuthoringScript remains the
//! durable IR. Presentation borrows captured snapshots; apply never replans.
use super::{
    history::{EditorCommand, History, ModelRevision, PendingCommandQueue},
    identity::{ElementId, ElementIdAllocator},
    modeling::dependency_graph::stamp_authored_entity_dependencies,
};
use crate::{
    authored_entity::BoxedEntity,
    capability_registry::CapabilityRegistry,
    semantics::{
        components::{SemanticGraph, WorldSemanticContext},
        evaluate, Refusal, SemanticPlan, Verdict,
    },
};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PlanId(pub String);
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InteractionId(pub String);

/// Capability-owned diagnostic/context data; never a second authored graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanContext {
    pub planner_id: String,
    pub planner_version: u32,
    pub request_kind: String,
    pub mutation_scope: String,
    pub intent: String,
    pub assumptions: Vec<String>,
    pub unresolved_decisions: Vec<String>,
    pub obligations: Vec<String>,
    pub findings: Vec<String>,
    pub derived_impacts: Vec<String>,
    pub presentation_hints: Vec<String>,
}

#[derive(Debug)]
pub struct AuthoredEditPlan {
    interaction_id: Option<InteractionId>,
    plan_id: PlanId,
    base_model_revision: ModelRevision,
    context: PlanContext,
    before_snapshots: Vec<BoxedEntity>,
    after_snapshots: Vec<BoxedEntity>,
    created_ids: Vec<ElementId>,
    removed_ids: Vec<ElementId>,
    semantic_intents: SemanticPlan,
    can_commit: bool,
    /// Content fingerprint, independent of the immutable candidate identity.
    digest: String,
    retained_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    UnknownPlan,
    UnknownInteraction,
    Stale,
    PendingWork,
    InvalidSnapshots(String),
    Capacity,
    Refused(Vec<Refusal>),
}
impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PlanError {}

impl AuthoredEditPlan {
    pub fn plan_id(&self) -> &PlanId {
        &self.plan_id
    }
    pub fn interaction_id(&self) -> Option<&InteractionId> {
        self.interaction_id.as_ref()
    }
    pub fn base_model_revision(&self) -> &ModelRevision {
        &self.base_model_revision
    }
    pub fn context(&self) -> &PlanContext {
        &self.context
    }
    pub fn before_snapshots(&self) -> &[BoxedEntity] {
        &self.before_snapshots
    }
    pub fn after_snapshots(&self) -> &[BoxedEntity] {
        &self.after_snapshots
    }
    pub fn created_ids(&self) -> &[ElementId] {
        &self.created_ids
    }
    pub fn removed_ids(&self) -> &[ElementId] {
        &self.removed_ids
    }
    pub fn semantic_intents(&self) -> &SemanticPlan {
        &self.semantic_intents
    }
    pub fn can_commit(&self) -> bool {
        self.can_commit
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn capture(
        world: &World,
        interaction_id: Option<InteractionId>,
        base: ModelRevision,
        context: PlanContext,
        before: Vec<BoxedEntity>,
        after: Vec<BoxedEntity>,
        semantic_intents: SemanticPlan,
    ) -> Result<Self, PlanError> {
        fn ids(snapshots: &[BoxedEntity]) -> Result<BTreeSet<ElementId>, PlanError> {
            let set: BTreeSet<_> = snapshots.iter().map(|s| s.element_id()).collect();
            if set.len() != snapshots.len() {
                return Err(PlanError::InvalidSnapshots(
                    "duplicate authored identity".into(),
                ));
            }
            Ok(set)
        }
        if world
            .get_resource::<History>()
            .map(History::revision_token)
            .as_ref()
            != Some(&base)
        {
            return Err(PlanError::Stale);
        }
        let old = ids(&before)?;
        let new = ids(&after)?;
        if new.contains(&ElementId(u64::MAX)) {
            return Err(PlanError::InvalidSnapshots(
                "authored identity cannot exhaust the allocator".into(),
            ));
        }
        let after_by_id: BTreeMap<_, _> = after.iter().map(|s| (s.element_id(), s)).collect();
        for snapshot in &before {
            if let Some(changed) = after_by_id.get(&snapshot.element_id()) {
                if snapshot.type_name() != changed.type_name() {
                    return Err(PlanError::InvalidSnapshots(
                        "retyping requires an explicit capability migration".into(),
                    ));
                }
            }
        }
        let mut plan = Self {
            interaction_id,
            plan_id: PlanId(uuid::Uuid::new_v4().to_string()),
            base_model_revision: base,
            context,
            before_snapshots: before,
            after_snapshots: after,
            created_ids: new.difference(&old).copied().collect(),
            removed_ids: old.difference(&new).copied().collect(),
            semantic_intents,
            can_commit: true,
            digest: String::new(),
            retained_bytes: 0,
        };
        if let Some(graph) = world.get_resource::<SemanticGraph>() {
            match evaluate(
                graph,
                &WorldSemanticContext::new(world),
                &plan.semantic_intents,
            ) {
                Verdict::Refuse(refusals) => {
                    plan.can_commit = false;
                    plan.context
                        .findings
                        .extend(refusals.iter().map(Refusal::summary));
                }
                Verdict::AdmitWithObligation(obligations) => plan
                    .context
                    .obligations
                    .extend(obligations.iter().map(|o| o.summary.clone())),
                Verdict::Admit => {}
            }
        }
        let encoded = serde_json::to_vec(&plan.content())
            .map_err(|e| PlanError::InvalidSnapshots(e.to_string()))?;
        plan.retained_bytes = encoded.len();
        plan.digest = blake3::hash(&encoded).to_hex().to_string();
        Ok(plan)
    }

    /// Normalized digest input. Snapshot application order remains captured in
    /// the carrier; the digest preserves it because dependency order matters.
    pub fn content(&self) -> Value {
        json!({"format":"talos.authored_edit_plan.transient.v1","base_model_revision":self.base_model_revision,
            "context":self.context,"before":self.before_snapshots.iter().map(BoxedEntity::to_json).collect::<Vec<_>>(),
            "after":self.after_snapshots.iter().map(BoxedEntity::to_json).collect::<Vec<_>>(),
            "semantic_intents":self.semantic_intents,"can_commit":self.can_commit})
    }

    fn preflight(&self, world: &World) -> Option<Refusal> {
        let refusal = |reason: &str| Refusal {
            violated: None,
            reason: reason.into(),
            observed: format!("plan {} at {:?}", self.plan_id.0, self.base_model_revision),
            repair: "Inspect the current model and create a new preview.".into(),
            contrasts: Vec::new(),
        };
        if world
            .get_resource::<History>()
            .map(History::revision_token)
            .as_ref()
            != Some(&self.base_model_revision)
        {
            return Some(refusal("The captured edit plan is stale."));
        }
        if !self.can_commit {
            return Some(refusal("The captured plan has unresolved refusals."));
        }
        // Also catch authored changes made by a legacy writer that did not yet
        // advance history. Presentation must restore its transient state first.
        for snapshot in &self.before_snapshots {
            if capture_snapshot(world, snapshot.element_id()).as_ref() != Some(snapshot) {
                return Some(refusal(
                    "The captured before-state no longer matches the model.",
                ));
            }
        }
        for id in &self.created_ids {
            if entity_exists(world, *id) {
                return Some(refusal("A created identity is already in use."));
            }
        }
        None
    }
}

fn entity_exists(world: &World, id: ElementId) -> bool {
    world
        .try_query::<&ElementId>()
        .is_some_and(|mut q| q.iter(world).any(|current| *current == id))
}
pub fn capture_snapshot(world: &World, id: ElementId) -> Option<BoxedEntity> {
    let registry = world.get_resource::<CapabilityRegistry>()?;
    let mut query = world.try_query::<(Entity, &ElementId)>()?;
    let entity = query
        .iter(world)
        .find_map(|(entity, current)| (*current == id).then_some(entity))?;
    registry.capture_snapshot(&world.entity(entity), world)
}

struct Interaction {
    base: ModelRevision,
    active: Option<PlanId>,
    originals: BTreeMap<ElementId, BoxedEntity>,
}

/// At most one candidate per interaction; FIFO eviction bounds both standalone
/// plans and interactions. Evicted, cancelled, superseded or consumed IDs cannot
/// apply. Per-entry byte limits bound retained snapshots and metadata.
#[derive(Resource)]
pub struct AuthoredEditPlanRegistry {
    plans: BTreeMap<PlanId, Arc<AuthoredEditPlan>>,
    interactions: BTreeMap<InteractionId, Interaction>,
    order: VecDeque<PlanId>,
    max_entries: usize,
    max_entry_bytes: usize,
}
impl Default for AuthoredEditPlanRegistry {
    fn default() -> Self {
        Self::with_limits(64, 4 * 1024 * 1024)
    }
}
impl AuthoredEditPlanRegistry {
    pub fn with_limits(max_entries: usize, max_entry_bytes: usize) -> Self {
        Self {
            plans: Default::default(),
            interactions: Default::default(),
            order: Default::default(),
            max_entries,
            max_entry_bytes,
        }
    }
    pub fn begin_interaction(&mut self, base: ModelRevision) -> Result<InteractionId, PlanError> {
        // Old document/revision interactions cannot be resumed. Release them
        // before checking capacity so a load or missed cancellation cannot
        // permanently exhaust the interaction budget.
        let stale: Vec<_> = self
            .interactions
            .iter()
            .filter_map(|(id, interaction)| (interaction.base != base).then_some(id.clone()))
            .collect();
        for id in stale {
            self.cancel(&id);
        }
        if self.interactions.len() >= self.max_entries {
            return Err(PlanError::Capacity);
        }
        let id = InteractionId(uuid::Uuid::new_v4().to_string());
        self.interactions.insert(
            id.clone(),
            Interaction {
                base,
                active: None,
                originals: Default::default(),
            },
        );
        Ok(id)
    }
    pub fn publish(
        &mut self,
        mut plan: AuthoredEditPlan,
    ) -> Result<Arc<AuthoredEditPlan>, PlanError> {
        if self.max_entries == 0 || plan.retained_bytes > self.max_entry_bytes {
            return Err(PlanError::Capacity);
        }
        // Publication assigns the candidate's final identity. Even recovering an
        // Arc after eviction and republishing its content cannot revive an old ID.
        plan.plan_id = PlanId(uuid::Uuid::new_v4().to_string());
        if let Some(id) = &plan.interaction_id {
            let interaction = self
                .interactions
                .get_mut(id)
                .ok_or(PlanError::UnknownInteraction)?;
            if interaction.base != plan.base_model_revision {
                return Err(PlanError::Stale);
            }
            let mut originals = interaction.originals.clone();
            for snapshot in &plan.before_snapshots {
                if originals
                    .get(&snapshot.element_id())
                    .is_some_and(|old| old != snapshot)
                {
                    return Err(PlanError::InvalidSnapshots(
                        "interaction before-state changed".into(),
                    ));
                }
                originals.insert(snapshot.element_id(), snapshot.clone());
            }
            let bytes = serde_json::to_vec(
                &originals
                    .values()
                    .map(BoxedEntity::to_json)
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| PlanError::InvalidSnapshots(e.to_string()))?
            .len();
            if bytes > self.max_entry_bytes {
                return Err(PlanError::Capacity);
            }
            interaction.originals = originals;
            if let Some(old) = interaction.active.replace(plan.plan_id.clone()) {
                self.plans.remove(&old);
                self.order.retain(|p| p != &old);
            }
        }
        while self.plans.len() >= self.max_entries {
            if let Some(old) = self.order.pop_front() {
                if let Some(evicted) = self.plans.remove(&old) {
                    if let Some(id) = &evicted.interaction_id {
                        self.interactions.remove(id);
                    }
                }
            } else {
                return Err(PlanError::Capacity);
            }
        }
        let plan = Arc::new(plan);
        self.order.push_back(plan.plan_id.clone());
        self.plans.insert(plan.plan_id.clone(), plan.clone());
        Ok(plan)
    }
    pub fn get(&self, id: &PlanId) -> Option<Arc<AuthoredEditPlan>> {
        self.plans.get(id).cloned()
    }
    pub fn active(&self, id: &InteractionId) -> Option<Arc<AuthoredEditPlan>> {
        self.interactions
            .get(id)?
            .active
            .as_ref()
            .and_then(|id| self.get(id))
    }
    pub fn cancel(&mut self, id: &InteractionId) {
        if let Some(interaction) = self.interactions.remove(id) {
            if let Some(plan) = interaction.active {
                self.plans.remove(&plan);
                self.order.retain(|p| p != &plan);
            }
        }
    }
    pub fn clear(&mut self) {
        self.plans.clear();
        self.interactions.clear();
        self.order.clear();
    }
    fn consume(&mut self, id: &PlanId) -> Result<Arc<AuthoredEditPlan>, PlanError> {
        let plan = self.plans.remove(id).ok_or(PlanError::UnknownPlan)?;
        self.order.retain(|p| p != id);
        if let Some(interaction) = &plan.interaction_id {
            self.interactions.remove(interaction);
        }
        Ok(plan)
    }
}

/// Queue the captured candidate once. The command repeats its preflight at the
/// actual history drain so an intervening queued edit cannot sneak past it.
pub fn queue_captured_plan(world: &mut World, id: &PlanId) -> Result<(), PlanError> {
    // Keep the captured command a standalone atomic history item. In particular
    // do not let an open legacy group mutate its inputs after group preflight.
    if !world.resource::<PendingCommandQueue>().is_empty() {
        return Err(PlanError::PendingWork);
    }
    let plan = world
        .resource::<AuthoredEditPlanRegistry>()
        .get(id)
        .ok_or(PlanError::UnknownPlan)?;
    if let Some(refusal) = plan.preflight(world) {
        return Err(PlanError::Refused(vec![refusal]));
    }
    let plan = world
        .resource_mut::<AuthoredEditPlanRegistry>()
        .consume(id)?;
    world
        .resource_mut::<PendingCommandQueue>()
        .push_command(Box::new(CapturedPlanCommand(plan)));
    Ok(())
}

struct CapturedPlanCommand(Arc<AuthoredEditPlan>);
impl EditorCommand for CapturedPlanCommand {
    fn label(&self) -> &'static str {
        "Apply captured edit plan"
    }
    fn preflight(&self, world: &World) -> Option<Refusal> {
        self.0.preflight(world)
    }
    fn semantic_plan(&self, _world: &World) -> SemanticPlan {
        self.0.semantic_intents.clone()
    }
    fn apply(&mut self, world: &mut World) {
        apply_snapshots(world, &self.0.after_snapshots, &self.0.before_snapshots);
    }
    fn undo(&mut self, world: &mut World) {
        apply_snapshots(world, &self.0.before_snapshots, &self.0.after_snapshots);
    }
    // Redo deliberately uses captured content rather than the original revision.
}

fn apply_snapshots(world: &mut World, target: &[BoxedEntity], previous: &[BoxedEntity]) {
    let target_ids: BTreeSet<_> = target.iter().map(|s| s.element_id()).collect();
    for snapshot in previous
        .iter()
        .rev()
        .filter(|s| !target_ids.contains(&s.element_id()))
    {
        snapshot.remove_from(world);
    }
    let previous: BTreeMap<_, _> = previous.iter().map(|s| (s.element_id(), s)).collect();
    if let (Some(max_id), Some(mut allocator)) = (
        target.iter().map(|s| s.element_id().0).max(),
        world.get_resource_mut::<ElementIdAllocator>(),
    ) {
        if allocator.next_value() <= max_id {
            allocator.set_next(max_id.saturating_add(1));
        }
    }
    for snapshot in target {
        snapshot.apply_with_previous(world, previous.get(&snapshot.element_id()).copied());
        stamp_authored_entity_dependencies(world, snapshot);
    }
}

#[cfg(test)]
mod tests;
