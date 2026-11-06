use jcode_rpc_for_codex::bridge::{BridgeClient, BridgeConfig, SessionEventBuffer};
use jcode_rpc_for_codex::mcp_http::{HttpServerConfig, start_http_server};
use jcode_rpc_for_codex::protocol::{
    ApiRequest, BridgeEventFrame, BridgeRequestEnvelope, JCODE_API_VERSION,
};
use jcode_rpc_for_codex::tools::{JcodeMcpService, slice_lines};
use jcode_rpc_for_codex::tunnel::{
    build_codex_connection_info, extract_trycloudflare_url, generate_bearer_token,
};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::process::Command as StdCommand;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[test]
fn test_protocol_request_serialization() {
    let hello = BridgeRequestEnvelope::new(
        1,
        ApiRequest::Hello {
            min_version: JCODE_API_VERSION,
            max_version: JCODE_API_VERSION,
            client: Some("test-client".to_string()),
            supports_user_response: false,
        },
    );
    let val = serde_json::to_value(&hello).expect("serialize hello");
    assert_eq!(val["v"], 1);
    assert_eq!(val["id"], 1);
    assert_eq!(val["req"], "hello");
    assert_eq!(val["client"], "test-client");
    assert!(val.get("supports_user_response").is_none());

    let find = BridgeRequestEnvelope::new(
        42,
        ApiRequest::FindFiles {
            session_id: "sess_1".to_string(),
            query: "Cargo.toml".to_string(),
            limit: Some(10),
        },
    );
    let find_val = serde_json::to_value(&find).expect("serialize find_files");
    assert_eq!(find_val["req"], "find_files");
    assert_eq!(find_val["session_id"], "sess_1");
    assert_eq!(find_val["limit"], 10);
}

#[test]
fn test_event_buffer_aggregation() {
    let mut buf = SessionEventBuffer::default();
    buf.reset_turn();
    assert_eq!(buf.status, "processing");
    assert!(!buf.turn_completed);

    let delta1: BridgeEventFrame = serde_json::from_value(json!({
        "v": 1,
        "ev": "text_delta",
        "session_id": "s1",
        "delta": "Hello "
    }))
    .unwrap();
    let delta2: BridgeEventFrame = serde_json::from_value(json!({
        "v": 1,
        "ev": "text_delta",
        "session_id": "s1",
        "delta": "world!"
    }))
    .unwrap();
    let reasoning: BridgeEventFrame = serde_json::from_value(json!({
        "v": 1,
        "ev": "reasoning_delta",
        "session_id": "s1",
        "delta": "thinking..."
    }))
    .unwrap();
    let tool_start: BridgeEventFrame = serde_json::from_value(json!({
        "v": 1,
        "ev": "tool_start",
        "session_id": "s1",
        "call_id": "c1",
        "tool_name": "read",
        "input": {"file_path": "Cargo.toml"}
    }))
    .unwrap();
    let tool_done: BridgeEventFrame = serde_json::from_value(json!({
        "v": 1,
        "ev": "tool_done",
        "session_id": "s1",
        "call_id": "c1",
        "output": "ok"
    }))
    .unwrap();
    let done: BridgeEventFrame = serde_json::from_value(json!({
        "v": 1,
        "ev": "turn_done",
        "session_id": "s1"
    }))
    .unwrap();

    buf.ingest(delta1);
    buf.ingest(delta2);
    buf.ingest(reasoning);
    buf.ingest(tool_start);
    buf.ingest(tool_done);
    buf.ingest(done);

    let summary = buf.snapshot_summary("s1");
    assert_eq!(summary.assembled_text, "Hello world!");
    assert_eq!(summary.reasoning_text, "thinking...");
    assert_eq!(summary.status, "idle");
    assert_eq!(summary.tool_calls.len(), 1);
    assert_eq!(summary.tool_calls[0].name, "read");
    assert!(summary.tool_calls[0].done);
    assert_eq!(summary.tool_calls[0].output.as_deref(), Some("ok"));
    assert!(buf.turn_completed);
}

#[test]
fn test_cwd_allowlist_policy() {
    let allowed_dir = tempdir().expect("create allowed tempdir");
    let forbidden_dir = tempdir().expect("create forbidden tempdir");

    let cfg = BridgeConfig {
        default_cwd: Some(allowed_dir.path().to_path_buf()),
        allowed_cwd_roots: vec![allowed_dir.path().to_path_buf()],
        ..Default::default()
    };

    let resolved = cfg
        .resolve_working_dir(None)
        .expect("default allowed dir should resolve");
    assert_eq!(
        resolved,
        allowed_dir.path().canonicalize().expect("canonicalize")
    );

    let err = cfg
        .resolve_working_dir(Some(&forbidden_dir.path().to_string_lossy()))
        .expect_err("forbidden directory must be rejected");
    assert!(
        err.to_string().contains("outside allowed workspace roots"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_tunnel_helpers_and_slice_lines() {
    let tok = generate_bearer_token();
    assert_eq!(tok.len(), 64);
    assert!(tok.chars().all(|c| c.is_ascii_hexdigit()));

    let log_line = "2026-11-05T03:00:00Z INF |  https://bright-fox-42.trycloudflare.com  |";
    assert_eq!(
        extract_trycloudflare_url(log_line).as_deref(),
        Some("https://bright-fox-42.trycloudflare.com")
    );

    let addr: SocketAddr = "127.0.0.1:8787".parse().unwrap();
    let info = build_codex_connection_info(
        "quick",
        addr,
        "https://bright-fox-42.trycloudflare.com/",
        "secret-123",
    );
    assert_eq!(
        info.public_mcp_url,
        "https://bright-fox-42.trycloudflare.com/mcp"
    );
    assert_eq!(
        info.codex_mcp_json["mcpServers"]["jcode"]["headers"]["Authorization"],
        "Bearer secret-123"
    );
    assert!(info.codex_config_toml.contains("Bearer secret-123"));

    let sample = "line1\nline2\nline3\nline4";
    assert_eq!(slice_lines(sample, Some(2), Some(3)), "line2\nline3");
}

async fn send_raw_http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    auth_header: Option<&str>,
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.expect("connect test http");
    let auth_line = auth_header
        .map(|h| format!("Authorization: {h}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n{auth_line}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.expect("write http");
    let mut resp_bytes = Vec::new();
    stream
        .read_to_end(&mut resp_bytes)
        .await
        .expect("read http");
    let text = String::from_utf8_lossy(&resp_bytes).to_string();
    let (head, payload) = text.split_once("\r\n\r\n").expect("http split");
    let status_line = head.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    (status_code, payload.to_string())
}

#[tokio::test]
async fn test_http_mcp_bearer_auth_and_batch_dispatch() {
    let service = JcodeMcpService::new(BridgeConfig::default());
    let token = "test-super-secret-bearer-token".to_string();
    let (addr, handle) = start_http_server(
        service,
        HttpServerConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            bearer_token: Some(token.clone()),
        },
    )
    .await
    .expect("start http server");

    // 1. /healthz works without auth
    let (status, body) = send_raw_http(addr, "GET", "/healthz", None, "").await;
    assert_eq!(status, 200);
    let health_json: Value = serde_json::from_str(&body).expect("parse healthz");
    assert_eq!(health_json["status"], "ok");

    // 2. /mcp without token -> 401
    let init_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2024-11-05" }
    })
    .to_string();
    let (status_no_auth, _) = send_raw_http(addr, "POST", "/mcp", None, &init_req).await;
    assert_eq!(status_no_auth, 401);

    // 3. /mcp with wrong token -> 401
    let (status_bad_auth, _) =
        send_raw_http(addr, "POST", "/mcp", Some("Bearer wrong-token"), &init_req).await;
    assert_eq!(status_bad_auth, 401);

    // 4. /mcp with valid token -> 200 initialize
    let auth_val = format!("Bearer {token}");
    let (status_ok, init_body) =
        send_raw_http(addr, "POST", "/mcp", Some(&auth_val), &init_req).await;
    assert_eq!(status_ok, 200);
    let init_resp: Value = serde_json::from_str(&init_body).expect("parse init response");
    assert_eq!(init_resp["result"]["serverInfo"]["name"], "jcode-mcp");

    // 5. Batch request: notification + tools/list
    let batch_req = json!([
        {
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        },
        {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }
    ])
    .to_string();
    let (status_batch, batch_body) =
        send_raw_http(addr, "POST", "/mcp", Some(&auth_val), &batch_req).await;
    assert_eq!(status_batch, 200);
    let batch_resp: Value = serde_json::from_str(&batch_body).expect("parse batch response");
    let arr = batch_resp.as_array().expect("batch response array");
    assert_eq!(arr.len(), 1);
    let tools = arr[0]["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 6);

    handle.abort();
}

#[tokio::test]
async fn test_live_jcode_bridge_and_workspace_tools() {
    if StdCommand::new("jcode").arg("--version").output().is_err() {
        eprintln!("Skipping live jcode test: jcode binary not found in PATH");
        return;
    }

    let cfg = BridgeConfig::default();
    let client = BridgeClient::new(cfg.clone());
    let handshake = client.handshake_info().await.expect("live handshake_info");
    assert!(
        handshake["capabilities"].is_array(),
        "expected capabilities array in handshake: {handshake}"
    );

    let service = JcodeMcpService::new(cfg);
    let overview = service
        .dispatch_tool("jcode_status", json!({ "action": "overview" }))
        .await
        .expect("jcode_status overview");
    assert_eq!(overview["ping"]["ok"], true);

    let created = service
        .dispatch_tool(
            "jcode_session_manage",
            json!({
                "action": "create",
                "cwd": env!("CARGO_MANIFEST_DIR"),
                "archive_after_days": 1
            }),
        )
        .await
        .expect("create session");
    let sid = created["session_id"]
        .as_str()
        .expect("session_id string")
        .to_string();

    let found = service
        .dispatch_tool(
            "jcode_workspace_file",
            json!({
                "action": "find",
                "session_id": sid,
                "query": "Cargo.toml",
                "limit": 5
            }),
        )
        .await
        .expect("find Cargo.toml");
    assert_eq!(found["ev"], "files");
    assert!(
        found["paths"]
            .as_array()
            .expect("paths array")
            .iter()
            .any(|p| p.as_str() == Some("Cargo.toml"))
    );

    let read_res = service
        .dispatch_tool(
            "jcode_workspace_file",
            json!({
                "action": "read",
                "session_id": sid,
                "path": "Cargo.toml",
                "start_line": 1,
                "end_line": 3
            }),
        )
        .await
        .expect("read Cargo.toml lines 1..=3");
    let content = read_res["content"].as_str().unwrap_or("");
    assert!(content.contains("[package]"));
    assert_eq!(content.lines().count(), 3);

    let search_res = service
        .dispatch_tool(
            "jcode_workspace_file",
            json!({
                "action": "search",
                "session_id": sid,
                "query": "jcode-mcp",
                "limit": 5
            }),
        )
        .await
        .expect("search jcode-mcp");
    assert_eq!(search_res["ev"], "text_matches");

    let status_res = service
        .dispatch_tool(
            "jcode_workspace_file",
            json!({
                "action": "status",
                "session_id": sid,
                "path": "Cargo.toml"
            }),
        )
        .await
        .expect("file status Cargo.toml");
    assert_eq!(status_res["ev"], "file_status");
    assert_eq!(status_res["exists"], true);

    let _ = service
        .dispatch_tool(
            "jcode_session_manage",
            json!({
                "action": "archive",
                "session_id": sid
            }),
        )
        .await
        .expect("archive test session");
}
