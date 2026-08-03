//! MCP-клиент (FR-057): подключение внешних MCP-серверов по stdio.
//!
//! Протокол MCP поверх JSON-RPC 2.0 (по stdio): initialize, tools/list, tools/call.
//! Реализован на собственном транспорте (без внешней библиотеки), чтобы держать
//! дерево зависимостей лёгким. Инструменты регистрируются с префиксом `mcp__<server>__<tool>`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

use serde_json::{json, Value};
use vpsagent_core::Result;

/// Конфигурация MCP-сервера.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    /// Команда запуска (например "npx", "python3").
    pub command: String,
    /// Аргументы.
    pub args: Vec<String>,
}

/// Запущенный MCP-сервер (процесс + каналы).
pub struct McpClient {
    pub name: String,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Доступные инструменты.
    pub tools: Vec<McpTool>,
}

/// Инструмент MCP-сервера.
#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl McpClient {
    /// Запустить MCP-сервер и выполнить initialize.
    pub fn spawn(cfg: &McpServerConfig) -> Result<Self> {
        let mut child = std::process::Command::new(&cfg.command)
            .args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| vpsagent_core::Error::Provider(format!("mcp spawn: {e}")))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());

        let mut client = Self {
            name: cfg.name.clone(),
            child,
            stdin,
            stdout,
            tools: vec![],
        };
        // initialize.
        let _ = client.call(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "vpsagent", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        let _ = client.call("notifications/initialized", json!({}))?;
        client.load_tools()?;
        Ok(client)
    }

    /// JSON-RPC запрос (построчно по stdio), возвращает result.
    fn call(&mut self, method: &str, params: Value) -> Result<Option<Value>> {
        let id = format!("mcp-{}", self.name.len()); // простой id
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;

        let mut buf = String::new();
        // Читаем до строки с нашим id.
        loop {
            buf.clear();
            if self.stdout.read_line(&mut buf)? == 0 {
                return Ok(None);
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue, // notification, пропускаем
            };
            if v.get("id").and_then(|i| i.as_str()) == Some(id.as_str()) {
                return Ok(v.get("result").cloned());
            }
        }
    }

    /// Загрузить список инструментов.
    fn load_tools(&mut self) -> Result<()> {
        if let Some(Value::Object(res)) = self.call("tools/list", json!({}))? {
            if let Some(tools) = res.get("tools").and_then(|t| t.as_array()) {
                for t in tools {
                    let name = t
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let description = t
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input_schema = t.get("inputSchema").cloned().unwrap_or(json!({}));
                    self.tools.push(McpTool {
                        name,
                        description,
                        input_schema,
                    });
                }
            }
        }
        Ok(())
    }

    /// Вызвать инструмент.
    pub fn call_tool(&mut self, tool: &str, args: Value) -> Result<String> {
        match self.call("tools/call", json!({ "name": tool, "arguments": args }))? {
            Some(result) => Ok(serde_json::to_string(&result)?),
            None => Ok("(нет результата)".to_string()),
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Полное имя инструмента MCP в реестре агента: `mcp__<server>__<tool>`.
pub fn full_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

// Используем Result импорт.
#[allow(dead_code)]
fn _ensure(_: &Result<()>) {}
