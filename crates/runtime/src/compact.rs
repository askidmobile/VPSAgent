//! Компактификация контекста (FR-070): суммаризация старых сообщений при
//! приближении к лимиту окна модели.

use vpsagent_core::{ContentBlock, Message, TokenUsage};

/// Лимит токенов, при котором запускается компактификация.
pub const COMPACT_THRESHOLD: usize = 60_000;

/// Грубая оценка токенов (примерно 4 символа на токен).
pub fn estimate_tokens(blocks: &[ContentBlock]) -> usize {
    let chars: usize = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::Image { data_base64, .. } => data_base64.len() / 4, // base64 ≈ 0.75 байт/символ
            ContentBlock::ToolResult { content, .. } => content.len(),
            ContentBlock::ToolUse { input, .. } => input.to_string().len(),
        })
        .sum();
    chars / 4
}

/// Оценка токенов для истории Message (сумма по всем блокам).
pub fn estimate_tokens_messages(messages: &[Message]) -> usize {
    messages.iter().map(|m| estimate_tokens(&m.content)).sum()
}

/// Суммаризация истории в компактное резюме.
/// В Фазе 8 — эвристическая (без LLM): берёт первые/последние сообщения,
/// средние заменяет маркером. LLM-суммаризация — отдельная итерация.
/// Сохраняет парность tool_use/tool_result: режет по границам сообщений.
pub fn compact(messages: &[Message], budget: usize) -> Vec<Message> {
    let current = estimate_tokens_messages(messages);
    if current <= budget {
        return messages.to_vec();
    }
    // Оставляем первые 20% и последние 40%, середину сжимаем.
    let keep_head = messages.len() / 5;
    let keep_tail = messages.len() * 2 / 5;
    let head = &messages[..keep_head.min(messages.len())];
    let tail = &messages[messages.len().saturating_sub(keep_tail)..];
    let mut out = head.to_vec();
    // Маркер сжатия как user-сообщение (не ломает чередование ролей).
    out.push(vpsagent_core::Message {
        id: uuid::Uuid::new_v4(),
        session_id: messages.first().map(|m| m.session_id).unwrap_or(uuid::Uuid::nil()),
        agent_id: None,
        role: vpsagent_core::Role::User,
        content: vec![ContentBlock::Text {
            text: format!("[... {} сообщений сжато компактификацией ...]", messages.len() - head.len() - tail.len()),
        }],
        tokens: None,
        created_at: chrono::Utc::now(),
    });
    out.extend_from_slice(tail);
    out
}

/// Оценка токенов для статистики.
pub fn usage_from_blocks(blocks: &[ContentBlock]) -> TokenUsage {
    TokenUsage {
        input: estimate_tokens(blocks) as u32,
        output: 0,
    }
}

// Используем тип для полноты API.
#[allow(dead_code)]
fn _ensure(_: &TokenUsage) {}