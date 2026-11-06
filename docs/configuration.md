# Configuration & Connection Modes

`jcode-mcp` supports four connection modes and a shared set of global workspace/transport options. All options can be supplied via CLI flags or environment variables.

## Global Options

| Flag | Environment Variable | Default | Description |
| :--- | :--- | :--- | :--- |
| `--jcode-bin <PATH>` | `JCODE_BIN` | `jcode` | Path or binary name for the `jcode` executable. |
| `--cwd <DIR>` | `JCODE_DEFAULT_CWD` | Current directory | Default working directory for new Jcode sessions. |
| `--allow-cwd <DIRS>` | `JCODE_ALLOWED_CWD_ROOTS` | *(unrestricted)* | Comma-separated or repeated list of allowed workspace root directories. |
| `--socket <PATH>` | `JCODE_SOCKET` | *(none)* | Custom internal Jcode daemon socket path (`jcode --socket`). |
| `--api-socket <PATH>` | `JCODE_API_SOCKET` | *(none)* | Connect directly to an existing `api-bridge` Unix domain socket instead of spawning `jcode api-bridge --stdio`. |
| `--ssh <HOST>` | `JCODE_SSH` | *(none)* | Forward `jcode` operations to a remote host over SSH (`[user@]host[:port]`). |
| `--ssh-binary <PATH>` | `JCODE_SSH_BINARY` | *(none)* | Remote `jcode` binary path when `--ssh` is enabled. |
| `--ssh-server-socket <PATH>` | `JCODE_SSH_SERVER_SOCKET` | *(none)* | Remote daemon socket path when `--ssh` is enabled. |
| `--remote-cwd <DIR>` | `JCODE_REMOTE_CWD` | *(none)* | Remote working directory when `--ssh` is enabled. |
| `--request-timeout-secs <SECS>` | *(none)* | `30` | Control-plane RPC timeout in seconds (minimum 5s). |

---

## Mode 1: Local `stdio` (Default)

Runs the MCP JSON-RPC 2.0 server over `stdin` and `stdout`.

```bash
jcode-mcp stdio
# or simply:
jcode-mcp
```

### Codex `~/.codex/config.toml`

```toml
[mcp_servers.jcode]
command = "/absolute/path/to/jcode-mcp"
args = ["--cwd", "/path/to/workspace", "--allow-cwd", "/path/to/workspace", "stdio"]
```

---

## Mode 2: Streamable HTTP (`serve-http`)

Exposes `POST /mcp`, `GET /mcp`, and `GET /healthz` over HTTP. Unless `--no-auth` is passed, `jcode-mcp` requires a Bearer token on `/mcp` (auto-generating a random 256-bit token if `--bearer-token` is omitted).

```bash
jcode-mcp serve-http \
  --bind 127.0.0.1:8787 \
  --bearer-token "your-secret-token"
```

### Subcommand Options (`serve-http`)

| Flag | Environment Variable | Default | Description |
| :--- | :--- | :--- | :--- |
| `--bind <ADDR>` | `JCODE_MCP_BIND` | `127.0.0.1:8787` | Local socket address to bind. |
| `--bearer-token <TOKEN>` | `JCODE_MCP_BEARER_TOKEN` | *(auto-generated)* | Bearer token required on `/mcp` requests. |
| `--no-auth` | *(none)* | `false` | Disable Bearer token authentication (trusted loopback only). |
| `--cloudflare-tunnel` | *(none)* | `false` | Also spawn `cloudflared` to expose the HTTP server over HTTPS. |
| `--cloudflared-bin <PATH>` | `CLOUDFLARED_BIN` | `cloudflared` | Path to `cloudflared` binary. |
| `--tunnel-token <TOKEN>` | `CLOUDFLARE_TUNNEL_TOKEN` | *(none)* | Optional Cloudflare Named Tunnel token. |
| `--tunnel-hostname <HOST>` | `CLOUDFLARE_TUNNEL_HOSTNAME` | *(none)* | Public hostname when using a Named Tunnel token. |

### Codex `~/.codex/config.toml`

```toml
[mcp_servers.jcode]
url = "http://127.0.0.1:8787/mcp"
http_headers = { Authorization = "Bearer your-secret-token" }
```

---

## Mode 3: Cloudflare Tunnel (`tunnel`)

Starts the HTTP MCP server on localhost (`127.0.0.1:0` by default) with a 256-bit Bearer token, spawns `cloudflared`, and prints a JSON document containing `public_mcp_url`, `bearer_token`, `codex_mcp_json`, and `codex_config_toml`.

### Zero-Config Quick Tunnel (`*.trycloudflare.com`)

```bash
jcode-mcp tunnel
```

### Named Tunnel (Custom Domain)

```bash
CLOUDFLARE_TUNNEL_TOKEN="eyJhIjoi..." \
CLOUDFLARE_TUNNEL_HOSTNAME="jcode-mcp.example.com" \
jcode-mcp tunnel
```

---

## Mode 4: Remote Jcode over SSH

Forwards all `api-bridge` and CLI invocations to a remote host over SSH. Combine with `stdio`, `serve-http`, or `tunnel`:

```bash
jcode-mcp \
  --ssh dev@remote-box \
  --remote-cwd /home/dev/workspace \
  stdio
```

---

## Diagnostic Command (`doctor`)

Verifies that `jcode` is installed, performs a live `api-bridge` `hello` + `ping` handshake, and reports the active provider/model and capabilities:

```bash
jcode-mcp doctor
```
