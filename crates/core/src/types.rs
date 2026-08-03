//! Общие типы VPSAgent: Message, Event, Agent, Session.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Уникальный идентификатор сессии/агента/сообщения.
pub type Id = Uuid;

/// Роль в диалоге.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

/// Блок содержимого сообщения (мультимодально; FR-075 — Image).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    /// Изображение (base64-данные + media_type для vision-моделей).
    Image { data_base64: String, media_type: String },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

/// Сообщение диалога.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Id,
    pub session_id: Id,
    pub agent_id: Option<Id>,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub tokens: Option<TokenUsage>,
    pub created_at: DateTime<Utc>,
}

impl Message {
    /// Собирает текст из всех Text-блоков (для отображения и промпта).
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// Расход токенов на сообщение.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u32,
    pub output: u32,
}

/// Статус агента (карточка deck-обзора).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    #[default]
    Waiting,
    Working,
    Done,
    Failed,
    Interrupted,
}

/// Тип агента.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Main,
    Sub,
}

/// Карточка агента для deck-обзора (FR-031) и agent.list (RPC).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: Id,
    pub session_id: Id,
    pub name: String,
    pub kind: AgentKind,
    pub status: AgentStatus,
    pub last_action: String,
    pub tokens: TokenUsage,
    pub model: String,
    pub parent_id: Option<Id>,
}

/// Статус сессии.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Active,
    Archived,
}

/// Сессия диалога.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Id,
    pub title: String,
    pub cwd: String,
    pub model: Option<String>,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Статус демона (daemon.status).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub version: String,
    pub pid: u32,
    pub uptime_secs: u64,
    pub sessions: usize,
    pub agents: usize,
}