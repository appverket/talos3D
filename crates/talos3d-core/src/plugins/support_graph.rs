//! Generic support/attachment relation vocabulary and traversal helpers.
//!
//! Per ADR-037, this module owns the reusable mechanism for reasoning about
//! support graphs. Domain capabilities still decide which members may support
//! which other members and which roots count as stable.

use std::collections::{HashSet, VecDeque};

use bevy::{app::App, ecs::world::EntityRef, prelude::*};

use crate::{
    capability_registry::{
        CapabilityRegistryAppExt, ElementClassAssignment, RelationTypeDescriptor,
    },
    plugins::{identity::ElementId, modeling::assembly::SemanticRelation},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportPathStep {
    pub relation_type: String,
    pub source: ElementId,
    pub target: ElementId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportPath {
    pub terminal: ElementId,
    pub steps: Vec<SupportPathStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupportPathError {
    NoPath,
    CycleDetected,
    DanglingTarget(ElementId),
}

/// Register domain-neutral support/attachment relations so all model-api
/// clients can discover and use the same generic vocabulary.
pub fn register_support_graph_relations(app: &mut App) {
    app.register_relation_type(RelationTypeDescriptor {
        relation_type: "supported_by".to_string(),
        label: "Supported By".to_string(),
        description:
            "Support relation: the source member or layer is physically carried by the target."
                .to_string(),
        valid_source_types: Vec::new(),
        valid_target_types: Vec::new(),
        parameter_schema: serde_json::json!({}),
        participates_in_dependency_graph: true,
    });
    app.register_relation_type(RelationTypeDescriptor {
        relation_type: "fastened_to".to_string(),
        label: "Fastened To".to_string(),
        description:
            "Attachment relation: the source member or layer is mechanically fixed to the target."
                .to_string(),
        valid_source_types: Vec::new(),
        valid_target_types: Vec::new(),
        parameter_schema: serde_json::json!({}),
        participates_in_dependency_graph: false,
    });
    app.register_relation_type(RelationTypeDescriptor {
        relation_type: "hangs_from".to_string(),
        label: "Hangs From".to_string(),
        description: "Support relation: the source member is suspended from the target."
            .to_string(),
        valid_source_types: Vec::new(),
        valid_target_types: Vec::new(),
        parameter_schema: serde_json::json!({}),
        participates_in_dependency_graph: true,
    });
    app.register_relation_type(RelationTypeDescriptor {
        relation_type: "spans_between".to_string(),
        label: "Spans Between".to_string(),
        description: "Support relation: the source member spans to the target support. Use multiple edges when a member spans between multiple supports."
            .to_string(),
        valid_source_types: Vec::new(),
        valid_target_types: Vec::new(),
        parameter_schema: serde_json::json!({}),
        participates_in_dependency_graph: true,
    });
    app.register_relation_type(RelationTypeDescriptor {
        relation_type: "braced_by".to_string(),
        label: "Braced By".to_string(),
        description: "Stability relation: the source member or assembly is laterally stabilized by the target."
            .to_string(),
        valid_source_types: Vec::new(),
        valid_target_types: Vec::new(),
        parameter_schema: serde_json::json!({}),
        participates_in_dependency_graph: false,
    });
}

/// Follow support relations from `start` until `is_root` succeeds.
///
/// The caller decides what counts as a stable root. In architecture that is
/// often a foundation or a bearing assembly already known to be supported.
pub fn resolve_support_path<F>(
    world: &World,
    start: ElementId,
    relation_types: &[&str],
    mut is_root: F,
) -> Result<SupportPath, SupportPathError>
where
    F: FnMut(ElementId, Option<&str>) -> bool,
{
    let mut frontier = VecDeque::from([(start, Vec::<SupportPathStep>::new())]);
    let mut visited: HashSet<u64> = HashSet::new();
    let mut saw_cycle = false;

    while let Some((current, path)) = frontier.pop_front() {
        if !visited.insert(current.0) {
            saw_cycle = true;
            continue;
        }

        let current_class = entity_class(world, current);
        if is_root(current, current_class.as_deref()) {
            return Ok(SupportPath {
                terminal: current,
                steps: path,
            });
        }

        let mut found_edge = false;
        for relation in all_relations(world) {
            if relation.source != current
                || !relation_types
                    .iter()
                    .any(|candidate| *candidate == relation.relation_type)
            {
                continue;
            }
            found_edge = true;
            if !entity_exists(world, relation.target) {
                return Err(SupportPathError::DanglingTarget(relation.target));
            }

            let mut next_path = path.clone();
            next_path.push(SupportPathStep {
                relation_type: relation.relation_type.clone(),
                source: relation.source,
                target: relation.target,
            });
            frontier.push_back((relation.target, next_path));
        }

        if !found_edge && current != start {
            continue;
        }
    }

    if saw_cycle {
        Err(SupportPathError::CycleDetected)
    } else {
        Err(SupportPathError::NoPath)
    }
}

fn all_relations(world: &World) -> Vec<SemanticRelation> {
    let Some(mut query) = world.try_query::<EntityRef>() else {
        return Vec::new();
    };
    let mut relations = Vec::new();
    for entity_ref in query.iter(world) {
        if let Some(relation) = entity_ref.get::<SemanticRelation>() {
            relations.push(relation.clone());
        }
    }
    relations
}

fn entity_exists(world: &World, id: ElementId) -> bool {
    let Some(mut query) = world.try_query::<EntityRef>() else {
        return false;
    };
    for entity_ref in query.iter(world) {
        if entity_ref.get::<ElementId>().copied() == Some(id) {
            return true;
        }
    }
    false
}

fn entity_class(world: &World, id: ElementId) -> Option<String> {
    let Some(mut query) = world.try_query::<EntityRef>() else {
        return None;
    };
    for entity_ref in query.iter(world) {
        if entity_ref.get::<ElementId>().copied() != Some(id) {
            continue;
        }
        return entity_ref
            .get::<ElementClassAssignment>()
            .map(|assignment| assignment.element_class.0.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability_registry::{CapabilityRegistry, ElementClassAssignment, ElementClassId},
        plugins::identity::{ElementId, ElementIdAllocator},
    };

    fn spawn_entity(world: &mut World, class: &str) -> ElementId {
        let element_id = world.resource::<ElementIdAllocator>().next_id();
        world.spawn((
            element_id,
            ElementClassAssignment {
                element_class: ElementClassId(class.to_string()),
                active_recipe: None,
            },
        ));
        element_id
    }

    fn spawn_relation(
        world: &mut World,
        source: ElementId,
        target: ElementId,
        relation_type: &str,
    ) {
        let rel_id = world.resource::<ElementIdAllocator>().next_id();
        world.spawn((
            rel_id,
            SemanticRelation {
                source,
                target,
                relation_type: relation_type.to_string(),
                parameters: serde_json::json!({}),
            },
        ));
    }

    #[test]
    fn resolve_support_path_follows_supported_by_chain_to_root() {
        let mut world = World::new();
        world.insert_resource(CapabilityRegistry::default());
        world.insert_resource(ElementIdAllocator::default());

        let cladding = spawn_entity(&mut world, "cladding");
        let battens = spawn_entity(&mut world, "battens");
        let studs = spawn_entity(&mut world, "wall_assembly");

        spawn_relation(&mut world, cladding, battens, "supported_by");
        spawn_relation(&mut world, battens, studs, "supported_by");

        let path = resolve_support_path(&world, cladding, &["supported_by"], |eid, class| {
            eid == studs || class == Some("wall_assembly")
        })
        .expect("support path should resolve");

        assert_eq!(path.terminal, studs);
        assert_eq!(path.steps.len(), 2);
        assert_eq!(path.steps[0].source, cladding);
        assert_eq!(path.steps[1].target, studs);
    }

    #[test]
    fn resolve_support_path_reports_cycle() {
        let mut world = World::new();
        world.insert_resource(CapabilityRegistry::default());
        world.insert_resource(ElementIdAllocator::default());

        let a = spawn_entity(&mut world, "member");
        let b = spawn_entity(&mut world, "member");
        spawn_relation(&mut world, a, b, "supported_by");
        spawn_relation(&mut world, b, a, "supported_by");

        let error = resolve_support_path(&world, a, &["supported_by"], |_eid, _class| false)
            .expect_err("cycle should fail");
        assert_eq!(error, SupportPathError::CycleDetected);
    }
}
