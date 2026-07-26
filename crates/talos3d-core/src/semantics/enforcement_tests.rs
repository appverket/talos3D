//! Enforcement tests: the kernel must bite through the *command pipeline*,
//! not merely evaluate correctly in isolation.
//!
//! ADR-064 §3 places the gate at `PendingCommandQueue` because that is the one
//! point interactive tools, command invocation, recipes, imports and MCP all
//! converge on. These tests exercise that path — a passing unit test on the
//! kernel would not have caught a gate that was never wired in.

use bevy::prelude::*;

use crate::plugins::history::{
    apply_pending_history_commands_for_test, EditorCommand, History, PendingCommandQueue,
    SemanticEnforcement,
};
use crate::plugins::identity::ElementId;

use super::components::{ConceptAssignment, PublishedAnchor, PublishedAnchors, SemanticGraph};
use super::ids::{AnchorKindId, AnchorRoleId, ConceptId, PredicateId};
use super::plan::{AnchorInstanceId, BindTarget, PlanIntent, SemanticPlan};
use super::test_fixtures::{
    roof_edge_fixture, BARGEBOARD, RAKE_EDGE, ROOF_SYSTEM, WALL_CLADDING, WALL_CORNER_ARRIS,
};

/// A command that declares a binding and records whether it actually ran.
struct BindCommand {
    subject: ElementId,
    target: BindTarget,
    applied: bool,
}

impl EditorCommand for BindCommand {
    fn label(&self) -> &'static str {
        "Bind trim"
    }

    fn apply(&mut self, _world: &mut World) {
        self.applied = true;
    }

    fn undo(&mut self, _world: &mut World) {
        self.applied = false;
    }

    fn semantic_plan(&self, _world: &World) -> SemanticPlan {
        SemanticPlan::none().with(PlanIntent::Bind {
            subject: self.subject,
            predicate: PredicateId::new("follows"),
            target: self.target.clone(),
        })
    }
}

/// A geometry-only command: declares nothing, so the kernel stays disarmed.
#[derive(Default)]
struct GeometryOnlyCommand {
    applied: bool,
}

impl EditorCommand for GeometryOnlyCommand {
    fn label(&self) -> &'static str {
        "Move geometry"
    }

    fn apply(&mut self, _world: &mut World) {
        self.applied = true;
    }

    fn undo(&mut self, _world: &mut World) {
        self.applied = false;
    }
}

fn spawn(world: &mut World, id: u64, concept: &str, anchors: &[(&str, &str)]) {
    let published = PublishedAnchors::new(anchors.iter().map(|(kind, role)| PublishedAnchor {
        kind: AnchorKindId::new(*kind),
        role: AnchorRoleId::new(*role),
        revision: 0,
    }));
    world.spawn((ElementId(id), ConceptAssignment::new(concept), published));
}

fn world_with_graph() -> World {
    let mut world = World::new();
    world.init_resource::<History>();
    world.init_resource::<PendingCommandQueue>();
    world.init_resource::<SemanticEnforcement>();
    world.insert_resource(SemanticGraph(roof_edge_fixture()));

    spawn(
        &mut world,
        388,
        ROOF_SYSTEM,
        &[(RAKE_EDGE, "north_west"), (RAKE_EDGE, "north_east")],
    );
    spawn(&mut world, 412, WALL_CLADDING, &[(WALL_CORNER_ARRIS, "ne")]);
    spawn(&mut world, 511, BARGEBOARD, &[]);
    world
}

fn queue(world: &mut World, command: Box<dyn EditorCommand>) {
    world
        .resource_mut::<PendingCommandQueue>()
        .push_command(command);
}

#[test]
fn a_refused_command_never_applies_and_never_enters_history() {
    let mut world = world_with_graph();
    queue(
        &mut world,
        Box::new(BindCommand {
            subject: ElementId(511),
            target: BindTarget::Entity(ElementId(412)),
            applied: false,
        }),
    );

    apply_pending_history_commands_for_test(&mut world);

    // The world must be untouched: a refusal that leaves partial state behind
    // is worse than no enforcement at all.
    assert_eq!(
        world.resource::<History>().undo_stack_len(),
        0,
        "refused command must not enter history"
    );
    let enforcement = world.resource::<SemanticEnforcement>();
    assert_eq!(enforcement.refusals.len(), 1);
    let refusal = enforcement.last_refusal().unwrap();
    assert!(refusal.repair.contains(ROOF_SYSTEM));
}

#[test]
fn a_valid_binding_applies_and_enters_history() {
    let mut world = world_with_graph();
    queue(
        &mut world,
        Box::new(BindCommand {
            subject: ElementId(511),
            target: BindTarget::Anchor(AnchorInstanceId::new(
                ElementId(388),
                RAKE_EDGE,
                "north_west",
            )),
            applied: false,
        }),
    );

    apply_pending_history_commands_for_test(&mut world);

    assert_eq!(world.resource::<History>().undo_stack_len(), 1);
    assert!(world.resource::<SemanticEnforcement>().refusals.is_empty());
}

#[test]
fn geometry_only_commands_are_unaffected_by_the_kernel() {
    let mut world = world_with_graph();
    queue(&mut world, Box::<GeometryOnlyCommand>::default());

    apply_pending_history_commands_for_test(&mut world);

    // The additive default is what keeps the existing command population
    // working; if this regresses, every unmigrated command starts refusing.
    assert_eq!(world.resource::<History>().undo_stack_len(), 1);
    assert!(world.resource::<SemanticEnforcement>().refusals.is_empty());
}

#[test]
fn authoring_still_works_when_no_concept_graph_is_installed() {
    // A build without a domain pack must keep authoring, not refuse
    // everything.
    let mut world = World::new();
    world.init_resource::<History>();
    world.init_resource::<PendingCommandQueue>();
    world.init_resource::<SemanticEnforcement>();
    world.spawn((ElementId(511), ConceptAssignment::new(BARGEBOARD)));

    queue(
        &mut world,
        Box::new(BindCommand {
            subject: ElementId(511),
            target: BindTarget::Entity(ElementId(412)),
            applied: false,
        }),
    );
    apply_pending_history_commands_for_test(&mut world);

    assert_eq!(world.resource::<History>().undo_stack_len(), 1);
}

#[test]
fn unclassified_geometry_is_not_governed_through_the_pipeline() {
    let mut world = world_with_graph();
    world.spawn(ElementId(999)); // no ConceptAssignment

    queue(
        &mut world,
        Box::new(BindCommand {
            subject: ElementId(999),
            target: BindTarget::Entity(ElementId(412)),
            applied: false,
        }),
    );
    apply_pending_history_commands_for_test(&mut world);

    assert_eq!(world.resource::<History>().undo_stack_len(), 1);
    assert!(world.resource::<SemanticEnforcement>().refusals.is_empty());
}

#[test]
fn published_anchors_survive_a_round_trip_through_the_context() {
    use super::plan::SemanticContext;
    let world = world_with_graph();
    let context = super::components::WorldSemanticContext::new(&world);

    let anchors = context.published_anchors(ElementId(388));
    assert_eq!(anchors.len(), 2);
    assert!(anchors.contains(&AnchorInstanceId::new(
        ElementId(388),
        RAKE_EDGE,
        "north_west"
    )));
    assert_eq!(
        context.concept_of(ElementId(388)),
        Some(ConceptId::new(ROOF_SYSTEM))
    );
    assert_eq!(context.concept_of(ElementId(4242)), None);
}
