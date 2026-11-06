//! Wrapper around non-interactive `jcode` CLI subcommands (`run --json`, `auth status --json`, `model list --json`, `version --json`, `server`).

use crate::bridge::BridgeConfig;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

/// Options for a one-shot `jcode run --json` execution.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct JcodeRunOptions {
    pub prompt: String,
    pub cwd: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub resume: Option<String>,
    pub tool_profile: Option<String>,
    pub tools: Option<String>,
    pub disabled_tools: Option<String>,
    pub timeout_secs: Option<u64>,
}

/// CLI runner bound to a `BridgeConfig`.
#[derive(Debug, Clone)]
pub struct JcodeCli {
    config: BridgeConfig,
}

impl JcodeCli {
    pub fn new(config: BridgeConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &BridgeConfig {
        &self.config
    }

    /// Execute an arbitrary `jcode` subcommand expecting JSON output on stdout.
    pub async fn run_json_command(
        &self,
        subcommand_args: &[String],
        cwd: Option<&PathBuf>,
        max_duration: Duration,
    ) -> Result<Value> {
        let raw = self
            .run_raw_command(subcommand_args, cwd, max_duration)
            .await?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(serde_json::json!({ "ok": true }));
        }
        serde_json::from_str::<Value>(trimmed).with_context(|| {
            format!(
                "failed to parse JSON output from `jcode {}`: {}",
                subcommand_args.join(" "),
                trimmed
            )
        })
    }

    /// Execute a `jcode` command and return stdout as UTF-8 text.
    pub async fn run_raw_command(
        &self,
        subcommand_args: &[String],
        cwd: Option<&PathBuf>,
        max_duration: Duration,
    ) -> Result<String> {
        let mut args = self.config.base_cli_args(cwd);
        args.extend_from_slice(subcommand_args);

        let mut cmd = Command::new(&self.config.jcode_bin);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        } else if let Some(default_dir) = &self.config.default_cwd
            && default_dir.is_dir()
        {
            cmd.current_dir(default_dir);
        }

        let output = timeout(max_duration, cmd.output())
            .await
            .map_err(|_| {
                anyhow!(
                    "`jcode {}` timed out after {}s",
                    subcommand_args.join(" "),
                    max_duration.as_secs()
                )
            })?
            .with_context(|| {
                format!(
                    "failed to execute `{}` with args {:?}",
                    self.config.jcode_bin.display(),
                    args
                )
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            bail!(
                "`jcode {}` exited with status {}: {}",
                subcommand_args.join(" "),
                output.status,
                if stderr.trim().is_empty() {
                    stdout.trim()
                } else {
                    stderr.trim()
                }
            );
        }

        Ok(stdout)
    }

    /// Inspect `jcode version --json`.
    pub async fn version(&self) -> Result<Value> {
        self.run_json_command(
            &["version".to_string(), "--json".to_string()],
            None,
            Duration::from_secs(15),
        )
        .await
    }

    /// Inspect `jcode auth status --json`.
    pub async fn auth_status(&self) -> Result<Value> {
        self.run_json_command(
            &[
                "auth".to_string(),
                "status".to_string(),
                "--json".to_string(),
            ],
            None,
            Duration::from_secs(15),
        )
        .await
    }

    /// Inspect `jcode provider current --json`.
    pub async fn provider_current(&self, cwd: Option<&PathBuf>) -> Result<Value> {
        self.run_json_command(
            &[
                "provider".to_string(),
                "current".to_string(),
                "--json".to_string(),
            ],
            cwd,
            Duration::from_secs(15),
        )
        .await
    }

    /// Inspect `jcode model list --json`.
    pub async fn model_list(&self, provider: Option<&str>) -> Result<Value> {
        let mut args = Vec::new();
        if let Some(p) = provider.filter(|s| !s.trim().is_empty()) {
            args.push("--provider".to_string());
            args.push(p.to_string());
        }
        args.push("model".to_string());
        args.push("list".to_string());
        args.push("--json".to_string());
        self.run_json_command(&args, None, Duration::from_secs(20))
            .await
    }

    /// Manage the background `jcode` server (`start`, `stop`, `reload`).
    pub async fn server_control(&self, action: &str) -> Result<Value> {
        match action {
            "start" | "stop" | "reload" => {}
            other => bail!(
                "unsupported server action '{}'; expected start, stop, or reload",
                other
            ),
        }
        let out = self
            .run_raw_command(
                &["server".to_string(), action.to_string()],
                None,
                Duration::from_secs(30),
            )
            .await?;
        Ok(serde_json::json!({
            "action": action,
            "ok": true,
            "message": out.trim(),
        }))
    }

    /// Run a one-shot prompt via `jcode run --json` inside the resolved workspace directory.
    pub async fn run_once(&self, opts: JcodeRunOptions) -> Result<Value> {
        if opts.prompt.trim().is_empty() {
            bail!("prompt must not be empty");
        }
        let resolved_cwd = self.config.resolve_working_dir(opts.cwd.as_deref())?;
        let mut args = Vec::new();

        if let Some(provider) = opts.provider.as_deref().filter(|s| !s.trim().is_empty()) {
            args.push("--provider".to_string());
            args.push(provider.to_string());
        }
        if let Some(model) = opts.model.as_deref().filter(|s| !s.trim().is_empty()) {
            args.push("--model".to_string());
            args.push(model.to_string());
        }
        if let Some(resume) = opts.resume.as_deref().filter(|s| !s.trim().is_empty()) {
            args.push("--resume".to_string());
            args.push(resume.to_string());
        }
        if let Some(profile) = opts
            .tool_profile
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            args.push("--tool-profile".to_string());
            args.push(profile.to_string());
        }
        if let Some(tools) = opts.tools.as_deref().filter(|s| !s.trim().is_empty()) {
            args.push("--tools".to_string());
            args.push(tools.to_string());
        }
        if let Some(disabled) = opts
            .disabled_tools
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            args.push("--disabled-tools".to_string());
            args.push(disabled.to_string());
        }

        args.push("run".to_string());
        args.push("--json".to_string());
        args.push(opts.prompt);

        let max_dur = Duration::from_secs(opts.timeout_secs.unwrap_or(600).clamp(5, 3600));
        let mut result = self
            .run_json_command(&args, Some(&resolved_cwd), max_dur)
            .await?;

        if let Some(obj) = result.as_object_mut() {
            obj.entry("working_dir".to_string())
                .or_insert_with(|| Value::String(resolved_cwd.to_string_lossy().to_string()));
        }
        Ok(result)
    }
}
