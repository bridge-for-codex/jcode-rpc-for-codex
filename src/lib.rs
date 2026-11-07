//! `jcode-rpc-for-codex` library exports.
//!
//! Provides an asynchronous Rust client for Jcode's `api-bridge` JSONLines RPC protocol,
//! a non-interactive CLI wrapper, an MCP server (`stdio` and Streamable HTTP with Bearer auth),
//! and a Cloudflare Tunnel (`cloudflared`) supervisor so Codex can operate Jcode locally or remotely.

pub mod bridge;
pub mod cli;
pub mod mcp_http;
pub mod mcp_stdio;
pub mod protocol;
pub mod tools;
pub mod tunnel;
