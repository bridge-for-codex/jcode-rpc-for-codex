//! Cloudflare Tunnel (`cloudflared`) supervisor for exposing the local HTTP MCP server over HTTPS.

use anyhow::{Context, Result, anyhow, bail};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio::time::timeout;

/// Configuration for starting a Cloudflare Tunnel (`cloudflared`).
#[derive(Debug, Clone)]
pub struct CloudflareTunnelConfig {
    /// Path or binary name for `cloudflared` (default: `"cloudflared"`).
    pub cloudflared_bin: PathBuf,
    /// Optional Cloudflare Named Tunnel token (`cloudflared tunnel run --token <TOKEN>`).
    /// When `None`, launches a zero-config Quick Tunnel (`cloudflared tunnel --url http://...`).
    pub tunnel_token: Option<String>,
    /// Optional public hostname when using a Named Tunnel token (e.g. `"jcode-mcp.example.com"`).
    pub hostname: Option<String>,
    /// Maximum time to wait for `cloudflared` to report its public URL / active connection.
    pub startup_timeout: Duration,
}

impl Default for CloudflareTunnelConfig {
    fn default() -> Self {
        Self {
            cloudflared_bin: PathBuf::from(
                std::env::var("CLOUDFLARED_BIN").unwrap_or_else(|_| "cloudflared".to_string()),
            ),
            tunnel_token: std::env::var("CLOUDFLARE_TUNNEL_TOKEN")
                .ok()
                .filter(|s| !s.is_empty()),
            hostname: std::env::var("CLOUDFLARE_TUNNEL_HOSTNAME")
                .ok()
                .filter(|s| !s.is_empty()),
            startup_timeout: Duration::from_secs(25),
        }
    }
}

/// Information about a running Cloudflare Tunnel and its Codex + Web MCP connection snippets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConnectionInfo {
    pub mode: String,
    pub local_origin: String,
    pub public_base_url: String,
    pub public_mcp_url: String,
    /// Secret URL containing the embedded token (`/mcp/<token>`) for Web MCP "No Authentication" mode.
    pub public_mcp_token_url: String,
    pub bearer_token: String,
    /// OAuth 2.0 endpoints and credentials for Web MCP "OAuth" mode.
    pub oauth: serde_json::Value,
    pub codex_mcp_json: serde_json::Value,
    pub codex_config_toml: String,
}

/// Active `cloudflared` child process handle and connection metadata.
pub struct RunningTunnel {
    pub info: TunnelConnectionInfo,
    child: Child,
}

impl RunningTunnel {
    pub async fn stop(&mut self) -> Result<()> {
        let _ = self.child.kill().await;
        Ok(())
    }
}

/// Generate a cryptographically random 256-bit hex Bearer token.
pub fn generate_bearer_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Extract a `https://<subdomain>.trycloudflare.com` URL from a `cloudflared` log line.
pub fn extract_trycloudflare_url(line: &str) -> Option<String> {
    let re = Regex::new(r"https://[a-zA-Z0-9-]+(?:\.[a-zA-Z0-9-]+)*\.trycloudflare\.com").ok()?;
    re.find(line).map(|m| m.as_str().to_string())
}

/// Build ready-to-paste Codex `.mcp.json` and `config.toml` snippets for a public MCP URL + Bearer token.
pub fn build_codex_connection_info(
    mode: &str,
    local_addr: SocketAddr,
    public_base_url: &str,
    bearer_token: &str,
) -> TunnelConnectionInfo {
    let trimmed_base = public_base_url.trim_end_matches('/');
    let public_mcp_url = format!("{trimmed_base}/mcp");
    let public_mcp_token_url = format!("{trimmed_base}/mcp/{bearer_token}");

    let oauth = serde_json::json!({
        "mcp_server_url": public_mcp_url,
        "client_id": "jcode-mcp",
        "client_secret": bearer_token,
        "authorization_url": format!("{trimmed_base}/oauth/authorize"),
        "token_url": format!("{trimmed_base}/oauth/token"),
        "registration_url": format!("{trimmed_base}/oauth/register"),
    });

    let codex_mcp_json = serde_json::json!({
        "mcpServers": {
            "jcode": {
                "url": public_mcp_url,
                "headers": {
                    "Authorization": format!("Bearer {bearer_token}")
                }
            }
        }
    });

    let codex_config_toml = format!(
        "[mcp_servers.jcode]\nurl = \"{public_mcp_url}\"\nhttp_headers = {{ Authorization = \"Bearer {bearer_token}\" }}\n"
    );

    TunnelConnectionInfo {
        mode: mode.to_string(),
        local_origin: format!("http://{local_addr}"),
        public_base_url: trimmed_base.to_string(),
        public_mcp_url,
        public_mcp_token_url,
        bearer_token: bearer_token.to_string(),
        oauth,
        codex_mcp_json,
        codex_config_toml,
    }
}

/// Spawn `cloudflared` pointing at `local_addr` and wait until the public URL or tunnel registration is ready.
pub async fn start_cloudflare_tunnel(
    local_addr: SocketAddr,
    bearer_token: &str,
    config: CloudflareTunnelConfig,
) -> Result<RunningTunnel> {
    let origin = format!("http://{local_addr}");
    let is_named = config.tunnel_token.is_some();

    let mut args = vec!["tunnel".to_string(), "--no-autoupdate".to_string()];
    if let Some(token) = &config.tunnel_token {
        args.push("run".to_string());
        args.push("--token".to_string());
        args.push(token.clone());
        args.push("--url".to_string());
        args.push(origin.clone());
    } else {
        args.push("--url".to_string());
        args.push(origin.clone());
    }

    let mut cmd = Command::new(&config.cloudflared_bin);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "failed to spawn `{}` (install with `brew install cloudflared` or pass --cloudflared-bin)",
            config.cloudflared_bin.display()
        )
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("missing stdout from cloudflared"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("missing stderr from cloudflared"))?;

    let (tx, rx) = oneshot::channel::<String>();
    let tx_mutex = std::sync::Arc::new(tokio::sync::Mutex::new(Some(tx)));
    let named_host = config.hostname.clone();

    for stream in [
        Box::new(stdout) as Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        Box::new(stderr) as Box<dyn tokio::io::AsyncRead + Send + Unpin>,
    ] {
        let tx_clone = tx_mutex.clone();
        let host_clone = named_host.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(url) = extract_trycloudflare_url(&line) {
                    if let Some(sender) = tx_clone.lock().await.take() {
                        let _ = sender.send(url);
                    }
                } else if is_named
                    && (line.contains("Registered tunnel connection")
                        || line.contains("Updated to new configuration"))
                {
                    let base = host_clone
                        .as_deref()
                        .map(|h| {
                            if h.starts_with("http://") || h.starts_with("https://") {
                                h.to_string()
                            } else {
                                format!("https://{h}")
                            }
                        })
                        .unwrap_or_else(|| "https://<your-cloudflare-tunnel-hostname>".to_string());
                    if let Some(sender) = tx_clone.lock().await.take() {
                        let _ = sender.send(base);
                    }
                }
            }
        });
    }

    let public_base_url = match timeout(config.startup_timeout, rx).await {
        Ok(Ok(url)) => url,
        Ok(Err(_)) => {
            let _ = child.kill().await;
            bail!("cloudflared exited before publishing a tunnel URL");
        }
        Err(_) => {
            let _ = child.kill().await;
            bail!(
                "timed out after {}s waiting for cloudflared tunnel URL",
                config.startup_timeout.as_secs()
            );
        }
    };

    let mode = if is_named { "named" } else { "quick" };
    let info = build_codex_connection_info(mode, local_addr, &public_base_url, bearer_token);

    Ok(RunningTunnel { info, child })
}
