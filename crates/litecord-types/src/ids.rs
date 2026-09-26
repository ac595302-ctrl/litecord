//! Strongly typed identifiers.
//!
//! Two families exist:
//!
//! * **Discord snowflakes** (`u64`) for objects that originate in Discord.
//! * **Local ids** (`i64`, SQLite row ids) for objects Litecord owns
//!   (memory items, tasks, reminders, actions, ...).
//!
//! Snowflakes serialize as JSON *strings* (as Discord itself does) so that
//! JavaScript-based agents/UIs never lose precision. Local ids serialize as
//! numbers.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ValidationError;

macro_rules! snowflake_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u64);

        impl $name {
            pub const fn new(raw: u64) -> Self { Self(raw) }
            pub const fn get(self) -> u64 { self.0 }
            /// SQLite stores integers as `i64`. Snowflakes are < 2^63 in practice,
            /// and the cast is a lossless bit reinterpretation either way.
            pub const fn to_sql(self) -> i64 { self.0 as i64 }
            pub const fn from_sql(raw: i64) -> Self { Self(raw as u64) }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = ValidationError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.trim().parse::<u64>().map(Self).map_err(|_| ValidationError::Parse {
                    what: stringify!($name),
                    input: s.to_owned(),
                })
            }
        }

        impl From<u64> for $name {
            fn from(v: u64) -> Self { Self(v) }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                #[derive(Deserialize)]
                #[serde(untagged)]
                enum Raw { Num(u64), Str(String) }
                match Raw::deserialize(d)? {
                    Raw::Num(n) => Ok(Self(n)),
                    Raw::Str(s) => s.parse().map_err(serde::de::Error::custom),
                }
            }
        }
    };
}

macro_rules! local_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl $name {
            pub const fn new(raw: i64) -> Self { Self(raw) }
            pub const fn get(self) -> i64 { self.0 }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = ValidationError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.trim().parse::<i64>().map(Self).map_err(|_| ValidationError::Parse {
                    what: stringify!($name),
                    input: s.to_owned(),
                })
            }
        }
    };
}

snowflake_id!(
    /// A Discord user (including provisional accounts and bots).
    UserId
);
snowflake_id!(
    /// A Discord guild ("server").
    GuildId
);
snowflake_id!(
    /// A Discord guild channel.
    ChannelId
);
snowflake_id!(
    /// A conversation: the Discord channel id of a DM, group DM, lobby or
    /// linked/native guild channel. Messages are always keyed by conversation.
    ConversationId
);
snowflake_id!(
    /// A Discord message.
    MessageId
);
snowflake_id!(
    /// A Social SDK lobby.
    LobbyId
);

local_id!(
    /// A unified-memory item.
    MemoryId
);
local_id!(
    /// A local (non-Discord) entity such as a topic or project.
    LocalEntityId
);
local_id!(
    /// A task.
    TaskId
);
local_id!(
    /// A reminder.
    ReminderId
);
local_id!(
    /// An action proposal handled by the Action Engine.
    ActionId
);
local_id!(
    /// A recorded agent run.
    AgentRunId
);
local_id!(
    /// A local message draft.
    DraftId
);
local_id!(
    /// A local account record (one per signed-in Discord identity).
    AccountId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snowflakes_serialize_as_strings_and_accept_numbers() {
        let id = UserId(1_234_567_890_123_456_789);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"1234567890123456789\"");
        let back: UserId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
        let from_num: UserId = serde_json::from_str("42").unwrap();
        assert_eq!(from_num, UserId(42));
    }

    #[test]
    fn local_ids_are_transparent_numbers() {
        let id = TaskId(7);
        assert_eq!(serde_json::to_string(&id).unwrap(), "7");
    }

    #[test]
    fn sql_roundtrip_preserves_high_bits() {
        let id = MessageId(u64::MAX - 5);
        assert_eq!(MessageId::from_sql(id.to_sql()), id);
    }
}
