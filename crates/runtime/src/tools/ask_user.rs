//! Инструмент ask_user: агент задаёт структурированный вопрос пользователю mid-task.
//!
//! Отправляет `EventKind::UserInputRequest` с request_id, регистрирует oneshot
//! в `ctx.pending_requests` и ждёт ответа (до 120 сек). Ответ доставляет демон
//! через `Request::UserInputAnswer` → `PendingRequests::answer_text`.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;
use vpsagent_core::EventKind;

use crate::pending::Answer;

use super::{Tool, ToolContext, ToolOutput};

/// Таймаут ожидания ответа пользователя.
const ASK_USER_TIMEOUT: Duration = Duration::from_secs(120);

pub struct AskUser;

#[async_trait]
impl Tool for AskUser {
    fn name(&self) -> &'static str {
        "ask_user"
    }
    fn description(&self) -> &'static str {
        "Задать вопрос пользователю mid-task. Доступен ТОЛЬКО главному агенту. \
         До 4 вариантов ответа (необязательно). Результат придёт как tool_result \
         с текстом ответа пользователя."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "Вопрос пользователю." },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "maxItems": 4,
                    "description": "Варианты ответа (опционально)."
                }
            },
            "required": ["prompt"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let prompt = input.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        if prompt.is_empty() {
            return ToolOutput::err("нужен 'prompt'");
        }
        let options: Vec<String> = input
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| o.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Регистрируем ожидание ДО отправки события (нет гонки с ответом).
        let request_id = Uuid::new_v4().to_string();
        let rx = ctx.pending_requests.register(request_id.clone());
        if ctx
            .event_tx
            .send(EventKind::UserInputRequest {
                request_id: request_id.clone(),
                agent_id: ctx.agent_id,
                prompt: prompt.to_string(),
                options: options.clone(),
            })
            .is_err()
        {
            ctx.pending_requests.cancel(&request_id);
            return ToolOutput::err("канал событий закрыт: некому ответить");
        }

        match tokio::time::timeout(ASK_USER_TIMEOUT, rx).await {
            Ok(Ok(Answer::Text(answer))) => {
                // Ответ-индекс резолвим в текст варианта (choice_index приходит строкой).
                if let Ok(i) = answer.trim().parse::<usize>() {
                    if let Some(opt) = options.get(i) {
                        return ToolOutput::ok(opt.clone());
                    }
                }
                ToolOutput::ok(answer)
            }
            Ok(Ok(Answer::Permission(_))) => {
                ToolOutput::err("неожиданный тип ответа (permission вместо текста)")
            }
            Ok(Err(_)) => ToolOutput::err("ответ не доставлен (отправитель отключён)"),
            Err(_) => {
                ctx.pending_requests.cancel(&request_id);
                ToolOutput::err("таймаут ответа пользователя (120с)")
            }
        }
    }
}
