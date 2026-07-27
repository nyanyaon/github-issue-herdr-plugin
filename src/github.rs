//! The GraphQL client and the issue list query.
//!
//! One blocking request at a time, gzip on, hand-written query strings. The
//! viewer issues no mutating request, ever.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::age;
use crate::identity::Slug;

/// The cheap query: one row per issue, ordered by most recently updated.
///
/// `nameWithOwner` is what the header displays — GraphQL follows renames, so a
/// repo renamed on GitHub keeps working with a stale remote.
const ISSUE_LIST_QUERY: &str = r"
query($owner:String!,$name:String!,$states:[IssueState!],$first:Int!,$after:String){
  repository(owner:$owner,name:$name){
    nameWithOwner
    issues(first:$first,after:$after,states:$states,orderBy:{field:UPDATED_AT,direction:DESC}){
      totalCount
      pageInfo{hasNextPage endCursor}
      nodes{number title state updatedAt comments{totalCount}
            author{login} labels(first:10){nodes{name color}}}
    }
  }
}";

/// Which issues the list query asks for — the `$states` argument, and the only
/// thing `o` changes.
///
/// The viewer holds one of these for the pane's lifetime; cycling it is the only
/// reason the list is queried a second time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IssueStates {
    #[default]
    Open,
    Closed,
    All,
}

impl IssueStates {
    /// The cycle `o` walks: open → closed → all → open.
    pub fn cycled(self) -> Self {
        match self {
            Self::Open => Self::Closed,
            Self::Closed => Self::All,
            Self::All => Self::Open,
        }
    }

    /// The word the header counts in: `6 open`, `6 closed`, `6 issues`.
    pub fn noun(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::All => "issues",
        }
    }

    /// The `$states` variable. `All` sends null, which is how GraphQL says
    /// "every state" — there is no `ALL` member of `IssueState`.
    fn argument(self) -> serde_json::Value {
        match self {
            Self::Open => json!(["OPEN"]),
            Self::Closed => json!(["CLOSED"]),
            Self::All => serde_json::Value::Null,
        }
    }
}

/// One row of the issue list.
#[derive(Debug, Clone)]
pub struct IssueRow {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub updated_at: Option<i64>,
    pub comment_count: u64,
    pub author: Option<String>,
    pub labels: Vec<String>,
}

impl IssueRow {
    /// The first label, which is all a row has room for.
    pub fn first_label(&self) -> Option<&str> {
        self.labels.first().map(String::as_str)
    }
}

/// The issue list as it came back from one query.
#[derive(Debug, Clone)]
pub struct IssueList {
    /// `nameWithOwner`, exactly as the API returned it.
    pub name_with_owner: String,
    /// How many issues match the query, not how many rows arrived.
    pub total_count: u64,
    pub rows: Vec<IssueRow>,
    /// When this list was fetched, in unix seconds.
    pub fetched_at: i64,
}

/// Every way the API can fail. Each one is a single status line; nothing
/// retries by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    NoToken,
    TokenRejected,
    NotFound { slug: String },
    RateLimited { resets_in_minutes: Option<i64> },
    Offline,
    Unexpected { message: String },
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoToken => write!(
                f,
                "no GitHub token found · set GITHUB_TOKEN or run `gh auth login`"
            ),
            Self::TokenRejected => write!(
                f,
                "token rejected · check GITHUB_TOKEN or run `gh auth login`"
            ),
            Self::NotFound { slug } => write!(f, "{slug} not found — or your token can't see it"),
            Self::RateLimited {
                resets_in_minutes: Some(minutes),
            } => write!(f, "rate limited · resets in {minutes}m"),
            Self::RateLimited {
                resets_in_minutes: None,
            } => write!(f, "rate limited"),
            Self::Offline => write!(f, "offline · could not reach the GitHub API"),
            Self::Unexpected { message } => write!(f, "github error · {message}"),
        }
    }
}

/// A GraphQL client bound to one endpoint and one token.
pub struct GithubClient {
    agent: ureq::Agent,
    url: String,
    token: String,
}

impl GithubClient {
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            // Errors are read off the response, not thrown: a 403 carries the
            // rate-limit reset we want to show.
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent("herdr-issues")
            .build()
            .new_agent();
        Self {
            agent,
            url: url.into(),
            token: token.into(),
        }
    }

    /// The one query this slice makes: the issues in `states`, most recently
    /// updated first.
    pub fn issue_list(
        &self,
        slug: &Slug,
        states: IssueStates,
        page_size: u32,
    ) -> Result<IssueList, ApiError> {
        let body = json!({
            "query": ISSUE_LIST_QUERY,
            "variables": {
                "owner": slug.owner,
                "name": slug.name,
                "states": states.argument(),
                "first": page_size,
                "after": serde_json::Value::Null,
            }
        });

        let mut response = self
            .agent
            .post(&self.url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/json")
            .send_json(&body)
            .map_err(|_| ApiError::Offline)?;

        let status = response.status().as_u16();
        if status == 401 {
            return Err(ApiError::TokenRejected);
        }
        if status == 403 || status == 429 {
            let resets_in_minutes = response
                .headers()
                .get("x-ratelimit-reset")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<i64>().ok())
                .map(|reset| ((reset - age::now()).max(0) + 59) / 60);
            return Err(ApiError::RateLimited { resets_in_minutes });
        }
        if !(200..300).contains(&status) {
            return Err(ApiError::Unexpected {
                message: format!("HTTP {status}"),
            });
        }

        let envelope: Envelope =
            response
                .body_mut()
                .read_json()
                .map_err(|error| ApiError::Unexpected {
                    message: error.to_string(),
                })?;
        envelope.into_issue_list(slug)
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

impl Envelope {
    fn into_issue_list(self, slug: &Slug) -> Result<IssueList, ApiError> {
        let repository = self.data.and_then(|data| data.repository);
        let Some(repository) = repository else {
            let not_found = self
                .errors
                .iter()
                .any(|error| error.error_type.as_deref() == Some("NOT_FOUND"));
            if not_found {
                return Err(ApiError::NotFound {
                    slug: slug.to_string(),
                });
            }
            let message = self
                .errors
                .first()
                .map(|error| error.message.clone())
                .unwrap_or_else(|| "empty response".to_string());
            return Err(ApiError::Unexpected { message });
        };

        let rows = repository
            .issues
            .nodes
            .into_iter()
            .flatten()
            .map(|node| IssueRow {
                number: node.number,
                title: node.title,
                state: node.state,
                updated_at: node.updated_at.as_deref().and_then(age::parse_timestamp),
                comment_count: node.comments.total_count,
                author: node.author.map(|author| author.login),
                labels: node
                    .labels
                    .nodes
                    .into_iter()
                    .flatten()
                    .map(|label| label.name)
                    .collect(),
            })
            .collect();

        Ok(IssueList {
            name_with_owner: repository.name_with_owner,
            total_count: repository.issues.total_count,
            rows,
            fetched_at: age::now(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    #[serde(default)]
    message: String,
    #[serde(rename = "type", default)]
    error_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Data {
    repository: Option<Repository>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    name_with_owner: String,
    issues: Issues,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Issues {
    total_count: u64,
    #[serde(default)]
    nodes: Vec<Option<IssueNode>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueNode {
    number: u64,
    title: String,
    state: String,
    updated_at: Option<String>,
    comments: CommentCount,
    author: Option<Author>,
    labels: Labels,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentCount {
    total_count: u64,
}

#[derive(Debug, Deserialize)]
struct Author {
    login: String,
}

#[derive(Debug, Deserialize)]
struct Labels {
    #[serde(default)]
    nodes: Vec<Option<Label>>,
}

#[derive(Debug, Deserialize)]
struct Label {
    name: String,
}
