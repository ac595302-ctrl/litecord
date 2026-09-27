//! Pure REST request builders and response parsers for Discord API v10.
//!
//! Every function here builds a [`RestRequest`] describing what to send, or
//! parses a [`serde_json::Value`] already received — it never performs an
//! HTTP call. A transport layer (added separately, alongside the bot token)
//! sends the request and hands the JSON body back to the `parse_*`
//! functions.

use serde_json::{json, Value};

use litecord_core::ports::BackendError;
use litecord_types::ids::{ChannelId, GuildId, MessageId};
use litecord_types::social::{Channel, Guild, Message};
use litecord_types::DurationMs;

use super::translate::{self, TranslateError};

/// The base URL for Discord's REST API, version 10.
pub const API_BASE: &str = "https://discord.com/api/v10";

/// HTTP method for a [`RestRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

/// A fully described REST request: nothing here has been sent yet.
#[derive(Debug, Clone, PartialEq)]
pub struct RestRequest {
    pub method: Method,
    /// Path relative to [`API_BASE`], including any query string.
    pub path: String,
    /// JSON request body, when the method takes one.
    pub body: Option<Value>,
    /// A route template (ids replaced by placeholders) used as the rate
    /// limit bucket hint, per Discord's per-route (not per-id) bucketing.
    pub route: &'static str,
}

/// `GET /users/@me` — the bot's own user object.
pub fn current_user() -> RestRequest {
    RestRequest {
        method: Method::Get,
        path: "/users/@me".to_string(),
        body: None,
        route: "GET /users/@me",
    }
}

/// `GET /users/@me/guilds` — guilds the bot is a member of.
pub fn current_user_guilds() -> RestRequest {
    RestRequest {
        method: Method::Get,
        path: "/users/@me/guilds".to_string(),
        body: None,
        route: "GET /users/@me/guilds",
    }
}

/// `GET /guilds/{guild.id}/channels`.
pub fn guild_channels(guild: GuildId) -> RestRequest {
    RestRequest {
        method: Method::Get,
        path: format!("/guilds/{guild}/channels"),
        body: None,
        route: "GET /guilds/{guild.id}/channels",
    }
}

/// `GET /channels/{channel.id}/messages`. `limit` is clamped to `1..=100`
/// (Discord's own bounds); `before` paginates backwards from a message id.
pub fn channel_messages(channel: ChannelId, limit: u32, before: Option<MessageId>) -> RestRequest {
    channel_messages_page(channel, limit, before, None)
}

/// `GET /channels/{channel.id}/messages` with a paging cursor: `before`
/// walks backwards, `after` catches up forwards. Discord accepts only one
/// cursor per request, so when both are given only `after` is sent (callers
/// filter the `before` bound client-side). `limit` is clamped to `1..=100`.
pub fn channel_messages_page(
    channel: ChannelId,
    limit: u32,
    before: Option<MessageId>,
    after: Option<MessageId>,
) -> RestRequest {
    let limit = limit.clamp(1, 100);
    let mut path = format!("/channels/{channel}/messages?limit={limit}");
    match (before, after) {
        (_, Some(after)) => path.push_str(&format!("&after={after}")),
        (Some(before), None) => path.push_str(&format!("&before={before}")),
        (None, None) => {}
    }
    RestRequest {
        method: Method::Get,
        path,
        body: None,
        route: "GET /channels/{channel.id}/messages",
    }
}

/// `POST /channels/{channel.id}/messages`. `allowed_mentions` is always
/// empty so an agent-authored message can never mass-ping anyone.
pub fn create_message(channel: ChannelId, content: &str) -> RestRequest {
    RestRequest {
        method: Method::Post,
        path: format!("/channels/{channel}/messages"),
        body: Some(json!({
            "content": content,
            "allowed_mentions": { "parse": [] },
        })),
        route: "POST /channels/{channel.id}/messages",
    }
}

/// `POST /channels/{channel.id}/messages` as a reply. `fail_if_not_exists`
/// is false so a reply to a since-deleted message still posts, as in the
/// Discord client.
pub fn create_reply(channel: ChannelId, content: &str, reply_to: MessageId) -> RestRequest {
    RestRequest {
        method: Method::Post,
        path: format!("/channels/{channel}/messages"),
        body: Some(json!({
            "content": content,
            "allowed_mentions": { "parse": [], "replied_user": false },
            "message_reference": {
                "message_id": reply_to.to_string(),
                "channel_id": channel.to_string(),
                "fail_if_not_exists": false,
            },
        })),
        route: "POST /channels/{channel.id}/messages",
    }
}

/// `PATCH /channels/{channel.id}/messages/{message.id}`.
pub fn edit_message(channel: ChannelId, message: MessageId, content: &str) -> RestRequest {
    RestRequest {
        method: Method::Patch,
        path: format!("/channels/{channel}/messages/{message}"),
        body: Some(json!({ "content": content })),
        route: "PATCH /channels/{channel.id}/messages/{message.id}",
    }
}

/// `DELETE /channels/{channel.id}/messages/{message.id}`.
pub fn delete_message(channel: ChannelId, message: MessageId) -> RestRequest {
    RestRequest {
        method: Method::Delete,
        path: format!("/channels/{channel}/messages/{message}"),
        body: None,
        route: "DELETE /channels/{channel.id}/messages/{message.id}",
    }
}

/// `GET /gateway/bot` — the recommended gateway URL and shard count.
pub fn gateway_bot() -> RestRequest {
    RestRequest {
        method: Method::Get,
        path: "/gateway/bot".to_string(),
        body: None,
        route: "GET /gateway/bot",
    }
}

/// Parses a `GET /channels/{channel.id}/messages` response body.
///
/// Discord returns messages newest-first; this returns them chronologically
/// (oldest first, by id — snowflakes are time-ordered), matching
/// [`litecord_core::ports::SocialBackend::messages`]. Sorting rather than
/// reversing keeps the order right whatever order an `after=` query uses.
pub fn parse_messages(v: &Value) -> Result<Vec<Message>, TranslateError> {
    let array = v.as_array().ok_or(TranslateError::Invalid("messages"))?;
    let mut messages = array
        .iter()
        .map(translate::message)
        .collect::<Result<Vec<_>, _>>()?;
    messages.sort_by_key(|m| m.id);
    Ok(messages)
}

/// Parses a `GET /users/@me/guilds` response body.
pub fn parse_guilds(v: &Value) -> Result<Vec<Guild>, TranslateError> {
    let array = v.as_array().ok_or(TranslateError::Invalid("guilds"))?;
    array.iter().map(translate::guild).collect()
}

/// Parses a `GET /guilds/{guild.id}/channels` response body, skipping any
/// DM-type entries (which should not appear in this endpoint, but are
/// tolerated defensively).
pub fn parse_channels(v: &Value, guild: GuildId) -> Result<Vec<Channel>, TranslateError> {
    let array = v.as_array().ok_or(TranslateError::Invalid("channels"))?;
    let mut channels = Vec::with_capacity(array.len());
    for raw in array {
        if let Some(c) = translate::channel(raw, guild)? {
            channels.push(c);
        }
    }
    Ok(channels)
}

/// Parses a `GET /gateway/bot` response body into a connectable gateway URL
/// (Discord's `url` field plus the required `v`/`encoding` query params).
pub fn parse_gateway_url(v: &Value) -> Result<String, TranslateError> {
    let url = v
        .get("url")
        .and_then(Value::as_str)
        .ok_or(TranslateError::Missing("url"))?;
    Ok(gateway_connect_url(url))
}

/// Appends the required `v`/`encoding` query to a gateway base URL. Discord
/// requires it on both the `/gateway/bot` URL and READY's
/// `resume_gateway_url`. URLs that already carry a query are left alone.
pub fn gateway_connect_url(base: &str) -> String {
    if base.contains('?') {
        return base.to_owned();
    }
    format!("{}/?v=10&encoding=json", base.trim_end_matches('/'))
}

/// Maps an HTTP error response to a [`BackendError`].
///
/// Messages are always sanitized: they never include the request path, body,
/// headers or any token — only the status code and, for `429`, a retry
/// delay. `retry_after_header` is the `Retry-After` header value in seconds,
/// used when the body has no `retry_after` field of its own.
pub fn error_for_status(
    status: u16,
    body: Option<&Value>,
    retry_after_header: Option<f64>,
) -> BackendError {
    match status {
        429 => {
            let seconds = body
                .and_then(|b| b.get("retry_after"))
                .and_then(Value::as_f64)
                .or(retry_after_header)
                .unwrap_or(0.0)
                .max(0.0);
            let millis = (seconds * 1000.0).round() as u64;
            BackendError::RateLimited {
                retry_after: DurationMs::from_millis(millis),
            }
        }
        401 | 403 => {
            BackendError::Authentication("discord rejected the bot's credentials".to_string())
        }
        404 => BackendError::NotFound {
            what: "discord resource".to_string(),
        },
        _ => BackendError::Sdk(format!("discord http {status}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_user_and_guilds_paths() {
        assert_eq!(current_user().path, "/users/@me");
        assert_eq!(current_user().method, Method::Get);
        assert_eq!(current_user_guilds().path, "/users/@me/guilds");
    }

    #[test]
    fn guild_channels_path() {
        assert_eq!(guild_channels(GuildId(42)).path, "/guilds/42/channels");
    }

    #[test]
    fn channel_messages_clamps_limit_and_includes_before() {
        let req = channel_messages(ChannelId(1), 500, Some(MessageId(9)));
        assert_eq!(req.path, "/channels/1/messages?limit=100&before=9");

        let req = channel_messages(ChannelId(1), 0, None);
        assert_eq!(req.path, "/channels/1/messages?limit=1");
    }

    #[test]
    fn channel_messages_page_query() {
        let c = ChannelId(7);
        assert_eq!(
            channel_messages_page(c, 50, None, None).path,
            "/channels/7/messages?limit=50"
        );
        assert_eq!(
            channel_messages_page(c, 50, Some(MessageId(3)), None).path,
            "/channels/7/messages?limit=50&before=3"
        );
        assert_eq!(
            channel_messages_page(c, 200, None, Some(MessageId(4))).path,
            "/channels/7/messages?limit=100&after=4"
        );
        // Discord takes one cursor: `after` wins.
        let req = channel_messages_page(c, 10, Some(MessageId(9)), Some(MessageId(4)));
        assert_eq!(req.path, "/channels/7/messages?limit=10&after=4");
        assert_eq!(req.route, "GET /channels/{channel.id}/messages");
        assert_eq!(req.method, Method::Get);
    }

    #[test]
    fn parse_messages_sorts_ascending_whatever_the_input_order() {
        let v = json!([
            { "id": "2", "channel_id": "1", "author": { "id": "9" }, "content": "b",
              "timestamp": "2026-01-01T00:00:02Z" },
            { "id": "3", "channel_id": "1", "author": { "id": "9" }, "content": "c",
              "timestamp": "2026-01-01T00:00:03Z" },
            { "id": "1", "channel_id": "1", "author": { "id": "9" }, "content": "a",
              "timestamp": "2026-01-01T00:00:01Z" },
        ]);
        let ids: Vec<u64> = parse_messages(&v)
            .unwrap()
            .iter()
            .map(|m| m.id.get())
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn create_message_body_forbids_mass_pings() {
        let req = create_message(ChannelId(1), "hi @everyone");
        assert_eq!(req.method, Method::Post);
        assert_eq!(req.path, "/channels/1/messages");
        let body = req.body.unwrap();
        assert_eq!(body["content"], "hi @everyone");
        assert_eq!(body["allowed_mentions"]["parse"], json!([]));
    }

    #[test]
    fn create_reply_references_the_message_without_pinging() {
        let req = create_reply(ChannelId(1), "ok", MessageId(9));
        assert_eq!(req.path, "/channels/1/messages");
        let body = req.body.unwrap();
        assert_eq!(body["message_reference"]["message_id"], "9");
        assert_eq!(body["message_reference"]["channel_id"], "1");
        assert_eq!(body["allowed_mentions"]["parse"], json!([]));
        assert_eq!(body["allowed_mentions"]["replied_user"], false);
    }

    #[test]
    fn edit_and_delete_message_paths() {
        let edit = edit_message(ChannelId(1), MessageId(2), "new content");
        assert_eq!(edit.method, Method::Patch);
        assert_eq!(edit.path, "/channels/1/messages/2");
        assert_eq!(edit.body.unwrap()["content"], "new content");

        let delete = delete_message(ChannelId(1), MessageId(2));
        assert_eq!(delete.method, Method::Delete);
        assert_eq!(delete.path, "/channels/1/messages/2");
        assert_eq!(delete.body, None);
    }

    #[test]
    fn gateway_bot_path() {
        assert_eq!(gateway_bot().path, "/gateway/bot");
    }

    #[test]
    fn parse_messages_reverses_to_chronological_order() {
        let v = json!([
            { "id": "3", "channel_id": "1", "author": { "id": "9" }, "content": "c",
              "timestamp": "2026-01-01T00:00:03Z" },
            { "id": "2", "channel_id": "1", "author": { "id": "9" }, "content": "b",
              "timestamp": "2026-01-01T00:00:02Z" },
            { "id": "1", "channel_id": "1", "author": { "id": "9" }, "content": "a",
              "timestamp": "2026-01-01T00:00:01Z" },
        ]);
        let messages = parse_messages(&v).unwrap();
        let ids: Vec<u64> = messages.iter().map(|m| m.id.get()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn parse_guilds_and_channels() {
        let guilds = json!([{ "id": "1", "name": "g" }]);
        assert_eq!(parse_guilds(&guilds).unwrap().len(), 1);

        let channels = json!([
            { "id": "10", "type": 0, "name": "general" },
            { "id": "11", "type": 1, "name": "dm-should-be-skipped" },
        ]);
        let parsed = parse_channels(&channels, GuildId(1)).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, ChannelId(10));
    }

    #[test]
    fn parse_gateway_url_appends_query() {
        let v = json!({ "url": "wss://gateway.discord.gg", "shards": 1 });
        assert_eq!(
            parse_gateway_url(&v).unwrap(),
            "wss://gateway.discord.gg/?v=10&encoding=json"
        );
        assert_eq!(
            gateway_connect_url("wss://gateway-us-east1-b.discord.gg/"),
            "wss://gateway-us-east1-b.discord.gg/?v=10&encoding=json"
        );
        assert_eq!(gateway_connect_url("wss://x/?v=10"), "wss://x/?v=10");
    }

    #[test]
    fn error_for_status_429_uses_body_retry_after() {
        let body = json!({ "retry_after": 1.5 });
        let err = error_for_status(429, Some(&body), None);
        assert_eq!(
            err,
            BackendError::RateLimited {
                retry_after: DurationMs::from_millis(1500)
            }
        );
    }

    #[test]
    fn error_for_status_429_falls_back_to_header() {
        let err = error_for_status(429, None, Some(2.0));
        assert_eq!(
            err,
            BackendError::RateLimited {
                retry_after: DurationMs::from_millis(2000)
            }
        );
    }

    #[test]
    fn error_for_status_401_and_403_are_authentication_and_sanitized() {
        let body = json!({ "message": "invalid token totally-secret-token-value" });
        for status in [401, 403] {
            let err = error_for_status(status, Some(&body), None);
            assert!(matches!(err, BackendError::Authentication(_)));
            let text = err.to_string();
            assert!(!text.contains("totally-secret-token-value"));
            assert!(!text.contains("token"));
        }
    }

    #[test]
    fn error_for_status_404_is_not_found() {
        assert!(matches!(
            error_for_status(404, None, None),
            BackendError::NotFound { .. }
        ));
    }

    #[test]
    fn error_for_status_5xx_and_other_4xx_are_sdk_and_retryable() {
        let err = error_for_status(503, None, None);
        assert!(matches!(err, BackendError::Sdk(_)));
        assert!(err.is_retryable());

        let err = error_for_status(400, None, None);
        assert!(matches!(err, BackendError::Sdk(_)));
    }
}
