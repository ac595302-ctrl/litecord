//! Owner-operated Discord sign-in in a new, isolated WebView2 profile.
//! The page performs authentication. No requests, challenge answers, or
//! credentials from other browser profiles are manufactured or imported.
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use litecord_core::secrets::Secret;
use wry::{WebContext, WebView, WebViewBuilder};
use zeroize::Zeroizing;

pub const HEADER_HEIGHT: f64 = 96.0;
const LIFETIME: Duration = Duration::from_secs(600);
const MAX_CREDENTIAL: usize = 2048;

fn discord_origin(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("discord.com")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
    })
}

fn navigation_allowed(value: &str) -> bool {
    discord_origin(value)
        || url::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && url.port_or_known_default() == Some(443)
                && url.username().is_empty()
                && url.password().is_none()
                && url
                    .host_str()
                    .is_some_and(|host| host == "hcaptcha.com" || host.ends_with(".hcaptcha.com"))
        })
}

fn candidate<'a>(uri: &str, body: &'a str, capability: &str) -> Option<&'a str> {
    if !discord_origin(uri) || body.len() > capability.len() + MAX_CREDENTIAL {
        return None;
    }
    let value = body.strip_prefix(capability)?;
    (value.len() >= 16
        && value.len() <= MAX_CREDENTIAL
        && value.bytes().all(|b| b.is_ascii_graphic())
        && !value.starts_with("Bot ")
        && !value.starts_with("Bearer "))
    .then_some(value)
}

// Drop the webview before its context and temporary profile.
pub struct LoginView {
    view: WebView,
    _context: WebContext,
    _profile: tempfile::TempDir,
    credentials: Receiver<Zeroizing<String>>,
    opened: Instant,
}

impl std::fmt::Debug for LoginView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginView").finish_non_exhaustive()
    }
}

impl LoginView {
    pub fn open(frame: &eframe::Frame, ctx: &eframe::egui::Context) -> Result<Self, String> {
        let profile = tempfile::Builder::new()
            .prefix("litecord-signin-")
            .tempdir()
            .map_err(|_| "Could not create the temporary sign-in profile.".to_owned())?;
        let mut context = WebContext::new(Some(profile.path().to_owned()));
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random)
            .map_err(|_| "Could not initialize the private sign-in handoff.".to_owned())?;
        let capability = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
            + ":";
        let script = include_str!("browser_login.js").replace("__LOGIN_CAPABILITY__", &capability);
        let (sender, credentials) = mpsc::sync_channel(1);
        let wake = ctx.clone();
        let opened = Instant::now();
        let view = WebViewBuilder::new_with_web_context(&mut context)
            .with_url("https://discord.com/login")
            .with_incognito(true)
            .with_devtools(false)
            .with_autoplay(false)
            .with_initialization_script_for_main_only(script, true)
            .with_navigation_handler(|address| navigation_allowed(&address))
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_download_started_handler(|_, _| false)
            .with_permission_handler(|_| wry::PermissionResponse::Deny)
            .with_ipc_handler(move |request| {
                if opened.elapsed() >= LIFETIME {
                    return;
                }
                let uri = request.uri().to_string();
                let body = Zeroizing::new(request.into_body());
                if let Some(value) = candidate(&uri, &body, &capability) {
                    if sender.try_send(Zeroizing::new(value.to_owned())).is_ok() {
                        wake.request_repaint();
                    }
                }
            })
            .with_bounds(bounds(ctx))
            .build_as_child(frame)
            .map_err(|_| "Could not open Discord sign-in. Check that Microsoft Edge WebView2 Runtime is installed.".to_owned())?;
        Ok(Self {
            view,
            _context: context,
            _profile: profile,
            credentials,
            opened,
        })
    }

    pub fn credential(&self) -> Option<Secret<String>> {
        if self.expired() {
            return None;
        }
        self.credentials
            .try_recv()
            .ok()
            .map(|value| Secret::new(value.to_string()))
    }

    pub fn expired(&self) -> bool {
        self.opened.elapsed() >= LIFETIME
    }

    pub fn resize(&self, ctx: &eframe::egui::Context) {
        let _ = self.view.set_bounds(bounds(ctx));
    }
}

fn bounds(ctx: &eframe::egui::Context) -> wry::Rect {
    let size = ctx.input(|i| {
        i.viewport()
            .inner_rect
            .map(|r| r.size())
            .unwrap_or(eframe::egui::vec2(900.0, 700.0))
    });
    let scale = f64::from(ctx.pixels_per_point());
    wry::Rect {
        position: wry::dpi::PhysicalPosition::new(0, (HEADER_HEIGHT * scale) as i32).into(),
        size: wry::dpi::PhysicalSize::new(
            (f64::from(size.x) * scale) as u32,
            ((f64::from(size.y) - HEADER_HEIGHT).max(1.0) * scale) as u32,
        )
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_origin_capability_and_unbounded_credentials() {
        let good = "cap:abcdefghijklmnop";
        assert!(candidate("https://discord.com/login", good, "cap:").is_some());
        for uri in [
            "http://discord.com",
            "https://discord.com.evil.test",
            "https://hcaptcha.com",
            "https://discord.com:444",
            "https://user@discord.com",
        ] {
            assert!(candidate(uri, good, "cap:").is_none());
        }
        for body in [
            "other:abcdefghijklmnop",
            "cap:short",
            "cap:abcdefghijklmnop\n",
            "cap:Bearer abcdefghijklmnop",
        ] {
            assert!(candidate("https://discord.com", body, "cap:").is_none());
        }
        assert!(candidate(
            "https://discord.com",
            &("cap:".to_owned() + &"a".repeat(2049)),
            "cap:"
        )
        .is_none());
    }

    #[test]
    fn challenge_navigation_is_limited_to_real_https_provider_hosts() {
        assert!(navigation_allowed("https://newassets.hcaptcha.com/a"));
        for uri in [
            "https://hcaptcha.com.evil.test",
            "http://hcaptcha.com",
            "file:///tmp/login.html",
            "https://example.com",
            "https://user@hcaptcha.com",
        ] {
            assert!(!navigation_allowed(uri));
        }
    }
}
