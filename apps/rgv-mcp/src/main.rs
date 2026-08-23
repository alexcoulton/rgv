use anyhow::Context as _;
use clap::Parser as _;
use rgv_mcp::{RgvMcpArgs, RgvMcpServer};
use rmcp::{ServiceExt as _, transport::stdio};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rgv_mcp=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = RgvMcpArgs::parse();
    let server = RgvMcpServer::from_args(args).context("could not configure RGV MCP")?;
    if server.viewer_is_compatible().await {
        tracing::info!("compatible RGV window found during startup probe");
    } else {
        tracing::warn!("RGV window is unavailable or incompatible; tool calls can retry later");
    }
    tracing::info!("RGV MCP server ready on stdio");
    let service = server
        .serve(stdio())
        .await
        .context("could not start MCP stdio transport")?;
    service
        .waiting()
        .await
        .context("MCP stdio service stopped unexpectedly")?;
    Ok(())
}
