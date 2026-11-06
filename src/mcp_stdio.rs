//! MCP `stdio` transport loop (JSON-RPC 2.0 over newline-delimited stdin/stdout).

use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::tools::JcodeMcpService;
use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Run the MCP server over `stdin`/`stdout`.
pub async fn serve_stdio(service: JcodeMcpService) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();

    while let Some(line) = lines
        .next_line()
        .await
        .context("failed reading MCP stdin")?
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed: Result<JsonRpcRequest, _> = serde_json::from_str(trimmed);
        let resp = match parsed {
            Ok(req) => service.handle_jsonrpc(req).await,
            Err(err) => Some(JsonRpcResponse::err(
                Value::Null,
                -32700,
                format!("Parse error: {err}"),
                None,
            )),
        };

        if let Some(reply) = resp {
            let encoded = serde_json::to_string(&reply)?;
            stdout
                .write_all(format!("{encoded}\n").as_bytes())
                .await
                .context("failed writing MCP stdout")?;
            stdout.flush().await.context("failed flushing MCP stdout")?;
        }
    }

    Ok(())
}
