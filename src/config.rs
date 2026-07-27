//! `$HERDR_PLUGIN_CONFIG_DIR/config.toml` — optional, and so is every key in it
//! (SPEC §10, [ADR-0003](../docs/adr/0003-packaging-and-distribution.md)).
//!
//! A fresh install has no file at all, which is not an error and is not
//! reported: the defaults here are what the viewer runs on. A file that is
//! present but wrong never stops the pane from opening — an unreadable or
//! malformed file falls back to the same defaults, an unknown key is ignored,
//! and each of those leaves a [`ConfigWarning`] for the status line to carry.
//!
//! Reading it is the only work startup does here, once, before anything else.

use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::identity::{Slug, SlugOverrides};

/// The file, inside `$HERDR_PLUGIN_CONFIG_DIR`.
pub const FILE_NAME: &str = "config.toml";

/// Every key of `config.toml`, resolved — the file's values where it had them
/// and the defaults everywhere else.
#[derive(Debug, Clone)]
pub struct Config {
    /// How many issues the list query asks for.
    pub list_page_size: u32,
    /// How many comments one detail query asks for. 100 is GraphQL's maximum.
    pub detail_comment_page_size: u32,
    /// How long a detail nothing has displayed survives the startup prune.
    pub prune_details_after_days: u32,
    /// How long a repo nothing has opened survives it — and it takes the
    /// repo's list rows and details with it.
    pub prune_repos_after_days: u32,
    /// A file whose first line is a GitHub token — the last source in
    /// ADR-0005's discovery order. The viewer only ever reads it.
    pub token_file: Option<PathBuf>,
    /// The `[repo."<repo_root>"] slug` table, parsed (ADR-0004). Ahead of
    /// `origin` in the resolution order, and the only identity a checkout with
    /// no remote can have.
    pub slug_overrides: SlugOverrides,
    /// What was wrong with the file, if anything. Empty when there is no file:
    /// needing no configuration is the normal case, not a problem to report.
    pub warnings: Vec<ConfigWarning>,
}

impl Default for Config {
    /// SPEC §10's defaults — what a viewer with no config file runs on.
    fn default() -> Self {
        Self {
            list_page_size: 50,
            detail_comment_page_size: 100,
            prune_details_after_days: 30,
            prune_repos_after_days: 90,
            token_file: None,
            slug_overrides: SlugOverrides::none(),
            warnings: Vec::new(),
        }
    }
}

impl Config {
    /// Loads `<config_dir>/config.toml`, or the defaults when there is no
    /// config directory, no file, or nothing usable in it.
    pub fn load(config_dir: Option<&Path>) -> Self {
        match config_dir {
            Some(directory) => Self::read(&directory.join(FILE_NAME)),
            None => Self::default(),
        }
    }

    /// Reads one file. **Absent is silent** — a fresh install needs no
    /// configuration, so there is nothing to warn about. A file that exists but
    /// cannot be read is a different thing and does warn.
    pub fn read(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, home_directory().as_deref()),
            Err(error) if error.kind() == ErrorKind::NotFound => Self::default(),
            Err(error) => Self {
                warnings: vec![ConfigWarning::Unreadable {
                    message: error.to_string(),
                }],
                ..Self::default()
            },
        }
    }

    /// Parses the file's text, key by key.
    ///
    /// Every key is independent: an unknown one, or one carrying the wrong kind
    /// of value, costs only itself and leaves the rest of the file in force.
    /// Only a file that will not parse at all falls back wholesale.
    ///
    /// `home` expands a leading `~/` in `token_file`, which is how SPEC §10
    /// itself writes that path.
    pub fn parse(text: &str, home: Option<&Path>) -> Self {
        let mut config = Self::default();
        let table: toml::Table = match text.parse() {
            Ok(table) => table,
            Err(error) => {
                // The whole file is unusable, so the defaults stand — but the
                // pane still opens, and the line says why it is ignoring the
                // file rather than why it refused to start.
                config.warnings.push(ConfigWarning::Malformed {
                    message: first_line(&error.to_string()),
                });
                return config;
            }
        };

        let mut unknown = Vec::new();
        let mut overrides = Vec::new();
        for (key, value) in &table {
            match key.as_str() {
                "list_page_size" => {
                    assign_count(&mut config.list_page_size, key, value, &mut config.warnings);
                }
                "detail_comment_page_size" => assign_count(
                    &mut config.detail_comment_page_size,
                    key,
                    value,
                    &mut config.warnings,
                ),
                "prune_details_after_days" => assign_count(
                    &mut config.prune_details_after_days,
                    key,
                    value,
                    &mut config.warnings,
                ),
                "prune_repos_after_days" => assign_count(
                    &mut config.prune_repos_after_days,
                    key,
                    value,
                    &mut config.warnings,
                ),
                "token_file" => match value.as_str() {
                    Some(path) => config.token_file = Some(expand_tilde(path, home)),
                    None => config.warnings.push(ConfigWarning::BadValue {
                        key: key.clone(),
                        expected: "a path",
                    }),
                },
                "repo" => match value.as_table() {
                    Some(repos) => {
                        read_repo_table(repos, &mut overrides, &mut unknown, &mut config.warnings)
                    }
                    None => config.warnings.push(ConfigWarning::BadValue {
                        key: key.clone(),
                        expected: "a table",
                    }),
                },
                _ => unknown.push(key.clone()),
            }
        }

        if !unknown.is_empty() {
            config
                .warnings
                .push(ConfigWarning::UnknownKeys { keys: unknown });
        }
        config.slug_overrides = SlugOverrides::from_entries(overrides);
        config
    }
}

/// `[repo."<repo_root>"] slug = "owner/repo"`, one entry per repo root.
///
/// The key is the repo root exactly as herdr reports it, and the value goes
/// through [`Slug::parse`], so an override that is not `owner/repo` warns and is
/// dropped rather than sending a nonsense query.
fn read_repo_table(
    repos: &toml::Table,
    overrides: &mut Vec<(PathBuf, Slug)>,
    unknown: &mut Vec<String>,
    warnings: &mut Vec<ConfigWarning>,
) {
    for (repo_root, entry) in repos {
        let Some(entry) = entry.as_table() else {
            warnings.push(ConfigWarning::BadValue {
                key: format!("repo.{repo_root:?}"),
                expected: "a table",
            });
            continue;
        };
        for key in entry.keys() {
            if key != "slug" {
                unknown.push(format!("repo.{repo_root:?}.{key}"));
            }
        }
        match entry.get("slug").and_then(toml::Value::as_str) {
            Some(text) => match Slug::parse(text) {
                Some(slug) => overrides.push((PathBuf::from(repo_root), slug)),
                None => warnings.push(ConfigWarning::NotASlug {
                    repo_root: repo_root.clone(),
                }),
            },
            None => warnings.push(ConfigWarning::MissingSlug {
                repo_root: repo_root.clone(),
            }),
        }
    }
}

/// A count key: a positive whole number, or the default kept and a warning.
fn assign_count(
    target: &mut u32,
    key: &str,
    value: &toml::Value,
    warnings: &mut Vec<ConfigWarning>,
) {
    match value
        .as_integer()
        .and_then(|number| u32::try_from(number).ok())
        .filter(|number| *number > 0)
    {
        Some(number) => *target = number,
        None => warnings.push(ConfigWarning::BadValue {
            key: key.to_string(),
            expected: "a positive whole number",
        }),
    }
}

/// Everything the file got wrong. Each one is ignored, never fatal, and each
/// one reaches the user as part of the single status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWarning {
    /// The file does not parse as TOML.
    Malformed { message: String },
    /// The file is there but could not be read — permissions, a directory in
    /// its place, a broken symlink.
    Unreadable { message: String },
    /// Keys nothing in this version knows about.
    UnknownKeys { keys: Vec<String> },
    /// A known key carrying the wrong kind of value.
    BadValue { key: String, expected: &'static str },
    /// An override that is not `owner/repo`. The value is not echoed — the
    /// status line is one line, and repo roots are long.
    NotASlug { repo_root: String },
    /// A `[repo."<path>"]` table with no `slug` in it.
    MissingSlug { repo_root: String },
}

impl fmt::Display for ConfigWarning {
    /// One clause, no prefix: [`crate::ui::status::StatusLine`] names the file
    /// once and joins whatever clauses there are.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { message } => write!(f, "malformed, using defaults — {message}"),
            Self::Unreadable { message } => write!(f, "unreadable, using defaults — {message}"),
            Self::UnknownKeys { keys } => {
                let noun = if keys.len() == 1 { "key" } else { "keys" };
                write!(f, "ignoring unknown {noun} {}", keys.join(", "))
            }
            Self::BadValue { key, expected } => {
                write!(f, "{key} must be {expected}, using the default")
            }
            Self::NotASlug { repo_root } => {
                write!(f, "[repo.{repo_root:?}] slug is not owner/repo")
            }
            Self::MissingSlug { repo_root } => write!(f, "[repo.{repo_root:?}] has no slug"),
        }
    }
}

/// Expands a leading `~/`, because SPEC §10's own `token_file` example is
/// written that way. Anything else is taken as given.
fn expand_tilde(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// A TOML parse error spans several lines with a source snippet; the status line
/// has one.
fn first_line(message: &str) -> String {
    message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_the_tilde_the_spec_writes_token_file_with() {
        assert_eq!(
            expand_tilde("~/.secrets/gh-issues", Some(Path::new("/home/me"))),
            PathBuf::from("/home/me/.secrets/gh-issues")
        );
        assert_eq!(
            expand_tilde("/etc/gh-issues", Some(Path::new("/home/me"))),
            PathBuf::from("/etc/gh-issues")
        );
        // No home to expand against: better a path that fails visibly than a
        // path silently rooted somewhere else.
        assert_eq!(
            expand_tilde("~/.secrets/gh-issues", None),
            PathBuf::from("~/.secrets/gh-issues")
        );
    }

    #[test]
    fn a_file_that_is_not_there_is_not_a_problem() {
        let config = Config::read(Path::new("/nonexistent/herdr-issues/config.toml"));
        assert!(config.warnings.is_empty());
        assert_eq!(config.list_page_size, 50);
    }
}
