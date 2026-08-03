//! Инструменты агента: трейт `Tool`, реестр, конкретные инструменты Фазы 1.
//!
//! Фаза 1: fs (read/write/edit/ls), shell, grep; todo; web.
//! Каждый инструмент — реализация `Tool`. Схемы — в OpenAI tool-use формате.

pub mod apply_patch;
pub mod ask_user;
pub mod create_skill;
pub mod fs;
pub mod search;
pub mod send_message;
pub mod shell;
pub mod task;
pub mod todo;
pub mod web;

use async_trait::async_trait;
use serde_json::Value;
use vpsagent_core::{Error, EventKind, Id, Result};
use vpsagent_storage::Storage;

use crate::pending::{PendingPermissions, PendingRequests};
use crate::subagent::SubagentManager;
use crate::tools::todo::TodoState;

/// Контекст вызова инструмента (разделяемые зависимости агента).
#[derive(Clone)]
pub struct ToolContext {
    /// Рабочий каталог сессии (для относительных путей).
    pub cwd: std::path::PathBuf,
    /// Сессия и текущий агент.
    pub session_id: Id,
    pub agent_id: Id,
    /// Менеджер субагентов (инструменты task/task_list/...).
    pub subagents: SubagentManager,
    /// Канал событий (для ask_user → UserInputRequest).
    pub event_tx: tokio::sync::mpsc::UnboundedSender<EventKind>,
    /// Хранилище (для записи сообщений субагента).
    pub storage: Storage,
    /// Реестр ожидающих разрешений (C1): call_id → oneshot с решением.
    pub pending: PendingPermissions,
    /// Реестр ожидающих ответов пользователя (C15): request_id → oneshot.
    pub pending_requests: PendingRequests,
    /// Состояние todo-списка агента (C14): одно на агента, живёт между витками.
    pub todo_state: TodoState,
    /// Opt-in список каталогов вне cwd, куда разрешён доступ (C2).
    /// По умолчанию пусто — только cwd.
    pub allow_paths: Vec<std::path::PathBuf>,
}

impl Default for ToolContext {
    fn default() -> Self {
        panic!("ToolContext требует инициализации полей")
    }
}

/// Результат вызова инструмента — текст для возврата модели.
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }
    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// Трейт инструмента.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Имя (совпадает с именем в tool-use).
    fn name(&self) -> &'static str;

    /// Описание для модели (system-промпт / tool-list).
    fn description(&self) -> &'static str;

    /// JSON Schema параметров (OpenAI tool-use формат).
    fn schema(&self) -> Value;

    /// Выполнить вызов.
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput;
}

/// Реестр инструментов: имя → реализация.
pub struct Registry {
    tools: std::collections::HashMap<&'static str, std::sync::Arc<dyn Tool>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            tools: Default::default(),
        }
    }

    /// Реестр Фазы 1/2 со встроенными инструментами.
    /// `is_main` — главный агент (получает ask_user + task-менеджмент).
    pub fn phase1(is_main: bool) -> Self {
        let mut r = Self::new();
        r.register(std::sync::Arc::new(fs::Read));
        r.register(std::sync::Arc::new(fs::Write));
        r.register(std::sync::Arc::new(fs::Edit));
        r.register(std::sync::Arc::new(fs::MultiEdit));
        r.register(std::sync::Arc::new(apply_patch::ApplyPatch));
        r.register(std::sync::Arc::new(fs::Ls));
        r.register(std::sync::Arc::new(shell::Shell));
        r.register(std::sync::Arc::new(search::Grep));
        r.register(std::sync::Arc::new(todo::Todo));
        r.register(std::sync::Arc::new(web::WebFetch));
        r.register(std::sync::Arc::new(task::TaskSpawn));
        r.register(std::sync::Arc::new(task::TaskList));
        r.register(std::sync::Arc::new(task::TaskOutput));
        r.register(std::sync::Arc::new(task::TaskStop));
        r.register(std::sync::Arc::new(send_message::SendMessage));
        if is_main {
            r.register(std::sync::Arc::new(ask_user::AskUser));
            r.register(std::sync::Arc::new(create_skill::CreateSkill));
        }
        r
    }

    pub fn register(&mut self, tool: std::sync::Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn get(&self, name: &str) -> Option<std::sync::Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Все инструменты как массив (для передачи в ChatRequest.tools).
    pub fn schemas(&self) -> Vec<Value> {
        self.tools
            .values()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.schema(),
                    }
                })
            })
            .collect()
    }

    /// Оставить только инструменты из переданного набора имён (сужение прав).
    pub fn filter(&self, keep: &std::collections::HashSet<&str>) -> Self {
        let mut out = Self::new();
        for (name, tool) in &self.tools {
            if keep.contains(name) {
                out.tools.insert(name, tool.clone());
            }
        }
        out
    }

    /// Выполнить вызов по имени.
    pub async fn run(&self, name: &str, input: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let tool = self
            .get(name)
            .ok_or_else(|| Error::PermissionDenied(format!("инструмент '{name}' не найден")))?;
        Ok(tool.run(input, ctx).await)
    }
}
