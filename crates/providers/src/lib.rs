//! `vpsagent-providers`: провайдеры LLM.
//!
//! Фаза 1: OpenAI-compatible API (chat/completions, SSE-стрим, tool-calling).
//! Фаза 2 добавит Anthropic; Фаза 4 — роутер.

pub mod anthropic;
pub mod mock;
pub mod openai;
pub mod openai_responses;
pub mod router;
pub mod stats;

pub use anthropic::Anthropic;
pub use mock::{MockProvider, MockStep};
pub use openai::OpenaiCompat;
pub use openai_responses::OpenaiResponses;
pub use router::{RouteRule, Router, TaskProfile};
pub use stats::{ModelRunMetric, ModelStats, ModelStatsRegistry};

use std::sync::Arc;
use vpsagent_core::{Config, EndpointKind};

/// Фабрика провайдера по типу endpoint'а (единая точка выбора).
pub fn make_provider(endpoint: &vpsagent_core::ModelEndpoint, api_key: &str) -> Arc<dyn Provider> {
    match endpoint.kind {
        EndpointKind::OpenaiCompat => Arc::new(OpenaiCompat::new(
            endpoint.name.clone(),
            endpoint.base_url.clone(),
            api_key,
        )),
        EndpointKind::OpenaiResponses => Arc::new(OpenaiResponses::new(
            endpoint.name.clone(),
            endpoint.base_url.clone(),
            api_key,
        )),
        EndpointKind::Anthropic => Arc::new(Anthropic::new(
            endpoint.name.clone(),
            endpoint.base_url.clone(),
            api_key,
        )),
    }
}

/// Получить endpoint по имени модели из конфига (удобная обёртка).
pub fn endpoint_for_model<'a>(
    config: &'a Config,
    model: &str,
) -> Option<&'a vpsagent_core::ModelEndpoint> {
    config.endpoint_for_model(model)
}

use futures::Stream;
use serde_json::Value;
use vpsagent_core::{Error, Message, Result, TokenUsage};

/// Запрос к провайдеру: история + системный промпт + инструменты.
///
/// `messages` — полные сообщения с ролями (User/Assistant/Tool).
/// Провайдер обязан мапить ContentBlock по роли сообщения:
/// - User → role user
/// - Assistant → role assistant (Text + ToolUse-блоки)
/// - Tool → role tool (ToolResult-блоки)
///
/// Это критично для корректного multi-turn: assistant-текст должен уходить
/// как assistant, а не как user (иначе контекст искажается, Anthropic — 400).
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    /// Инструменты как JSON Schema (для OpenAI-формата).
    pub tools: Vec<Value>,
    pub temperature: Option<f32>,
}

/// Чанк стрима ответа модели.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// Дельта текста.
    TextDelta(String),
    /// Запуск инструмента (может прийти в одном чанке).
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    /// Расход токенов на виток.
    Usage(TokenUsage),
    /// Конец ответа.
    Done,
}

/// Трейт провайдера.
pub trait Provider: Send + Sync {
    /// Имя (для логов и UI).
    fn name(&self) -> &str;

    /// Стрим ответа на запрос. Ретраи встроены внутри (см. `with_retries`).
    fn stream(
        &self,
        req: ChatRequest,
    ) -> std::pin::Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>;
}

/// Экспоненциальный backoff для ретраев API-ошибок (429/5xx).
pub fn backoff_delay(attempt: u32) -> std::time::Duration {
    let base = 2u64.pow(attempt.min(6));
    std::time::Duration::from_millis(base * 100 + (base % 200))
}

#[allow(dead_code)]
fn _ensure_error_variant() {
    let _ = Error::Provider("x".into());
}
