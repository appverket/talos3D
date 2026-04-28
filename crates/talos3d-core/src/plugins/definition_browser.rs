use std::collections::HashMap;

use bevy::{ecs::world::EntityRef, prelude::*};
use bevy_egui::egui;
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
            apply_patch_to_draft, blank_definition, compile_definition_summary,
            draft_effective_definition, preview_registry_for_draft, validate_draft,
            DefinitionDraft, DefinitionDraftId, DefinitionDraftRegistry, DefinitionPatch,
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
    pub inspector_tab: String,
    pub selected_slot_id: Option<String>,
    pub selected_derived_name: Option<String>,
    pub selected_constraint_id: Option<String>,
    pub selected_anchor_id: Option<String>,
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
    let definition_id = selected_occurrence_definition_id(world)?;
    let output = execute_open_definition_draft(
        world,
        &json!({
            "definition_id": definition_id.to_string(),
        }),
    )?;
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

fn selected_occurrence_definition_id(world: &mut World) -> Result<DefinitionId, String> {
    let mut selected_query = world.query_filtered::<Entity, With<Selected>>();
    let selected_entities: Vec<Entity> = selected_query.iter(world).collect();
    if selected_entities.len() != 1 {
        return Err("Select exactly one occurrence".to_string());
    }

    let selected = selected_entities[0];
    if let Some(identity) = world.get::<OccurrenceIdentity>(selected) {
        return Ok(identity.definition_id.clone());
    }

    if let Some(relation) = world.get::<SemanticRelation>(selected) {
        if relation.relation_type == "hosted_on" {
            if let Some(definition_id) = occurrence_definition_for_element(world, relation.source) {
                return Ok(definition_id);
            }
        }
    }

    if let Some(generated) =
        world.get::<crate::plugins::modeling::occurrence::GeneratedOccurrencePart>(selected)
    {
        if let Some(definition_id) = occurrence_definition_for_element(world, generated.owner) {
            return Ok(definition_id);
        }
    }

    if let Some(opening_id) = world.get::<ElementId>(selected).copied() {
        if let Some(definition_id) = occurrence_definition_for_hosted_opening(world, opening_id) {
            return Ok(definition_id);
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
    state: &mut DefinitionsWindowState,
    selection: &DefinitionSelectionContext,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    pending: &mut PendingCommandInvocations,
    cursor_world_pos: &CursorWorldPos,
    status: &mut StatusBarData,
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
    draw_definition_inspector(ctx, state, definitions, libraries, drafts, pending, status);
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

fn draw_definition_inspector(
    ctx: &egui::Context,
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

    let validation_result = validate_draft(definitions, libraries, &active_draft);
    let preview_registry = preview_registry_for_draft(definitions, libraries, &active_draft).ok();
    let compile_result = preview_registry
        .as_ref()
        .and_then(|preview| compile_definition_summary(preview, &active_draft.working_copy).ok());
    let effective_definition =
        draft_effective_definition(definitions, libraries, &active_draft).ok();

    let inspector_rect =
        tool_window_rect(ctx, egui::pos2(568.0, 88.0), INSPECTOR_WINDOW_DEFAULT_SIZE);
    let mut open = state.inspector_visible;
    egui::Window::new("Definition Inspector")
        .id(egui::Id::new("definition_inspector"))
        .default_rect(inspector_rect)
        .min_size(INSPECTOR_WINDOW_MIN_SIZE)
        .max_size(tool_window_max_size(ctx, INSPECTOR_WINDOW_MAX_SIZE))
        .constrain_to(tool_window_bounds(ctx))
        .open(&mut open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (tab_id, label) in [
                    ("overview", "Overview"),
                    ("interface", "Interface"),
                    ("structure", "Structure"),
                    ("graph", "Graph"),
                ] {
                    if ui
                        .selectable_label(state.inspector_tab == tab_id, label)
                        .clicked()
                    {
                        state.inspector_tab = tab_id.to_string();
                    }
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("definitions.inspector.root")
                .auto_shrink([false, false])
                .show(ui, |ui| match state.inspector_tab.as_str() {
                    "interface" => draw_definition_interface_tab(
                        ui,
                        state,
                        definitions,
                        libraries,
                        drafts,
                        &active_draft_id,
                        &active_draft,
                        effective_definition.as_ref(),
                        status,
                    ),
                    "structure" => draw_definition_structure_tab(
                        ui,
                        state,
                        definitions,
                        libraries,
                        drafts,
                        pending,
                        &active_draft_id,
                        &active_draft,
                        status,
                    ),
                    "graph" => draw_definition_graph_tab(
                        ui,
                        state,
                        definitions,
                        libraries,
                        drafts,
                        &active_draft_id,
                        &active_draft,
                        status,
                    ),
                    _ => draw_definition_overview_tab(
                        ui,
                        state,
                        definitions,
                        libraries,
                        drafts,
                        &active_draft_id,
                        &active_draft,
                        validation_result.as_ref().err(),
                        compile_result.as_ref(),
                        pending,
                        status,
                    ),
                });
        });
    state.inspector_visible = open;
}

#[allow(clippy::too_many_arguments)]
fn draw_definition_overview_tab(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    validation_error: Option<&String>,
    compile_result: Option<&crate::plugins::definition_authoring::DefinitionCompileSummary>,
    pending: &mut PendingCommandInvocations,
    status: &mut StatusBarData,
) {
    ui.label(egui::RichText::new(&active_draft.working_copy.name).strong());
    ui.label(
        egui::RichText::new(active_draft.working_copy.id.to_string())
            .small()
            .monospace(),
    );
    ui.label(format!(
        "Dirty: {}",
        if active_draft.dirty { "yes" } else { "no" }
    ));
    if let Some(source_definition_id) = &active_draft.source_definition_id {
        ui.label(format!(
            "Editing published definition {}",
            source_definition_id
        ));
    } else if let Some(base_definition_id) = &active_draft.working_copy.base_definition_id {
        ui.label(format!("Derived from {}", base_definition_id));
    } else {
        ui.label("Standalone draft");
    }
    if let Some(source_library_id) = &active_draft.source_library_id {
        ui.label(format!("Source library {}", source_library_id));
    }
    if definition_is_glazing(&active_draft.working_copy)
        && ui.button("Set Glass Material").clicked()
    {
        let domain_data = domain_data_with_glass_material(&active_draft.working_copy.domain_data);
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

    ui.separator();
    ui.horizontal(|ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut state.new_definition_name);
    });

    ui.separator();
    ui.label("Domain Data");
    ui.add(
        egui::TextEdit::multiline(&mut state.domain_data_buffer)
            .desired_rows(6)
            .code_editor(),
    );
    ui.horizontal(|ui| {
        if ui.button("Apply Domain Data").clicked() {
            queue_patch_from_buffer(
                state,
                active_draft_id,
                DefinitionPatch::SetDomainData {
                    value: parse_json_or_string(&state.domain_data_buffer),
                },
                pending,
            );
        }
        if ui.button("Apply Name").clicked() {
            queue_patch_from_buffer(
                state,
                active_draft_id,
                DefinitionPatch::SetName {
                    name: state.new_definition_name.clone(),
                },
                pending,
            );
        }
        if ui.button("Publish").clicked() {
            queue_command_invocation_resource(
                pending,
                "modeling.publish_definition_draft".to_string(),
                json!({ "draft_id": active_draft_id.to_string() }),
            );
        }
    });

    ui.separator();
    ui.label("Evaluators");
    ui.add(
        egui::TextEdit::multiline(&mut state.evaluators_buffer)
            .desired_rows(4)
            .code_editor(),
    );
    if ui.button("Apply Evaluators").clicked() {
        match serde_json::from_str::<Vec<crate::plugins::modeling::definition::EvaluatorDecl>>(
            &state.evaluators_buffer,
        ) {
            Ok(evaluators) => {
                if let Err(error) = apply_patch_to_draft(
                    definitions,
                    libraries,
                    drafts,
                    active_draft_id,
                    DefinitionPatch::SetEvaluators { evaluators },
                ) {
                    status.set_feedback(error, 2.0);
                }
            }
            Err(error) => status.set_feedback(error.to_string(), 2.0),
        }
    }

    ui.label("Representations");
    ui.add(
        egui::TextEdit::multiline(&mut state.representations_buffer)
            .desired_rows(4)
            .code_editor(),
    );
    if ui.button("Apply Representations").clicked() {
        match serde_json::from_str::<Vec<crate::plugins::modeling::definition::RepresentationDecl>>(
            &state.representations_buffer,
        ) {
            Ok(representations) => {
                if let Err(error) = apply_patch_to_draft(
                    definitions,
                    libraries,
                    drafts,
                    active_draft_id,
                    DefinitionPatch::SetRepresentations { representations },
                ) {
                    status.set_feedback(error, 2.0);
                }
            }
            Err(error) => status.set_feedback(error.to_string(), 2.0),
        }
    }

    ui.separator();
    if let Some(error) = validation_error {
        ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
    } else {
        ui.label("Definition validates successfully.");
    }
    if let Some(compile) = compile_result {
        ui.label(format!(
            "{} nodes, {} edges, {} child slots, {} derived, {} constraints",
            compile.nodes.len(),
            compile.edges.len(),
            compile.child_slot_count,
            compile.derived_parameter_count,
            compile.constraint_count,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_definition_interface_tab(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    effective_definition: Option<&Definition>,
    status: &mut StatusBarData,
) {
    let Some(effective_definition) = effective_definition else {
        ui.label("This draft is currently invalid; fix the graph before editing inputs here.");
        return;
    };

    egui::ScrollArea::vertical()
        .id_salt("definitions.inspector.interface")
        .show(ui, |ui| {
            for parameter in &effective_definition.interface.parameters.0 {
                if parameter.metadata.mutability
                    == crate::plugins::modeling::definition::ParameterMutability::Derived
                {
                    continue;
                }
                let default_key = format!("{}:{}", active_draft_id, parameter.name);
                state
                    .parameter_default_buffers
                    .entry(default_key.clone())
                    .or_insert_with(|| compact_json(&parameter.default_value));
                state
                    .parameter_unit_buffers
                    .entry(default_key.clone())
                    .or_insert_with(|| parameter.metadata.unit.clone().unwrap_or_default());
                let local = active_draft
                    .working_copy
                    .interface
                    .parameters
                    .get(&parameter.name)
                    .is_some();
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&parameter.name).strong());
                        ui.label(if local { "local" } else { "inherited" });
                        ui.label(format!("{:?}", parameter.override_policy));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Default");
                        if let Some(buffer) = state.parameter_default_buffers.get_mut(&default_key)
                        {
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
                        if ui.button("Apply").clicked() {
                            let mut updated = parameter.clone();
                            if let Some(buffer) = state.parameter_default_buffers.get(&default_key)
                            {
                                updated.default_value = parse_json_or_string(buffer);
                            }
                            if let Some(buffer) = state.parameter_unit_buffers.get(&default_key) {
                                updated.metadata.unit =
                                    (!buffer.trim().is_empty()).then_some(buffer.clone());
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
                                status.set_feedback(
                                    format!("Updated parameter '{}'", parameter.name),
                                    2.0,
                                );
                            }
                        }
                        if local && ui.button("Remove Local Override").clicked() {
                            if let Err(error) = apply_patch_to_draft(
                                definitions,
                                libraries,
                                drafts,
                                active_draft_id,
                                DefinitionPatch::RemoveParameter {
                                    name: parameter.name.clone(),
                                },
                            ) {
                                status.set_feedback(error, 2.0);
                            }
                        }
                    });
                });
                ui.separator();
            }

            ui.collapsing("Add Parameter", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut state.new_parameter_name);
                });
                ui.horizontal(|ui| {
                    ui.label("Type");
                    ui.text_edit_singleline(&mut state.new_parameter_type);
                    ui.label("Override");
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
                            if let Err(error) = apply_patch_to_draft(
                                definitions,
                                libraries,
                                drafts,
                                active_draft_id,
                                DefinitionPatch::SetParameter { parameter },
                            ) {
                                status.set_feedback(error, 2.0);
                            } else {
                                status.set_feedback("Added parameter".to_string(), 2.0);
                                state.new_parameter_name.clear();
                                state.new_parameter_default.clear();
                                state.new_parameter_unit.clear();
                            }
                        }
                        Err(error) => status.set_feedback(error, 2.0),
                    }
                }
            });
        });
}

#[allow(clippy::too_many_arguments)]
fn draw_definition_structure_tab(
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
    let child_slots = active_draft
        .working_copy
        .compound
        .as_ref()
        .map(|compound| compound.child_slots.clone())
        .unwrap_or_default();
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(egui::RichText::new("Child Slots").strong());
            egui::ScrollArea::vertical()
                .id_salt("definitions.inspector.structure.slots")
                .max_height(260.0)
                .show(ui, |ui| {
                    for slot in &child_slots {
                        let selected =
                            state.selected_slot_id.as_deref() == Some(slot.slot_id.as_str());
                        if ui
                            .selectable_label(selected, format!("{} ({})", slot.slot_id, slot.role))
                            .clicked()
                        {
                            state.selected_slot_id = Some(slot.slot_id.clone());
                            state.slot_editor_buffer = pretty_json(slot);
                        }
                    }
                });
            ui.separator();
            ui.label("Add Slot");
            ui.text_edit_singleline(&mut state.new_slot_id);
            ui.text_edit_singleline(&mut state.new_slot_role);
            ui.text_edit_singleline(&mut state.new_slot_definition_id);
            if ui.button("Add Child Slot").clicked() {
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
                        definition_id: DefinitionId(
                            state.new_slot_definition_id.trim().to_string(),
                        ),
                        parameter_bindings: Vec::new(),
                        transform_binding: Default::default(),
                        suppression_expr: None,
                    };
                    if let Err(error) = apply_patch_to_draft(
                        definitions,
                        libraries,
                        drafts,
                        active_draft_id,
                        DefinitionPatch::SetChildSlot { child_slot: slot },
                    ) {
                        status.set_feedback(error, 2.0);
                    } else {
                        status.set_feedback("Added child slot".to_string(), 2.0);
                    }
                }
            }
        });
        ui.separator();
        ui.vertical(|ui| {
            if let Some(slot_id) = state.selected_slot_id.clone() {
                ui.label(egui::RichText::new(&slot_id).strong());
                if let Some(slot) = child_slots.iter().find(|slot| slot.slot_id == slot_id) {
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
                }
                ui.add(
                    egui::TextEdit::multiline(&mut state.slot_editor_buffer)
                        .desired_rows(18)
                        .code_editor(),
                );
                ui.horizontal(|ui| {
                    if ui.button("Apply Slot JSON").clicked() {
                        match serde_json::from_str::<
                            crate::plugins::modeling::definition::ChildSlotDef,
                        >(&state.slot_editor_buffer)
                        {
                            Ok(slot) => {
                                if let Err(error) = apply_patch_to_draft(
                                    definitions,
                                    libraries,
                                    drafts,
                                    active_draft_id,
                                    DefinitionPatch::SetChildSlot { child_slot: slot },
                                ) {
                                    status.set_feedback(error, 2.0);
                                }
                            }
                            Err(error) => status.set_feedback(error.to_string(), 2.0),
                        }
                    }
                    if ui.button("Remove Slot").clicked() {
                        if let Err(error) = apply_patch_to_draft(
                            definitions,
                            libraries,
                            drafts,
                            active_draft_id,
                            DefinitionPatch::RemoveChildSlot {
                                slot_id: slot_id.clone(),
                            },
                        ) {
                            status.set_feedback(error, 2.0);
                        } else {
                            state.selected_slot_id = None;
                            state.slot_editor_buffer.clear();
                        }
                    }
                });
            } else {
                ui.label("Select a child slot to edit its JSON.");
            }
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_definition_graph_tab(
    ui: &mut egui::Ui,
    state: &mut DefinitionsWindowState,
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    active_draft_id: &DefinitionDraftId,
    active_draft: &DefinitionDraft,
    status: &mut StatusBarData,
) {
    let compound = active_draft
        .working_copy
        .compound
        .clone()
        .unwrap_or_default();
    ui.columns(3, |columns| {
        columns[0].vertical(|ui| {
            ui.label(egui::RichText::new("Derived").strong());
            egui::ScrollArea::vertical()
                .id_salt("definitions.inspector.graph.derived")
                .max_height(180.0)
                .show(ui, |ui| {
                    for entry in &compound.derived_parameters {
                        let selected =
                            state.selected_derived_name.as_deref() == Some(entry.name.as_str());
                        if ui.selectable_label(selected, &entry.name).clicked() {
                            state.selected_derived_name = Some(entry.name.clone());
                            state.derived_editor_buffer = pretty_json(entry);
                        }
                    }
                });
            if ui.button("New").clicked() {
                state.selected_derived_name = None;
                state.derived_editor_buffer = pretty_json(
                    &crate::plugins::modeling::definition::DerivedParameterDef {
                        name: "derived_param".to_string(),
                        param_type: crate::plugins::modeling::definition::ParamType::Numeric,
                        expr: crate::plugins::modeling::definition::ExprNode::Literal {
                            value: json!(0.0),
                        },
                        dependencies: Vec::new(),
                        metadata: Default::default(),
                    },
                );
            }
            ui.add(
                egui::TextEdit::multiline(&mut state.derived_editor_buffer)
                    .desired_rows(10)
                    .code_editor(),
            );
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    match serde_json::from_str::<
                        crate::plugins::modeling::definition::DerivedParameterDef,
                    >(&state.derived_editor_buffer)
                    {
                        Ok(derived_parameter) => {
                            if let Err(error) = apply_patch_to_draft(
                                definitions,
                                libraries,
                                drafts,
                                active_draft_id,
                                DefinitionPatch::SetDerivedParameter { derived_parameter },
                            ) {
                                status.set_feedback(error, 2.0);
                            }
                        }
                        Err(error) => status.set_feedback(error.to_string(), 2.0),
                    }
                }
                if let Some(selected) = state.selected_derived_name.clone() {
                    if ui.button("Remove").clicked() {
                        if let Err(error) = apply_patch_to_draft(
                            definitions,
                            libraries,
                            drafts,
                            active_draft_id,
                            DefinitionPatch::RemoveDerivedParameter { name: selected },
                        ) {
                            status.set_feedback(error, 2.0);
                        } else {
                            state.selected_derived_name = None;
                            state.derived_editor_buffer.clear();
                        }
                    }
                }
            });
        });

        columns[1].vertical(|ui| {
            ui.label(egui::RichText::new("Constraints").strong());
            egui::ScrollArea::vertical()
                .id_salt("definitions.inspector.graph.constraints")
                .max_height(180.0)
                .show(ui, |ui| {
                    for entry in &compound.constraints {
                        let selected =
                            state.selected_constraint_id.as_deref() == Some(entry.id.as_str());
                        if ui.selectable_label(selected, &entry.id).clicked() {
                            state.selected_constraint_id = Some(entry.id.clone());
                            state.constraint_editor_buffer = pretty_json(entry);
                        }
                    }
                });
            if ui.button("New").clicked() {
                state.selected_constraint_id = None;
                state.constraint_editor_buffer = pretty_json(
                    &crate::plugins::modeling::definition::ConstraintDef {
                        id: "constraint".to_string(),
                        expr: crate::plugins::modeling::definition::ExprNode::Literal {
                            value: json!(true),
                        },
                        dependencies: Vec::new(),
                        severity: crate::plugins::modeling::definition::ConstraintSeverity::Error,
                        message: "Constraint failed".to_string(),
                    },
                );
            }
            ui.add(
                egui::TextEdit::multiline(&mut state.constraint_editor_buffer)
                    .desired_rows(10)
                    .code_editor(),
            );
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    match serde_json::from_str::<crate::plugins::modeling::definition::ConstraintDef>(
                        &state.constraint_editor_buffer,
                    ) {
                        Ok(constraint) => {
                            if let Err(error) = apply_patch_to_draft(
                                definitions,
                                libraries,
                                drafts,
                                active_draft_id,
                                DefinitionPatch::SetConstraint { constraint },
                            ) {
                                status.set_feedback(error, 2.0);
                            }
                        }
                        Err(error) => status.set_feedback(error.to_string(), 2.0),
                    }
                }
                if let Some(selected) = state.selected_constraint_id.clone() {
                    if ui.button("Remove").clicked() {
                        if let Err(error) = apply_patch_to_draft(
                            definitions,
                            libraries,
                            drafts,
                            active_draft_id,
                            DefinitionPatch::RemoveConstraint { id: selected },
                        ) {
                            status.set_feedback(error, 2.0);
                        } else {
                            state.selected_constraint_id = None;
                            state.constraint_editor_buffer.clear();
                        }
                    }
                }
            });
        });

        columns[2].vertical(|ui| {
            ui.label(egui::RichText::new("Anchors").strong());
            egui::ScrollArea::vertical()
                .id_salt("definitions.inspector.graph.anchors")
                .max_height(180.0)
                .show(ui, |ui| {
                    for entry in &compound.anchors {
                        let selected =
                            state.selected_anchor_id.as_deref() == Some(entry.id.as_str());
                        if ui.selectable_label(selected, &entry.id).clicked() {
                            state.selected_anchor_id = Some(entry.id.clone());
                            state.anchor_editor_buffer = pretty_json(entry);
                        }
                    }
                });
            if ui.button("New").clicked() {
                state.selected_anchor_id = None;
                state.anchor_editor_buffer =
                    pretty_json(&crate::plugins::modeling::definition::AnchorDef {
                        id: "anchor.id".to_string(),
                        kind: "anchor_kind".to_string(),
                    });
            }
            ui.add(
                egui::TextEdit::multiline(&mut state.anchor_editor_buffer)
                    .desired_rows(10)
                    .code_editor(),
            );
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    match serde_json::from_str::<crate::plugins::modeling::definition::AnchorDef>(
                        &state.anchor_editor_buffer,
                    ) {
                        Ok(anchor) => {
                            if let Err(error) = apply_patch_to_draft(
                                definitions,
                                libraries,
                                drafts,
                                active_draft_id,
                                DefinitionPatch::SetAnchor { anchor },
                            ) {
                                status.set_feedback(error, 2.0);
                            }
                        }
                        Err(error) => status.set_feedback(error.to_string(), 2.0),
                    }
                }
                if let Some(selected) = state.selected_anchor_id.clone() {
                    if ui.button("Remove").clicked() {
                        if let Err(error) = apply_patch_to_draft(
                            definitions,
                            libraries,
                            drafts,
                            active_draft_id,
                            DefinitionPatch::RemoveAnchor { id: selected },
                        ) {
                            status.set_feedback(error, 2.0);
                        } else {
                            state.selected_anchor_id = None;
                            state.anchor_editor_buffer.clear();
                        }
                    }
                }
            });
        });
    });
}

fn sync_inspector_state(state: &mut DefinitionsWindowState, draft: &DefinitionDraft) {
    if state.selected_draft_id.as_deref() == Some(draft.draft_id.0.as_str()) {
        return;
    }
    state.selected_draft_id = Some(draft.draft_id.0.clone());
    state.inspector_tab = "overview".to_string();
    state.new_definition_name = draft.working_copy.name.clone();
    state.domain_data_buffer = pretty_json(&draft.working_copy.domain_data);
    state.evaluators_buffer = pretty_json(&draft.working_copy.evaluators);
    state.representations_buffer = pretty_json(&draft.working_copy.representations);
    state.selected_slot_id = None;
    state.selected_derived_name = None;
    state.selected_constraint_id = None;
    state.selected_anchor_id = None;
    state.slot_editor_buffer.clear();
    state.derived_editor_buffer.clear();
    state.constraint_editor_buffer.clear();
    state.anchor_editor_buffer.clear();
}

fn queue_patch_from_buffer(
    state: &mut DefinitionsWindowState,
    active_draft_id: &DefinitionDraftId,
    patch: DefinitionPatch,
    pending: &mut PendingCommandInvocations,
) {
    let _ = state;
    queue_command_invocation_resource(
        pending,
        "modeling.patch_definition_draft".to_string(),
        json!({
            "draft_id": active_draft_id.to_string(),
            "patch": patch,
        }),
    );
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
