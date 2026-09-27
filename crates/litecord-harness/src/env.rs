//! Environment for harness sidecars.
//!
//! Children start from an empty environment plus an allowlist, so Litecord
//! secrets (`LITECORD_BOT_TOKEN`, anything `DISCORD_*`) can never reach the
//! harness or the tools it runs. Provider keys the harness itself uses
//! (`OPENAI_API_KEY`, ...) are the user's harness credentials and pass
//! through unchanged; Litecord never reads them.
//!
//! Windows variable names are case-insensitive (`std::env::vars_os()`
//! usually yields `Path`, not `PATH`), so on Windows the allowlist is
//! matched ignoring ASCII case. The deny prefixes are matched ignoring case
//! everywhere.

use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Exact names passed through (ignoring case on Windows).
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
    // Windows: what Node-based CLIs, npm `.cmd` shims and `cmd.exe` need
    // to run and to find per-user and program directories.
    "SystemRoot",
    "SystemDrive",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "OS",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "ALLUSERSPROFILE",
    "PUBLIC",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_ARCHITEW6432",
    "PROCESSOR_IDENTIFIER",
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

/// Never passed, whatever the allowlist says (ignoring case everywhere).
const DENY_PREFIXES: &[&str] = &["LITECORD_", "DISCORD_"];

/// The name of the executable search path variable.
const PATH_VAR: &str = "PATH";

fn has_prefix(name: &str, prefix: &str, ignore_case: bool) -> bool {
    match name.get(..prefix.len()) {
        Some(head) if ignore_case => head.eq_ignore_ascii_case(prefix),
        Some(head) => head == prefix,
        None => false,
    }
}

fn same_name(a: &str, b: &str, windows: bool) -> bool {
    if windows {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Whether `name` may reach a harness on this platform.
pub fn allowed(name: &OsStr) -> bool {
    allowed_on(name, cfg!(windows))
}

/// [`allowed`] with the platform rules chosen explicitly (for tests).
pub fn allowed_on(name: &OsStr, windows: bool) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if DENY_PREFIXES.iter().any(|p| has_prefix(name, p, true)) {
        return false;
    }
    EXACT.iter().any(|e| same_name(name, e, windows))
        || PREFIXES.iter().any(|p| has_prefix(name, p, windows))
}

/// Filters `vars` (normally `std::env::vars_os()`) down to the allowlist.
pub fn filtered(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    filtered_on(vars, cfg!(windows))
}

/// [`filtered`] with the platform rules chosen explicitly (for tests).
pub fn filtered_on(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
    windows: bool,
) -> Vec<(OsString, OsString)> {
    vars.into_iter()
        .filter(|(k, _)| allowed_on(k, windows))
        .collect()
}

/// The sidecar environment for this process.
pub fn child_env() -> Vec<(OsString, OsString)> {
    filtered(std::env::vars_os())
}

/// [`child_env`] with `dir` put first on `PATH` (see
/// [`crate::ResolvedExecutable::bin_dir`]).
pub fn child_env_with_path(dir: Option<&Path>) -> Vec<(OsString, OsString)> {
    let env = child_env();
    match dir {
        Some(dir) => prepend_path(env, dir, cfg!(windows)),
        None => env,
    }
}

/// Puts `dir` first on the `PATH` entry of `env` (matched ignoring case on
/// Windows), adding `PATH` if there is none. A `dir` already on `PATH` is
/// moved to the front rather than repeated.
pub fn prepend_path(
    mut env: Vec<(OsString, OsString)>,
    dir: &Path,
    windows: bool,
) -> Vec<(OsString, OsString)> {
    let pos = env
        .iter()
        .position(|(k, _)| k.to_str().is_some_and(|k| same_name(k, PATH_VAR, windows)));
    let current: Vec<std::path::PathBuf> = pos
        .map(|i| std::env::split_paths(&env[i].1).collect())
        .unwrap_or_default();
    let entries =
        std::iter::once(dir.to_path_buf()).chain(current.into_iter().filter(|p| p != dir));
    let Ok(joined) = std::env::join_paths(entries) else {
        // `dir` cannot be on PATH (it contains the separator); leave PATH be.
        return env;
    };
    match pos {
        Some(i) => env[i].1 = joined,
        None => env.push((OsString::from(PATH_VAR), joined)),
    }
    env
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn names(vars: &[(&str, &str)], windows: bool) -> Vec<String> {
        filtered_on(
            vars.iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v))),
            windows,
        )
        .into_iter()
        .map(|(k, _)| k.into_string().unwrap_or_default())
        .collect()
    }

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
        ];
        for windows in [false, true] {
            assert_eq!(
                names(&vars, windows),
                ["PATH", "OPENAI_API_KEY", "LC_ALL", "AWS_SECRET_ACCESS_KEY"]
            );
        }
    }

    #[test]
    fn deny_prefixes_ignore_case_everywhere() {
        let vars = [
            ("litecord_bot_token", "t"),
            ("Litecord_Data_Dir", "d"),
            ("discord_token", "t"),
            ("Discord_Client_Secret", "t"),
        ];
        assert!(names(&vars, false).is_empty());
        assert!(names(&vars, true).is_empty());
    }

    #[test]
    fn windows_names_match_ignoring_case() {
        // What `vars_os()` typically yields on Windows.
        let vars = [
            ("Path", r"C:\Windows\system32;C:\Program Files\nodejs\"),
            ("PATHEXT", ".COM;.EXE;.BAT;.CMD"),
            ("SystemRoot", r"C:\Windows"),
            ("windir", r"C:\Windows"),
            ("ComSpec", r"C:\Windows\system32\cmd.exe"),
            ("SystemDrive", "C:"),
            ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
            ("CommonProgramFiles", r"C:\Program Files\Common Files"),
            ("NUMBER_OF_PROCESSORS", "8"),
            ("PROCESSOR_ARCHITECTURE", "AMD64"),
            ("OS", "Windows_NT"),
            ("AppData", r"C:\Users\me\AppData\Roaming"),
            ("Openai_Api_Key", "k"),
            ("SESSIONNAME", "Console"),
        ];
        let kept = names(&vars, true);
        assert_eq!(kept.len(), vars.len() - 1, "{kept:?}");
        assert!(!kept.contains(&"SESSIONNAME".to_owned()));
        // On Unix, names are case-sensitive: `Path` is not `PATH`.
        let unix = names(&vars, false);
        assert!(!unix.contains(&"Path".to_owned()));
        assert!(!unix.contains(&"windir".to_owned()));
        assert!(!unix.contains(&"Openai_Api_Key".to_owned()));
    }

    #[test]
    fn prepend_path_replaces_the_existing_entry() {
        let dir = Path::new("/opt/node/bin");
        let env = vec![
            (OsString::from("HOME"), OsString::from("/h")),
            (
                OsString::from("Path"),
                std::env::join_paths(["/usr/bin", "/opt/node/bin"]).unwrap(),
            ),
        ];
        // Windows: `Path` is PATH; `dir` moves to the front, not repeated.
        let out = prepend_path(env.clone(), dir, true);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].0, "Path");
        let entries: Vec<_> = std::env::split_paths(&out[1].1).collect();
        assert_eq!(entries, [Path::new("/opt/node/bin"), Path::new("/usr/bin")]);
        // Unix: `Path` is not PATH, so a PATH entry is added.
        let out = prepend_path(env, dir, false);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2].0, "PATH");
        assert_eq!(out[2].1, OsString::from("/opt/node/bin"));
    }
}
