//! OpenAI-compatible провайдер: chat/completions с SSE-стримингом и tool-calling.
//!
//! Работает с OpenAI, OpenRouter, OmniRoute, vLLM, llama.cpp — любой endpoint,
//! поддерживающий `POST /chat/completions` с `stream: true`.

use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};
use vpsagent_core::{ContentBlock, Error, Result, Role, TokenUsage};

use crate::{backoff_delay, ChatRequest, Provider, StreamChunk};

/// OpenAI-compatible клиент.
#[derive(Clone)]
pub struct OpenaiCompat {
    name: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl OpenaiCompat {
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client"),
        }
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        // api_key уже без префикса; OpenAI ждёт "Bearer <key>", но OpenRouter/OmniRoute — тоже.
        let bearer = format!("Bearer {}", self.api_key);
        h.insert(AUTHORIZATION, HeaderValue::from_str(&bearer).unwrap());
        h
    }

    /// Сборка тела запроса chat/completions.
    /// История — Vec<Message> с ролями; маппим блоки по роли (C6).
    fn body(&self, req: &ChatRequest) -> Value {
        let mut messages = vec![json!({ "role": "system", "content": req.system })];
        for msg in &req.messages {
            match msg.role {
                Role::User => {
                    // User-сообщение: Text/Image-блоки.
                    let mut content: Vec<Value> = vec![];
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } => {
                                content.push(json!({ "type": "text", "text": text }));
                            }
                            ContentBlock::Image {
                                data_base64,
                                media_type,
                            } => {
                                content.push(json!({
                                    "type": "image_url",
                                    "image_url": { "url": format!("data:{media_type};base64,{data_base64}") }
                                }));
                            }
                            _ => {}
                        }
                    }
                    if content.is_empty() {
                        content.push(json!({ "type": "text", "text": "" }));
                    }
                    messages.push(json!({ "role": "user", "content": content }));
                }
                Role::Assistant => {
                    // Assistant: Text + ToolUse-блоки (tool_calls).
                    let mut text = String::new();
                    let mut tool_calls: Vec<Value> = vec![];
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text: t } => {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(t);
                            }
                            ContentBlock::ToolUse { id, name, input } => {
                                tool_calls.push(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": { "name": name, "arguments": input.to_string() }
                                }));
                            }
                            _ => {}
                        }
                    }
                    let mut m = json!({ "role": "assistant", "content": text });
                    if !tool_calls.is_empty() {
                        m["tool_calls"] = json!(tool_calls);
                    }
                    messages.push(m);
                }
                Role::Tool => {
                    // Tool-результаты (по одному сообщению на блок).
                    for block in &msg.content {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } = block
                        {
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": content
                            }));
                        }
                    }
                }
            }
        }
        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if !req.tools.is_empty() {
            body["tools"] = json!(req.tools);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        body
    }

    async fn send(&self, req: ChatRequest) -> Result<reqwest::Response> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = self.body(&req);
        let resp = self
            .client
            .post(&url)
            .headers(self.headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("запрос: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Provider(format!("HTTP {status}: {text}")));
        }
        Ok(resp)
    }
}

impl Provider for OpenaiCompat {
    fn name(&self) -> &str {
        &self.name
    }

    fn stream(
        &self,
        req: ChatRequest,
    ) -> std::pin::Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>> {
        let this = self.clone();
        Box::pin(async_stream::try_stream! {
            let mut attempt = 0u32;
            const MAX_ATTEMPTS: u32 = 4;
            loop {
                let resp = match this.send(req.clone()).await {
                    Ok(r) => r,
                    Err(e) => {
                        if attempt + 1 >= MAX_ATTEMPTS {
                            Err(e)?;
                            unreachable!();
                        }
                        attempt += 1;
                        let d = backoff_delay(attempt);
                        tokio::time::sleep(d).await;
                        continue;
                    }
                };
                let mut stream = resp.bytes_stream().eventsource();
                // Флаг: уже отдали хоть один чанк потребителю.
                // C7: ретрай только ДО первого yield — иначе повторная отправка
                // запроса продублирует TextDelta/ToolCall и shell исполнится дважды.
                let mut yielded_any = false;
                // C5: аккумулятор tool_calls по index — OpenAI шлёт id/name
                // только в первом чанке, arguments дополняется последующими.
                let mut tool_acc: std::collections::HashMap<usize, (String, String, String)> =
                    std::collections::HashMap::new();
                while let Some(item) = stream.next().await {
                    let event = match item {
                        Ok(e) => e,
                        Err(e) => {
                            // Сетевая ошибка стрима.
                            if yielded_any {
                                // Уже отдали часть ответа — ретрай запрещён (C7):
                                // иначе контент/инструменты продублируются.
                                // Завершаем стрим, чтобы агент не завис.
                                tracing::warn!(error=%e, "SSE ошибка после yield, завершаем стрим без ретрая");
                                yield StreamChunk::Done;
                                return;
                            }
                            tracing::warn!(error=%e, "SSE ошибка до первого чанка, ретрай");
                            if attempt + 1 >= MAX_ATTEMPTS {
                                Err(Error::Provider(format!("SSE: {e}")))?;
                                unreachable!();
                            }
                            attempt += 1;
                            tokio::time::sleep(backoff_delay(attempt)).await;
                            break; // внешний loop пересоздаст стрим
                        }
                    };
                    if event.data == "[DONE]" {
                        // Эмитим накопленные tool_calls (по порядку index), затем Done.
                        let mut calls: Vec<(usize, (String, String, String))> =
                            tool_acc.drain().collect();
                        calls.sort_by_key(|(i, _)| *i);
                        for (_, (id, name, args)) in calls {
                            let input: Value = serde_json::from_str(&args).unwrap_or(Value::Null);
                            yield StreamChunk::ToolCall { id, name, input };
                        }
                        yield StreamChunk::Done;
                        return;
                    }
                    let chunk: StreamResponse = match serde_json::from_str(&event.data) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(data=%event.data, err=%e, "битый SSE-чанк, пропуск");
                            continue;
                        }
                    };
                    let mut finish = false;
                    for choice in chunk.choices {
                        if choice.finish_reason.is_some() {
                            finish = true;
                        }
                        let delta = match choice.delta {
                            Some(d) => d,
                            None => continue,
                        };
                        // reasoning_content (Kimi K2, DeepSeek-R1) / thinking (Qwen3)
                        // идёт ПЕРЕД content: пока модель размышляет, content == None.
                        // Без этого reasoning-чанки молча пропускались — агент завершался
                        // без единого TextDelta (симптом «привет → агент завершил, но
                        // ответа нет»). yield ThinkingDelta до content и поднимаем
                        // yielded_any, чтобы ретрай-логика (C7) не перезапустила запрос
                        // и не продублировала reasoning.
                        if let Some(r) = delta.reasoning_content {
                            if !r.is_empty() {
                                yielded_any = true;
                                yield StreamChunk::ThinkingDelta(r);
                            }
                        }
                        if let Some(text) = delta.content {
                            if !text.is_empty() {
                                yielded_any = true;
                                yield StreamChunk::TextDelta(text);
                            }
                        }
                        if let Some(tool_calls) = delta.tool_calls {
                            for tc in tool_calls {
                                // index определяет слот вызова; без index — 0 (одиночный вызов).
                                let idx = tc.index.unwrap_or(0);
                                let entry = tool_acc.entry(idx).or_insert_with(|| {
                                    (String::new(), String::new(), String::new())
                                });
                                if let Some(id) = tc.id {
                                    entry.0 = id;
                                }
                                if let Some(function) = tc.function {
                                    if let Some(name) = function.name {
                                        entry.1 = name;
                                    }
                                    // arguments приходят чанками — дополняем буфер.
                                    entry.2.push_str(&function.arguments);
                                }
                            }
                        }
                    }
                    if let Some(usage) = chunk.usage {
                        yielded_any = true;
                        yield StreamChunk::Usage(TokenUsage {
                            input: usage.prompt_tokens,
                            output: usage.completion_tokens,
                        });
                    }
                    if finish {
                        // finish_reason (например "tool_calls") — сливаем аккумулятор.
                        let mut calls: Vec<(usize, (String, String, String))> =
                            tool_acc.drain().collect();
                        calls.sort_by_key(|(i, _)| *i);
                        for (_, (id, name, args)) in calls {
                            let input: Value = serde_json::from_str(&args).unwrap_or(Value::Null);
                            yield StreamChunk::ToolCall { id, name, input };
                        }
                    }
                }
                // Стрим завершился без [DONE] — сливаем остаток аккумулятора.
                let mut calls: Vec<(usize, (String, String, String))> = tool_acc.drain().collect();
                calls.sort_by_key(|(i, _)| *i);
                for (_, (id, name, args)) in calls {
                    let input: Value = serde_json::from_str(&args).unwrap_or(Value::Null);
                    yield StreamChunk::ToolCall { id, name, input };
                }
                yield StreamChunk::Done;
                return;
            }
        })
    }
}

// --- SSE-схемы ---

#[derive(Debug, Deserialize)]
struct StreamResponse {
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    delta: Option<Delta>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Delta {
    content: Option<String>,
    /// Reasoning/thinking-контент: Kimi K2/DeepSeek-R1 шлют `reasoning_content`,
    /// Qwen3 — `thinking`. alias покрывает оба имени; `default` — поле опционально
    /// (у обычных OpenAI-моделей его нет).
    #[serde(default, alias = "thinking")]
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct ToolCallDelta {
    // Слот вызова: OpenAI группирует чанки tool_call по index (C5).
    index: Option<usize>,
    id: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct FunctionDelta {
    // name приходит только в первом чанке вызова.
    name: Option<String>,
    #[serde(default)]
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

// async-stream для try_stream! — нужен в deps.
// (через explicit re-export для избежания лишних public-crate-зависимостей в фазе 1)

#[cfg(test)]
mod tests {
    use super::*;

    /// Kimi K2 / DeepSeek-R1 шлют reasoning_content отдельным полем, ПЕРЕД
    /// content. Без этого поля reasoning-токены молча теряются при десериализации
    /// (симптом «привет → агент завершил, но ответа нет»).
    #[test]
    fn delta_parses_reasoning_content() {
        let raw = r#"{"reasoning_content":"думаю над задачей"}"#;
        let d: Delta = serde_json::from_str(raw).expect("reasoning_content должен парситься");
        assert_eq!(d.reasoning_content.as_deref(), Some("думаю над задачей"));
        assert!(
            d.content.is_none(),
            "content не должно быть при чистом reasoning"
        );
        assert!(d.tool_calls.is_none());
    }

    /// Qwen3 использует имя `thinking` вместо `reasoning_content` — alias
    /// покрывает оба имени.
    #[test]
    fn delta_parses_thinking_alias() {
        let raw = r#"{"thinking":"размышляю шаг за шагом"}"#;
        let d: Delta = serde_json::from_str(raw).expect("thinking-alias должен парситься");
        assert_eq!(
            d.reasoning_content.as_deref(),
            Some("размышляю шаг за шагом")
        );
    }

    /// Обычная OpenAI-модель не шлёт reasoning_content — поле опционально,
    /// дельта с одним content десериализуется без ошибок.
    #[test]
    fn delta_without_reasoning_still_parses() {
        let raw = r#"{"content":"ответ"}"#;
        let d: Delta = serde_json::from_str(raw).expect("content-only должен парситься");
        assert_eq!(d.content.as_deref(), Some("ответ"));
        assert!(
            d.reasoning_content.is_none(),
            "reasoning_content должно быть None"
        );
    }

    /// Reasoning и content могут идти вместе в одном чанке (граница фаз).
    #[test]
    fn delta_with_both_reasoning_and_content() {
        let raw = r#"{"reasoning_content":"финал размышлений","content":"ответ"}"#;
        let d: Delta = serde_json::from_str(raw).expect("оба поля должны парситься вместе");
        assert_eq!(d.reasoning_content.as_deref(), Some("финал размышлений"));
        assert_eq!(d.content.as_deref(), Some("ответ"));
    }
}
