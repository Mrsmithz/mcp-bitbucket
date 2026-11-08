use crate::{diff::Diff, domain::*};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Method, Response, StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{env, net::IpAddr, time::Duration};
use url::Url;

mod file_review;

const MAX_BODY: usize = 1_000_000;
const MAX_DIFF: usize = 500_000;
const MAX_REDIRECTS: usize = 3;
const MAX_PAGE: usize = 50;
#[derive(Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    Read,
    Mutation,
}

#[derive(Clone)]
struct Workspace(String);

#[derive(Clone)]
pub struct Config {
    root: Url,
    workspace: Workspace,
    auth: String,
    secret: String,
}
impl Config {
    pub fn from_env() -> Result<Self, String> {
        let email =
            env::var("BITBUCKET_EMAIL").map_err(|_| "BITBUCKET_EMAIL is required".to_string())?;
        let token = env::var("BITBUCKET_API_TOKEN")
            .map_err(|_| "BITBUCKET_API_TOKEN is required".to_string())?;
        let workspace = env::var("BITBUCKET_WORKSPACE")
            .map_err(|_| "BITBUCKET_WORKSPACE is required".to_string())?;
        Self::new(
            &email,
            &token,
            &workspace,
            &env::var("BITBUCKET_BASE_URL")
                .unwrap_or_else(|_| "https://api.bitbucket.org/2.0".into()),
        )
    }
    pub fn new(email: &str, token: &str, workspace: &str, base: &str) -> Result<Self, String> {
        if email.is_empty()
            || token.is_empty()
            || email.contains(':')
            || email.bytes().any(|b| b.is_ascii_control())
            || token.bytes().any(|b| b.is_ascii_control())
        {
            return Err("credentials are missing or invalid".into());
        }
        validate_segment("workspace", workspace)
            .map_err(|_| "BITBUCKET_WORKSPACE is invalid".to_string())?;
        let workspace = Workspace(workspace.to_owned());
        let root = Url::parse(base).map_err(|_| "BITBUCKET_BASE_URL is invalid".to_string())?;
        let loopback = root
            .host_str()
            .and_then(|h| h.parse::<IpAddr>().ok())
            .is_some_and(|a| a.is_loopback())
            || root.host_str() == Some("localhost");
        if root.query().is_some()
            || root.fragment().is_some()
            || !root.username().is_empty()
            || root.password().is_some()
            || (root.scheme() != "https" && !(root.scheme() == "http" && loopback))
        {
            return Err("BITBUCKET_BASE_URL must be HTTPS (loopback HTTP is allowed for local tests), without credentials, query, or fragment".into());
        }
        let mut path = root.path().trim_end_matches('/').to_string();
        if path.is_empty() {
            path = "/".into();
        }
        root.path_segments();
        let mut root = root;
        root.set_path(&path);
        Ok(Self {
            root,
            workspace,
            auth: format!("Basic {}", STANDARD.encode(format!("{email}:{token}"))),
            secret: token.to_owned(),
        })
    }
}

#[derive(Clone)]
pub struct Bitbucket {
    config: Config,
    http: reqwest::Client,
}
impl Bitbucket {
    pub fn new(config: Config) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| "HTTP client initialization failed".to_string())?;
        Ok(Self { config, http })
    }
    pub fn sanitize<T: serde::Serialize>(&self, value: T) -> Value {
        let mut value = serde_json::to_value(value).unwrap_or(Value::Null);
        redact(&mut value, &self.config.secret, &self.config.auth);
        value
    }
    fn url(&self, suffix: &[&str]) -> Result<Url, ToolError> {
        let mut url = self.config.root.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| ToolError::UnsafeNavigation)?;
            for s in suffix {
                segments.push(s);
            }
        }
        Ok(url)
    }
    fn target(&self, repo: &str, pr: Option<u64>) -> Result<Url, ToolError> {
        validate_segment("repo_slug", repo)?;
        let mut p = vec!["repositories", self.config.workspace.0.as_str(), repo];
        let id = pr.map(|n| n.to_string());
        if let Some(id) = &id {
            p.extend(["pullrequests", id.as_str()]);
        }
        self.url(&p)
    }
    fn safe_get_redirect(&self, from: &Url, to: &Url) -> bool {
        let base_path = self.config.root.path();
        let origin_ok = from.scheme() == to.scheme()
            && from.host_str() == to.host_str()
            && from.port_or_known_default() == to.port_or_known_default();
        let no_sensitive_query = to.username().is_empty() && to.password().is_none();
        let prefix_ok = base_path == "/"
            || to.path() == base_path
            || to.path().starts_with(&format!("{base_path}/"));
        let path_ok = prefix_ok && !to.path().split('/').any(|p| p == ".." || p == ".");
        origin_ok && no_sensitive_query && path_ok
    }
    async fn send_once(
        &self,
        method: &Method,
        url: &Url,
        body: Option<&Value>,
        kind: RequestKind,
    ) -> Result<Response, ToolError> {
        let mut req = self
            .http
            .request(method.clone(), url.clone())
            .header(header::AUTHORIZATION, &self.config.auth)
            .header(header::ACCEPT, "application/json");
        if let Some(body) = body {
            req = req.json(body);
        }
        req.send().await.map_err(|_| {
            if kind == RequestKind::Read {
                ToolError::Transport
            } else {
                ToolError::OutcomeUnknown
            }
        })
    }
    async fn send(
        &self,
        method: Method,
        mut url: Url,
        body: Option<Value>,
        kind: RequestKind,
    ) -> Result<Response, ToolError> {
        for hop in 0..=if kind == RequestKind::Read {
            MAX_REDIRECTS
        } else {
            0
        } {
            let response = self.send_once(&method, &url, body.as_ref(), kind).await?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            if kind != RequestKind::Read || hop == MAX_REDIRECTS {
                return Err(if kind == RequestKind::Read {
                    ToolError::UnsafeNavigation
                } else {
                    ToolError::OutcomeUnknown
                });
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(ToolError::UnsafeNavigation)?;
            let next = url
                .join(location)
                .map_err(|_| ToolError::UnsafeNavigation)?;
            if !self.safe_get_redirect(&url, &next) {
                return Err(ToolError::UnsafeNavigation);
            }
            url = next;
        }
        Err(ToolError::UnsafeNavigation)
    }
    async fn bounded_body(response: Response, kind: RequestKind) -> Result<Vec<u8>, ToolError> {
        let status = response.status();
        if !status.is_success() {
            return Err(
                if kind == RequestKind::Mutation && status.is_server_error() {
                    ToolError::OutcomeUnknown
                } else {
                    ToolError::Rejected(status.as_u16())
                },
            );
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BODY as u64)
        {
            return Err(if kind == RequestKind::Mutation {
                ToolError::OutcomeUnknown
            } else {
                ToolError::Malformed
            });
        }
        let mut stream = response.bytes_stream();
        let mut out = Vec::new();
        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| {
                if kind == RequestKind::Mutation {
                    ToolError::OutcomeUnknown
                } else {
                    ToolError::Malformed
                }
            })?;
            if out.len() + chunk.len() > MAX_BODY {
                return Err(if kind == RequestKind::Mutation {
                    ToolError::OutcomeUnknown
                } else {
                    ToolError::Malformed
                });
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }
    async fn get_json(&self, url: Url) -> Result<Value, ToolError> {
        let response = self.send(Method::GET, url, None, RequestKind::Read).await?;
        serde_json::from_slice(&Self::bounded_body(response, RequestKind::Read).await?)
            .map_err(|_| ToolError::Malformed)
    }
    async fn post_json(&self, url: Url, body: Value) -> Result<Value, ToolError> {
        let response = self
            .send(Method::POST, url, Some(body), RequestKind::Mutation)
            .await?;
        serde_json::from_slice(&Self::bounded_body(response, RequestKind::Mutation).await?)
            .map_err(|_| ToolError::OutcomeUnknown)
    }
    fn page_url(
        &self,
        mut url: Url,
        page: Option<&str>,
        pagelen: Option<u8>,
    ) -> Result<Url, ToolError> {
        if let Some(p) = page {
            if p.is_empty()
                || p.len() > 256
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.~".contains(&b))
            {
                return Err(ToolError::Invalid(
                    "page must be an opaque safe page token".into(),
                ));
            }
            url.query_pairs_mut().append_pair("page", p);
        }
        let n = pagelen.unwrap_or(MAX_PAGE as u8);
        if n == 0 {
            return Err(ToolError::Invalid(
                "pagelen must be between 1 and 50".into(),
            ));
        }
        url.query_pairs_mut()
            .append_pair("pagelen", &n.min(MAX_PAGE as u8).to_string());
        Ok(url)
    }
    fn page<T: for<'de> Deserialize<'de>>(
        &self,
        value: Value,
    ) -> Result<(Vec<T>, Option<String>), ToolError> {
        let vals = value
            .get("values")
            .and_then(Value::as_array)
            .ok_or(ToolError::Malformed)?;
        let values = vals
            .iter()
            .take(MAX_PAGE)
            .cloned()
            .map(serde_json::from_value)
            .collect::<Result<Vec<T>, _>>()
            .map_err(|_| ToolError::Malformed)?;
        let next = value
            .get("next")
            .and_then(Value::as_str)
            .map(|s| {
                Url::parse(s)
                    .ok()
                    .and_then(|u| {
                        u.query_pairs()
                            .find(|(k, _)| k == "page")
                            .map(|(_, v)| v.into_owned())
                    })
                    .filter(|p| valid_page_token(p))
                    .ok_or(ToolError::Malformed)
            })
            .transpose()?;
        Ok((values, next))
    }
    pub async fn list_pull_requests(
        &self,
        input: ListPullRequestsInput,
    ) -> Result<Page<PullRequest>, ToolError> {
        let mut url = self.target(&input.repo_slug, None)?;
        url.path_segments_mut()
            .map_err(|_| ToolError::UnsafeNavigation)?
            .push("pullrequests");
        let mut url = self.page_url(url, input.page.as_deref(), input.pagelen)?;
        if let Some(state) = input.state {
            if !["OPEN", "MERGED", "DECLINED", "SUPERSEDED"].contains(&state.as_str()) {
                return Err(ToolError::Invalid(
                    "state must be OPEN, MERGED, DECLINED, or SUPERSEDED".into(),
                ));
            }
            url.query_pairs_mut().append_pair("state", &state);
        }
        let data = self.get_json(url).await?;
        let (values, next_page) = self.page::<WirePr>(data.clone())?;
        Ok(Page {
            truncated: data
                .get("values")
                .and_then(Value::as_array)
                .is_some_and(|v| v.len() > MAX_PAGE),
            values: values.into_iter().map(Into::into).collect(),
            next_page,
        })
    }
    pub async fn get_pull_request(&self, t: PullRequestTarget) -> Result<PullRequest, ToolError> {
        validate_pr(t.pull_request_id)?;
        self.get_json(self.target(&t.repo_slug, Some(t.pull_request_id))?)
            .await
            .and_then(|v| {
                serde_json::from_value::<WirePr>(v)
                    .map(Into::into)
                    .map_err(|_| ToolError::Malformed)
            })
    }
    pub async fn get_pull_request_diff(&self, t: PullRequestTarget) -> Result<Diff, ToolError> {
        validate_pr(t.pull_request_id)?;
        let u = self.target(&t.repo_slug, Some(t.pull_request_id))?;
        let mut u = u;
        u.path_segments_mut()
            .map_err(|_| ToolError::UnsafeNavigation)?
            .push("diff");
        let r = self.send(Method::GET, u, None, RequestKind::Read).await?;
        let (bytes, truncated) = bounded_diff(r).await?;
        let text = String::from_utf8(bytes).map_err(|_| ToolError::Malformed)?;
        Ok(Diff::new(text, truncated))
    }
    pub async fn list_pull_request_comments(
        &self,
        input: ListCommentsInput,
    ) -> Result<Page<PullRequestComment>, ToolError> {
        validate_pr(input.pull_request_id)?;
        let mut u = self.target(&input.repo_slug, Some(input.pull_request_id))?;
        u.path_segments_mut()
            .map_err(|_| ToolError::UnsafeNavigation)?
            .push("comments");
        let u = self.page_url(u, input.page.as_deref(), input.pagelen)?;
        let d = self.get_json(u).await?;
        let (values, next_page) = self.page::<WireComment>(d.clone())?;
        Ok(Page {
            truncated: d["values"].as_array().is_some_and(|v| v.len() > MAX_PAGE),
            values: values.into_iter().map(Into::into).collect(),
            next_page,
        })
    }
    pub async fn create_pull_request(
        &self,
        i: CreatePullRequestInput,
    ) -> Result<PullRequest, ToolError> {
        validate_text("title", &i.title, 300)?;
        validate_branch(&i.source_branch)?;
        validate_branch(&i.destination_branch)?;
        if let Some(d) = &i.description {
            validate_text("description", d, 100_000)?
        }
        let reviewers = i
            .reviewers
            .unwrap_or_default()
            .into_iter()
            .map(|u| {
                if !is_uuid(&u) {
                    return Err(ToolError::Invalid(
                        "reviewers must be UUIDs, optionally wrapped in one matching brace pair"
                            .into(),
                    ));
                }
                let id = u
                    .strip_prefix('{')
                    .and_then(|s| s.strip_suffix('}'))
                    .unwrap_or(&u);
                Ok(json!({"uuid":format!("{{{id}}}")}))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let url = self.target(&i.repo_slug, None)?;
        let mut url = url;
        url.path_segments_mut()
            .map_err(|_| ToolError::UnsafeNavigation)?
            .push("pullrequests");
        let data=self.post_json(url,json!({"title":i.title,"source":{"branch":{"name":i.source_branch}},"destination":{"branch":{"name":i.destination_branch}},"description":i.description,"reviewers":reviewers})).await?;
        serde_json::from_value::<WirePr>(data)
            .map(Into::into)
            .map_err(|_| ToolError::OutcomeUnknown)
    }
    pub async fn create_pull_request_comment(
        &self,
        i: CreateCommentInput,
    ) -> Result<PullRequestComment, ToolError> {
        validate_pr(i.pull_request_id)?;
        let url = self.target(&i.repo_slug, Some(i.pull_request_id))?;
        let mut url = url;
        url.path_segments_mut()
            .map_err(|_| ToolError::UnsafeNavigation)?
            .push("comments");
        let body = match i.comment {
            CommentInput::General { body } => {
                validate_text("body", &body, 100_000)?;
                json!({"content":{"raw":body}})
            }
            CommentInput::Reply { body, parent_id } => {
                validate_text("body", &body, 100_000)?;
                validate_pr(parent_id)?;
                json!({"content":{"raw":body},"parent":{"id":parent_id}})
            }
            CommentInput::Inline {
                body,
                path,
                side,
                line,
            } => {
                validate_text("body", &body, 100_000)?;
                validate_inline_path(&path)?;
                let diff = self
                    .get_pull_request_diff(PullRequestTarget {
                        repo_slug: i.repo_slug.clone(),
                        pull_request_id: i.pull_request_id,
                    })
                    .await?;
                let side_s = match side {
                    DiffSide::Old => "old",
                    DiffSide::New => "new",
                };
                if !diff.has_position(&path, side_s, line) {
                    return Err(ToolError::Invalid("inline position is not a supported line in the latest pull request diff; no comment was posted".into()));
                }
                let mut inline = serde_json::Map::new();
                inline.insert("path".into(), json!(path));
                inline.insert(
                    if side_s == "old" { "from" } else { "to" }.into(),
                    json!(line),
                );
                json!({"content":{"raw":body},"inline":inline})
            }
        };
        let d = self.post_json(url, body).await?;
        serde_json::from_value::<WireComment>(d)
            .map(Into::into)
            .map_err(|_| ToolError::OutcomeUnknown)
    }
    pub async fn approve_pull_request(
        &self,
        t: PullRequestTarget,
    ) -> Result<ApprovalMutation, ToolError> {
        validate_pr(t.pull_request_id)?;
        let mut u = self.target(&t.repo_slug, Some(t.pull_request_id))?;
        u.path_segments_mut()
            .map_err(|_| ToolError::UnsafeNavigation)?
            .push("approve");
        let response = self
            .send(Method::POST, u, None, RequestKind::Mutation)
            .await?;
        let body = Self::bounded_body(response, RequestKind::Mutation).await?;
        let participant = serde_json::from_slice::<WireParticipant>(&body)
            .map_err(|_| ToolError::OutcomeUnknown)?;
        if !participant.approved {
            return Err(ToolError::OutcomeUnknown);
        }
        Ok(ApprovalMutation { approved: true })
    }
    pub async fn withdraw_pull_request_approval(
        &self,
        t: PullRequestTarget,
    ) -> Result<WithdrawalMutation, ToolError> {
        validate_pr(t.pull_request_id)?;
        let mut u = self.target(&t.repo_slug, Some(t.pull_request_id))?;
        u.path_segments_mut()
            .map_err(|_| ToolError::UnsafeNavigation)?
            .push("approve");
        let r = self
            .send(Method::DELETE, u, None, RequestKind::Mutation)
            .await?;
        if r.status() != StatusCode::NO_CONTENT {
            let _ = Self::bounded_body(r, RequestKind::Mutation).await?;
            return Err(ToolError::OutcomeUnknown);
        }
        let body = Self::bounded_body(r, RequestKind::Mutation).await?;
        if !body.is_empty() {
            return Err(ToolError::OutcomeUnknown);
        }
        Ok(WithdrawalMutation { withdrawn: true })
    }
    pub async fn get_pull_request_approvals(
        &self,
        t: PullRequestTarget,
    ) -> Result<Approvals, ToolError> {
        validate_pr(t.pull_request_id)?;
        let d = self
            .get_json(self.target(&t.repo_slug, Some(t.pull_request_id))?)
            .await?;
        let values = d
            .get("participants")
            .and_then(Value::as_array)
            .ok_or(ToolError::Malformed)?;
        let mut approvers = Vec::new();
        for participant in values {
            let participant = serde_json::from_value::<WireParticipant>(participant.clone())
                .map_err(|_| ToolError::Malformed)?;
            if participant.approved {
                let user = participant.user.ok_or(ToolError::Malformed)?;
                let uuid = user.uuid.filter(|value| !value.trim().is_empty());
                let account_id = user.account_id.filter(|value| !value.trim().is_empty());
                if uuid.is_none() && account_id.is_none() {
                    return Err(ToolError::Malformed);
                }
                approvers.push(Approval {
                    uuid,
                    display_name: user.display_name,
                    account_id,
                });
            }
        }
        Ok(Approvals {
            count: approvers.len(),
            approvers,
        })
    }
}

fn valid_page_token(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 256
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.~".contains(&b))
}
async fn bounded_diff(response: Response) -> Result<(Vec<u8>, bool), ToolError> {
    if !response.status().is_success() {
        return Err(ToolError::Rejected(response.status().as_u16()));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ToolError::Malformed)?;
        let remaining = MAX_DIFF.saturating_add(1).saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if bytes.len() > MAX_DIFF {
            break;
        }
    }
    let truncated = bytes.len() > MAX_DIFF;
    if truncated {
        bytes.truncate(MAX_DIFF);
        if let Some(boundary) = bytes.iter().rposition(|b| *b == b'\n') {
            bytes.truncate(boundary + 1);
        }
        while std::str::from_utf8(&bytes).is_err() {
            bytes.pop();
        }
    }
    Ok((bytes, truncated))
}
fn redact(v: &mut Value, secret: &str, auth: &str) {
    match v {
        Value::String(s) => {
            let encoded = STANDARD.encode(secret);
            *s = s
                .replace(secret, "[REDACTED]")
                .replace(auth, "[REDACTED]")
                .replace(auth.trim_start_matches("Basic "), "[REDACTED]")
                .replace(&encoded, "[REDACTED]");
        }
        Value::Array(a) => {
            for x in a {
                redact(x, secret, auth)
            }
        }
        Value::Object(m) => {
            let entries = std::mem::take(m);
            for (key, mut value) in entries {
                redact(&mut value, secret, auth);
                let key = key
                    .replace(secret, "[REDACTED]")
                    .replace(auth, "[REDACTED]")
                    .replace(auth.trim_start_matches("Basic "), "[REDACTED]")
                    .replace(&STANDARD.encode(secret), "[REDACTED]");
                m.insert(key, value);
            }
        }
        _ => {}
    }
}
fn is_uuid(s: &str) -> bool {
    let s = s
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(s);
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
fn safe_pr_url(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("bitbucket.org")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url.to_string())
}
#[derive(Deserialize)]
struct WireParticipant {
    approved: bool,
    user: Option<WireUser>,
}
#[derive(Deserialize)]
struct WireUser {
    uuid: Option<String>,
    account_id: Option<String>,
    display_name: Option<String>,
}
#[derive(Deserialize)]
struct WirePr {
    id: std::num::NonZeroU64,
    #[serde(deserialize_with = "nonempty_string")]
    title: String,
    description: Option<String>,
    #[serde(deserialize_with = "nonempty_string")]
    state: String,
    source: WireBranchContainer,
    destination: WireBranchContainer,
    author: Option<WireUser>,
    links: Option<Value>,
}
#[derive(Deserialize)]
struct WireBranchContainer {
    branch: WireBranch,
}
#[derive(Deserialize)]
struct WireBranch {
    #[serde(deserialize_with = "branch_name")]
    name: String,
}
fn nonempty_string<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.trim().is_empty() {
        return Err(serde::de::Error::custom("string must not be empty"));
    }
    Ok(value)
}
fn branch_name<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    validate_branch(&value).map_err(serde::de::Error::custom)?;
    Ok(value)
}
impl From<WirePr> for PullRequest {
    fn from(v: WirePr) -> Self {
        Self {
            id: v.id.get(),
            title: v.title,
            description: v.description,
            state: v.state,
            source_branch: v.source.branch.name,
            destination_branch: v.destination.branch.name,
            author: v.author.and_then(|a| a.display_name),
            url: v
                .links
                .and_then(|l| l["html"]["href"].as_str().and_then(safe_pr_url)),
        }
    }
}
#[derive(Deserialize)]
struct WireComment {
    id: std::num::NonZeroU64,
    content: WireContent,
    user: Option<WireUser>,
    parent: Option<WireParent>,
    inline: Option<WireInline>,
}
#[derive(Deserialize)]
struct WireContent {
    raw: String,
}
#[derive(Deserialize)]
struct WireParent {
    id: std::num::NonZeroU64,
}
#[derive(Deserialize)]
struct WireInline {
    path: String,
    from: Option<std::num::NonZeroU64>,
    to: Option<std::num::NonZeroU64>,
}
impl From<WireComment> for PullRequestComment {
    fn from(v: WireComment) -> Self {
        let (path, line, side) = match v.inline {
            Some(inline) => {
                let (line, side) = if let Some(to) = inline.to {
                    (Some(to.get()), Some("new".into()))
                } else if let Some(from) = inline.from {
                    (Some(from.get()), Some("old".into()))
                } else {
                    (None, None)
                };
                (Some(inline.path), line, side)
            }
            None => (None, None, None),
        };
        Self {
            id: v.id.get(),
            body: v.content.raw,
            user: v.user.and_then(|user| user.display_name),
            parent_id: v.parent.map(|parent| parent.id.get()),
            path,
            line,
            side,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_is_required_and_rejects_unsafe_segments_without_echoing_values() {
        let base = "https://api.bitbucket.org/2.0";
        assert!(Config::new("a@b.test", "token", "acme", base).is_ok());
        for workspace in ["", "   ", "../unsafe", "."] {
            assert!(matches!(
                Config::new("a@b.test", "token", workspace, base),
                Err(error) if error == "BITBUCKET_WORKSPACE is invalid"
            ));
        }
    }

    #[test]
    fn unsafe_pr_urls_are_not_exposed() {
        assert!(safe_pr_url("https://user:secret@bitbucket.org/repo/pull/1").is_none());
        assert!(safe_pr_url("https://bitbucket.org/repo/pull/1?token=secret").is_none());
        assert_eq!(
            safe_pr_url("https://bitbucket.org/acme/repo/pull-requests/1"),
            Some("https://bitbucket.org/acme/repo/pull-requests/1".into())
        );
    }
    #[test]
    fn safe_uuid() {
        assert!(is_uuid("{504c3b62-8120-4f0c-a7bc-87800b9d6f70}"));
        assert!(!is_uuid("me"))
    }
}
