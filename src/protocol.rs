//! Wire protocol types for Jcode `api-bridge` v1 and Model Context Protocol (MCP) JSON-RPC 2.0.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Default API version spoken by `jcode api-bridge`.
pub const JCODE_API_VERSION: u32 = 1;

/// MCP protocol version negotiated with Codex / MCP clients.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Outbound request envelope sent to `jcode api-bridge`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeRequestEnvelope {
    pub v: u32,
    pub id: u64,
    #[serde(flatten)]
    pub request: ApiRequest,
}

impl BridgeRequestEnvelope {
    pub fn new(id: u64, request: ApiRequest) -> Self {
        Self {
            v: JCODE_API_VERSION,
            id,
            request,
        }
    }
}

/// Request payloads supported by `jcode api-bridge` v1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "req", rename_all = "snake_case")]
pub enum ApiRequest {
    Hello {
        min_version: u32,
        max_version: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        client: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        supports_user_response: bool,
    },
    Ping,
    ListSessions {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        include_archived: bool,
    },
    CreateSession {
        #[serde(skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        archive_after_days: Option<u32>,
    },
    AttachSession {
        session_id: String,
    },
    DetachSession {
        session_id: String,
    },
    PeekSession {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    ArchiveSession {
        session_id: String,
    },
    RestoreSession {
        session_id: String,
    },
    ForkSession {
        session_id: String,
    },
    TransferSession {
        session_id: String,
    },
    RenameSession {
        session_id: String,
        title: String,
    },
    SetSessionSaved {
        session_id: String,
        saved: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Clear {
        session_id: String,
    },
    Compact {
        session_id: String,
    },
    Rewind {
        session_id: String,
        message_index: usize,
    },
    RewindUndo {
        session_id: String,
    },
    GetHistory {
        session_id: String,
    },
    GetRuntimeInfo {
        session_id: String,
    },
    ListModels {
        session_id: String,
    },
    SetModel {
        session_id: String,
        model: String,
    },
    SetReasoningEffort {
        session_id: String,
        effort: String,
    },
    GetTodoState {
        session_id: String,
    },
    ListTools {
        session_id: String,
    },
    SendMessage {
        session_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        system_reminder: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        no_reply: bool,
    },
    SoftInterrupt {
        session_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Value>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        urgent: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
    },
    CancelSoftInterrupts {
        session_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        client_ids: Vec<String>,
    },
    Cancel {
        session_id: String,
    },
    RuntimeCommand {
        session_id: String,
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
    },
    ReadFile {
        session_id: String,
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_bytes: Option<usize>,
    },
    FindFiles {
        session_id: String,
        query: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    SearchText {
        session_id: String,
        query: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    FileStatus {
        session_id: String,
        path: String,
    },
}

/// Inbound event or response frame emitted by `jcode api-bridge`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeEventFrame {
    #[serde(default = "default_api_version")]
    pub v: u32,
    #[serde(default)]
    pub reply_to: Option<u64>,
    pub ev: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

fn default_api_version() -> u32 {
    JCODE_API_VERSION
}

impl BridgeEventFrame {
    pub fn is_error(&self) -> bool {
        self.ev == "error"
    }

    pub fn error_code(&self) -> Option<&str> {
        self.fields.get("code")?.as_str()
    }

    pub fn error_message(&self) -> Option<&str> {
        self.fields.get("message")?.as_str()
    }
}

/// Summary of a turned conversation execution collected from `api-bridge` streaming events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnExecutionSummary {
    pub session_id: String,
    pub assembled_text: String,
    pub reasoning_text: String,
    pub status: String,
    pub stop_reason: Option<String>,
    pub tool_calls: Vec<ToolTraceEntry>,
    pub usage: Option<Value>,
    pub events_observed: usize,
}

/// Trace of a tool call observed during a session turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTraceEntry {
    pub call_id: Option<String>,
    pub name: String,
    pub input: Option<Value>,
    pub output: Option<String>,
    pub done: bool,
}

// ============================================================================
// MCP JSON-RPC 2.0 Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDefinition {
    pub name: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolResult {
    pub content: Vec<McpContentBlock>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpContentBlock {
    Text { text: String },
}

impl McpToolResult {
    pub fn success_json(payload: Value) -> Self {
        let pretty = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
        Self {
            content: vec![McpContentBlock::Text { text: pretty }],
            is_error: false,
            structured_content: Some(payload),
        }
    }

    pub fn error_message(message: impl Into<String>, details: Option<Value>) -> Self {
        let msg = message.into();
        let payload = serde_json::json!({
            "error": msg,
            "details": details,
        });
        Self {
            content: vec![McpContentBlock::Text {
                text: serde_json::to_string_pretty(&payload).unwrap_or(msg),
            }],
            is_error: true,
            structured_content: Some(payload),
        }
    }
}
