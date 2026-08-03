//! Иерархия ошибок VPSAgent.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("ошибка конфигурации: {0}")]
    Config(String),

    #[error("ошибка хранилища: {0}")]
    Storage(String),

    #[error("ошибка провайдера LLM: {0}")]
    Provider(String),

    #[error("ошибка протокола: {0}")]
    Protocol(String),

    #[error("агент не найден: {0}")]
    AgentNotFound(String),

    #[error("сессия не найдена: {0}")]
    SessionNotFound(String),

    #[error("доступ запрещён: {0}")]
    PermissionDenied(String),

    #[error("операция отменена")]
    Cancelled,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
