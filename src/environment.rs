//! The environment description the viewer is constructed from.
//!
//! Everything the process learns from outside itself — the workspace directory
//! to resolve the repo root from, the GraphQL endpoint, the token — is gathered
//! here, once, at startup. Tests build an [`Environment`] literally instead of
//! exporting variables, which is what makes the app seam drivable.

use std::env;
use std::path::PathBuf;
use std::process::Command;

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
    /// The GraphQL endpoint to POST to.
    pub graphql_url: String,
    /// The GitHub token, or `None` for the no-token status line.
    pub token: Option<String>,
    /// The `[repo."<repo_root>"] slug` overrides, ahead of `origin` in the
    /// resolution order (ADR-0004).
    ///
    /// Empty here: the config file that fills it is a later ticket. This is the
    /// shape it has to produce.
    pub slug_overrides: SlugOverrides,
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
    pub fn from_process() -> Self {
        let context = plugin_context();
        Self {
            workspace_cwd: workspace_cwd(context.as_ref()),
            herdr_socket: non_empty_var("HERDR_SOCKET_PATH").map(PathBuf::from),
            workspace_id: non_empty_var("HERDR_WORKSPACE_ID")
                .or_else(|| string_field(context.as_ref(), "workspace_id")),
            graphql_url: env::var(Self::GRAPHQL_URL_VAR)
                .ok()
                .filter(|url| !url.is_empty())
                .unwrap_or_else(|| DEFAULT_GRAPHQL_URL.to_string()),
            token: discover_token(),
            // The config file is a later ticket; until it lands no override is
            // ever populated and resolution starts at `origin`.
            slug_overrides: SlugOverrides::none(),
        }
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

/// `$GITHUB_TOKEN`, then `$GH_TOKEN`, then `gh auth token`.
///
/// The `token_file` config key is a later ticket; this slice has no config file.
/// The viewer never writes a credential of its own.
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
