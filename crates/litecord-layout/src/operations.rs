use crate::{Axis, LayoutError, LayoutNode, LayoutResult, Placement, WeightedNode};

impl LayoutNode {
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
    let nodes = if before {
        vec![moved, original]
    } else {
        vec![original, moved]
    };
    *target = LayoutNode::Split {
        id: id.into(),
        axis,
        children: nodes
            .into_iter()
            .map(|node| WeightedNode { weight: 1.0, node })
            .collect(),
    };
    Ok(())
}
