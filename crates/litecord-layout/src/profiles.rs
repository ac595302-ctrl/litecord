use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    default_shell, default_workspace, model::valid_id, Destination, LayoutError, LayoutNode,
    LayoutResult,
};

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_PROFILES: usize = 64;
pub const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
pub const SETTINGS_KEY: &str = "workspace.layout_profiles";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutProfile {
    pub id: String,
    pub name: String,
    pub shell: LayoutNode,
    pub destinations: BTreeMap<Destination, LayoutNode>,
}

impl LayoutProfile {
    pub fn new(id: String, name: String) -> Self {
        Self {
            id,
            name,
            shell: default_shell(),
            destinations: Destination::ALL
                .into_iter()
                .map(|d| (d, default_workspace(d)))
                .collect(),
        }
    }

    pub fn validate(&self) -> LayoutResult<()> {
        if !valid_id(&self.id)
            || self.name.trim() != self.name
            || self.name.is_empty()
            || self.name.chars().count() > 64
            || self.name.chars().any(char::is_control)
        {
            return Err(LayoutError("invalid profile ID or name".into()));
        }
        self.shell.validate()?;
        let shell_panels = self.shell.panel_ids();
        for required in ["primary_navigation", "user_controls", "workspace"] {
            if !contains_visible(&self.shell, required) {
                return Err(LayoutError("shell is missing an essential panel".into()));
            }
        }
        for d in Destination::ALL {
            let tree = self
                .destinations
                .get(&d)
                .ok_or_else(|| LayoutError("missing destination layout".into()))?;
            tree.validate()?;
            if !contains_visible(tree, d.main_panel()) {
                return Err(LayoutError(
                    "destination is missing its main content".into(),
                ));
            }
            if tree.panel_ids().iter().any(|p| shell_panels.contains(p)) {
                return Err(LayoutError("shell panel duplicated in destination".into()));
            }
        }
        Ok(())
    }
}

fn contains_visible(tree: &LayoutNode, required: &str) -> bool {
    match tree {
        LayoutNode::Panel { panel, visible, .. } => panel == required && *visible,
        LayoutNode::Split { children, .. } => {
            children.iter().any(|c| contains_visible(&c.node, required))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutProfiles {
    pub version: u32,
    pub active_profile_id: String,
    pub profiles: Vec<LayoutProfile>,
}

impl Default for LayoutProfiles {
    fn default() -> Self {
        let id = "profile_default".to_string();
        Self {
            version: FORMAT_VERSION,
            active_profile_id: id.clone(),
            profiles: vec![LayoutProfile::new(id, "Default".into())],
        }
    }
}

impl LayoutProfiles {
    pub fn validate(&self) -> LayoutResult<()> {
        if self.version != FORMAT_VERSION {
            return Err(LayoutError("unsupported layout format version".into()));
        }
        if self.profiles.is_empty() || self.profiles.len() > MAX_PROFILES {
            return Err(LayoutError("invalid profile count".into()));
        }
        let mut ids = BTreeSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            if !ids.insert(&profile.id) {
                return Err(LayoutError("duplicate profile ID".into()));
            }
        }
        if !ids.contains(&self.active_profile_id) {
            return Err(LayoutError("active profile is missing".into()));
        }
        let json = serde_json::to_vec(self).map_err(|e| LayoutError(e.to_string()))?;
        if json.len() > MAX_DOCUMENT_BYTES {
            return Err(LayoutError("layout document is too large".into()));
        }
        Ok(())
    }

    pub fn from_json(value: &serde_json::Value) -> LayoutResult<Self> {
        if value.to_string().len() > MAX_DOCUMENT_BYTES {
            return Err(LayoutError("layout document is too large".into()));
        }
        let mut profiles: Self = serde_json::from_value(value.clone())
            .map_err(|_| LayoutError("invalid layout document".into()))?;
        profiles.validate()?;
        for p in &mut profiles.profiles {
            p.upgrade_untouched_defaults();
        }
        Ok(profiles)
    }

    pub fn active(&self) -> Option<&LayoutProfile> {
        self.profiles
            .iter()
            .find(|p| p.id == self.active_profile_id)
    }

    pub fn activate(&mut self, id: &str) -> LayoutResult<()> {
        if !self.profiles.iter().any(|p| p.id == id) {
            return Err(LayoutError("profile not found".into()));
        }
        self.active_profile_id = id.into();
        Ok(())
    }

    pub fn create(&mut self, name: &str) -> LayoutResult<String> {
        if self.profiles.len() >= MAX_PROFILES {
            return Err(LayoutError("profile limit reached".into()));
        }
        let id = self.new_id();
        let profile = LayoutProfile::new(id.clone(), name.trim().into());
        profile.validate()?;
        self.profiles.push(profile);
        Ok(id)
    }

    pub fn duplicate(&mut self, id: &str, name: &str) -> LayoutResult<String> {
        if self.profiles.len() >= MAX_PROFILES {
            return Err(LayoutError("profile limit reached".into()));
        }
        let mut profile = self
            .profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| LayoutError("profile not found".into()))?;
        profile.id = self.new_id();
        profile.name = name.trim().into();
        profile.validate()?;
        let new_id = profile.id.clone();
        self.profiles.push(profile);
        Ok(new_id)
    }

    pub fn rename(&mut self, id: &str, name: &str) -> LayoutResult<()> {
        let mut draft = self.clone();
        let profile = draft
            .profiles
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| LayoutError("profile not found".into()))?;
        profile.name = name.trim().into();
        profile.validate()?;
        *self = draft;
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> LayoutResult<()> {
        if self.profiles.len() <= 1 {
            return Err(LayoutError("keep at least one layout profile".into()));
        }
        let index = self
            .profiles
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| LayoutError("profile not found".into()))?;
        self.profiles.remove(index);
        if self.active_profile_id == id {
            self.active_profile_id = self.profiles[0].id.clone();
        }
        Ok(())
    }

    pub fn replace(&mut self, profile: LayoutProfile) -> LayoutResult<()> {
        profile.validate()?;
        let slot = self
            .profiles
            .iter_mut()
            .find(|p| p.id == profile.id)
            .ok_or_else(|| LayoutError("profile not found".into()))?;
        *slot = profile;
        Ok(())
    }

    pub fn reset(&mut self, id: &str) -> LayoutResult<()> {
        let p = self
            .profiles
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| LayoutError("profile not found".into()))?;
        *p = LayoutProfile::new(p.id.clone(), p.name.clone());
        Ok(())
    }

    fn new_id(&self) -> String {
        (1..=MAX_PROFILES)
            .map(|n| format!("profile_{n}"))
            .find(|id| !self.profiles.iter().any(|p| p.id == *id))
            .unwrap_or_else(|| "profile_extra".into())
    }
}

/// Editing never persists implicitly; caller applies the draft through app services.
#[derive(Debug, Clone)]
pub struct LayoutEditSession {
    pub original: LayoutProfile,
    pub draft: LayoutProfile,
}

impl LayoutEditSession {
    pub fn new(profile: &LayoutProfile) -> Self {
        Self {
            original: profile.clone(),
            draft: profile.clone(),
        }
    }
    pub fn cancel(self) -> LayoutProfile {
        self.original
    }
    pub fn apply(self) -> LayoutResult<LayoutProfile> {
        self.draft.validate()?;
        Ok(self.draft)
    }
}

#[cfg(test)]
mod upgrade_tests {
    use super::*;

    fn weights(n: &LayoutNode) -> Vec<f32> {
        match n {
            LayoutNode::Split { children, .. } => children.iter().map(|c| c.weight).collect(),
            _ => vec![],
        }
    }

    fn set(n: &mut LayoutNode, to: &[f32]) {
        if let LayoutNode::Split { children, .. } = n {
            for (c, w) in children.iter_mut().zip(to) {
                c.weight = *w;
            }
        }
    }

    #[test]
    fn untouched_legacy_defaults_upgrade_but_resized_splits_stay() {
        let mut profiles = LayoutProfiles::default();
        let p = &mut profiles.profiles[0];
        set(&mut p.shell, &[88.0, 1498.0]);
        for tree in p.destinations.values_mut() {
            set(tree, &[280.0, 918.0, 300.0]);
        }
        // The user resized Tasks; it must keep their weights.
        set(
            p.destinations.get_mut(&Destination::Tasks).unwrap(),
            &[333.0, 918.0, 300.0],
        );
        let loaded = LayoutProfiles::from_json(&serde_json::to_value(&profiles).unwrap()).unwrap();
        let p = &loaded.profiles[0];
        assert_eq!(weights(&p.shell), vec![100.0, 1486.0]);
        assert_eq!(
            weights(&p.destinations[&Destination::Home]),
            vec![296.0, 840.0, 350.0]
        );
        assert_eq!(
            weights(&p.destinations[&Destination::Messages]),
            vec![360.0, 776.0, 350.0]
        );
        assert_eq!(
            weights(&p.destinations[&Destination::Tasks]),
            vec![333.0, 918.0, 300.0]
        );
    }

    #[test]
    fn memory_destination_key_still_loads_as_omni() {
        let profiles = LayoutProfiles::default();
        let text = serde_json::to_string(&profiles)
            .unwrap()
            .replace("\"omni\"", "\"memory\"");
        assert!(text.contains("\"memory\""));
        let loaded = LayoutProfiles::from_json(&serde_json::from_str(&text).unwrap()).unwrap();
        assert!(loaded.profiles[0]
            .destinations
            .contains_key(&Destination::Omni));
    }
}
