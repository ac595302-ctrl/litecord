//! One data folder per Discord account.
//!
//! A database belongs to the account that first signed in to it (mixing two
//! accounts' messages would be wrong), so signing in with another account
//! used to fail with "this database belongs to a different account" and no
//! way forward. The default account folder now holds one sub-folder per
//! extra account, plus an `active` file naming the one to open. Switching
//! restarts the app, since the database is opened at startup.
//!
//! Layout under the account root (e.g. `…/Litecord/account`):
//! * `litecord.db` — the original account (kept where it always was);
//! * `a<millis>/litecord.db` — each added account;
//! * `active` — the sub-folder to open (absent: the root itself);
//! * `<folder>/account-name` — a label for the account list.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const ACTIVE: &str = "active";
const LABEL: &str = "account-name";
const DB: &str = "litecord.db";

/// The data folder to open under `root`.
pub fn resolve(root: &Path) -> PathBuf {
    std::fs::read_to_string(root.join(ACTIVE))
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| valid_name(name) && root.join(name).is_dir())
        .map_or_else(|| root.to_path_buf(), |name| root.join(name))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// One saved account folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub dir: PathBuf,
    pub label: String,
    pub active: bool,
}

/// Account folders, and a flag the launcher reads to restart after a switch.
#[derive(Debug, Clone)]
pub struct AccountSlots {
    root: PathBuf,
    active: PathBuf,
    restart: Arc<AtomicBool>,
}

impl AccountSlots {
    pub fn new(root: PathBuf, active: PathBuf) -> Self {
        Self {
            root,
            active,
            restart: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether a switch asked for the app to be started again.
    pub fn restart_requested(&self) -> bool {
        self.restart.load(Ordering::Acquire)
    }

    /// Saved accounts, the root folder first.
    pub fn list(&self) -> Vec<Slot> {
        let mut dirs = vec![self.root.clone()];
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            let mut subs: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_dir()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(valid_name)
                })
                .collect();
            subs.sort();
            dirs.extend(subs);
        }
        dirs.into_iter()
            .filter(|d| d.join(DB).is_file() || *d == self.active)
            .map(|dir| Slot {
                label: std::fs::read_to_string(dir.join(LABEL))
                    .ok()
                    .map(|l| l.trim().to_owned())
                    .filter(|l| !l.is_empty())
                    .unwrap_or_else(|| "Account not signed in yet".to_owned()),
                active: dir == self.active,
                dir,
            })
            .collect()
    }

    /// Remember the signed-in account's name for the list.
    pub fn remember_label(&self, label: &str) {
        let label = label.trim();
        if label.is_empty() {
            return;
        }
        let path = self.active.join(LABEL);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(label) {
            let _ = std::fs::write(path, label);
        }
    }

    /// Open `dir` on the next start and ask for a restart.
    pub fn switch_to(&self, dir: &Path) -> std::io::Result<()> {
        if dir == self.root {
            match std::fs::remove_file(self.root.join(ACTIVE)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        } else {
            let name = dir
                .strip_prefix(&self.root)
                .ok()
                .and_then(|p| p.to_str())
                .filter(|n| valid_name(n))
                .ok_or_else(|| std::io::Error::other("not an account folder"))?;
            std::fs::write(self.root.join(ACTIVE), name)?;
        }
        self.restart.store(true, Ordering::Release);
        Ok(())
    }

    /// Create an empty folder for another account and switch to it.
    pub fn add_account(&self) -> std::io::Result<()> {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let dir = self.root.join(format!("a{millis}"));
        std::fs::create_dir_all(&dir)?;
        self.switch_to(&dir)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn switching_between_account_folders() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().to_path_buf();
        std::fs::write(root.join(DB), b"").unwrap();
        assert_eq!(resolve(&root), root);

        let slots = AccountSlots::new(root.clone(), resolve(&root));
        slots.remember_label("first");
        slots.add_account().unwrap();
        assert!(slots.restart_requested());
        let added = resolve(&root);
        assert_ne!(added, root);
        assert!(added.starts_with(&root));

        let slots = AccountSlots::new(root.clone(), added.clone());
        let list = slots.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].label, "first");
        assert!(list[1].active);

        slots.switch_to(&root).unwrap();
        assert_eq!(resolve(&root), root);
    }

    #[test]
    fn a_bad_active_file_falls_back_to_the_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(ACTIVE), "../elsewhere").unwrap();
        assert_eq!(resolve(root.path()), root.path());
    }
}
