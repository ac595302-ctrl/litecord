//! Experimental account source with typed read/write capability gates. Authentication is explicitly
//! supplied by the account owner, never obtained from another application.
//! This is distinct from an OAuth/Social SDK backend.
mod backend;
pub mod translate;
pub use backend::UserSessionBackend;
// Socket and request shapes are source-neutral despite their historical module
// location. User auth/header and Identify are distinct from the bot path.
#[cfg(feature = "discord-user-session")]
pub use crate::bot::http::HttpTransport;
pub use crate::bot::transport::{BotTransport as SessionTransport, GatewaySocket, SocketEvent};

/// Account transport allowlist: validate actual path segments, never trust route labels.
pub fn allows_request(
    access: litecord_types::capability::SessionAccessMode,
    req: &crate::bot::rest::RestRequest,
) -> bool {
    use crate::bot::rest::Method;
    if req.method == Method::Get {
        return req.path.starts_with('/') && !req.path.contains("..") && !req.path.contains('#');
    }
    if access != litecord_types::capability::SessionAccessMode::ReadWrite {
        return false;
    }
    let parts: Vec<_> = req.path.split('/').collect();
    let id = |v: &str| v.parse::<u64>().is_ok_and(|id| id > 0);
    match (req.method, parts.as_slice()) {
        (Method::Post, ["", "channels", channel, "messages"]) => id(channel),
        (Method::Patch | Method::Delete, ["", "channels", channel, "messages", message]) => {
            id(channel) && id(message)
        }
        (Method::Post, ["", "users", "@me", "channels"]) => true,
        _ => false,
    }
}

#[cfg(test)]
mod access_tests {
    use super::*;
    use crate::bot::rest::{Method, RestRequest};
    use litecord_types::capability::SessionAccessMode;
    #[test]
    fn writes_require_mode_and_numeric_allowlisted_paths() {
        for (method, path) in [
            (Method::Post, "/channels/2/messages"),
            (Method::Patch, "/channels/2/messages/3"),
            (Method::Delete, "/channels/2/messages/3"),
        ] {
            let req = RestRequest {
                method,
                path: path.into(),
                body: None,
                route: "untrusted route label",
            };
            assert!(!allows_request(SessionAccessMode::ReadOnly, &req));
            assert!(allows_request(SessionAccessMode::ReadWrite, &req));
        }
        for path in [
            "/users/@me",
            "/channels/2/messages/../3",
            "/channels/2/messages?x=1",
            "/channels/2/messages/3/pin",
            "/guilds/2",
        ] {
            let req = RestRequest {
                method: Method::Post,
                path: path.into(),
                body: None,
                route: "POST /channels/{channel.id}/messages",
            };
            assert!(!allows_request(SessionAccessMode::ReadWrite, &req));
        }
    }
}
