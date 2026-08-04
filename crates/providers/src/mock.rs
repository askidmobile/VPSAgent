//! Mock-провайдер для тестирования (не тащит реальный LLM).
//!
//! Возвращает заранее заданные ответы: либо текст, либо tool-call.

use std::collections::VecDeque;
use std::sync::Arc;

use futures::Stream;
use serde_json::Value;
use vpsagent_core::Result;

use crate::{ChatRequest, Provider, StreamChunk};

#[derive(Clone)]
pub enum MockStep {
    Text(String),
    /// Reasoning/thinking-дельта (эмитится как StreamChunk::ThinkingDelta).
    Thinking(String),
    ToolCall {
        name: String,
        input: Value,
    },
    Done,
}

/// Mock-провайдер (для юнит и интеграционных тестов без сети).
///
/// Последовательность шагов хранится в общей очереди: каждый вызов stream()
/// продолжает с того места, где закончил предыдущий (M). Это позволяет
/// агентному циклу делать несколько витков (tool-call → результат → ответ)
/// с одной заданной последовательностью.
pub struct MockProvider {
    queue: Arc<tokio::sync::Mutex<VecDeque<MockStep>>>,
}

impl MockProvider {
    pub fn new(sequence: Vec<MockStep>) -> Self {
        Self {
            queue: Arc::new(tokio::sync::Mutex::new(sequence.into())),
        }
    }
}

impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    fn stream(
        &self,
        _req: ChatRequest,
    ) -> std::pin::Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>> {
        let queue = self.queue.clone();
        Box::pin(futures::stream::unfold(
            (queue, 0usize),
            |(queue, i)| async move {
                // Берём следующий шаг из общей очереди (pop_front — очередь продвигается).
                let step = {
                    let mut q = queue.lock().await;
                    q.pop_front()
                };
                let step = step?;
                let (chunk, done) = match step {
                    MockStep::Text(t) => (StreamChunk::TextDelta(t), false),
                    MockStep::Thinking(t) => (StreamChunk::ThinkingDelta(t), false),
                    MockStep::ToolCall { name, input } => (
                        StreamChunk::ToolCall {
                            id: format!("call-{i}"),
                            name,
                            input,
                        },
                        false,
                    ),
                    MockStep::Done => (StreamChunk::Done, true),
                };
                if done {
                    // Done — завершаем текущий стрим.
                    Some((Ok(chunk), (queue, usize::MAX)))
                } else {
                    Some((Ok(chunk), (queue, i.saturating_add(1))))
                }
            },
        ))
    }
}

/// Хелпер: конфиг, который просто отвечает текстом и завершается.
pub fn text_only(text: &str) -> Vec<MockStep> {
    vec![MockStep::Text(text.to_string()), MockStep::Done]
}

/// Хелпер: reasoning (thinking), затем текстовый ответ — воспроизводит поведение
/// reasoning-моделей (Kimi K2, DeepSeek-R1, Claude с thinking, GPT-5).
pub fn thinking_then_text(thinking: &str, text: &str) -> Vec<MockStep> {
    vec![
        MockStep::Thinking(thinking.to_string()),
        MockStep::Text(text.to_string()),
        MockStep::Done,
    ]
}

/// Конфиг: один tool-call, затем ответ текстом.
pub fn tool_then_text(tool: &str, input: Value, final_text: &str) -> Vec<MockStep> {
    vec![
        MockStep::ToolCall {
            name: tool.to_string(),
            input,
        },
        MockStep::Text(final_text.to_string()),
        MockStep::Done,
    ]
}
