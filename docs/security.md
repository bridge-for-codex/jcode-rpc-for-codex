# Security & Workspace Boundaries

This document explains the authentication and workspace boundary mechanisms in `jcode-mcp`, their exact scope, and how to harden deployments when exposing `jcode` over HTTP or Cloudflare Tunnel.

## 1. Streamable HTTP & Web MCP Authentication

- **Three Constant-Time Authentication Mechanisms**:
  1. **HTTP Bearer Header** (`Authorization: Bearer <TOKEN>`): Standard mode for Codex CLI / Desktop (`~/.codex/config.toml`).
  2. **Secret URL Path or Query Token** (`/mcp/<TOKEN>` or `/mcp?token=<TOKEN>`): Designed for web MCP clients (such as ChatGPT Web) when configured with "No authentication", keeping the endpoint protected by a 256-bit secret token in the URL.
  3. **Built-in OAuth 2.0 Authorization Server** (`/.well-known/oauth-protected-resource`, `/.well-known/oauth-authorization-server`, `/oauth/register`, `/oauth/authorize`, `/oauth/token`): Supports Authorization Code + PKCE (`S256` and `plain`), Dynamic Client Registration, and `client_credentials` grants for web MCP clients configured with "OAuth".
- **256-Bit Random Token Generation**: When `--bearer-token` / `JCODE_MCP_BEARER_TOKEN` is omitted, `jcode-mcp` generates a 32-byte (64-hex-character) token using `rand::rng()`.
- **Constant-Time Verification**: All token and secret comparisons in `src/mcp_http.rs` use `subtle::ConstantTimeEq` (`ct_eq`) to prevent timing side-channel attacks.

## 2. Workspace Root Allowlist (`--allow-cwd`)

When `--allow-cwd` (or `JCODE_ALLOWED_CWD_ROOTS`) is configured:

1. Every requested `cwd` (or `default_cwd` when omitted) is resolved to an absolute path and canonicalized via `std::fs::canonicalize()`, resolving `..` segments and symlinks.
2. `BridgeConfig::resolve_working_dir` verifies that the canonical path starts with at least one canonicalized root in `allowed_cwd_roots`.
3. Requests outside the allowlist are rejected before creating a session or running `jcode run --json`.

### Important Limitation: `--allow-cwd` Is Not an OS Sandbox

`--allow-cwd` is a **control-plane guard on the initial session working directory (`cwd`)**, not an operating-system kernel sandbox:

- Once a `jcode` session is running inside an allowed `cwd`, the `jcode` agent has access to its own internal tools (such as `bash`, `read`, `write`, `edit`).
- If the agent executes a shell command via `bash` (e.g., `cd / && cat /etc/hosts` or `ls ../..`) or accesses an absolute path outside `cwd`, those operations run with the OS permissions of the user running `jcode`.
- Similarly, attaching to an already-existing `session_id` reuses that session's existing context.

## 3. Hardening Recommendations for Remote / Tunnel Deployments

If you expose `jcode-mcp` beyond localhost (via `serve-http` or `tunnel`):

1. **Always Keep Bearer Auth Enabled**: Never pass `--no-auth` when binding to a non-loopback interface or running a tunnel.
2. **Run Under a Dedicated Least-Privilege OS User / Container**: If untrusted prompts or remote clients may connect, run `jcode-mcp` and `jcode` inside an unprivileged container, VM, or OS sandbox (`sandbox-exec` on macOS / `bwrap` on Linux) scoped to the target workspace directory.
3. **Restrict Available Jcode Tools When Appropriate**: When invoking `jcode` in automated pipelines where arbitrary shell execution is undesired, use `jcode`'s `--disabled-tools bash` or `--tool-profile` options.
