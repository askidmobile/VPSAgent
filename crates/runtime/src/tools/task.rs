//! Инструменты управления субагентами: task (spawn), task_list, task_output, task_stop.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Tool, ToolContext, ToolOutput};

pub struct TaskSpawn;

#[async_trait]
impl Tool for TaskSpawn {
    fn name(&self) -> &'static str {
        "task"
    }
    fn description(&self) -> &'static str {
        "Спавн субагента из определения (см. task_list_defs для доступных). \
         Субагент работает параллельно с изолированным контекстом."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "definition": { "type": "string", "description": "Имя определения субагента." },
                "task": { "type": "string", "description": "Задача для субагента." },
                "fork": { "type": "boolean", "description": "Наследовать контекст родителя.", "default": false }
            },
            "required": ["definition", "task"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let def = input.get("definition").and_then(|v| v.as_str()).unwrap_or("");
        let task = input.get("task").and_then(|v| v.as_str()).unwrap_or("");
        let fork = input.get("fork").and_then(|v| v.as_bool()).unwrap_or(false);
        if def.is_empty() || task.is_empty() {
            return ToolOutput::err("нужны 'definition' и 'task'");
        }
        // fork-контекст в Фазе 2: пока пустой (наследуем только системный промпт).
        // Полная передача истории родителя — Фаза 7.
        let fork_context = vec![];
        match ctx
            .subagents
            .spawn(
                ctx.session_id,
                ctx.agent_id,
                def,
                task.to_string(),
                fork,
                fork_context,
                ctx.event_tx.clone(),
            )
            .await
        {
            Ok(id) => ToolOutput::ok(format!("Субагент '{def}' запущен: {id}")),
            Err(e) => ToolOutput::err(format!("task: {e}")),
        }
    }
}

pub struct TaskList;

#[async_trait]
impl Tool for TaskList {
    fn name(&self) -> &'static str {
        "task_list"
    }
    fn description(&self) -> &'static str {
        "Список активных субагентов (id, имя, статус)."
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn run(&self, _input: Value, ctx: &ToolContext) -> ToolOutput {
        let agents = ctx.subagents.list().await;
        if agents.is_empty() {
            return ToolOutput::ok("(активных субагентов нет)");
        }
        let out = agents
            .iter()
            .map(|a| format!("{} {} [{}]", a.id, a.name, status_str(a.status)))
            .collect::<Vec<_>>()
            .join("\n");
        ToolOutput::ok(out)
    }
}

pub struct TaskOutput;

#[async_trait]
impl Tool for TaskOutput {
    fn name(&self) -> &'static str {
        "task_output"
    }
    fn description(&self) -> &'static str {
        "Вывод субагента по id (накопленный текст)."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let id = input.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let Ok(agent_id) = uuid::Uuid::parse_str(id) else {
            return ToolOutput::err("некорректный id");
        };
        let out = ctx.subagents.output(agent_id).await;
        if out.is_empty() {
            ToolOutput::ok("(пока нет вывода)")
        } else {
            ToolOutput::ok(out)
        }
    }
}

pub struct TaskStop;

#[async_trait]
impl Tool for TaskStop {
    fn name(&self) -> &'static str {
        "task_stop"
    }
    fn description(&self) -> &'static str {
        "Прервать субагента по id."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let id = input.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let Ok(agent_id) = uuid::Uuid::parse_str(id) else {
            return ToolOutput::err("некорректный id");
        };
        ctx.subagents.interrupt(agent_id).await;
        ToolOutput::ok("субагент остановлен")
    }
}

fn status_str(s: vpsagent_core::AgentStatus) -> &'static str {
    use vpsagent_core::AgentStatus::*;
    match s {
        Working => "работает",
        Waiting => "ждёт",
        Done => "завершён",
        Failed => "ошибка",
        Interrupted => "прерван",
    }
}

// Держим импорт Id для ясности API.
#[allow(dead_code)]
fn _ensure(_: vpsagent_core::Id) {}
