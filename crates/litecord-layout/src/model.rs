use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{registry, LayoutError, LayoutResult};

pub const MAX_NODES: usize = 256;
pub const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    Home,
    Messages,
    Friends,
    Servers,
    Voice,
    Inbox,
    /// Omni: the assistant and the unified memory it works from. Saved
    /// layouts from before the rename still load (`memory`).
    #[serde(alias = "memory")]
    Omni,
    Tasks,
    Settings,
}

impl Destination {
    pub const ALL: [Self; 9] = [
        Self::Home,
        Self::Messages,
        Self::Friends,
        Self::Servers,
        Self::Voice,
        Self::Inbox,
        Self::Omni,
        Self::Tasks,
        Self::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Messages => "Messages",
            Self::Friends => "Friends",
            Self::Servers => "Servers",
            Self::Voice => "Voice",
            Self::Inbox => "Inbox",
            Self::Omni => "Omni",
            Self::Tasks => "Tasks",
            Self::Settings => "Settings",
        }
    }

    pub fn main_panel(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Messages => "chat",
            Self::Friends => "friends",
            Self::Servers => "server_content",
            Self::Voice => "voice_room",
            Self::Inbox => "agent_inbox",
            Self::Omni => "memory",
            Self::Tasks => "tasks",
            Self::Settings => "settings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightedNode {
    pub weight: f32,
    pub node: LayoutNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutNode {
    Panel {
        id: String,
        panel: String,
        visible: bool,
        placement: Placement,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        orientation: Option<Orientation>,
    },
    Split {
        id: String,
        axis: Axis,
        children: Vec<WeightedNode>,
    },
}

impl LayoutNode {
    pub fn panel(id: &str, panel: &str, placement: Placement) -> Self {
        Self::Panel {
            id: id.into(),
            panel: panel.into(),
            visible: true,
            placement,
            orientation: None,
        }
    }

    pub fn split(id: &str, axis: Axis, children: Vec<(f32, Self)>) -> Self {
        Self::Split {
            id: id.into(),
            axis,
            children: children
                .into_iter()
                .map(|(weight, node)| WeightedNode { weight, node })
                .collect(),
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Panel { id, .. } | Self::Split { id, .. } => id,
        }
    }

    pub fn find(&self, id: &str) -> Option<&Self> {
        if self.id() == id {
            return Some(self);
        }
        match self {
            Self::Split { children, .. } => children.iter().find_map(|c| c.node.find(id)),
            Self::Panel { .. } => None,
        }
    }

    pub fn panel_ids(&self) -> Vec<&str> {
        match self {
            Self::Panel { panel, .. } => vec![panel],
            Self::Split { children, .. } => {
                children.iter().flat_map(|c| c.node.panel_ids()).collect()
            }
        }
    }

    /// Context suppression is a disposable projection; the durable tree is untouched.
    pub fn project(&self, destination: Destination) -> Option<Self> {
        match self {
            Self::Panel { visible, panel, .. } => {
                (*visible && registry::available(panel, destination)).then(|| self.clone())
            }
            Self::Split { id, axis, children } => {
                let children: Vec<_> = children
                    .iter()
                    .filter_map(|c| {
                        c.node.project(destination).map(|node| WeightedNode {
                            weight: c.weight,
                            node,
                        })
                    })
                    .collect();
                match children.len() {
                    0 => None,
                    1 => children.into_iter().next().map(|c| c.node),
                    _ => Some(Self::Split {
                        id: id.clone(),
                        axis: *axis,
                        children,
                    }),
                }
            }
        }
    }

    pub fn validate(&self) -> LayoutResult<()> {
        let mut ids = BTreeSet::new();
        let mut panels = BTreeSet::new();
        let mut count = 0;
        self.validate_inner(0, &mut count, &mut ids, &mut panels)
    }

    fn validate_inner<'a>(
        &'a self,
        depth: usize,
        count: &mut usize,
        ids: &mut BTreeSet<&'a str>,
        panels: &mut BTreeSet<&'a str>,
    ) -> LayoutResult<()> {
        *count += 1;
        if depth > MAX_DEPTH || *count > MAX_NODES {
            return Err(LayoutError("layout exceeds structural limits".into()));
        }
        if !valid_id(self.id()) || !ids.insert(self.id()) {
            return Err(LayoutError("invalid or duplicate node ID".into()));
        }
        match self {
            Self::Panel {
                panel,
                placement,
                orientation,
                ..
            } => {
                if !valid_id(panel) || !panels.insert(panel) {
                    return Err(LayoutError("invalid or duplicate singleton panel".into()));
                }
                if registry::descriptor(panel).is_some()
                    && registry::orientation(panel, *placement, *orientation).is_none()
                {
                    return Err(LayoutError("unsupported panel orientation".into()));
                }
            }
            Self::Split { children, .. } => {
                if children.len() < 2 {
                    return Err(LayoutError("split needs at least two children".into()));
                }
                for c in children {
                    if !c.weight.is_finite() || c.weight <= 0.0 || c.weight > 1_000_000.0 {
                        return Err(LayoutError(
                            "split weights must be positive and finite".into(),
                        ));
                    }
                    c.node.validate_inner(depth + 1, count, ids, panels)?;
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

pub fn default_shell() -> LayoutNode {
    LayoutNode::split(
        "shell",
        Axis::Horizontal,
        vec![
            (
                SHELL_WEIGHTS[0],
                LayoutNode::split(
                    "rail",
                    Axis::Vertical,
                    vec![
                        (
                            9.0,
                            LayoutNode::panel("nav", "primary_navigation", Placement::Left),
                        ),
                        (
                            1.0,
                            LayoutNode::panel("account", "user_controls", Placement::Bottom),
                        ),
                    ],
                ),
            ),
            (
                SHELL_WEIGHTS[1],
                LayoutNode::panel("outlet", "workspace", Placement::Center),
            ),
        ],
    )
}

pub fn default_workspace(destination: Destination) -> LayoutNode {
    let sidebar = match destination {
        Destination::Messages => "conversation_list",
        Destination::Servers => "channel_list",
        _ => "contextual_sidebar",
    };
    let main = destination.main_panel();
    let [side, center, inspector] = workspace_weights(destination);
    LayoutNode::split(
        "destination_root",
        Axis::Horizontal,
        vec![
            (side, LayoutNode::panel("context", sidebar, Placement::Left)),
            (center, LayoutNode::panel("main", main, Placement::Center)),
            (
                inspector,
                LayoutNode::panel("inspector", "context_inspector", Placement::Right),
            ),
        ],
    )
}

/// Default rail/workspace split (the mock's ~100px rail at 1586px).
const SHELL_WEIGHTS: [f32; 2] = [100.0, 1486.0];
/// Defaults shipped before the mock-aligned layout; profiles still carrying
/// them exactly were never resized and are upgraded on load.
const LEGACY_SHELL_WEIGHTS: [f32; 2] = [88.0, 1498.0];
const LEGACY_WORKSPACE_WEIGHTS: [f32; 3] = [280.0, 918.0, 300.0];

/// Sidebar / main / inspector weights per destination (mock proportions).
fn workspace_weights(destination: Destination) -> [f32; 3] {
    match destination {
        Destination::Messages => [360.0, 776.0, 350.0],
        _ => [296.0, 840.0, 350.0],
    }
}

impl crate::LayoutProfile {
    /// Move splits that still hold the previous built-in defaults to the
    /// current ones. Anything the user resized is left alone.
    pub fn upgrade_untouched_defaults(&mut self) {
        set_weights_if(
            &mut self.shell,
            "shell",
            &LEGACY_SHELL_WEIGHTS,
            &SHELL_WEIGHTS,
        );
        for (destination, tree) in self.destinations.iter_mut() {
            set_weights_if(
                tree,
                "destination_root",
                &LEGACY_WORKSPACE_WEIGHTS,
                &workspace_weights(*destination),
            );
        }
    }
}

fn set_weights_if(node: &mut LayoutNode, split_id: &str, from: &[f32], to: &[f32]) {
    if let LayoutNode::Split { id, children, .. } = node {
        if id == split_id
            && children.len() == from.len()
            && children
                .iter()
                .zip(from)
                .all(|(c, w)| (c.weight - w).abs() < 1e-3)
        {
            for (c, w) in children.iter_mut().zip(to) {
                c.weight = *w;
            }
            return;
        }
        for c in children {
            set_weights_if(&mut c.node, split_id, from, to);
        }
    }
}
