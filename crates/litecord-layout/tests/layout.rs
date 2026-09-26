#![allow(clippy::unwrap_used, clippy::expect_used)]

use litecord_layout::registry;
use litecord_layout::{
    default_shell, Axis, Destination, LayoutEditSession, LayoutNode, LayoutProfile, LayoutProfiles,
    Orientation, Placement, FORMAT_VERSION, MAX_DEPTH, MAX_NODES,
};

fn panel(id: &str, panel: &str, placement: Placement) -> LayoutNode {
    LayoutNode::panel(id, panel, placement)
}

fn docking_fixture() -> LayoutNode {
    LayoutNode::split(
        "root",
        Axis::Horizontal,
        vec![
            (2.0, panel("source_node", "source_panel", Placement::Left)),
            (3.0, panel("target_node", "target_panel", Placement::Center)),
            (5.0, panel("other_node", "other_panel", Placement::Right)),
        ],
    )
}

fn two_panel_split() -> LayoutNode {
    LayoutNode::split(
        "root",
        Axis::Horizontal,
        vec![
            (0.5, panel("left_node", "left_panel", Placement::Left)),
            (0.5, panel("right_node", "right_panel", Placement::Right)),
        ],
    )
}

fn set_panel_visibility(node: &mut LayoutNode, panel_id: &str, visible: bool) -> bool {
    match node {
        LayoutNode::Panel {
            panel,
            visible: slot,
            ..
        } if panel == panel_id => {
            *slot = visible;
            true
        }
        LayoutNode::Split { children, .. } => children
            .iter_mut()
            .any(|child| set_panel_visibility(&mut child.node, panel_id, visible)),
        LayoutNode::Panel { .. } => false,
    }
}

fn panel_is_visible(node: &LayoutNode, panel_id: &str) -> bool {
    match node {
        LayoutNode::Panel { panel, visible, .. } => panel == panel_id && *visible,
        LayoutNode::Split { children, .. } => children
            .iter()
            .any(|child| panel_is_visible(&child.node, panel_id)),
    }
}

fn deep_tree(depth: usize, stop_at: usize) -> LayoutNode {
    if depth == stop_at {
        return panel(
            &format!("deep_node_{depth}"),
            &format!("deep_panel_{depth}"),
            Placement::Center,
        );
    }

    LayoutNode::split(
        &format!("split_{depth}"),
        Axis::Vertical,
        vec![
            (
                1.0,
                panel(
                    &format!("shallow_node_{depth}"),
                    &format!("shallow_panel_{depth}"),
                    Placement::Center,
                ),
            ),
            (1.0, deep_tree(depth + 1, stop_at)),
        ],
    )
}

fn wide_tree(total_nodes: usize) -> LayoutNode {
    let children = (0..total_nodes - 1)
        .map(|index| {
            (
                1.0,
                panel(
                    &format!("node_{index}"),
                    &format!("panel_{index}"),
                    Placement::Center,
                ),
            )
        })
        .collect();
    LayoutNode::split("wide_root", Axis::Horizontal, children)
}

#[test]
fn default_document_roundtrips_through_serde_and_validated_loader() {
    let default = LayoutProfiles::default();
    default.validate().unwrap();

    let json = serde_json::to_vec(&default).unwrap();
    let decoded: LayoutProfiles = serde_json::from_slice(&json).unwrap();
    assert_eq!(decoded, default);
    let loaded = LayoutProfiles::from_json(&serde_json::to_value(&default).unwrap()).unwrap();
    assert_eq!(loaded, default);
}

#[test]
fn every_docking_direction_moves_one_leaf_and_preserves_existing_node_ids() {
    for (placement, axis, moved_first) in [
        (Placement::Left, Axis::Horizontal, true),
        (Placement::Right, Axis::Horizontal, false),
        (Placement::Top, Axis::Vertical, true),
        (Placement::Bottom, Axis::Vertical, false),
    ] {
        let mut tree = docking_fixture();
        tree.dock("source_node", "target_node", placement).unwrap();
        tree.validate().unwrap();

        for id in ["root", "source_node", "target_node", "other_node"] {
            assert!(tree.find(id).is_some(), "lost existing node id {id}");
        }
        assert_eq!(
            tree.panel_ids()
                .iter()
                .filter(|panel| **panel == "source_panel")
                .count(),
            1,
            "moved panel must occur exactly once"
        );

        let LayoutNode::Panel {
            placement: actual_placement,
            ..
        } = tree.find("source_node").unwrap()
        else {
            panic!("source node changed kind")
        };
        assert_eq!(*actual_placement, placement);

        let LayoutNode::Split {
            axis: actual_axis,
            children,
            ..
        } = tree.find("dock_0").unwrap()
        else {
            panic!("target should be wrapped in a docking split")
        };
        assert_eq!(*actual_axis, axis);
        assert_eq!(children.len(), 2);
        let first_panels = children[0].node.panel_ids();
        let second_panels = children[1].node.panel_ids();
        assert_eq!(first_panels.contains(&"source_panel"), moved_first);
        assert_eq!(first_panels.contains(&"target_panel"), !moved_first);
        assert_eq!(second_panels.contains(&"source_panel"), !moved_first);
        assert_eq!(second_panels.contains(&"target_panel"), moved_first);
    }
}

#[test]
fn moving_a_nested_leaf_normalizes_its_single_child_split() {
    let mut tree = LayoutNode::split(
        "root",
        Axis::Horizontal,
        vec![
            (
                2.0,
                LayoutNode::split(
                    "nested_split",
                    Axis::Vertical,
                    vec![
                        (1.0, panel("source_node", "source_panel", Placement::Top)),
                        (1.0, panel("target_node", "target_panel", Placement::Bottom)),
                    ],
                ),
            ),
            (3.0, panel("other_node", "other_panel", Placement::Right)),
        ],
    );

    tree.dock("source_node", "target_node", Placement::Bottom)
        .unwrap();

    tree.validate().unwrap();
    assert!(tree.find("nested_split").is_none());
    assert!(tree.find("root").is_some());
    for id in ["source_node", "target_node", "other_node"] {
        assert!(tree.find(id).is_some(), "lost leaf id {id}");
    }
    assert_eq!(
        tree.panel_ids()
            .iter()
            .filter(|panel| **panel == "source_panel")
            .count(),
        1
    );
}

#[test]
fn invalid_docks_leave_the_original_tree_unchanged() {
    let mut tree = docking_fixture();
    let original = tree.clone();

    for (source, target, placement) in [
        ("missing_source", "target_node", Placement::Left),
        ("source_node", "missing_target", Placement::Right),
        ("source_node", "target_node", Placement::Center),
    ] {
        assert!(tree.dock(source, target, placement).is_err());
        assert_eq!(tree, original, "failed dock partially changed the tree");
    }
}

#[test]
fn docking_rejects_self_moves_and_group_sources_without_mutating() {
    let mut tree = docking_fixture();
    let original = tree.clone();

    assert!(tree
        .dock("target_node", "target_node", Placement::Left)
        .is_err());
    assert_eq!(tree, original);

    assert!(tree.dock("root", "target_node", Placement::Left).is_err());
    assert_eq!(tree, original);
}

#[test]
fn invalid_resizes_roll_back_and_valid_resizes_preserve_the_split() {
    let mut tree = two_panel_split();
    let original = tree.clone();

    for (split_id, weights) in [
        ("root", &[0.25][..]),
        ("root", &[0.25, 0.0][..]),
        ("missing_split", &[0.25, 0.75][..]),
    ] {
        assert!(tree.resize(split_id, weights).is_err());
        assert_eq!(tree, original, "failed resize partially changed the tree");
    }

    assert!(tree.resize("root", &[0.25, 0.75]).is_ok());
    let LayoutNode::Split { children, .. } = tree.find("root").unwrap() else {
        panic!("root should remain a split")
    };
    assert_eq!(children[0].weight, 0.25);
    assert_eq!(children[1].weight, 0.75);
    assert!(tree.find("left_node").is_some());
    assert!(tree.find("right_node").is_some());
}

#[test]
fn reorder_moves_nodes_with_their_weights_and_invalid_reorder_is_transactional() {
    let mut tree = LayoutNode::split(
        "root",
        Axis::Horizontal,
        vec![
            (2.0, panel("a", "panel_a", Placement::Left)),
            (3.0, panel("b", "panel_b", Placement::Center)),
            (5.0, panel("c", "panel_c", Placement::Right)),
        ],
    );

    tree.reorder("root", 0, 2).unwrap();
    let LayoutNode::Split { children, .. } = tree.find("root").unwrap() else {
        panic!("root should remain a split")
    };
    assert_eq!(
        children
            .iter()
            .map(|child| child.node.id())
            .collect::<Vec<_>>(),
        vec!["b", "c", "a"]
    );
    assert_eq!(
        children
            .iter()
            .map(|child| child.weight)
            .collect::<Vec<_>>(),
        vec![3.0, 5.0, 2.0]
    );

    let reordered = tree.clone();
    assert!(tree.reorder("root", 0, 3).is_err());
    assert_eq!(tree, reordered);
    assert!(tree.reorder("missing_split", 0, 1).is_err());
    assert_eq!(tree, reordered);
}

#[test]
fn context_projection_filters_unavailable_and_unknown_panels_without_mutation() {
    let tree = LayoutNode::split(
        "root",
        Axis::Horizontal,
        vec![
            (1.0, panel("home_node", "home", Placement::Center)),
            (2.0, panel("chat_node", "chat", Placement::Center)),
            (
                3.0,
                panel("context_node", "contextual_sidebar", Placement::Left),
            ),
            (
                4.0,
                panel(
                    "unknown_node",
                    "custom_unregistered_panel",
                    Placement::Right,
                ),
            ),
        ],
    );
    let original = tree.clone();

    let projected = tree.project(Destination::Home).unwrap();
    assert_eq!(tree, original, "projection mutated the durable tree");
    let panels = projected.panel_ids();
    assert!(panels.contains(&"home"));
    assert!(panels.contains(&"contextual_sidebar"));
    assert!(!panels.contains(&"chat"));
    assert!(!panels.contains(&"custom_unregistered_panel"));
}

#[test]
fn registry_exposes_destination_availability_and_orientation_rules() {
    for destination in Destination::ALL {
        for shell_panel in ["primary_navigation", "user_controls", "workspace"] {
            assert!(registry::available(shell_panel, destination));
        }
        assert!(registry::available(destination.main_panel(), destination));
        assert!(registry::available("contextual_sidebar", destination));
        assert!(registry::available("context_inspector", destination));
    }
    assert!(registry::available("chat", Destination::Messages));
    assert!(!registry::available("chat", Destination::Home));
    assert!(!registry::available(
        "server_content",
        Destination::Messages
    ));
    assert!(!registry::available(
        "custom_unregistered_panel",
        Destination::Home
    ));

    for (placement, expected) in [
        (Placement::Left, Orientation::Vertical),
        (Placement::Right, Orientation::Vertical),
        (Placement::Top, Orientation::Horizontal),
        (Placement::Bottom, Orientation::Horizontal),
        (Placement::Center, Orientation::Vertical),
    ] {
        assert_eq!(
            registry::orientation("server_list", placement, None),
            Some(expected)
        );
    }
    assert_eq!(
        registry::orientation(
            "server_list",
            Placement::Left,
            Some(Orientation::Horizontal)
        ),
        Some(Orientation::Horizontal)
    );
    assert_eq!(
        registry::orientation("home", Placement::Center, Some(Orientation::Horizontal)),
        None
    );
    assert_eq!(
        registry::orientation("unknown", Placement::Center, None),
        None
    );
}

#[test]
fn loader_rejects_corrupt_and_future_version_documents() {
    assert!(LayoutProfiles::from_json(&serde_json::json!({ "version": "broken" })).is_err());

    let mut future = serde_json::to_value(LayoutProfiles::default()).unwrap();
    future["version"] = serde_json::json!(FORMAT_VERSION + 1);
    assert!(LayoutProfiles::from_json(&future).is_err());
}

#[test]
fn split_weights_must_be_positive_finite_and_within_the_supported_range() {
    assert!(two_panel_split().validate().is_ok());

    for invalid in [0.0, -0.1, f32::NAN, f32::INFINITY, 1_000_001.0] {
        let tree = LayoutNode::split(
            "root",
            Axis::Horizontal,
            vec![
                (invalid, panel("left_node", "left_panel", Placement::Left)),
                (1.0, panel("right_node", "right_panel", Placement::Right)),
            ],
        );
        assert!(
            tree.validate().is_err(),
            "accepted invalid weight {invalid}"
        );
    }

    let max_weight = LayoutNode::split(
        "root",
        Axis::Horizontal,
        vec![
            (
                1_000_000.0,
                panel("left_node", "left_panel", Placement::Left),
            ),
            (1.0, panel("right_node", "right_panel", Placement::Right)),
        ],
    );
    assert!(max_weight.validate().is_ok());
}

#[test]
fn depth_and_node_limits_accept_the_boundary_and_reject_one_over() {
    assert!(deep_tree(0, MAX_DEPTH).validate().is_ok());
    assert!(deep_tree(0, MAX_DEPTH + 1).validate().is_err());

    assert!(wide_tree(MAX_NODES).validate().is_ok());
    assert!(wide_tree(MAX_NODES + 1).validate().is_err());
}

#[test]
fn default_shell_keeps_all_required_panels_visible() {
    let shell = default_shell();
    shell.validate().unwrap();
    for required in ["primary_navigation", "user_controls", "workspace"] {
        assert!(panel_is_visible(&shell, required));
    }

    let mut profile = LayoutProfile::new("test_profile".into(), "Test".into());
    assert!(profile.validate().is_ok());
    assert!(set_panel_visibility(
        &mut profile.shell,
        "user_controls",
        false
    ));
    assert!(profile.validate().is_err());
}

#[test]
fn profile_crud_duplicate_is_independent_and_active_delete_selects_a_survivor() {
    let mut profiles = LayoutProfiles::default();
    let default_id = profiles.active_profile_id.clone();
    let work_id = profiles.create("  Work  ").unwrap();
    assert_eq!(
        profiles
            .profiles
            .iter()
            .find(|profile| profile.id == work_id)
            .unwrap()
            .name,
        "Work"
    );
    profiles.activate(&work_id).unwrap();
    profiles.rename(&work_id, "  Work layout  ").unwrap();
    assert_eq!(profiles.active().unwrap().name, "Work layout");

    let copy_id = profiles.duplicate(&work_id, "  Copy  ").unwrap();
    assert_ne!(copy_id, work_id);
    let mut copy = profiles
        .profiles
        .iter()
        .find(|profile| profile.id == copy_id)
        .unwrap()
        .clone();
    copy.destinations
        .get_mut(&Destination::Home)
        .unwrap()
        .dock("context", "main", Placement::Right)
        .unwrap();
    profiles.replace(copy).unwrap();
    let work_home = &profiles
        .profiles
        .iter()
        .find(|profile| profile.id == work_id)
        .unwrap()
        .destinations[&Destination::Home];
    let copy_home = &profiles
        .profiles
        .iter()
        .find(|profile| profile.id == copy_id)
        .unwrap()
        .destinations[&Destination::Home];
    assert_ne!(
        work_home, copy_home,
        "duplicate shared its layout with source"
    );
    assert!(work_home.find("dock_0").is_none());
    assert!(copy_home.find("dock_0").is_some());

    profiles.activate(&copy_id).unwrap();
    profiles.delete(&copy_id).unwrap();
    assert_eq!(profiles.active_profile_id, default_id);
    assert!(profiles
        .profiles
        .iter()
        .all(|profile| profile.id != copy_id));
    profiles.delete(&work_id).unwrap();
    let before_failed_delete = profiles.clone();
    assert!(profiles.delete(&default_id).is_err());
    assert_eq!(profiles, before_failed_delete);
    assert_eq!(profiles.profiles.len(), 1);
}

#[test]
fn reset_restores_defaults_while_preserving_profile_identity_and_name() {
    let mut profiles = LayoutProfiles::default();
    let id = profiles.active_profile_id.clone();
    profiles.rename(&id, "Personal layout").unwrap();
    let mut modified = profiles.active().unwrap().clone();
    modified
        .destinations
        .get_mut(&Destination::Home)
        .unwrap()
        .dock("context", "main", Placement::Right)
        .unwrap();
    profiles.replace(modified).unwrap();
    assert!(profiles.active().unwrap().destinations[&Destination::Home]
        .find("dock_0")
        .is_some());

    profiles.reset(&id).unwrap();
    let reset = profiles.active().unwrap();
    let defaults = LayoutProfile::new(id.clone(), "Personal layout".into());
    assert_eq!(reset.id, id);
    assert_eq!(reset.name, "Personal layout");
    assert_eq!(reset.shell, defaults.shell);
    assert_eq!(reset.destinations, defaults.destinations);
}

#[test]
fn edit_sessions_cancel_to_original_and_apply_only_valid_drafts() {
    let original = LayoutProfile::new("edit_profile".into(), "Original".into());
    let mut cancel_session = LayoutEditSession::new(&original);
    cancel_session.draft.name = "Unsaved name".into();
    cancel_session
        .draft
        .destinations
        .get_mut(&Destination::Home)
        .unwrap()
        .reorder("destination_root", 0, 2)
        .unwrap();
    assert_eq!(cancel_session.cancel(), original);

    let mut apply_session = LayoutEditSession::new(&original);
    apply_session.draft.name = "Applied name".into();
    let applied = apply_session.apply().unwrap();
    assert_eq!(applied.name, "Applied name");
    assert_eq!(applied.id, original.id);

    let mut invalid_session = LayoutEditSession::new(&original);
    assert!(set_panel_visibility(
        invalid_session
            .draft
            .destinations
            .get_mut(&Destination::Home)
            .unwrap(),
        "home",
        false
    ));
    assert!(invalid_session.apply().is_err());
}
