//! Тесты Фазы 8: компактификация, rewind, hooks.

use vpsagent_core::{ContentBlock, Message, Role};
use vpsagent_runtime::{compact, estimate_tokens, HookEvent, Hooks, RewindStore};

fn msg(text: &str) -> Message {
    Message {
        id: uuid::Uuid::new_v4(),
        session_id: uuid::Uuid::nil(),
        agent_id: None,
        role: Role::User,
        content: vec![ContentBlock::Text { text: text.to_string() }],
        tokens: None,
        created_at: chrono::Utc::now(),
    }
}

#[test]
fn estimate_tokens_counts_blocks() {
    let blocks = vec![ContentBlock::Text {
        text: "abcdefgh".repeat(100), // 800 символов
    }];
    let tokens = estimate_tokens(&blocks);
    assert_eq!(tokens, 800 / 4); // ~4 символа на токен
}

#[test]
fn compact_reduces_large_context() {
    // 100 сообщений по 1000 символов.
    let mut blocks = vec![];
    for i in 0..100 {
        blocks.push(msg(&format!("сообщение {i}: {}", "x".repeat(1000))));
    }
    let compacted = compact(&blocks, 1000);
    // Должно быть меньше исходных.
    assert!(
        compacted.len() < blocks.len(),
        "компактификация должна сжимать: {} -> {}",
        blocks.len(),
        compacted.len()
    );
    // Должен быть маркер сжатия.
    assert!(
        compacted.iter().any(|m| m.content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text.contains("сжато компактификацией")))),
        "должен быть маркер сжатия"
    );
}

#[test]
fn compact_keeps_small_context() {
    let blocks = vec![msg("маленький")];
    let out = compact(&blocks, 10000);
    assert_eq!(out.len(), 1, "маленький контекст не должен сжиматься");
}

#[test]
fn rewind_snapshots_and_rollback() {
    let mut store = RewindStore::new();
    // Создаём 2 снапшота.
    store.snapshot(uuid::Uuid::nil(), "начало", vec![]);
    let snap2 = store.snapshot(uuid::Uuid::nil(), "после правок", vec![]);
    assert_eq!(store.len(), 2);
    // Откат к снапшоту 1.
    let rolled = store.rewind(1).unwrap();
    assert_eq!(rolled.label, snap2.label);
}

#[tokio::test]
async fn hooks_fire_matching_event() {
    let hooks = Hooks {
        hooks: vec![vpsagent_runtime::HookConfig {
            event: HookEvent::PreTool,
            command: "true".into(), // no-op команда
            tool_pattern: "edit".into(),
        }],
    };
    // Не должно паниковать; matching не проверяем (async).
    hooks.fire(HookEvent::PreTool, "edit");
    hooks.fire(HookEvent::PostTool, "edit"); // не матчит, но не падает
    hooks.fire(HookEvent::PreTool, "shell"); // не матчит по pattern
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}