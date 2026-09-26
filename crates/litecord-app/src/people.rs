//! Account and contact snapshots for workspace identity surfaces.

use litecord_core::Result;
use litecord_store::repos;
use litecord_types::ids::UserId;
use litecord_types::provenance::Origin;
use litecord_types::social::{Presence, PresenceStatus};
use litecord_types::Revision;
use serde::Serialize;

use crate::app::LitecordApp;

/// The signed-in user displayed in the shared application shell.
#[derive(Debug, Clone, Serialize)]
pub struct AccountViewModel {
    pub as_of_revision: Revision,
    pub user_id: Option<UserId>,
    pub display_name: String,
    pub username: Option<String>,
    pub avatar_url: Option<String>,
    pub origin: Option<Origin>,
}

/// A contact profile and its local annotations for an inspector or profile view.
#[derive(Debug, Clone, Serialize)]
pub struct ContactViewModel {
    pub as_of_revision: Revision,
    pub user_id: UserId,
    pub display_name: String,
    pub username: Option<String>,
    pub avatar_url: Option<String>,
    pub presence: Presence,
    pub alias: Option<String>,
    pub note: Option<String>,
    pub favorite: bool,
    pub origin: Origin,
    /// True when only a reference to this user is known, without a profile.
    pub is_stub: bool,
}

impl LitecordApp {
    /// Loads the current user from the active user account.
    ///
    /// The not-signed-in state is represented explicitly; no demo persona or
    /// fallback self identity is invented when the account is absent.
    pub fn account_view(&self) -> Result<AccountViewModel> {
        self.inner.db.read(|conn| -> Result<AccountViewModel> {
            let account = repos::accounts::current_user(conn)?;
            let Some(account) = account else {
                return Ok(AccountViewModel {
                    as_of_revision: conn.revision(),
                    user_id: None,
                    display_name: "Not signed in".into(),
                    username: None,
                    avatar_url: None,
                    origin: None,
                });
            };

            let profile = repos::users::get(conn, account.user_id)?;
            let profile = profile.filter(|user| !user.is_stub);
            let (display_name, username, avatar_url) = match profile {
                Some(user) => (
                    user.user.display_name().to_owned(),
                    Some(user.user.username.to_string()),
                    user.user.avatar_url.map(|url| url.to_string()),
                ),
                None => ("Unknown user".into(), None, None),
            };

            Ok(AccountViewModel {
                as_of_revision: conn.revision(),
                user_id: Some(account.user_id),
                display_name,
                username,
                avatar_url,
                origin: Some(account.origin),
            })
        })
    }

    /// Loads a known contact, returning `None` when no user record exists.
    ///
    /// Stub rows are kept visible as "Unknown user" with an explicit `is_stub`
    /// flag, rather than displaying a made-up profile name.
    pub fn contact_view(&self, user_id: UserId) -> Result<Option<ContactViewModel>> {
        self.inner
            .db
            .read(|conn| -> Result<Option<ContactViewModel>> {
                let Some(user) = repos::users::get(conn, user_id)? else {
                    return Ok(None);
                };
                let note = repos::notes::get_note(conn, user_id)?;
                let (display_name, username, avatar_url) = if user.is_stub {
                    ("Unknown user".to_owned(), None, None)
                } else {
                    (
                        user.user.display_name().to_owned(),
                        Some(user.user.username.to_string()),
                        user.user.avatar_url.map(|url| url.to_string()),
                    )
                };

                Ok(Some(ContactViewModel {
                    as_of_revision: conn.revision(),
                    user_id,
                    display_name,
                    username,
                    avatar_url,
                    presence: if user.is_stub {
                        Presence {
                            status: PresenceStatus::Unknown,
                            activity: None,
                        }
                    } else {
                        user.presence
                    },
                    alias: note.as_ref().and_then(|note| note.alias.clone()),
                    note: note.as_ref().and_then(|note| note.note.clone()),
                    favorite: note.is_some_and(|note| note.favorite),
                    origin: user.origin,
                    is_stub: user.is_stub,
                }))
            })
    }
}
