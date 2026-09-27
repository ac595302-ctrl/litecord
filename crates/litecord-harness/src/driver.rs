//! The one trait every harness implements, plus the launcher that starts
//! its sidecar on demand, and how harness executables are found.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::types::*;

/// A running harness sidecar. All methods are cheap to call concurrently;
/// drivers serialize what the harness requires internally.
#[async_trait]
pub trait HarnessDriver: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> HarnessKind;

    /// Events from the sidecar. Subscribe before calling `send`.
    fn subscribe(&self) -> broadcast::Receiver<HarnessEvent>;

    async fn login_state(&self) -> HarnessResult<LoginState>;
    /// Starts the harness's own sign-in; returns `SigningIn { url }`.
    /// Completion arrives as `HarnessEvent::Login`.
    async fn begin_login(&self) -> HarnessResult<LoginState>;
    /// Signs out of the harness (for OpenCode: every stored provider
    /// credential). The resulting state also arrives as `HarnessEvent::Login`.
    async fn logout(&self) -> HarnessResult<()>;

    /// Signs out of one provider, for harnesses that hold several.
    async fn logout_provider(&self, provider: &str) -> HarnessResult<()> {
        let _ = provider;
        self.logout().await
    }

    /// Abandons a browser sign-in in progress and frees its localhost
    /// callback. `Unsupported` means only restarting the sidecar cancels it.
    async fn cancel_login(&self) -> HarnessResult<()> {
        Ok(())
    }

    /// Sign-in methods the harness offers (browser OAuth, API key, ...).
    /// The default is one browser method that maps to [`Self::begin_login`].
    async fn login_options(&self) -> HarnessResult<Vec<LoginOption>> {
        Ok(vec![LoginOption::new(
            "default",
            "Sign in with browser",
            LoginKind::Browser,
        )])
    }

    /// Start a specific browser sign-in method from [`Self::login_options`].
    async fn begin_login_with(&self, option: &str) -> HarnessResult<LoginState> {
        let _ = option;
        self.begin_login().await
    }

    /// [`Self::begin_login_with`] plus values for the option's
    /// [`LoginOption::prompts`].
    async fn begin_login_with_inputs(
        &self,
        option: &str,
        inputs: &LoginInputs,
    ) -> HarnessResult<LoginState> {
        let _ = inputs;
        self.begin_login_with(option).await
    }

    /// Finish a sign-in that needs a code pasted from the browser
    /// (`SigningIn { needs_code: true }`).
    async fn submit_login_code(&self, code: &str) -> HarnessResult<()> {
        let _ = code;
        Err(HarnessError::Unsupported("pasted sign-in codes"))
    }

    /// Sign in with an API key. The key goes straight to the harness, which
    /// stores it in its own credential store; Litecord keeps no copy.
    async fn login_api_key(&self, option: &str, key: &str) -> HarnessResult<()> {
        let _ = (option, key);
        Err(HarnessError::Unsupported("API key sign-in"))
    }

    /// [`Self::login_api_key`] plus values for the option's
    /// [`LoginOption::prompts`].
    async fn login_api_key_with(
        &self,
        option: &str,
        key: &str,
        inputs: &LoginInputs,
    ) -> HarnessResult<()> {
        let _ = inputs;
        self.login_api_key(option, key).await
    }

    /// Models the harness can use, as it names them (a bare id for Codex,
    /// `provider/model` for OpenCode). Empty when the harness can't list
    /// them.
    async fn models(&self) -> HarnessResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn start_session(&self, cfg: &SessionConfig) -> HarnessResult<ExternalId>;
    /// Reopen a session after a sidecar restart. Drivers that cannot resume
    /// return `Unsupported`; the service then starts a fresh session.
    async fn resume_session(&self, id: &str, cfg: &SessionConfig) -> HarnessResult<ExternalId>;
    async fn send(&self, session: &str, text: &str) -> HarnessResult<()>;
    async fn interrupt(&self, session: &str) -> HarnessResult<()>;
    async fn compact(&self, session: &str) -> HarnessResult<()>;
    async fn answer(&self, request_id: &str, decision: Decision) -> HarnessResult<()>;

    /// Stop the sidecar. Idempotent.
    async fn shutdown(&self);
}

/// Paths and launch details the app gives a launcher.
#[derive(Debug, Clone)]
pub struct LaunchContext {
    /// The Omni workspace directory (cwd for the harness).
    pub workspace: PathBuf,
    /// Litecord's data directory; must not be exposed to the harness.
    pub protected_dir: PathBuf,
    pub mcp: McpLaunch,
}

/// Detects and starts one harness.
#[async_trait]
pub trait HarnessLauncher: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> HarnessKind;
    /// Where the harness binary is, if installed (no process is started).
    /// Looked up afresh on every call, so an install made while Litecord
    /// runs is picked up.
    fn executable(&self) -> Option<PathBuf> {
        None
    }
    /// Whether the harness binary is available (no process is started).
    fn installed(&self) -> bool {
        self.executable().is_some()
    }
    async fn launch(&self, ctx: &LaunchContext) -> HarnessResult<Arc<dyn HarnessDriver>>;
}

// ---- Finding harness executables ------------------------------------------

/// Finds `name` on `PATH`, then in the usual per-user install directories
/// (npm/pnpm/bun global bins, Homebrew, the OpenCode installer, nvm), so a
/// harness is found even when the app did not inherit the shell's `PATH`
/// (a macOS app started from Finder, a Windows `Path` changed by an
/// installer after Litecord started). `explicit` wins when set.
pub fn find_executable(name: &str, explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return is_executable(p).then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH");
    search_executable(name, path.as_deref(), &install_dirs(), cfg!(windows))
}

/// [`find_executable`] with every input explicit. `windows` selects the
/// Windows extension order (`.exe`, then `.cmd`, then `.bat`).
pub fn search_executable(
    name: &str,
    path: Option<&OsStr>,
    extra: &[PathBuf],
    windows: bool,
) -> Option<PathBuf> {
    let exts: &[&str] = if windows {
        &["exe", "cmd", "bat"]
    } else {
        &[""]
    };
    let dirs: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    for dir in dirs.iter().chain(extra) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in exts {
            let mut candidate = dir.join(name);
            if !ext.is_empty() {
                candidate.set_extension(ext);
            }
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// A regular file the OS will run (on Unix: an execute bit is set).
pub fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Per-user and package-manager bin directories searched after `PATH`.
pub fn install_dirs() -> Vec<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Some(appdata) = var("APPDATA") {
            dirs.push(appdata.join("npm"));
        }
        if let Some(local) = var("LOCALAPPDATA") {
            dirs.push(local.join("pnpm"));
            dirs.push(local.join("Volta").join("bin"));
        }
        if let Some(home) = var("USERPROFILE") {
            dirs.push(home.join(".opencode").join("bin"));
            dirs.push(home.join(".bun").join("bin"));
            dirs.push(home.join("scoop").join("shims"));
        }
        if let Some(pf) = var("ProgramFiles") {
            dirs.push(pf.join("nodejs"));
        }
        return dirs;
    }
    if let Some(home) = var("HOME") {
        dirs.push(home.join(".opencode").join("bin"));
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".npm-global").join("bin"));
        dirs.push(home.join(".bun").join("bin"));
        dirs.push(home.join(".volta").join("bin"));
        dirs.push(home.join(".local").join("share").join("pnpm"));
        dirs.push(home.join("Library").join("pnpm"));
        // nvm: every installed Node version, highest name first.
        let nvm = home.join(".nvm").join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(&nvm) {
            let mut versions: Vec<PathBuf> =
                entries.flatten().map(|e| e.path().join("bin")).collect();
            versions.sort();
            versions.reverse();
            dirs.extend(versions);
        }
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// How to start a harness binary that may be an npm shim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExecutable {
    /// What to spawn: the native binary when the npm shim could be seen
    /// through, else the executable that was found.
    pub program: PathBuf,
    /// Directory of the executable that was found. Prepended to the
    /// child's `PATH` so a `#!/usr/bin/env node` shim (or the `node` lookup
    /// in an npm `.cmd`) finds the Node that installed it.
    pub bin_dir: Option<PathBuf>,
    /// The npm package root, when the executable belongs to one.
    pub package_root: Option<PathBuf>,
}

impl ResolvedExecutable {
    /// Spawn `found` as is.
    pub fn plain(found: PathBuf) -> Self {
        Self {
            bin_dir: found.parent().map(Path::to_path_buf),
            program: found,
            package_root: None,
        }
    }
}

/// The directory of the npm package `package` that `found` (a symlinked
/// bin, a Windows `.cmd`/`.ps1` shim, or a copied Unix shim) belongs to.
pub fn npm_package_root(found: &Path, package: &str) -> Option<PathBuf> {
    let rel: PathBuf = package.split('/').collect();
    let marker = Path::new("node_modules").join(&rel);
    // A symlinked bin (`<prefix>/bin/codex -> ../lib/node_modules/...`).
    if let Ok(real) = std::fs::canonicalize(found) {
        let mut dir = real.parent();
        while let Some(d) = dir {
            if d.ends_with(&marker) {
                return Some(d.to_path_buf());
            }
            dir = d.parent();
        }
    }
    // Windows npm: `%APPDATA%\npm\codex.cmd` next to `node_modules\`;
    // a Unix prefix: `<prefix>/bin/x` beside `<prefix>/lib/node_modules/`.
    let parent = found.parent()?;
    [
        parent.join(&marker),
        parent.join("..").join("lib").join(&marker),
    ]
    .into_iter()
    .find(|p| p.join("package.json").is_file())
}

/// Spawn settings shared by every harness child: no console window on
/// Windows (a GUI app has none, so an npm `.cmd` shim or the harness itself
/// would otherwise pop one up for as long as it runs).
#[cfg(any(feature = "codex", feature = "opencode"))]
pub fn hide_console(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("litecord-driver-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(p: &Path, exec: bool) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if exec { 0o755 } else { 0o644 };
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = exec;
    }

    #[test]
    fn search_path_then_install_dirs() {
        let root = temp_dir("search");
        let (a, b, extra) = (root.join("a"), root.join("b"), root.join("extra"));
        touch(&extra.join("tool"), true);
        let path = std::env::join_paths([&a, &b]).unwrap();
        let extras = std::slice::from_ref(&extra);
        // Only in an install dir: found there.
        assert_eq!(
            search_executable("tool", Some(&path), extras, false),
            Some(extra.join("tool"))
        );
        // PATH wins over install dirs.
        touch(&b.join("tool"), true);
        assert_eq!(
            search_executable("tool", Some(&path), extras, false),
            Some(b.join("tool"))
        );
        assert_eq!(
            search_executable("missing", Some(&path), extras, false),
            None
        );
        assert_eq!(
            search_executable("tool", None, extras, false),
            Some(extra.join("tool"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_files_are_skipped() {
        let root = temp_dir("noexec");
        touch(&root.join("a").join("tool"), false);
        touch(&root.join("b").join("tool"), true);
        let path = std::env::join_paths([root.join("a"), root.join("b")]).unwrap();
        assert_eq!(
            search_executable("tool", Some(&path), &[], false),
            Some(root.join("b").join("tool"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn windows_extension_order_prefers_exe_then_cmd() {
        let root = temp_dir("winext");
        touch(&root.join("tool.cmd"), true);
        let path = std::env::join_paths([&root]).unwrap();
        assert_eq!(
            search_executable("tool", Some(&path), &[], true),
            Some(root.join("tool.cmd"))
        );
        touch(&root.join("tool.exe"), true);
        assert_eq!(
            search_executable("tool", Some(&path), &[], true),
            Some(root.join("tool.exe"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn npm_roots_for_windows_shim_and_unix_symlink() {
        let root = temp_dir("npmroot");
        // Windows layout: %APPDATA%\npm\codex.cmd + node_modules\@openai\codex.
        let npm = root.join("npm");
        touch(&npm.join("codex.cmd"), true);
        let pkg = npm.join("node_modules").join("@openai").join("codex");
        touch(&pkg.join("package.json"), false);
        assert_eq!(
            npm_package_root(&npm.join("codex.cmd"), "@openai/codex"),
            Some(pkg)
        );
        assert_eq!(
            npm_package_root(&npm.join("codex.cmd"), "opencode-ai"),
            None
        );
        #[cfg(unix)]
        {
            // Unix prefix: bin/codex -> ../lib/node_modules/@openai/codex/bin/codex.js
            let prefix = root.join("prefix");
            let pkg = prefix.join("lib/node_modules/@openai/codex");
            touch(&pkg.join("bin/codex.js"), true);
            std::fs::create_dir_all(prefix.join("bin")).unwrap();
            std::os::unix::fs::symlink(
                "../lib/node_modules/@openai/codex/bin/codex.js",
                prefix.join("bin/codex"),
            )
            .unwrap();
            assert_eq!(
                npm_package_root(&prefix.join("bin/codex"), "@openai/codex"),
                Some(std::fs::canonicalize(&pkg).unwrap())
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
