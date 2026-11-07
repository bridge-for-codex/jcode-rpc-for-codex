//! MCP tool catalog and JSON-RPC 2.0 dispatcher for operating `jcode`.

use crate::bridge::{BridgeClient, BridgeConfig};
use crate::cli::{JcodeCli, JcodeRunOptions};
use crate::protocol::{
    ApiRequest, JsonRpcRequest, JsonRpcResponse, MCP_PROTOCOL_VERSION, McpToolDefinition,
    McpToolResult,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

/// Central MCP request router backed by `BridgeClient` (`api-bridge`) and `JcodeCli`.
#[derive(Clone)]
pub struct JcodeMcpService {
    bridge: BridgeClient,
    cli: JcodeCli,
}

impl JcodeMcpService {
    pub fn new(config: BridgeConfig) -> Self {
        let cli = JcodeCli::new(config.clone());
        let bridge = BridgeClient::new(config);
        Self { bridge, cli }
    }

    pub fn bridge(&self) -> &BridgeClient {
        &self.bridge
    }

    pub fn cli(&self) -> &JcodeCli {
        &self.cli
    }

    /// Return the MCP tool catalog exposed to Codex.
    pub fn tool_definitions() -> Vec<McpToolDefinition> {
        vec![
            McpToolDefinition {
                name: "jcode_status".to_string(),
                title: "Jcode Runtime Status & Daemon Control".to_string(),
                description: "Inspect Jcode runtime health, version, active provider/model, auth status, api-bridge capabilities, or control the local daemon (start/stop/reload).".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["overview", "version", "auth", "provider", "models", "runtime_info", "daemon_start", "daemon_stop", "daemon_reload"],
                            "default": "overview",
                            "description": "Status or daemon management action to perform."
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Optional workspace directory for provider/runtime queries."
                        },
                        "provider": {
                            "type": "string",
                            "description": "Optional provider filter when action='models'."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional session ID when action='runtime_info'."
                        }
                    },
                    "additionalProperties": false
                }),
                annotations: Some(json!({
                    "readOnlyHint": false,
                    "destructiveHint": false
                })),
            },
            McpToolDefinition {
                name: "jcode_run".to_string(),
                title: "Run Jcode Task in Workspace".to_string(),
                description: "Run a one-shot non-interactive Jcode task in any workspace directory (`cwd`), or create/reuse a persistent session via `api-bridge` and execute a prompt.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "prompt": {
                            "type": "string",
                            "description": "Task instructions or prompt for Jcode to execute."
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Target workspace directory path (absolute or relative to default cwd)."
                        },
                        "mode": {
                            "type": "string",
                            "enum": ["bridge", "cli"],
                            "default": "bridge",
                            "description": "'bridge' creates/uses an api-bridge session (recommended, captures tool calls and session_id); 'cli' spawns `jcode run --json`."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional existing session ID to reuse."
                        },
                        "provider": {
                            "type": "string",
                            "description": "Optional provider override (for cli mode or runtime switch)."
                        },
                        "model": {
                            "type": "string",
                            "description": "Optional model override."
                        },
                        "system_prompt": {
                            "type": "string",
                            "description": "Optional system prompt override when creating a bridge session."
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "default": 300,
                            "description": "Maximum execution time in seconds."
                        }
                    },
                    "required": ["prompt"],
                    "additionalProperties": false
                }),
                annotations: Some(json!({
                    "readOnlyHint": false,
                    "destructiveHint": false
                })),
            },
            McpToolDefinition {
                name: "jcode_session_manage".to_string(),
                title: "Manage Jcode Sessions".to_string(),
                description: "List, create, attach, detach, peek, rename, archive, restore, fork, or inspect persistent Jcode sessions (history, todo state, available tools, streaming poll) across workspace directories.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": [
                                "list",
                                "create",
                                "attach",
                                "detach",
                                "peek",
                                "rename",
                                "archive",
                                "restore",
                                "fork",
                                "history",
                                "todos",
                                "tools",
                                "poll"
                            ],
                            "description": "Session operation to execute."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Target session ID (required for all actions except 'list' and 'create')."
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Workspace directory for 'create'."
                        },
                        "title": {
                            "type": "string",
                            "description": "New session title for 'rename'."
                        },
                        "system_prompt": {
                            "type": "string",
                            "description": "Optional custom system prompt for 'create'."
                        },
                        "archive_after_days": {
                            "type": "integer",
                            "description": "Optional auto-archive retention days for 'create'."
                        },
                        "include_archived": {
                            "type": "boolean",
                            "default": false,
                            "description": "Include archived sessions when action='list'."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum message count when action='peek'."
                        }
                    },
                    "required": ["action"],
                    "additionalProperties": false
                }),
                annotations: Some(json!({
                    "readOnlyHint": false,
                    "destructiveHint": false
                })),
            },
            McpToolDefinition {
                name: "jcode_session_prompt".to_string(),
                title: "Send Prompt or Control Turn in Jcode Session".to_string(),
                description: "Send a prompt, soft-interrupt message, or turn/history control command (cancel, clear, compact, rewind, rewind_undo) to an existing Jcode session via `api-bridge`.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["send", "soft_interrupt", "cancel", "clear", "compact", "rewind", "rewind_undo"],
                            "default": "send",
                            "description": "Prompt or turn control action."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Target Jcode session ID."
                        },
                        "prompt": {
                            "type": "string",
                            "description": "Message content for 'send' or 'soft_interrupt'."
                        },
                        "system_reminder": {
                            "type": "string",
                            "description": "Optional system reminder injected for this turn."
                        },
                        "urgent": {
                            "type": "boolean",
                            "default": false,
                            "description": "For 'soft_interrupt', whether to interrupt immediately."
                        },
                        "wait": {
                            "type": "boolean",
                            "default": true,
                            "description": "For 'send', whether to wait for turn completion and return assembled output."
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "default": 300,
                            "description": "Turn completion timeout in seconds when wait=true."
                        },
                        "message_index": {
                            "type": "integer",
                            "description": "Message index to rewind to when action='rewind' (defaults to 0 if omitted)."
                        }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }),
                annotations: Some(json!({
                    "readOnlyHint": false,
                    "destructiveHint": false
                })),
            },
            McpToolDefinition {
                name: "jcode_runtime_command".to_string(),
                title: "Execute Jcode Session Runtime Command".to_string(),
                description: "Execute a Jcode runtime command or model/effort configuration (`model`, `effort`, `models`, or arbitrary runtime command) within a session.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "Runtime command name (e.g. 'models', 'model', 'effort', 'status', 'compact')."
                        },
                        "args": {
                            "type": "array",
                            "items": { "type": "string" },
                            "default": [],
                            "description": "Command arguments."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional session ID scope (if omitted, a temporary session in `cwd` is created)."
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Optional workspace directory if `session_id` is omitted."
                        }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }),
                annotations: Some(json!({
                    "readOnlyHint": false,
                    "destructiveHint": false
                })),
            },
            McpToolDefinition {
                name: "jcode_workspace_file".to_string(),
                title: "Inspect & Search Workspace Files via Jcode".to_string(),
                description: "Find files (`find`), read file contents/line ranges (`read`), search text (`search`), or check git/file status (`status`) inside a Jcode workspace directory via `api-bridge`.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["find", "read", "search", "status"],
                            "description": "Workspace file operation."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional session ID; if omitted, a temporary session in `cwd` is created."
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Workspace directory if `session_id` is omitted."
                        },
                        "query": {
                            "type": "string",
                            "description": "Search query for action='find' or action='search'."
                        },
                        "limit": {
                            "type": "integer",
                            "default": 50,
                            "description": "Maximum results for action='find' or action='search'."
                        },
                        "path": {
                            "type": "string",
                            "description": "File or directory path for action='read', 'search', or 'status'."
                        },
                        "max_bytes": {
                            "type": "integer",
                            "description": "Optional maximum bytes to read for action='read'."
                        },
                        "start_line": {
                            "type": "integer",
                            "description": "Optional 1-based start line for action='read'."
                        },
                        "end_line": {
                            "type": "integer",
                            "description": "Optional 1-based inclusive end line for action='read'."
                        }
                    },
                    "required": ["action"],
                    "additionalProperties": false
                }),
                annotations: Some(json!({
                    "readOnlyHint": true,
                    "destructiveHint": false
                })),
            },
        ]
    }

    /// Process a single MCP JSON-RPC 2.0 request. Returns `None` for notifications (`id == None`).
    pub async fn handle_jsonrpc(&self, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
        let id = req.id.clone()?;
        let params = req.params.unwrap_or_else(|| json!({}));

        let response = match req.method.as_str() {
            "initialize" => {
                let requested_version = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(MCP_PROTOCOL_VERSION);
                JsonRpcResponse::ok(
                    id,
                    json!({
                        "protocolVersion": requested_version,
                        "capabilities": {
                            "tools": {
                                "listChanged": false
                            }
                        },
                        "serverInfo": {
                            "name": "jcode-mcp",
                            "version": env!("CARGO_PKG_VERSION")
                        },
                        "instructions": "Use jcode_* tools to inspect, run, and orchestrate Jcode sessions across any local or remote workspace directory (`cwd`)."
                    }),
                )
            }
            "ping" => JsonRpcResponse::ok(id, json!({})),
            "tools/list" => {
                let tools = Self::tool_definitions();
                JsonRpcResponse::ok(id, json!({ "tools": tools }))
            }
            "resources/list" => JsonRpcResponse::ok(id, json!({ "resources": [] })),
            "prompts/list" => JsonRpcResponse::ok(id, json!({ "prompts": [] })),
            "tools/call" => {
                let tool_name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));

                let result = match self.dispatch_tool(tool_name, args).await {
                    Ok(val) => McpToolResult::success_json(val),
                    Err(err) => McpToolResult::error_message(format!("{err:#}"), None),
                };
                JsonRpcResponse::ok(
                    id,
                    serde_json::to_value(result).unwrap_or_else(|_| json!({})),
                )
            }
            other => JsonRpcResponse::err(id, -32601, format!("Method not found: {other}"), None),
        };

        Some(response)
    }

    /// Execute a named MCP tool directly and return its structured JSON payload.
    pub async fn dispatch_tool(&self, name: &str, args: Value) -> Result<Value> {
        match name {
            "jcode_status" => self.tool_status(args).await,
            "jcode_run" => self.tool_run(args).await,
            "jcode_session_manage" => self.tool_session_manage(args).await,
            "jcode_session_prompt" => self.tool_session_prompt(args).await,
            "jcode_runtime_command" => self.tool_runtime_command(args).await,
            "jcode_workspace_file" => self.tool_workspace_file(args).await,
            other => bail!("unknown tool: {other}"),
        }
    }

    async fn resolve_or_create_session(&self, args: &Value) -> Result<String> {
        if let Some(sid) = args
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            return Ok(sid.to_string());
        }
        let cwd = args.get("cwd").and_then(Value::as_str);
        let (sid, _) = self
            .bridge
            .create_session(cwd, None, Some(1))
            .await
            .context("failed to create temporary workspace session")?;
        Ok(sid)
    }

    async fn tool_status(&self, args: Value) -> Result<Value> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("overview");
        let requested_cwd: Option<PathBuf> = args
            .get("cwd")
            .and_then(Value::as_str)
            .and_then(|c| self.bridge.config().resolve_working_dir(Some(c)).ok());

        match action {
            "version" => self.cli.version().await,
            "auth" => self.cli.auth_status().await,
            "provider" => self.cli.provider_current(requested_cwd.as_ref()).await,
            "models" => {
                let provider = args.get("provider").and_then(Value::as_str);
                self.cli.model_list(provider).await
            }
            "runtime_info" => {
                let session_id = self.resolve_or_create_session(&args).await?;
                let frame = self
                    .bridge
                    .request(ApiRequest::GetRuntimeInfo { session_id })
                    .await?;
                Ok(Value::Object(frame.fields.into_iter().collect()))
            }
            "daemon_start" => self.cli.server_control("start").await,
            "daemon_stop" => self.cli.server_control("stop").await,
            "daemon_reload" => self.cli.server_control("reload").await,
            "overview" => {
                let version = self
                    .cli
                    .version()
                    .await
                    .unwrap_or_else(|e| json!({ "error": e.to_string() }));
                let handshake = self
                    .bridge
                    .handshake_info()
                    .await
                    .unwrap_or_else(|e| json!({ "error": e.to_string() }));
                let ping = self
                    .bridge
                    .request(ApiRequest::Ping)
                    .await
                    .map(|f| json!({ "ev": f.ev, "ok": true }))
                    .unwrap_or_else(|e| json!({ "error": e.to_string() }));
                let provider = self
                    .cli
                    .provider_current(requested_cwd.as_ref())
                    .await
                    .unwrap_or_else(|e| json!({ "error": e.to_string() }));
                Ok(json!({
                    "version": version,
                    "bridge": handshake,
                    "ping": ping,
                    "provider": provider,
                    "default_cwd": self.bridge.config().default_cwd,
                    "ssh_host": self.bridge.config().ssh_host,
                }))
            }
            other => bail!("unsupported jcode_status action: {other}"),
        }
    }

    async fn tool_run(&self, args: Value) -> Result<Value> {
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("`prompt` is required and cannot be empty"))?;
        let cwd = args.get("cwd").and_then(Value::as_str);
        let mode = args.get("mode").and_then(Value::as_str).unwrap_or("bridge");
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(300)
            .clamp(5, 3600);
        let provider = args.get("provider").and_then(Value::as_str);
        let model = args.get("model").and_then(Value::as_str);

        if mode == "cli" {
            let result = self
                .cli
                .run_once(JcodeRunOptions {
                    prompt: prompt.to_string(),
                    cwd: cwd.map(str::to_string),
                    provider: provider.map(str::to_string),
                    model: model.map(str::to_string),
                    resume: args
                        .get("session_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    tool_profile: None,
                    tools: None,
                    disabled_tools: None,
                    timeout_secs: Some(timeout_secs),
                })
                .await?;
            return Ok(result);
        }

        let (session_id, created_new) =
            if let Some(existing) = args.get("session_id").and_then(Value::as_str) {
                let _ = self.bridge.attach_session(existing).await;
                (existing.to_string(), false)
            } else {
                let sys_prompt = args
                    .get("system_prompt")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let (sid, _) = self.bridge.create_session(cwd, sys_prompt, None).await?;
                (sid, true)
            };

        if let Some(m) = model {
            let _ = self
                .bridge
                .request(ApiRequest::SetModel {
                    session_id: session_id.clone(),
                    model: m.to_string(),
                })
                .await;
        }

        let summary = self
            .bridge
            .send_message_and_wait(
                &session_id,
                prompt.to_string(),
                None,
                true,
                Duration::from_secs(timeout_secs),
            )
            .await?;

        Ok(json!({
            "mode": "bridge",
            "created_session": created_new,
            "session_id": session_id,
            "cwd": self.bridge.config().resolve_working_dir(cwd).ok(),
            "summary": summary,
        }))
    }

    async fn tool_session_manage(&self, args: Value) -> Result<Value> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("`action` is required"))?;

        if action == "list" {
            let include_archived = args
                .get("include_archived")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let frame = self
                .bridge
                .request(ApiRequest::ListSessions { include_archived })
                .await?;
            return Ok(serde_json::to_value(frame)?);
        }

        if action == "create" {
            let cwd = args.get("cwd").and_then(Value::as_str);
            let sys_prompt = args
                .get("system_prompt")
                .and_then(Value::as_str)
                .map(str::to_string);
            let archive_days = args
                .get("archive_after_days")
                .and_then(Value::as_u64)
                .map(|v| v as u32);
            let (session_id, session) = self
                .bridge
                .create_session(cwd, sys_prompt, archive_days)
                .await?;
            return Ok(json!({
                "action": "create",
                "session_id": session_id,
                "session": session,
            }));
        }

        let session_id = args
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("`session_id` is required for action='{action}'"))?
            .to_string();

        let frame = match action {
            "attach" => {
                self.bridge
                    .request(ApiRequest::AttachSession { session_id })
                    .await?
            }
            "detach" => {
                self.bridge
                    .request(ApiRequest::DetachSession { session_id })
                    .await?
            }
            "peek" => {
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                self.bridge
                    .request(ApiRequest::PeekSession { session_id, limit })
                    .await?
            }
            "rename" => {
                let title = args
                    .get("title")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("`title` is required for action='rename'"))?
                    .to_string();
                self.bridge
                    .request(ApiRequest::RenameSession { session_id, title })
                    .await?
            }
            "archive" => {
                self.bridge
                    .request(ApiRequest::ArchiveSession { session_id })
                    .await?
            }
            "restore" => {
                self.bridge
                    .request(ApiRequest::RestoreSession { session_id })
                    .await?
            }
            "fork" => {
                self.bridge
                    .request(ApiRequest::ForkSession { session_id })
                    .await?
            }
            "history" => {
                self.bridge
                    .request(ApiRequest::GetHistory { session_id })
                    .await?
            }
            "todos" => {
                self.bridge
                    .request(ApiRequest::GetTodoState { session_id })
                    .await?
            }
            "tools" => {
                self.bridge
                    .request(ApiRequest::ListTools { session_id })
                    .await?
            }
            "poll" => {
                let summary = self.bridge.poll_session_summary(&session_id).await?;
                return Ok(serde_json::to_value(summary)?);
            }
            other => bail!("unsupported jcode_session_manage action: {other}"),
        };

        Ok(serde_json::to_value(frame)?)
    }

    async fn tool_session_prompt(&self, args: Value) -> Result<Value> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("send");
        let session_id = args
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("`session_id` is required"))?
            .to_string();

        match action {
            "send" => {
                let prompt = args
                    .get("prompt")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| anyhow!("`prompt` is required for action='send'"))?
                    .to_string();
                let reminder = args
                    .get("system_reminder")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(true);
                let timeout_secs = args
                    .get("timeout_secs")
                    .and_then(Value::as_u64)
                    .unwrap_or(300)
                    .clamp(5, 3600);
                let summary = self
                    .bridge
                    .send_message_and_wait(
                        &session_id,
                        prompt,
                        reminder,
                        wait,
                        Duration::from_secs(timeout_secs),
                    )
                    .await?;
                Ok(serde_json::to_value(summary)?)
            }
            "soft_interrupt" => {
                let content = args
                    .get("prompt")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("`prompt` is required for action='soft_interrupt'"))?
                    .to_string();
                let urgent = args.get("urgent").and_then(Value::as_bool).unwrap_or(false);
                let frame = self
                    .bridge
                    .request(ApiRequest::SoftInterrupt {
                        session_id,
                        content,
                        images: Vec::new(),
                        urgent,
                        client_id: None,
                    })
                    .await?;
                Ok(serde_json::to_value(frame)?)
            }
            "cancel" => {
                let frame = self
                    .bridge
                    .request(ApiRequest::Cancel { session_id })
                    .await?;
                Ok(serde_json::to_value(frame)?)
            }
            "clear" => {
                let frame = self
                    .bridge
                    .request(ApiRequest::Clear { session_id })
                    .await?;
                Ok(serde_json::to_value(frame)?)
            }
            "compact" => {
                let frame = self
                    .bridge
                    .request(ApiRequest::Compact { session_id })
                    .await?;
                Ok(serde_json::to_value(frame)?)
            }
            "rewind" => {
                let message_index = args
                    .get("message_index")
                    .or_else(|| args.get("count"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let frame = self
                    .bridge
                    .request(ApiRequest::Rewind {
                        session_id,
                        message_index,
                    })
                    .await?;
                Ok(serde_json::to_value(frame)?)
            }
            "rewind_undo" => {
                let frame = self
                    .bridge
                    .request(ApiRequest::RewindUndo { session_id })
                    .await?;
                Ok(serde_json::to_value(frame)?)
            }
            other => bail!("unsupported jcode_session_prompt action: {other}"),
        }
    }

    async fn tool_runtime_command(&self, args: Value) -> Result<Value> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("`command` is required"))?
            .to_string();
        let cmd_args = args
            .get("args")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let session_id = self.resolve_or_create_session(&args).await?;

        let req = match command.as_str() {
            "models" | "list_models" if cmd_args.is_empty() => {
                ApiRequest::ListModels { session_id }
            }
            "set_model" if !cmd_args.is_empty() => ApiRequest::SetModel {
                session_id,
                model: cmd_args[0].clone(),
            },
            "set_effort" if !cmd_args.is_empty() => ApiRequest::SetReasoningEffort {
                session_id,
                effort: cmd_args[0].clone(),
            },
            _ => ApiRequest::RuntimeCommand {
                session_id,
                command,
                args: cmd_args,
            },
        };

        let frame = self.bridge.request(req).await?;
        Ok(serde_json::to_value(frame)?)
    }

    async fn tool_workspace_file(&self, args: Value) -> Result<Value> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("`action` is required"))?;

        let session_id = self.resolve_or_create_session(&args).await?;

        match action {
            "find" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let limit = args
                    .get("limit")
                    .or_else(|| args.get("max_results"))
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                let frame = self
                    .bridge
                    .request(ApiRequest::FindFiles {
                        session_id,
                        query,
                        limit,
                    })
                    .await?;
                Ok(serde_json::to_value(frame)?)
            }
            "read" => {
                let path = args
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("`path` is required for action='read'"))?
                    .to_string();
                let max_bytes = args
                    .get("max_bytes")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                let start_line = args
                    .get("start_line")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                let end_line = args
                    .get("end_line")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                let mut frame = self
                    .bridge
                    .request(ApiRequest::ReadFile {
                        session_id,
                        path,
                        max_bytes,
                    })
                    .await?;

                if (start_line.is_some() || end_line.is_some())
                    && let Some(content) = frame
                        .fields
                        .get("content")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                {
                    let sliced = slice_lines(&content, start_line, end_line);
                    frame
                        .fields
                        .insert("content".to_string(), Value::String(sliced));
                }

                Ok(serde_json::to_value(frame)?)
            }
            "search" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| anyhow!("`query` is required for action='search'"))?
                    .to_string();
                let path = args.get("path").and_then(Value::as_str).map(str::to_string);
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                let frame = self
                    .bridge
                    .request(ApiRequest::SearchText {
                        session_id,
                        query,
                        path,
                        limit,
                    })
                    .await?;
                Ok(serde_json::to_value(frame)?)
            }
            "status" => {
                let path = args
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string();
                let frame = self
                    .bridge
                    .request(ApiRequest::FileStatus { session_id, path })
                    .await?;
                Ok(serde_json::to_value(frame)?)
            }
            other => bail!("unsupported jcode_workspace_file action: {other}"),
        }
    }
}

/// Slice a multi-line string by 1-based inclusive `[start_line, end_line]`.
pub fn slice_lines(content: &str, start_line: Option<usize>, end_line: Option<usize>) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let start_idx = start_line.unwrap_or(1).saturating_sub(1).min(lines.len());
    let end_idx = end_line
        .unwrap_or(lines.len())
        .min(lines.len())
        .max(start_idx);
    lines[start_idx..end_idx].join("\n")
}
