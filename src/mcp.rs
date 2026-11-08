use crate::{
    bitbucket::{Bitbucket, Config},
    domain::*,
};
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use serde::Serialize;

#[derive(Clone)]
pub struct BitbucketServer {
    api: Bitbucket,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}
impl BitbucketServer {
    pub fn new(config: Config) -> Result<Self, String> {
        Ok(Self {
            api: Bitbucket::new(config)?,
            tool_router: Self::tool_router(),
        })
    }
}
fn output<T: Serialize>(api: &Bitbucket, r: Result<T, ToolError>) -> CallToolResult {
    match r {
        Ok(value) => CallToolResult::structured(api.sanitize(value)),
        Err(e) => CallToolResult::structured_error(serde_json::json!({"error":e.public()})),
    }
}
#[tool_router]
impl BitbucketServer {
    #[tool(
        description = "List pull requests in a repository. Returns bounded values and an actionable next_page token."
    )]
    async fn list_pull_requests(
        &self,
        Parameters(input): Parameters<ListPullRequestsInput>,
    ) -> CallToolResult {
        output(&self.api, self.api.list_pull_requests(input).await)
    }
    #[tool(
        description = "Read one pull request by repository slug and positive ID in the configured workspace."
    )]
    async fn get_pull_request(
        &self,
        Parameters(input): Parameters<PullRequestTarget>,
    ) -> CallToolResult {
        output(&self.api, self.api.get_pull_request(input).await)
    }
    #[tool(
        description = "Read the bounded raw pull request diff. The result reports truncation and whether inline placement is supported; unsupported formats remain readable but cannot be used for inline comments."
    )]
    async fn get_pull_request_diff(
        &self,
        Parameters(input): Parameters<PullRequestTarget>,
    ) -> CallToolResult {
        output(&self.api, self.api.get_pull_request_diff(input).await)
    }
    #[tool(
        description = "List files changed by a pull request. Returns bounded file metadata and an actionable next_page token."
    )]
    async fn list_pull_request_files(
        &self,
        Parameters(input): Parameters<ListPullRequestFilesInput>,
    ) -> CallToolResult {
        output(&self.api, self.api.list_pull_request_files(input).await)
    }
    #[tool(
        description = "Read the bounded diff for one pull request file. The result reports truncation and does not imply inline-comment placement support."
    )]
    async fn get_pull_request_file_diff(
        &self,
        Parameters(input): Parameters<GetPullRequestFileDiffInput>,
    ) -> CallToolResult {
        output(&self.api, self.api.get_pull_request_file_diff(input).await)
    }
    #[tool(
        description = "List pull request comments. Returns bounded values and an actionable next_page token."
    )]
    async fn list_pull_request_comments(
        &self,
        Parameters(input): Parameters<ListCommentsInput>,
    ) -> CallToolResult {
        output(&self.api, self.api.list_pull_request_comments(input).await)
    }
    #[tool(
        description = "Create a same-repository non-draft pull request from explicit branches. Reviewer entries must be Bitbucket UUIDs."
    )]
    async fn create_pull_request(
        &self,
        Parameters(input): Parameters<CreatePullRequestInput>,
    ) -> CallToolResult {
        output(&self.api, self.api.create_pull_request(input).await)
    }
    #[tool(
        description = "Create a general comment, reply, or single-line comment anchored to the current diff."
    )]
    async fn create_pull_request_comment(
        &self,
        Parameters(input): Parameters<CreateCommentInput>,
    ) -> CallToolResult {
        output(&self.api, self.api.create_pull_request_comment(input).await)
    }
    #[tool(description = "Approve a pull request as the authenticated token owner.")]
    async fn approve_pull_request(
        &self,
        Parameters(input): Parameters<PullRequestTarget>,
    ) -> CallToolResult {
        output(&self.api, self.api.approve_pull_request(input).await)
    }
    #[tool(description = "Withdraw the authenticated token owner's pull request approval.")]
    async fn withdraw_pull_request_approval(
        &self,
        Parameters(input): Parameters<PullRequestTarget>,
    ) -> CallToolResult {
        output(
            &self.api,
            self.api.withdraw_pull_request_approval(input).await,
        )
    }
    #[tool(
        description = "Report the current approval count and approved participant identities. This is not a merge-readiness check."
    )]
    async fn get_pull_request_approvals(
        &self,
        Parameters(input): Parameters<PullRequestTarget>,
    ) -> CallToolResult {
        output(&self.api, self.api.get_pull_request_approvals(input).await)
    }
}
#[tool_handler]
impl ServerHandler for BitbucketServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions("Bitbucket Cloud pull-request tools. All writes use the configured token owner; approval consent is client-specific.")
    }
}
