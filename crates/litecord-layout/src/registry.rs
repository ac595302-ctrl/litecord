//! Static metadata for the built-in workspace panels.
//!
//! The registry is deliberately declarative: persisted profiles refer to panel IDs,
//! while availability and rendering orientation are resolved from this table.

use crate::{Destination, Orientation, Placement};

/// Display and compatibility metadata for one built-in panel.
#[derive(Debug, Clone, Copy)]
pub struct PanelDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub minimum_width: f32,
    pub minimum_height: f32,
    pub destinations: &'static [Destination],
    pub orientations: &'static [Orientation],
}

const ALL_DESTINATIONS: &[Destination] = &[
    Destination::Home,
    Destination::Messages,
    Destination::Friends,
    Destination::Servers,
    Destination::Voice,
    Destination::Inbox,
    Destination::Memory,
    Destination::Tasks,
    Destination::Settings,
];
const MESSAGE_DESTINATION: &[Destination] = &[Destination::Messages];
const SERVER_DESTINATION: &[Destination] = &[Destination::Servers];
const VERTICAL: &[Orientation] = &[Orientation::Vertical];
const HORIZONTAL_AND_VERTICAL: &[Orientation] = &[Orientation::Horizontal, Orientation::Vertical];

static PANELS: [PanelDescriptor; 17] = [
    PanelDescriptor {
        id: "primary_navigation",
        label: "Primary navigation",
        minimum_width: 64.0,
        minimum_height: 320.0,
        destinations: ALL_DESTINATIONS,
        orientations: HORIZONTAL_AND_VERTICAL,
    },
    PanelDescriptor {
        id: "user_controls",
        label: "User controls",
        minimum_width: 64.0,
        minimum_height: 64.0,
        destinations: ALL_DESTINATIONS,
        orientations: HORIZONTAL_AND_VERTICAL,
    },
    PanelDescriptor {
        id: "workspace",
        label: "Workspace",
        minimum_width: 320.0,
        minimum_height: 240.0,
        destinations: ALL_DESTINATIONS,
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "contextual_sidebar",
        label: "Contextual sidebar",
        minimum_width: 200.0,
        minimum_height: 160.0,
        destinations: ALL_DESTINATIONS,
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "context_inspector",
        label: "Context inspector",
        minimum_width: 220.0,
        minimum_height: 160.0,
        destinations: ALL_DESTINATIONS,
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "home",
        label: "Home",
        minimum_width: 320.0,
        minimum_height: 240.0,
        destinations: &[Destination::Home],
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "chat",
        label: "Chat",
        minimum_width: 320.0,
        minimum_height: 240.0,
        destinations: MESSAGE_DESTINATION,
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "conversation_list",
        label: "Conversations",
        minimum_width: 208.0,
        minimum_height: 160.0,
        destinations: MESSAGE_DESTINATION,
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "friends",
        label: "Friends",
        minimum_width: 280.0,
        minimum_height: 200.0,
        destinations: &[Destination::Friends],
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "server_list",
        label: "Servers",
        minimum_width: 72.0,
        minimum_height: 96.0,
        destinations: SERVER_DESTINATION,
        orientations: HORIZONTAL_AND_VERTICAL,
    },
    PanelDescriptor {
        id: "channel_list",
        label: "Channels",
        minimum_width: 180.0,
        minimum_height: 160.0,
        destinations: SERVER_DESTINATION,
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "server_content",
        label: "Server content",
        minimum_width: 320.0,
        minimum_height: 240.0,
        destinations: SERVER_DESTINATION,
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "voice_room",
        label: "Voice room",
        minimum_width: 360.0,
        minimum_height: 240.0,
        destinations: &[Destination::Voice],
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "agent_inbox",
        label: "Agent inbox",
        minimum_width: 280.0,
        minimum_height: 200.0,
        destinations: &[Destination::Inbox],
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "memory",
        label: "Memory",
        minimum_width: 300.0,
        minimum_height: 220.0,
        destinations: &[Destination::Memory],
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "tasks",
        label: "Tasks",
        minimum_width: 280.0,
        minimum_height: 200.0,
        destinations: &[Destination::Tasks],
        orientations: VERTICAL,
    },
    PanelDescriptor {
        id: "settings",
        label: "Settings",
        minimum_width: 300.0,
        minimum_height: 240.0,
        destinations: &[Destination::Settings],
        orientations: VERTICAL,
    },
];

/// Returns the descriptor for a registered built-in panel ID.
pub fn descriptor(id: &str) -> Option<&'static PanelDescriptor> {
    PANELS.iter().find(|panel| panel.id == id)
}

/// Returns whether a panel belongs in the current destination's projection.
///
/// Shell panels and contextual panels are present in every destination. A panel
/// that is unavailable in a destination remains intact in the saved profile.
pub fn available(id: &str, destination: Destination) -> bool {
    if matches!(id, "primary_navigation" | "user_controls" | "workspace") {
        return true;
    }

    match descriptor(id) {
        Some(panel) => panel.destinations.contains(&destination),
        None => false,
    }
}

/// Resolves an orientation from an explicit preference or placement metadata.
///
/// Adaptable panels use horizontal orientation in top or bottom placements and
/// vertical orientation at the sides. Center placements default to vertical.
/// Panels that do not support the placement's natural orientation use a
/// supported vertical orientation where possible. Explicit preferences must be
/// listed in the panel descriptor.
pub fn orientation(
    id: &str,
    placement: Placement,
    override_orientation: Option<Orientation>,
) -> Option<Orientation> {
    let panel = descriptor(id)?;

    if let Some(orientation) = override_orientation {
        return panel
            .orientations
            .contains(&orientation)
            .then_some(orientation);
    }

    let preferred = match placement {
        Placement::Top | Placement::Bottom => Orientation::Horizontal,
        Placement::Left | Placement::Right | Placement::Center => Orientation::Vertical,
    };
    if panel.orientations.contains(&preferred) {
        return Some(preferred);
    }

    if panel.orientations.contains(&Orientation::Vertical) {
        Some(Orientation::Vertical)
    } else if panel.orientations.contains(&Orientation::Horizontal) {
        Some(Orientation::Horizontal)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{available, orientation};
    use crate::{Destination, Orientation, Placement};

    #[test]
    fn contextual_and_shell_panels_are_available_everywhere() {
        let destinations = [
            Destination::Home,
            Destination::Messages,
            Destination::Friends,
            Destination::Servers,
            Destination::Voice,
            Destination::Inbox,
            Destination::Memory,
            Destination::Tasks,
            Destination::Settings,
        ];
        for destination in destinations {
            assert!(available("contextual_sidebar", destination));
            assert!(available("context_inspector", destination));
            assert!(available("primary_navigation", destination));
            assert!(available("user_controls", destination));
            assert!(available("workspace", destination));
        }
    }

    #[test]
    fn server_list_orientation_follows_placement_and_validates_overrides() {
        assert_eq!(
            orientation("server_list", Placement::Left, None),
            Some(Orientation::Vertical)
        );
        assert_eq!(
            orientation("server_list", Placement::Right, None),
            Some(Orientation::Vertical)
        );
        assert_eq!(
            orientation("server_list", Placement::Top, None),
            Some(Orientation::Horizontal)
        );
        assert_eq!(
            orientation("server_list", Placement::Bottom, None),
            Some(Orientation::Horizontal)
        );
        assert_eq!(
            orientation("server_list", Placement::Center, None),
            Some(Orientation::Vertical)
        );
        assert_eq!(
            orientation(
                "server_list",
                Placement::Left,
                Some(Orientation::Horizontal)
            ),
            Some(Orientation::Horizontal)
        );
        assert_eq!(
            orientation("home", Placement::Center, Some(Orientation::Horizontal)),
            None
        );
        assert_eq!(orientation("unknown", Placement::Center, None), None);
    }
}
