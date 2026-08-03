//! Критики (FR-061): /review и авто-цикл «генератор → критик → доработка».
//!
//! Критик — субагент-ревьюер, проверяющий результат автора. Запускается через
//! SubagentManager.spawn со встроенным определением code-reviewer.

use tokio::sync::mpsc;
use vpsagent_core::{EventKind, Id};

use crate::definition::AgentDefinition;
use crate::subagent::SubagentManager;

/// Лимит итераций авто-цикла критика (защита от зацикливания).
pub const MAX_REVIEW_ITERATIONS: usize = 3;

/// Системный промпт критика кода.
pub const CODE_REVIEWER_PROMPT: &str = "\
Ты — критик кода (code-reviewer). Твоя задача — проверять результат работы другого агента.
Правила:
- Анализируй изменённые файлы и diff (используй read/grep).
- Находи: баги, логические ошибки, проблемы безопасности, несоответствие требованиям.
- Верни СТРУКТУРИРОВАННЫЙ отчёт:
  * строка 'ПРОБЛЕМ НЕ НАЙДЕНО' — если всё чисто.
  * иначе список: [критичность] файл:строка — описание.
- НЕ правь код сам. Только ревью.
- Ответь только отчётом, без лишнего текста.
";

/// Встроенное определение критика.
pub fn code_reviewer_definition() -> AgentDefinition {
    AgentDefinition {
        name: "code-reviewer".into(),
        description: "Ревьюер кода: проверяет результат агента".into(),
        prompt: CODE_REVIEWER_PROMPT.to_string(),
        tools: vec!["read".into(), "grep".into()],
        model: None,
        fork: true, // видит контекст автора (дифф)
        source: std::path::PathBuf::from("builtin:code-reviewer"),
    }
}

/// Результат ревью.
#[derive(Debug, Clone)]
pub struct ReviewResult {
    pub has_issues: bool,
    pub report: String,
}

/// Запустить критика. Возвращает отчёт.
///
/// В Фазе 3 — спавнится субагент-критик; его вывод накапливается.
/// Полный сбор вывода субагента (task_output) и авто-цикл доработки — Фаза 5.
pub async fn run_review(
    subagents: &SubagentManager,
    session_id: Id,
    author_agent_id: Id,
    task: String,
    event_tx: mpsc::UnboundedSender<EventKind>,
) -> anyhow::Result<ReviewResult> {
    // Регистрируем встроенного критика, если ещё нет.
    subagents.register_definition(code_reviewer_definition());
    let review_task = format!(
        "Проверь результат работы агента по задаче:\n{}\n\nИзучи изменения и дай отчёт.",
        task
    );
    // Спавним критика.
    let critic_id = subagents
        .spawn(
            session_id,
            author_agent_id,
            "code-reviewer",
            review_task,
            true, // fork: критик видит контекст автора
            vec![],
            event_tx,
        )
        .await?;
    // Пока возвращаем «в процессе»; полный результат — через task_output в Фазе 5.
    Ok(ReviewResult {
        has_issues: false,
        report: format!("критик запущен (id {critic_id}); отчёт будет доступен в Фазе 5"),
    })
}