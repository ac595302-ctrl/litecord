#![allow(clippy::unwrap_used, clippy::expect_used)]

use litecord_layout::{
    registry, Axis, Destination, LayoutNode, LayoutProfile, Orientation, Placement,
};

#[test]
fn inserting_optional_server_list_above_main_preserves_ids_and_projects_horizontal() {
    let mut profile = LayoutProfile::new("profile_test".into(), "Test".into());
    let tree = profile.destinations.get_mut(&Destination::Servers).unwrap();
    let existing_ids = ["destination_root", "context", "main", "inspector"];

    tree.insert_panel(
        LayoutNode::panel("server_list_node", "server_list", Placement::Center),
        "main",
        Placement::Top,
    )
    .unwrap();

    for id in existing_ids {
        assert!(tree.find(id).is_some(), "insertion lost existing node {id}");
    }
    assert_eq!(
        tree.panel_ids()
            .iter()
            .filter(|id| **id == "server_list")
            .count(),
        1,
        "inserted panel should occur once"
    );

    let LayoutNode::Split { axis, .. } = tree.find("insert_0").unwrap() else {
        panic!("insertion should wrap main in a split")
    };
    assert_eq!(*axis, Axis::Vertical);

    let projected = tree.project(Destination::Servers).unwrap();
    let LayoutNode::Panel {
        placement,
        orientation,
        ..
    } = projected.find("server_list_node").unwrap()
    else {
        panic!("server_list should remain a panel in the destination projection")
    };
    assert_eq!(
        registry::orientation("server_list", *placement, *orientation),
        Some(Orientation::Horizontal)
    );
    profile.validate().unwrap();
}

#[test]
fn insertion_rejects_duplicate_node_or_panel_ids_without_mutating_tree() {
    let mut tree = LayoutProfile::new("profile_test".into(), "Test".into()).destinations
        [&Destination::Servers]
        .clone();
    let original = tree.clone();

    // The new node reuses the existing main node ID while its panel ID is
    // otherwise valid and unique.
    assert!(tree
        .insert_panel(
            LayoutNode::panel("main", "server_list", Placement::Top),
            "main",
            Placement::Top,
        )
        .is_err());
    assert_eq!(
        tree, original,
        "duplicate node ID partially changed the tree"
    );

    // This node ID is unique, but server_content already exists in the tree.
    assert!(tree
        .insert_panel(
            LayoutNode::panel("extra_server_content", "server_content", Placement::Top),
            "main",
            Placement::Top,
        )
        .is_err());
    assert_eq!(
        tree, original,
        "duplicate panel ID partially changed the tree"
    );
}

#[test]
fn hiding_optional_inspector_only_removes_it_from_projection() {
    let mut profile = LayoutProfile::new("profile_test".into(), "Test".into());
    let tree = profile
        .destinations
        .get_mut(&Destination::Messages)
        .unwrap();

    tree.set_visible("inspector", false).unwrap();
    profile.validate().unwrap();

    let saved = &profile.destinations[&Destination::Messages];
    assert!(matches!(
        saved.find("inspector"),
        Some(LayoutNode::Panel { visible: false, .. })
    ));
    assert!(saved.panel_ids().contains(&"context_inspector"));
    let projected = saved.project(Destination::Messages).unwrap();
    assert!(!projected.panel_ids().contains(&"context_inspector"));
    assert!(saved.panel_ids().contains(&"context_inspector"));
}

#[test]
fn hiding_destination_main_panel_invalidates_the_profile() {
    let mut profile = LayoutProfile::new("profile_test".into(), "Test".into());
    profile
        .destinations
        .get_mut(&Destination::Home)
        .unwrap()
        .set_visible("main", false)
        .unwrap();

    assert!(profile.validate().is_err());
}
