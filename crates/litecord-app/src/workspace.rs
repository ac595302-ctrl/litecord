//! Atomic, validated workspace preferences. Widgets never open the store.

use litecord_core::{events::ApplicationEvent, Error, Result};
use litecord_layout::{LayoutProfile, LayoutProfiles, LayoutResult, SETTINGS_KEY};
use litecord_store::repos;
use litecord_types::Revision;
use serde::Serialize;

use crate::LitecordApp;

#[derive(Debug, Clone, Serialize)]
pub struct LayoutProfilesViewModel {
    pub as_of_revision: Revision,
    pub profiles: LayoutProfiles,
    /// Optimistic token for this setting only; social revisions do not invalidate edits.
    pub storage_token: String,
    pub recovery_notice: Option<String>,
}

fn token(value: &Option<String>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(if value.is_some() {
        b"present:"
    } else {
        b"missing:"
    });
    if let Some(text) = value {
        hasher.update(text.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn decode(value: &Option<String>) -> (LayoutProfiles, Option<String>) {
    match value {
        None => (LayoutProfiles::default(),None),
        Some(text) => match parse_document(text) {
            Ok(p) => (p,None),
            Err(_) => (LayoutProfiles::default(),Some("Saved layout could not be loaded. Using Default; saved data is preserved until you reset it.".into())),
        },
    }
}

fn parse_document(text: &str) -> LayoutResult<LayoutProfiles> {
    if text.len() > litecord_layout::MAX_DOCUMENT_BYTES {
        return Err(litecord_layout::LayoutError(
            "layout document is too large".into(),
        ));
    }
    let value = serde_json::from_str(text)
        .map_err(|_| litecord_layout::LayoutError("invalid layout JSON".into()))?;
    LayoutProfiles::from_json(&value)
}

impl LitecordApp {
    pub fn layout_profiles_view(&self) -> Result<LayoutProfilesViewModel> {
        self.inner.db.read(|r| {
            let raw = repos::settings::get_raw(r, SETTINGS_KEY)?;
            let (profiles, recovery_notice) = decode(&raw);
            Ok(LayoutProfilesViewModel {
                as_of_revision: r.revision(),
                profiles,
                storage_token: token(&raw),
                recovery_notice,
            })
        })
    }

    fn mutate_layout<T>(
        &self,
        expected_token: &str,
        recover: bool,
        edit: impl FnOnce(&mut LayoutProfiles) -> LayoutResult<T>,
    ) -> Result<T> {
        let committed = self.inner.db.write(|tx| -> Result<T> {
            let raw = repos::settings::get_raw(tx, SETTINGS_KEY)?;
            if token(&raw) != expected_token {
                return Err(Error::validation(
                    "Layout changed elsewhere. Reload before saving.",
                ));
            }
            let (mut profiles, notice) = decode(&raw);
            if notice.is_some() && !recover {
                return Err(Error::validation(
                    "Reset saved layouts explicitly before changing recovered data.",
                ));
            }
            let result = edit(&mut profiles).map_err(|e| Error::validation(e.to_string()))?;
            profiles
                .validate()
                .map_err(|e| Error::validation(e.to_string()))?;
            let value =
                serde_json::to_value(profiles).map_err(|e| Error::internal(e.to_string()))?;
            repos::settings::set_json(tx, SETTINGS_KEY, &value)?;
            Ok(result)
        })?;
        if committed.changed() {
            self.inner.bus.publish(ApplicationEvent::StateChanged {
                revision: committed.revision,
                changes: committed.events.clone().into(),
            });
        }
        Ok(committed.value)
    }

    pub fn create_layout_profile(&self, name: &str, expected_token: &str) -> Result<String> {
        self.mutate_layout(expected_token, false, |p| p.create(name))
    }

    pub fn rename_layout_profile(&self, id: &str, name: &str, expected_token: &str) -> Result<()> {
        self.mutate_layout(expected_token, false, |p| p.rename(id, name))
    }

    pub fn duplicate_layout_profile(
        &self,
        id: &str,
        name: &str,
        expected_token: &str,
    ) -> Result<String> {
        self.mutate_layout(expected_token, false, |p| p.duplicate(id, name))
    }

    pub fn delete_layout_profile(&self, id: &str, expected_token: &str) -> Result<()> {
        self.mutate_layout(expected_token, false, |p| p.delete(id))
    }

    pub fn activate_layout_profile(&self, id: &str, expected_token: &str) -> Result<()> {
        self.mutate_layout(expected_token, false, |p| p.activate(id))
    }

    pub fn save_layout_profile(&self, profile: LayoutProfile, expected_token: &str) -> Result<()> {
        self.mutate_layout(expected_token, false, |p| p.replace(profile))
    }

    pub fn reset_layout_profile(&self, id: &str, expected_token: &str) -> Result<()> {
        self.mutate_layout(expected_token, false, |p| p.reset(id))
    }

    pub fn reset_all_layout_profiles(&self, expected_token: &str) -> Result<()> {
        self.mutate_layout(expected_token, true, |p| {
            *p = LayoutProfiles::default();
            Ok(())
        })
    }
}
