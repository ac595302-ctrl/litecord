//! Environment for harness sidecars.
//!
//! Children start from an empty environment plus an allowlist, so Litecord
//! secrets (`LITECORD_BOT_TOKEN`, anything `DISCORD_*`) can never reach the
//! harness or the tools it runs. Provider keys the harness itself uses
//! (`OPENAI_API_KEY`, ...) are the user's harness credentials and pass
//! through unchanged; Litecord never reads them.

use std::ffi::{OsStr, OsString};

/// Exact names passed through.
const EXACT: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "USERNAME",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "TERM",
    "TZ",
    "TMPDIR",
    "TEMP",
    "TMP",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    // Windows essentials.
    "SystemRoot",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "ProgramData",
    "ProgramFiles",
    "HOMEDRIVE",
    "HOMEPATH",
];

/// Prefixes passed through: locale, XDG dirs, and the harnesses' own
/// configuration and provider credentials.
const PREFIXES: &[&str] = &[
    "LC_",
    "XDG_",
    "CODEX_",
    "OPENAI_",
    "OPENCODE_",
    "ANTHROPIC_",
    "GEMINI_",
    "GOOGLE_",
    "AZURE_",
    "AWS_",
    "OPENROUTER_",
    "GROQ_",
    "MISTRAL_",
    "DEEPSEEK_",
    "XAI_",
];

/// Never passed, whatever the allowlist says.
const DENY_PREFIXES: &[&str] = &["LITECORD_", "DISCORD_"];

pub fn allowed(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if DENY_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return false;
    }
    EXACT.contains(&name) || PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Filters `vars` (normally `std::env::vars_os()`) down to the allowlist.
pub fn filtered(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    vars.into_iter().filter(|(k, _)| allowed(k)).collect()
}

/// The sidecar environment for this process, with `PATH` widened to the
/// folders Litecord searched (see [`crate::driver::search_path`]).
pub fn child_env() -> Vec<(OsString, OsString)> {
    let mut env = filtered(std::env::vars_os());
    if let Ok(path) = std::env::join_paths(crate::driver::search_path()) {
        env.retain(|(k, _)| k != "PATH");
        env.push((OsString::from("PATH"), path));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_pass() {
        let vars = [
            ("LITECORD_BOT_TOKEN", "t"),
            ("DISCORD_TOKEN", "t"),
            ("PATH", "/usr/bin"),
            ("OPENAI_API_KEY", "k"),
            ("LC_ALL", "C"),
            ("AWS_SECRET_ACCESS_KEY", "k"),
            ("RANDOM_THING", "x"),
            ("GITHUB_TOKEN", "x"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let kept: Vec<String> = filtered(vars)
            .into_iter()
            .map(|(k, _)| k.into_string().unwrap_or_default())
            .collect();
        assert_eq!(
            kept,
            ["PATH", "OPENAI_API_KEY", "LC_ALL", "AWS_SECRET_ACCESS_KEY"]
        );
    }
}
