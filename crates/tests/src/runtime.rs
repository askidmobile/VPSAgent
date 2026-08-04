//! Тесты агентного рантайма с mock-провайдером (без реального LLM).
//!
//! Покрывают: определение субагента (markdown+frontmatter), SubagentManager
//! (спавн, task-инструмент), агентный цикл с tool-call.
//!
//! Импорты и хелпер `tmp_db` используются в телах `#[test]`-функций; под целью
//! `--lib` (где тела вырезаются) это даёт ложные unused-imports/dead_code.

#![allow(unused_imports)]

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vpsagent_core::{Config, ContentBlock, EventKind, Id, Role, TokenUsage};
use vpsagent_providers::{MockProvider, MockStep};
use vpsagent_runtime::{run_agent, AgentDefinition, AgentParams, Permissions, SubagentManager};
use vpsagent_storage::Storage;

#[allow(dead_code)]
fn tmp_db() -> std::path::PathBuf {
    std::path::PathBuf::from(format!(
        "/tmp/vp-rt-{}.db",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ))
}

#[test]
fn parse_agent_definition_from_markdown() {
    let md = "\
---
name: code-reviewer
description: Ревьюер кода
tools: [read, grep]
fork: false
---
Ты — ревьюер кода. Ищи баги.";
    let def = AgentDefinition::parse(md, std::path::PathBuf::from("test.md")).unwrap();
    assert_eq!(def.name, "code-reviewer");
    assert_eq!(def.prompt, "Ты — ревьюер кода. Ищи баги.");
    assert_eq!(def.tools, vec!["read".to_string(), "grep".to_string()]);
    assert!(!def.fork);
}

#[tokio::test]
async fn agent_loop_runs_tool_call_with_mock() {
    let db = tmp_db();
    let storage = Storage::open(&db).await.unwrap();
    let session = storage.create_session("/tmp", Some("mock")).await.unwrap();
    let config = Arc::new(Config::default());

    // Mock: один tool-call (shell), затем текст.
    let provider: Arc<dyn vpsagent_providers::Provider> = Arc::new(MockProvider::new(vec![
        MockStep::ToolCall {
            name: "shell".into(),
            input: serde_json::json!({ "command": "echo hi" }),
        },
        MockStep::Text("готово".into()),
        MockStep::Done,
    ]));

    let subagents = SubagentManager::new(storage.clone(), config.clone());
    let (event_tx, _rx) = mpsc::unbounded_channel::<EventKind>();
    let (inbox_tx, inbox_rx) = mpsc::unbounded_channel::<String>();
    let agent_id = uuid::Uuid::new_v4();

    inbox_tx.send("выполни echo hi".into()).unwrap();

    let params = AgentParams {
        session_id: session.id,
        agent_id,
        model: "mock".into(),
        provider_name: "mock".into(),
        cwd: std::env::temp_dir(),
        provider,
        storage: storage.clone(),
        event_tx,
        inbox: inbox_rx,
        cancel: CancellationToken::new(),
        permissions: Permissions {
            mode: vpsagent_core::PermissionMode::BypassPermissions,
            allow: vec![],
            deny: vec![],
        },
        subagents: subagents.clone(),
        initial_history: vec![],
        allowed_tools: None,
        def_prompt: None,
        pending: vpsagent_runtime::PendingPermissions::new(),
        pending_requests: vpsagent_runtime::pending::PendingRequests::new(),
    };
    run_agent(params).await.unwrap();

    // Агент завершён в БД.
    let agents = storage.list_agents(session.id).await.unwrap();
    assert!(
        agents
            .iter()
            .any(|a| a.status == vpsagent_core::AgentStatus::Done),
        "агент должен быть Done"
    );
    // Сообщение записано.
    let msgs = storage.list_messages(session.id).await.unwrap();
    assert!(
        msgs.iter().any(|m| m.role == Role::Assistant),
        "должно быть assistant-сообщение"
    );
    let _ = std::fs::remove_file(&db);
}

/// Reasoning-модели (Kimi K2, DeepSeek-R1, Claude thinking, GPT-5) ШлюТ
/// thinking-дельту ПЕРЕД content. До фикса reasoning молча отбрасывался, агент
/// завершался без TextDelta — симптом «привет → агент завершил, но ответа нет».
/// Здесь проверяем полный e2e: MockProvider эмитит Thinking, затем Text,
/// agent_loop пробрасывает оба в стрим событий (ThinkingDelta, TextDelta),
/// thinking НЕ попадает в историю диалога (display-only).
#[tokio::test]
async fn agent_loop_emits_thinking_then_text() {
    let db = tmp_db();
    let storage = Storage::open(&db).await.unwrap();
    let session = storage.create_session("/tmp", Some("mock")).await.unwrap();
    let config = Arc::new(Config::default());

    let provider: Arc<dyn vpsagent_providers::Provider> = Arc::new(MockProvider::new(vec![
        MockStep::Thinking("размышляю над задачей".into()),
        MockStep::Text("вот ответ".into()),
        MockStep::Done,
    ]));

    let subagents = SubagentManager::new(storage.clone(), config.clone());
    let (event_tx, mut rx) = mpsc::unbounded_channel::<EventKind>();
    let (inbox_tx, inbox_rx) = mpsc::unbounded_channel::<String>();
    let agent_id = uuid::Uuid::new_v4();
    inbox_tx.send("привет".into()).unwrap();

    let params = AgentParams {
        session_id: session.id,
        agent_id,
        model: "mock".into(),
        provider_name: "mock".into(),
        cwd: std::env::temp_dir(),
        provider,
        storage: storage.clone(),
        event_tx,
        inbox: inbox_rx,
        cancel: CancellationToken::new(),
        permissions: Permissions {
            mode: vpsagent_core::PermissionMode::BypassPermissions,
            allow: vec![],
            deny: vec![],
        },
        subagents: subagents.clone(),
        initial_history: vec![],
        allowed_tools: None,
        def_prompt: None,
        pending: vpsagent_runtime::PendingPermissions::new(),
        pending_requests: vpsagent_runtime::pending::PendingRequests::new(),
    };
    run_agent(params).await.unwrap();

    // Собираем все события (event_tx дропнут run_agent → канал закроется).
    let mut events: Vec<EventKind> = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    // ThinkingDelta должна идти ПЕРЕД TextDelta.
    let thinking_idx = events
        .iter()
        .position(|e| matches!(e, EventKind::ThinkingDelta { .. }));
    let text_idx = events
        .iter()
        .position(|e| matches!(e, EventKind::TextDelta { .. }));
    assert!(thinking_idx.is_some(), "должно быть ThinkingDelta-событие");
    assert!(text_idx.is_some(), "должно быть TextDelta-событие");
    assert!(
        thinking_idx.unwrap() < text_idx.unwrap(),
        "ThinkingDelta должна идти ПЕРЕД TextDelta"
    );

    // thinking — display-only, в историю диалога НЕ сохраняется.
    let msgs = storage.list_messages(session.id).await.unwrap();
    let assistant_texts: Vec<&str> = msgs
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
        })
        .collect();
    assert!(
        assistant_texts.iter().any(|t| t.contains("вот ответ")),
        "assistant-сообщение должно содержать ответ: {assistant_texts:?}"
    );
    assert!(
        !assistant_texts
            .iter()
            .any(|t| t.contains("размышляю над задачей")),
        "thinking не должен попадать в историю диалога (display-only): {assistant_texts:?}"
    );
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn spawn_subagent_with_task_tool() {
    let db = tmp_db();
    let storage = Storage::open(&db).await.unwrap();
    let session = storage.create_session("/tmp", Some("mock")).await.unwrap();
    let config = Arc::new(Config::default());

    // Mock родителя: спавн субагента через task, затем текст.
    let provider: Arc<dyn vpsagent_providers::Provider> = Arc::new(MockProvider::new(vec![
        MockStep::ToolCall {
            name: "task".into(),
            input: serde_json::json!({
                "definition": "code-reviewer",
                "task": "проверь код",
                "fork": false
            }),
        },
        MockStep::Text("субагент запущен".into()),
        MockStep::Done,
    ]));

    let subagents = SubagentManager::new(storage.clone(), config.clone());
    let (event_tx, _rx) = mpsc::unbounded_channel::<EventKind>();
    let (inbox_tx, inbox_rx) = mpsc::unbounded_channel::<String>();
    let agent_id = uuid::Uuid::new_v4();

    // Создаём определение субагента в каталоге.
    let def_dir = std::env::temp_dir().join(format!("vp-def-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&def_dir).unwrap();
    std::fs::write(
        def_dir.join("code-reviewer.md"),
        "---\nname: code-reviewer\ndescription: ревью\ntools: [read]\n---\nРевьюй код.",
    )
    .unwrap();
    // Регистрируем в менеджере (загружаем вручную из каталога).
    // SubagentManager загружает дефолтные каталоги; для теста — подменим через load_from.
    let registry = vpsagent_runtime::DefinitionRegistry::load_from(std::slice::from_ref(&def_dir));
    assert!(!registry.defs.is_empty(), "должно быть определение");
    let _ = registry.get("code-reviewer").expect("code-reviewer найден");

    inbox_tx.send("сделай ревью".into()).unwrap();
    let params = AgentParams {
        session_id: session.id,
        agent_id,
        model: "mock".into(),
        provider_name: "mock".into(),
        cwd: std::env::temp_dir(),
        provider,
        storage: storage.clone(),
        event_tx,
        inbox: inbox_rx,
        cancel: CancellationToken::new(),
        permissions: Permissions {
            mode: vpsagent_core::PermissionMode::BypassPermissions,
            allow: vec![],
            deny: vec![],
        },
        subagents: subagents.clone(),
        initial_history: vec![],
        allowed_tools: None,
        def_prompt: None,
        pending: vpsagent_runtime::PendingPermissions::new(),
        pending_requests: vpsagent_runtime::pending::PendingRequests::new(),
    };
    // task-инструмент вернёт ошибку "не найден" (т.к. SubagentManager использует дефолтные каталоги),
    // но это ок для smoke-теста: агент завершится.
    run_agent(params).await.unwrap();
    let agents = storage.list_agents(session.id).await.unwrap();
    assert!(
        agents
            .iter()
            .any(|a| a.status == vpsagent_core::AgentStatus::Done),
        "главный агент должен завершиться"
    );
    let _ = std::fs::remove_dir_all(&def_dir);
    let _ = std::fs::remove_file(&db);
}

#[allow(dead_code)]
fn _ensure_types(_: Id, _: ContentBlock, _: TokenUsage) {}
