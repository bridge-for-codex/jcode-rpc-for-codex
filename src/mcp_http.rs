//! Streamable HTTP MCP server (`POST /mcp`, `/mcp/{token}`, `GET /healthz`) with:
//! 1. Constant-time Bearer Token header auth (`Authorization: Bearer <token>`)
//! 2. Constant-time Secret URL path/query auth (`/mcp/<token>` or `/mcp?token=<token>`) for Web MCP "No Authentication" mode
//! 3. Built-in OAuth 2.0 Authorization Server (`/.well-known/*`, `/oauth/register`, `/oauth/authorize`, `/oauth/token`) for Web MCP "OAuth" mode

use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::tools::JcodeMcpService;
use crate::tunnel::generate_bearer_token;
use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD as BASE64_STD, URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Configuration for the Streamable HTTP MCP server.
#[derive(Debug, Clone)]
pub struct HttpServerConfig {
    pub bind_addr: SocketAddr,
    /// Optional Bearer Token required on `/mcp` requests.
    pub bearer_token: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingAuthCode {
    redirect_uri: String,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    created_at: Instant,
}

#[derive(Clone)]
struct AppState {
    service: JcodeMcpService,
    bearer_token: Option<String>,
    auth_codes: Arc<Mutex<HashMap<String, PendingAuthCode>>>,
}

#[derive(Debug, Default, Deserialize)]
struct McpTokenQuery {
    token: Option<String>,
    access_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OAuthAuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    scope: Option<String>,
    token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OAuthAuthorizeForm {
    access_key: String,
    redirect_uri: String,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OAuthTokenPayload {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
}

/// Build the Axum `Router` for testing or serving.
pub fn build_router(service: JcodeMcpService, bearer_token: Option<String>) -> Router {
    let state = Arc::new(AppState {
        service,
        bearer_token,
        auth_codes: Arc::new(Mutex::new(HashMap::new())),
    });

    Router::new()
        .route("/healthz", get(handle_healthz))
        .route("/mcp", post(handle_mcp_post).get(handle_mcp_get))
        .route(
            "/mcp/{token}",
            post(handle_mcp_post_with_path).get(handle_mcp_get_with_path),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(handle_oauth_protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(handle_oauth_protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(handle_oauth_authorization_server),
        )
        .route(
            "/.well-known/openid-configuration",
            get(handle_oauth_authorization_server),
        )
        .route("/oauth/register", post(handle_oauth_register))
        .route(
            "/oauth/authorize",
            get(handle_oauth_authorize_get).post(handle_oauth_authorize_post),
        )
        .route("/oauth/token", post(handle_oauth_token))
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

fn constant_time_str_eq(provided: &str, expected: &str) -> bool {
    let a = provided.trim().as_bytes();
    let b = expected.trim().as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

fn verify_request_token(
    headers: &HeaderMap,
    path_token: Option<&str>,
    query: &McpTokenQuery,
    expected: Option<&str>,
) -> bool {
    let Some(expected_token) = expected.filter(|s| !s.is_empty()) else {
        return true;
    };

    // 1. Check Authorization: Bearer <token>
    if let Some(auth_val) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(provided) = auth_val
            .strip_prefix("Bearer ")
            .or_else(|| auth_val.strip_prefix("bearer "))
        && constant_time_str_eq(provided, expected_token)
    {
        return true;
    }

    // 2. Check URL path parameter /mcp/{token}
    if let Some(pt) = path_token
        && constant_time_str_eq(pt, expected_token)
    {
        return true;
    }

    // 3. Check query parameter ?token=<token> or ?access_token=<token>
    if let Some(qt) = query.token.as_deref().or(query.access_token.as_deref())
        && constant_time_str_eq(qt, expected_token)
    {
        return true;
    }

    false
}

fn external_base_url(headers: &HeaderMap) -> String {
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("http");

    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("127.0.0.1:8787");

    format!("{proto}://{host}")
}

fn unauthorized_response(headers: &HeaderMap) -> Response {
    let base = external_base_url(headers);
    let www_auth = format!(
        "Bearer realm=\"jcode-mcp\", resource_metadata=\"{base}/.well-known/oauth-protected-resource\""
    );
    let mut resp = (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "Unauthorized: valid Bearer token, /mcp/<token> path, or OAuth 2.0 token required"
        })),
    )
        .into_response();
    if let Ok(hv) = HeaderValue::from_str(&www_auth) {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, hv);
    }
    resp
}

async fn handle_healthz() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "jcode-mcp",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn handle_mcp_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<McpTokenQuery>,
    headers: HeaderMap,
) -> Response {
    handle_mcp_get_inner(state, None, query, headers).await
}

async fn handle_mcp_get_with_path(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Query(query): Query<McpTokenQuery>,
    headers: HeaderMap,
) -> Response {
    handle_mcp_get_inner(state, Some(token), query, headers).await
}

async fn handle_mcp_get_inner(
    state: Arc<AppState>,
    path_token: Option<String>,
    query: McpTokenQuery,
    headers: HeaderMap,
) -> Response {
    if !verify_request_token(
        &headers,
        path_token.as_deref(),
        &query,
        state.bearer_token.as_deref(),
    ) {
        return unauthorized_response(&headers);
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
    Query(query): Query<McpTokenQuery>,
    headers: HeaderMap,
    body: String,
) -> Response {
    handle_mcp_post_inner(state, None, query, headers, body).await
}

async fn handle_mcp_post_with_path(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Query(query): Query<McpTokenQuery>,
    headers: HeaderMap,
    body: String,
) -> Response {
    handle_mcp_post_inner(state, Some(token), query, headers, body).await
}

async fn handle_mcp_post_inner(
    state: Arc<AppState>,
    path_token: Option<String>,
    query: McpTokenQuery,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !verify_request_token(
        &headers,
        path_token.as_deref(),
        &query,
        state.bearer_token.as_deref(),
    ) {
        return unauthorized_response(&headers);
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

// ============================================================================
// Built-in OAuth 2.0 Server for Web MCP Connectors (ChatGPT Web / Claude Web)
// ============================================================================

async fn handle_oauth_protected_resource(headers: HeaderMap) -> impl IntoResponse {
    let base = external_base_url(&headers);
    Json(json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
        "scopes_supported": ["mcp"]
    }))
}

async fn handle_oauth_authorization_server(headers: HeaderMap) -> impl IntoResponse {
    let base = external_base_url(&headers);
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "client_credentials", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none", "client_secret_post", "client_secret_basic"],
        "code_challenge_methods_supported": ["S256", "plain"],
        "scopes_supported": ["mcp"]
    }))
}

async fn handle_oauth_register(
    State(state): State<Arc<AppState>>,
    body: String,
) -> impl IntoResponse {
    let req_json: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({}));
    let redirect_uris = req_json
        .get("redirect_uris")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let client_name = req_json
        .get("client_name")
        .and_then(Value::as_str)
        .unwrap_or("mcp-oauth-client");
    let token_endpoint_auth_method = req_json
        .get("token_endpoint_auth_method")
        .and_then(Value::as_str)
        .unwrap_or("none");

    let client_id = format!("jcode-client-{}", &generate_bearer_token()[..16]);
    let mut resp = json!({
        "client_id": client_id,
        "client_name": client_name,
        "redirect_uris": redirect_uris,
        "grant_types": ["authorization_code", "client_credentials", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": token_endpoint_auth_method,
    });

    if token_endpoint_auth_method != "none"
        && let Some(tok) = &state.bearer_token
    {
        resp["client_secret"] = Value::String(tok.clone());
    }

    (StatusCode::CREATED, Json(resp))
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

async fn issue_auth_code_and_redirect(
    state: &Arc<AppState>,
    redirect_uri: &str,
    oauth_state: Option<&str>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
) -> Response {
    let code = generate_bearer_token();
    {
        let mut guard = state.auth_codes.lock().await;
        // Prune expired codes older than 10 minutes
        guard.retain(|_, v| v.created_at.elapsed() < Duration::from_secs(600));
        guard.insert(
            code.clone(),
            PendingAuthCode {
                redirect_uri: redirect_uri.to_string(),
                code_challenge,
                code_challenge_method,
                created_at: Instant::now(),
            },
        );
    }

    let sep = if redirect_uri.contains('?') { '&' } else { '?' };
    let mut target = format!("{redirect_uri}{sep}code={code}");
    if let Some(st) = oauth_state.filter(|s| !s.is_empty()) {
        target.push_str(&format!("&state={}", urlencoding_simple(st)));
    }
    Redirect::to(&target).into_response()
}

fn urlencoding_simple(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

async fn handle_oauth_authorize_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<OAuthAuthorizeQuery>,
) -> Response {
    let _ = (&query.response_type, &query.client_id, &query.scope);
    let Some(redirect_uri) = query.redirect_uri.as_deref().filter(|s| !s.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({ "error": "invalid_request", "error_description": "Missing redirect_uri" }),
            ),
        )
            .into_response();
    };

    // If the server runs with --no-auth, or if a valid token was passed in ?token=..., auto-approve immediately
    let auto_approved = match state.bearer_token.as_deref() {
        None => true,
        Some(expected) => query
            .token
            .as_deref()
            .is_some_and(|provided| constant_time_str_eq(provided, expected)),
    };

    if auto_approved {
        return issue_auth_code_and_redirect(
            &state,
            redirect_uri,
            query.state.as_deref(),
            query.code_challenge,
            query.code_challenge_method,
        )
        .await;
    }

    // Otherwise render a clean consent form requesting the server's Bearer token
    let redirect_escaped = html_escape(redirect_uri);
    let state_escaped = html_escape(query.state.as_deref().unwrap_or(""));
    let challenge_escaped = html_escape(query.code_challenge.as_deref().unwrap_or(""));
    let method_escaped = html_escape(query.code_challenge_method.as_deref().unwrap_or("S256"));

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Authorize jcode-mcp</title>
<style>
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; background: #0f172a; color: #f8fafc; display: flex; align-items: center; justify-content: center; min-height: 100vh; margin: 0; }}
  .card {{ background: #1e293b; border: 1px solid #334155; border-radius: 12px; padding: 28px; width: 100%; max-width: 420px; box-shadow: 0 10px 25px rgba(0,0,0,0.4); }}
  h1 {{ font-size: 20px; margin: 0 0 8px; }}
  p {{ font-size: 14px; color: #94a3b8; margin: 0 0 20px; line-height: 1.5; }}
  label {{ display: block; font-size: 13px; margin-bottom: 6px; color: #cbd5e1; }}
  input[type="password"] {{ width: 100%; box-sizing: border-box; padding: 10px 12px; border-radius: 8px; border: 1px solid #475569; background: #0f172a; color: #f8fafc; font-family: monospace; font-size: 14px; margin-bottom: 18px; }}
  button {{ width: 100%; padding: 10px 14px; border: none; border-radius: 8px; background: #3b82f6; color: white; font-weight: 600; font-size: 14px; cursor: pointer; }}
  button:hover {{ background: #2563eb; }}
</style>
</head>
<body>
  <div class="card">
    <h1>Authorize jcode-mcp</h1>
    <p>Enter your <code>jcode-mcp</code> Bearer Token to grant MCP access to this client.</p>
    <form method="POST" action="/oauth/authorize">
      <input type="hidden" name="redirect_uri" value="{redirect_escaped}">
      <input type="hidden" name="state" value="{state_escaped}">
      <input type="hidden" name="code_challenge" value="{challenge_escaped}">
      <input type="hidden" name="code_challenge_method" value="{method_escaped}">
      <label for="access_key">Bearer Token</label>
      <input id="access_key" name="access_key" type="password" required autofocus placeholder="Paste bearer_token here...">
      <button type="submit">Approve &amp; Connect</button>
    </form>
  </div>
</body>
</html>"#
    );

    Html(html).into_response()
}

async fn handle_oauth_authorize_post(
    State(state): State<Arc<AppState>>,
    Form(form): Form<OAuthAuthorizeForm>,
) -> Response {
    if let Some(expected) = state.bearer_token.as_deref().filter(|s| !s.is_empty())
        && !constant_time_str_eq(&form.access_key, expected)
    {
        return (
            StatusCode::UNAUTHORIZED,
            Html("<!DOCTYPE html><html><body style=\"font-family:sans-serif;background:#0f172a;color:#f8fafc;padding:40px;text-align:center;\"><h2>Invalid Bearer Token</h2><p>The token you entered does not match the running jcode-mcp server.</p><a href=\"javascript:history.back()\" style=\"color:#60a5fa;\">Go back and try again</a></body></html>"),
        )
            .into_response();
    }

    issue_auth_code_and_redirect(
        &state,
        &form.redirect_uri,
        form.state.as_deref().filter(|s| !s.is_empty()),
        form.code_challenge.filter(|s| !s.is_empty()),
        form.code_challenge_method.filter(|s| !s.is_empty()),
    )
    .await
}

fn verify_pkce(
    code_verifier: Option<&str>,
    code_challenge: Option<&str>,
    code_challenge_method: Option<&str>,
) -> bool {
    let Some(challenge) = code_challenge.filter(|s| !s.is_empty()) else {
        return true;
    };
    let Some(verifier) = code_verifier.filter(|s| !s.is_empty()) else {
        return false;
    };

    match code_challenge_method.unwrap_or("S256") {
        "plain" => constant_time_str_eq(verifier, challenge),
        "S256" | "s256" => {
            let digest = Sha256::digest(verifier.as_bytes());
            let computed = URL_SAFE_NO_PAD.encode(digest);
            constant_time_str_eq(&computed, challenge)
        }
        _ => false,
    }
}

fn extract_basic_auth_secret(headers: &HeaderMap) -> Option<String> {
    let auth_val = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = auth_val
        .strip_prefix("Basic ")
        .or_else(|| auth_val.strip_prefix("basic "))?
        .trim();
    let decoded = BASE64_STD.decode(encoded).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (_client_id, secret) = text.split_once(':')?;
    Some(secret.to_string())
}

async fn handle_oauth_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let payload: OAuthTokenPayload =
        if content_type.contains("application/json") || body.trim_start().starts_with('{') {
            serde_json::from_str(&body).unwrap_or_else(|_| serde_urlencoded_parse(&body))
        } else {
            serde_urlencoded_parse(&body)
        };

    let _ = &payload.client_id;
    let grant_type = payload
        .grant_type
        .as_deref()
        .unwrap_or("authorization_code");
    let effective_token = state
        .bearer_token
        .clone()
        .unwrap_or_else(|| "no-auth-token".to_string());

    match grant_type {
        "authorization_code" => {
            let Some(code) = payload.code.as_deref().filter(|s| !s.is_empty()) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(
                        json!({ "error": "invalid_request", "error_description": "Missing code" }),
                    ),
                )
                    .into_response();
            };

            let pending = {
                let mut guard = state.auth_codes.lock().await;
                guard.remove(code)
            };

            let Some(entry) = pending else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "invalid_grant", "error_description": "Invalid or expired authorization code" })),
                )
                    .into_response();
            };

            if entry.created_at.elapsed() > Duration::from_secs(600) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "invalid_grant", "error_description": "Authorization code expired" })),
                )
                    .into_response();
            }

            if let Some(req_redirect) = payload.redirect_uri.as_deref().filter(|s| !s.is_empty())
                && req_redirect != entry.redirect_uri
            {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "invalid_grant", "error_description": "redirect_uri mismatch" })),
                )
                    .into_response();
            }

            if !verify_pkce(
                payload.code_verifier.as_deref(),
                entry.code_challenge.as_deref(),
                entry.code_challenge_method.as_deref(),
            ) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "invalid_grant", "error_description": "PKCE verification failed" })),
                )
                    .into_response();
            }

            (
                StatusCode::OK,
                Json(json!({
                    "access_token": effective_token,
                    "token_type": "Bearer",
                    "expires_in": 86400,
                    "refresh_token": effective_token,
                    "scope": "mcp"
                })),
            )
                .into_response()
        }

        "client_credentials" => {
            let provided_secret = payload
                .client_secret
                .or_else(|| extract_basic_auth_secret(&headers));

            if let Some(expected) = state.bearer_token.as_deref().filter(|s| !s.is_empty()) {
                let ok = provided_secret
                    .as_deref()
                    .is_some_and(|s| constant_time_str_eq(s, expected));
                if !ok {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({ "error": "invalid_client", "error_description": "Invalid client_secret" })),
                    )
                        .into_response();
                }
            }

            (
                StatusCode::OK,
                Json(json!({
                    "access_token": effective_token,
                    "token_type": "Bearer",
                    "expires_in": 86400,
                    "scope": "mcp"
                })),
            )
                .into_response()
        }

        "refresh_token" => {
            if let Some(expected) = state.bearer_token.as_deref().filter(|s| !s.is_empty()) {
                let ok = payload
                    .refresh_token
                    .as_deref()
                    .is_some_and(|s| constant_time_str_eq(s, expected));
                if !ok {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": "invalid_grant", "error_description": "Invalid refresh_token" })),
                    )
                        .into_response();
                }
            }

            (
                StatusCode::OK,
                Json(json!({
                    "access_token": effective_token,
                    "token_type": "Bearer",
                    "expires_in": 86400,
                    "refresh_token": effective_token,
                    "scope": "mcp"
                })),
            )
                .into_response()
        }

        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unsupported_grant_type" })),
        )
            .into_response(),
    }
}

fn percent_decode(input: &str) -> String {
    let mut bytes = Vec::with_capacity(input.len());
    let raw = input.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'+' {
            bytes.push(b' ');
            i += 1;
        } else if raw[i] == b'%' && i + 2 < raw.len() {
            if let Ok(hex_str) = std::str::from_utf8(&raw[i + 1..i + 3])
                && let Ok(val) = u8::from_str_radix(hex_str, 16)
            {
                bytes.push(val);
                i += 3;
                continue;
            }
            bytes.push(raw[i]);
            i += 1;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn serde_urlencoded_parse(body: &str) -> OAuthTokenPayload {
    let mut payload = OAuthTokenPayload::default();
    for pair in body.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        };
        match k.as_str() {
            "grant_type" => payload.grant_type = Some(v),
            "code" => payload.code = Some(v),
            "redirect_uri" => payload.redirect_uri = Some(v),
            "code_verifier" => payload.code_verifier = Some(v),
            "client_id" => payload.client_id = Some(v),
            "client_secret" => payload.client_secret = Some(v),
            "refresh_token" => payload.refresh_token = Some(v),
            _ => {}
        }
    }
    payload
}
