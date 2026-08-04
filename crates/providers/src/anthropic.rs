//! Anthropic Messages API провайдер: SSE-стриминг, tool_use блоки.

use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};
use vpsagent_core::{ContentBlock, Error, Result, Role, TokenUsage};

use crate::{backoff_delay, ChatRequest, Provider, StreamChunk};

/// Anthropic Messages API клиент.
#[derive(Clone)]
pub struct Anthropic {
    name: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl Anthropic {
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
        h.insert("x-api-key", HeaderValue::from_str(&self.api_key).unwrap());
        h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        h
    }

    /// Сборка тела запроса Messages API.
    /// Сборка тела запроса Messages API.
    /// История — Vec<Message> с ролями; маппим блоки по роли (C6).
    /// ToolResult-блоки одного tool-шага объединяем в один user (требование Anthropic).
    fn body(&self, req: &ChatRequest) -> Value {
        let mut messages: Vec<Value> = vec![];
        for msg in &req.messages {
            match msg.role {
                Role::User => {
                    // User: Text/Image.
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
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": media_type,
                                        "data": data_base64
                                    }
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
                    // Assistant: Text + tool_use.
                    let mut content: Vec<Value> = vec![];
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } => {
                                content.push(json!({ "type": "text", "text": text }));
                            }
                            ContentBlock::ToolUse { id, name, input } => {
                                content.push(json!({
                                    "type": "tool_use",
                                    "id": id,
                                    "name": name,
                                    "input": input
                                }));
                            }
                            _ => {}
                        }
                    }
                    messages.push(json!({ "role": "assistant", "content": content }));
                }
                Role::Tool => {
                    // Tool-результаты: ОДИН user с массивом tool_result (требование API).
                    let mut tool_results: Vec<Value> = vec![];
                    for block in &msg.content {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } = block
                        {
                            let mut b = json!({
                                "type": "tool_result",
                                "tool_use_id": tool_use_id,
                                "content": content,
                            });
                            if *is_error {
                                b["is_error"] = json!(true);
                            }
                            tool_results.push(b);
                        }
                    }
                    if !tool_results.is_empty() {
                        messages.push(json!({ "role": "user", "content": tool_results }));
                    }
                }
            }
        }

        // Инструменты в формате Anthropic.
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                // t — OpenAI-формат {type:function, function:{name,description,parameters}}.
                let function = &t["function"];
                json!({
                    "name": function["name"],
                    "description": function["description"],
                    "input_schema": function["parameters"],
                })
            })
            .collect();

        let mut body = json!({
            "model": req.model,
            // Лимит выходных токенов (обязателен в Messages API).
            // TODO: вынести в конфиг endpoint'а.
            "max_tokens": 4096,
            "system": req.system,
            "messages": messages,
            "stream": true,
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        body
    }

    async fn send(&self, req: ChatRequest) -> Result<reqwest::Response> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
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

impl Provider for Anthropic {
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
                        tokio::time::sleep(backoff_delay(attempt)).await;
                        continue;
                    }
                };
                let mut stream = resp.bytes_stream().eventsource();
                // C7: ретрай только до первого yield — иначе уже отданные
                // TextDelta/ToolCall придут повторно и инструмент исполнится дважды.
                let mut yielded_any = false;
                let mut current_tool_id: Option<String> = None;
                let mut current_tool_name = String::new();
                let mut current_tool_input = String::new();
                let mut usage = TokenUsage::default();
                let mut done = false;
                while let Some(item) = stream.next().await {
                    let event = match item {
                        Ok(e) => e,
                        Err(e) => {
                            if yielded_any {
                                // Уже отдали часть ответа — ретрай запрещён (C7).
                                // Завершаем стрим, чтобы агент не завис.
                                tracing::warn!(error=%e, "SSE ошибка Anthropic после yield, завершаем стрим");
                                yield StreamChunk::Done;
                                return;
                            }
                            tracing::warn!(error=%e, "SSE ошибка Anthropic до первого чанка, ретрай");
                            if attempt + 1 >= MAX_ATTEMPTS {
                                Err(Error::Provider(format!("SSE: {e}")))?;
                                unreachable!();
                            }
                            attempt += 1;
                            tokio::time::sleep(backoff_delay(attempt)).await;
                            break;
                        }
                    };
                    // Парсим события Anthropic.
                    let parsed: AnthropicEvent = match serde_json::from_str(&event.data) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(data=%event.data, err=%e, "битый SSE-чанк Anthropic");
                            continue;
                        }
                    };
                    match parsed {
                        AnthropicEvent::MessageStart { message: Some(m) } => {
                            // C9: input_tokens приходят один раз в message_start.
                            if let Some(u) = m.usage {
                                usage.input = u.input_tokens.unwrap_or(usage.input);
                                usage.output = u.output_tokens.unwrap_or(usage.output);
                            }
                        }
                        AnthropicEvent::ContentBlockDelta { delta } => {
                            match delta {
                                AnthropicDelta::TextDelta { text } => {
                                    yielded_any = true;
                                    yield StreamChunk::TextDelta(text);
                                }
                                AnthropicDelta::InputJsonDelta { partial_json } => {
                                    current_tool_input.push_str(&partial_json);
                                }
                                // Extended thinking: эмитим как ThinkingDelta
                                // (display-only; в историю не сохраняется).
                                // Поднимаем yielded_any — чтобы ретрай (C7) не
                                // перезапустил запрос и не продублировал thinking.
                                AnthropicDelta::ThinkingDelta { thinking } => {
                                    yielded_any = true;
                                    yield StreamChunk::ThinkingDelta(thinking);
                                }
                                // signature нужен для multi-turn roundtrip (future),
                                // здесь только парсим — не эмитим.
                                AnthropicDelta::SignatureDelta { .. } | AnthropicDelta::Other => {}
                            }
                        }
                        AnthropicEvent::ContentBlockStart {
                            block: AnthropicContentBlock::ToolUse { id, name, .. },
                        } => {
                            current_tool_id = Some(id);
                            current_tool_name = name;
                            current_tool_input.clear();
                        }
                        AnthropicEvent::ContentBlockStop {} => {
                            if let Some(id) = current_tool_id.take() {
                                let input: Value = serde_json::from_str(&current_tool_input)
                                    .unwrap_or(Value::Null);
                                yielded_any = true;
                                yield StreamChunk::ToolCall {
                                    id,
                                    name: current_tool_name.clone(),
                                    input,
                                };
                                current_tool_input.clear();
                            }
                        }
                        AnthropicEvent::MessageDelta { usage: Some(u) } => {
                            // C9: message_delta шлёт КУМУЛЯТИВНЫЕ счётчики —
                            // присваиваем, а не += (иначе квадратичный рост).
                            usage.input = u.input_tokens.unwrap_or(usage.input);
                            usage.output = u.output_tokens.unwrap_or(usage.output);
                            yielded_any = true;
                            yield StreamChunk::Usage(usage);
                        }
                        AnthropicEvent::MessageStop {} => {
                            done = true;
                            break;
                        }
                        AnthropicEvent::Error { error } => {
                            Err(Error::Provider(format!("anthropic: {error}")))?;
                            unreachable!();
                        }
                        _ => {}
                    }
                }
                if done {
                    yield StreamChunk::Done;
                    return;
                }
                return;
            }
        })
    }
}

// --- SSE-схемы Anthropic ---

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicEvent {
    #[serde(rename = "message_start")]
    MessageStart {
        // message_start несёт message.usage.input_tokens/output_tokens (C9).
        message: Option<MessageStartBody>,
    },
    #[serde(rename = "content_block_start")]
    ContentBlockStart { block: AnthropicContentBlock },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { delta: AnthropicDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop {},
    #[serde(rename = "message_delta")]
    MessageDelta { usage: Option<AnthropicUsage> },
    #[serde(rename = "message_stop")]
    MessageStop {},
    #[serde(rename = "error")]
    Error { error: String },
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text {
        // Поле десериализуется из API Anthropic, но не читается в коде
        // (текстовые блоки в ContentBlockStart нам не нужны).
        #[allow(dead_code)]
        text: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        // Поле десериализуется, но парсится из отдельного InputJsonDelta-стрима.
        #[allow(dead_code)]
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
    /// Extended thinking: инкрементальные токены размышления (до content).
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    /// Подпись thinking-блока (нужна для multi-turn roundtrip — future;
    /// здесь только парсим, не эмитим).
    #[serde(rename = "signature_delta")]
    SignatureDelta {
        #[allow(dead_code)]
        signature: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct MessageStartBody {
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anthropic extended thinking: `thinking_delta` несёт поле `thinking` с
    /// инкрементальными токенами размышления. До фикса это был unit-вариант
    /// без поля — данные терялись при десериализации.
    #[test]
    fn anthropic_delta_thinking_has_field() {
        let raw = r#"{"type":"thinking_delta","thinking":"шаг рассуждения"}"#;
        let d: AnthropicDelta =
            serde_json::from_str(raw).expect("thinking_delta с полем thinking должен парситься");
        assert!(
            matches!(d, AnthropicDelta::ThinkingDelta { ref thinking } if thinking == "шаг рассуждения"),
            "ожидал ThinkingDelta {{ thinking: \"шаг рассуждения\" }}, got {d:?}"
        );
    }

    /// `signature_delta` парсится с полем signature (для future multi-turn
    /// roundtrip); пока не эмитится, но должен десериализоваться без ошибок.
    #[test]
    fn anthropic_delta_signature_has_field() {
        let raw = r#"{"type":"signature_delta","signature":"base64sig"}"#;
        let d: AnthropicDelta = serde_json::from_str(raw)
            .expect("signature_delta с полем signature должен парситься");
        assert!(
            matches!(d, AnthropicDelta::SignatureDelta { ref signature } if signature == "base64sig"),
            "ожидал SignatureDelta с signature, got {d:?}"
        );
    }

    /// Обычный text_delta по-прежнему парсится (регрессия на правку enum).
    #[test]
    fn anthropic_delta_text_still_parses() {
        let raw = r#"{"type":"text_delta","text":"ответ"}"#;
        let d: AnthropicDelta =
            serde_json::from_str(raw).expect("text_delta должен парситься");
        assert!(matches!(d, AnthropicDelta::TextDelta { ref text } if text == "ответ"));
    }
}
