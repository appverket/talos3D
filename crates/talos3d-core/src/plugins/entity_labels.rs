//! Shared `ElementId` → display-label resolution.
//!
//! Several read-only views (Outliner, Layers panel, Dependency-graph panel and
//! their model-api mirrors) need a human label for each authored entity. They
//! all derive it the same way: through the generic
//! [`CapabilityRegistry::capture_snapshot`] path so any authored entity kind
//! contributes its own `label()` without the caller knowing concrete types.
//! This module owns that single derivation so the surfaces never drift.

use std::collections::HashMap;

use bevy::{ecs::world::EntityRef, prelude::*};

use crate::capability_registry::CapabilityRegistry;
use crate::plugins::identity::ElementId;

/// Resolve a display label for one entity via the capability snapshot path.
/// Returns `None` when there is no `CapabilityRegistry`, no snapshot for the
/// entity, or the snapshot's label is empty.
pub fn entity_label(world: &World, entity: Entity) -> Option<String> {
    let registry = world.get_resource::<CapabilityRegistry>()?;
    let entity_ref: EntityRef = world.get_entity(entity).ok()?;
    let snapshot = registry.capture_snapshot(&entity_ref, world)?;
    let label = snapshot.label();
    (!label.is_empty()).then_some(label)
}

/// Build an `ElementId.0` → label map for every authored entity in the world.
/// Entities without a resolvable label are simply absent from the map; callers
/// fall back to a synthetic `#id` form.
pub fn collect_entity_labels(world: &mut World) -> HashMap<u64, String> {
    let ids: Vec<(u64, Entity)> = {
        let mut query = world.query::<(Entity, &ElementId)>();
        query
            .iter(world)
            .map(|(entity, element_id)| (element_id.0, entity))
            .collect()
    };
    let mut labels = HashMap::with_capacity(ids.len());
    for (eid, entity) in ids {
        if let Some(label) = entity_label(world, entity) {
            labels.insert(eid, label);
        }
    }
    labels
}
