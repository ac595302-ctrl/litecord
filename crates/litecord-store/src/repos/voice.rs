//! Local (single-row) voice session state.

use rusqlite::{params, Connection, OptionalExtension};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;
use litecord_types::social::{VoiceParticipant, VoiceState};
use litecord_types::{LobbyId, Timestamp};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::sql::json_err;

/// Replace the single voice-state row. Emits
/// [`UnifiedEvent::VoiceStateChanged`] if anything changed.
pub fn set(
    tx: &WriteTx<'_>,
    voice: &VoiceState,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let participants_json = serde_json::to_string(&voice.participants)?;
    let n = tx.execute(
        "INSERT INTO voice_state (id, connected, lobby_id, muted, deafened, input_device, output_device, output_volume, noise_suppression, push_to_talk, participants_json, observed_at, revision)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(id) DO UPDATE SET
             connected = excluded.connected,
             lobby_id = excluded.lobby_id,
             muted = excluded.muted,
             deafened = excluded.deafened,
             input_device = excluded.input_device,
             output_device = excluded.output_device,
             output_volume = excluded.output_volume,
             noise_suppression = excluded.noise_suppression,
             push_to_talk = excluded.push_to_talk,
             participants_json = excluded.participants_json,
             observed_at = excluded.observed_at,
             revision = excluded.revision
         WHERE voice_state.connected IS NOT excluded.connected
            OR voice_state.lobby_id IS NOT excluded.lobby_id
            OR voice_state.muted IS NOT excluded.muted
            OR voice_state.deafened IS NOT excluded.deafened
            OR voice_state.input_device IS NOT excluded.input_device
            OR voice_state.output_device IS NOT excluded.output_device
            OR voice_state.output_volume IS NOT excluded.output_volume
            OR voice_state.noise_suppression IS NOT excluded.noise_suppression
            OR voice_state.push_to_talk IS NOT excluded.push_to_talk
            OR voice_state.participants_json IS NOT excluded.participants_json",
        params![
            voice.connected,
            voice.lobby_id.map(|l| l.to_sql()),
            voice.muted,
            voice.deafened,
            voice.input_device,
            voice.output_device,
            voice.output_volume,
            voice.noise_suppression,
            voice.push_to_talk,
            participants_json,
            observed_at.as_millis(),
            tx.revision().get() as i64,
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(UnifiedEvent::VoiceStateChanged, origin)?;
    }
    Ok(changed)
}

/// `(connected, lobby_id, muted, deafened, input_device, output_device,
/// output_volume, noise_suppression, push_to_talk, participants_json)`.
type VoiceRow = (
    bool,
    Option<i64>,
    bool,
    bool,
    Option<String>,
    Option<String>,
    f32,
    bool,
    bool,
    String,
);

/// Read the current voice state, if it has ever been set.
pub fn get(conn: &Connection) -> StoreResult<Option<VoiceState>> {
    let row: Option<VoiceRow> = conn
        .query_row(
            "SELECT connected, lobby_id, muted, deafened, input_device, output_device, output_volume, noise_suppression, push_to_talk, participants_json
             FROM voice_state WHERE id = 1",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            },
        )
        .optional()?;

    let Some((
        connected,
        lobby_id,
        muted,
        deafened,
        input_device,
        output_device,
        output_volume,
        noise_suppression,
        push_to_talk,
        participants_json,
    )) = row
    else {
        return Ok(None);
    };

    let participants: Vec<VoiceParticipant> =
        serde_json::from_str(&participants_json).map_err(|e| json_err("voice_state", e))?;

    Ok(Some(VoiceState {
        connected,
        lobby_id: lobby_id.map(LobbyId::from_sql),
        muted,
        deafened,
        input_device,
        output_device,
        output_volume,
        noise_suppression,
        push_to_talk,
        participants,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn set_then_get_roundtrips() {
        let db = Database::open_in_memory().unwrap();
        let voice = VoiceState {
            connected: true,
            ..VoiceState::default()
        };
        let c = db
            .write(|tx| set(tx, &voice, Origin::Synthetic, Timestamp::from_millis(1)))
            .unwrap();
        assert!(c.changed());
        let got = db.read(|r| get(r)).unwrap().unwrap();
        assert!(got.connected);
    }
}
