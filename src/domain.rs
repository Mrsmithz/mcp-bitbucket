use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("Invalid input: {0}")]
    Invalid(String),
    #[error("Bitbucket rejected the request")]
    Rejected(u16),
    #[error("Bitbucket response was invalid or exceeded the size limit")]
    Malformed,
    #[error("Unsafe upstream navigation was blocked")]
    UnsafeNavigation,
    #[error("Bitbucket request failed")]
    Transport,
    #[error("The write may have completed; check Bitbucket before retrying")]
    OutcomeUnknown,
}
#[derive(Serialize)]
pub struct PublicToolError<'a> {
    pub code: &'a str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
}
impl ToolError {
    pub fn public(&self) -> PublicToolError<'static> {
        let (code, message, http_status) = match self {
            Self::Invalid(message) => ("invalid_input", message.clone(), None),
            Self::Rejected(status) => (
                "upstream_rejected",
                "Bitbucket rejected the request".into(),
                Some(*status),
            ),
            Self::Malformed => ("upstream_invalid_response", self.to_string(), None),
            Self::UnsafeNavigation => ("unsafe_navigation", self.to_string(), None),
            Self::Transport => ("upstream_transport_error", self.to_string(), None),
            Self::OutcomeUnknown => ("outcome_unknown", self.to_string(), None),
        };
        PublicToolError {
            code,
            message,
            http_status,
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PullRequestTarget {
    pub repo_slug: String,
    pub pull_request_id: u64,
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchRepositoriesInput {
    pub query: Option<String>,
    pub page: Option<String>,
    pub pagelen: Option<u8>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Repository {
    pub uuid: String,
    pub name: String,
    pub slug: String,
    pub workspace: String,
    pub url: String,
    pub is_private: bool,
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListPullRequestsInput {
    pub repo_slug: String,
    pub state: Option<String>,
    pub page: Option<String>,
    pub pagelen: Option<u8>,
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreatePullRequestInput {
    pub repo_slug: String,
    pub title: String,
    pub source_branch: String,
    pub destination_branch: String,
    pub description: Option<String>,
    pub reviewers: Option<Vec<String>>,
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommentInput {
    General {
        body: String,
    },
    Reply {
        body: String,
        parent_id: u64,
    },
    Inline {
        body: String,
        path: String,
        side: DiffSide,
        line: u64,
    },
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    Old,
    New,
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCommentInput {
    pub repo_slug: String,
    pub pull_request_id: u64,
    pub comment: CommentInput,
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListPullRequestFilesInput {
    pub repo_slug: String,
    pub pull_request_id: u64,
    pub page: Option<String>,
    pub pagelen: Option<u8>,
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetPullRequestFileDiffInput {
    pub repo_slug: String,
    pub pull_request_id: u64,
    pub path: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct PullRequestFile {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub status: Option<String>,
    pub lines_added: Option<u64>,
    pub lines_removed: Option<u64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct PullRequestFileDiff {
    pub path: String,
    pub diff: String,
    pub truncated: bool,
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListCommentsInput {
    pub repo_slug: String,
    pub pull_request_id: u64,
    pub page: Option<String>,
    pub pagelen: Option<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Page<T> {
    pub values: Vec<T>,
    pub next_page: Option<String>,
    pub truncated: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct PullRequest {
    pub id: u64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub source_branch: String,
    pub destination_branch: String,
    pub author: Option<String>,
    pub url: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct PullRequestComment {
    pub id: u64,
    pub body: String,
    pub user: Option<String>,
    pub parent_id: Option<u64>,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub side: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Approval {
    pub uuid: Option<String>,
    pub display_name: Option<String>,
    pub account_id: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Approvals {
    pub count: usize,
    pub approvers: Vec<Approval>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ApprovalMutation {
    pub approved: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct WithdrawalMutation {
    pub withdrawn: bool,
}

pub fn validate_segment(label: &str, s: &str) -> Result<(), ToolError> {
    if s.is_empty()
        || s.len() > 128
        || s == "."
        || s == ".."
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.~".contains(&b))
    {
        return Err(ToolError::Invalid(format!(
            "{label} must be a safe non-empty path segment"
        )));
    }
    Ok(())
}
pub fn validate_branch(s: &str) -> Result<(), ToolError> {
    if s.trim().is_empty()
        || s.len() > 255
        || s.bytes().any(|b| b.is_ascii_control())
        || s.contains('\\')
    {
        return Err(ToolError::Invalid(
            "branch must be non-empty, bounded, and contain no control characters".into(),
        ));
    }
    Ok(())
}
pub fn validate_repo_path(s: &str) -> Result<(), ToolError> {
    if s.is_empty()
        || s.len() > 512
        || s.starts_with('/')
        || s.contains('\\')
        || s.bytes().any(|b| b.is_ascii_control())
        || s.split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ToolError::Invalid(
            "path must contain safe repository path segments".into(),
        ));
    }
    Ok(())
}
pub fn validate_inline_path(s: &str) -> Result<(), ToolError> {
    validate_repo_path(s)?;
    if s.contains('"') {
        return Err(ToolError::Invalid(
            "inline path must not contain a quote".into(),
        ));
    }
    Ok(())
}
pub fn validate_pr(id: u64) -> Result<(), ToolError> {
    if id == 0 || id > i64::MAX as u64 {
        return Err(ToolError::Invalid(
            "pull_request_id must be a positive safe integer".into(),
        ));
    }
    Ok(())
}
pub fn validate_text(label: &str, s: &str, max: usize) -> Result<(), ToolError> {
    if s.trim().is_empty() || s.len() > max {
        return Err(ToolError::Invalid(format!("{label} is empty or too long")));
    }
    Ok(())
}
