//! Классификатор мультипотока (FR-055, D2): маршрутизация сообщений пользователя.
//!
//! Стратегия «эвристики + LLM-fallback»:
//! 1. Явные команды (/tell <агент>, /new <задача>, /steer) — всегда приоритет.
//! 2. @упоминания агента.
//! 3. Фокус-контекст (если пользователь смотрит на субагента).
//! 4. LLM-fallback (в Фазе 7 — заглушка, возвращающая «главному»; реальный вызов — Фаза 8).

use vpsagent_core::{Id, Message, Role};

/// Куда направить сообщение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Главному агенту.
    Main,
    /// Конкретному субагенту.
    ToAgent(Id),
    /// Спавн нового агента (из /new).
    SpawnNew { task: String },
    /// Steering работающего агента (вмешательство).
    Steer(Id, String),
}

/// Маршрут сообщения.
pub fn classify(text: &str, session_messages: &[Message]) -> Route {
    let trimmed = text.trim();

    // 1. /new <задача> — спавн нового субагента.
    if let Some(rest) = trimmed.strip_prefix("/new ") {
        return Route::SpawnNew {
            task: rest.to_string(),
        };
    }

    // 2. /tell <имя или id> <сообщение> — конкретному агенту.
    if let Some(rest) = trimmed.strip_prefix("/tell ") {
        // Разделяем на адресат и текст.
        if let Some((target, msg)) = split_first_word(rest) {
            if let Ok(id) = Id::parse_str(target) {
                return Route::ToAgent(id);
            }
            // По имени агента — ищем в сообщениях сессии (недоступно здесь).
            let _ = (target, msg);
            return Route::Main;
        }
    }

    // 3. /steer — steering главного/текущего.
    if let Some(rest) = trimmed.strip_prefix("/steer") {
        return Route::Steer(Main_id(), rest.trim().to_string());
    }

    // 4. @упоминание агента.
    if let Some(idx) = trimmed.find('@') {
        let rest = &trimmed[idx + 1..];
        if let Some(name) = rest.split_whitespace().next() {
            // Ищем агента по имени в истории (id в messages).
            if let Some(agent_id) = find_agent_by_name(session_messages, name) {
                return Route::ToAgent(agent_id);
            }
        }
    }

    // 5. Неоднозначно — главному (в Фазе 7 без LLM-fallback; Фаза 8 добавит).
    Route::Main
}

/// Главный агент — определяем как agent с role Assistant и parent_id None.
/// В упрощении Фазы 7 — заглушка: возвращаем первый assistant-id или новый.
#[allow(non_snake_case)]
fn Main_id() -> Id {
    // В Фазе 7 классификатор не знает главного; возвращаем nil.
    // Реальная маршрутизация — в SessionManager.
    uuid::Uuid::nil()
}

fn split_first_word(s: &str) -> Option<(&str, &str)> {
    let trimmed = s.trim();
    let space = trimmed.find(char::is_whitespace)?;
    Some((&trimmed[..space], trimmed[space..].trim()))
}

fn find_agent_by_name(messages: &[Message], name: &str) -> Option<Id> {
    // Ищем agent_id у сообщений, где в тексте упоминается имя.
    // Упрощение: возвращаем None (в Фазе 7 агенты адресуются по id через /tell).
    let _ = (messages, name);
    None
}

/// Проверить, является ли сообщение явной командой мультипотока.
pub fn is_multiplex_command(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("/tell ") || t.starts_with("/new ") || t.starts_with("/steer")
}

/// Разделить текст и роль (для session_manager).
pub fn as_user_message(text: &str) -> (String, Role) {
    (text.to_string(), Role::User)
}

// Используем типы для ясности API.
#[allow(dead_code)]
fn _ensure(_: &str, _: &Role) {}
