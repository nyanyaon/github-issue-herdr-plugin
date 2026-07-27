//! The environment description the viewer is constructed from.
//!
//! Everything the process learns from outside itself — the workspace directory
//! to resolve the repo root from, the GraphQL endpoint, the token — is gathered
//! here, once, at startup. Tests build an [`Environment`] literally instead of
//! exporting variables, which is what makes the app seam drivable.

use std::env;
use std::path::PathBuf;
use std::process::Command;

/// The GraphQL endpoint, unless [`Environment::GRAPHQL_URL_VAR`] overrides it.
pub const DEFAULT_GRAPHQL_URL: &str = "https://api.github.com/graphql";

#[derive(Debug, Clone)]
pub struct Environment {
    /// The workspace's directory. The repo root is resolved from it with `git`.
    ///
    /// A pane command's working directory is the plugin root, never the repo, so
    /// this comes from `HERDR_PLUGIN_CONTEXT_JSON` when herdr launched us and
    /// from the process working directory when it did not.
    pub workspace_cwd: PathBuf,
    /// The GraphQL endpoint to POST to.
    pub graphql_url: String,
    /// The GitHub token, or `None` for the no-token status line.
    pub token: Option<String>,
}

impl Environment {
    /// Overrides the GraphQL endpoint. Set by the tests to point at the stub
    /// server; a user would only set it to talk to a proxy.
    pub const GRAPHQL_URL_VAR: &'static str = "HERDR_ISSUES_GRAPHQL_URL";

    /// Reads the environment of a real pane process.
    ///
    /// No herdr socket is consulted: the repo root comes from `git` alone, which
    /// is also the path a user hits when running the viewer outside herdr.
    pub fn from_process() -> Self {
        Self {
            workspace_cwd: workspace_cwd(),
            graphql_url: env::var(Self::GRAPHQL_URL_VAR)
                .ok()
                .filter(|url| !url.is_empty())
                .unwrap_or_else(|| DEFAULT_GRAPHQL_URL.to_string()),
            token: discover_token(),
        }
    }
}

fn workspace_cwd() -> PathBuf {
    let from_context = env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|context| {
            context
                .get("workspace_cwd")
                .and_then(|value| value.as_str())
                .map(PathBuf::from)
        });
    from_context.unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
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
