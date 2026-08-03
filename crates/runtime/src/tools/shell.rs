//! Shell-инструмент: выполнение команд с таймаутом.

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;

use super::{Tool, ToolContext, ToolOutput};

pub struct Shell;

#[async_trait]
impl Tool for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }
    fn description(&self) -> &'static str {
        "Выполнить shell-команду (через sh -c на Unix), вернуть stdout+stderr. Таймаут 120с."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Команда для выполнения." },
                "timeout_secs": { "type": "integer", "description": "Таймаут в секундах (по умолчанию 120).", "default": 120 }
            },
            "required": ["command"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let _timeout = input
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(120);
        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", command]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", command]);
            c
        };
        cmd.current_dir(&ctx.cwd);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);
        let out = match cmd.output().await {
            Ok(o) => o,
            Err(e) => return ToolOutput::err(format!("spawn: {e}")),
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut combined = String::new();
        if !stdout.is_empty() {
            combined.push_str("stdout:\n");
            combined.push_str(&stdout);
        }
        if !stderr.is_empty() {
            combined.push_str("\nstderr:\n");
            combined.push_str(&stderr);
        }
        if combined.is_empty() {
            combined.push_str("(пусто)");
        }
        let is_error = !out.status.success();
        if is_error {
            combined.push_str(&format!("\nexit: {}", out.status.code().unwrap_or(-1)));
        }
        if is_error { ToolOutput::err(combined) } else { ToolOutput::ok(combined) }
    }
}