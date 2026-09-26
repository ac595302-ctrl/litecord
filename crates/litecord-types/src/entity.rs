//! Entity references for the unified-memory graph.
//!
//! An [`EntityId`] can name a Discord object (by snowflake), a Litecord-owned
//! object (by local id), or a local graph-only entity such as a topic. The
//! canonical string encoding is `kind:id` (e.g. `user:80351110224678912`,
//! `topic:12`), which is what is stored in SQLite and shown to agents.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ids::*;
use crate::ValidationError;

/// Kinds of graph-only entities that Litecord creates itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalEntityKind {
    Topic,
    Project,
    Person,
    Other,
    /// An Omni session (the source of Omni-derived memories).
    OmniSession,
}

impl LocalEntityKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            LocalEntityKind::Topic => "topic",
            LocalEntityKind::Project => "project",
            LocalEntityKind::Person => "person",
            LocalEntityKind::Other => "entity",
            LocalEntityKind::OmniSession => "omni_session",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntityId {
    User(UserId),
    Guild(GuildId),
    Channel(ChannelId),
    Conversation(ConversationId),
    Message(MessageId),
    Lobby(LobbyId),
    Memory(MemoryId),
    Task(TaskId),
    Reminder(ReminderId),
    Local(LocalEntityKind, LocalEntityId),
}

impl EntityId {
    pub fn kind_str(&self) -> &'static str {
        match self {
            EntityId::User(_) => "user",
            EntityId::Guild(_) => "guild",
            EntityId::Channel(_) => "channel",
            EntityId::Conversation(_) => "conversation",
            EntityId::Message(_) => "message",
            EntityId::Lobby(_) => "lobby",
            EntityId::Memory(_) => "memory",
            EntityId::Task(_) => "task",
            EntityId::Reminder(_) => "reminder",
            EntityId::Local(k, _) => k.as_str(),
        }
    }

    /// True for objects whose existence is asserted by Discord itself.
    pub fn is_discord_object(&self) -> bool {
        matches!(
            self,
            EntityId::User(_)
                | EntityId::Guild(_)
                | EntityId::Channel(_)
                | EntityId::Conversation(_)
                | EntityId::Message(_)
                | EntityId::Lobby(_)
        )
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = self.kind_str();
        match self {
            EntityId::User(id) => write!(f, "{kind}:{id}"),
            EntityId::Guild(id) => write!(f, "{kind}:{id}"),
            EntityId::Channel(id) => write!(f, "{kind}:{id}"),
            EntityId::Conversation(id) => write!(f, "{kind}:{id}"),
            EntityId::Message(id) => write!(f, "{kind}:{id}"),
            EntityId::Lobby(id) => write!(f, "{kind}:{id}"),
            EntityId::Memory(id) => write!(f, "{kind}:{id}"),
            EntityId::Task(id) => write!(f, "{kind}:{id}"),
            EntityId::Reminder(id) => write!(f, "{kind}:{id}"),
            EntityId::Local(_, id) => write!(f, "{kind}:{id}"),
        }
    }
}

impl fmt::Debug for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EntityId({self})")
    }
}

impl FromStr for EntityId {
    type Err = ValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ValidationError::Parse {
            what: "EntityId",
            input: s.to_owned(),
        };
        let (kind, raw) = s.split_once(':').ok_or_else(err)?;
        Ok(match kind {
            "user" => EntityId::User(raw.parse()?),
            "guild" => EntityId::Guild(raw.parse()?),
            "channel" => EntityId::Channel(raw.parse()?),
            "conversation" => EntityId::Conversation(raw.parse()?),
            "message" => EntityId::Message(raw.parse()?),
            "lobby" => EntityId::Lobby(raw.parse()?),
            "memory" => EntityId::Memory(raw.parse()?),
            "task" => EntityId::Task(raw.parse()?),
            "reminder" => EntityId::Reminder(raw.parse()?),
            "topic" => EntityId::Local(LocalEntityKind::Topic, raw.parse()?),
            "project" => EntityId::Local(LocalEntityKind::Project, raw.parse()?),
            "person" => EntityId::Local(LocalEntityKind::Person, raw.parse()?),
            "entity" => EntityId::Local(LocalEntityKind::Other, raw.parse()?),
            "omni_session" => EntityId::Local(LocalEntityKind::OmniSession, raw.parse()?),
            _ => return Err(err()),
        })
    }
}

impl Serialize for EntityId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EntityId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_id_string_roundtrip() {
        for e in [
            EntityId::User(UserId(99)),
            EntityId::Conversation(ConversationId(5)),
            EntityId::Local(LocalEntityKind::Topic, LocalEntityId(3)),
            EntityId::Task(TaskId(1)),
        ] {
            let s = e.to_string();
            assert_eq!(s.parse::<EntityId>().unwrap(), e);
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<EntityId>(&json).unwrap(), e);
        }
        assert!("nope:1".parse::<EntityId>().is_err());
        assert!("user".parse::<EntityId>().is_err());
    }
}
