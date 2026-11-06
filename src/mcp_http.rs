//! Streamable HTTP MCP server (`POST /mcp`, `GET /healthz`) with constant-time Bearer Token auth.

use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::tools::JcodeMcpService;
use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;

/// Configuration for the Streamable HTTP MCP server.
#[derive(Debug, Clone)]
pub struct HttpServerConfig {
    pub bind_addr: SocketAddr,
    /// Optional Bearer Token required on `/mcp` requests.
    pub bearer_token: Option<String>,
}

#[derive(Clone)]
struct AppState {
    service: JcodeMcpService,
    bearer_token: Option<String>,
}

/// Build the Axum `Router` for testing or serving.
pub fn build_router(service: JcodeMcpService, bearer_token: Option<String>) -> Router {
    let state = Arc::new(AppState {
        service,
        bearer_token,
    });

    Router::new()
        .route("/healthz", get(handle_healthz))
        .route("/mcp", post(handle_mcp_post).get(handle_mcp_get))
        .with_state(state)
}

/// Start listening on `config.bind_addr` and return the bound address and server task handle.
pub async fn start_http_server(
    service: JcodeMcpService,
    config: HttpServerConfig,
) -> Result<(SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("failed to bind HTTP MCP server on {}", config.bind_addr))?;
    let local_addr = listener.local_addr()?;
    let router = build_router(service, config.bearer_token);

    let handle = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .context("HTTP MCP server error")
    });

    Ok((local_addr, handle))
}

fn verify_bearer(headers: &HeaderMap, expected: Option<&str>) -> bool {
    let Some(expected_token) = expected.filter(|s| !s.is_empty()) else {
        return true;
    };

    let Some(auth_val) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };

    let Some(provided) = auth_val
        .strip_prefix("Bearer ")
        .or_else(|| auth_val.strip_prefix("bearer "))
    else {
        return false;
    };

    let a = provided.trim().as_bytes();
    let b = expected_token.trim().as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

async fn handle_healthz() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "jcode-mcp",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn handle_mcp_get(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !verify_bearer(&headers, state.bearer_token.as_deref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Unauthorized: valid Bearer token required" })),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        Json(json!({
            "name": "jcode-mcp",
            "transport": "streamable-http",
            "version": env!("CARGO_PKG_VERSION")
        })),
    )
        .into_response()
}

async fn handle_mcp_post(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !verify_bearer(&headers, state.bearer_token.as_deref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Unauthorized: valid Bearer token required" })),
        )
            .into_response();
    }

    // Support both single JSON-RPC object and JSON-RPC batch arrays
    let parsed_val: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(err) => {
            let reply =
                JsonRpcResponse::err(Value::Null, -32700, format!("Parse error: {err}"), None);
            return (StatusCode::OK, Json(reply)).into_response();
        }
    };

    if let Some(arr) = parsed_val.as_array() {
        let mut responses = Vec::new();
        for item in arr {
            match serde_json::from_value::<JsonRpcRequest>(item.clone()) {
                Ok(req) => {
                    if let Some(resp) = state.service.handle_jsonrpc(req).await {
                        responses.push(resp);
                    }
                }
                Err(err) => {
                    responses.push(JsonRpcResponse::err(
                        Value::Null,
                        -32600,
                        format!("Invalid Request: {err}"),
                        None,
                    ));
                }
            }
        }
        if responses.is_empty() {
            return StatusCode::ACCEPTED.into_response();
        }
        return (StatusCode::OK, Json(responses)).into_response();
    }

    match serde_json::from_value::<JsonRpcRequest>(parsed_val) {
        Ok(req) => match state.service.handle_jsonrpc(req).await {
            Some(resp) => (StatusCode::OK, Json(resp)).into_response(),
            None => StatusCode::ACCEPTED.into_response(),
        },
        Err(err) => {
            let reply =
                JsonRpcResponse::err(Value::Null, -32600, format!("Invalid Request: {err}"), None);
            (StatusCode::OK, Json(reply)).into_response()
        }
    }
}
