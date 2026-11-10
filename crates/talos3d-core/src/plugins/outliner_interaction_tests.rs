use super::*;

fn fixture() -> (World, OutlinerTree) {
    let mut world = World::new();
    for (id, name, members) in [
        (100, "Parent", vec![1, 2, 3, 4]),
        (1, "A", vec![]),
        (2, "B", vec![20]),
        (3, "C", vec![]),
        (4, "D", vec![]),
        (20, "Nested", vec![]),
        (200, "Other", vec![5]),
        (5, "E", vec![]),
    ] {
        world.spawn((
            ElementId(id),
            GroupMembers {
                name: name.into(),
                member_ids: members.into_iter().map(ElementId).collect(),
                frame: default(),
                linked_model: None,
            },
        ));
    }
    let tree = build(&mut world);
    (world, tree)
}

fn index(tree: &OutlinerTree, id: u64) -> usize {
    tree.nodes
        .iter()
        .position(|node| node.node_id == id)
        .unwrap()
}

fn apply(world: &mut World, action: OutlinerSelectAction) -> HashSet<Entity> {
    let selected = world
        .query_filtered::<Entity, With<Selected>>()
        .iter(world)
        .collect();
    let mut queue = bevy::ecs::world::CommandQueue::default();
    apply_outliner_selection(&mut Commands::new(&mut queue, world), &selected, action);
    queue.apply(world);
    world
        .query_filtered::<Entity, With<Selected>>()
        .iter(world)
        .collect()
}

#[test]
fn outliner_range_stays_with_siblings_skips_locks_and_keeps_anchor() {
    let (mut world, mut tree) = fixture();
    let locked = index(&tree, 3);
    tree.nodes[locked].select_entity = None;
    let mut state = OutlinerWindowState::default();
    let first =
        row_selection_action(&tree, &mut state, index(&tree, 1), egui::Modifiers::NONE).unwrap();
    apply(&mut world, first);
    let range =
        row_selection_action(&tree, &mut state, index(&tree, 4), egui::Modifiers::SHIFT).unwrap();
    let selected = apply(&mut world, range);
    assert_eq!(selected.len(), 3);
    for id in [1, 2, 4] {
        assert!(selected.contains(&node_for(&tree, id).unwrap().select_entity.unwrap()));
    }
    assert!(!selected.contains(&node_for(&tree, 20).unwrap().select_entity.unwrap()));
    assert!(can_group_selection(&tree, &selected));
    let range =
        row_selection_action(&tree, &mut state, index(&tree, 2), egui::Modifiers::SHIFT).unwrap();
    assert_eq!(
        apply(&mut world, range).len(),
        2,
        "shortening retains the original anchor"
    );
    let other =
        row_selection_action(&tree, &mut state, index(&tree, 5), egui::Modifiers::SHIFT).unwrap();
    assert_eq!(
        apply(&mut world, other),
        HashSet::from([node_for(&tree, 5).unwrap().select_entity.unwrap()])
    );
    assert_eq!(state.selection_anchor, Some(5));
}

#[test]
fn outliner_reverse_and_additive_ranges_do_not_toggle_existing_members() {
    let (mut world, tree) = fixture();
    let mut state = OutlinerWindowState::default();
    let click =
        row_selection_action(&tree, &mut state, index(&tree, 4), egui::Modifiers::NONE).unwrap();
    apply(&mut world, click);
    let range = row_selection_action(
        &tree,
        &mut state,
        index(&tree, 1),
        egui::Modifiers {
            shift: true,
            command: true,
            ..default()
        },
    )
    .unwrap();
    let selected = apply(&mut world, range);
    assert_eq!(selected.len(), 4);
    assert!(can_group_selection(&tree, &selected));
    let click = row_selection_action(
        &tree,
        &mut state,
        index(&tree, 5),
        egui::Modifiers {
            command: true,
            ..default()
        },
    )
    .unwrap();
    let selected = apply(&mut world, click);
    assert_eq!(selected.len(), 5);
    assert!(
        !can_group_selection(&tree, &selected),
        "grouping cannot reparent unrelated branches"
    );
}

struct UiHarness {
    world: World,
    tree: OutlinerTree,
    state: OutlinerWindowState,
    ctx: egui::Context,
    selected: HashSet<Entity>,
    pending: PendingCommandInvocations,
    text: HashMap<String, egui::Pos2>,
}

impl UiHarness {
    fn new() -> Self {
        let (world, tree) = fixture();
        let mut harness = Self {
            world,
            tree,
            state: OutlinerWindowState {
                visible: true,
                ..default()
            },
            ctx: default(),
            selected: default(),
            pending: default(),
            text: default(),
        };
        harness.frame(vec![], egui::Modifiers::NONE);
        harness.frame(vec![], egui::Modifiers::NONE);
        harness
    }

    fn frame(&mut self, mut events: Vec<egui::Event>, modifiers: egui::Modifiers) {
        events.insert(0, egui::Event::ModifiersChanged(modifiers));
        let mut action = None;
        let output = self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000., 800.),
                )),
                events,
                ..default()
            },
            |ui| {
                action = draw_outliner_window(
                    ui.ctx(),
                    &mut self.state,
                    &self.tree,
                    &self.selected,
                    &mut self.pending,
                    &CommandRegistry::default(),
                );
            },
        );
        self.text.clear();
        for shape in &output.shapes {
            if let egui::epaint::Shape::Text(text) = &shape.shape {
                self.text.insert(
                    text.galley.job.text.clone(),
                    text.visual_bounding_rect().center(),
                );
            }
        }
        output.drop_without_applying_deltas();
        if let Some(action) = action {
            self.selected = apply(&mut self.world, action);
        }
    }

    fn click(&mut self, label: &str, button: egui::PointerButton, modifiers: egui::Modifiers) {
        let pos = *self
            .text
            .get(label)
            .unwrap_or_else(|| panic!("missing {label}: {:?}", self.text.keys()));
        self.frame(
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed: true,
                    modifiers,
                },
            ],
            modifiers,
        );
        self.frame(
            vec![egui::Event::PointerButton {
                pos,
                button,
                pressed: false,
                modifiers,
            }],
            modifiers,
        );
        self.frame(vec![], modifiers);
    }
}

#[test]
fn outliner_native_egui_shift_range_context_group_preserves_selection() {
    let mut ui = UiHarness::new();
    ui.click("▣ A", egui::PointerButton::Primary, egui::Modifiers::NONE);
    ui.click("▣ D", egui::PointerButton::Primary, egui::Modifiers::SHIFT);
    assert_eq!(ui.selected.len(), 4);
    ui.click("▣ B", egui::PointerButton::Secondary, egui::Modifiers::NONE);
    assert_eq!(
        ui.selected.len(),
        4,
        "context click must preserve the range"
    );
    ui.click(
        "Group Selected",
        egui::PointerButton::Primary,
        egui::Modifiers::NONE,
    );
    assert!(!ui.pending.is_empty());
    assert_eq!(ui.selected.len(), 4);
}

#[test]
fn outliner_native_egui_context_click_selects_new_target_and_locks_it() {
    let mut ui = UiHarness::new();
    ui.click("▣ A", egui::PointerButton::Primary, egui::Modifiers::NONE);
    ui.click("▣ D", egui::PointerButton::Secondary, egui::Modifiers::NONE);
    assert_eq!(
        ui.selected,
        HashSet::from([node_for(&ui.tree, 4).unwrap().select_entity.unwrap()])
    );
    ui.click(
        "Lock Selected",
        egui::PointerButton::Primary,
        egui::Modifiers::NONE,
    );
    assert!(!ui.pending.is_empty());
}
