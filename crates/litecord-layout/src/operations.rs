use std::collections::BTreeSet;

use crate::{Axis, LayoutError, LayoutNode, LayoutResult, Placement, WeightedNode};

impl LayoutNode {
    /// Add an optional panel beside an existing leaf, without touching other IDs.
    pub fn insert_panel(
        &mut self,
        panel: Self,
        target: &str,
        placement: Placement,
    ) -> LayoutResult<()> {
        if !matches!(panel, Self::Panel { .. }) || placement == Placement::Center {
            return Err(LayoutError(
                "insert requires a panel and edge placement".into(),
            ));
        }
        let mut draft = self.clone();
        let id = unique_id(&draft, "insert", panel.id());
        let mut panel = panel;
        if let Self::Panel { placement: p, .. } = &mut panel {
            *p = placement;
        }
        insert_relative(&mut draft, target, panel, placement, &id)?;
        draft.validate()?;
        *self = draft;
        Ok(())
    }

    pub fn set_visible(&mut self, id: &str, visible: bool) -> LayoutResult<()> {
        let mut draft = self.clone();
        match find_mut(&mut draft, id) {
            Some(Self::Panel { visible: v, .. }) => *v = visible,
            _ => return Err(LayoutError("visibility requires an existing panel".into())),
        }
        draft.validate()?;
        *self = draft;
        Ok(())
    }

    /// Transactional leaf move. IDs and all unrelated subtrees are preserved.
    pub fn dock(&mut self, source: &str, target: &str, placement: Placement) -> LayoutResult<()> {
        self.validate()?;
        if source == target || placement == Placement::Center {
            return Err(LayoutError("invalid docking target".into()));
        }
        if !matches!(self.find(source), Some(Self::Panel { .. }))
            || !matches!(self.find(target), Some(Self::Panel { .. }))
        {
            return Err(LayoutError(
                "dock requires two existing panel leaves".into(),
            ));
        }
        let mut draft = self.clone();
        let moved = remove_leaf(&mut draft, source)
            .ok_or_else(|| LayoutError("source is not movable".into()))?;
        normalize(&mut draft);
        let mut moved = moved;
        if let Self::Panel { placement: p, .. } = &mut moved {
            *p = placement;
        }
        let id = unique_id(&draft, "dock", moved.id());
        insert_relative(&mut draft, target, moved, placement, &id)?;
        draft.validate()?;
        *self = draft;
        Ok(())
    }

    pub fn resize(&mut self, split_id: &str, weights: &[f32]) -> LayoutResult<()> {
        let mut draft = self.clone();
        let node =
            find_mut(&mut draft, split_id).ok_or_else(|| LayoutError("split not found".into()))?;
        match node {
            Self::Split { children, .. } if children.len() == weights.len() => {
                for (c, weight) in children.iter_mut().zip(weights) {
                    c.weight = *weight;
                }
            }
            _ => return Err(LayoutError("resize must match a split's children".into())),
        }
        draft.validate()?;
        *self = draft;
        Ok(())
    }

    /// Resize a split using sizes measured on the *projected* tree, rather than the
    /// saved tree's own children.
    ///
    /// The UI renders `LayoutNode::project`ed trees: hidden/unavailable panels are
    /// dropped and a group that collapses to a single visible descendant is replaced
    /// by that descendant. So a size the UI measured for a projected child may in
    /// fact belong to a *descendant* of a saved child (when that saved child is a
    /// group that collapsed), not to the saved child itself.
    ///
    /// `visible` is `(projected_child_id, size)` pairs, in any order; a plain
    /// `String` key (rather than `&str`) is used so callers can hand over owned UI
    /// measurement data without borrowing back into the tree. Each projected id must
    /// resolve to exactly one direct child of the saved split named `split_id`
    /// (either the child itself, or an ancestor whose subtree contains that id).
    /// Every direct child may be claimed by at most one projected id.
    ///
    /// Hidden/non-participating children are left untouched (weight and visibility
    /// unchanged). Participating children share out their combined existing weight
    /// `T` in proportion to the measured sizes: child `i` gets `T * size_i /
    /// sum(sizes)`. The result is applied through [`LayoutNode::resize`], so the
    /// existing validation runs and a rejected input leaves `self` unchanged.
    pub fn resize_projected(
        &mut self,
        split_id: &str,
        visible: &[(String, f32)],
    ) -> LayoutResult<()> {
        if visible.is_empty() {
            return Err(LayoutError(
                "resize_projected requires at least one measured size".into(),
            ));
        }

        let mut seen_ids = BTreeSet::new();
        let mut total_size = 0.0f32;
        for (id, size) in visible {
            if !size.is_finite() || *size <= 0.0 {
                return Err(LayoutError(
                    "resize_projected sizes must be positive and finite".into(),
                ));
            }
            if !seen_ids.insert(id.as_str()) {
                return Err(LayoutError("duplicate projected ID".into()));
            }
            total_size += *size;
        }

        let children = match self.find(split_id) {
            Some(Self::Split { children, .. }) => children,
            _ => return Err(LayoutError("split not found".into())),
        };

        // Map each projected id to the index of the direct child that contains it.
        let mut claimed = vec![false; children.len()];
        let mut mapped_index = Vec::with_capacity(visible.len());
        for (id, _) in visible {
            let idx = children
                .iter()
                .position(|c| c.node.find(id).is_some())
                .ok_or_else(|| LayoutError("projected ID does not map to any child".into()))?;
            if claimed[idx] {
                return Err(LayoutError(
                    "two projected IDs map to the same saved child".into(),
                ));
            }
            claimed[idx] = true;
            mapped_index.push(idx);
        }

        let participating_weight: f32 = children
            .iter()
            .enumerate()
            .filter(|(i, _)| claimed[*i])
            .map(|(_, c)| c.weight)
            .sum();

        let mut new_weights: Vec<f32> = children.iter().map(|c| c.weight).collect();
        for ((_, size), idx) in visible.iter().zip(mapped_index) {
            new_weights[idx] = participating_weight * size / total_size;
        }

        self.resize(split_id, &new_weights)
    }

    pub fn reorder(&mut self, split_id: &str, from: usize, to: usize) -> LayoutResult<()> {
        let mut draft = self.clone();
        match find_mut(&mut draft, split_id) {
            Some(Self::Split { children, .. }) if from < children.len() && to < children.len() => {
                let node = children.remove(from);
                children.insert(to, node);
            }
            _ => return Err(LayoutError("invalid group reorder".into())),
        }
        draft.validate()?;
        *self = draft;
        Ok(())
    }
}

fn find_mut<'a>(node: &'a mut LayoutNode, id: &str) -> Option<&'a mut LayoutNode> {
    if node.id() == id {
        return Some(node);
    }
    match node {
        LayoutNode::Split { children, .. } => {
            children.iter_mut().find_map(|c| find_mut(&mut c.node, id))
        }
        _ => None,
    }
}

fn remove_leaf(node: &mut LayoutNode, id: &str) -> Option<LayoutNode> {
    if let LayoutNode::Split { children, .. } = node {
        if let Some(index) = children.iter().position(|c| c.node.id() == id) {
            return Some(children.remove(index).node);
        }
        for child in children {
            if let Some(removed) = remove_leaf(&mut child.node, id) {
                return Some(removed);
            }
        }
    }
    None
}

fn normalize(node: &mut LayoutNode) {
    if let LayoutNode::Split { children, .. } = node {
        for child in children.iter_mut() {
            normalize(&mut child.node);
        }
        children.retain(
            |c| !matches!(&c.node,LayoutNode::Split { children,.. } if children.is_empty()),
        );
        if children.len() == 1 {
            *node = children.remove(0).node;
        }
    }
}

fn unique_id(root: &LayoutNode, prefix: &str, reserved: &str) -> String {
    for i in 0..=crate::MAX_NODES {
        let candidate = format!("{prefix}_{i}");
        if candidate != reserved && root.find(&candidate).is_none() {
            return candidate;
        }
    }
    // Structural limits make this branch unreachable for a valid tree.
    format!("{prefix}_extra")
}

fn insert_relative(
    root: &mut LayoutNode,
    target: &str,
    moved: LayoutNode,
    placement: Placement,
    id: &str,
) -> LayoutResult<()> {
    let target = find_mut(root, target)
        .ok_or_else(|| LayoutError("target disappeared during move".into()))?;
    let original = target.clone();
    let before = matches!(placement, Placement::Left | Placement::Top);
    let axis = if matches!(placement, Placement::Left | Placement::Right) {
        Axis::Horizontal
    } else {
        Axis::Vertical
    };
    let moved_weight = match &moved {
        LayoutNode::Panel { panel, .. }
            if matches!(panel.as_str(), "primary_navigation" | "user_controls") =>
        {
            0.08
        }
        LayoutNode::Panel { panel, .. } if panel == "server_list" => 0.12,
        _ => 0.5,
    };
    let nodes = if before {
        vec![(moved_weight, moved), (1.0 - moved_weight, original)]
    } else {
        vec![(1.0 - moved_weight, original), (moved_weight, moved)]
    };
    *target = LayoutNode::Split {
        id: id.into(),
        axis,
        children: nodes
            .into_iter()
            .map(|(weight, node)| WeightedNode { weight, node })
            .collect(),
    };
    Ok(())
}
