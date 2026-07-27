//! The GraphQL client and the issue list query.
//!
//! One blocking request at a time, gzip on, hand-written query strings. The
//! viewer issues no mutating request, ever.

use std::fmt;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
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

/// The detail query: one issue, its body, and one page of comments.
///
/// `first` is the comment page size, and one page is all this slice fetches —
/// the `[m]ore` affordance for longer threads is a later ticket.
const ISSUE_DETAIL_QUERY: &str = r"
query($owner:String!,$name:String!,$number:Int!,$first:Int!,$after:String){
  repository(owner:$owner,name:$name){
    issue(number:$number){
      number title body state createdAt updatedAt author{login}
      labels(first:20){nodes{name color}}
      comments(first:$first,after:$after){
        totalCount pageInfo{hasNextPage endCursor}
        nodes{author{login} createdAt body}
      }
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

/// One comment of an issue detail, in the order it was written.
///
/// Serializable because a fetched comment page is cached as JSON in one column,
/// exactly as the schema asks (`issue_comments.nodes_json`) — this type is the
/// viewer's own shape, not the wire's, so the cached page is the parsed comments
/// rather than a copy of the GraphQL response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueComment {
    pub author: Option<String>,
    /// When the comment was written, in unix seconds.
    pub created_at: Option<i64>,
    /// Markdown, rendered at draw time against the pane's current width.
    pub body: String,
}

/// The issue detail: body plus the comments fetched so far.
///
/// Cached separately from the list, and separately invalidated.
#[derive(Debug, Clone)]
pub struct IssueDetail {
    pub number: u64,
    pub title: String,
    /// Markdown, rendered at draw time against the pane's current width.
    pub body: String,
    pub state: String,
    pub updated_at: Option<i64>,
    pub author: Option<String>,
    pub labels: Vec<String>,
    /// How many comments the issue has, not how many arrived.
    pub comment_total_count: u64,
    pub comments: Vec<IssueComment>,
    /// Whether comments remain beyond the ones fetched.
    pub has_more_comments: bool,
    /// The cursor the next comment page would be asked for with. Cached with
    /// the page it ends, which is what makes paging resumable; asking for that
    /// page is a later ticket.
    pub comments_end_cursor: Option<String>,
    /// When this detail was fetched, in unix seconds.
    pub fetched_at: i64,
}

impl IssueDetail {
    /// The first label, which is all the header line has room for.
    pub fn first_label(&self) -> Option<&str> {
        self.labels.first().map(String::as_str)
    }
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
            // All three ways to provide one, in SPEC §11's words — `token_file`
            // included now that the config file it comes from exists.
            Self::NoToken => write!(
                f,
                "no GitHub token found · set GITHUB_TOKEN, run `gh auth login`, or set token_file"
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

        let envelope: Envelope<Repository> = self.post(&body)?;
        envelope.into_issue_list(slug)
    }

    /// One issue's body and its first page of comments.
    ///
    /// User-initiated only: `enter` on a row, and `n`/`p` from inside the detail
    /// view. Nothing else fetches, and nothing retries by itself.
    pub fn issue_detail(
        &self,
        slug: &Slug,
        number: u64,
        comment_page_size: u32,
    ) -> Result<IssueDetail, ApiError> {
        let body = json!({
            "query": ISSUE_DETAIL_QUERY,
            "variables": {
                "owner": slug.owner,
                "name": slug.name,
                "number": number,
                "first": comment_page_size,
                // Paging past the first hundred comments is a later ticket.
                "after": serde_json::Value::Null,
            }
        });

        let envelope: Envelope<RepositoryDetail> = self.post(&body)?;
        envelope.into_issue_detail(slug, number)
    }

    /// One POST, one response, one status mapping — shared by both queries.
    fn post<R: DeserializeOwned>(&self, body: &serde_json::Value) -> Result<Envelope<R>, ApiError> {
        let mut response = self
            .agent
            .post(&self.url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/json")
            .send_json(body)
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

        response
            .body_mut()
            .read_json()
            .map_err(|error| ApiError::Unexpected {
                message: error.to_string(),
            })
    }
}

/// A GraphQL response body, generic over the shape of `data.repository` — the
/// two queries differ only there.
#[derive(Debug, Deserialize)]
struct Envelope<R> {
    #[serde(default = "Option::default")]
    data: Option<Data<R>>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

impl<R> Envelope<R> {
    /// The repository the response carried, or the error it carried instead.
    ///
    /// `NOT_FOUND` covers both a missing repo and one this token cannot see;
    /// the API cannot distinguish them, so neither can the message.
    fn into_repository(self, missing: &str) -> Result<R, ApiError> {
        if let Some(repository) = self.data.and_then(|data| data.repository) {
            return Ok(repository);
        }
        let not_found = self
            .errors
            .iter()
            .any(|error| error.error_type.as_deref() == Some("NOT_FOUND"));
        if not_found {
            return Err(ApiError::NotFound {
                slug: missing.to_string(),
            });
        }
        let message = self
            .errors
            .first()
            .map(|error| error.message.clone())
            .unwrap_or_else(|| "empty response".to_string());
        Err(ApiError::Unexpected { message })
    }
}

impl Envelope<Repository> {
    fn into_issue_list(self, slug: &Slug) -> Result<IssueList, ApiError> {
        let repository = self.into_repository(&slug.to_string())?;

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
struct Data<R> {
    repository: Option<R>,
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

impl Envelope<RepositoryDetail> {
    fn into_issue_detail(self, slug: &Slug, number: u64) -> Result<IssueDetail, ApiError> {
        let missing = format!("{slug}#{number}");
        let repository = self.into_repository(&missing)?;
        // A repo that exists but has no such issue reads the same way: the
        // number is not there, or not visible to this token.
        let issue = repository
            .issue
            .ok_or(ApiError::NotFound { slug: missing })?;

        let comments = issue
            .comments
            .nodes
            .into_iter()
            .flatten()
            .map(|node| IssueComment {
                author: node.author.map(|author| author.login),
                created_at: node.created_at.as_deref().and_then(age::parse_timestamp),
                body: node.body,
            })
            .collect();

        Ok(IssueDetail {
            number: issue.number,
            title: issue.title,
            body: issue.body,
            state: issue.state,
            updated_at: issue.updated_at.as_deref().and_then(age::parse_timestamp),
            author: issue.author.map(|author| author.login),
            labels: issue
                .labels
                .nodes
                .into_iter()
                .flatten()
                .map(|label| label.name)
                .collect(),
            comment_total_count: issue.comments.total_count,
            comments,
            has_more_comments: issue.comments.page_info.has_next_page,
            comments_end_cursor: issue.comments.page_info.end_cursor,
            fetched_at: age::now(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct RepositoryDetail {
    issue: Option<IssueDetailNode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueDetailNode {
    number: u64,
    title: String,
    #[serde(default)]
    body: String,
    state: String,
    updated_at: Option<String>,
    author: Option<Author>,
    labels: Labels,
    comments: Comments,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comments {
    total_count: u64,
    #[serde(default)]
    page_info: PageInfo,
    #[serde(default)]
    nodes: Vec<Option<CommentNode>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    #[serde(default)]
    has_next_page: bool,
    /// Cached with the page, so the page after it can be asked for later.
    #[serde(default)]
    end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentNode {
    author: Option<Author>,
    created_at: Option<String>,
    #[serde(default)]
    body: String,
}
