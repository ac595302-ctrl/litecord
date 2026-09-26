//! Canonical room discovery for workspace navigation.

use litecord_core::Result;
use litecord_store::repos;
use litecord_types::social::Lobby;
use litecord_types::{ConversationId, Revision};
use serde::Serialize;

use crate::app::LitecordApp;

const ROOM_CONVERSATION_LIMIT: u32 = 200;
const ROOM_LIST_LIMIT: u32 = 256;

/// A single known voice lobby together with its canonical message surface,
/// when the store has observed one.
#[derive(Debug, Clone, Serialize)]
pub struct RoomRow {
    pub lobby: Lobby,
    pub title: String,
    pub conversation_id: Option<ConversationId>,
}

/// A point-in-time snapshot of known rooms for a navigation surface.
#[derive(Debug, Clone, Serialize)]
pub struct RoomsViewModel {
    pub as_of_revision: Revision,
    pub rooms: Vec<RoomRow>,
}

impl LitecordApp {
    /// Lists canonical lobbies and associates a recent conversation when its
    /// stored `lobby_id` matches. Display names come from observed data only.
    pub fn rooms_view(&self) -> Result<RoomsViewModel> {
        self.inner.db.read(|conn| -> Result<RoomsViewModel> {
            let lobbies = repos::lobbies::list(conn, ROOM_LIST_LIMIT)?;
            let conversations =
                repos::conversations::list_recent(conn, ROOM_CONVERSATION_LIMIT, 0)?;

            let mut rooms = Vec::with_capacity(lobbies.len());
            for lobby in lobbies {
                let matching = conversations
                    .iter()
                    .find(|record| record.conversation.lobby_id == Some(lobby.id));
                let linked_channel_name = match lobby.linked_channel_id {
                    Some(channel_id) => repos::channels::get(conn, channel_id)?
                        .filter(|channel| !channel.removed)
                        .map(|channel| channel.channel.name.to_string()),
                    None => None,
                };
                let conversation_title = matching
                    .and_then(|record| record.conversation.title.as_deref())
                    .filter(|title| !title.trim().is_empty())
                    .map(str::to_owned);
                let title = conversation_title
                    .or(linked_channel_name)
                    .unwrap_or_else(|| format!("Room {}", lobby.id));

                rooms.push(RoomRow {
                    lobby,
                    title,
                    conversation_id: matching.map(|record| record.conversation.id),
                });
            }

            Ok(RoomsViewModel {
                as_of_revision: conn.revision(),
                rooms,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::time::Duration;

    use discord_adapter::{fixtures, MockBackend};
    use litecord_core::bus::IngestSender;
    use litecord_core::config::LitecordConfig;
    use litecord_core::events::HydrationKey;
    use litecord_core::ports::{BackendResult, SocialBackend};
    use litecord_store::repos;
    use litecord_types::capability::{BackendMode, Capability, CapabilitySet, SupportLevel};
    use litecord_types::ids::{ChannelId, ConversationId, GuildId, LobbyId};
    use litecord_types::provenance::{DiscordSource, Origin};
    use litecord_types::social::{
        Channel, ChannelAccess, ChannelCapabilities, ChannelKind, Conversation, ConversationKind,
        Lobby, User,
    };
    use litecord_types::Timestamp;
    use tokio::time::sleep;

    use crate::LitecordApp;

    #[derive(Debug)]
    struct BackendWithLobby {
        demo: MockBackend,
        lobby: Lobby,
    }

    impl SocialBackend for BackendWithLobby {
        fn source(&self) -> DiscordSource {
            DiscordSource::Synthetic
        }

        fn mode(&self) -> BackendMode {
            BackendMode::Demo
        }

        fn capabilities(&self) -> CapabilitySet {
            CapabilitySet::default()
                .with(Capability::CurrentUser, SupportLevel::Full)
                .with(Capability::Lobbies, SupportLevel::Full)
        }

        fn connect<'life0, 'async_trait>(
            &'life0 self,
            sink: IngestSender,
        ) -> Pin<Box<dyn Future<Output = BackendResult<()>> + Send + 'async_trait>>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.demo.connect(sink).await })
        }

        fn disconnect<'life0, 'async_trait>(
            &'life0 self,
        ) -> Pin<Box<dyn Future<Output = BackendResult<()>> + Send + 'async_trait>>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.demo.disconnect().await })
        }

        fn current_user<'life0, 'async_trait>(
            &'life0 self,
        ) -> Pin<Box<dyn Future<Output = BackendResult<User>> + Send + 'async_trait>>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.demo.current_user().await })
        }

        fn lobby<'life0, 'async_trait>(
            &'life0 self,
            lobby_id: LobbyId,
        ) -> Pin<Box<dyn Future<Output = BackendResult<Lobby>> + Send + 'async_trait>>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                if self.lobby.id == lobby_id {
                    Ok(self.lobby.clone())
                } else {
                    Err(litecord_core::ports::BackendError::NotFound {
                        what: format!("lobby {lobby_id}"),
                    })
                }
            })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rooms_view_uses_hydrated_lobby_and_observed_channel_name() {
        let now = Timestamp::now();
        let data = fixtures::generate(17, now);
        let current_user = data.current_user.clone();
        let lobby = Lobby {
            id: LobbyId(810_017),
            member_ids: vec![current_user.id],
            linked_channel_id: Some(ChannelId(820_017)),
        };
        let channel = Channel {
            id: ChannelId(820_017),
            guild_id: GuildId(830_017),
            name: "session-room".into(),
            kind: ChannelKind::Voice,
            position: 0,
            parent_id: None,
            access: ChannelAccess::Linked,
            capabilities: ChannelCapabilities::empty(),
        };
        let conversation = Conversation {
            id: ConversationId(840_017),
            kind: ConversationKind::Lobby,
            recipient_id: None,
            guild_id: None,
            lobby_id: Some(lobby.id),
            title: None,
            last_message_id: None,
            last_activity_at: Some(now),
        };
        let app = LitecordApp::builder(LitecordConfig::default())
            .backend(Arc::new(BackendWithLobby {
                demo: MockBackend::new(data),
                lobby: lobby.clone(),
            }))
            .in_memory()
            .start()
            .await
            .unwrap();

        app.database()
            .write(|tx| -> litecord_store::StoreResult<()> {
                repos::channels::upsert(tx, &channel, Origin::Synthetic, now)?;
                repos::conversations::upsert(tx, &conversation, Origin::Synthetic, now)?;
                Ok(())
            })
            .unwrap();

        app.refresh(HydrationKey::Lobby { lobby_id: lobby.id });
        let room = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let view = app.rooms_view().unwrap();
                if let Some(room) = view
                    .rooms
                    .into_iter()
                    .find(|room| room.lobby.id == lobby.id)
                {
                    break room;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the requested lobby should hydrate before timeout");

        assert_eq!(room.title, "session-room");
        assert_eq!(room.conversation_id, Some(conversation.id));
        assert_eq!(room.lobby, lobby);
        assert!(app.rooms_view().unwrap().as_of_revision.get() > 0);

        app.shutdown().await;
    }
}
