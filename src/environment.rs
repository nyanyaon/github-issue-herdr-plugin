//! The environment description the viewer is constructed from.
//!
//! Everything the process learns from outside itself — the workspace directory
//! to resolve the repo root from, the GraphQL endpoint, the token — is gathered
//! here, once, at startup. Tests build an [`Environment`] literally instead of
//! exporting variables, which is what makes the app seam drivable.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{Config, ConfigWarning};
use crate::identity::SlugOverrides;

/// The GraphQL endpoint, unless [`Environment::GRAPHQL_URL_VAR`] overrides it.
pub const DEFAULT_GRAPHQL_URL: &str = "https://api.github.com/graphql";

#[derive(Debug, Clone)]
pub struct Environment {
    /// The workspace's directory. The repo root is resolved from it — by asking
    /// herdr when there is a socket, by `git` when there is not.
    ///
    /// A pane command's working directory is the plugin root, never the repo, so
    /// this comes from `HERDR_PLUGIN_CONTEXT_JSON` when herdr launched us and
    /// from the process working directory when it did not.
    pub workspace_cwd: PathBuf,
    /// `HERDR_SOCKET_PATH`, or `None` when the viewer was not launched by herdr.
    ///
    /// With no socket the repo root comes from `git` alone, which is both the
    /// fallback real users hit outside herdr and what makes the app seam
    /// drivable without one.
    pub herdr_socket: Option<PathBuf>,
    /// The workspace the pane lives in, fixed for the process lifetime. It is
    /// what `worktree.list` is asked about.
    pub workspace_id: Option<String>,
    /// `HERDR_PLUGIN_STATE_DIR`, which holds the one cache database every pane
    /// shares, or `None` when the viewer was not launched by herdr.
    ///
    /// With no state dir there is nowhere a cache may be written — the viewer
    /// invents no directory of its own — so the pane simply fetches every time.
    pub state_dir: Option<PathBuf>,
    /// The GraphQL endpoint to POST to.
    pub graphql_url: String,
    /// The GitHub token, or `None` for the no-token status line.
    pub token: Option<String>,
    /// The `[repo."<repo_root>"] slug` overrides, ahead of `origin` in the
    /// resolution order (ADR-0004). Filled from `config.toml`.
    pub slug_overrides: SlugOverrides,
    /// `list_page_size` — how many issues the list query asks for.
    pub list_page_size: u32,
    /// `detail_comment_page_size` — how many comments one detail query asks for.
    pub detail_comment_page_size: u32,
    /// `prune_details_after_days`. Carried for the startup prune, which is a
    /// later ticket; nothing in this slice deletes anything.
    pub prune_details_after_days: u32,
    /// `prune_repos_after_days`. Carried, as above.
    pub prune_repos_after_days: u32,
    /// Whatever `config.toml` got wrong. The app turns a non-empty list into the
    /// one status line it has.
    pub config_warnings: Vec<ConfigWarning>,
}

impl Default for Environment {
    /// A viewer that has learned nothing from outside: no workspace, no herdr,
    /// no token, and the config file's defaults everywhere else. Startup and the
    /// tests both build on this and replace what they actually know.
    fn default() -> Self {
        let config = Config::default();
        Self {
            workspace_cwd: PathBuf::new(),
            herdr_socket: None,
            workspace_id: None,
            state_dir: None,
            graphql_url: DEFAULT_GRAPHQL_URL.to_string(),
            token: None,
            slug_overrides: config.slug_overrides,
            list_page_size: config.list_page_size,
            detail_comment_page_size: config.detail_comment_page_size,
            prune_details_after_days: config.prune_details_after_days,
            prune_repos_after_days: config.prune_repos_after_days,
            config_warnings: config.warnings,
        }
    }
}

impl Environment {
    /// Overrides the GraphQL endpoint. Set by the tests to point at the stub
    /// server; a user would only set it to talk to a proxy.
    pub const GRAPHQL_URL_VAR: &'static str = "HERDR_ISSUES_GRAPHQL_URL";

    /// Reads the environment of a real pane process.
    ///
    /// Everything here is free: variables herdr already injected, plus at most
    /// one `gh` spawn for the token. The socket is not touched until the repo
    /// root is resolved.
    /// `HERDR_PLUGIN_CONFIG_DIR`, which is where `config.toml` lives.
    pub const CONFIG_DIR_VAR: &'static str = "HERDR_PLUGIN_CONFIG_DIR";

    /// `HERDR_PLUGIN_STATE_DIR`, which is where the cache database lives.
    pub const STATE_DIR_VAR: &'static str = "HERDR_PLUGIN_STATE_DIR";

    pub fn from_process() -> Self {
        let context = plugin_context();
        let config_dir = non_empty_var(Self::CONFIG_DIR_VAR).map(PathBuf::from);
        Self {
            workspace_cwd: workspace_cwd(context.as_ref()),
            herdr_socket: non_empty_var("HERDR_SOCKET_PATH").map(PathBuf::from),
            workspace_id: non_empty_var("HERDR_WORKSPACE_ID")
                .or_else(|| string_field(context.as_ref(), "workspace_id")),
            state_dir: non_empty_var(Self::STATE_DIR_VAR).map(PathBuf::from),
            graphql_url: env::var(Self::GRAPHQL_URL_VAR)
                .ok()
                .filter(|url| !url.is_empty())
                .unwrap_or_else(|| DEFAULT_GRAPHQL_URL.to_string()),
            token: discover_token(),
            // Everything the config file decides keeps its default until the
            // file is folded in below.
            ..Self::default()
        }
        .with_config(Config::load(config_dir.as_deref()))
    }

    /// Folds a loaded `config.toml` in, which is the last thing startup learns
    /// from outside itself.
    ///
    /// It is applied *after* [`discover_token`] has run, because that is where
    /// `token_file` sits in ADR-0005's order — `$GITHUB_TOKEN`, `$GH_TOKEN`,
    /// `gh auth token`, then the file — so the file only ever supplies a token
    /// nothing above it did. The path is read, never written: the viewer holds
    /// no credential of its own.
    ///
    /// Tests apply a config parsed from a real file in a temp directory, which
    /// is how every key reaches the app seam.
    pub fn with_config(mut self, config: Config) -> Self {
        self.list_page_size = config.list_page_size;
        self.detail_comment_page_size = config.detail_comment_page_size;
        self.prune_details_after_days = config.prune_details_after_days;
        self.prune_repos_after_days = config.prune_repos_after_days;
        self.slug_overrides = config.slug_overrides;
        self.config_warnings = config.warnings;
        self.token = self
            .token
            .or_else(|| config.token_file.as_deref().and_then(token_from_file));
        self
    }
}

/// `HERDR_PLUGIN_CONTEXT_JSON`, parsed once — it carries both the workspace
/// directory and the workspace id.
fn plugin_context() -> Option<serde_json::Value> {
    let json = non_empty_var("HERDR_PLUGIN_CONTEXT_JSON")?;
    serde_json::from_str(&json).ok()
}

fn workspace_cwd(context: Option<&serde_json::Value>) -> PathBuf {
    string_field(context, "workspace_cwd")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn string_field(context: Option<&serde_json::Value>, key: &str) -> Option<String> {
    context?
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn non_empty_var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

/// The first three legs of ADR-0005's order: `$GITHUB_TOKEN`, then `$GH_TOKEN`,
/// then `gh auth token`.
///
/// The fourth, `token_file`, needs the config file and so lives in
/// [`Environment::with_config`], which runs after this. The viewer never writes
/// a credential of its own.
fn discover_token() -> Option<String> {
    for variable in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(token) = env::var(variable) {
            let token = token.trim().to_string();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// The token in `token_file`: **the first line, trimmed**, so a file with a
/// trailing newline or a comment under it still works.
///
/// Read-only, like everything else here — a file the user already owns and
/// manages, that the viewer never writes to.
pub fn token_from_file(path: &Path) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    let token = contents.lines().next().unwrap_or_default().trim();
    (!token.is_empty()).then(|| token.to_string())
}
