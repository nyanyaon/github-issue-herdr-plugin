//! Repo identity: the resolution of a workspace directory to a repo root, and
//! of that repo root to a slug (ADR-0004).
//!
//! In this slice the repo root comes from `git` alone — `rev-parse
//! --show-toplevel` plus `--git-common-dir`, so a linked worktree collapses to
//! the repo it was made from. The herdr socket, and the `[repo."<path>"]`
//! config override that sits ahead of `origin` in the ADR's order, are later
//! tickets.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A GitHub `owner/repo` parsed from a remote.
///
/// It names the repo to *query*. What the header displays is always the
/// `nameWithOwner` the API answered with, never this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slug {
    pub owner: String,
    pub name: String,
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// One repo per pane, bound at startup and never re-resolved.
#[derive(Debug, Clone)]
pub struct RepoIdentity {
    pub repo_root: PathBuf,
    pub slug: Slug,
}

/// Every way repo identity can fail. Each one is a single status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    NoRepo { workspace_cwd: PathBuf },
    NoRemote,
    AmbiguousRemote { candidates: Vec<String> },
    UnsupportedHost { host: String },
    NoHost { url: String },
    Git { message: String },
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRepo { workspace_cwd } => {
                write!(
                    f,
                    "no git repo in this workspace ({})",
                    workspace_cwd.display()
                )
            }
            Self::NoRemote => write!(f, "no git remote · set slug in config.toml"),
            Self::AmbiguousRemote { candidates } => write!(
                f,
                "several remotes and no origin: {} · set slug in config.toml",
                candidates.join(", ")
            ),
            Self::UnsupportedHost { host } => {
                write!(f, "{host} is not supported — github.com only")
            }
            Self::NoHost { url } => write!(f, "remote {url} has no github.com host"),
            Self::Git { message } => write!(f, "git failed: {message}"),
        }
    }
}

/// Resolves the workspace directory to a repo root and a slug.
pub fn resolve(workspace_cwd: &Path) -> Result<RepoIdentity, IdentityError> {
    let repo_root = repo_root(workspace_cwd)?;
    let slug = slug_of(&repo_root)?;
    Ok(RepoIdentity { repo_root, slug })
}

/// The repo root behind a workspace directory.
///
/// `--show-toplevel` alone stops at a linked worktree, so the common dir — which
/// every worktree shares with its source repo — is what collapses them.
pub fn repo_root(workspace_cwd: &Path) -> Result<PathBuf, IdentityError> {
    let output = git(
        workspace_cwd,
        &["rev-parse", "--show-toplevel", "--git-common-dir"],
    )
    .map_err(|_| IdentityError::NoRepo {
        workspace_cwd: workspace_cwd.to_path_buf(),
    })?;
    let mut lines = output.lines();
    let toplevel = PathBuf::from(lines.next().unwrap_or_default());
    let common_dir = lines.next().unwrap_or_default();
    if common_dir.is_empty() {
        return Ok(toplevel);
    }
    let common_dir = match Path::new(common_dir) {
        absolute if absolute.is_absolute() => absolute.to_path_buf(),
        relative => workspace_cwd.join(relative),
    };
    let root = match common_dir.file_name() {
        Some(name) if name == ".git" => common_dir.parent().map(Path::to_path_buf),
        _ => None,
    };
    let root = root.unwrap_or(toplevel);
    Ok(root.canonicalize().unwrap_or(root))
}

/// `origin`, else the sole remote, else an error naming the candidates.
fn slug_of(repo_root: &Path) -> Result<Slug, IdentityError> {
    if let Ok(url) = git(repo_root, &["remote", "get-url", "origin"]) {
        return parse_remote_url(url.trim());
    }
    let remotes: Vec<String> = git(repo_root, &["remote"])
        .map_err(|message| IdentityError::Git { message })?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    match remotes.as_slice() {
        [] => Err(IdentityError::NoRemote),
        [only] => {
            let url = git(repo_root, &["remote", "get-url", only])
                .map_err(|message| IdentityError::Git { message })?;
            parse_remote_url(url.trim())
        }
        candidates => Err(IdentityError::AmbiguousRemote {
            candidates: candidates.to_vec(),
        }),
    }
}

/// Accepts `https://github.com/o/r(.git)`, `git@github.com:o/r(.git)` and
/// `ssh://git@github.com/o/r(.git)`. Any other host is a named failure.
pub fn parse_remote_url(url: &str) -> Result<Slug, IdentityError> {
    let (host, path) = split_remote_url(url).ok_or_else(|| IdentityError::NoHost {
        url: url.to_string(),
    })?;
    if !host.eq_ignore_ascii_case("github.com") {
        return Err(IdentityError::UnsupportedHost { host });
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    match (segments.next(), segments.next()) {
        (Some(owner), Some(name)) => Ok(Slug {
            owner: owner.to_string(),
            name: name.to_string(),
        }),
        _ => Err(IdentityError::NoHost {
            url: url.to_string(),
        }),
    }
}

/// Splits a remote URL into its host and its path, for both the URL form and the
/// scp-like form.
fn split_remote_url(url: &str) -> Option<(String, String)> {
    let after_scheme = match url.find("://") {
        Some(index) => &url[index + 3..],
        None => {
            // scp-like: `[user@]host:owner/repo`, distinguished from a bare path
            // by having a colon that is not part of a `://`.
            let (authority, path) = url.split_once(':')?;
            let host = authority.rsplit('@').next()?;
            return Some((strip_port(host), path.to_string()));
        }
    };
    let (authority, path) = after_scheme.split_once('/')?;
    let host = authority.rsplit('@').next()?;
    Some((strip_port(host), path.to_string()))
}

fn strip_port(host: &str) -> String {
    host.split(':').next().unwrap_or(host).to_string()
}

/// One `git` invocation. Stderr becomes the error, so a failure can be reported
/// rather than swallowed.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_supported_remote_form() {
        let expected = Slug {
            owner: "nyanyaon".to_string(),
            name: "github-issue-herdr-plugin".to_string(),
        };
        for url in [
            "https://github.com/nyanyaon/github-issue-herdr-plugin.git",
            "https://github.com/nyanyaon/github-issue-herdr-plugin",
            "git@github.com:nyanyaon/github-issue-herdr-plugin.git",
            "ssh://git@github.com/nyanyaon/github-issue-herdr-plugin.git",
        ] {
            assert_eq!(parse_remote_url(url), Ok(expected.clone()), "{url}");
        }
    }

    #[test]
    fn names_the_host_it_does_not_support() {
        assert_eq!(
            parse_remote_url("git@gitlab.com:nyanyaon/thing.git"),
            Err(IdentityError::UnsupportedHost {
                host: "gitlab.com".to_string()
            })
        );
    }
}
