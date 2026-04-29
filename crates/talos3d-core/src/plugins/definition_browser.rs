use std::collections::HashMap;

use bevy::{ecs::world::EntityRef, prelude::*};
use bevy_egui::{egui, EguiContexts};
use serde_json::{json, Value};

use crate::{
    authored_entity::BoxedEntity,
    capability_registry::CapabilityRegistry,
    plugins::{
        command_registry::{
            queue_command_invocation_resource, CommandResult, PendingCommandInvocations,
        },
        commands::{enqueue_create_boxed_entity, enqueue_create_definition},
        cursor::CursorWorldPos,
        definition_authoring::{
            apply_patch_to_draft, blank_definition,
            draft_effective_definition, preview_registry_for_draft, validate_draft,
            DefinitionDraft, DefinitionDraftId, DefinitionDraftRegistry, DefinitionPatch,
        },
        definition_preview_scene::{
            draw_definition_3d_preview, DefinitionPreviewScene, PendingPreviewClick,
        },
        history::apply_pending_history_commands,
        identity::{ElementId, ElementIdAllocator},
        modeling::{
            assembly::{RelationSnapshot, SemanticRelation},
            definition::{
                Definition, DefinitionId, DefinitionKind, DefinitionLibraryId,
                DefinitionLibraryRegistry, DefinitionRegistry, OverrideMap,
            },
            occurrence::{
                HostedAnchor, HostedOccurrenceContext, OccurrenceIdentity, OccurrenceSnapshot,
            },
        },
        selection::Selected,
        ui::{tool_window_bounds, tool_window_max_size, tool_window_rect, StatusBarData},
    },
};

const DEFINITIONS_WINDOW_DEFAULT_SIZE: egui::Vec2 = egui::vec2(520.0, 540.0);
const DEFINITIONS_WINDOW_MIN_SIZE: egui::Vec2 = egui::vec2(400.0, 320.0);
const DEFINITIONS_WINDOW_MAX_SIZE: egui::Vec2 = egui::vec2(620.0, 680.0);
const INSPECTOR_WINDOW_DEFAULT_SIZE: egui::Vec2 = egui::vec2(620.0, 620.0);
const INSPECTOR_WINDOW_MIN_SIZE: egui::Vec2 = egui::vec2(460.0, 360.0);
const INSPECTOR_WINDOW_MAX_SIZE: egui::Vec2 = egui::vec2(760.0, 760.0);
/// Height (in egui logical pixels) of the 3D occurrence preview panel.
const DEFINITION_PREVIEW_HEIGHT: f32 = 220.0;

#[derive(Debug, Clone)]
struct DefinitionListEntry {
    id: String,
    name: String,
    definition_kind: DefinitionKind,
    parameter_count: usize,
    representation_count: usize,
    child_slot_count: usize,
    derived_parameter_count: usize,
}

impl DefinitionListEntry {
    fn from_definition(definition: &Definition) -> Self {
        let compound = definition.compound.as_ref();
        Self {
            id: definition.id.to_string(),
            name: definition.name.clone(),
            definition_kind: definition.definition_kind.clone(),
            parameter_count: definition.interface.parameters.0.len(),
            representation_count: definition.representations.len(),
            child_slot_count: compound.map(|value| value.child_slots.len()).unwrap_or(0),
            derived_parameter_count: compound
                .map(|value| value.derived_parameters.len())
                .unwrap_or(0),
        }
    }

    fn meta_label(&self) -> String {
        let kind = match self.definition_kind {
            DefinitionKind::Solid => "Solid",
            DefinitionKind::Annotation => "Annotation",
        };
        let mut parts = vec![kind.to_string(), format!("{} params", self.parameter_count)];
        if self.child_slot_count > 0 {
            parts.push(format!("{} slots", self.child_slot_count));
        }
        if self.derived_parameter_count > 0 {
            parts.push(format!("{} derived", self.derived_parameter_count));
        }
        if self.representation_count > 0 {
            parts.push(format!("{} reps", self.representation_count));
        }
        parts.join(" · ")
    }
}
/// Which node of the active definition draft the editor is currently focused on.
///
/// This unified selection type replaces the scattered per-kind `selected_*`
/// fields that existed before PP-DBUX2.  The property tree, context editor,
/// and technical view all key off this single value.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub enum DefinitionEditorNode {
    /// The definition root: name, kind, top-level parameters.
    #[default]
    Definition,
    /// A single authored parameter, identified by name.
    Parameter(String),
    /// A child slot, identified by its `slot_id`.
    Slot(String),
    /// A parameter binding inside a specific child slot.
    SlotParameterBinding {
        slot_id: String,
        parameter_name: String,
    },
    /// An anchor, identified by `anchor.id`.
    Anchor(String),
    /// A constraint, identified by `constraint.id`.
    Constraint(String),
    /// A derived parameter, identified by name.
    DerivedParameter(String),
}

#[derive(Resource, Default, Debug, Clone)]
pub struct DefinitionsWindowState {
    pub visible: bool,
    pub search: String,
    pub selected_library_id: Option<String>,
    pub selected_definition_id: Option<String>,
    pub instantiate_label: String,
    pub host_in_selection: bool,
    pub inspector_visible: bool,
    pub selected_draft_id: Option<String>,
    /// PP-DBUX2: unified selection for the Definition Editor.
    pub selected_node: DefinitionEditorNode,
    /// PP-DBUX4: frame-local hover target in the property tree.
    ///
    /// Set to `Some(node)` while the pointer hovers a tree row and reset to
    /// `None` at the start of each frame by `reset_hovered_node`.  Never
    /// persists across frames — do not read this outside the same frame it
    /// was written.
    pub hovered_node: Option<DefinitionEditorNode>,
    /// PP-DBUX2: when true the center pane shows scoped JSON instead of the
    /// context editor.
    pub technical_view: bool,
    /// Buffer used by the Technical view for the JSON being edited.
    pub technical_view_buffer: String,
    /// Error message shown in the Technical view when JSON is invalid.
    pub technical_view_error: Option<String>,
    pub selected_slot_role_buffer: String,
    pub selected_slot_definition_buffer: String,
    pub selected_slot_translation_buffer: String,
    pub new_definition_name: String,
    pub domain_data_buffer: String,
    pub evaluators_buffer: String,
    pub representations_buffer: String,
    pub slot_editor_buffer: String,
    pub derived_editor_buffer: String,
    pub constraint_editor_buffer: String,
    pub anchor_editor_buffer: String,
    pub new_parameter_name: String,
    pub new_parameter_default: String,
    pub new_parameter_unit: String,
    pub new_parameter_type: String,
    pub new_parameter_override_policy: String,
    pub new_slot_id: String,
    pub new_slot_role: String,
    pub new_slot_definition_id: String,
    pub parameter_default_buffers: HashMap<String, String>,
    pub parameter_unit_buffers: HashMap<String, String>,
}

#[derive(Resource, Debug, Clone, Default)]
pub struct DefinitionSelectionContext {
    pub wall_element_id: Option<ElementId>,
    pub opening_element_id: Option<ElementId>,
    pub opening_center: Option<Vec3>,
    pub wall_axis: Option<Vec3>,
    pub wall_thickness: Option<f64>,
    pub opening_width: Option<f64>,
    pub opening_height: Option<f64>,
}

impl DefinitionSelectionContext {
    pub fn can_host_in_opening(&self) -> bool {
        self.wall_element_id.is_some() && self.opening_element_id.is_some()
    }

    pub fn summary(&self) -> String {
        if let (Some(wall), Some(opening)) = (self.wall_element_id, self.opening_element_id) {
            format!("Opening {} in wall {}", opening.0, wall.0)
        } else if let Some(wall) = self.wall_element_id {
            format!("Wall {}", wall.0)
        } else {
            "No wall/opening selection".to_string()
        }
    }
}

pub fn sync_definition_selection_context(world: &mut World) {
    let context = capture_definition_selection_context(world);
    if let Some(mut current) = world.get_resource_mut::<DefinitionSelectionContext>() {
        *current = context;
    } else {
        world.insert_resource(context);
    }
}

pub fn execute_toggle_definitions_browser(
    world: &mut World,
    _: &Value,
) -> Result<CommandResult, String> {
    let visible = {
        let mut state = world.resource_mut::<DefinitionsWindowState>();
        state.visible = !state.visible;
        state.visible
    };
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        let message = if visible {
            "Definitions window opened"
        } else {
            "Definitions window closed"
        };
        status.set_feedback(message.to_string(), 2.0);
    }
    Ok(CommandResult::empty())
}

pub fn execute_instantiate_definition(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let object = parameters
        .as_object()
        .ok_or_else(|| "Instantiate Definition requires a JSON object".to_string())?;
    let definition_id = required_string(object, "definition_id")?;
    let library_id = optional_string(object, "library_id");
    let label = optional_string(object, "label");
    let host_mode = optional_string(object, "host_mode").unwrap_or_else(|| "auto".to_string());

    let imported_definition_ids =
        ensure_definition_available(world, &definition_id, library_id.as_deref())?;

    let selection = capture_definition_selection_context(world);
    let hosted = match host_mode.as_str() {
        "unhosted" => None,
        "opening" | "auto" => selection.can_host_in_opening().then_some(selection.clone()),
        "wall" => selection.wall_element_id.map(|_| selection.clone()),
        other => {
            return Err(format!(
                "Unsupported host_mode '{other}'. Expected: auto, unhosted, opening, or wall"
            ))
        }
    };

    if (host_mode == "opening" || host_mode == "auto")
        && hosted.is_none()
        && (host_mode == "opening" || definition_requires_opening_host(world, &definition_id)?)
    {
        return Err("Select an opening to host this definition".to_string());
    }

    let def_id = DefinitionId(definition_id.clone());
    let definition = world
        .resource::<DefinitionRegistry>()
        .effective_definition(&def_id)?;
    let definition_version = world
        .resource::<DefinitionRegistry>()
        .get(&def_id)
        .ok_or_else(|| format!("Definition '{}' not found", definition_id))?
        .definition_version;

    let mut overrides = parse_override_map(object.get("overrides"))?;
    apply_contextual_window_overrides(&definition, hosted.as_ref(), &mut overrides);
    {
        let registry = world.resource::<DefinitionRegistry>();
        registry.validate_overrides(&def_id, &overrides)?;
    }

    let offset =
        explicit_offset(object).or_else(|| hosted.as_ref().and_then(|ctx| ctx.opening_center));
    let element_id = world.resource_mut::<ElementIdAllocator>().next_id();
    let mut identity = OccurrenceIdentity::new(def_id.clone(), definition_version);
    identity.overrides = overrides;
    if let Some(domain_data) = object.get("domain_data") {
        identity.domain_data = domain_data.clone();
    }

    let mut created = Vec::new();
    let mut relation_ids = Vec::new();

    if let Some(host_context) = hosted.as_ref() {
        identity.hosting = Some(build_hosted_occurrence_context(host_context));
    }

    let mut snapshot = OccurrenceSnapshot::new(
        element_id,
        identity,
        label.unwrap_or_else(|| definition.name.clone()),
    );
    if let Some(offset) = offset {
        snapshot.offset = offset;
    }
    if let Some(host_context) = hosted.as_ref() {
        if let Some(rotation) = wall_host_rotation(host_context) {
            snapshot.rotation = rotation;
        }
    }
    enqueue_create_boxed_entity(world, snapshot.into());
    apply_pending_history_commands(world);
    created.push(element_id.0);

    if let Some(host_context) = hosted {
        if let Some(wall_id) = host_context.wall_element_id {
            let relation_id = world.resource_mut::<ElementIdAllocator>().next_id();
            let mut parameters = json!({});
            if let Some(opening_id) = host_context.opening_element_id {
                parameters["opening_element_id"] = json!(opening_id.0);
                parameters["placement_anchor"] = json!("opening.center");
            }
            enqueue_create_boxed_entity(
                world,
                RelationSnapshot {
                    element_id: relation_id,
                    relation: SemanticRelation {
                        source: element_id,
                        target: wall_id,
                        relation_type: "hosted_on".to_string(),
                        parameters,
                    },
                }
                .into(),
            );
            apply_pending_history_commands(world);
            relation_ids.push(relation_id.0);
            created.push(relation_id.0);
        }
    }

    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        status.set_feedback(format!("Instantiated '{}'", definition.name), 2.0);
    }

    Ok(CommandResult {
        created,
        output: Some(json!({
            "element_id": element_id.0,
            "definition_id": definition_id,
            "imported_definition_ids": imported_definition_ids,
            "relation_ids": relation_ids,
        })),
        ..CommandResult::empty()
    })
}

pub fn execute_create_definition_draft(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let object = parameters
        .as_object()
        .ok_or_else(|| "Create Definition Draft requires a JSON object".to_string())?;
    let name = optional_string(object, "name").unwrap_or_else(|| "New Definition".to_string());
    let mut definition = blank_definition(name);
    if let Some(kind) = object.get("definition_kind").and_then(Value::as_str) {
        definition.definition_kind = match kind {
            "Solid" => crate::plugins::modeling::definition::DefinitionKind::Solid,
            "Annotation" => crate::plugins::modeling::definition::DefinitionKind::Annotation,
            other => return Err(format!("Unsupported definition_kind '{other}'")),
        };
    }

    let draft_id = {
        let mut drafts = world.resource_mut::<DefinitionDraftRegistry>();
        drafts.insert(DefinitionDraft {
            draft_id: DefinitionDraftId::new(),
            source_definition_id: None,
            source_library_id: None,
            working_copy: definition.clone(),
            dirty: true,
        })
    };
    if let Some(mut state) = world.get_resource_mut::<DefinitionsWindowState>() {
        state.visible = true;
        state.inspector_visible = true;
    }
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        status.set_feedback(format!("Created draft '{}'", definition.name), 2.0);
    }
    Ok(CommandResult {
        output: Some(json!({
            "draft_id": draft_id.0,
            "definition_id": definition.id.to_string(),
        })),
        ..CommandResult::empty()
    })
}

pub fn execute_open_definition_draft(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let object = parameters
        .as_object()
        .ok_or_else(|| "Open Definition Draft requires a JSON object".to_string())?;
    let definition_id = required_string(object, "definition_id")?;
    let library_id = optional_string(object, "library_id");
    let (definition, source_definition_id, source_library_id, _) = {
        let definitions = world.resource::<DefinitionRegistry>();
        let libraries = world.resource::<DefinitionLibraryRegistry>();
        crate::plugins::definition_authoring::resolve_definition_for_authoring(
            definitions,
            libraries,
            &definition_id,
            library_id.as_deref(),
        )?
    };

    let draft_id = {
        let mut drafts = world.resource_mut::<DefinitionDraftRegistry>();
        drafts.insert(DefinitionDraft {
            draft_id: DefinitionDraftId::new(),
            source_definition_id,
            source_library_id,
            working_copy: definition.clone(),
            dirty: false,
        })
    };
    if let Some(mut state) = world.get_resource_mut::<DefinitionsWindowState>() {
        state.visible = true;
        state.inspector_visible = true;
    }
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        status.set_feedback(format!("Opened draft '{}'", definition.name), 2.0);
    }
    Ok(CommandResult {
        output: Some(json!({
            "draft_id": draft_id.0,
            "definition_id": definition.id.to_string(),
        })),
        ..CommandResult::empty()
    })
}

pub fn execute_open_selected_occurrence_definition(
    world: &mut World,
    _: &Value,
) -> Result<CommandResult, String> {
    let (definition_id, source_slot_path) =
        selected_occurrence_definition_id_and_slot(world)?;

    let output = execute_open_definition_draft(
        world,
        &json!({
            "definition_id": definition_id.to_string(),
        }),
    )?;

    // PP-DBUX4 D: when the editor was opened by clicking a generated part in
    // the main viewport, auto-select the matching slot in the property tree.
    // This is best-effort: if the slot path leaf does not exist in the opened
    // definition's child slots, fall back to the definition root.
    if let Some(slot_path) = source_slot_path {
        // The `slot_path` may be nested (e.g. "glazing.left_pane"). The
        // controlling definition is the child definition at the top-level
        // slot, so its own slot list may not contain the nested segment.
        // Use only the top-level segment when testing for a match.
        let top_level_slot = slot_path
            .split('.')
            .next()
            .unwrap_or(&slot_path)
            .to_string();

        // Verify the slot exists in the opened definition before committing
        // to the selection — best-effort, fall through silently if not found.
        let slot_exists = {
            let definitions = world.resource::<DefinitionRegistry>();
            let libraries = world.resource::<DefinitionLibraryRegistry>();
            let drafts = world.resource::<DefinitionDraftRegistry>();
            if let Some(active_draft_id) = &drafts.active_draft_id.clone() {
                if let Some(draft) = drafts.get(active_draft_id) {
                    let eff = crate::plugins::definition_authoring::draft_effective_definition(
                        definitions, libraries, draft,
                    );
                    eff.ok().and_then(|def| {
                        def.compound.map(|compound| {
                            compound
                                .child_slots
                                .iter()
                                .any(|s| s.slot_id == top_level_slot)
                        })
                    }).unwrap_or(false)
                } else {
                    false
                }
            } else {
                false
            }
        };

        if slot_exists {
            if let Some(mut state) = world.get_resource_mut::<DefinitionsWindowState>() {
                state.selected_node =
                    DefinitionEditorNode::Slot(top_level_slot);
                state.technical_view_buffer.clear();
                state.technical_view_error = None;
            }
        }
    }

    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        status.set_feedback(
            format!("Opened occurrence definition '{}'", definition_id),
            2.0,
        );
    }
    Ok(output)
}

pub fn execute_derive_definition_draft(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let object = parameters
        .as_object()
        .ok_or_else(|| "Derive Definition Draft requires a JSON object".to_string())?;
    let definition_id = required_string(object, "definition_id")?;
    let library_id = optional_string(object, "library_id");
    let (base_definition, _, source_library_id, effective_base) = {
        let definitions = world.resource::<DefinitionRegistry>();
        let libraries = world.resource::<DefinitionLibraryRegistry>();
        crate::plugins::definition_authoring::resolve_definition_for_authoring(
            definitions,
            libraries,
            &definition_id,
            library_id.as_deref(),
        )?
    };
    let name = optional_string(object, "name")
        .unwrap_or_else(|| format!("{} Variant", base_definition.name));
    let definition = crate::plugins::definition_authoring::derive_definition_from_base(
        &base_definition,
        &effective_base,
        name,
    );
    let draft_id = {
        let mut drafts = world.resource_mut::<DefinitionDraftRegistry>();
        drafts.insert(DefinitionDraft {
            draft_id: DefinitionDraftId::new(),
            source_definition_id: None,
            source_library_id,
            working_copy: definition.clone(),
            dirty: true,
        })
    };
    if let Some(mut state) = world.get_resource_mut::<DefinitionsWindowState>() {
        state.visible = true;
        state.inspector_visible = true;
    }
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        status.set_feedback(format!("Derived draft '{}'", definition.name), 2.0);
    }
    Ok(CommandResult {
        output: Some(json!({
            "draft_id": draft_id.0,
            "definition_id": definition.id.to_string(),
        })),
        ..CommandResult::empty()
    })
}

pub fn execute_publish_definition_draft(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let object = parameters
        .as_object()
        .ok_or_else(|| "Publish Definition Draft requires a JSON object".to_string())?;
    let draft_id = required_string(object, "draft_id")?;
    let published = crate::plugins::definition_authoring::publish_draft(
        world,
        &DefinitionDraftId(draft_id.clone()),
    )?;
    if let Some(mut state) = world.get_resource_mut::<DefinitionsWindowState>() {
        state.inspector_visible = true;
    }
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        status.set_feedback(format!("Published '{}'", published.name), 2.0);
    }
    Ok(CommandResult {
        output: Some(json!({
            "draft_id": draft_id,
            "definition_id": published.id.to_string(),
        })),
        ..CommandResult::empty()
    })
}

pub fn execute_patch_definition_draft(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let object = parameters
        .as_object()
        .ok_or_else(|| "Patch Definition Draft requires a JSON object".to_string())?;
    let draft_id = required_string(object, "draft_id")?;
    let patch = object
        .get("patch")
        .cloned()
        .ok_or_else(|| "Missing 'patch'".to_string())?;
    let patch =
        serde_json::from_value::<DefinitionPatch>(patch).map_err(|error| error.to_string())?;
    let definitions = world.resource::<DefinitionRegistry>().clone();
    let libraries = world.resource::<DefinitionLibraryRegistry>().clone();
    {
        let mut drafts = world.resource_mut::<DefinitionDraftRegistry>();
        apply_patch_to_draft(
            &definitions,
            &libraries,
            &mut drafts,
            &DefinitionDraftId(draft_id.clone()),
            patch,
        )?;
    }
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        status.set_feedback("Draft updated".to_string(), 1.5);
    }
    Ok(CommandResult {
        output: Some(json!({ "draft_id": draft_id })),
        ..CommandResult::empty()
    })
}

pub fn capture_definition_selection_context(world: &mut World) -> DefinitionSelectionContext {
    let mut query = world.query_filtered::<Entity, With<Selected>>();
    let selected_entities: Vec<Entity> = query.iter(world).collect();
    let registry = world.resource::<CapabilityRegistry>();
    let selected_snapshots: Vec<BoxedEntity> = selected_entities
        .iter()
        .filter_map(|entity| world.get_entity(*entity).ok())
        .filter_map(|entity_ref: EntityRef<'_>| registry.capture_snapshot(&entity_ref, world))
        .collect();
    analyze_selection_snapshots(&selected_snapshots)
}

pub fn analyze_selection_snapshots(snapshots: &[BoxedEntity]) -> DefinitionSelectionContext {
    let mut context = DefinitionSelectionContext::default();
    for snapshot in snapshots {
        let json = snapshot.to_json();
        match snapshot.type_name() {
            "opening" => {
                let Some(opening_root) = json.get("Opening") else {
                    continue;
                };
                context.opening_element_id = Some(snapshot.element_id());
                context.opening_center = Some(snapshot.center());
                context.wall_element_id = opening_root
                    .get("parent_wall_element_id")
                    .and_then(Value::as_u64)
                    .map(ElementId);
                context.wall_axis = opening_root
                    .get("parent_wall")
                    .and_then(wall_axis_from_value);
                context.opening_width = opening_root
                    .get("opening")
                    .and_then(|opening| opening.get("width"))
                    .and_then(Value::as_f64);
                context.opening_height = opening_root
                    .get("opening")
                    .and_then(|opening| opening.get("height"))
                    .and_then(Value::as_f64);
                context.wall_thickness = opening_root
                    .get("parent_wall")
                    .and_then(|wall| wall.get("thickness"))
                    .and_then(Value::as_f64);
                break;
            }
            "wall" => {
                if context.wall_element_id.is_none() {
                    context.wall_element_id = Some(snapshot.element_id());
                }
                if context.wall_thickness.is_none() {
                    context.wall_thickness = json
                        .get("Wall")
                        .and_then(|wall| wall.get("wall"))
                        .and_then(|wall| wall.get("thickness"))
                        .and_then(Value::as_f64);
                }
                if context.wall_axis.is_none() {
                    context.wall_axis = json
                        .get("Wall")
                        .and_then(|wall| wall.get("wall"))
                        .and_then(wall_axis_from_value);
                }
            }
            _ => {}
        }
    }
    context
}

/// Return the controlling definition id for the single currently-selected
/// entity, plus the slot path that led to it (populated when the selection is
/// a [`GeneratedOccurrencePart`]).
///
/// The slot path is used by PP-DBUX4 to auto-select the matching slot in the
/// Definition Editor property tree when the editor is opened from the main
/// viewport.
fn selected_occurrence_definition_id_and_slot(
    world: &mut World,
) -> Result<(DefinitionId, Option<String>), String> {
    let mut selected_query = world.query_filtered::<Entity, With<Selected>>();
    let selected_entities: Vec<Entity> = selected_query.iter(world).collect();
    if selected_entities.len() != 1 {
        return Err("Select exactly one occurrence".to_string());
    }

    let selected = selected_entities[0];
    if let Some(identity) = world.get::<OccurrenceIdentity>(selected) {
        return Ok((identity.definition_id.clone(), None));
    }

    if let Some(relation) = world.get::<SemanticRelation>(selected) {
        if relation.relation_type == "hosted_on" {
            if let Some(definition_id) = occurrence_definition_for_element(world, relation.source) {
                return Ok((definition_id, None));
            }
        }
    }

    if let Some(generated) =
        world.get::<crate::plugins::modeling::occurrence::GeneratedOccurrencePart>(selected)
    {
        // PP-DBUX1: when the user selects a generated part (e.g. a window
        // pane), open the *controlling* child definition (e.g. the Glazing
        // definition), not the parent occurrence's definition. Per
        // DEFINITION_BROWSER_UX_AGREEMENT.md: "Selecting a generated pane
        // must identify the controlling child definition and offer that
        // route before any one-off face/material override." This is what
        // makes a one-click material assignment to glazing reach every
        // pane in the project.
        //
        // PP-DBUX4: also capture the slot path so the editor can auto-select
        // the matching slot row when it opens.
        let slot_path = generated.slot_path.clone();
        return Ok((generated.definition_id.clone(), Some(slot_path)));
    }

    if let Some(opening_id) = world.get::<ElementId>(selected).copied() {
        if let Some(definition_id) = occurrence_definition_for_hosted_opening(world, opening_id) {
            return Ok((definition_id, None));
        }
    }

    Err("Selected entity is not an occurrence".to_string())
}

fn occurrence_definition_for_element(
    world: &mut World,
    occurrence_id: ElementId,
) -> Option<DefinitionId> {
    let mut owner_query = world.query::<(&ElementId, &OccurrenceIdentity)>();
    owner_query.iter(world).find_map(|(element_id, identity)| {
        (*element_id == occurrence_id).then_some(identity.definition_id.clone())
    })
}

fn occurrence_definition_for_hosted_opening(
    world: &mut World,
    opening_id: ElementId,
) -> Option<DefinitionId> {
    let hosted_sources = {
        let mut relation_query = world.query::<&SemanticRelation>();
        relation_query
            .iter(world)
            .filter_map(|relation| {
                if relation.relation_type != "hosted_on" {
                    return None;
                }
                let relation_opening_id = relation
                    .parameters
                    .get("opening_element_id")
                    .and_then(Value::as_u64)
                    .map(ElementId);
                (relation_opening_id == Some(opening_id)).then_some(relation.source)
            })
            .collect::<Vec<_>>()
    };

    hosted_sources
        .into_iter()
        .find_map(|source| occurrence_definition_for_element(world, source))
}

fn wall_axis_from_value(value: &Value) -> Option<Vec3> {
    let start = value.get("start")?.as_array()?;
    let end = value.get("end")?.as_array()?;
    let start = Vec3::new(
        start.first()?.as_f64()? as f32,
        0.0,
        start.get(1)?.as_f64()? as f32,
    );
    let end = Vec3::new(
        end.first()?.as_f64()? as f32,
        0.0,
        end.get(1)?.as_f64()? as f32,
    );
    (end - start).try_normalize()
}

#[allow(clippy::too_many_arguments)]
pub fn draw_definitions_window(
    ctx: &egui::Context,
    contexts: &mut EguiContexts,
    state: &mut DefinitionsWindowState,
    selection: &DefinitionSelectionContext,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    pending: &mut PendingCommandInvocations,
    cursor_world_pos: &CursorWorldPos,
    status: &mut StatusBarData,
    preview_scene: &DefinitionPreviewScene,
    pending_click: &mut PendingPreviewClick,
) {
    if !state.visible {
        return;
    }

    let definitions_rect =
        tool_window_rect(ctx, egui::pos2(24.0, 88.0), DEFINITIONS_WINDOW_DEFAULT_SIZE);
    let mut open = state.visible;
    egui::Window::new("Definitions")
        .id(egui::Id::new("definitions_browser"))
        .default_rect(definitions_rect)
        .min_size(DEFINITIONS_WINDOW_MIN_SIZE)
        .max_size(tool_window_max_size(ctx, DEFINITIONS_WINDOW_MAX_SIZE))
        .constrain_to(tool_window_bounds(ctx))
        .open(&mut open)
        .show(ctx, |ui| {
            let mut library_entries = libraries.list();
            library_entries.sort_by(|left, right| left.name.cmp(&right.name));
            if let Some(selected_library_id) = state.selected_library_id.as_deref() {
                if !library_entries
                    .iter()
                    .any(|library| library.id.0 == selected_library_id)
                {
                    state.selected_library_id = None;
                }
            }

            let selected_library_id = state.selected_library_id.clone();
            let mut definition_entries: Vec<DefinitionListEntry> = match selected_library_id.as_deref() {
                Some(library_id) => libraries
                    .get(&DefinitionLibraryId(library_id.to_string()))
                    .map(|library| {
                        library
                            .definitions
                            .values()
                            .map(DefinitionListEntry::from_definition)
                            .collect()
                    })
                    .unwrap_or_default(),
                None => definitions
                    .list()
                    .into_iter()
                    .map(DefinitionListEntry::from_definition)
                    .collect(),
            };
            definition_entries.sort_by(|left, right| left.name.cmp(&right.name));
            let search = state.search.trim().to_ascii_lowercase();
            if !search.is_empty() {
                definition_entries.retain(|entry| {
                    entry.id.to_ascii_lowercase().contains(&search)
                        || entry.name.to_ascii_lowercase().contains(&search)
                });
            }

            if state.selected_definition_id.as_ref().is_none_or(|selected_id| {
                !definition_entries
                    .iter()
                    .any(|entry| entry.id == *selected_id)
            }) {
                state.selected_definition_id =
                    definition_entries.first().map(|entry| entry.id.clone());
            }

            let selected_definition_id = state.selected_definition_id.as_deref().unwrap_or_default();
            let effective_definition = match selected_library_id.as_deref() {
                Some(library_id) if !selected_definition_id.is_empty() => {
                    library_effective_definition(libraries, library_id, selected_definition_id).ok()
                }
                None if !selected_definition_id.is_empty() => definitions
                    .effective_definition(&DefinitionId(selected_definition_id.to_string()))
                    .ok(),
                _ => None,
            };
            egui::ScrollArea::vertical()
                .id_salt("definitions.browser.root")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("New Draft").clicked() {
                            queue_command_invocation_resource(
                                pending,
                                "modeling.create_definition_draft".to_string(),
                                json!({"name":"New Definition"}),
                            );
                        }
                        let can_open = state.selected_definition_id.is_some();
                        if ui.add_enabled(can_open, egui::Button::new("Edit As Draft")).clicked() {
                            let mut parameters = json!({
                                "definition_id": state.selected_definition_id.clone().unwrap_or_default(),
                            });
                            if let Some(library_id) = state.selected_library_id.as_deref() {
                                parameters["library_id"] = json!(library_id);
                            }
                            queue_command_invocation_resource(
                                pending,
                                "modeling.open_definition_draft".to_string(),
                                parameters,
                            );
                        }
                        if ui.add_enabled(can_open, egui::Button::new("Derive Draft")).clicked() {
                            let mut parameters = json!({
                                "definition_id": state.selected_definition_id.clone().unwrap_or_default(),
                            });
                            if let Some(library_id) = state.selected_library_id.as_deref() {
                                parameters["library_id"] = json!(library_id);
                            }
                            queue_command_invocation_resource(
                                pending,
                                "modeling.derive_definition_draft".to_string(),
                                parameters,
                            );
                        }
                        if ui
                            .add_enabled(drafts.active_draft_id.is_some(), egui::Button::new("Open Inspector"))
                            .clicked()
                        {
                            state.inspector_visible = true;
                        }
                    });
                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label("Search");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.search)
                                .desired_width(220.0)
                                .hint_text("Definition name or id"),
                        );
                    });
                    ui.label(egui::RichText::new(selection.summary()).small());
                    ui.separator();

                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(176.0, ui.available_height()),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.group(|ui| {
                                    ui.set_min_width(164.0);
                                    ui.label(egui::RichText::new("Sources").strong());
                                    ui.separator();
                                    if ui
                                        .selectable_label(
                                            state.selected_library_id.is_none(),
                                            format!("Document ({})", definitions.list().len()),
                                        )
                                        .clicked()
                                    {
                                        state.selected_library_id = None;
                                    }
                                    egui::ScrollArea::vertical()
                                        .id_salt("definitions.browser.sources")
                                        .max_height(140.0)
                                        .show(ui, |ui| {
                                            for library in &library_entries {
                                                let selected = state.selected_library_id.as_deref()
                                                    == Some(library.id.0.as_str());
                                                if ui
                                                    .selectable_label(
                                                        selected,
                                                        format!(
                                                            "{} ({})",
                                                            library.name,
                                                            library.definitions.len()
                                                        ),
                                                    )
                                                    .clicked()
                                                {
                                                    state.selected_library_id =
                                                        Some(library.id.0.clone());
                                                }
                                            }
                                        });
                                });

                                ui.add_space(8.0);
                                ui.group(|ui| {
                                    ui.set_min_width(164.0);
                                    ui.label(egui::RichText::new("Drafts").strong());
                                    ui.separator();
                                    let mut draft_entries =
                                        drafts.list().into_iter().cloned().collect::<Vec<_>>();
                                    draft_entries.sort_by(|left, right| {
                                        left.working_copy.name.cmp(&right.working_copy.name)
                                    });
                                    egui::ScrollArea::vertical()
                                        .id_salt("definitions.browser.drafts")
                                        .max_height(180.0)
                                        .show(ui, |ui| {
                                            for draft in draft_entries {
                                                let selected =
                                                    drafts.active_draft_id.as_ref()
                                                        == Some(&draft.draft_id);
                                                let label = if draft.dirty {
                                                    format!("{} *", draft.working_copy.name)
                                                } else {
                                                    draft.working_copy.name.clone()
                                                };
                                                if ui.selectable_label(selected, label).clicked() {
                                                    drafts.active_draft_id =
                                                        Some(draft.draft_id.clone());
                                                    state.inspector_visible = true;
                                                }
                                            }
                                        });
                                });
                            },
                        );

                        ui.separator();

                        ui.vertical(|ui| {
                            ui.group(|ui| {
                                ui.set_width(ui.available_width());
                                ui.label(egui::RichText::new("Definitions").strong());
                                ui.separator();
                                egui::ScrollArea::vertical()
                                    .id_salt("definitions.browser.list")
                                    .max_height(220.0)
                                    .show(ui, |ui| {
                                        if definition_entries.is_empty() {
                                            ui.label("No definitions match the current source and search.");
                                        }
                                        for entry in &definition_entries {
                                            let selected = state.selected_definition_id.as_deref()
                                                == Some(entry.id.as_str());
                                            ui.horizontal(|ui| {
                                                draw_definition_list_thumbnail(ui, entry);
                                                ui.vertical(|ui| {
                                                    if ui
                                                        .selectable_label(selected, &entry.name)
                                                        .clicked()
                                                    {
                                                        state.selected_definition_id =
                                                            Some(entry.id.clone());
                                                        if state.instantiate_label.is_empty() {
                                                            state.instantiate_label =
                                                                entry.name.clone();
                                                        }
                                                    }
                                                    ui.label(
                                                        egui::RichText::new(entry.meta_label())
                                                            .small()
                                                            .weak(),
                                                    );
                                                });
                                            });
                                            ui.add_space(4.0);
                                        }
                                    });
                            });

                            ui.add_space(8.0);
                            ui.group(|ui| {
                                ui.set_width(ui.available_width());
                                ui.label(egui::RichText::new("Selected Definition").strong());
                                ui.separator();
                                if let Some(definition) = effective_definition {
                                    let requires_opening_host =
                                        definition_requires_opening_host_definition(&definition);
                                    draw_definition_3d_preview(
                                        ui,
                                        contexts,
                                        preview_scene,
                                        pending_click,
                                        DEFINITION_PREVIEW_HEIGHT,
                                    );
                                    ui.add_space(6.0);
                                    ui.label(egui::RichText::new(&definition.name).strong());
                                    ui.label(
                                        egui::RichText::new(definition.id.to_string())
                                            .small()
                                            .monospace(),
                                    );
                                    ui.label(format!(
                                        "{} parameters, {} child slots",
                                        definition.interface.parameters.0.len(),
                                        definition
                                            .compound
                                            .as_ref()
                                            .map(|compound| compound.child_slots.len())
                                            .unwrap_or(0)
                                    ));
                                    if requires_opening_host {
                                        ui.label(
                                            egui::RichText::new(
                                                "Requires a selected wall opening.",
                                            )
                                            .small(),
                                        );
                                    } else if selection.can_host_in_opening() {
                                        ui.checkbox(
                                            &mut state.host_in_selection,
                                            "Host in selected opening",
                                        );
                                    }
                                    ui.horizontal(|ui| {
                                        ui.label("Label");
                                        ui.add(
                                            egui::TextEdit::singleline(&mut state.instantiate_label)
                                                .desired_width(220.0)
                                                .hint_text(&definition.name),
                                        );
                                    });
                                    let can_host = selection.can_host_in_opening();
                                    let host_mode =
                                        if requires_opening_host || (state.host_in_selection && can_host)
                                        {
                                            "opening"
                                        } else {
                                            "unhosted"
                                        };
                                    let instantiate_enabled = !requires_opening_host || can_host;
                                    if !instantiate_enabled {
                                        ui.label(
                                            egui::RichText::new(
                                                "Select an opening in a wall to instantiate this definition.",
                                            )
                                            .small(),
                                        );
                                    }
                                    let button_text = if host_mode == "opening" {
                                        "Instantiate In Opening"
                                    } else {
                                        "Instantiate At Cursor"
                                    };
                                    if ui
                                        .add_enabled(instantiate_enabled, egui::Button::new(button_text))
                                        .clicked()
                                    {
                                        let mut parameters = json!({
                                            "definition_id": definition.id.to_string(),
                                            "host_mode": host_mode,
                                        });
                                        if let Some(library_id) = selected_library_id.as_deref() {
                                            parameters["library_id"] = json!(library_id);
                                        }
                                        let label = state.instantiate_label.trim();
                                        if !label.is_empty() {
                                            parameters["label"] = json!(label);
                                        }
                                        if host_mode == "unhosted" {
                                            if let Some(position) =
                                                cursor_world_pos.snapped.or(cursor_world_pos.raw)
                                            {
                                                parameters["offset"] =
                                                    json!([position.x, position.y, position.z]);
                                            }
                                        }
                                        queue_command_invocation_resource(
                                            pending,
                                            "modeling.instantiate_definition".to_string(),
                                            parameters,
                                        );
                                    }
                                } else {
                                    ui.label("Select a definition from the list.");
                                }
                            });
                        });
                    });
                });
        });
    state.visible = open;
    draw_definition_editor(
        ctx,
        contexts,
        preview_scene,
        pending_click,
        state,
        definitions,
        libraries,
        drafts,
        pending,
        status,
    );
}

fn draw_definition_list_thumbnail(ui: &mut egui::Ui, entry: &DefinitionListEntry) {
    let size = egui::vec2(40.0, 40.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let (fill, accent) = match entry.definition_kind {
        DefinitionKind::Solid => (
            egui::Color32::from_rgb(52, 73, 64),
            egui::Color32::from_rgb(169, 218, 186),
        ),
        DefinitionKind::Annotation => (
            egui::Color32::from_rgb(49, 63, 83),
            egui::Color32::from_rgb(182, 213, 255),
        ),
    };

    ui.painter().rect_filled(rect, 8.0, fill);
    ui.painter().rect_stroke(
        rect,
        8.0,
        egui::Stroke::new(1.0, egui::Color32::from_black_alpha(60)),
        egui::StrokeKind::Inside,
    );

    match entry.definition_kind {
        DefinitionKind::Solid => {
            let front = egui::Rect::from_min_max(
                rect.left_top() + egui::vec2(9.0, 13.0),
                rect.right_bottom() - egui::vec2(11.0, 9.0),
            );
            let offset = egui::vec2(6.0, -5.0);
            let back = front.translate(offset);
            ui.painter().rect_stroke(
                front,
                4.0,
                egui::Stroke::new(1.5, accent),
                egui::StrokeKind::Inside,
            );
            ui.painter().rect_stroke(
                back,
                4.0,
                egui::Stroke::new(1.0, accent.gamma_multiply(0.8)),
                egui::StrokeKind::Inside,
            );
            for (a, b) in [
                (front.left_top(), back.left_top()),
                (front.right_top(), back.right_top()),
                (front.left_bottom(), back.left_bottom()),
                (front.right_bottom(), back.right_bottom()),
            ] {
                ui.painter()
                    .line_segment([a, b], egui::Stroke::new(1.0, accent));
            }
        }
        DefinitionKind::Annotation => {
            let sheet = egui::Rect::from_min_max(
                rect.left_top() + egui::vec2(10.0, 8.0),
                rect.right_bottom() - egui::vec2(10.0, 8.0),
            );
            ui.painter()
                .rect_filled(sheet, 4.0, egui::Color32::from_white_alpha(18));
            ui.painter().rect_stroke(
                sheet,
                4.0,
                egui::Stroke::new(1.5, accent),
                egui::StrokeKind::Inside,
            );
            for y in [15.0, 20.0, 25.0] {
                ui.painter().line_segment(
                    [
                        egui::pos2(sheet.left() + 4.0, rect.top() + y),
                        egui::pos2(sheet.right() - 4.0, rect.top() + y),
                    ],
                    egui::Stroke::new(1.0, accent),
                );
            }
        }
    }

    let badge_text = if entry.child_slot_count > 0 {
        entry.child_slot_count.to_string()
    } else {
        entry.parameter_count.to_string()
    };
    let badge_rect = egui::Rect::from_min_size(
        rect.right_bottom() - egui::vec2(16.0, 16.0),
        egui::vec2(14.0, 14.0),
    );
    ui.painter().circle_filled(
        badge_rect.center(),
        7.0,
        egui::Color32::from_black_alpha(70),
    );
    ui.painter().text(
        badge_rect.center(),
        egui::Align2::CENTER_CENTER,
        badge_text,
        egui::TextStyle::Small.resolve(ui.style()),
        egui::Color32::WHITE,
    );
}


/// PP-DBUX3: unified Definition Editor window with real 3D occurrence preview.
///
/// Four panes:
///
/// ```text
/// +--------------------------------------------------------------+
/// | Title bar: <Name>  [Draft/Published pill]  [Technical] [Publish] [Revert] [Close]
/// +--------------------------------------------------------------+
/// | LEFT (280px)        | CENTER (360px)          | RIGHT (180px)|
/// |  3D preview (top)   | context editor           | assets strip |
/// |  property tree (bot)| – or –                  |              |
/// |                     | technical JSON view      |              |
/// +---------------------+-------------------------+--------------+
/// ```
#[allow(clippy::too_many_arguments)]
fn draw_definition_editor(
    ctx: &egui::Context,
    contexts: &mut EguiContexts,
    preview_scene: &DefinitionPreviewScene,
    pending_click: &mut PendingPreviewClick,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    pending: &mut PendingCommandInvocations,
    status: &mut StatusBarData,
) {
    if !state.inspector_visible {
        return;
    }

    let Some(active_draft_id) = drafts.active_draft_id.clone() else {
        return;
    };
    let Some(active_draft) = drafts.get(&active_draft_id).cloned() else {
        return;
    };
    sync_inspector_state(state, &active_draft);

    let editor_rect =
        tool_window_rect(ctx, egui::pos2(568.0, 88.0), INSPECTOR_WINDOW_DEFAULT_SIZE);
    let mut open = state.inspector_visible;
    egui::Window::new("Definition Editor")
        .id(egui::Id::new("definition_inspector"))
        .default_rect(editor_rect)
        .min_size(INSPECTOR_WINDOW_MIN_SIZE)
        .max_size(tool_window_max_size(ctx, INSPECTOR_WINDOW_MAX_SIZE))
        .constrain_to(tool_window_bounds(ctx))
        .open(&mut open)
        .show(ctx, |ui| {
            // ----------------------------------------------------------------
            // Title bar
            // ----------------------------------------------------------------
            ui.horizontal(|ui| {
                // Left side: name + status pill
                let heading_text = egui::RichText::new(&active_draft.working_copy.name)
                    .heading()
                    .strong();
                ui.label(heading_text);
                let (pill_text, pill_color) = if active_draft.dirty {
                    (
                        "Draft",
                        egui::Color32::from_rgb(220, 140, 50),
                    )
                } else {
                    (
                        "Published",
                        egui::Color32::from_rgb(80, 180, 110),
                    )
                };
                ui.label(
                    egui::RichText::new(pill_text)
                        .small()
                        .color(pill_color)
                        .background_color(egui::Color32::from_black_alpha(40)),
                );

                // Right side: action buttons + technical toggle
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        state.inspector_visible = false;
                    }
                    if ui.button("Revert").clicked() {
                        // Revert: reload draft from the published registry copy.
                        // If a source_definition_id is set we re-open that definition;
                        // otherwise the draft has no canonical published counterpart.
                        if let Some(source_id) = &active_draft.source_definition_id.clone() {
                            match definitions.get(source_id).cloned() {
                                Some(published) => {
                                    if let Some(draft_mut) = drafts.get_mut(&active_draft_id) {
                                        draft_mut.working_copy = published;
                                        draft_mut.dirty = false;
                                        state.new_definition_name = drafts
                                            .get(&active_draft_id)
                                            .map(|d| d.working_copy.name.clone())
                                            .unwrap_or_default();
                                        status.set_feedback(
                                            "Reverted to published version".to_string(),
                                            2.0,
                                        );
                                    } else {
                                        status.set_feedback("Draft not found".to_string(), 2.0);
                                    }
                                }
                                None => status.set_feedback(
                                    format!("Cannot find published definition '{source_id}'"),
                                    2.0,
                                ),
                            }
                        } else {
                            status.set_feedback(
                                "No published version to revert to — draft is standalone"
                                    .to_string(),
                                2.0,
                            );
                        }
                    }
                    if ui.button("Publish").clicked() {
                        queue_command_invocation_resource(
                            pending,
                            "modeling.publish_definition_draft".to_string(),
                            json!({ "draft_id": active_draft_id.to_string() }),
                        );
                    }
                    ui.toggle_value(&mut state.technical_view, "Technical");
                });
            });
            ui.separator();

            // ----------------------------------------------------------------
            // Three-column body
            // ----------------------------------------------------------------
            let available = ui.available_size();
            let left_width = (available.x * 0.30).clamp(220.0, 300.0);
            let right_width = (available.x * 0.22).clamp(150.0, 200.0);
            let center_width = available.x - left_width - right_width - 16.0; // 16 for separators

            ui.horizontal_top(|ui| {
                // ── LEFT COLUMN: 3D preview (top) + property tree (bottom) ──
                ui.allocate_ui_with_layout(
                    egui::vec2(left_width, ui.available_height()),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(left_width);
                        // PP-DBUX3: real 3D occurrence preview rendered via RenderTarget::Image.
                        // PP-DBUX4: pending_click is written here on primary click and consumed
                        // by the `resolve_preview_click` Bevy system on the same frame.
                        draw_definition_3d_preview(
                            ui,
                            contexts,
                            preview_scene,
                            pending_click,
                            DEFINITION_PREVIEW_HEIGHT,
                        );
                        ui.separator();
                        // Property tree
                        egui::ScrollArea::vertical()
                            .id_salt("definition_editor.property_tree")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                draw_property_tree(
                                    ui,
                                    state,
                                    &active_draft,
                                    definitions,
                                    libraries,
                                );
                            });
                    },
                );

                ui.separator();

                // ── CENTER COLUMN: context editor or technical view ──
                ui.allocate_ui_with_layout(
                    egui::vec2(center_width, ui.available_height()),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(center_width);
                        egui::ScrollArea::vertical()
                            .id_salt("definition_editor.center")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if state.technical_view {
                                    draw_technical_view(
                                        ui,
                                        state,
                                        definitions,
                                        libraries,
                                        drafts,
                                        &active_draft_id,
                                        &active_draft,
                                        status,
                                    );
                                } else {
                                    draw_context_editor(
                                        ui,
                                        state,
                                        definitions,
                                        libraries,
                                        drafts,
                                        pending,
                                        &active_draft_id,
                                        &active_draft,
                                        status,
                                    );
                                }
                            });
                    },
                );

                ui.separator();

                // ── RIGHT COLUMN: assets strip ──
                ui.allocate_ui_with_layout(
                    egui::vec2(right_width, ui.available_height()),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(right_width);
                        egui::ScrollArea::vertical()
                            .id_salt("definition_editor.assets")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                draw_assets_strip(
                                    ui,
                                    state,
                                    definitions,
                                    libraries,
                                    drafts,
                                    &active_draft_id,
                                    &active_draft,
                                    status,
                                );
                            });
                    },
                );
            });
        });
    state.inspector_visible = open;
}

// ---------------------------------------------------------------------------
// Property tree
// ---------------------------------------------------------------------------

/// Scrollable hierarchical list of all nodes in the draft's definition.
///
/// Clicking a row sets `state.selected_node`, which drives the context editor
/// and the Technical view.
#[allow(clippy::too_many_arguments)]
fn draw_property_tree(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    draft: &DefinitionDraft,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
) {
    let def = &draft.working_copy;
    let compound = def.compound.as_ref();

    // Helper: resolve a child definition's human-readable name given its id.
    let preview_reg = preview_registry_for_draft(definitions, libraries, draft)
        .unwrap_or_else(|_| definitions.clone());
    let child_def_name = |child_id: &crate::plugins::modeling::definition::DefinitionId| -> String {
        preview_reg
            .effective_definition(child_id)
            .map(|d| d.name)
            .unwrap_or_else(|_| child_id.to_string())
    };

    // -- Definition root row --
    let def_selected = state.selected_node == DefinitionEditorNode::Definition;
    if ui
        .selectable_label(
            def_selected,
            egui::RichText::new(format!("Definition: {}", def.name)).strong(),
        )
        .clicked()
    {
        state.selected_node = DefinitionEditorNode::Definition;
        state.technical_view_buffer.clear();
        state.technical_view_error = None;
    }

    // -- Parameters --
    let params: Vec<_> = def
        .interface
        .parameters
        .0
        .iter()
        .filter(|p| {
            p.metadata.mutability
                != crate::plugins::modeling::definition::ParameterMutability::Derived
        })
        .collect();
    if !params.is_empty() {
        egui::CollapsingHeader::new(format!("Parameters ({})", params.len()))
            .default_open(true)
            .show(ui, |ui| {
                for param in params {
                    let selected = state.selected_node
                        == DefinitionEditorNode::Parameter(param.name.clone());
                    let label = egui::RichText::new(&param.name);
                    let unit_hint = param
                        .metadata
                        .unit
                        .as_deref()
                        .unwrap_or("")
                        .to_string();
                    let secondary = egui::RichText::new(if unit_hint.is_empty() {
                        format!("{:?}", param.param_type)
                    } else {
                        format!("{:?} ({})", param.param_type, unit_hint)
                    })
                    .small()
                    .weak();
                    ui.horizontal(|ui| {
                        if ui.selectable_label(selected, label).clicked() {
                            state.selected_node =
                                DefinitionEditorNode::Parameter(param.name.clone());
                            state.technical_view_buffer.clear();
                            state.technical_view_error = None;
                        }
                        ui.label(secondary);
                    });
                }
            });
    }

    // -- Slots --
    if let Some(compound) = compound {
        if !compound.child_slots.is_empty() {
            egui::CollapsingHeader::new(format!("Slots ({})", compound.child_slots.len()))
                .default_open(true)
                .show(ui, |ui| {
                    for slot in &compound.child_slots {
                        let slot_selected =
                            state.selected_node == DefinitionEditorNode::Slot(slot.slot_id.clone());
                        let child_name = child_def_name(&slot.definition_id);
                        let slot_secondary = egui::RichText::new(format!(
                            "Role: {} — {}",
                            slot.role, child_name
                        ))
                        .small()
                        .weak();
                        let header_id = ui.make_persistent_id(format!(
                            "def_editor.slot.{}",
                            slot.slot_id
                        ));
                        egui::collapsing_header::CollapsingState::load_with_default_open(
                            ui.ctx(),
                            header_id,
                            false,
                        )
                        .show_header(ui, |ui| {
                            ui.horizontal(|ui| {
                                let slot_response = ui
                                    .selectable_label(slot_selected, &slot.slot_id);
                                if slot_response.clicked() {
                                    state.selected_node =
                                        DefinitionEditorNode::Slot(slot.slot_id.clone());
                                    state.technical_view_buffer.clear();
                                    state.technical_view_error = None;
                                }
                                // PP-DBUX4 C: track hover for preview pulse highlight.
                                if slot_response.hovered() {
                                    state.hovered_node = Some(
                                        DefinitionEditorNode::Slot(slot.slot_id.clone()),
                                    );
                                }
                                ui.label(slot_secondary);
                            });
                        })
                        .body(|ui| {
                            // Slot parameter bindings as children
                            if !slot.parameter_bindings.is_empty() {
                                for binding in &slot.parameter_bindings {
                                    let binding_selected = state.selected_node
                                        == DefinitionEditorNode::SlotParameterBinding {
                                            slot_id: slot.slot_id.clone(),
                                            parameter_name: binding.target_param.clone(),
                                        };
                                    let value_hint = compact_json_expr(&binding.expr);
                                    let binding_label = egui::RichText::new(format!(
                                        "{} = {}",
                                        binding.target_param, value_hint
                                    ))
                                    .small();
                                    if ui
                                        .selectable_label(binding_selected, binding_label)
                                        .clicked()
                                    {
                                        state.selected_node =
                                            DefinitionEditorNode::SlotParameterBinding {
                                                slot_id: slot.slot_id.clone(),
                                                parameter_name: binding.target_param.clone(),
                                            };
                                        state.technical_view_buffer.clear();
                                        state.technical_view_error = None;
                                    }
                                }
                            }
                        });
                    }
                });
        }

        // -- Anchors --
        if !compound.anchors.is_empty() {
            egui::CollapsingHeader::new(format!("Anchors ({})", compound.anchors.len()))
                .default_open(false)
                .show(ui, |ui| {
                    for anchor in &compound.anchors {
                        let selected = state.selected_node
                            == DefinitionEditorNode::Anchor(anchor.id.clone());
                        let secondary = egui::RichText::new(&anchor.kind).small().weak();
                        ui.horizontal(|ui| {
                            if ui.selectable_label(selected, &anchor.id).clicked() {
                                state.selected_node =
                                    DefinitionEditorNode::Anchor(anchor.id.clone());
                                state.technical_view_buffer.clear();
                                state.technical_view_error = None;
                            }
                            ui.label(secondary);
                        });
                    }
                });
        }

        // -- Constraints --
        if !compound.constraints.is_empty() {
            egui::CollapsingHeader::new(format!("Constraints ({})", compound.constraints.len()))
                .default_open(false)
                .show(ui, |ui| {
                    for constraint in &compound.constraints {
                        let selected = state.selected_node
                            == DefinitionEditorNode::Constraint(constraint.id.clone());
                        let secondary =
                            egui::RichText::new(format!("{:?}", constraint.severity))
                                .small()
                                .weak();
                        ui.horizontal(|ui| {
                            if ui.selectable_label(selected, &constraint.id).clicked() {
                                state.selected_node =
                                    DefinitionEditorNode::Constraint(constraint.id.clone());
                                state.technical_view_buffer.clear();
                                state.technical_view_error = None;
                            }
                            ui.label(secondary);
                        });
                    }
                });
        }

        // -- Derived parameters --
        if !compound.derived_parameters.is_empty() {
            egui::CollapsingHeader::new(format!("Derived ({})", compound.derived_parameters.len()))
                .default_open(false)
                .show(ui, |ui| {
                    for derived in &compound.derived_parameters {
                        let selected = state.selected_node
                            == DefinitionEditorNode::DerivedParameter(derived.name.clone());
                        let secondary = egui::RichText::new(format!("{:?}", derived.param_type))
                            .small()
                            .weak();
                        ui.horizontal(|ui| {
                            if ui.selectable_label(selected, &derived.name).clicked() {
                                state.selected_node =
                                    DefinitionEditorNode::DerivedParameter(derived.name.clone());
                                state.technical_view_buffer.clear();
                                state.technical_view_error = None;
                            }
                            ui.label(secondary);
                        });
                    }
                });
        }
    }
}

// ---------------------------------------------------------------------------
// Context editor
// ---------------------------------------------------------------------------

/// Context-sensitive editor.  Shows different controls depending on
/// `state.selected_node`.  All writes go through `apply_patch_to_draft`.
#[allow(clippy::too_many_arguments)]
fn draw_context_editor(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    pending: &mut PendingCommandInvocations,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    status: &mut StatusBarData,
) {
    let selected_node = state.selected_node.clone();
    match selected_node {
        DefinitionEditorNode::Definition => {
            draw_context_definition_root(
                ui, state, definitions, libraries, drafts, pending, active_draft_id, active_draft,
                status,
            );
        }
        DefinitionEditorNode::Parameter(ref name) => {
            draw_context_parameter(
                ui, state, definitions, libraries, drafts, active_draft_id, active_draft, name,
                status,
            );
        }
        DefinitionEditorNode::Slot(ref slot_id) => {
            draw_context_slot(
                ui, state, definitions, libraries, drafts, pending, active_draft_id, active_draft,
                slot_id, status,
            );
        }
        DefinitionEditorNode::SlotParameterBinding {
            ref slot_id,
            ref parameter_name,
        } => {
            draw_context_slot_binding(
                ui, state, definitions, libraries, drafts, active_draft_id, active_draft, slot_id,
                parameter_name, status,
            );
        }
        DefinitionEditorNode::Anchor(ref anchor_id) => {
            draw_context_anchor(
                ui, state, definitions, libraries, drafts, active_draft_id, active_draft, anchor_id,
                status,
            );
        }
        DefinitionEditorNode::Constraint(ref constraint_id) => {
            draw_context_constraint(
                ui, state, definitions, libraries, drafts, active_draft_id, active_draft,
                constraint_id, status,
            );
        }
        DefinitionEditorNode::DerivedParameter(ref name) => {
            draw_context_derived_parameter(
                ui, state, definitions, libraries, drafts, active_draft_id, active_draft, name,
                status,
            );
        }
    }
}

// -- Context: Definition root --

#[allow(clippy::too_many_arguments)]
fn draw_context_definition_root(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    pending: &mut PendingCommandInvocations,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    status: &mut StatusBarData,
) {
    let def = &active_draft.working_copy;
    ui.label(egui::RichText::new("Definition").strong());
    ui.separator();

    ui.horizontal(|ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut state.new_definition_name);
        if ui.button("Apply").clicked() {
            let patch = DefinitionPatch::SetName {
                name: state.new_definition_name.clone(),
            };
            if let Err(error) =
                apply_patch_to_draft(definitions, libraries, drafts, active_draft_id, patch)
            {
                status.set_feedback(error, 2.0);
            } else {
                status.set_feedback(format!("Renamed to '{}'", state.new_definition_name), 2.0);
            }
        }
    });

    ui.horizontal(|ui| {
        ui.label("Kind");
        ui.label(egui::RichText::new(format!("{:?}", def.definition_kind)).weak());
    });

    if let Some(source_id) = &active_draft.source_definition_id {
        ui.label(
            egui::RichText::new(format!("Editing published definition {source_id}"))
                .small()
                .weak(),
        );
    } else if let Some(base_id) = &def.base_definition_id {
        ui.label(
            egui::RichText::new(format!("Derived from {base_id}"))
                .small()
                .weak(),
        );
    } else {
        ui.label(egui::RichText::new("Standalone draft").small().weak());
    }

    ui.add_space(4.0);

    let validation_result = validate_draft(definitions, libraries, active_draft);
    if let Err(ref error) = validation_result {
        ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
    } else {
        ui.colored_label(
            egui::Color32::from_rgb(110, 180, 130),
            "Definition validates successfully.",
        );
    }

    ui.add_space(8.0);

    // "Add Parameter" form
    egui::CollapsingHeader::new("Add Parameter")
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut state.new_parameter_name);
            });
            ui.horizontal(|ui| {
                ui.label("Type");
                ui.text_edit_singleline(&mut state.new_parameter_type);
            });
            ui.horizontal(|ui| {
                ui.label("Override policy");
                ui.text_edit_singleline(&mut state.new_parameter_override_policy);
            });
            ui.horizontal(|ui| {
                ui.label("Default");
                ui.text_edit_singleline(&mut state.new_parameter_default);
            });
            ui.horizontal(|ui| {
                ui.label("Unit");
                ui.text_edit_singleline(&mut state.new_parameter_unit);
            });
            if ui.button("Add Parameter").clicked() {
                match build_parameter_from_state(state) {
                    Ok(parameter) => {
                        let name = parameter.name.clone();
                        if let Err(error) = apply_patch_to_draft(
                            definitions,
                            libraries,
                            drafts,
                            active_draft_id,
                            DefinitionPatch::SetParameter { parameter },
                        ) {
                            status.set_feedback(error, 2.0);
                        } else {
                            state.selected_node = DefinitionEditorNode::Parameter(name.clone());
                            status.set_feedback(format!("Added parameter '{name}'"), 2.0);
                            state.new_parameter_name.clear();
                            state.new_parameter_default.clear();
                            state.new_parameter_unit.clear();
                        }
                    }
                    Err(error) => status.set_feedback(error, 2.0),
                }
            }
        });

    // "Add Slot" form
    egui::CollapsingHeader::new("Add Slot")
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Slot id");
                ui.text_edit_singleline(&mut state.new_slot_id);
            });
            ui.horizontal(|ui| {
                ui.label("Role");
                ui.text_edit_singleline(&mut state.new_slot_role);
            });
            ui.horizontal(|ui| {
                ui.label("Child definition id");
                ui.text_edit_singleline(&mut state.new_slot_definition_id);
            });
            if ui.button("Add Slot").clicked() {
                if state.new_slot_id.trim().is_empty()
                    || state.new_slot_definition_id.trim().is_empty()
                {
                    status.set_feedback("Provide slot id and child definition id".to_string(), 2.0);
                } else {
                    let slot = crate::plugins::modeling::definition::ChildSlotDef {
                        slot_id: state.new_slot_id.trim().to_string(),
                        role: if state.new_slot_role.trim().is_empty() {
                            "member".to_string()
                        } else {
                            state.new_slot_role.trim().to_string()
                        },
                        definition_id: crate::plugins::modeling::definition::DefinitionId(
                            state.new_slot_definition_id.trim().to_string(),
                        ),
                        parameter_bindings: Vec::new(),
                        transform_binding: Default::default(),
                        suppression_expr: None,
                    };
                    let slot_id = slot.slot_id.clone();
                    if let Err(error) = apply_patch_to_draft(
                        definitions,
                        libraries,
                        drafts,
                        active_draft_id,
                        DefinitionPatch::SetChildSlot { child_slot: slot },
                    ) {
                        status.set_feedback(error, 2.0);
                    } else {
                        state.selected_node = DefinitionEditorNode::Slot(slot_id);
                        status.set_feedback("Added slot".to_string(), 2.0);
                        state.new_slot_id.clear();
                        state.new_slot_role.clear();
                        state.new_slot_definition_id.clear();
                    }
                }
            }
        });

    let _ = pending; // reserved for future "promote to definition default" actions
}

// -- Context: Parameter --

#[allow(clippy::too_many_arguments)]
fn draw_context_parameter(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    parameter_name: &str,
    status: &mut StatusBarData,
) {
    // Resolve the parameter from the effective definition (includes inherited).
    let effective_definition =
        draft_effective_definition(definitions, libraries, active_draft).ok();
    let Some(effective) = effective_definition.as_ref() else {
        ui.label("Draft is currently invalid; cannot edit parameter.");
        return;
    };
    let Some(parameter) = effective.interface.parameters.get(parameter_name) else {
        ui.label(format!("Parameter '{parameter_name}' not found."));
        return;
    };

    let local = active_draft
        .working_copy
        .interface
        .parameters
        .get(parameter_name)
        .is_some();
    let default_key = format!("{}:{}", active_draft_id, parameter.name);
    state
        .parameter_default_buffers
        .entry(default_key.clone())
        .or_insert_with(|| compact_json(&parameter.default_value));
    state
        .parameter_unit_buffers
        .entry(default_key.clone())
        .or_insert_with(|| parameter.metadata.unit.clone().unwrap_or_default());

    ui.label(egui::RichText::new(&parameter.name).heading());
    ui.horizontal(|ui| {
        ui.label("Type");
        ui.label(egui::RichText::new(format!("{:?}", parameter.param_type)).weak());
        ui.label(if local { "local" } else { "inherited" });
    });
    ui.separator();

    ui.horizontal(|ui| {
        ui.label("Default");
        if let Some(buffer) = state.parameter_default_buffers.get_mut(&default_key) {
            ui.text_edit_singleline(buffer);
        }
    });
    ui.horizontal(|ui| {
        ui.label("Unit");
        if let Some(buffer) = state.parameter_unit_buffers.get_mut(&default_key) {
            ui.text_edit_singleline(buffer);
        }
    });
    ui.horizontal(|ui| {
        ui.label("Override policy");
        ui.label(egui::RichText::new(format!("{:?}", parameter.override_policy)).weak());
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Apply").clicked() {
            let mut updated = parameter.clone();
            if let Some(buffer) = state.parameter_default_buffers.get(&default_key) {
                updated.default_value = parse_json_or_string(buffer);
            }
            if let Some(buffer) = state.parameter_unit_buffers.get(&default_key) {
                updated.metadata.unit =
                    (!buffer.trim().is_empty()).then_some(buffer.trim().to_string());
            }
            if let Err(error) = apply_patch_to_draft(
                definitions,
                libraries,
                drafts,
                active_draft_id,
                DefinitionPatch::SetParameter { parameter: updated },
            ) {
                status.set_feedback(error, 2.0);
            } else {
                status.set_feedback(format!("Updated parameter '{parameter_name}'"), 2.0);
            }
        }
        if local && ui.button("Delete").clicked() {
            if let Err(error) = apply_patch_to_draft(
                definitions,
                libraries,
                drafts,
                active_draft_id,
                DefinitionPatch::RemoveParameter {
                    name: parameter_name.to_string(),
                },
            ) {
                status.set_feedback(error, 2.0);
            } else {
                state.selected_node = DefinitionEditorNode::Definition;
            }
        }
    });
}

// -- Context: Slot --

#[allow(clippy::too_many_arguments)]
fn draw_context_slot(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    pending: &mut PendingCommandInvocations,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    slot_id: &str,
    status: &mut StatusBarData,
) {
    let child_slots = active_draft
        .working_copy
        .compound
        .as_ref()
        .map(|c| c.child_slots.clone())
        .unwrap_or_default();
    let Some(slot) = child_slots.iter().find(|s| s.slot_id == slot_id) else {
        ui.label(format!("Slot '{slot_id}' not found."));
        return;
    };

    // Initialize buffers when first shown for this slot
    if state.selected_slot_role_buffer.is_empty()
        || state
            .slot_editor_buffer
            .is_empty()
    {
        sync_selected_slot_buffers(state, slot);
    }

    // Resolve child definition name (never show raw id to the user)
    let preview_reg = preview_registry_for_draft(definitions, libraries, active_draft)
        .unwrap_or_else(|_| definitions.clone());
    let child_def_name = preview_reg
        .effective_definition(&slot.definition_id)
        .map(|d| d.name)
        .unwrap_or_else(|_| slot.definition_id.to_string());

    ui.label(egui::RichText::new(slot_id).heading());
    ui.separator();

    ui.horizontal(|ui| {
        ui.label("Role");
        ui.text_edit_singleline(&mut state.selected_slot_role_buffer);
    });
    // Child definition: show human-readable name; definition_id is edit-only
    // through Technical view or the definition_buffer field below.
    ui.horizontal(|ui| {
        ui.label("Child definition");
        ui.label(egui::RichText::new(&child_def_name).strong());
    });
    ui.horizontal(|ui| {
        ui.label("Definition id");
        ui.add(
            egui::TextEdit::singleline(&mut state.selected_slot_definition_buffer)
                .hint_text("definition id"),
        );
    });
    ui.horizontal(|ui| {
        ui.label("Position");
        ui.add(
            egui::TextEdit::singleline(&mut state.selected_slot_translation_buffer)
                .hint_text("x, y, z"),
        );
    });
    ui.label(
        egui::RichText::new(format!("{} parameter binding(s)", slot.parameter_bindings.len()))
            .small()
            .weak(),
    );

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Apply Slot").clicked() {
            match build_slot_from_editor_buffers(state, slot) {
                Ok(updated_slot) => {
                    state.slot_editor_buffer = pretty_json(&updated_slot);
                    if let Err(error) = apply_patch_to_draft(
                        definitions,
                        libraries,
                        drafts,
                        active_draft_id,
                        DefinitionPatch::SetChildSlot {
                            child_slot: updated_slot,
                        },
                    ) {
                        status.set_feedback(error, 2.0);
                    } else {
                        status.set_feedback(format!("Updated slot '{slot_id}'"), 2.0);
                    }
                }
                Err(error) => status.set_feedback(error, 2.0),
            }
        }
        if ui.button("Open Child Definition").clicked() {
            let mut parameters = json!({
                "definition_id": slot.definition_id.to_string(),
            });
            if let Some(library_id) = &active_draft.source_library_id {
                parameters["library_id"] = json!(library_id.to_string());
            }
            queue_command_invocation_resource(
                pending,
                "modeling.open_definition_draft".to_string(),
                parameters,
            );
        }
    });
    if ui.button("Remove Slot").clicked() {
        if let Err(error) = apply_patch_to_draft(
            definitions,
            libraries,
            drafts,
            active_draft_id,
            DefinitionPatch::RemoveChildSlot {
                slot_id: slot_id.to_string(),
            },
        ) {
            status.set_feedback(error, 2.0);
        } else {
            state.selected_node = DefinitionEditorNode::Definition;
            state.slot_editor_buffer.clear();
            state.selected_slot_role_buffer.clear();
            state.selected_slot_definition_buffer.clear();
            state.selected_slot_translation_buffer.clear();
        }
    }
}

// -- Context: SlotParameterBinding --

#[allow(clippy::too_many_arguments)]
fn draw_context_slot_binding(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    slot_id: &str,
    parameter_name: &str,
    status: &mut StatusBarData,
) {
    // PP-DBUX3+ TODO: surface a dedicated binding editor widget here.
    // For PP-DBUX2 we show the JSON for this single binding with Apply.
    let child_slots = active_draft
        .working_copy
        .compound
        .as_ref()
        .map(|c| c.child_slots.clone())
        .unwrap_or_default();
    let Some(slot) = child_slots.iter().find(|s| s.slot_id == slot_id) else {
        ui.label(format!("Slot '{slot_id}' not found."));
        return;
    };
    let Some(binding) = slot
        .parameter_bindings
        .iter()
        .find(|b| b.target_param == parameter_name)
    else {
        ui.label(format!(
            "Binding '{parameter_name}' not found in slot '{slot_id}'."
        ));
        return;
    };

    ui.label(egui::RichText::new(format!("{slot_id} → {parameter_name}")).heading());
    ui.separator();

    // Initialize the slot_editor_buffer with this binding's JSON if empty.
    let binding_key = format!("{slot_id}:{parameter_name}");
    if state.slot_editor_buffer.is_empty()
        || !state.slot_editor_buffer.contains(&binding_key)
    {
        state.slot_editor_buffer = pretty_json(binding);
    }

    ui.add(
        egui::TextEdit::multiline(&mut state.slot_editor_buffer)
            .desired_rows(8)
            .code_editor(),
    );
    ui.add_space(4.0);
    if ui.button("Apply Binding").clicked() {
        match serde_json::from_str::<crate::plugins::modeling::definition::ParameterBinding>(
            &state.slot_editor_buffer,
        ) {
            Ok(updated_binding) => {
                if let Err(error) = apply_patch_to_draft(
                    definitions,
                    libraries,
                    drafts,
                    active_draft_id,
                    DefinitionPatch::SetChildSlotBinding {
                        slot_id: slot_id.to_string(),
                        binding: updated_binding,
                    },
                ) {
                    status.set_feedback(error, 2.0);
                } else {
                    status.set_feedback(
                        format!("Updated binding '{parameter_name}' in slot '{slot_id}'"),
                        2.0,
                    );
                }
            }
            Err(error) => status.set_feedback(error.to_string(), 2.0),
        }
    }

    let _ = state;
}

// -- Context: Anchor --

#[allow(clippy::too_many_arguments)]
fn draw_context_anchor(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    anchor_id: &str,
    status: &mut StatusBarData,
) {
    let compound = active_draft.working_copy.compound.as_ref();
    let Some(anchor) = compound
        .and_then(|c| c.anchors.iter().find(|a| a.id == anchor_id))
    else {
        ui.label(format!("Anchor '{anchor_id}' not found."));
        return;
    };

    if state.anchor_editor_buffer.is_empty() {
        state.anchor_editor_buffer = pretty_json(anchor);
    }

    ui.label(egui::RichText::new(anchor_id).heading());
    ui.separator();
    ui.label(egui::RichText::new(&anchor.kind).weak());
    ui.add_space(4.0);
    ui.add(
        egui::TextEdit::multiline(&mut state.anchor_editor_buffer)
            .desired_rows(8)
            .code_editor(),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("Apply").clicked() {
            match serde_json::from_str::<crate::plugins::modeling::definition::AnchorDef>(
                &state.anchor_editor_buffer,
            ) {
                Ok(updated) => {
                    if let Err(error) = apply_patch_to_draft(
                        definitions,
                        libraries,
                        drafts,
                        active_draft_id,
                        DefinitionPatch::SetAnchor { anchor: updated },
                    ) {
                        status.set_feedback(error, 2.0);
                    } else {
                        status.set_feedback(format!("Updated anchor '{anchor_id}'"), 2.0);
                    }
                }
                Err(error) => status.set_feedback(error.to_string(), 2.0),
            }
        }
        if ui.button("Delete").clicked() {
            if let Err(error) = apply_patch_to_draft(
                definitions,
                libraries,
                drafts,
                active_draft_id,
                DefinitionPatch::RemoveAnchor {
                    id: anchor_id.to_string(),
                },
            ) {
                status.set_feedback(error, 2.0);
            } else {
                state.selected_node = DefinitionEditorNode::Definition;
                state.anchor_editor_buffer.clear();
            }
        }
    });
}

// -- Context: Constraint --

#[allow(clippy::too_many_arguments)]
fn draw_context_constraint(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    constraint_id: &str,
    status: &mut StatusBarData,
) {
    let compound = active_draft.working_copy.compound.as_ref();
    let Some(constraint) = compound
        .and_then(|c| c.constraints.iter().find(|c| c.id == constraint_id))
    else {
        ui.label(format!("Constraint '{constraint_id}' not found."));
        return;
    };

    if state.constraint_editor_buffer.is_empty() {
        state.constraint_editor_buffer = pretty_json(constraint);
    }

    ui.label(egui::RichText::new(constraint_id).heading());
    ui.separator();
    ui.label(egui::RichText::new(&constraint.message).weak());
    ui.add_space(4.0);
    ui.add(
        egui::TextEdit::multiline(&mut state.constraint_editor_buffer)
            .desired_rows(8)
            .code_editor(),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("Apply").clicked() {
            match serde_json::from_str::<crate::plugins::modeling::definition::ConstraintDef>(
                &state.constraint_editor_buffer,
            ) {
                Ok(updated) => {
                    if let Err(error) = apply_patch_to_draft(
                        definitions,
                        libraries,
                        drafts,
                        active_draft_id,
                        DefinitionPatch::SetConstraint { constraint: updated },
                    ) {
                        status.set_feedback(error, 2.0);
                    } else {
                        status.set_feedback(format!("Updated constraint '{constraint_id}'"), 2.0);
                    }
                }
                Err(error) => status.set_feedback(error.to_string(), 2.0),
            }
        }
        if ui.button("Delete").clicked() {
            if let Err(error) = apply_patch_to_draft(
                definitions,
                libraries,
                drafts,
                active_draft_id,
                DefinitionPatch::RemoveConstraint {
                    id: constraint_id.to_string(),
                },
            ) {
                status.set_feedback(error, 2.0);
            } else {
                state.selected_node = DefinitionEditorNode::Definition;
                state.constraint_editor_buffer.clear();
            }
        }
    });
}

// -- Context: DerivedParameter --

#[allow(clippy::too_many_arguments)]
fn draw_context_derived_parameter(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    derived_name: &str,
    status: &mut StatusBarData,
) {
    let compound = active_draft.working_copy.compound.as_ref();
    let Some(derived) = compound
        .and_then(|c| c.derived_parameters.iter().find(|d| d.name == derived_name))
    else {
        ui.label(format!("Derived parameter '{derived_name}' not found."));
        return;
    };

    if state.derived_editor_buffer.is_empty() {
        state.derived_editor_buffer = pretty_json(derived);
    }

    ui.label(egui::RichText::new(derived_name).heading());
    ui.separator();
    ui.label(egui::RichText::new(format!("{:?}", derived.param_type)).weak());
    ui.add_space(4.0);
    ui.add(
        egui::TextEdit::multiline(&mut state.derived_editor_buffer)
            .desired_rows(8)
            .code_editor(),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("Apply").clicked() {
            match serde_json::from_str::<crate::plugins::modeling::definition::DerivedParameterDef>(
                &state.derived_editor_buffer,
            ) {
                Ok(updated) => {
                    if let Err(error) = apply_patch_to_draft(
                        definitions,
                        libraries,
                        drafts,
                        active_draft_id,
                        DefinitionPatch::SetDerivedParameter {
                            derived_parameter: updated,
                        },
                    ) {
                        status.set_feedback(error, 2.0);
                    } else {
                        status.set_feedback(
                            format!("Updated derived parameter '{derived_name}'"),
                            2.0,
                        );
                    }
                }
                Err(error) => status.set_feedback(error.to_string(), 2.0),
            }
        }
        if ui.button("Delete").clicked() {
            if let Err(error) = apply_patch_to_draft(
                definitions,
                libraries,
                drafts,
                active_draft_id,
                DefinitionPatch::RemoveDerivedParameter {
                    name: derived_name.to_string(),
                },
            ) {
                status.set_feedback(error, 2.0);
            } else {
                state.selected_node = DefinitionEditorNode::Definition;
                state.derived_editor_buffer.clear();
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Technical view
// ---------------------------------------------------------------------------

/// Technical JSON view for the currently selected node.
///
/// Only the center context-editor pane swaps; the property tree, preview, and
/// assets strip remain visible.
///
/// Per the agreement: "If a technical edit cannot round-trip through the same
/// validation path, it is rejected in place."
#[allow(clippy::too_many_arguments)]
fn draw_technical_view(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    status: &mut StatusBarData,
) {
    let def = &active_draft.working_copy;

    ui.label(egui::RichText::new("Technical view").strong());
    ui.label(
        egui::RichText::new("Edits are validated before applying.  Invalid JSON is rejected in place.")
            .small()
            .weak(),
    );
    ui.separator();

    // Populate buffer from the selected node whenever it is empty (i.e., after
    // a node change that clears the buffer).
    let selected_node = state.selected_node.clone();
    if state.technical_view_buffer.is_empty() {
        state.technical_view_buffer = technical_view_json_for_node(&selected_node, active_draft);
        state.technical_view_error = None;
    }

    // Node label
    let node_label = match &selected_node {
        DefinitionEditorNode::Definition => format!("Definition: {}", def.name),
        DefinitionEditorNode::Parameter(n) => format!("Parameter: {n}"),
        DefinitionEditorNode::Slot(id) => format!("Slot: {id}"),
        DefinitionEditorNode::SlotParameterBinding { slot_id, parameter_name } => {
            format!("Binding: {slot_id} → {parameter_name}")
        }
        DefinitionEditorNode::Anchor(id) => format!("Anchor: {id}"),
        DefinitionEditorNode::Constraint(id) => format!("Constraint: {id}"),
        DefinitionEditorNode::DerivedParameter(n) => format!("Derived: {n}"),
    };
    ui.label(egui::RichText::new(node_label).small().weak());
    ui.add_space(4.0);

    ui.add(
        egui::TextEdit::multiline(&mut state.technical_view_buffer)
            .desired_rows(16)
            .code_editor(),
    );

    if let Some(error) = &state.technical_view_error.clone() {
        ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
    }

    ui.add_space(4.0);
    if ui.button("Apply").clicked() {
        let result = apply_technical_view_edit(
            &selected_node,
            &state.technical_view_buffer,
            definitions,
            libraries,
            drafts,
            active_draft_id,
            active_draft,
        );
        match result {
            Ok(()) => {
                state.technical_view_error = None;
                status.set_feedback("Technical edit applied".to_string(), 2.0);
            }
            Err(error) => {
                state.technical_view_error = Some(error.clone());
                status.set_feedback(error, 2.0);
            }
        }
    }
}

/// Serialise the currently selected node to JSON for the Technical view buffer.
fn technical_view_json_for_node(
    selected_node: &DefinitionEditorNode,
    draft: &DefinitionDraft,
) -> String {
    let def = &draft.working_copy;
    let compound = def.compound.as_ref();
    match selected_node {
        DefinitionEditorNode::Definition => {
            // Full definition minus compound.child_slots — editing the compound
            // through Technical view is done by selecting the individual slot.
            let mut value = serde_json::to_value(def).unwrap_or(json!(null));
            if let Some(obj) = value.as_object_mut() {
                if let Some(compound_val) = obj.get_mut("compound") {
                    if let Some(compound_obj) = compound_val.as_object_mut() {
                        compound_obj.remove("child_slots");
                    }
                }
            }
            pretty_json(&value)
        }
        DefinitionEditorNode::Parameter(name) => {
            def.interface
                .parameters
                .get(name)
                .map(|p| pretty_json(p))
                .unwrap_or_else(|| "{}".to_string())
        }
        DefinitionEditorNode::Slot(slot_id) => compound
            .and_then(|c| c.child_slots.iter().find(|s| s.slot_id == *slot_id))
            .map(|s| pretty_json(s))
            .unwrap_or_else(|| "{}".to_string()),
        DefinitionEditorNode::SlotParameterBinding {
            slot_id,
            parameter_name,
        } => compound
            .and_then(|c| c.child_slots.iter().find(|s| s.slot_id == *slot_id))
            .and_then(|s| {
                s.parameter_bindings
                    .iter()
                    .find(|b| b.target_param == *parameter_name)
            })
            .map(|b| pretty_json(b))
            .unwrap_or_else(|| "{}".to_string()),
        DefinitionEditorNode::Anchor(anchor_id) => compound
            .and_then(|c| c.anchors.iter().find(|a| a.id == *anchor_id))
            .map(|a| pretty_json(a))
            .unwrap_or_else(|| "{}".to_string()),
        DefinitionEditorNode::Constraint(constraint_id) => compound
            .and_then(|c| c.constraints.iter().find(|c| c.id == *constraint_id))
            .map(|c| pretty_json(c))
            .unwrap_or_else(|| "{}".to_string()),
        DefinitionEditorNode::DerivedParameter(name) => compound
            .and_then(|c| c.derived_parameters.iter().find(|d| d.name == *name))
            .map(|d| pretty_json(d))
            .unwrap_or_else(|| "{}".to_string()),
    }
}

/// Parse the Technical view buffer and apply the edit through the typed patch
/// path, followed by `validate_draft`.  Returns `Err` if the JSON is invalid
/// or the validated draft rejects the edit.
#[allow(clippy::too_many_arguments)]
fn apply_technical_view_edit(
    selected_node: &DefinitionEditorNode,
    buffer: &str,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
) -> Result<(), String> {
    use crate::plugins::modeling::definition as def_mod;
    let patch = match selected_node {
        DefinitionEditorNode::Definition => {
            // Parse into a Value and apply patchable fields individually to
            // avoid overwriting child_slots (which were stripped in the buffer).
            let value: serde_json::Map<String, Value> = serde_json::from_str(buffer)
                .map_err(|e| format!("JSON parse error: {e}"))?;
            // Apply name if changed
            if let Some(name) = value.get("name").and_then(Value::as_str) {
                if name != active_draft.working_copy.name {
                    apply_patch_to_draft(
                        definitions,
                        libraries,
                        drafts,
                        active_draft_id,
                        DefinitionPatch::SetName {
                            name: name.to_string(),
                        },
                    )?;
                }
            }
            // Apply domain_data if present
            if let Some(dd) = value.get("domain_data") {
                apply_patch_to_draft(
                    definitions,
                    libraries,
                    drafts,
                    active_draft_id,
                    DefinitionPatch::SetDomainData { value: dd.clone() },
                )?;
            }
            // Apply evaluators if present
            if let Some(evs) = value.get("evaluators") {
                let evaluators: Vec<def_mod::EvaluatorDecl> =
                    serde_json::from_value(evs.clone())
                        .map_err(|e| format!("evaluators parse error: {e}"))?;
                apply_patch_to_draft(
                    definitions,
                    libraries,
                    drafts,
                    active_draft_id,
                    DefinitionPatch::SetEvaluators { evaluators },
                )?;
            }
            // Apply representations if present
            if let Some(reps) = value.get("representations") {
                let representations: Vec<def_mod::RepresentationDecl> =
                    serde_json::from_value(reps.clone())
                        .map_err(|e| format!("representations parse error: {e}"))?;
                apply_patch_to_draft(
                    definitions,
                    libraries,
                    drafts,
                    active_draft_id,
                    DefinitionPatch::SetRepresentations { representations },
                )?;
            }
            // Validate after all patches
            let refreshed = drafts.get(active_draft_id)
                .cloned()
                .ok_or_else(|| "Draft not found after patch".to_string())?;
            validate_draft(definitions, libraries, &refreshed)
                .map_err(|e| format!("Validation failed: {e}"))?;
            return Ok(());
        }
        DefinitionEditorNode::Parameter(_) => {
            let parameter: def_mod::ParameterDef = serde_json::from_str(buffer)
                .map_err(|e| format!("JSON parse error: {e}"))?;
            DefinitionPatch::SetParameter { parameter }
        }
        DefinitionEditorNode::Slot(_) => {
            let child_slot: def_mod::ChildSlotDef = serde_json::from_str(buffer)
                .map_err(|e| format!("JSON parse error: {e}"))?;
            DefinitionPatch::SetChildSlot { child_slot }
        }
        DefinitionEditorNode::SlotParameterBinding { slot_id, .. } => {
            let binding: def_mod::ParameterBinding = serde_json::from_str(buffer)
                .map_err(|e| format!("JSON parse error: {e}"))?;
            DefinitionPatch::SetChildSlotBinding {
                slot_id: slot_id.clone(),
                binding,
            }
        }
        DefinitionEditorNode::Anchor(_) => {
            let anchor: def_mod::AnchorDef = serde_json::from_str(buffer)
                .map_err(|e| format!("JSON parse error: {e}"))?;
            DefinitionPatch::SetAnchor { anchor }
        }
        DefinitionEditorNode::Constraint(_) => {
            let constraint: def_mod::ConstraintDef = serde_json::from_str(buffer)
                .map_err(|e| format!("JSON parse error: {e}"))?;
            DefinitionPatch::SetConstraint { constraint }
        }
        DefinitionEditorNode::DerivedParameter(_) => {
            let derived_parameter: def_mod::DerivedParameterDef = serde_json::from_str(buffer)
                .map_err(|e| format!("JSON parse error: {e}"))?;
            DefinitionPatch::SetDerivedParameter { derived_parameter }
        }
    };

    apply_patch_to_draft(definitions, libraries, drafts, active_draft_id, patch)?;
    // Validate after the patch
    let refreshed = drafts
        .get(active_draft_id)
        .cloned()
        .ok_or_else(|| "Draft not found after patch".to_string())?;
    validate_draft(definitions, libraries, &refreshed)
        .map_err(|e| format!("Validation failed after patch: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Assets strip
// ---------------------------------------------------------------------------

/// Right-hand assets strip: representations list and material affordances.
///
/// PP-DBUX5 replaces the orphan `Use Glass Material` button with an inline
/// material chip system.
#[allow(clippy::too_many_arguments)]
fn draw_assets_strip(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    status: &mut StatusBarData,
) {
    let def = &active_draft.working_copy;

    ui.label(egui::RichText::new("Assets").strong());
    ui.separator();

    // Representations
    ui.label(
        egui::RichText::new(format!("Representations ({})", def.representations.len()))
            .small()
            .strong(),
    );
    if def.representations.is_empty() {
        ui.label(egui::RichText::new("None").small().weak());
    } else {
        for rep in &def.representations {
            ui.label(
                egui::RichText::new(format!(
                    "{:?} / {:?}",
                    rep.kind, rep.role
                ))
                .small(),
            );
        }
    }

    ui.add_space(8.0);

    // PP-DBUX5: replace with material chip
    if definition_is_glazing(def) {
        ui.label(egui::RichText::new("Material").small().strong());
        // PP-DBUX5: replace with material chip
        #[allow(deprecated)]
        if ui.button("Use Glass Material").clicked() {
            let domain_data = domain_data_with_glass_material(&def.domain_data);
            match apply_patch_to_draft(
                definitions,
                libraries,
                drafts,
                active_draft_id,
                DefinitionPatch::SetDomainData {
                    value: domain_data.clone(),
                },
            ) {
                Ok(()) => {
                    state.domain_data_buffer = pretty_json(&domain_data);
                    status.set_feedback("Glass material assigned".to_string(), 2.0);
                }
                Err(error) => status.set_feedback(error, 2.0),
            }
        }
    }
}

/// Reset per-draft editor state whenever the active draft switches.
///
/// PP-DBUX3: the deprecated legacy per-kind selection fields
/// (`selected_slot_id`, `selected_derived_name`, etc.) and
/// `sync_legacy_selection_fields` have been removed.  `selected_node` is now
/// the single source of truth for selection.
fn sync_inspector_state(state: &mut DefinitionsWindowState, draft: &DefinitionDraft) {
    if state.selected_draft_id.as_deref() == Some(draft.draft_id.0.as_str()) {
        // Same draft — keep the current selected_node as-is.
        return;
    }
    // New or switched draft: reset selection and all buffers.
    state.selected_draft_id = Some(draft.draft_id.0.clone());
    state.new_definition_name = draft.working_copy.name.clone();
    state.domain_data_buffer = pretty_json(&draft.working_copy.domain_data);
    state.evaluators_buffer = pretty_json(&draft.working_copy.evaluators);
    state.representations_buffer = pretty_json(&draft.working_copy.representations);
    state.selected_node = DefinitionEditorNode::Definition;
    state.technical_view_buffer.clear();
    state.technical_view_error = None;
    state.slot_editor_buffer.clear();
    state.selected_slot_role_buffer.clear();
    state.selected_slot_definition_buffer.clear();
    state.selected_slot_translation_buffer.clear();
    state.derived_editor_buffer.clear();
    state.constraint_editor_buffer.clear();
    state.anchor_editor_buffer.clear();
}

fn sync_selected_slot_buffers(
    state: &mut DefinitionsWindowState,
    slot: &crate::plugins::modeling::definition::ChildSlotDef,
) {
    state.slot_editor_buffer = pretty_json(slot);
    state.selected_slot_role_buffer = slot.role.clone();
    state.selected_slot_definition_buffer = slot.definition_id.to_string();
    state.selected_slot_translation_buffer = slot_translation_to_text(slot);
}

fn slot_translation_to_text(slot: &crate::plugins::modeling::definition::ChildSlotDef) -> String {
    let Some(translation) = &slot.transform_binding.translation else {
        return "0, 0, 0".to_string();
    };
    if translation.len() != 3 {
        return String::new();
    }
    translation
        .iter()
        .map(expr_to_short_text)
        .collect::<Vec<_>>()
        .join(", ")
}

fn expr_to_short_text(expr: &crate::plugins::modeling::definition::ExprNode) -> String {
    match expr {
        crate::plugins::modeling::definition::ExprNode::Literal { value } => compact_json(value),
        crate::plugins::modeling::definition::ExprNode::ParamRef { path } => path.clone(),
        _ => serde_json::to_string(expr).unwrap_or_else(|_| "?".to_string()),
    }
}

fn build_slot_from_editor_buffers(
    state: &DefinitionsWindowState,
    original: &crate::plugins::modeling::definition::ChildSlotDef,
) -> Result<crate::plugins::modeling::definition::ChildSlotDef, String> {
    let mut slot = original.clone();
    let role = state.selected_slot_role_buffer.trim();
    if role.is_empty() {
        return Err("Component role is required".to_string());
    }
    let definition_id = state.selected_slot_definition_buffer.trim();
    if definition_id.is_empty() {
        return Err("Component definition is required".to_string());
    }
    slot.role = role.to_string();
    slot.definition_id = DefinitionId(definition_id.to_string());
    slot.transform_binding.translation =
        parse_translation_binding(&state.selected_slot_translation_buffer)?;
    Ok(slot)
}

fn parse_translation_binding(
    text: &str,
) -> Result<Option<Vec<crate::plugins::modeling::definition::ExprNode>>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parts = trimmed
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err("Position must be three comma-separated values".to_string());
    }
    Ok(Some(
        parts
            .into_iter()
            .map(|part| {
                if let Ok(value) = part.parse::<f64>() {
                    crate::plugins::modeling::definition::ExprNode::Literal {
                        value: Value::from(value),
                    }
                } else {
                    crate::plugins::modeling::definition::ExprNode::ParamRef {
                        path: part.to_string(),
                    }
                }
            })
            .collect(),
    ))
}

fn build_parameter_from_state(
    state: &DefinitionsWindowState,
) -> Result<crate::plugins::modeling::definition::ParameterDef, String> {
    let name = state.new_parameter_name.trim();
    if name.is_empty() {
        return Err("Parameter name is required".to_string());
    }
    let param_type = match state.new_parameter_type.trim() {
        "" | "numeric" => crate::plugins::modeling::definition::ParamType::Numeric,
        "boolean" => crate::plugins::modeling::definition::ParamType::Boolean,
        "string" => crate::plugins::modeling::definition::ParamType::StringVal,
        other => {
            return Err(format!(
                "Unsupported parameter type '{}'. Use numeric, boolean, or string",
                other
            ))
        }
    };
    let override_policy = match state.new_parameter_override_policy.trim() {
        "" | "overridable" => crate::plugins::modeling::definition::OverridePolicy::Overridable,
        "locked" => crate::plugins::modeling::definition::OverridePolicy::Locked,
        "required" => crate::plugins::modeling::definition::OverridePolicy::Required,
        other => {
            return Err(format!(
                "Unsupported override policy '{}'. Use overridable, locked, or required",
                other
            ))
        }
    };
    Ok(crate::plugins::modeling::definition::ParameterDef {
        name: name.to_string(),
        param_type,
        default_value: parse_json_or_string(&state.new_parameter_default),
        override_policy,
        metadata: crate::plugins::modeling::definition::ParameterMetadata {
            unit: (!state.new_parameter_unit.trim().is_empty())
                .then_some(state.new_parameter_unit.trim().to_string()),
            ..Default::default()
        },
    })
}

fn parse_json_or_string(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

fn pretty_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string())
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

/// Short human-readable summary of an expression node for display in the
/// property tree (slot parameter binding rows).
fn compact_json_expr(expr: &crate::plugins::modeling::definition::ExprNode) -> String {
    use crate::plugins::modeling::definition::ExprNode;
    match expr {
        ExprNode::Literal { value } => compact_json(value),
        ExprNode::ParamRef { path } => path.clone(),
        _ => serde_json::to_string(expr).unwrap_or_else(|_| "…".to_string()),
    }
}

fn definition_is_glazing(definition: &Definition) -> bool {
    definition
        .id
        .to_string()
        .to_ascii_lowercase()
        .contains("glazing")
        || definition.name.to_ascii_lowercase().contains("glazing")
}

fn domain_data_with_glass_material(current: &Value) -> Value {
    let mut domain_data = current.clone();
    if !domain_data.is_object() {
        domain_data = json!({});
    }
    let Some(root) = domain_data.as_object_mut() else {
        return domain_data;
    };
    let architectural = root.entry("architectural").or_insert_with(|| json!({}));
    if !architectural.is_object() {
        *architectural = json!({});
    }
    if let Some(architectural) = architectural.as_object_mut() {
        architectural.insert(
            "material_assignment".to_string(),
            json!({
                "material_id": crate::plugins::materials::BUILTIN_MATERIAL_BLUE_TINT_GLAZING_80
            }),
        );
    }
    domain_data
}

pub fn library_effective_definition(
    libraries: &DefinitionLibraryRegistry,
    library_id: &str,
    definition_id: &str,
) -> Result<Definition, String> {
    let library = libraries
        .get(&DefinitionLibraryId(library_id.to_string()))
        .ok_or_else(|| format!("Definition library '{}' not found", library_id))?;
    let mut registry = DefinitionRegistry::default();
    for definition in library.definitions.values() {
        registry.insert(definition.clone());
    }
    registry.effective_definition(&DefinitionId(definition_id.to_string()))
}

pub fn definition_requires_opening_host_definition(definition: &Definition) -> bool {
    if definition.interface.void_declaration.is_some() {
        return true;
    }
    definition
        .domain_data
        .get("architectural")
        .and_then(|architectural| architectural.get("void_declaration"))
        .and_then(|declaration| declaration.get("kind"))
        .and_then(Value::as_str)
        == Some("opening")
}

fn definition_requires_opening_host(world: &World, definition_id: &str) -> Result<bool, String> {
    let definition = world
        .resource::<DefinitionRegistry>()
        .effective_definition(&DefinitionId(definition_id.to_string()))?;
    Ok(definition_requires_opening_host_definition(&definition))
}

fn ensure_definition_available(
    world: &mut World,
    definition_id: &str,
    library_id: Option<&str>,
) -> Result<Vec<String>, String> {
    let def_id = DefinitionId(definition_id.to_string());
    if world
        .resource::<DefinitionRegistry>()
        .get(&def_id)
        .is_some()
    {
        return Ok(Vec::new());
    }

    let library_id = library_id.ok_or_else(|| {
        format!(
            "Definition '{}' is not present in the document; provide a library_id",
            definition_id
        )
    })?;
    let library_id = DefinitionLibraryId(library_id.to_string());
    let library = world
        .resource::<DefinitionLibraryRegistry>()
        .get(&library_id)
        .cloned()
        .ok_or_else(|| format!("Definition library '{}' not found", library_id))?;
    let root_definition = library.get(&def_id).cloned().ok_or_else(|| {
        format!(
            "Definition '{}' not found in library '{}'",
            definition_id, library_id
        )
    })?;

    let mut to_import = vec![root_definition];
    let mut imported = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while let Some(definition) = to_import.pop() {
        if !seen.insert(definition.id.clone()) {
            continue;
        }
        if let Some(base_definition_id) = &definition.base_definition_id {
            if let Some(base_definition) = library.get(base_definition_id).cloned() {
                to_import.push(base_definition);
            }
        }
        if let Some(compound) = &definition.compound {
            for slot in &compound.child_slots {
                if let Some(child) = library.get(&slot.definition_id).cloned() {
                    to_import.push(child);
                }
            }
        }
        if world
            .resource::<DefinitionRegistry>()
            .get(&definition.id)
            .is_none()
        {
            imported.push(definition.id.to_string());
            enqueue_create_definition(world, definition);
        }
    }
    apply_pending_history_commands(world);
    Ok(imported)
}

fn build_hosted_occurrence_context(
    context: &DefinitionSelectionContext,
) -> HostedOccurrenceContext {
    let mut anchors = Vec::new();
    if let Some(center) = context.opening_center {
        anchors.push(HostedAnchor {
            id: "opening.center".to_string(),
            kind: Some("opening_center".to_string()),
            position: center.to_array(),
        });
        if let Some(thickness) = context.wall_thickness {
            let half = thickness as f32 * 0.5;
            let normal = context
                .wall_axis
                .map(|axis| Vec3::new(-axis.z, 0.0, axis.x).normalize_or_zero())
                .filter(|normal| normal.length_squared() > 0.0)
                .unwrap_or(Vec3::Z);
            anchors.push(HostedAnchor {
                id: "opening.exterior_face".to_string(),
                kind: Some("host_exterior_face".to_string()),
                position: (center - normal * half).to_array(),
            });
            anchors.push(HostedAnchor {
                id: "opening.interior_face".to_string(),
                kind: Some("host_interior_face".to_string()),
                position: (center + normal * half).to_array(),
            });
        }
    }
    HostedOccurrenceContext {
        host_element_id: context.wall_element_id,
        opening_element_id: context.opening_element_id,
        anchors,
    }
}

fn wall_host_rotation(context: &DefinitionSelectionContext) -> Option<Quat> {
    let axis = context.wall_axis?;
    let planar = Vec2::new(axis.x, axis.z).try_normalize()?;
    let angle = planar.y.atan2(planar.x);
    Some(Quat::from_rotation_y(-angle))
}

fn apply_contextual_window_overrides(
    definition: &Definition,
    hosted: Option<&DefinitionSelectionContext>,
    overrides: &mut OverrideMap,
) {
    let Some(hosted) = hosted else {
        return;
    };
    let auto_values = [
        ("overall_width", hosted.opening_width.map(Value::from)),
        ("overall_height", hosted.opening_height.map(Value::from)),
        ("wall_thickness", hosted.wall_thickness.map(Value::from)),
    ];
    for (parameter_name, value) in auto_values {
        if overrides.get(parameter_name).is_some() {
            continue;
        }
        let Some(value) = value else {
            continue;
        };
        if let Some(parameter) = definition.interface.parameters.get(parameter_name) {
            if parameter.override_policy
                != crate::plugins::modeling::definition::OverridePolicy::Locked
            {
                overrides.set(parameter_name.to_string(), value);
            }
        }
    }
}

fn parse_override_map(value: Option<&Value>) -> Result<OverrideMap, String> {
    let mut overrides = OverrideMap::default();
    let Some(value) = value else {
        return Ok(overrides);
    };
    let map = value
        .as_object()
        .ok_or_else(|| "'overrides' must be a JSON object".to_string())?;
    for (key, entry) in map {
        overrides.set(key.clone(), entry.clone());
    }
    Ok(overrides)
}

fn explicit_offset(object: &serde_json::Map<String, Value>) -> Option<Vec3> {
    object
        .get("offset")
        .and_then(|value| serde_json::from_value::<[f32; 3]>(value.clone()).ok())
        .map(|[x, y, z]| Vec3::new(x, y, z))
}

fn required_string(object: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("Missing '{key}'"))
}

fn optional_string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}
