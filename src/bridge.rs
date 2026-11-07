//! Asynchronous client for Jcode `api-bridge` (`--stdio` subprocess, `--ssh` remote, or Unix socket).

use crate::protocol::{
    ApiRequest, BridgeEventFrame, BridgeRequestEnvelope, JCODE_API_VERSION, ToolTraceEntry,
    TurnExecutionSummary,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify, oneshot};
use tokio::time::timeout;

/// Configuration for connecting to a local or remote `jcode` instance.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Path or binary name for `jcode` (default: `"jcode"`).
    pub jcode_bin: PathBuf,
    /// Default working directory on the local machine.
    pub default_cwd: Option<PathBuf>,
    /// Optional allowed workspace root prefixes. When non-empty, requested local working directories
    /// must reside inside one of these canonicalized roots.
    pub allowed_cwd_roots: Vec<PathBuf>,
    /// Optional custom internal daemon socket (`--socket`).
    pub daemon_socket: Option<PathBuf>,
    /// Optional direct `api-bridge` Unix socket path. When `None`, spawns `jcode api-bridge --stdio`.
    pub api_socket: Option<PathBuf>,
    /// Optional SSH host (`--ssh <HOST>`) to forward operations to a remote Jcode daemon.
    pub ssh_host: Option<String>,
    /// Optional remote `jcode` binary path when using `--ssh`.
    pub ssh_binary: Option<String>,
    /// Optional remote daemon socket path when using `--ssh`.
    pub ssh_server_socket: Option<String>,
    /// Optional remote working directory (`--remote-working-dir`).
    pub remote_working_dir: Option<String>,
    /// Default request timeout for control-plane RPCs.
    pub request_timeout: Duration,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            jcode_bin: PathBuf::from(
                std::env::var("JCODE_BIN").unwrap_or_else(|_| "jcode".to_string()),
            ),
            default_cwd: std::env::current_dir().ok(),
            allowed_cwd_roots: Vec::new(),
            daemon_socket: None,
            api_socket: None,
            ssh_host: None,
            ssh_binary: None,
            ssh_server_socket: None,
            remote_working_dir: None,
            request_timeout: Duration::from_secs(30),
        }
    }
}

impl BridgeConfig {
    /// Validate and resolve a requested working directory against `default_cwd` and `allowed_cwd_roots`.
    pub fn resolve_working_dir(&self, requested: Option<&str>) -> Result<PathBuf> {
        let candidate = match requested.map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => {
                let p = PathBuf::from(raw);
                if p.is_absolute() {
                    p
                } else if let Some(base) = &self.default_cwd {
                    base.join(p)
                } else {
                    std::env::current_dir()
                        .context("failed to resolve relative cwd")?
                        .join(p)
                }
            }
            None => self
                .default_cwd
                .clone()
                .or_else(|| std::env::current_dir().ok())
                .ok_or_else(|| {
                    anyhow!("no working directory specified and current_dir unavailable")
                })?,
        };

        // If connecting via SSH without local directory requirement, skip local canonicalization if path doesn't exist locally
        if self.ssh_host.is_some() && !candidate.exists() {
            return Ok(candidate);
        }

        let canonical = candidate.canonicalize().with_context(|| {
            format!(
                "working directory does not exist or is inaccessible: {}",
                candidate.display()
            )
        })?;

        if !canonical.is_dir() {
            bail!(
                "working directory path is not a directory: {}",
                canonical.display()
            );
        }

        if !self.allowed_cwd_roots.is_empty() {
            let mut allowed = false;
            for root in &self.allowed_cwd_roots {
                if let Ok(canon_root) = root.canonicalize()
                    && canonical.starts_with(&canon_root)
                {
                    allowed = true;
                    break;
                }
            }
            if !allowed {
                bail!(
                    "working directory '{}' is outside allowed workspace roots",
                    canonical.display()
                );
            }
        }

        Ok(canonical)
    }

    /// Build base CLI arguments (`--quiet --no-update --no-selfdev` + socket/ssh flags).
    pub fn base_cli_args(&self, cwd: Option<&PathBuf>) -> Vec<String> {
        let mut args = vec![
            "--quiet".to_string(),
            "--no-update".to_string(),
            "--no-selfdev".to_string(),
        ];
        if let Some(dir) = cwd {
            args.push("-C".to_string());
            args.push(dir.to_string_lossy().to_string());
        }
        if let Some(sock) = &self.daemon_socket {
            args.push("--socket".to_string());
            args.push(sock.to_string_lossy().to_string());
        }
        if let Some(host) = &self.ssh_host {
            args.push("--ssh".to_string());
            args.push(host.clone());
        }
        if let Some(bin) = &self.ssh_binary {
            args.push("--ssh-binary".to_string());
            args.push(bin.clone());
        }
        if let Some(remote_sock) = &self.ssh_server_socket {
            args.push("--ssh-server-socket".to_string());
            args.push(remote_sock.clone());
        }
        if let Some(remote_dir) = &self.remote_working_dir {
            args.push("--remote-working-dir".to_string());
            args.push(remote_dir.clone());
        }
        args
    }
}

/// Per-session event buffer and state tracked by `BridgeClient`.
#[derive(Debug, Default)]
pub struct SessionEventBuffer {
    pub events: VecDeque<BridgeEventFrame>,
    pub assembled_text: String,
    pub reasoning_text: String,
    pub status: String,
    pub stop_reason: Option<String>,
    pub tool_calls: Vec<ToolTraceEntry>,
    pub last_usage: Option<Value>,
    pub turn_completed: bool,
}

impl SessionEventBuffer {
    pub fn reset_turn(&mut self) {
        self.assembled_text.clear();
        self.reasoning_text.clear();
        self.stop_reason = None;
        self.tool_calls.clear();
        self.turn_completed = false;
        self.status = "processing".to_string();
    }

    pub fn ingest(&mut self, frame: BridgeEventFrame) {
        match frame.ev.as_str() {
            "text_delta" => {
                if let Some(delta) = frame.fields.get("delta").and_then(Value::as_str) {
                    self.assembled_text.push_str(delta);
                } else if let Some(text) = frame.fields.get("text").and_then(Value::as_str) {
                    self.assembled_text.push_str(text);
                }
            }
            "text_replace" | "text_done" => {
                if let Some(text) = frame
                    .fields
                    .get("text")
                    .or_else(|| frame.fields.get("content"))
                    .and_then(Value::as_str)
                {
                    self.assembled_text = text.to_string();
                }
            }
            "reasoning_delta" => {
                if let Some(delta) = frame.fields.get("delta").and_then(Value::as_str) {
                    self.reasoning_text.push_str(delta);
                }
            }
            "tool_start" | "tool_call" => {
                let name = frame
                    .fields
                    .get("tool_name")
                    .or_else(|| frame.fields.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let call_id = frame
                    .fields
                    .get("call_id")
                    .or_else(|| frame.fields.get("tool_call_id"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let input = frame.fields.get("input").cloned();
                self.tool_calls.push(ToolTraceEntry {
                    call_id,
                    name,
                    input,
                    output: None,
                    done: false,
                });
            }
            "tool_done" => {
                let call_id = frame
                    .fields
                    .get("call_id")
                    .or_else(|| frame.fields.get("tool_call_id"))
                    .and_then(Value::as_str);
                let output = frame
                    .fields
                    .get("output")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(entry) = self
                    .tool_calls
                    .iter_mut()
                    .rev()
                    .find(|t| !t.done && (call_id.is_none() || t.call_id.as_deref() == call_id))
                {
                    entry.output = output;
                    entry.done = true;
                }
            }
            "token_usage" => {
                self.last_usage = Some(Value::Object(frame.fields.clone().into_iter().collect()));
            }
            "session_status" => {
                if let Some(st) = frame.fields.get("status").and_then(Value::as_str) {
                    self.status = st.to_string();
                    if st == "idle" && !self.assembled_text.is_empty() {
                        self.turn_completed = true;
                    }
                }
            }
            "turn_done" => {
                self.status = "idle".to_string();
                self.turn_completed = true;
            }
            "turn_stopped" => {
                self.status = "stopped".to_string();
                self.stop_reason = frame
                    .fields
                    .get("reason")
                    .or_else(|| frame.fields.get("provider_stop_reason"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.turn_completed = true;
            }
            _ => {}
        }

        if self.events.len() >= 512 {
            self.events.pop_front();
        }
        self.events.push_back(frame);
    }

    pub fn snapshot_summary(&self, session_id: &str) -> TurnExecutionSummary {
        TurnExecutionSummary {
            session_id: session_id.to_string(),
            assembled_text: self.assembled_text.clone(),
            reasoning_text: self.reasoning_text.clone(),
            status: if self.status.is_empty() {
                "unknown".to_string()
            } else {
                self.status.clone()
            },
            stop_reason: self.stop_reason.clone(),
            tool_calls: self.tool_calls.clone(),
            usage: self.last_usage.clone(),
            events_observed: self.events.len(),
        }
    }
}

type BoxedReader = Box<dyn AsyncRead + Send + Unpin>;
type BoxedWriter = Box<dyn AsyncWrite + Send + Unpin>;

struct ActiveTransport {
    writer: Mutex<BoxedWriter>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<BridgeEventFrame>>>>,
    sessions: Arc<Mutex<HashMap<String, SessionEventBuffer>>>,
    session_notify: Arc<Notify>,
    capabilities: Vec<String>,
    server_version: String,
    _child: Option<Mutex<Child>>,
}

impl ActiveTransport {
    async fn send_request_with_timeout(
        &self,
        id: u64,
        request: ApiRequest,
        max_wait: Duration,
    ) -> Result<BridgeEventFrame> {
        let envelope = BridgeRequestEnvelope::new(id, request);
        let serialized =
            serde_json::to_string(&envelope).context("failed to serialize api-bridge request")?;

        let (tx, rx) = oneshot::channel();
        {
            let mut pend = self.pending.lock().await;
            pend.insert(id, tx);
        }

        {
            let mut writer = self.writer.lock().await;
            if let Err(err) = writer.write_all(format!("{serialized}\n").as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(err).context("failed to write request to jcode api-bridge");
            }
            if let Err(err) = writer.flush().await {
                self.pending.lock().await.remove(&id);
                return Err(err).context("failed to flush request to jcode api-bridge");
            }
        }

        match timeout(max_wait, rx).await {
            Ok(Ok(frame)) => {
                if frame.is_error() {
                    let code = frame.error_code().unwrap_or("unknown_error");
                    let msg = frame.error_message().unwrap_or("api-bridge returned error");
                    bail!("jcode api-bridge error [{code}]: {msg}");
                }
                Ok(frame)
            }
            Ok(Err(_)) => bail!("jcode api-bridge connection closed while awaiting reply"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!(
                    "jcode api-bridge request id={id} timed out after {}s",
                    max_wait.as_secs()
                );
            }
        }
    }
}

/// Multiplexed asynchronous client for `jcode api-bridge`.
#[derive(Clone)]
pub struct BridgeClient {
    config: BridgeConfig,
    next_id: Arc<AtomicU64>,
    transport: Arc<Mutex<Option<Arc<ActiveTransport>>>>,
}

impl BridgeClient {
    pub fn new(config: BridgeConfig) -> Self {
        Self {
            config,
            next_id: Arc::new(AtomicU64::new(1)),
            transport: Arc::new(Mutex::new(None)),
        }
    }

    pub fn config(&self) -> &BridgeConfig {
        &self.config
    }

    fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Ensure an active, handshaken connection to `jcode api-bridge` exists.
    async fn ensure_connected(&self) -> Result<Arc<ActiveTransport>> {
        let mut guard = self.transport.lock().await;
        if let Some(existing) = guard.as_ref() {
            return Ok(existing.clone());
        }

        let connected = self.connect_fresh().await?;
        let arc = Arc::new(connected);
        *guard = Some(arc.clone());
        Ok(arc)
    }

    async fn connect_fresh(&self) -> Result<ActiveTransport> {
        let (reader, writer, child): (BoxedReader, BoxedWriter, Option<Mutex<Child>>) =
            if let Some(sock_path) = &self.config.api_socket {
                let stream = UnixStream::connect(sock_path).await.with_context(|| {
                    format!(
                        "failed to connect to jcode api-bridge socket at {}",
                        sock_path.display()
                    )
                })?;
                let (r, w) = stream.into_split();
                (Box::new(r), Box::new(w), None)
            } else {
                let mut args = self.config.base_cli_args(self.config.default_cwd.as_ref());
                args.push("api-bridge".to_string());
                args.push("--stdio".to_string());

                let mut cmd = Command::new(&self.config.jcode_bin);
                cmd.args(&args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);

                let mut child = cmd.spawn().with_context(|| {
                    format!(
                        "failed to spawn `{}` with args {:?}",
                        self.config.jcode_bin.display(),
                        args
                    )
                })?;
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| anyhow!("missing stdout from jcode api-bridge"))?;
                let stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| anyhow!("missing stdin from jcode api-bridge"))?;
                (Box::new(stdout), Box::new(stdin), Some(Mutex::new(child)))
            };

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<BridgeEventFrame>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let sessions: Arc<Mutex<HashMap<String, SessionEventBuffer>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let session_notify = Arc::new(Notify::new());

        let pending_bg = pending.clone();
        let sessions_bg = sessions.clone();
        let notify_bg = session_notify.clone();
        let transport_slot = self.transport.clone();

        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(frame) = serde_json::from_str::<BridgeEventFrame>(trimmed) else {
                    continue;
                };

                if let Some(sid) = frame.session_id.clone().or_else(|| {
                    frame
                        .fields
                        .get("session")
                        .and_then(|s| s.get("session_id"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }) {
                    let mut sess_map = sessions_bg.lock().await;
                    let buf = sess_map.entry(sid).or_default();
                    buf.ingest(frame.clone());
                    drop(sess_map);
                    notify_bg.notify_waiters();
                }

                if let Some(reply_id) = frame.reply_to {
                    let mut pend = pending_bg.lock().await;
                    if let Some(tx) = pend.remove(&reply_id) {
                        let _ = tx.send(frame);
                    }
                }
            }

            {
                let mut slot = transport_slot.lock().await;
                *slot = None;
            }
            let mut pend = pending_bg.lock().await;
            pend.clear();
            notify_bg.notify_waiters();
        });

        let mut active = ActiveTransport {
            writer: Mutex::new(writer),
            pending,
            sessions,
            session_notify,
            capabilities: Vec::new(),
            server_version: String::new(),
            _child: child,
        };

        let hello_id = self.alloc_id();
        let hello_reply = active
            .send_request_with_timeout(
                hello_id,
                ApiRequest::Hello {
                    min_version: JCODE_API_VERSION,
                    max_version: JCODE_API_VERSION,
                    client: Some("jcode-rpc-for-codex/0.1.0".to_string()),
                    supports_user_response: false,
                },
                self.config.request_timeout,
            )
            .await?;

        if hello_reply.ev != "hello_ok" {
            bail!(
                "unexpected handshake response from jcode api-bridge: {}",
                hello_reply.ev
            );
        }

        active.server_version = hello_reply
            .fields
            .get("server")
            .and_then(Value::as_str)
            .unwrap_or("jcode-harness-api-bridge")
            .to_string();

        if let Some(caps) = hello_reply
            .fields
            .get("capabilities")
            .and_then(Value::as_array)
        {
            active.capabilities = caps
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
        }

        Ok(active)
    }

    /// Return negotiated bridge handshake metadata (`server` and `capabilities`).
    pub async fn handshake_info(&self) -> Result<Value> {
        let conn = self.ensure_connected().await?;
        Ok(serde_json::json!({
            "server": conn.server_version,
            "capabilities": conn.capabilities,
        }))
    }

    /// Send an `ApiRequest` and wait for its correlated `reply_to` frame.
    pub async fn request(&self, req: ApiRequest) -> Result<BridgeEventFrame> {
        self.request_with_timeout(req, self.config.request_timeout)
            .await
    }

    /// Send an `ApiRequest` with a custom timeout.
    pub async fn request_with_timeout(
        &self,
        req: ApiRequest,
        max_wait: Duration,
    ) -> Result<BridgeEventFrame> {
        let conn = self.ensure_connected().await?;
        let id = self.alloc_id();
        conn.send_request_with_timeout(id, req, max_wait).await
    }

    /// Create a new Jcode session in the given working directory and return its `session_id` and session payload.
    pub async fn create_session(
        &self,
        cwd: Option<&str>,
        system_prompt: Option<String>,
        archive_after_days: Option<u32>,
    ) -> Result<(String, Value)> {
        let resolved_cwd = self.config.resolve_working_dir(cwd)?;
        let cwd_str = resolved_cwd.to_string_lossy().to_string();
        let frame = self
            .request(ApiRequest::CreateSession {
                working_dir: Some(cwd_str.clone()),
                system_prompt,
                archive_after_days,
            })
            .await?;

        let session_val = frame
            .fields
            .get("session")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let session_id = session_val
            .get("session_id")
            .and_then(Value::as_str)
            .or(frame.session_id.as_deref())
            .ok_or_else(|| anyhow!("create_session response did not include session_id"))?
            .to_string();

        // Persist session metadata immediately so `find_files` / `read_file` / `search_text` / `file_status`
        // can resolve the session's `working_dir` even before the first prompt turn.
        let _ = self
            .request(ApiRequest::RenameSession {
                session_id: session_id.clone(),
                title: format!("codex-rpc: {cwd_str}"),
            })
            .await;

        Ok((session_id, session_val))
    }

    /// Ensure the given session is attached on this bridge connection.
    pub async fn attach_session(&self, session_id: &str) -> Result<BridgeEventFrame> {
        self.request(ApiRequest::AttachSession {
            session_id: session_id.to_string(),
        })
        .await
    }

    /// Send a prompt to a session and optionally block until `turn_done` / `turn_stopped`.
    pub async fn send_message_and_wait(
        &self,
        session_id: &str,
        content: String,
        system_reminder: Option<String>,
        wait_for_completion: bool,
        turn_timeout: Duration,
    ) -> Result<TurnExecutionSummary> {
        let conn = self.ensure_connected().await?;
        {
            let mut sessions = conn.sessions.lock().await;
            let buf = sessions.entry(session_id.to_string()).or_default();
            buf.reset_turn();
        }

        let id = self.alloc_id();
        let ack = conn
            .send_request_with_timeout(
                id,
                ApiRequest::SendMessage {
                    session_id: session_id.to_string(),
                    content,
                    images: Vec::new(),
                    system_reminder,
                    no_reply: false,
                },
                self.config.request_timeout,
            )
            .await?;

        // Record the ack frame into the session buffer if it was not already tagged with session_id
        if ack.session_id.is_none() {
            let mut sessions = conn.sessions.lock().await;
            let buf = sessions.entry(session_id.to_string()).or_default();
            buf.ingest(ack);
        }

        if !wait_for_completion {
            let sessions = conn.sessions.lock().await;
            let summary = sessions
                .get(session_id)
                .map(|b| b.snapshot_summary(session_id))
                .unwrap_or_else(|| TurnExecutionSummary {
                    session_id: session_id.to_string(),
                    status: "accepted".to_string(),
                    ..Default::default()
                });
            return Ok(summary);
        }

        let wait_fut = async {
            loop {
                let notified = conn.session_notify.notified();
                {
                    let sessions = conn.sessions.lock().await;
                    if let Some(buf) = sessions.get(session_id)
                        && buf.turn_completed
                    {
                        return Ok::<TurnExecutionSummary, anyhow::Error>(
                            buf.snapshot_summary(session_id),
                        );
                    }
                }
                if self.transport.lock().await.is_none() {
                    bail!("jcode api-bridge disconnected while waiting for turn completion");
                }
                notified.await;
            }
        };

        match timeout(turn_timeout, wait_fut).await {
            Ok(res) => res,
            Err(_) => {
                let sessions = conn.sessions.lock().await;
                let mut partial = sessions
                    .get(session_id)
                    .map(|b| b.snapshot_summary(session_id))
                    .unwrap_or_else(|| TurnExecutionSummary {
                        session_id: session_id.to_string(),
                        ..Default::default()
                    });
                partial.status = "timeout_in_progress".to_string();
                Ok(partial)
            }
        }
    }

    /// Poll buffered streaming state for a session without sending a new prompt.
    pub async fn poll_session_summary(&self, session_id: &str) -> Result<TurnExecutionSummary> {
        let conn = self.ensure_connected().await?;
        let sessions = conn.sessions.lock().await;
        Ok(sessions
            .get(session_id)
            .map(|b| b.snapshot_summary(session_id))
            .unwrap_or_else(|| TurnExecutionSummary {
                session_id: session_id.to_string(),
                status: "untracked".to_string(),
                ..Default::default()
            }))
    }
}
