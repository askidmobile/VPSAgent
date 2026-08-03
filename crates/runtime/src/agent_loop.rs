//! Harness-цикл агента (FR-027):messages → provider.stream → tool calls → repeat.
//!
//! В Фазе 1 — основной цикл; ретраи, cancel, truncate, лимит витков.
//! Steering через inbox — будет в Фазе 7 (пока drain inbox в конце).

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vpsagent_core::{
    AgentInfo, AgentKind, AgentStatus, ContentBlock, EventKind, Id, Message, Result, Role,
    TokenUsage,
};
use vpsagent_providers::{ChatRequest, Provider, StreamChunk};
use vpsagent_storage::Storage;

use crate::context::system_prompt;
use crate::pending::{PendingPermissions, PendingRequests};
use crate::permissions::{Decision, Permissions};
use crate::subagent::SubagentManager;
use crate::tools::{Registry, ToolContext};
use crate::truncate;

/// Параметры запуска агента.
pub struct AgentParams {
    pub session_id: Id,
    pub agent_id: Id,
    pub model: String,
    pub cwd: PathBuf,
    pub provider: Arc<dyn Provider>,
    pub storage: Storage,
    /// Куда отправлять события (для broadcast подписчикам/клиентам).
    pub event_tx: mpsc::UnboundedSender<EventKind>,
    /// Входящие сообщения пользователя (steering/мультипоток).
    pub inbox: mpsc::UnboundedReceiver<String>,
    /// Токен отмены.
    pub cancel: CancellationToken,
    pub permissions: Permissions,
    /// Менеджер субагентов (для инструмента task).
    pub subagents: SubagentManager,
    /// Начальная история (fork-режим: контекст родителя).
    pub initial_history: Vec<Message>,
    /// Системный промпт определения субагента (если субагент).
    pub def_prompt: Option<String>,
    /// Ограничение набора инструментов (сужение прав субагента; None = все).
    pub allowed_tools: Option<Vec<String>>,
    /// Реестр ожидающих разрешений (C1): решение пользователя приходит из демона.
    pub pending: PendingPermissions,
    /// Реестр ожидающих ответов ask_user (C15).
    pub pending_requests: PendingRequests,
}

const MAX_TURNS: u32 = 30;

/// Создать сообщение (хелпер, т.к. Message в core без конструктора).
fn new_message(session_id: Id, agent_id: Option<Id>, role: Role, content: Vec<ContentBlock>) -> Message {
    Message {
        id: Uuid::new_v4(),
        session_id,
        agent_id,
        role,
        content,
        tokens: None,
        created_at: chrono::Utc::now(),
    }
}

/// Запустить агента. Блокирует до завершения (Done/Failed/Interrupted).
pub async fn run_agent(mut params: AgentParams) -> Result<()> {
    let is_main = params.def_prompt.is_none();
    let mut tools = Registry::phase1(is_main);
    // Сужение инструментов субагента (FR-015): разрешён только allowed_tools.
    if let Some(allowed) = &params.allowed_tools {
        let keep: std::collections::HashSet<&str> = allowed.iter().map(|s| s.as_str()).collect();
        tools = tools.filter(&keep);
    }
    let tool_schemas = tools.schemas();
    // Скиллы (FR-040): загружаем реестр, индекс пойдёт в системный промпт.
    let skills_registry = crate::skills::SkillRegistry::load_defaults();
    let ctx = ToolContext {
        cwd: params.cwd.clone(),
        session_id: params.session_id,
        agent_id: params.agent_id,
        subagents: params.subagents.clone(),
        event_tx: params.event_tx.clone(),
        storage: params.storage.clone(),
        pending: params.pending.clone(),
        pending_requests: params.pending_requests.clone(),
        todo_state: crate::tools::todo::TodoState::default(),
        allow_paths: vec![],
    };
    let temp_dir = params.cwd.join(".vpsagent/tmp");
    let temp_dir: std::path::PathBuf = temp_dir;
    let _ = std::fs::create_dir_all(&temp_dir);

    // Регистрируем агента в БД.
    let info = AgentInfo {
        id: params.agent_id,
        session_id: params.session_id,
        name: params
            .def_prompt
            .as_ref()
            .map(|_| "sub".to_string())
            .unwrap_or_else(|| "main".to_string()),
        kind: if is_main { AgentKind::Main } else { AgentKind::Sub },
        status: AgentStatus::Working,
        last_action: "старт".into(),
        tokens: TokenUsage::default(),
        model: params.model.clone(),
        parent_id: None,
    };
    let _ = params.storage.upsert_agent(&info).await;

    // История диалога: initial_history (fork) + сообщения.
    let mut history: Vec<Message> = params.initial_history.clone();
    let mut turns: u32 = 0;
    let mut total_usage = TokenUsage::default();

    // Стартовое сообщение из inbox (если есть).
    let initial = params.inbox.recv().await;
    if let Some(text) = initial {
        let user_msg = new_message(
            params.session_id,
            Some(params.agent_id),
            Role::User,
            vec![ContentBlock::Text { text }],
        );
        let _ = params.storage.insert_message(&user_msg).await;
        history.push(user_msg);
    }

    // Основной цикл обёрнут в async-блок: финализация статуса выполняется
    // на ЛЮБОМ выходе (Ok, Err по `?`, отмена) — C11.
    let loop_result: Result<()> = async {
        'outer: loop {
        if params.cancel.is_cancelled() {
            break 'outer;
        }
        turns += 1;
        if turns > MAX_TURNS {
            let _ = params.event_tx.send(EventKind::Error {
                agent_id: Some(params.agent_id),
                message: format!("превышен лимит витков ({MAX_TURNS})"),
            });
            break 'outer;
        }

        let system = match &params.def_prompt {
            Some(p) => p.clone(),
            None => {
                let base = system_prompt(&params.cwd);
                let skills_index = skills_registry.index();
                if skills_index.is_empty() {
                    base
                } else {
                    format!(
                        "{base}\n\n## Доступные скиллы (вызывай по имени через инструменты/slash)\n{skills_index}\n"
                    )
                }
            }
        };
        // Компактификация (FR-070): при приближении к лимиту окна — сжать историю.
        if crate::compact::estimate_tokens_messages(&history) > crate::compact::COMPACT_THRESHOLD {
            let before = history.len();
            history = crate::compact::compact(&history, crate::compact::COMPACT_THRESHOLD);
            let _ = params.event_tx.send(EventKind::UserMessage {
                agent_id: params.agent_id,
                text: format!("[компактификация: {before} → {} сообщений]", history.len()),
            });
        }
        let req = ChatRequest {
            model: params.model.clone(),
            system,
            messages: history.clone(),
            tools: tool_schemas.clone(),
            temperature: Some(0.0),
        };

        // Стрим с ретраями встроен в Provider.
        let mut stream = params.provider.stream(req);
        let mut assistant_blocks: Vec<ContentBlock> = vec![];
        let mut assistant_text = String::new();
        let mut tool_calls: Vec<(String, String, serde_json::Value)> = vec![];

        while let Some(chunk) = stream.next().await {
            if params.cancel.is_cancelled() {
                break 'outer;
            }
            match chunk? {
                StreamChunk::TextDelta(t) => {
                    assistant_text.push_str(&t);
                    let _ = params.event_tx.send(EventKind::TextDelta {
                        agent_id: params.agent_id,
                        text: t,
                    });
                }
                StreamChunk::ToolCall { id, name, input } => {
                    tool_calls.push((id.clone(), name.clone(), input.clone()));
                    let _ = params.event_tx.send(EventKind::ToolCallStart {
                        agent_id: params.agent_id,
                        call_id: id,
                        name,
                        input,
                    });
                }
                StreamChunk::Usage(u) => {
                    total_usage.input += u.input;
                    total_usage.output += u.output;
                    let _ = params.event_tx.send(EventKind::TokenTick {
                        agent_id: params.agent_id,
                        tokens: total_usage,
                    });
                }
                StreamChunk::Done => break,
            }
        }

        if !assistant_text.is_empty() {
            assistant_blocks.push(ContentBlock::Text { text: assistant_text.clone() });
            let msg = new_message(
                params.session_id,
                Some(params.agent_id),
                Role::Assistant,
                assistant_blocks.clone(),
            );
            let _ = params.storage.insert_message(&msg).await;
            let _ = params.event_tx.send(EventKind::UserMessage {
                agent_id: params.agent_id,
                text: assistant_text.clone(),
            });
            history.push(msg);
        }

        // Вызовы инструментов (последовательно в Фазе 1; параллельно — Фаза 2).
        let had_tool_calls = !tool_calls.is_empty();
        // tool_use-блоки ассистента (для истории).
        let mut tool_use_blocks: Vec<ContentBlock> = vec![];
        let mut tool_results: Vec<ContentBlock> = vec![];
        for (id, name, input) in tool_calls {
            let decision = params.permissions.check(&name);
            let (output, is_error) = match decision {
                Decision::Deny(reason) => (reason, true),
                Decision::Ask => {
                    // C1: шлём запрос разрешения и ЖДЁМ решения пользователя
                    // (раньше исполняли инструмент сразу — обход прав).
                    let summary_input = input.to_string();
                    let _ = params.event_tx.send(EventKind::PermissionRequest {
                        agent_id: params.agent_id,
                        call_id: id.clone(),
                        name: name.clone(),
                        // C4: срез по символам, не по байтам (паника на UTF-8).
                        summary: format!("{name} {}", truncate::truncate_chars(&summary_input, 80)),
                    });
                    let rx = params.pending.register(id.clone());
                    // Ждём ответ из демона (PermissionAnswer) до 60 секунд.
                    let answer = tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        rx,
                    )
                    .await;
                    match answer {
                        Ok(Ok(true)) => {
                            // Разрешено — выполняем.
                            let res = tools.run(&name, input.clone(), &ctx).await?;
                            let content = truncate::apply(&res.content, &temp_dir);
                            (content, res.is_error)
                        }
                        Ok(Ok(false)) | Ok(Err(_)) => {
                            // Запрещено (или канал закрылся — демон ушёл).
                            params.pending.cancel(&id);
                            ("отклонено пользователем".to_string(), true)
                        }
                        Err(_) => {
                            // Таймаут ожидания решения.
                            params.pending.cancel(&id);
                            ("таймаут ожидания разрешения (60с)".to_string(), true)
                        }
                    }
                }
                Decision::Allow => {
                    let res = tools.run(&name, input.clone(), &ctx).await?;
                    let content = truncate::apply(&res.content, &temp_dir);
                    (content, res.is_error)
                }
            };
            tool_use_blocks.push(ContentBlock::ToolUse { id: id.clone(), name: name.clone(), input });
            tool_results.push(ContentBlock::ToolResult {
                tool_use_id: id.clone(),
                content: output.clone(),
                is_error,
            });
            let _ = params.event_tx.send(EventKind::ToolCallEnd {
                agent_id: params.agent_id,
                call_id: id,
                output: output.clone(),
                is_error,
            });
            let _ = params
                .storage
                .upsert_agent(&AgentInfo {
                    last_action: name.clone(),
                    tokens: total_usage,
                    ..info.clone()
                })
                .await;
        }

        // История: assistant с tool_use, затем tool с результатами.
        if !tool_use_blocks.is_empty() {
            let assistant_tool_msg = new_message(
                params.session_id,
                Some(params.agent_id),
                Role::Assistant,
                tool_use_blocks,
            );
            let _ = params.storage.insert_message(&assistant_tool_msg).await;
            history.push(assistant_tool_msg);
        }
        if !tool_results.is_empty() {
            let tool_result_msg = new_message(
                params.session_id,
                Some(params.agent_id),
                Role::Tool,
                tool_results,
            );
            let _ = params.storage.insert_message(&tool_result_msg).await;
            history.push(tool_result_msg);
        }

        // Если в этом витке не было tool calls — финальный ответ, выход.
        if !had_tool_calls {
            break 'outer;
        }

        // Steering: проверить новые сообщения (мультипоток — Фаза 7; пока просто дреин).
        while let Ok(msg) = params.inbox.try_recv() {
            let user_msg = new_message(
                params.session_id,
                Some(params.agent_id),
                Role::User,
                vec![ContentBlock::Text { text: msg }],
            );
            let _ = params.storage.insert_message(&user_msg).await;
            history.push(user_msg);
        }
        }
        Ok(())
    }
    .await;

    // Финальный статус: Failed при ошибке, Interrupted при отмене, Done иначе (C11).
    let status = if params.cancel.is_cancelled() {
        AgentStatus::Interrupted
    } else if loop_result.is_err() {
        AgentStatus::Failed
    } else {
        AgentStatus::Done
    };
    if let Err(e) = &loop_result {
        tracing::warn!(agent_id = %params.agent_id, error = ?e, "агент завершился с ошибкой");
        let _ = params.event_tx.send(EventKind::Error {
            agent_id: Some(params.agent_id),
            message: e.to_string(),
        });
    }
    let _ = params
        .storage
        .upsert_agent(&AgentInfo {
            status,
            tokens: total_usage,
            ..info
        })
        .await;
    let _ = params
        .event_tx
        .send(EventKind::AgentFinished { agent_id: params.agent_id });
    loop_result
}