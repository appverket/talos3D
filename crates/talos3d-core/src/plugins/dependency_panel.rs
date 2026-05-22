//! Dependency-graph panel — a read-only inspector of the model's change
//! propagation graph.
//!
//! The panel is a *view* of the live dependency graph (ADR-007): the directed
//! edges between authored entities along which a change propagates. Each frame,
//! while the panel is open, [`build_dependency_panel_data`] takes a fresh
//! snapshot of the graph (via
//! [`dependency_graph::build_graph_snapshot`](crate::plugins::modeling::dependency_graph::build_graph_snapshot))
//! into the [`DependencyPanelData`] arena. The egui renderer
//! ([`draw_dependency_window`]) — called from `egui_chrome` — reads that arena
//! and turns clicks into selection changes.
//!
//! Edges are *derived* (from `SemanticRelation`s that participate in the
//! dependency graph, plus factory-declared parametric edges), so the panel is
//! deliberately read-only: it shows what depends on what and what a change
//! would propagate to, but never lets the user hand-edit edges that the engine
//! would just recompute. The same snapshot backs the model-api
//! `dependency_graph` / `entity_dependencies` tools so the UI and API agree.

use std::collections::{HashMap, HashSet, VecDeque};

use bevy::prelude::*;
use bevy_egui::egui;
use serde_json::Value;

use crate::plugins::{
    command_registry::{CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult},
    egui_chrome::EguiChromeSystems,
    entity_labels::collect_entity_labels,
    identity::ElementId,
    modeling::dependency_graph::build_graph_snapshot,
    selection::apply_click_selection,
    ui::StatusBarData,
};

const PANEL_DEFAULT_WIDTH: f32 = 300.0;
const PANEL_DEFAULT_HEIGHT: f32 = 440.0;

#[derive(Resource, Debug, Clone, Default)]
pub struct DependencyPanelState {
    pub visible: bool,
    /// `element_id`s whose transitive "propagates to" list is collapsed.
    pub collapsed: HashSet<u64>,
    /// Whether the fallback full-graph list (shown when nothing is selected) is
    /// collapsed.
    pub all_collapsed: bool,
}

/// One direct dependency edge out of an entity, resolved for display.
#[derive(Debug, Clone)]
pub struct DependencyParent {
    pub element_id: u64,
    pub role: String,
}

/// Flattened, owned snapshot of the dependency graph rebuilt each frame while
/// the panel is visible. Entities are keyed by `ElementId.0`.
#[derive(Resource, Debug, Clone, Default)]
pub struct DependencyPanelData {
    pub entity_by_eid: HashMap<u64, Entity>,
    pub label_by_eid: HashMap<u64, String>,
    /// eid → the entities it depends on (with edge roles).
    pub parents: HashMap<u64, Vec<DependencyParent>>,
    /// eid → the entities that directly depend on it.
    pub children: HashMap<u64, Vec<u64>>,
    /// All graph nodes, sorted by id.
    pub nodes: Vec<u64>,
    pub has_cycle: bool,
    pub node_count: usize,
    pub edge_count: usize,
}

impl DependencyPanelData {
    fn clear(&mut self) {
        self.entity_by_eid.clear();
        self.label_by_eid.clear();
        self.parents.clear();
        self.children.clear();
        self.nodes.clear();
        self.has_cycle = false;
        self.node_count = 0;
        self.edge_count = 0;
    }

    fn label_for(&self, eid: u64) -> String {
        self.label_by_eid
            .get(&eid)
            .cloned()
            .unwrap_or_else(|| format!("#{eid}"))
    }

    /// All transitive dependents of `eid` (everything a change to it reaches),
    /// in BFS order, bounded by the node count to defend against malformed
    /// cyclic graphs.
    fn propagation_of(&self, eid: u64) -> Vec<u64> {
        let mut out = Vec::new();
        let mut seen: HashSet<u64> = HashSet::new();
        seen.insert(eid);
        let bound = self.node_count.saturating_add(1);
        let mut frontier: VecDeque<(u64, usize)> = VecDeque::from([(eid, 0usize)]);
        while let Some((node, depth)) = frontier.pop_front() {
            if depth >= bound {
                continue;
            }
            if let Some(children) = self.children.get(&node) {
                for child in children {
                    if seen.insert(*child) {
                        out.push(*child);
                        frontier.push_back((*child, depth + 1));
                    }
                }
            }
        }
        out
    }
}

/// A pending selection change produced by a click on a dependency row.
#[derive(Debug, Clone, Copy)]
pub struct DependencySelectAction {
    pub target: Entity,
    pub additive: bool,
}

pub struct DependencyPanelPlugin;

impl Plugin for DependencyPanelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DependencyPanelState>()
            .init_resource::<DependencyPanelData>()
            .register_command(
                CommandDescriptor {
                    id: "view.toggle_dependency_graph".to_string(),
                    label: "Toggle Dependency Graph".to_string(),
                    description: "Show or hide the dependency graph inspector.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: Some("Ctrl/Cmd+Shift+Y".to_string()),
                    icon: None,
                    hint: Some(
                        "Inspect what depends on what and how changes propagate".to_string(),
                    ),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_toggle_dependency_graph,
            )
            .add_systems(Update, build_dependency_panel_data.before(EguiChromeSystems));
    }
}

pub fn execute_toggle_dependency_graph(
    world: &mut World,
    _: &Value,
) -> Result<CommandResult, String> {
    let visible = {
        let mut state = world.resource_mut::<DependencyPanelState>();
        state.visible = !state.visible;
        state.visible
    };
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        let message = if visible {
            "Dependency graph opened"
        } else {
            "Dependency graph closed"
        };
        status.set_feedback(message.to_string(), 2.0);
    }
    Ok(CommandResult::empty())
}

/// Rebuild [`DependencyPanelData`] from a fresh graph snapshot while the panel
/// is visible; otherwise clear it so stale entities are never rendered.
pub fn build_dependency_panel_data(world: &mut World) {
    let visible = world
        .get_resource::<DependencyPanelState>()
        .map(|state| state.visible)
        .unwrap_or(false);
    if !visible {
        if let Some(mut data) = world.get_resource_mut::<DependencyPanelData>() {
            if !data.nodes.is_empty() || !data.entity_by_eid.is_empty() {
                data.clear();
            }
        }
        return;
    }

    let labels = collect_entity_labels(world);
    let graph = build_graph_snapshot(world);

    let entity_by_eid: HashMap<u64, Entity> = {
        let mut query = world.query::<(Entity, &ElementId)>();
        query
            .iter(world)
            .map(|(entity, element_id)| (element_id.0, entity))
            .collect()
    };

    let nodes = graph.nodes();
    let mut parents: HashMap<u64, Vec<DependencyParent>> = HashMap::new();
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for node in &nodes {
        let ps: Vec<DependencyParent> = graph
            .parents_of(*node)
            .iter()
            .map(|edge| DependencyParent {
                element_id: edge.on.0,
                role: edge.role.as_str().to_string(),
            })
            .collect();
        if !ps.is_empty() {
            parents.insert(node.0, ps);
        }
        let cs: Vec<u64> = graph.children_of(*node).iter().map(|c| c.0).collect();
        if !cs.is_empty() {
            children.insert(node.0, cs);
        }
    }
    let has_cycle = graph.topological_order().is_err();
    let edge_count = graph.edges().len();
    let node_ids: Vec<u64> = nodes.iter().map(|n| n.0).collect();

    let mut data = world.resource_mut::<DependencyPanelData>();
    data.label_by_eid = labels;
    data.entity_by_eid = entity_by_eid;
    data.parents = parents;
    data.children = children;
    data.node_count = node_ids.len();
    data.edge_count = edge_count;
    data.has_cycle = has_cycle;
    data.nodes = node_ids;
}

/// Render the Dependency Graph window. Returns a pending selection action if a
/// row was clicked this frame.
pub fn draw_dependency_window(
    ctx: &egui::Context,
    state: &mut DependencyPanelState,
    data: &DependencyPanelData,
    selected: &HashSet<Entity>,
) -> Option<DependencySelectAction> {
    if !state.visible {
        return None;
    }

    let mut action: Option<DependencySelectAction> = None;
    let mut open = state.visible;
    egui::Window::new("Dependency Graph")
        .id(egui::Id::new("talos_dependency_window"))
        .default_width(PANEL_DEFAULT_WIDTH)
        .default_height(PANEL_DEFAULT_HEIGHT)
        .resizable(true)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.weak(format!(
                "{} nodes · {} edges",
                data.node_count, data.edge_count
            ));
            if data.has_cycle {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 120, 60),
                    "⚠ Dependency cycle detected — propagation order is undefined.",
                );
            }
            ui.separator();

            // Which selected entities are addressable in the graph view?
            let eid_by_entity: HashMap<Entity, u64> =
                data.entity_by_eid.iter().map(|(k, v)| (*v, *k)).collect();
            let mut focus: Vec<u64> = selected
                .iter()
                .filter_map(|entity| eid_by_entity.get(entity).copied())
                .collect();
            focus.sort_unstable();
            focus.dedup();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if focus.is_empty() {
                        render_no_selection(ui, state, data, selected, &mut action);
                    } else {
                        for (i, eid) in focus.iter().enumerate() {
                            if i > 0 {
                                ui.separator();
                            }
                            render_focus(ui, *eid, state, data, selected, &mut action);
                        }
                    }
                });
        });
    state.visible = open;
    action
}

fn render_no_selection(
    ui: &mut egui::Ui,
    state: &mut DependencyPanelState,
    data: &DependencyPanelData,
    selected: &HashSet<Entity>,
    action: &mut Option<DependencySelectAction>,
) {
    ui.weak("Select an element to inspect what it depends on and what a change to it propagates to.");
    let with_edges: Vec<u64> = data
        .nodes
        .iter()
        .copied()
        .filter(|eid| data.parents.contains_key(eid) || data.children.contains_key(eid))
        .collect();
    if with_edges.is_empty() {
        return;
    }
    ui.add_space(4.0);
    let header = format!("All connected elements ({})", with_edges.len());
    if collapsing_header(ui, &header, !state.all_collapsed) {
        state.all_collapsed = !state.all_collapsed;
    }
    if !state.all_collapsed {
        for eid in with_edges {
            let depends = data.parents.get(&eid).map(Vec::len).unwrap_or(0);
            let used_by = data.children.get(&eid).map(Vec::len).unwrap_or(0);
            let label = format!("{}  ({}↑ {}↓)", data.label_for(eid), depends, used_by);
            row_select(ui, 1, eid, &label, data, selected, action);
        }
    }
}

fn render_focus(
    ui: &mut egui::Ui,
    eid: u64,
    state: &mut DependencyPanelState,
    data: &DependencyPanelData,
    selected: &HashSet<Entity>,
    action: &mut Option<DependencySelectAction>,
) {
    ui.strong(data.label_for(eid));

    // Depends on (direct inputs).
    ui.label(egui::RichText::new("Depends on").weak());
    match data.parents.get(&eid) {
        Some(parents) if !parents.is_empty() => {
            for parent in parents {
                let label = format!("↑ {}  ({})", data.label_for(parent.element_id), parent.role);
                row_select(ui, 1, parent.element_id, &label, data, selected, action);
            }
        }
        _ => {
            ui.indent("deps_none", |ui| ui.weak("nothing"));
        }
    }

    // Used by (direct dependents).
    ui.label(egui::RichText::new("Used by").weak());
    match data.children.get(&eid) {
        Some(children) if !children.is_empty() => {
            for child in children {
                let label = format!("↓ {}", data.label_for(*child));
                row_select(ui, 1, *child, &label, data, selected, action);
            }
        }
        _ => {
            ui.indent("dependents_none", |ui| ui.weak("nothing"));
        }
    }

    // Propagates to (transitive dependents).
    let propagation = data.propagation_of(eid);
    let expanded = !state.collapsed.contains(&eid);
    let header = format!("Propagates to ({})", propagation.len());
    if collapsing_header(ui, &header, expanded && !propagation.is_empty()) {
        if expanded {
            state.collapsed.insert(eid);
        } else {
            state.collapsed.remove(&eid);
        }
    }
    if expanded && !propagation.is_empty() {
        for node in propagation {
            let label = format!("• {}", data.label_for(node));
            row_select(ui, 1, node, &label, data, selected, action);
        }
    }
}

/// Render a clickable, indented row that selects the entity for `eid` (if it
/// resolves to a live entity). Dangling ids render as weak, non-interactive text.
fn row_select(
    ui: &mut egui::Ui,
    depth: usize,
    eid: u64,
    text: &str,
    data: &DependencyPanelData,
    selected: &HashSet<Entity>,
    action: &mut Option<DependencySelectAction>,
) {
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 14.0);
        let Some(entity) = data.entity_by_eid.get(&eid).copied() else {
            ui.weak(text);
            return;
        };
        let is_selected = selected.contains(&entity);
        if ui.selectable_label(is_selected, text).clicked() {
            let additive = ui.input(|input| input.modifiers.command || input.modifiers.shift);
            *action = Some(DependencySelectAction {
                target: entity,
                additive,
            });
        }
    });
}

/// A minimal frameless collapsing header: a triangle + label that toggles when
/// clicked. Returns whether it was clicked this frame. `expanded` controls only
/// the triangle direction.
fn collapsing_header(ui: &mut egui::Ui, label: &str, expanded: bool) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(16.0, 14.0), egui::Sense::click());
        let color = if response.hovered() {
            ui.visuals().strong_text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        let c = rect.center();
        let points = if expanded {
            vec![
                c + egui::vec2(-4.0, -2.0),
                c + egui::vec2(4.0, -2.0),
                c + egui::vec2(0.0, 4.0),
            ]
        } else {
            vec![
                c + egui::vec2(-2.0, -4.0),
                c + egui::vec2(4.0, 0.0),
                c + egui::vec2(-2.0, 4.0),
            ]
        };
        ui.painter()
            .add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
        if response.clicked() || ui.label(egui::RichText::new(label).weak()).clicked() {
            clicked = true;
        }
    });
    clicked
}

/// Apply a dependency-panel click to the ECS selection set, mirroring viewport
/// selection semantics (exclusive by default, toggle when additive).
pub fn apply_dependency_selection(
    commands: &mut Commands,
    current: &HashSet<Entity>,
    action: DependencySelectAction,
) {
    apply_click_selection(commands, current, action.target, action.additive);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::CapabilityRegistry;
    use crate::plugins::modeling::dependency_graph::EntityDependencies;

    fn build(world: &mut World) -> DependencyPanelData {
        world.insert_resource(DependencyPanelState {
            visible: true,
            ..Default::default()
        });
        world.init_resource::<DependencyPanelData>();
        world.init_resource::<CapabilityRegistry>();
        build_dependency_panel_data(world);
        world.resource::<DependencyPanelData>().clone()
    }

    #[test]
    fn hidden_panel_yields_empty_data() {
        let mut world = World::new();
        world.spawn((
            ElementId(1),
            EntityDependencies::empty().with_edge(ElementId(2), "p"),
        ));
        world.init_resource::<DependencyPanelState>();
        world.init_resource::<DependencyPanelData>();
        world.init_resource::<CapabilityRegistry>();
        build_dependency_panel_data(&mut world);
        assert!(world.resource::<DependencyPanelData>().nodes.is_empty());
    }

    #[test]
    fn build_collects_parents_children_and_entities() {
        let mut world = World::new();
        // 1 depends on 2, 2 depends on 3.
        world.spawn((
            ElementId(1),
            EntityDependencies::empty().with_edge(ElementId(2), "parametric"),
        ));
        world.spawn((
            ElementId(2),
            EntityDependencies::empty().with_edge(ElementId(3), "host"),
        ));
        world.spawn((ElementId(3), EntityDependencies::empty()));

        let data = build(&mut world);
        assert_eq!(data.node_count, 3);
        assert_eq!(data.edge_count, 2);
        assert!(!data.has_cycle);
        assert_eq!(data.parents.get(&1).unwrap()[0].element_id, 2);
        assert_eq!(data.parents.get(&1).unwrap()[0].role, "parametric");
        assert_eq!(data.children.get(&3).unwrap(), &vec![2]);
        // Every node resolves to a live entity.
        assert_eq!(data.entity_by_eid.len(), 3);
    }

    #[test]
    fn propagation_follows_transitive_dependents() {
        let mut world = World::new();
        world.spawn((
            ElementId(1),
            EntityDependencies::empty().with_edge(ElementId(2), "p"),
        ));
        world.spawn((
            ElementId(2),
            EntityDependencies::empty().with_edge(ElementId(3), "p"),
        ));
        world.spawn((ElementId(3), EntityDependencies::empty()));

        let data = build(&mut world);
        // A change to 3 propagates to 2 then 1.
        assert_eq!(data.propagation_of(3), vec![2, 1]);
        // A change to 1 propagates to nothing (it is a leaf dependent).
        assert!(data.propagation_of(1).is_empty());
    }

    #[test]
    fn toggle_command_flips_visibility() {
        let mut world = World::new();
        world.init_resource::<DependencyPanelState>();
        execute_toggle_dependency_graph(&mut world, &Value::Null).unwrap();
        assert!(world.resource::<DependencyPanelState>().visible);
        execute_toggle_dependency_graph(&mut world, &Value::Null).unwrap();
        assert!(!world.resource::<DependencyPanelState>().visible);
    }
}
