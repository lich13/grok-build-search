use grok_build_search_mcp::{GrokClient, GrokConfig, GrokLocator, GrokMcpServer, SearchService};
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init()
        .ok();

    let service = match GrokLocator::from_environment()
        .locate()
        .and_then(|binary| GrokClient::new(GrokConfig::new(binary)))
    {
        Ok(client) => SearchService::new(client),
        Err(error) => {
            tracing::warn!(%error, "Grok backend is unavailable at startup");
            SearchService::unavailable(error)
        }
    };
    let server = GrokMcpServer::new(service)
        .serve(rmcp::transport::stdio())
        .await?;
    server.waiting().await?;
    Ok(())
}
