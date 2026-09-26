#![allow(clippy::unwrap_used, clippy::expect_used)]

use litecord_layout::{Axis, Destination, LayoutNode, LayoutProfile, Placement};

fn tree_with_hidden_sibling() -> LayoutNode {
    // root (Horizontal): a (weight 1.0), b (weight 1.0, hidden), c (weight 2.0)
    let mut tree = LayoutNode::split(
        "root",
        Axis::Horizontal,
        vec![
            (1.0, LayoutNode::panel("a", "home", Placement::Left)),
            (1.0, LayoutNode::panel("b", "friends", Placement::Center)),
            (2.0, LayoutNode::panel("c", "tasks", Placement::Right)),
        ],
    );
    tree.set_visible("b", false).unwrap();
    tree
}

#[test]
fn hidden_sibling_weight_and_visibility_survive_resize_of_visible_children() {
    let mut tree = tree_with_hidden_sibling();

    // Only "a" and "c" are visible/projected; resize them 3:1 instead of 1:2.
    tree.resize_projected("root", &[("a".to_string(), 30.0), ("c".to_string(), 10.0)])
        .unwrap();

    let LayoutNode::Split { children, .. } = tree.find("root").unwrap() else {
        panic!("root should still be a split");
    };

    let a = children.iter().find(|c| c.node.id() == "a").unwrap();
    let b = children.iter().find(|c| c.node.id() == "b").unwrap();
    let c = children.iter().find(|c| c.node.id() == "c").unwrap();

    // Hidden sibling untouched.
    assert_eq!(b.weight, 1.0);
    assert!(matches!(&b.node, LayoutNode::Panel { visible: false, .. }));

    // Visible siblings keep their combined total (1.0 + 2.0 = 3.0) and split it 3:1.
    assert!((a.weight + c.weight - 3.0).abs() < 1e-5);
    assert!((a.weight / c.weight - 3.0).abs() < 1e-4);
}

#[test]
fn collapsed_group_is_addressed_by_its_sole_visible_descendant() {
    // root (Horizontal): a (weight 1.0), group (weight 1.0, a Split of [visible x, hidden y])
    let group = LayoutNode::split(
        "group",
        Axis::Vertical,
        vec![
            (1.0, LayoutNode::panel("x", "home", Placement::Top)),
            (
                1.0,
                LayoutNode::panel("y", "user_controls", Placement::Bottom),
            ),
        ],
    );
    let mut tree = LayoutNode::split(
        "root",
        Axis::Horizontal,
        vec![
            (
                1.0,
                LayoutNode::panel("a", "primary_navigation", Placement::Left),
            ),
            (1.0, group),
        ],
    );
    tree.set_visible("y", false).unwrap();

    // Projection of "group" collapses to just "x", since "y" is hidden.
    let projected = tree.project(Destination::Home).unwrap();
    assert!(projected.find("group").is_none());
    assert!(projected.find("x").is_some());

    // "group" (addressed via its collapsed descendant "x") and "a" are resized 1:3.
    tree.resize_projected("root", &[("a".to_string(), 10.0), ("x".to_string(), 30.0)])
        .unwrap();

    let LayoutNode::Split { children, .. } = tree.find("root").unwrap() else {
        panic!("root should still be a split");
    };
    let a = children.iter().find(|c| c.node.id() == "a").unwrap();
    let group = children.iter().find(|c| c.node.id() == "group").unwrap();

    assert!((a.weight + group.weight - 2.0).abs() < 1e-5);
    assert!((group.weight / a.weight - 3.0).abs() < 1e-4);

    // The hidden descendant inside the group is left alone.
    let LayoutNode::Split {
        children: group_children,
        ..
    } = &group.node
    else {
        panic!("group should still be a split");
    };
    let y = group_children.iter().find(|c| c.node.id() == "y").unwrap();
    assert_eq!(y.weight, 1.0);
    assert!(matches!(&y.node, LayoutNode::Panel { visible: false, .. }));
}

#[test]
fn invalid_inputs_are_rejected_and_leave_the_tree_unchanged() {
    let mut tree = tree_with_hidden_sibling();
    let original = tree.clone();

    // Unknown split id.
    assert!(tree
        .resize_projected("nonexistent", &[("a".to_string(), 10.0)])
        .is_err());
    assert_eq!(tree, original);

    // Projected id maps to no child.
    assert!(tree
        .resize_projected("root", &[("does_not_exist".to_string(), 10.0)])
        .is_err());
    assert_eq!(tree, original);

    // Duplicate mapping: "a" and "root" both resolve within the same child... use two
    // ids that map to the same saved child by targeting "a" twice (also covers the
    // duplicate-projected-id case) plus a genuinely distinct duplicate-mapping case
    // using a collapsed group is covered in the other test; here duplicate ids to the
    // very same child id are exercised directly.
    assert!(tree
        .resize_projected("root", &[("a".to_string(), 10.0), ("a".to_string(), 20.0)])
        .is_err());
    assert_eq!(tree, original);

    // Zero, negative, NaN, infinite sizes.
    for bad in [0.0f32, -1.0, f32::NAN, f32::INFINITY] {
        assert!(tree
            .resize_projected("root", &[("a".to_string(), bad), ("c".to_string(), 10.0)])
            .is_err());
        assert_eq!(tree, original);
    }

    // Empty input.
    assert!(tree.resize_projected("root", &[]).is_err());
    assert_eq!(tree, original);
}

#[test]
fn duplicate_mapping_to_the_same_saved_child_is_rejected() {
    // "a" itself and "a" via `find` both resolve to child "a"; use a group so two
    // distinct projected ids resolve to the very same saved child.
    let group = LayoutNode::split(
        "group",
        Axis::Vertical,
        vec![
            (1.0, LayoutNode::panel("x", "home", Placement::Top)),
            (1.0, LayoutNode::panel("y", "friends", Placement::Bottom)),
        ],
    );
    let mut tree = LayoutNode::split(
        "root",
        Axis::Horizontal,
        vec![
            (1.0, LayoutNode::panel("a", "tasks", Placement::Left)),
            (1.0, group),
        ],
    );
    let original = tree.clone();

    // "x" and "y" both live inside "group": mapping both is a duplicate mapping.
    assert!(tree
        .resize_projected("root", &[("x".to_string(), 10.0), ("y".to_string(), 20.0)])
        .is_err());
    assert_eq!(tree, original);
}

#[test]
fn survives_a_serialize_resize_deserialize_round_trip() {
    let mut profile = LayoutProfile::new("profile_test".into(), "Test".into());
    let tree = profile
        .destinations
        .get_mut(&Destination::Messages)
        .unwrap();
    tree.set_visible("inspector", false).unwrap();
    let original_inspector_weight = match tree.find("destination_root") {
        Some(LayoutNode::Split { children, .. }) => {
            children
                .iter()
                .find(|c| c.node.id() == "inspector")
                .unwrap()
                .weight
        }
        _ => panic!("destination_root should be a split"),
    };

    let json = serde_json::to_string(&profile).unwrap();
    let mut profile: LayoutProfile = serde_json::from_str(&json).unwrap();

    let tree = profile
        .destinations
        .get_mut(&Destination::Messages)
        .unwrap();
    tree.resize_projected(
        "destination_root",
        &[("context".to_string(), 100.0), ("main".to_string(), 300.0)],
    )
    .unwrap();

    let json = serde_json::to_string(&profile).unwrap();
    let mut profile: LayoutProfile = serde_json::from_str(&json).unwrap();

    let tree = profile
        .destinations
        .get_mut(&Destination::Messages)
        .unwrap();
    tree.set_visible("inspector", true).unwrap();

    let LayoutNode::Split { children, .. } = tree.find("destination_root").unwrap() else {
        panic!("destination_root should still be a split");
    };
    let inspector = children
        .iter()
        .find(|c| c.node.id() == "inspector")
        .unwrap();
    assert_eq!(inspector.weight, original_inspector_weight);
    assert!(matches!(
        &inspector.node,
        LayoutNode::Panel { visible: true, .. }
    ));

    profile.validate().unwrap();
}
