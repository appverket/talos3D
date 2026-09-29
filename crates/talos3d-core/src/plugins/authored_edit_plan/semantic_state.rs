//! Captured views of existing semantic components, held by AuthoredEditPlan.
//! These are transient before/after snapshots, not another authored graph.
use super::*;
use crate::semantics::{
    BindTarget, ConceptAssignment, PlanIntent, PublishedAnchor, PublishedAnchors, SemanticBinding,
    SemanticBindings,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(super) struct State {
    concept: Option<ConceptAssignment>,
    published_anchors: Option<PublishedAnchors>,
    bindings: Option<SemanticBindings>,
}
impl State {
    fn capture(world: &World, id: ElementId) -> Option<Self> {
        let entity = crate::plugins::commands::find_entity_by_element_id_readonly(world, id)?;
        Some(Self {
            concept: world.get::<ConceptAssignment>(entity).cloned(),
            published_anchors: world.get::<PublishedAnchors>(entity).cloned(),
            bindings: world.get::<SemanticBindings>(entity).cloned(),
        })
    }
    fn apply(&self, world: &mut World, id: ElementId) {
        let Some(entity) = crate::plugins::commands::find_entity_by_element_id_readonly(world, id)
        else {
            return;
        };
        let mut entity = world.entity_mut(entity);
        entity.remove::<(ConceptAssignment, PublishedAnchors, SemanticBindings)>();
        if let Some(value) = &self.concept {
            entity.insert(value.clone());
        }
        if let Some(value) = &self.published_anchors {
            entity.insert(value.clone());
        }
        if let Some(value) = &self.bindings {
            entity.insert(value.clone());
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct Change {
    element_id: ElementId,
    before: State,
    after: State,
    existed: bool,
}
impl Change {
    pub(super) fn matches_before(&self, world: &World) -> bool {
        if self.existed {
            State::capture(world, self.element_id).as_ref() == Some(&self.before)
        } else {
            State::capture(world, self.element_id).is_none()
        }
    }
    pub(super) fn apply(&self, world: &mut World, undo: bool) {
        if self.before != self.after {
            if undo { &self.before } else { &self.after }.apply(world, self.element_id);
        }
    }
}

pub(super) fn capture(
    world: &World,
    plan: &SemanticPlan,
    created: &[ElementId],
    admitted: bool,
) -> Result<Vec<Change>, PlanError> {
    let mut ids = BTreeSet::new();
    for intent in &plan.intents {
        match intent {
            PlanIntent::AssignConcept { entity, .. }
            | PlanIntent::RemoveConcept { entity, .. }
            | PlanIntent::PublishAnchors { entity, .. } => {
                ids.insert(*entity);
            }
            PlanIntent::Bind {
                subject, target, ..
            } => {
                ids.insert(*subject);
                ids.insert(match target {
                    BindTarget::Entity(id) => *id,
                    BindTarget::Anchor(anchor) => anchor.publisher,
                });
            }
        }
    }
    let mut changes = BTreeMap::new();
    for id in ids {
        let current = State::capture(world, id);
        if current.is_none() && !created.contains(&id) {
            return Err(PlanError::InvalidSnapshots(format!(
                "Unknown semantic entity {}",
                id.0
            )));
        }
        let existed = current.is_some();
        let before = current.unwrap_or_default();
        changes.insert(
            id,
            Change {
                element_id: id,
                after: before.clone(),
                before,
                existed,
            },
        );
    }
    for intent in &plan.intents {
        match intent {
            PlanIntent::AssignConcept { entity, concept } => {
                changes.get_mut(entity).unwrap().after.concept =
                    Some(ConceptAssignment::new(concept.clone()));
            }
            PlanIntent::RemoveConcept { entity, concept } => {
                let state = &mut changes.get_mut(entity).unwrap().after;
                if state
                    .concept
                    .as_ref()
                    .is_some_and(|assignment| &assignment.concept != concept)
                {
                    return Err(PlanError::InvalidSnapshots(
                        "Downgrade names a different current concept".into(),
                    ));
                }
                state.concept = None;
                state.published_anchors = None;
                state.bindings = None;
            }
            PlanIntent::PublishAnchors { entity, anchors } => {
                let state = &mut changes.get_mut(entity).unwrap().after;
                let mut unique = BTreeSet::new();
                let mut published = Vec::new();
                for anchor in anchors {
                    if !unique.insert((&anchor.kind, &anchor.role)) {
                        return Err(PlanError::InvalidSnapshots(
                            "Duplicate anchor identity".into(),
                        ));
                    }
                    let revision = state
                        .published_anchors
                        .as_ref()
                        .and_then(|old| old.revision_of(&anchor.kind, &anchor.role))
                        .unwrap_or(0);
                    published.push(PublishedAnchor {
                        kind: anchor.kind.clone(),
                        role: anchor.role.clone(),
                        revision,
                    });
                }
                state.published_anchors = Some(PublishedAnchors::new(published));
            }
            PlanIntent::Bind {
                subject,
                predicate,
                target: BindTarget::Anchor(anchor),
            } => {
                let bindings = changes
                    .get_mut(subject)
                    .unwrap()
                    .after
                    .bindings
                    .get_or_insert_with(SemanticBindings::default);
                let binding = SemanticBinding {
                    predicate: predicate.clone(),
                    anchor_publisher: anchor.publisher.0,
                    anchor_kind: anchor.kind.clone(),
                    anchor_role: anchor.role.clone(),
                };
                if !bindings.bindings.contains(&binding) {
                    bindings.bindings.push(binding);
                }
            }
            PlanIntent::Bind {
                target: BindTarget::Entity(_),
                ..
            } if admitted => {
                return Err(PlanError::InvalidSnapshots("Captured semantic bindings require an explicit published anchor. Use the relation entity planner for bare-entity relations.".into()));
            }
            PlanIntent::Bind { .. } => {}
        }
    }
    if admitted {
        let graph = world.get_resource::<crate::semantics::SemanticGraph>();
        for intent in &plan.intents {
            if let PlanIntent::Bind {
                subject, predicate, ..
            } = intent
            {
                let concept = changes[subject].after.concept.as_ref().ok_or_else(|| {
                    PlanError::InvalidSnapshots("An authored anchor binding needs a declared subject concept; resolve the term and assign it first.".into())
                })?;
                use crate::semantics::SemanticContext;
                let jurisdiction =
                    crate::semantics::WorldSemanticContext::new(world).jurisdiction();
                if graph.is_none_or(|graph| {
                    graph
                        .propositions_for(&concept.concept, predicate, jurisdiction.as_ref())
                        .is_empty()
                }) {
                    return Err(PlanError::InvalidSnapshots(format!(
                        "No registered proposition admits `{predicate}` for `{}`. Resolve the term and use its declared predicate, or raise a corpus gap.", concept.concept
                    )));
                }
            }
        }
    }
    Ok(changes.into_values().collect())
}
