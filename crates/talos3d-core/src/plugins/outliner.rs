//! Outliner panel — an indented, collapsible tree of the whole model's
//! aggregation structure.
//!
//! The panel is a *view* of the live ECS world. Each frame, while the panel is
//! open, `build_outliner_tree` walks the world and flattens the aggregation
//! hierarchy into an [`OutlinerTree`] arena resource. The egui renderer
//! ([`draw_outliner_window`]) — called from `egui_chrome` — reads that arena and
//! turns clicks into selection changes.
//!
//! Two containment mechanisms are unified into one tree:
//! - **Groups** (`GroupMembers`) aggregate other authored elements by
//!   `ElementId`, and may nest.
//! - **Compound occurrences** aggregate transient `GeneratedOccurrencePart`
//!   geometry under the owning occurrence.
//!
//! Display labels come from the generic `CapabilityRegistry::capture_snapshot`
//! path so any authored entity contributes its own `label()`/`type_name()`
//! without the outliner knowing about concrete entity kinds.

use std::collections::{HashMap, HashSet};

use bevy::{ecs::world::EntityRef, prelude::*};
use bevy_egui::egui;
use serde_json::Value;

use crate::capability_registry::CapabilityRegistry;
use crate::plugins::{
    command_registry::{CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult},
    egui_chrome::EguiChromeSystems,
    identity::ElementId,
    modeling::{
        assembly::SemanticRelation,
        group::GroupMembers,
        occurrence::{GeneratedOccurrencePart, OccurrenceIdentity},
    },
    selection::Selected,
    ui::StatusBarData,
};

const OUTLINER_DEFAULT_WIDTH: f32 = 280.0;
const OUTLINER_DEFAULT_HEIGHT: f32 = 420.0;
const OUTLINER_INDENT_PER_DEPTH: f32 = 14.0;
const OUTLINER_TOGGLE_WIDTH: f32 = 16.0;

/// What kind of model element a tree row represents. Used only for the row
/// glyph/styling — selection and traversal do not depend on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlinerKind {
    Group,
    Occurrence,
    Part,
    Leaf,
}

/// One row in the flattened outliner tree.
#[derive(Debug, Clone)]
pub struct OutlinerNode {
    /// Stable id used to key the per-row collapse state across frames.
    pub node_id: u64,
    /// Entity to (de)select when this row is clicked. For a generated part this
    /// is the owning occurrence, matching viewport selection semantics.
    pub select_entity: Entity,
    pub label: String,
    pub kind: OutlinerKind,
    /// Arena indices of child rows.
    pub children: Vec<usize>,
}

/// Flattened arena of [`OutlinerNode`]s rebuilt each frame while the panel is
/// open. `roots` holds the indices of top-level rows.
#[derive(Resource, Debug, Clone, Default)]
pub struct OutlinerTree {
    pub nodes: Vec<OutlinerNode>,
    pub roots: Vec<usize>,
}

impl OutlinerTree {
    fn clear(&mut self) {
        self.nodes.clear();
        self.roots.clear();
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct OutlinerWindowState {
    pub visible: bool,
    /// `node_id`s that are currently collapsed. Rows are expanded by default so
    /// the whole structure is visible the moment the panel opens.
    pub collapsed: HashSet<u64>,
}

/// A pending selection change produced by a click on an outliner row.
#[derive(Debug, Clone, Copy)]
pub struct OutlinerSelectAction {
    pub target: Entity,
    pub additive: bool,
}

pub struct OutlinerPlugin;

impl Plugin for OutlinerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OutlinerWindowState>()
            .init_resource::<OutlinerTree>()
            .register_command(
                CommandDescriptor {
                    id: "view.toggle_outliner".to_string(),
                    label: "Toggle Outliner".to_string(),
                    description: "Show or hide the model outline tree.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: Some("Ctrl/Cmd+Shift+O".to_string()),
                    icon: None,
                    hint: Some("Show or hide the model aggregation tree".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_toggle_outliner,
            )
            .add_systems(Update, build_outliner_tree.before(EguiChromeSystems));
    }
}

pub fn execute_toggle_outliner(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let visible = {
        let mut state = world.resource_mut::<OutlinerWindowState>();
        state.visible = !state.visible;
        state.visible
    };
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        let message = if visible {
            "Outliner opened"
        } else {
            "Outliner closed"
        };
        status.set_feedback(message.to_string(), 2.0);
    }
    Ok(CommandResult::empty())
}

/// One node of the model's aggregation tree, in nested form. Shared by the
/// Outliner panel (flattened into [`OutlinerTree`]) and the model-api
/// `outline_tree` tool (serialised to JSON) so the UI and API never drift.
#[derive(Debug, Clone)]
pub struct OutlineEntry {
    /// Stable id for UI collapse state: the `ElementId` for authored entities,
    /// or a synthetic hash for transient generated parts.
    pub node_id: u64,
    /// `ElementId` of the entity, or `None` for a transient generated part.
    pub element_id: Option<u64>,
    /// Entity to select when this row is activated (the owner, for parts).
    pub entity: Entity,
    pub label: String,
    pub kind: OutlinerKind,
    pub children: Vec<OutlineEntry>,
}

/// Walk the world and (re)build the flattened [`OutlinerTree`] used by the panel.
///
/// Runs only while the panel is visible; otherwise it just clears the arena so
/// stale entities are never rendered.
pub fn build_outliner_tree(world: &mut World) {
    let visible = world
        .get_resource::<OutlinerWindowState>()
        .map(|state| state.visible)
        .unwrap_or(false);
    if !visible {
        if let Some(mut tree) = world.get_resource_mut::<OutlinerTree>() {
            tree.clear();
        }
        return;
    }

    let forest = collect_outline_forest(world);
    let mut nodes: Vec<OutlinerNode> = Vec::new();
    let roots: Vec<usize> = forest
        .iter()
        .map(|entry| flatten_entry(entry, &mut nodes))
        .collect();
    let mut tree = world.resource_mut::<OutlinerTree>();
    tree.nodes = nodes;
    tree.roots = roots;
}

/// Flatten a nested [`OutlineEntry`] into the panel arena, returning its index.
/// Children are pushed before the parent so arena indices stay valid.
fn flatten_entry(entry: &OutlineEntry, nodes: &mut Vec<OutlinerNode>) -> usize {
    let children: Vec<usize> = entry
        .children
        .iter()
        .map(|child| flatten_entry(child, nodes))
        .collect();
    nodes.push(OutlinerNode {
        node_id: entry.node_id,
        select_entity: entry.entity,
        label: entry.label.clone(),
        kind: entry.kind,
        children,
    });
    nodes.len() - 1
}

/// Build the model's aggregation tree as a nested forest of [`OutlineEntry`]
/// roots — the single source of truth shared by the Outliner panel and the
/// model-api `outline_tree` tool.
pub fn collect_outline_forest(world: &mut World) -> Vec<OutlineEntry> {
    // --- Phase A: structural data via mutable queries ---
    let mut entity_by_eid: HashMap<u64, Entity> = HashMap::new();
    let mut all_eids: Vec<(u64, Entity)> = Vec::new();
    {
        let mut query = world.query::<(Entity, &ElementId)>();
        for (entity, element_id) in query.iter(world) {
            entity_by_eid.insert(element_id.0, entity);
            all_eids.push((element_id.0, entity));
        }
    }

    let mut group_members: HashMap<u64, (String, Vec<u64>)> = HashMap::new();
    let mut member_set: HashSet<u64> = HashSet::new();
    {
        let mut query = world.query::<(&ElementId, &GroupMembers)>();
        for (element_id, members) in query.iter(world) {
            let member_ids: Vec<u64> = members.member_ids.iter().map(|id| id.0).collect();
            for id in &member_ids {
                member_set.insert(*id);
            }
            group_members.insert(element_id.0, (members.name.clone(), member_ids));
        }
    }

    let mut occurrence_set: HashSet<u64> = HashSet::new();
    {
        let mut query = world.query_filtered::<&ElementId, With<OccurrenceIdentity>>();
        for element_id in query.iter(world) {
            occurrence_set.insert(element_id.0);
        }
    }

    // Relations are connectivity, not aggregation — keep them out of the tree.
    let mut relation_set: HashSet<u64> = HashSet::new();
    {
        let mut query = world.query_filtered::<&ElementId, With<SemanticRelation>>();
        for element_id in query.iter(world) {
            relation_set.insert(element_id.0);
        }
    }

    let mut parts_by_owner: HashMap<u64, Vec<(Entity, String)>> = HashMap::new();
    {
        let mut query = world.query::<&GeneratedOccurrencePart>();
        // Collect owner -> label only; the part's own entity isn't needed since
        // clicking a part selects the owner.
        let mut tmp: Vec<(u64, String)> = Vec::new();
        for part in query.iter(world) {
            let label = if part.slot_path.is_empty() {
                part.definition_id.to_string()
            } else {
                part.slot_path.clone()
            };
            tmp.push((part.owner.0, label));
        }
        for (owner, label) in tmp {
            if let Some(&owner_entity) = entity_by_eid.get(&owner) {
                parts_by_owner
                    .entry(owner)
                    .or_default()
                    .push((owner_entity, label));
            }
        }
    }

    // --- Phase B: display labels via the generic snapshot path ---
    let mut labels: HashMap<u64, String> = HashMap::new();
    {
        let registry = world.resource::<CapabilityRegistry>();
        for (eid, entity) in &all_eids {
            if let Ok(entity_ref) = world.get_entity(*entity) {
                let entity_ref: EntityRef = entity_ref;
                if let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) {
                    let label = snapshot.label();
                    if !label.is_empty() {
                        labels.insert(*eid, label);
                    }
                }
            }
        }
    }

    // --- Assemble the nested forest ---
    let ctx = BuildContext {
        entity_by_eid: &entity_by_eid,
        group_members: &group_members,
        occurrence_set: &occurrence_set,
        parts_by_owner: &parts_by_owner,
        labels: &labels,
    };

    let mut visited: HashSet<u64> = HashSet::new();
    let mut root_eids: Vec<u64> = all_eids
        .iter()
        .map(|(eid, _)| *eid)
        .filter(|eid| !member_set.contains(eid) && !relation_set.contains(eid))
        .collect();
    root_eids.sort_by_key(|a| ctx.label_for(*a));

    let mut roots: Vec<OutlineEntry> = Vec::new();
    for eid in root_eids {
        if visited.contains(&eid) {
            continue;
        }
        roots.push(build_entry(&ctx, eid, &mut visited));
    }
    roots
}

struct BuildContext<'a> {
    entity_by_eid: &'a HashMap<u64, Entity>,
    group_members: &'a HashMap<u64, (String, Vec<u64>)>,
    occurrence_set: &'a HashSet<u64>,
    parts_by_owner: &'a HashMap<u64, Vec<(Entity, String)>>,
    labels: &'a HashMap<u64, String>,
}

impl BuildContext<'_> {
    fn label_for(&self, eid: u64) -> String {
        if let Some(label) = self.labels.get(&eid) {
            return label.clone();
        }
        if let Some((name, _)) = self.group_members.get(&eid) {
            if !name.is_empty() {
                return name.clone();
            }
        }
        format!("#{eid}")
    }
}

/// Recursively materialise the nested [`OutlineEntry`] for `eid` and its
/// children. `visited` prevents infinite recursion on (malformed) cyclic
/// group membership and stops an element appearing under two parents.
fn build_entry(ctx: &BuildContext, eid: u64, visited: &mut HashSet<u64>) -> OutlineEntry {
    visited.insert(eid);
    let entity = ctx.entity_by_eid[&eid];

    let mut children = Vec::new();

    if let Some((_, member_ids)) = ctx.group_members.get(&eid) {
        let mut members: Vec<u64> = member_ids
            .iter()
            .copied()
            .filter(|m| ctx.entity_by_eid.contains_key(m) && !visited.contains(m))
            .collect();
        members.sort_by_key(|a| ctx.label_for(*a));
        for member in members {
            if visited.contains(&member) {
                continue;
            }
            children.push(build_entry(ctx, member, visited));
        }
    }

    if let Some(parts) = ctx.parts_by_owner.get(&eid) {
        for (owner_entity, label) in parts {
            children.push(OutlineEntry {
                node_id: part_node_id(eid, label),
                element_id: None,
                entity: *owner_entity,
                label: label.clone(),
                kind: OutlinerKind::Part,
                children: Vec::new(),
            });
        }
    }

    let kind = if ctx.group_members.contains_key(&eid) {
        OutlinerKind::Group
    } else if ctx.occurrence_set.contains(&eid) {
        OutlinerKind::Occurrence
    } else {
        OutlinerKind::Leaf
    };

    OutlineEntry {
        node_id: eid,
        element_id: Some(eid),
        entity,
        label: ctx.label_for(eid),
        kind,
        children,
    }
}

fn part_node_id(owner: u64, slot: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "generated_part".hash(&mut hasher);
    owner.hash(&mut hasher);
    slot.hash(&mut hasher);
    hasher.finish()
}

/// Stable lowercase tag for [`OutlinerKind`], used in the MCP JSON.
fn kind_str(kind: OutlinerKind) -> &'static str {
    match kind {
        OutlinerKind::Group => "group",
        OutlinerKind::Occurrence => "occurrence",
        OutlinerKind::Part => "part",
        OutlinerKind::Leaf => "leaf",
    }
}

/// Serialise the model's aggregation tree for the model-api `outline_tree`
/// tool. Shares [`collect_outline_forest`] with the panel so the UI and API
/// can never disagree about the model's structure.
pub fn outline_tree_json(world: &mut World) -> Value {
    fn entry_json(entry: &OutlineEntry) -> Value {
        serde_json::json!({
            "element_id": entry.element_id,
            "label": entry.label,
            "kind": kind_str(entry.kind),
            "children": entry.children.iter().map(entry_json).collect::<Vec<_>>(),
        })
    }
    let roots = collect_outline_forest(world);
    serde_json::json!({
        "roots": roots.iter().map(entry_json).collect::<Vec<_>>(),
    })
}

/// Render the Outliner window. Returns a pending selection action if a row was
/// clicked this frame.
pub fn draw_outliner_window(
    ctx: &egui::Context,
    state: &mut OutlinerWindowState,
    tree: &OutlinerTree,
    selected: &HashSet<Entity>,
) -> Option<OutlinerSelectAction> {
    if !state.visible {
        return None;
    }

    let mut action: Option<OutlinerSelectAction> = None;
    let mut open = state.visible;
    egui::Window::new("Outliner")
        .id(egui::Id::new("talos_outliner_window"))
        .default_width(OUTLINER_DEFAULT_WIDTH)
        .default_height(OUTLINER_DEFAULT_HEIGHT)
        .resizable(true)
        .open(&mut open)
        .show(ctx, |ui| {
            if tree.roots.is_empty() {
                ui.weak("The model is empty.");
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let roots = tree.roots.clone();
                    for root in roots {
                        render_outliner_node(ui, root, tree, state, selected, &mut action, 0);
                    }
                });
        });
    state.visible = open;
    action
}

fn render_outliner_node(
    ui: &mut egui::Ui,
    idx: usize,
    tree: &OutlinerTree,
    state: &mut OutlinerWindowState,
    selected: &HashSet<Entity>,
    action: &mut Option<OutlinerSelectAction>,
    depth: usize,
) {
    let node = &tree.nodes[idx];
    let has_children = !node.children.is_empty();
    let node_id = node.node_id;
    let expanded = has_children && !state.collapsed.contains(&node_id);

    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * OUTLINER_INDENT_PER_DEPTH);

        if has_children {
            if draw_disclosure_triangle(ui, expanded) {
                if expanded {
                    state.collapsed.insert(node_id);
                } else {
                    state.collapsed.remove(&node_id);
                }
            }
        } else {
            ui.add_space(OUTLINER_TOGGLE_WIDTH);
        }

        let is_selected = selected.contains(&node.select_entity);
        let text = format!("{} {}", kind_glyph(node.kind), node.label);
        let response = ui.selectable_label(is_selected, text);
        if response.clicked() {
            let additive = ui.input(|input| input.modifiers.command || input.modifiers.shift);
            *action = Some(OutlinerSelectAction {
                target: node.select_entity,
                additive,
            });
        }
    });

    if has_children && expanded {
        let children = node.children.clone();
        for child in children {
            render_outliner_node(ui, child, tree, state, selected, action, depth + 1);
        }
    }
}

/// Paint a small disclosure triangle as a frameless clickable widget. Painted
/// (rather than a glyph) so it never depends on font coverage. Returns whether
/// it was clicked.
fn draw_disclosure_triangle(ui: &mut egui::Ui, expanded: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(OUTLINER_TOGGLE_WIDTH, 14.0),
        egui::Sense::click(),
    );
    let color = if response.hovered() {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let center = rect.center();
    let points = if expanded {
        vec![
            center + egui::vec2(-4.0, -2.0),
            center + egui::vec2(4.0, -2.0),
            center + egui::vec2(0.0, 4.0),
        ]
    } else {
        vec![
            center + egui::vec2(-2.0, -4.0),
            center + egui::vec2(4.0, 0.0),
            center + egui::vec2(-2.0, 4.0),
        ]
    };
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
    response.clicked()
}

fn kind_glyph(kind: OutlinerKind) -> &'static str {
    match kind {
        OutlinerKind::Group => "[]",
        OutlinerKind::Occurrence => "<>",
        OutlinerKind::Part => "-",
        OutlinerKind::Leaf => "*",
    }
}

/// Apply an outliner click to the ECS selection set, mirroring viewport
/// selection semantics (exclusive by default, toggle when additive).
pub fn apply_outliner_selection(
    commands: &mut Commands,
    current: &HashSet<Entity>,
    action: OutlinerSelectAction,
) {
    let OutlinerSelectAction { target, additive } = action;
    if additive {
        if current.contains(&target) {
            commands.entity(target).remove::<Selected>();
        } else {
            commands.entity(target).insert(Selected);
        }
    } else {
        for entity in current {
            if *entity != target {
                commands.entity(*entity).remove::<Selected>();
            }
        }
        commands.entity(target).insert(Selected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::modeling::{
        assembly::SemanticRelation,
        definition::DefinitionId,
        occurrence::{GeneratedOccurrencePart, OccurrenceIdentity},
    };
    use serde_json::json;

    fn node_for(tree: &OutlinerTree, eid: u64) -> Option<&OutlinerNode> {
        tree.nodes.iter().find(|node| node.node_id == eid)
    }

    fn build(world: &mut World) -> OutlinerTree {
        world.insert_resource(OutlinerWindowState {
            visible: true,
            ..Default::default()
        });
        world.init_resource::<OutlinerTree>();
        world.init_resource::<CapabilityRegistry>();
        build_outliner_tree(world);
        world.resource::<OutlinerTree>().clone()
    }

    #[test]
    fn groups_aggregate_members_and_relations_are_excluded() {
        let mut world = World::new();
        // Group #1 aggregates #2 and #3.
        world.spawn((
            ElementId(1),
            GroupMembers {
                name: "Assembly".to_string(),
                member_ids: vec![ElementId(2), ElementId(3)],
            },
        ));
        world.spawn(ElementId(2));
        world.spawn(ElementId(3));
        // Standalone root.
        world.spawn(ElementId(4));
        // A relation must never appear as an aggregation node.
        world.spawn((
            ElementId(5),
            SemanticRelation {
                source: ElementId(4),
                target: ElementId(1),
                relation_type: "hosted_on".to_string(),
                parameters: json!({}),
            },
        ));

        let tree = build(&mut world);

        let root_ids: Vec<u64> = tree.roots.iter().map(|i| tree.nodes[*i].node_id).collect();
        assert!(root_ids.contains(&1), "group should be a root");
        assert!(root_ids.contains(&4), "standalone element should be a root");
        assert!(!root_ids.contains(&2), "member must not be a root");
        assert!(!root_ids.contains(&3), "member must not be a root");
        assert!(!root_ids.contains(&5), "relation must be excluded");

        let group = node_for(&tree, 1).expect("group node");
        assert_eq!(group.kind, OutlinerKind::Group);
        assert_eq!(group.label, "Assembly");
        let child_ids: Vec<u64> = group
            .children
            .iter()
            .map(|i| tree.nodes[*i].node_id)
            .collect();
        assert_eq!(child_ids, vec![2, 3]);
    }

    #[test]
    fn occurrences_aggregate_generated_parts() {
        let mut world = World::new();
        let occ = world
            .spawn((
                ElementId(10),
                OccurrenceIdentity::new(DefinitionId("window".to_string()), 1),
            ))
            .id();
        world.spawn(GeneratedOccurrencePart {
            owner: ElementId(10),
            slot_path: "glazing".to_string(),
            definition_id: DefinitionId("pane".to_string()),
        });

        let tree = build(&mut world);

        let occurrence = node_for(&tree, 10).expect("occurrence node");
        assert_eq!(occurrence.kind, OutlinerKind::Occurrence);
        assert_eq!(occurrence.children.len(), 1, "one generated part");
        let part = &tree.nodes[occurrence.children[0]];
        assert_eq!(part.kind, OutlinerKind::Part);
        assert_eq!(part.label, "glazing");
        // Clicking a generated part selects the owning occurrence.
        assert_eq!(part.select_entity, occ);
    }

    #[test]
    fn hidden_panel_yields_empty_tree() {
        let mut world = World::new();
        world.spawn(ElementId(1));
        world.insert_resource(OutlinerWindowState::default());
        world.init_resource::<OutlinerTree>();
        world.init_resource::<CapabilityRegistry>();
        build_outliner_tree(&mut world);
        assert!(world.resource::<OutlinerTree>().roots.is_empty());
    }

    #[test]
    fn toggle_command_flips_visibility() {
        let mut world = World::new();
        world.init_resource::<OutlinerWindowState>();
        execute_toggle_outliner(&mut world, &Value::Null).unwrap();
        assert!(world.resource::<OutlinerWindowState>().visible);
        execute_toggle_outliner(&mut world, &Value::Null).unwrap();
        assert!(!world.resource::<OutlinerWindowState>().visible);
    }

    #[test]
    fn outline_tree_json_nests_group_members() {
        let mut world = World::new();
        world.init_resource::<CapabilityRegistry>();
        world.spawn((
            ElementId(1),
            GroupMembers {
                name: "Assembly".to_string(),
                member_ids: vec![ElementId(2), ElementId(3)],
            },
        ));
        world.spawn(ElementId(2));
        world.spawn(ElementId(3));

        let json = outline_tree_json(&mut world);
        let roots = json["roots"].as_array().expect("roots array");
        assert_eq!(roots.len(), 1, "group is the only root");
        let group = &roots[0];
        assert_eq!(group["element_id"], json!(1));
        assert_eq!(group["kind"], json!("group"));
        assert_eq!(group["label"], json!("Assembly"));
        let children = group["children"].as_array().expect("children array");
        let child_ids: Vec<u64> = children
            .iter()
            .map(|c| c["element_id"].as_u64().unwrap())
            .collect();
        assert_eq!(child_ids, vec![2, 3]);
        assert!(children.iter().all(|c| c["kind"] == json!("leaf")));
    }
}
