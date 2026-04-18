//! PP73 pavilion integration example.
//!
//! Builds a minimal single-room pavilion (1 slab + 4 walls + 1 shed roof),
//! wires up `bears_on` relations so walls rest on the foundation and the
//! roof rests on the walls, then promotes everything to `Constructible` in
//! dependency order (foundation -> walls -> roof).
//!
//! Run: `cargo run -p talos3d-architectural --example promote_pavilion --features model-api`
//!
//! This exercises the PP70 + PP71 + PP72 + PP73 substrate end to end and is
//! the capstone for the PP70-PP73 slate. It is also the starting point for
//! the MCP validation exercise (the next step after PP70-73 ship).

#![cfg(feature = "model-api")]

use bevy::prelude::*;
use talos3d_architectural::{
    recipes::{
        foundation_slab_on_grade::{foundation_system_class, slab_on_grade_recipe},
        roof_shed_framing::{roof_system_class, shed_roof_framing_recipe},
        wall_light_frame_exterior::{light_frame_exterior_wall_recipe, wall_assembly_class},
    },
    snapshots::WallFactory,
};
use talos3d_core::{
    authored_entity::AuthoredEntity,
    capability_registry::{
        CapabilityRegistry, ElementClassAssignment, ElementClassId, RecipeFamilyId,
        RelationTypeDescriptor,
    },
    plugins::{
        commands::{
            ApplyEntityChangesCommand, BeginCommandGroup, CreateBoxCommand, CreateCylinderCommand,
            CreateEntityCommand, CreatePlaneCommand, CreatePolylineCommand, CreateSphereCommand,
            CreateTriangleMeshCommand, DeleteEntitiesCommand, EndCommandGroup,
            ResolvedDeleteEntitiesCommand,
        },
        history::{History, PendingCommandQueue},
        identity::{ElementId, ElementIdAllocator},
        model_api::{handle_promote_refinement, handle_run_validation},
        modeling::assembly::{AssemblyFactory, RelationFactory, RelationSnapshot, SemanticRelation},
        refinement::{
            ClaimGrounding, ClaimPath, ClaimRecord, Grounding, ObligationSet, ObligationStatus,
            RefinementStateComponent,
        },
    },
};

fn main() {
    println!("=== Pavilion: 6m x 8m, 4 walls, 1 slab, 1 shed roof ===\n");

    let mut world = build_world();

    // -- Foundation --
    let foundation_eid = spawn_tagged(
        &mut world,
        "foundation_system",
        "slab_on_grade",
        "foundation",
    );
    println!("Foundation spawned: eid={}", foundation_eid);

    // -- Walls --
    let walls: [u64; 4] = [
        spawn_tagged(
            &mut world,
            "wall_assembly",
            "light_frame_exterior_wall",
            "wall_N",
        ),
        spawn_tagged(
            &mut world,
            "wall_assembly",
            "light_frame_exterior_wall",
            "wall_E",
        ),
        spawn_tagged(
            &mut world,
            "wall_assembly",
            "light_frame_exterior_wall",
            "wall_S",
        ),
        spawn_tagged(
            &mut world,
            "wall_assembly",
            "light_frame_exterior_wall",
            "wall_W",
        ),
    ];
    for (idx, wid) in walls.iter().enumerate() {
        println!("Wall {} spawned: eid={}", idx, wid);
        spawn_bears_on(&mut world, ElementId(*wid), ElementId(foundation_eid));
    }

    // -- Roof --
    let roof_eid = spawn_tagged(
        &mut world,
        "roof_system",
        "shed_roof_framing",
        "roof",
    );
    // For PP73 simplicity the roof bears on one of the walls (wall_N). The
    // engine marks the `bears_on` obligation SatisfiedBy that wall once the
    // wall is Constructible.
    spawn_bears_on(&mut world, ElementId(roof_eid), ElementId(walls[0]));
    println!("Roof spawned: eid={} (bears_on wall_N)\n", roof_eid);

    // --- Promote in dependency order ---
    println!("--- Promote: foundation ---");
    promote(
        &mut world,
        foundation_eid,
        "slab_on_grade",
        serde_json::json!({
            "footprint_polygon": [[0, 0], [6000, 0], [6000, 8000], [0, 8000]],
            "floor_datum_mm": 0,
            "slab_thickness_mm": 150
        }),
    );

    println!("--- Promote: walls ---");
    for wid in &walls {
        promote(
            &mut world,
            *wid,
            "light_frame_exterior_wall",
            serde_json::json!({"length_mm": 6000, "height_mm": 2700, "thickness_mm": 200}),
        );
        // FRICTION NOTE (for MCP validation exercise):
        // The `bears_on` resolver looks for a `top_datum_mm` claim on the
        // target, but the wall recipe does not yet emit this claim. Seeding it
        // by hand so the roof can bears_on the wall. Fixing this belongs in a
        // wall-recipe refinement: generate should install
        // top_datum_mm = floor_datum + height_mm.
        inject_top_datum_claim(&mut world, *wid, 2700);
    }

    println!("--- Promote: roof ---");
    promote(
        &mut world,
        roof_eid,
        "shed_roof_framing",
        serde_json::json!({
            "footprint_polygon": [[0, 0], [6000, 0], [6000, 8000], [0, 8000]],
            "pitch_deg": 15.0,
            "high_edge_direction": "W",
            "rafter_spacing_mm": 600
        }),
    );

    println!("\n--- Obligation summary ---");
    summarize_obligations(&world, foundation_eid, "foundation");
    for (idx, wid) in walls.iter().enumerate() {
        summarize_obligations(&world, *wid, &format!("wall_{}", "NESW".as_bytes()[idx] as char));
    }
    summarize_obligations(&world, roof_eid, "roof");

    println!("\n--- Validation findings ---");
    for (label, eid) in [
        ("foundation", foundation_eid),
        ("wall_N", walls[0]),
        ("wall_E", walls[1]),
        ("wall_S", walls[2]),
        ("wall_W", walls[3]),
        ("roof", roof_eid),
    ] {
        let findings = handle_run_validation(&world, eid).expect("run_validation");
        if findings.is_empty() {
            println!("  {:12} : OK", label);
        } else {
            for f in findings {
                println!("  {:12} : {:?} {}", label, f.severity, f.message);
            }
        }
    }

    println!("\nPavilion assembled.");
}

// -- helpers --

fn build_world() -> World {
    let mut world = World::new();
    world.insert_resource(Messages::<CreateBoxCommand>::default());
    world.insert_resource(Messages::<CreateCylinderCommand>::default());
    world.insert_resource(Messages::<CreateSphereCommand>::default());
    world.insert_resource(Messages::<CreatePlaneCommand>::default());
    world.insert_resource(Messages::<CreatePolylineCommand>::default());
    world.insert_resource(Messages::<CreateTriangleMeshCommand>::default());
    world.insert_resource(Messages::<CreateEntityCommand>::default());
    world.insert_resource(Messages::<DeleteEntitiesCommand>::default());
    world.insert_resource(Messages::<ResolvedDeleteEntitiesCommand>::default());
    world.insert_resource(Messages::<ApplyEntityChangesCommand>::default());
    world.insert_resource(Messages::<BeginCommandGroup>::default());
    world.insert_resource(Messages::<EndCommandGroup>::default());
    world.insert_resource(PendingCommandQueue::default());
    world.insert_resource(History::default());
    world.insert_resource(ElementIdAllocator::default());

    let mut registry = CapabilityRegistry::default();
    registry.register_element_class(foundation_system_class());
    registry.register_element_class(wall_assembly_class());
    registry.register_element_class(roof_system_class());
    registry.register_recipe_family(slab_on_grade_recipe());
    registry.register_recipe_family(light_frame_exterior_wall_recipe());
    registry.register_recipe_family(shed_roof_framing_recipe());

    for (rt, lbl, desc) in [
        ("refinement_of", "Refinement Of", "child -> parent"),
        ("refined_into", "Refined Into", "parent -> child"),
        ("bears_on", "Bears On", "structural load path"),
    ] {
        registry.register_relation_type(RelationTypeDescriptor {
            relation_type: rt.into(),
            label: lbl.into(),
            description: desc.into(),
            valid_source_types: vec![],
            valid_target_types: vec![],
            parameter_schema: serde_json::json!({}),
            participates_in_dependency_graph: false,
        });
    }

    registry.register_factory(WallFactory);
    registry.register_factory(AssemblyFactory);
    registry.register_factory(RelationFactory);
    world.insert_resource(registry);
    world
}

fn spawn_tagged(world: &mut World, class_id: &str, recipe_id: &str, _label: &str) -> u64 {
    let eid = world.resource::<ElementIdAllocator>().next_id();
    world.spawn((
        eid,
        ElementClassAssignment {
            element_class: ElementClassId(class_id.into()),
            active_recipe: Some(RecipeFamilyId(recipe_id.into())),
        },
    ));
    eid.0
}

fn inject_top_datum_claim(world: &mut World, eid: u64, value_mm: i64) {
    let element_id = ElementId(eid);
    let entity = {
        let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
        q.iter(world)
            .find(|(_, id)| **id == element_id)
            .map(|(e, _)| e)
            .expect("entity exists")
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut claims = std::collections::HashMap::new();
    claims.insert(
        ClaimPath("top_datum_mm".into()),
        ClaimRecord {
            grounding: Grounding::Refined(eid),
            set_at: now,
            set_by: None,
        },
    );
    world.entity_mut(entity).insert(ClaimGrounding { claims });
    let _ = value_mm; // top_datum_mm is stored in the grounding, value lookup is TBD
}

fn spawn_bears_on(world: &mut World, source: ElementId, target: ElementId) {
    let rel_eid = world.resource::<ElementIdAllocator>().next_id();
    let snapshot = RelationSnapshot {
        element_id: rel_eid,
        relation: SemanticRelation {
            source,
            target,
            relation_type: "bears_on".into(),
            parameters: serde_json::json!({}),
        },
    };
    snapshot.apply_to(world);
}

fn promote(world: &mut World, eid: u64, recipe: &str, overrides: serde_json::Value) {
    let result = handle_promote_refinement(
        world,
        eid,
        "Constructible".into(),
        Some(recipe.into()),
        overrides,
    );
    match result {
        Ok(r) => println!(
            "  eid={} {} -> {}",
            eid, r.previous_state, r.new_state
        ),
        Err(e) => println!("  eid={} FAILED: {}", eid, e),
    }
}

fn summarize_obligations(world: &World, eid: u64, label: &str) {
    let element_id = ElementId(eid);
    let mut q = world
        .try_query::<(bevy::prelude::EntityRef,)>()
        .unwrap();
    let (obligations, state) = q
        .iter(world)
        .find_map(|(eref,)| {
            if eref.get::<ElementId>().copied() != Some(element_id) {
                return None;
            }
            let obligations = eref.get::<ObligationSet>().cloned();
            let state = eref
                .get::<RefinementStateComponent>()
                .map(|c| c.state)
                .unwrap_or_default();
            Some((obligations, state))
        })
        .unwrap_or((None, Default::default()));

    let set = match obligations {
        Some(s) => s,
        None => {
            println!("  {:12} state={} (no obligations)", label, state.as_str());
            return;
        }
    };
    let total = set.entries.len();
    let satisfied = set
        .entries
        .iter()
        .filter(|o| matches!(o.status, ObligationStatus::SatisfiedBy(_)))
        .count();
    let unresolved = set
        .entries
        .iter()
        .filter(|o| matches!(o.status, ObligationStatus::Unresolved))
        .count();
    println!(
        "  {:12} state={:15} obligations: {}/{} satisfied, {} unresolved",
        label,
        state.as_str(),
        satisfied,
        total,
        unresolved
    );
}
