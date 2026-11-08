use mcp_bitbucket::bitbucket;
use mcp_bitbucket::mcp::BitbucketServer;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = bitbucket::Config::from_env().map_err(|message| anyhow::anyhow!(message))?;
    let service = BitbucketServer::new(config).map_err(|message| anyhow::anyhow!(message))?;
    let running = service.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}
