use std::sync::Arc;

use anyhow::Result;
use axum::{Router, http::StatusCode, routing::get};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::bridge::BridgeServer;
use crate::mcp_server::BridgeMcpServer;

pub async fn run_http_server(
    bridge: BridgeServer,
    addr: &str,
    shutdown: CancellationToken,
) -> Result<()> {
    let service: StreamableHttpService<BridgeMcpServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(BridgeMcpServer::new(bridge.clone())),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig {
                stateful_mode: true,
                json_response: false,
                cancellation_token: shutdown.child_token(),
                ..Default::default()
            },
        );

    let router = Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_metadata_unsupported),
        )
        .route(
            "/.well-known/oauth-authorization-server/mcp",
            get(oauth_metadata_unsupported),
        )
        .route(
            "/mcp/.well-known/oauth-authorization-server",
            get(oauth_metadata_unsupported),
        )
        .route(
            "/.well-known/openid-configuration",
            get(oauth_metadata_unsupported),
        )
        .route(
            "/.well-known/openid-configuration/mcp",
            get(oauth_metadata_unsupported),
        )
        .route(
            "/mcp/.well-known/openid-configuration",
            get(oauth_metadata_unsupported),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth_metadata_unsupported),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth_metadata_unsupported),
        )
        .route(
            "/mcp/.well-known/oauth-protected-resource",
            get(oauth_metadata_unsupported),
        )
        .nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(addr).await?;

    axum::serve(listener, router)
        .with_graceful_shutdown({
            let shutdown = shutdown.clone();
            async move {
                shutdown.cancelled().await;
            }
        })
        .await?;

    Ok(())
}

async fn oauth_metadata_unsupported() -> axum::http::Response<axum::body::Body> {
    let body = serde_json::to_vec(&json!({})).expect("valid oauth unsupported json");

    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body))
        .expect("valid oauth unsupported response")
}
