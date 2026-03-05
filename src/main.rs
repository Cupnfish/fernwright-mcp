use std::env;
use std::time::Duration;

use anyhow::Result;

mod bridge;
mod mcp;

use bridge::BridgeServer;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let bridge_addr =
        env::var("PLAYWRIGHT_MCP_BRIDGE_ADDR").unwrap_or_else(|_| "127.0.0.1:17373".to_owned());
    let request_timeout_ms = env::var("PLAYWRIGHT_MCP_REQUEST_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60_000);

    let bridge = BridgeServer::new(Duration::from_millis(request_timeout_ms));
    let bridge_for_listener = bridge.clone();

    let listener_addr = bridge_addr.clone();
    let listener_task = tokio::spawn(async move {
        if let Err(err) = bridge_for_listener.run_ws_listener(&listener_addr).await {
            eprintln!("WebSocket listener failed: {err:#}");
        }
    });

    eprintln!(
        "playright-mcp server started. Listening for extension bridge on ws://{}",
        bridge_addr
    );

    let result = mcp::run_stdio_server(bridge).await;
    listener_task.abort();
    result
}
