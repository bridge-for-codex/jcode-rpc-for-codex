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

    let mut args = vec![
        "tunnel".to_string(),
        "--no-autoupdate".to_string(),
        "--protocol".to_string(),
        "http2".to_string(),
    ];
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

    #[cfg(target_os = "macos")]
    if let Some(dylib_path) = ensure_macos_fakeip_bypass_dylib() {
        cmd.env("DYLD_INSERT_LIBRARIES", dylib_path);
    }

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
    let discovered_url = std::sync::Arc::new(tokio::sync::Mutex::new(None::<String>));
    let connection_registered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let named_host = config.hostname.clone();

    for stream in [
        Box::new(stdout) as Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        Box::new(stderr) as Box<dyn tokio::io::AsyncRead + Send + Unpin>,
    ] {
        let tx_clone = tx_mutex.clone();
        let url_clone = discovered_url.clone();
        let reg_clone = connection_registered.clone();
        let host_clone = named_host.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(url) = extract_trycloudflare_url(&line) {
                    *url_clone.lock().await = Some(url.clone());
                    if reg_clone.load(std::sync::atomic::Ordering::SeqCst)
                        && let Some(sender) = tx_clone.lock().await.take()
                    {
                        let _ = sender.send(url);
                    }
                } else if line.contains("Registered tunnel connection")
                    || line.contains("Updated to new configuration")
                {
                    reg_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                    if is_named {
                        let base = host_clone
                            .as_deref()
                            .map(|h| {
                                if h.starts_with("http://") || h.starts_with("https://") {
                                    h.to_string()
                                } else {
                                    format!("https://{h}")
                                }
                            })
                            .unwrap_or_else(|| {
                                "https://<your-cloudflare-tunnel-hostname>".to_string()
                            });
                        if let Some(sender) = tx_clone.lock().await.take() {
                            let _ = sender.send(base);
                        }
                    } else if let Some(url) = url_clone.lock().await.clone()
                        && let Some(sender) = tx_clone.lock().await.take()
                    {
                        let _ = sender.send(url);
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

#[cfg(target_os = "macos")]
fn ensure_macos_fakeip_bypass_dylib() -> Option<PathBuf> {
    if std::env::var_os("JCODE_DISABLE_CF_BYPASS").is_some() {
        return None;
    }

    const BYPASS_C_SRC: &str = r#"
#include <sys/types.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <net/if.h>
#include <string.h>
#include <stdlib.h>

extern int connect(int socket, const struct sockaddr *address, socklen_t address_len);

static const char *cf_edge_ips[] = {
    "198.41.192.7",
    "198.41.192.27",
    "198.41.192.37",
    "198.41.192.47",
    "198.41.192.57",
    "198.41.192.67",
    "198.41.192.77",
    "198.41.192.107",
    "198.41.200.13",
    "198.41.200.23",
    "198.41.200.53",
    "198.41.200.63",
};
static int cf_rr = 0;

int jcode_cf_connect(int sockfd, const struct sockaddr *addr, socklen_t addrlen) {
    if (addr && addr->sa_family == AF_INET && addrlen >= sizeof(struct sockaddr_in)) {
        const struct sockaddr_in *sin = (const struct sockaddr_in *)addr;
        uint16_t port = ntohs(sin->sin_port);
        uint32_t ip = ntohl(sin->sin_addr.s_addr);
        if (port == 7844 && (ip & 0xfffe0000U) == 0xc6120000U) {
            const char *ifname = getenv("JCODE_CF_BYPASS_IF");
            if (!ifname || !*ifname) {
                ifname = "en0";
            }
            unsigned int ifidx = if_nametoindex(ifname);
            if (ifidx > 0) {
                setsockopt(sockfd, IPPROTO_IP, IP_BOUND_IF, &ifidx, sizeof(ifidx));
            }
            struct sockaddr_in real_sin;
            memcpy(&real_sin, sin, sizeof(real_sin));
            int idx = __sync_fetch_and_add(&cf_rr, 1) % 12;
            inet_pton(AF_INET, cf_edge_ips[idx], &real_sin.sin_addr);
            return connect(sockfd, (const struct sockaddr *)&real_sin, sizeof(real_sin));
        }
    }
    return connect(sockfd, addr, addrlen);
}

__attribute__((used, section("__DATA,__interpose")))
static struct {
    const void *replacement;
    const void *original;
} jcode_interpose_connect[] = {
    { (const void *)jcode_cf_connect, (const void *)connect }
};
"#;

    let cache_dir = std::env::temp_dir().join("jcode-mcp-cf-bypass-v1");
    let _ = std::fs::create_dir_all(&cache_dir);
    let dylib_path = cache_dir.join("libjcode_cf_bypass.dylib");
    if dylib_path.exists() {
        return Some(dylib_path);
    }

    let c_path = cache_dir.join("jcode_cf_bypass.c");
    if std::fs::write(&c_path, BYPASS_C_SRC).is_err() {
        return None;
    }

    let status = std::process::Command::new("cc")
        .args(["-dynamiclib", "-O2"])
        .arg(&c_path)
        .arg("-o")
        .arg(&dylib_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;

    if status.success() && dylib_path.exists() {
        Some(dylib_path)
    } else {
        None
    }
}
