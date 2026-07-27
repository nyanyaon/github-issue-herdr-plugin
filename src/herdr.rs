//! The herdr socket client: one `worktree.list` request, at startup.
//!
//! Newline-delimited JSON over the unix socket at `HERDR_SOCKET_PATH`, measured
//! at ~2 ms. One request, one reply, connection closed. Nothing here subscribes
//! to anything: a held subscription is work while idle, and the pane does none —
//! `pane.updated` alone was measured at 69 messages in 6 seconds on a near-idle
//! session.
//!
//! What the request buys is the **repo root** behind a workspace, with linked
//! worktrees already collapsed onto their source repo, which is why every
//! worktree of a repo shows one issue list.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

/// How long to wait on a socket measured at 2 ms. Generous, and only ever paid
/// once, at startup; past it the git fallback answers instead.
const TIMEOUT: Duration = Duration::from_secs(2);

/// How many lines to read before giving up on finding the reply. The socket
/// answers on the next line; the budget only exists so a chatty server can never
/// block startup.
const MAX_REPLY_LINES: usize = 8;

/// The source repo `worktree.list` reports for a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSource {
    /// `result.source.repo_root` — the repo every worktree of it collapses onto.
    pub repo_root: PathBuf,
}

/// The two ways asking herdr can end without a repo root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HerdrError {
    /// herdr's own `not_git_worktree` — the canonical "no repo in this
    /// workspace" signal. Authoritative: the caller does not second-guess it
    /// with `git`.
    NotGitWorktree,
    /// The socket was not there, did not answer, or answered something this
    /// version does not understand. The caller falls back to `git`, which is
    /// also the path a user hits running the viewer outside herdr.
    Unavailable { message: String },
}

/// Asks herdr which repo backs a workspace.
///
/// `workspace_id` is what a pane knows about itself and never changes for its
/// process lifetime; `cwd` is the fallback question for a viewer launched
/// without one. Both are optional to herdr — with neither it answers for the
/// active workspace, which is not necessarily ours, so one of them is always
/// sent.
pub fn worktree_source(
    socket_path: &Path,
    workspace_id: Option<&str>,
    cwd: &Path,
) -> Result<WorktreeSource, HerdrError> {
    let params = match workspace_id {
        Some(id) => json!({ "workspace_id": id }),
        None => json!({ "cwd": cwd.to_string_lossy() }),
    };
    let request = json!({ "id": "repo-root", "method": "worktree.list", "params": params });

    let stream = UnixStream::connect(socket_path).map_err(unavailable)?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(unavailable)?;
    stream
        .set_write_timeout(Some(TIMEOUT))
        .map_err(unavailable)?;
    writeln!(&stream, "{request}").map_err(unavailable)?;

    let mut reader = BufReader::new(&stream);
    for _ in 0..MAX_REPLY_LINES {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(unavailable)? == 0 {
            break;
        }
        if let Some(reply) = serde_json::from_str::<Reply>(line.trim())
            .ok()
            .filter(Reply::is_answer)
        {
            return reply.into_source();
        }
    }
    Err(HerdrError::Unavailable {
        message: format!("no worktree.list reply from {}", socket_path.display()),
    })
}

fn unavailable(error: impl ToString) -> HerdrError {
    HerdrError::Unavailable {
        message: error.to_string(),
    }
}

#[derive(Debug, Deserialize)]
struct Reply {
    #[serde(default)]
    result: Option<ReplyResult>,
    #[serde(default)]
    error: Option<ReplyError>,
}

impl Reply {
    /// Whether this line is an answer at all, rather than an event or an
    /// acknowledgement that happened to arrive first.
    fn is_answer(&self) -> bool {
        self.result.is_some() || self.error.is_some()
    }

    fn into_source(self) -> Result<WorktreeSource, HerdrError> {
        if let Some(error) = self.error {
            if error.code.as_deref() == Some("not_git_worktree") {
                return Err(HerdrError::NotGitWorktree);
            }
            return Err(HerdrError::Unavailable {
                message: error.message.unwrap_or_else(|| "herdr error".to_string()),
            });
        }
        let repo_root = self
            .result
            .and_then(|result| result.source)
            .and_then(|source| source.repo_root)
            .filter(|root| !root.is_empty());
        match repo_root {
            Some(root) => Ok(WorktreeSource {
                repo_root: PathBuf::from(root),
            }),
            None => Err(HerdrError::Unavailable {
                message: "worktree.list answered with no repo_root".to_string(),
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReplyResult {
    #[serde(default)]
    source: Option<Source>,
}

#[derive(Debug, Deserialize)]
struct Source {
    #[serde(default)]
    repo_root: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReplyError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(line: &str) -> Result<WorktreeSource, HerdrError> {
        serde_json::from_str::<Reply>(line)
            .expect("a reply this test wrote")
            .into_source()
    }

    #[test]
    fn reads_the_repo_root_out_of_a_worktree_list_reply() {
        let source = reply(
            r#"{"result":{"type":"worktree_list",
                "source":{"repo_key":"/home/nyaon/simata-start/.git",
                          "repo_name":"simata-start",
                          "repo_root":"/home/nyaon/simata-start"},
                "worktrees":[]}}"#,
        );
        assert_eq!(
            source,
            Ok(WorktreeSource {
                repo_root: PathBuf::from("/home/nyaon/simata-start")
            })
        );
    }

    #[test]
    fn recognises_herdrs_no_repo_here_signal() {
        assert_eq!(
            reply(
                r#"{"error":{"code":"not_git_worktree",
                    "message":"Herdr worktree actions require a path inside a Git work tree"}}"#
            ),
            Err(HerdrError::NotGitWorktree)
        );
    }
}
