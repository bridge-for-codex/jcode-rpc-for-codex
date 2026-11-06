//! `jcode-mcp` CLI entrypoint supporting `stdio`, `serve-http`, `tunnel`, and `doctor` subcommands.

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use jcode_rpc_for_codex::bridge::BridgeConfig;
use jcode_rpc_for_codex::mcp_http::{HttpServerConfig, start_http_server};
use jcode_rpc_for_codex::mcp_stdio::serve_stdio;
use jcode_rpc_for_codex::tools::JcodeMcpService;
use jcode_rpc_for_codex::tunnel::{
    CloudflareTunnelConfig, generate_bearer_token, start_cloudflare_tunnel,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(
    name = "jcode-mcp",
    version,
    about = "MCP server & RPC bridge for operating Jcode from Codex (stdio, Streamable HTTP, SSH, and Cloudflare Tunnel)"
)]
struct Cli {
    #[command(flatten)]
    common: CommonOpts,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Clone, Args)]
struct CommonOpts {
    /// Path or binary name for `jcode`
    #[arg(long, env = "JCODE_BIN", default_value = "jcode", global = true)]
    jcode_bin: PathBuf,

    /// Default working directory for Jcode sessions
    #[arg(long, env = "JCODE_DEFAULT_CWD", global = true)]
    cwd: Option<PathBuf>,

    /// Allowed workspace root directories (comma-separated or repeated)
    #[arg(
        long = "allow-cwd",
        env = "JCODE_ALLOWED_CWD_ROOTS",
        value_delimiter = ',',
        global = true
    )]
    allowed_cwd_roots: Vec<PathBuf>,

    /// Optional internal Jcode daemon socket path (`--socket`)
    #[arg(long, env = "JCODE_SOCKET", global = true)]
    socket: Option<PathBuf>,

    /// Optional direct `api-bridge` Unix socket path
    #[arg(long, env = "JCODE_API_SOCKET", global = true)]
    api_socket: Option<PathBuf>,

    /// Forward operations to a remote Jcode host over SSH (`[user@]host[:port]`)
    #[arg(long, env = "JCODE_SSH", global = true)]
    ssh: Option<String>,

    /// Remote `jcode` binary path when `--ssh` is enabled
    #[arg(long, env = "JCODE_SSH_BINARY", global = true)]
    ssh_binary: Option<String>,

    /// Remote Jcode daemon socket path when `--ssh` is enabled
    #[arg(long, env = "JCODE_SSH_SERVER_SOCKET", global = true)]
    ssh_server_socket: Option<String>,

    /// Remote working directory when `--ssh` is enabled
    #[arg(long, env = "JCODE_REMOTE_CWD", global = true)]
    remote_cwd: Option<String>,

    /// Control-plane RPC timeout in seconds
    #[arg(long, default_value_t = 30, global = true)]
    request_timeout_secs: u64,
}

impl CommonOpts {
    fn into_bridge_config(self) -> BridgeConfig {
        BridgeConfig {
            jcode_bin: self.jcode_bin,
            default_cwd: self.cwd.or_else(|| std::env::current_dir().ok()),
            allowed_cwd_roots: self.allowed_cwd_roots,
            daemon_socket: self.socket,
            api_socket: self.api_socket,
            ssh_host: self.ssh,
            ssh_binary: self.ssh_binary,
            ssh_server_socket: self.ssh_server_socket,
            remote_working_dir: self.remote_cwd,
            request_timeout: Duration::from_secs(self.request_timeout_secs.max(5)),
        }
    }
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run the MCP server over `stdin`/`stdout` (default when no subcommand is given)
    Stdio,

    /// Run the Streamable HTTP MCP server (`/mcp` and `/healthz`) with Bearer Token auth
    ServeHttp {
        /// Local bind address
        #[arg(long, env = "JCODE_MCP_BIND", default_value = "127.0.0.1:8787")]
        bind: SocketAddr,

        /// Bearer token required on `/mcp` requests (auto-generated if omitted unless `--no-auth` is set)
        #[arg(long, env = "JCODE_MCP_BEARER_TOKEN")]
        bearer_token: Option<String>,

        /// Disable Bearer token authentication (only recommended on trusted loopback)
        #[arg(long, default_value_t = false)]
        no_auth: bool,

        /// Also launch a Cloudflare Tunnel (`cloudflared`) exposing this HTTP server over HTTPS
        #[arg(long, default_value_t = false)]
        cloudflare_tunnel: bool,

        /// Path to `cloudflared` binary when `--cloudflare-tunnel` is set
        #[arg(long, env = "CLOUDFLARED_BIN", default_value = "cloudflared")]
        cloudflared_bin: PathBuf,

        /// Optional Cloudflare Named Tunnel token (`CLOUDFLARE_TUNNEL_TOKEN`)
        #[arg(long, env = "CLOUDFLARE_TUNNEL_TOKEN")]
        tunnel_token: Option<String>,

        /// Optional public hostname when using a Named Tunnel token
        #[arg(long, env = "CLOUDFLARE_TUNNEL_HOSTNAME")]
        tunnel_hostname: Option<String>,
    },

    /// Start the HTTP MCP server on localhost AND expose it publicly via Cloudflare Tunnel (`cloudflared`)
    Tunnel {
        /// Local bind address (use port 0 for an ephemeral free port)
        #[arg(long, env = "JCODE_MCP_BIND", default_value = "127.0.0.1:0")]
        bind: SocketAddr,

        /// Bearer token required on `/mcp` requests (auto-generated 256-bit token if omitted)
        #[arg(long, env = "JCODE_MCP_BEARER_TOKEN")]
        bearer_token: Option<String>,

        /// Path to `cloudflared` binary
        #[arg(long, env = "CLOUDFLARED_BIN", default_value = "cloudflared")]
        cloudflared_bin: PathBuf,

        /// Optional Cloudflare Named Tunnel token (`CLOUDFLARE_TUNNEL_TOKEN`); uses Quick Tunnel if omitted
        #[arg(long, env = "CLOUDFLARE_TUNNEL_TOKEN")]
        tunnel_token: Option<String>,

        /// Optional public hostname when using a Named Tunnel token
        #[arg(long, env = "CLOUDFLARE_TUNNEL_HOSTNAME")]
        tunnel_hostname: Option<String>,

        /// Startup timeout in seconds while waiting for `cloudflared` URL
        #[arg(long, default_value_t = 25)]
        startup_timeout_secs: u64,
    },

    /// Verify `jcode` binary, `api-bridge` handshake, provider status, and optional `cloudflared` availability
    Doctor,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = cli.common.into_bridge_config();
    let service = JcodeMcpService::new(config);

    match cli.command.unwrap_or(Commands::Stdio) {
        Commands::Stdio => serve_stdio(service).await,

        Commands::ServeHttp {
            bind,
            bearer_token,
            no_auth,
            cloudflare_tunnel,
            cloudflared_bin,
            tunnel_token,
            tunnel_hostname,
        } => {
            let effective_token = if no_auth {
                None
            } else {
                Some(
                    bearer_token
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(generate_bearer_token),
                )
            };

            let (local_addr, server_handle) = start_http_server(
                service,
                HttpServerConfig {
                    bind_addr: bind,
                    bearer_token: effective_token.clone(),
                },
            )
            .await?;

            eprintln!("jcode-mcp HTTP server listening on http://{local_addr}/mcp");
            if let Some(tok) = &effective_token {
                eprintln!("Authorization: Bearer {tok}");
            } else {
                eprintln!("Authorization: disabled (--no-auth)");
            }

            let mut running_tunnel = if cloudflare_tunnel {
                let tok = effective_token
                    .clone()
                    .unwrap_or_else(generate_bearer_token);
                let tun = start_cloudflare_tunnel(
                    local_addr,
                    &tok,
                    CloudflareTunnelConfig {
                        cloudflared_bin,
                        tunnel_token,
                        hostname: tunnel_hostname,
                        startup_timeout: Duration::from_secs(25),
                    },
                )
                .await?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&tun.info)
                        .context("failed to format tunnel connection info")?
                );
                Some(tun)
            } else {
                None
            };

            tokio::select! {
                res = server_handle => {
                    res.context("HTTP server task panicked")??;
                }
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("Shutting down jcode-mcp HTTP server...");
                }
            }

            if let Some(mut tun) = running_tunnel.take() {
                let _ = tun.stop().await;
            }
            Ok(())
        }

        Commands::Tunnel {
            bind,
            bearer_token,
            cloudflared_bin,
            tunnel_token,
            tunnel_hostname,
            startup_timeout_secs,
        } => {
            let token = bearer_token
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(generate_bearer_token);

            let (local_addr, server_handle) = start_http_server(
                service,
                HttpServerConfig {
                    bind_addr: bind,
                    bearer_token: Some(token.clone()),
                },
            )
            .await?;

            let mut running_tunnel = start_cloudflare_tunnel(
                local_addr,
                &token,
                CloudflareTunnelConfig {
                    cloudflared_bin,
                    tunnel_token,
                    hostname: tunnel_hostname,
                    startup_timeout: Duration::from_secs(startup_timeout_secs.max(5)),
                },
            )
            .await?;

            println!(
                "{}",
                serde_json::to_string_pretty(&running_tunnel.info)
                    .context("failed to serialize tunnel info")?
            );

            tokio::select! {
                res = server_handle => {
                    res.context("HTTP server task panicked")??;
                }
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("Shutting down Cloudflare Tunnel and jcode-mcp HTTP server...");
                }
            }

            running_tunnel.stop().await?;
            Ok(())
        }

        Commands::Doctor => {
            let overview = service
                .dispatch_tool("jcode_status", serde_json::json!({ "action": "overview" }))
                .await?;
            println!("{}", serde_json::to_string_pretty(&overview)?);
            Ok(())
        }
    }
}
