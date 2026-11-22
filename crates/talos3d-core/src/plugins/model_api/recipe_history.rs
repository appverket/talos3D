//! Transient metadata capture for a recipe's newly created entities. Geometry,
//! Definitions and occurrences retain their existing command implementations.
//! The enclosing HistoryTransaction makes these and final annotations one edit.
use super::*;
use crate::capability_registry::ElementClassAssignment;
use crate::plugins::history::{EditorCommand, PendingCommandQueue};
use crate::plugins::refinement::{
    AuthoringMode, AuthoringProvenance, ClaimGrounding, ObligationSet, RecipeId,
    RefinementStateComponent, SemanticIntent, SettingOutContract,
};

#[derive(Clone)]
struct Metadata {
    id: ElementId,
    class: Option<ElementClassAssignment>,
    state: Option<RefinementStateComponent>,
    intent: Option<SemanticIntent>,
    provenance: Option<AuthoringProvenance>,
    obligations: Option<ObligationSet>,
    grounding: Option<ClaimGrounding>,
    setting_out: Option<SettingOutContract>,
}

impl Metadata {
    fn capture(world: &World, id: ElementId) -> Self {
        let entity = find_entity_by_element_id_readonly(world, id)
            .expect("recipe output exists before finalization");
        Self {
            id,
            class: world.get::<ElementClassAssignment>(entity).cloned(),
            state: world.get::<RefinementStateComponent>(entity).cloned(),
            intent: world.get::<SemanticIntent>(entity).cloned(),
            provenance: world.get::<AuthoringProvenance>(entity).cloned(),
            obligations: world.get::<ObligationSet>(entity).cloned(),
            grounding: world.get::<ClaimGrounding>(entity).cloned(),
            setting_out: world.get::<SettingOutContract>(entity).cloned(),
        }
    }

    fn apply(&self, world: &mut World) {
        let Some(entity) = find_entity_by_element_id_readonly(world, self.id) else {
            return;
        };
        let mut entity = world.entity_mut(entity);
        // Absence is part of the capture: the tiny locator must not regain the
        // aggregate's class/parameters when earlier creation commands redo.
        entity.remove::<(
            ElementClassAssignment,
            RefinementStateComponent,
            SemanticIntent,
            AuthoringProvenance,
            ObligationSet,
            ClaimGrounding,
            SettingOutContract,
        )>();
        macro_rules! restore {
            ($($field:ident),+ $(,)?) => {$(
                if let Some(value) = &self.$field { entity.insert(value.clone()); }
            )+};
        }
        restore!(
            class,
            state,
            intent,
            provenance,
            obligations,
            grounding,
            setting_out
        );
    }
}

/// Preserve the locator's identity and semantics without manufacturing geometry.
/// A type transition must remove the old representation: applying a group
/// snapshot over an existing box would leave the box's physical components.
struct FinalizeLocator {
    before: BoxedEntity,
    after: BoxedEntity,
    metadata: Metadata,
}
impl EditorCommand for FinalizeLocator {
    fn label(&self) -> &'static str {
        "Finalize recipe locator"
    }
    fn apply(&mut self, world: &mut World) {
        self.before.remove_from(world);
        self.after.apply_to(world);
        self.metadata.apply(world);
    }
    fn undo(&mut self, world: &mut World) {
        self.after.remove_from(world);
        self.before.apply_to(world);
        self.metadata.apply(world);
    }
}

pub(super) fn finalize_locator(world: &mut World, root: ElementId) -> ApiResult<()> {
    let before = capture_snapshot_by_id(world, root)?;
    let metadata = Metadata::capture(world, root);
    let after = physical_group_snapshot(world, root, "Recipe locator".into(), vec![]);
    world
        .resource_mut::<PendingCommandQueue>()
        .push_command(Box::new(FinalizeLocator {
            before,
            after,
            metadata,
        }));
    flush_model_api_write_pipeline(world);
    Ok(())
}

struct FinalizeRecipeMetadata {
    before: Vec<Metadata>,
    after: Vec<Metadata>,
}

impl EditorCommand for FinalizeRecipeMetadata {
    fn label(&self) -> &'static str {
        "Finalize recipe annotations"
    }
    fn apply(&mut self, world: &mut World) {
        for state in &self.after {
            state.apply(world);
        }
    }
    fn undo(&mut self, world: &mut World) {
        for state in &self.before {
            state.apply(world);
        }
    }
}

/// Only newly created outputs are accepted by the caller. Always capture all
/// their final metadata, even unchanged values: creation snapshots from legacy
/// factories do not include subsequent semantic annotations.
pub(super) fn finalize(
    world: &mut World,
    root: ElementId,
    members: &[u64],
    group: Option<ElementId>,
    family_id: &str,
) {
    let ids: Vec<_> = std::iter::once(root)
        .chain(members.iter().copied().map(ElementId))
        .chain(group)
        .collect();
    let before = ids.iter().map(|id| Metadata::capture(world, *id)).collect();
    if let Some(group) = group {
        move_recipe_semantics_from_anchor_to_group(world, root, group);
    }
    for id in &ids {
        let entity = find_entity_by_element_id_readonly(world, *id).expect("recipe output exists");
        let rationale = world
            .get::<AuthoringProvenance>(entity)
            .and_then(|provenance| provenance.rationale.clone());
        world.entity_mut(entity).insert(AuthoringProvenance {
            mode: AuthoringMode::ViaRecipe(RecipeId(family_id.into())),
            rationale,
        });
    }
    let after = ids.iter().map(|id| Metadata::capture(world, *id)).collect();
    world
        .resource_mut::<PendingCommandQueue>()
        .push_command(Box::new(FinalizeRecipeMetadata { before, after }));
    flush_model_api_write_pipeline(world);
}
