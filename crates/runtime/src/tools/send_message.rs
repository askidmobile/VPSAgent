//! Инструмент send_message: агент отправляет сообщение другому агенту (мультипоток, FR-024).

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Tool, ToolContext, ToolOutput};

pub struct SendMessage;

#[async_trait]
impl Tool for SendMessage {
    fn name(&self) -> &'static str {
        "send_message"
    }
    fn description(&self) -> &'static str {
        "Отправить сообщение другому работающему агенту (субагенту). Полезно для \
         координации между агентами. Адресат — id агента из task_list."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent_id": { "type": "string", "description": "ID агента-получателя." },
                "message": { "type": "string", "description": "Текст сообщения." }
            },
            "required": ["agent_id", "message"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let agent_id = input.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let message = input.get("message").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() || message.is_empty() {
            return ToolOutput::err("нужны 'agent_id' и 'message'");
        }
        let Ok(id) = uuid::Uuid::parse_str(agent_id) else {
            return ToolOutput::err(format!("некорректный agent_id: {agent_id}"));
        };
        if ctx.subagents.send_message(id, message.to_string()).await {
            ToolOutput::ok(format!("сообщение отправлено агенту {id}"))
        } else {
            ToolOutput::err(format!("агент {id} не найден или недоступен"))
        }
    }
}
