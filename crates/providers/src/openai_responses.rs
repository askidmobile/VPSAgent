//! OpenAI Responses API провайдер (native GPT): POST /v1/responses со SSE-стримингом.
//!
//! Отличается от Chat Completions форматом ответа (responses, output array,
//! response.created) и tool-вызовами (function_call блоки). Покрывает нативные
//! модели GPT-5/4.1 через Responses API.

use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};
use vpsagent_core::{ContentBlock, Error, Result, Role, TokenUsage};

use crate::{backoff_delay, ChatRequest, Provider, StreamChunk};

/// OpenAI Responses API клиент.
#[derive(Clone)]
pub struct OpenaiResponses {
    name: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl OpenaiResponses {
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
        let bearer = format!("Bearer {}", self.api_key);
        h.insert(AUTHORIZATION, HeaderValue::from_str(&bearer).unwrap());
        h
    }

    /// Сборка тела Responses API.
    /// История — Vec<Message> с ролями; маппим блоки по роли (C6).
    fn body(&self, req: &ChatRequest) -> Value {
        let mut input: Vec<Value> = vec![];
        for msg in &req.messages {
            match msg.role {
                Role::User => {
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } => {
                                input.push(json!({ "role": "user", "content": text }));
                            }
                            ContentBlock::Image {
                                data_base64,
                                media_type,
                            } => {
                                input.push(json!({
                                    "role": "user",
                                    "content": [{
                                        "type": "input_image",
                                        "image_url": format!("data:{media_type};base64,{data_base64}")
                                    }]
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                Role::Assistant => {
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } => {
                                input.push(json!({ "role": "assistant", "content": text }));
                            }
                            ContentBlock::ToolUse {
                                id,
                                name,
                                input: ti,
                            } => {
                                input.push(json!({
                                    "role": "assistant",
                                    "content": [{
                                        "type": "function_call",
                                        "id": id,
                                        "call_id": id,
                                        "name": name,
                                        "arguments": ti.to_string(),
                                    }]
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                Role::Tool => {
                    for block in &msg.content {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } = block
                        {
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": content,
                            }));
                        }
                    }
                }
            }
        }
        let mut body = json!({
            "model": req.model,
            "input": input,
            "stream": true,
            "store": false,
            // Лимит выходных токенов (без него Responses API может обрезать ответ
            // дефолтом endpoint'а). TODO: вынести в конфиг endpoint'а.
            "max_output_tokens": 4096,
        });
        if !req.system.is_empty() {
            body["instructions"] = json!(req.system);
        }
        // Инструменты в Responses-формате.
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let f = &t["function"];
                json!({
                    "type": "function",
                    "name": f["name"],
                    "description": f["description"],
                    "parameters": f["parameters"],
                })
            })
            .collect();
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        body
    }

    async fn send(&self, req: ChatRequest) -> Result<reqwest::Response> {
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
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

impl Provider for OpenaiResponses {
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
                // C7: ретрай только до первого yield — иначе продублируем
                // TextDelta/ToolCall и инструмент исполнится дважды.
                let mut yielded_any = false;
                // C8: буфер аргументов по item_id — delta несёт item_id,
                // вызовов может быть несколько параллельно.
                let mut args_by_item: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                let mut done = false;
                while let Some(item) = stream.next().await {
                    let event = match item {
                        Ok(e) => e,
                        Err(e) => {
                            if yielded_any {
                                // Уже отдали часть ответа — ретрай запрещён (C7).
                                // Завершаем стрим, чтобы агент не завис.
                                tracing::warn!(error=%e, "SSE ошибка Responses после yield, завершаем стрим");
                                yield StreamChunk::Done;
                                return;
                            }
                            tracing::warn!(error=%e, "SSE ошибка Responses до первого чанка, ретрай");
                            if attempt + 1 >= MAX_ATTEMPTS {
                                Err(Error::Provider(format!("SSE: {e}")))?;
                                unreachable!();
                            }
                            attempt += 1;
                            tokio::time::sleep(backoff_delay(attempt)).await;
                            break;
                        }
                    };
                    let parsed: ResponsesEvent = match serde_json::from_str(&event.data) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(data=%event.data, err=%e, "битый SSE-чанк Responses");
                            continue;
                        }
                    };
                    match parsed {
                        ResponsesEvent::ResponseOutputTextDelta { delta } => {
                            yielded_any = true;
                            yield StreamChunk::TextDelta(delta);
                        }
                        ResponsesEvent::ResponseFunctionCallArgumentsDelta { item_id, delta } => {
                            // Копим delta-чанки аргументов по item_id (C8).
                            if let Some(id) = item_id {
                                args_by_item.entry(id).or_default().push_str(&delta);
                            }
                        }
                        ResponsesEvent::ResponseOutputItemDone { item } => {
                            // Завершён function_call → выдаём ToolCall.
                            if let Some(ItemDone::FunctionCall { id, call_id, name, arguments }) = item {
                                // C8: output_item.done несёт ПОЛНЫЕ arguments —
                                // берём как есть, НЕ конкатенируем с delta-буфером.
                                // Fallback на буфер — если arguments пусты.
                                let args = if arguments.is_empty() {
                                    args_by_item.remove(&id).unwrap_or_default()
                                } else {
                                    args_by_item.remove(&id);
                                    arguments
                                };
                                let input: Value = serde_json::from_str(&args).unwrap_or(Value::Null);
                                // call_id — идентификатор для function_call_output; id — fallback.
                                yielded_any = true;
                                yield StreamChunk::ToolCall {
                                    id: call_id.unwrap_or(id),
                                    name,
                                    input,
                                };
                            }
                        }
                        ResponsesEvent::ResponseCompleted { response } => {
                            // Usage из финального события (M).
                            if let Some(r) = response {
                                if let Some(u) = r.usage {
                                    yielded_any = true;
                                    yield StreamChunk::Usage(TokenUsage {
                                        input: u.input_tokens.unwrap_or(0),
                                        output: u.output_tokens.unwrap_or(0),
                                    });
                                }
                            }
                            done = true;
                            break;
                        }
                        ResponsesEvent::Error { code, message } => {
                            Err(Error::Provider(format!("responses error {code}: {message}")))?;
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

// --- SSE-схемы Responses API ---

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponsesEvent {
    #[serde(rename = "response.output_text.delta")]
    ResponseOutputTextDelta { delta: String },
    #[serde(rename = "response.function_call_arguments.delta")]
    ResponseFunctionCallArgumentsDelta {
        // item_id связывает delta-чанки с конкретным function_call (C8).
        item_id: Option<String>,
        delta: String,
    },
    #[serde(rename = "response.output_item.done")]
    ResponseOutputItemDone { item: Option<ItemDone> },
    #[serde(rename = "response.completed")]
    ResponseCompleted { response: Option<CompletedResponse> },
    #[serde(rename = "error")]
    Error { code: String, message: String },
    #[serde(rename = "response.created")]
    ResponseCreated,
    #[serde(rename = "response.output_item.added")]
    ResponseOutputItemAdded,
    #[serde(rename = "response.content_part.added")]
    ContentPartAdded,
    #[serde(rename = "response.content_part.done")]
    ContentPartDone,
    #[serde(rename = "response.output_text.done")]
    OutputTextDone,
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgsDone,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ItemDone {
    #[serde(rename = "function_call")]
    FunctionCall {
        id: String,
        // call_id — то, что кладём в function_call_output.call_id при ответе.
        call_id: Option<String>,
        name: String,
        #[serde(default)]
        arguments: String,
    },
    #[serde(rename = "message")]
    Message,
    #[serde(other)]
    Other,
}

/// Объект response из события response.completed (M: usage).
#[derive(Debug, Deserialize)]
struct CompletedResponse {
    usage: Option<ResponsesUsage>,
}

#[derive(Debug, Deserialize)]
struct ResponsesUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}
