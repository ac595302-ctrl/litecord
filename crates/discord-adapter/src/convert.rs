//! Conversion from `discord-ffi`'s plain C-ABI mirror types into Litecord
//! domain types.
//!
//! This module is **always compiled** (unlike `social_sdk`, which is gated on
//! the `discord-social-sdk` feature) because `discord-ffi` is a non-optional,
//! always-plain-Rust dependency of this crate: with the SDK feature off,
//! `LcUser`/`LcMessage`/`LcStr` are just `#[repr(C)]` structs with no native
//! code behind them, so nothing here needs the feature, and these converters
//! can be unit-tested without any native SDK present.
//!
//! Neither converter uses `unsafe`: `discord_ffi::LcStr::to_owned_string` is
//! itself a safe function (see its doc comment) — this crate, like everything
//! above `discord-ffi`, never contains `unsafe` code (workspace lint
//! `unsafe_code = "deny"`).

use std::sync::Arc;

use litecord_types::ids::{MessageId, UserId};
use litecord_types::social::{Message, User};
use litecord_types::Timestamp;

/// Converts a native user record into a Litecord [`User`].
///
/// Returns `None` when the record has no id at all (id `0` never denotes a
/// real Discord snowflake — Discord's epoch starts well after the Unix
/// epoch) or no username, since a `User` cannot meaningfully exist without
/// either.
pub fn convert_user(raw: &discord_ffi::LcUser<'_>) -> Option<User> {
    if raw.id == 0 {
        return None;
    }
    let username = raw.username.to_owned_string()?;
    if username.is_empty() {
        return None;
    }
    let global_name = raw.global_name.to_owned_string().filter(|s| !s.is_empty());
    let avatar_url = raw.avatar_url.to_owned_string().filter(|s| !s.is_empty());

    Some(User {
        id: UserId(raw.id),
        username: Arc::from(username.as_str()),
        global_name: global_name.map(|s| Arc::from(s.as_str())),
        avatar_url: avatar_url.map(|s| Arc::from(s.as_str())),
        is_bot: false,
        is_provisional: raw.is_provisional != 0,
    })
}

/// Converts a native message record into a Litecord [`Message`].
///
/// Returns `None` when the record has no id, no channel id, or the content
/// pointer was null (as opposed to merely empty, which is a valid — if
/// unusual — message body, e.g. an attachment-only message).
pub fn convert_message(raw: &discord_ffi::LcMessage<'_>) -> Option<Message> {
    if raw.id == 0 || raw.channel_id == 0 {
        return None;
    }
    let content = raw.content.to_owned_string()?;
    let edited_at = if raw.edited_at_ms == 0 {
        None
    } else {
        Some(Timestamp::from_millis(raw.edited_at_ms))
    };

    Some(Message {
        id: MessageId(raw.id),
        conversation_id: raw.channel_id.into(),
        author_id: UserId(raw.author_id),
        content: Arc::from(content.as_str()),
        sent_at: Timestamp::from_millis(raw.sent_at_ms),
        edited_at,
        reply_to: None,
        extras: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use discord_ffi::{LcMessage, LcStr, LcUser};

    fn user_with<'a>(
        id: u64,
        username: &'a str,
        global_name: LcStr<'a>,
        avatar_url: LcStr<'a>,
    ) -> LcUser<'a> {
        LcUser {
            id,
            username: LcStr::from_bytes(username.as_bytes()),
            global_name,
            avatar_url,
            is_provisional: 0,
        }
    }

    #[test]
    fn converts_full_user() {
        let raw = user_with(
            42,
            "ada",
            LcStr::from_bytes(b"Ada Lovelace"),
            LcStr::from_bytes(b"https://example.invalid/a.png"),
        );
        let user = convert_user(&raw).expect("valid user");
        assert_eq!(user.id, UserId(42));
        assert_eq!(user.username.as_ref(), "ada");
        assert_eq!(user.global_name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(
            user.avatar_url.as_deref(),
            Some("https://example.invalid/a.png")
        );
        assert!(!user.is_provisional);
    }

    #[test]
    fn null_optional_strings_become_none() {
        let raw = user_with(42, "ada", LcStr::NULL, LcStr::NULL);
        let user = convert_user(&raw).expect("valid user");
        assert_eq!(user.global_name, None);
        assert_eq!(user.avatar_url, None);
    }

    #[test]
    fn empty_optional_strings_become_none() {
        let raw = user_with(42, "ada", LcStr::from_bytes(b""), LcStr::from_bytes(b""));
        let user = convert_user(&raw).expect("valid user");
        assert_eq!(user.global_name, None);
        assert_eq!(user.avatar_url, None);
    }

    #[test]
    fn zero_id_user_is_rejected() {
        let raw = user_with(0, "ada", LcStr::NULL, LcStr::NULL);
        assert!(convert_user(&raw).is_none());
    }

    #[test]
    fn null_username_is_rejected() {
        let raw = LcUser {
            id: 1,
            username: LcStr::NULL,
            global_name: LcStr::NULL,
            avatar_url: LcStr::NULL,
            is_provisional: 0,
        };
        assert!(convert_user(&raw).is_none());
    }

    #[test]
    fn lossy_utf8_in_username_is_replaced_not_rejected() {
        let raw = user_with(1, "ok", LcStr::NULL, LcStr::NULL);
        // sanity: normal path still works with a valid-UTF8 username
        assert!(convert_user(&raw).is_some());

        let bad = LcUser {
            id: 1,
            username: LcStr::from_bytes(&[0x68, 0x69, 0xff]),
            global_name: LcStr::NULL,
            avatar_url: LcStr::NULL,
            is_provisional: 1,
        };
        let user = convert_user(&bad).expect("lossy but present username still converts");
        assert!(user.username.contains('\u{FFFD}'));
        assert!(user.is_provisional);
    }

    #[test]
    fn converts_message_with_edit() {
        let raw = LcMessage {
            id: 7,
            channel_id: 99,
            author_id: 42,
            sent_at_ms: 1_000,
            edited_at_ms: 2_000,
            content: LcStr::from_bytes(b"hello"),
        };
        let msg = convert_message(&raw).expect("valid message");
        assert_eq!(msg.id, MessageId(7));
        assert_eq!(msg.conversation_id, 99u64.into());
        assert_eq!(msg.author_id, UserId(42));
        assert_eq!(msg.content.as_ref(), "hello");
        assert_eq!(msg.sent_at, Timestamp::from_millis(1_000));
        assert_eq!(msg.edited_at, Some(Timestamp::from_millis(2_000)));
    }

    #[test]
    fn edited_at_zero_means_never_edited() {
        let raw = LcMessage {
            id: 7,
            channel_id: 99,
            author_id: 42,
            sent_at_ms: 1_000,
            edited_at_ms: 0,
            content: LcStr::from_bytes(b"hello"),
        };
        let msg = convert_message(&raw).expect("valid message");
        assert_eq!(msg.edited_at, None);
    }

    #[test]
    fn null_content_is_rejected() {
        let raw = LcMessage {
            id: 7,
            channel_id: 99,
            author_id: 42,
            sent_at_ms: 1_000,
            edited_at_ms: 0,
            content: LcStr::NULL,
        };
        assert!(convert_message(&raw).is_none());
    }

    #[test]
    fn zero_ids_are_rejected() {
        let raw = LcMessage {
            id: 0,
            channel_id: 99,
            author_id: 42,
            sent_at_ms: 1_000,
            edited_at_ms: 0,
            content: LcStr::from_bytes(b"hello"),
        };
        assert!(convert_message(&raw).is_none());
    }
}
