//! Todo-инструмент: трекер задач агента (FR-017).
//!
//! Состояние — [`TodoState`] в `ToolContext` (одно на агента, живёт между
//! витками). Действия: create (полная замена), update (статус по индексу,
//! не более одного in_progress), list (рендер), clear.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

use super::{Tool, ToolContext, ToolOutput};

/// Одна задача.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub priority: TodoPriority,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TodoPriority {
    High,
    Medium,
    Low,
}

/// Состояние todo агента (разделяемое между tool-вызовами одного агента).
#[derive(Default, Clone)]
pub struct TodoState(pub Arc<AsyncMutex<Vec<TodoItem>>>);

/// Рендер списка задач текстом (для модели и UI).
fn render(list: &[TodoItem]) -> String {
    if list.is_empty() {
        return "(список задач пуст)".into();
    }
    list.iter()
        .enumerate()
        .map(|(i, t)| {
            let mark = match t.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[~]",
                TodoStatus::Completed => "[x]",
            };
            let prio = match t.priority {
                TodoPriority::High => "high",
                TodoPriority::Medium => "medium",
                TodoPriority::Low => "low",
            };
            format!("{i}. {mark} ({prio}) {}", t.content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Оставить не более одного in_progress (первый по списку, остальные → pending).
fn enforce_single_in_progress(list: &mut [TodoItem]) {
    let mut seen = false;
    for item in list.iter_mut() {
        if item.status == TodoStatus::InProgress {
            if seen {
                item.status = TodoStatus::Pending;
            }
            seen = true;
        }
    }
}

fn parse_status(v: Option<&Value>) -> Option<TodoStatus> {
    match v.and_then(|v| v.as_str()) {
        Some("pending") => Some(TodoStatus::Pending),
        Some("in_progress") => Some(TodoStatus::InProgress),
        Some("completed") => Some(TodoStatus::Completed),
        _ => None,
    }
}

#[async_trait]
impl Tool for Todo {
    fn name(&self) -> &'static str {
        "todo"
    }
    fn description(&self) -> &'static str {
        "Управление списком задач агента: создание, обновление статуса, чтение. \
         Используй для нетривиальных многошаговых задач. Только один in_progress за раз."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["create", "update", "list", "clear"] },
                "todos": {
                    "type": "array",
                    "description": "Для action=create: полный список задач (замена).",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                            "priority": { "type": "string", "enum": ["high", "medium", "low"] }
                        },
                        "required": ["content", "status", "priority"]
                    }
                },
                "index": { "type": "integer", "description": "Для action=update: индекс задачи (0-based)." },
                "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
            },
            "required": ["action"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");
        let state = ctx.todo_state.0.clone();
        let mut list = state.lock().await;
        match action {
            "create" => {
                let raw = input.get("todos").cloned().unwrap_or(Value::Array(vec![]));
                let mut todos: Vec<TodoItem> = match serde_json::from_value(raw) {
                    Ok(t) => t,
                    Err(e) => return ToolOutput::err(format!("todo: битый список 'todos': {e}")),
                };
                enforce_single_in_progress(&mut todos);
                let created = todos.len();
                *list = todos;
                ToolOutput::ok(format!("Создано задач: {created}\n{}", render(&list)))
            }
            "update" => {
                let index = input
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .map(|i| i as usize);
                let status = parse_status(input.get("status"));
                let (Some(index), Some(status)) = (index, status) else {
                    return ToolOutput::err("todo update: нужны 'index' и 'status'");
                };
                if index >= list.len() {
                    return ToolOutput::err(format!(
                        "todo update: индекс {index} вне списка (всего {})",
                        list.len()
                    ));
                }
                // Новый in_progress сбрасывает прежний (один активный за раз).
                if status == TodoStatus::InProgress {
                    for item in list.iter_mut() {
                        if item.status == TodoStatus::InProgress {
                            item.status = TodoStatus::Pending;
                        }
                    }
                }
                list[index].status = status;
                ToolOutput::ok(render(&list))
            }
            "list" => ToolOutput::ok(render(&list)),
            "clear" => {
                list.clear();
                ToolOutput::ok("Список задач очищен")
            }
            other => ToolOutput::err(format!("todo: неизвестный action '{other}'")),
        }
    }
}

pub struct Todo;
