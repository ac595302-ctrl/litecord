//! Experimental, read-only account source. Authentication is explicitly
//! supplied by the account owner, never obtained from another application.
//! This is not an OAuth/Social SDK backend and has no user-account writes.
mod backend;
pub mod translate;
pub use backend::UserSessionBackend;
// Socket and request shapes are source-neutral despite their historical module
// location. User auth/header and Identify are distinct from the bot path.
#[cfg(feature = "discord-user-session")]
pub use crate::bot::http::HttpTransport;
pub use crate::bot::transport::{BotTransport as SessionTransport, GatewaySocket, SocketEvent};
