//! Local records of which Discord identities have signed in.

use rusqlite::{params, Connection, OptionalExtension};

use litecord_types::provenance::{DiscordIdentity, Origin};
use litecord_types::{AccountId, Timestamp, UserId};

use crate::db::WriteTx;
use crate::error::{StoreError, StoreResult};
use crate::sql::col_err;

/// A local account record: one per signed-in Discord identity.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AccountRecord {
    pub id: AccountId,
    pub user_id: UserId,
    pub identity: DiscordIdentity,
    pub origin: Origin,
    pub is_current: bool,
    pub created_at: Timestamp,
    pub last_seen_at: Timestamp,
}

/// `DiscordIdentity` has no stable string form in `litecord-types`; this
/// mirrors the one the handoff spec asks for ("user_social_sdk" /
/// "application_bot") without modifying that (out-of-scope) crate.
fn identity_as_str(identity: DiscordIdentity) -> &'static str {
    match identity {
        DiscordIdentity::UserSocialSdk => "user_social_sdk",
        DiscordIdentity::ApplicationBot => "application_bot",
    }
}

fn identity_parse(idx: usize, s: &str) -> rusqlite::Result<DiscordIdentity> {
    match s {
        "user_social_sdk" => Ok(DiscordIdentity::UserSocialSdk),
        "application_bot" => Ok(DiscordIdentity::ApplicationBot),
        other => Err(col_err(
            idx,
            StoreError::corrupt("accounts", format!("unknown identity {other:?}")),
        )),
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AccountRecord> {
    let id = AccountId(row.get(0)?);
    let user_id = UserId::from_sql(row.get(1)?);
    let identity_s: String = row.get(2)?;
    let identity = identity_parse(2, &identity_s)?;
    let origin_s: String = row.get(3)?;
    let origin = crate::sql::origin(3, &origin_s)?;
    let is_current: bool = row.get::<_, i64>(4)? != 0;
    let created_at = Timestamp::from_millis(row.get(5)?);
    let last_seen_at = Timestamp::from_millis(row.get(6)?);
    Ok(AccountRecord {
        id,
        user_id,
        identity,
        origin,
        is_current,
        created_at,
        last_seen_at,
    })
}

/// Record `user_id` as the current account for `identity`, demoting any
/// other account under the same identity. No events (accounts are local
/// bookkeeping, not unified-memory state).
pub fn upsert_current(
    tx: &WriteTx<'_>,
    user_id: UserId,
    identity: DiscordIdentity,
    origin: Origin,
) -> StoreResult<AccountId> {
    let now = tx.now().as_millis();
    let identity_s = identity_as_str(identity);
    tx.execute(
        "UPDATE accounts SET is_current = 0 WHERE identity = ?1 AND user_id != ?2",
        params![identity_s, user_id.to_sql()],
    )?;
    tx.execute(
        "INSERT INTO accounts (user_id, identity, origin, is_current, created_at, last_seen_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(user_id, identity) DO UPDATE SET
             origin = excluded.origin,
             is_current = 1,
             last_seen_at = excluded.last_seen_at",
        params![user_id.to_sql(), identity_s, origin.as_str(), now],
    )?;
    let id: i64 = tx.query_row(
        "SELECT id FROM accounts WHERE user_id = ?1 AND identity = ?2",
        params![user_id.to_sql(), identity_s],
        |r| r.get(0),
    )?;
    Ok(AccountId(id))
}

/// The current account for `identity`, if any.
pub fn current(conn: &Connection, identity: DiscordIdentity) -> StoreResult<Option<AccountRecord>> {
    Ok(conn
        .query_row(
            "SELECT id, user_id, identity, origin, is_current, created_at, last_seen_at
             FROM accounts WHERE identity = ?1 AND is_current = 1 LIMIT 1",
            params![identity_as_str(identity)],
            map_row,
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn upsert_current_demotes_previous() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            upsert_current(
                tx,
                UserId(1),
                DiscordIdentity::UserSocialSdk,
                Origin::Synthetic,
            )
        })
        .unwrap();
        db.write(|tx| {
            upsert_current(
                tx,
                UserId(2),
                DiscordIdentity::UserSocialSdk,
                Origin::Synthetic,
            )
        })
        .unwrap();
        let cur = db
            .read(|r| current(r, DiscordIdentity::UserSocialSdk))
            .unwrap()
            .unwrap();
        assert_eq!(cur.user_id, UserId(2));
    }
}
