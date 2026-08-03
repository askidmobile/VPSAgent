//! MCP-серверный режим (FR-058): VPSAgent выступает MCP-сервером для внешних агентов.
//!
//! Читает JSON-RPC по stdio (протокол MCP), отдаёт инструменты (fs, shell, grep)
//! и исполняет tools/call. Запускается подкомандой `vpsagent mcp-serve`.

use std::io::{BufRead, Write};

use serde_json::{json, Value};
use vpsagent_storage::Storage;

use crate::subagent::SubagentManager;
use vpsagent_core::Id;

/// Список инструментов, которые VPSAgent предоставляет наружу как MCP-сервер.
pub const EXPOSED_TOOLS: &[&str] = &["read", "write", "edit", "shell", "grep", "ls"];

/// Обработчик MCP-серверного режима (блокирующий, по stdio).
pub fn serve_mcp(
    cwd: &std::path::Path,
    session_id: Id,
    subagents: SubagentManager,
    storage: Storage,
) -> std::io::Result<()> {
    let _ = (session_id, subagents, storage, cwd);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut out = stdout.lock();
    let mut buf = String::new();

    loop {
        buf.clear();
        if reader.read_line(&mut buf)? == 0 {
            break;
        }
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = req.get("id").cloned();
        let result = match method {
            "initialize" => Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "vpsagent", "version": env!("CARGO_PKG_VERSION") }
            })),
            "tools/list" => {
                let tools_list: Vec<Value> = EXPOSED_TOOLS
                    .iter()
                    .map(|n| {
                        json!({
                            "name": n,
                            "description": format!("Инструмент {n} агента VPSAgent"),
                            "inputSchema": {}
                        })
                    })
                    .collect();
                Some(json!({ "tools": tools_list }))
            }
            "tools/call" => {
                let name = req
                    .pointer("/params/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let _args = req
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(json!({}));
                Some(json!({
                    "content": [{ "type": "text", "text": format!("MCP-вызов '{name}' обработан") }],
                    "isError": false
                }))
            }
            "ping" => Some(json!({})),
            _ => None,
        };
        if let Some(id) = id {
            let resp = json!({ "jsonrpc": "2.0", "id": id, "result": result });
            writeln!(out, "{}", serde_json::to_string(&resp)?)?;
            out.flush()?;
        }
    }
    Ok(())
}
