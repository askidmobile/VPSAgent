//! Состояние приложения TUI.

use std::collections::{HashMap, VecDeque};

use vpsagent_core::{AgentInfo, EventKind, Id, Session};

/// Максимальный размер текстового буфера одного агента (байт).
/// При превышении обрезаем голову по границе char (keep tail).
const MAX_AGENT_TEXT: usize = 256 * 1024;

/// Ожидающий подтверждения tool call (PermissionRequest).
pub struct PendingPermission {
    pub call_id: String,
    pub name: String,
    pub summary: String,
}

/// Состояние TUI.
pub struct App {
    /// Текущая сессия.
    pub session: Option<Session>,
    /// Активные агенты (deck-обзор, FR-031).
    pub agents: Vec<AgentInfo>,
    /// Текст по агентам (agent_id → накопленный стрим).
    pub texts: HashMap<Id, String>,
    /// Общий буфер (эхо пользователя, сообщения без привязки к выбранному агенту).
    pub agent_text: String,
    /// Лог событий (последние N, для отладки/статуса).
    pub events: VecDeque<String>,
    /// Строка ввода пользователя.
    pub input: String,
    /// История ввода.
    pub input_history: Vec<String>,
    /// Сообщение-статус (одна строка внизу).
    pub status: String,
    /// Агент работает?
    pub agent_busy: bool,
    /// Прокрутка: нужно ли автоскроллить вниз.
    pub auto_scroll: bool,
    /// Подписаны ли уже на события сессии.
    pub attached: bool,
    /// Выбранный агент в deck (для фокуса).
    pub selected_agent: Option<Id>,
    /// Ожидающее подтверждение разрешения (y/n).
    pub pending_permission: Option<PendingPermission>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            session: None,
            agents: vec![],
            texts: HashMap::new(),
            agent_text: String::new(),
            events: VecDeque::with_capacity(200),
            input: String::new(),
            input_history: vec![],
            status: "vpsagent — Ctrl+C выход, Enter отправить".into(),
            agent_busy: false,
            auto_scroll: true,
            attached: false,
            selected_agent: None,
            pending_permission: None,
        }
    }
}

impl App {
    pub fn push_event(&mut self, line: impl Into<String>) {
        if self.events.len() >= 200 {
            self.events.pop_front();
        }
        self.events.push_back(line.into());
    }

    /// Обновить/добавить карточку агента в deck.
    pub fn upsert_agent(&mut self, info: AgentInfo) {
        if let Some(slot) = self.agents.iter_mut().find(|a| a.id == info.id) {
            *slot = info;
        } else {
            self.agents.push(info);
        }
    }

    /// Выбрать агента (фокус-панель покажет его текст).
    pub fn select_agent(&mut self, id: Option<Id>) {
        self.selected_agent = id;
        self.auto_scroll = true;
    }

    /// Текст для фокус-панели: выбранный агент, иначе общий буфер (main).
    pub fn current_text(&self) -> &str {
        if let Some(id) = self.selected_agent {
            if let Some(t) = self.texts.get(&id) {
                return t;
            }
        }
        &self.agent_text
    }

    /// Добавить дельту текста агента с обрезкой буфера до MAX_AGENT_TEXT.
    pub fn apply_text_delta(&mut self, agent_id: Id, text: &str) {
        let buf = self.texts.entry(agent_id).or_default();
        buf.push_str(text);
        if buf.len() > MAX_AGENT_TEXT {
            let mut start = buf.len() - MAX_AGENT_TEXT;
            while !buf.is_char_boundary(start) {
                start += 1;
            }
            buf.drain(..start);
        }
        // Дублируем в общий буфер, если агент не выбран явно.
        if self.selected_agent.is_none() {
            self.agent_text.push_str(text);
            if self.agent_text.len() > MAX_AGENT_TEXT {
                let mut start = self.agent_text.len() - MAX_AGENT_TEXT;
                while !self.agent_text.is_char_boundary(start) {
                    start += 1;
                }
                self.agent_text.drain(..start);
            }
        }
        self.agent_busy = true;
        self.auto_scroll = true;
    }

    /// Применить событие от демона к состоянию.
    pub fn apply_event(&mut self, ev: EventKind) {
        match ev {
            EventKind::TextDelta { agent_id, text } => {
                self.apply_text_delta(agent_id, &text);
            }
            EventKind::UserMessage { agent_id, text } => {
                self.apply_text_delta(agent_id, &format!("\n\n--- агент ---\n{text}"));
            }
            EventKind::ToolCallStart { name, .. } => {
                self.push_event(format!("[tool] {name}"));
                self.agent_busy = true;
            }
            EventKind::ToolCallEnd { is_error, output, .. } => {
                if is_error {
                    self.push_event(format!("[error] {output}"));
                }
            }
            EventKind::AgentSpawned(info) => {
                self.upsert_agent(info);
            }
            EventKind::AgentFinished { agent_id } => {
                self.agent_busy = false;
                self.status = "агент завершил".into();
                if let Some(a) = self.agents.iter_mut().find(|a| a.id == agent_id) {
                    a.status = vpsagent_core::AgentStatus::Done;
                }
            }
            EventKind::PermissionRequest { call_id, name, summary, .. } => {
                self.pending_permission = Some(PendingPermission {
                    call_id,
                    name: name.clone(),
                    summary,
                });
                self.status = format!("подтвердить tool {name}? [y/n]");
            }
            EventKind::Error { message, .. } => {
                self.status = format!("ошибка: {message}");
                self.agent_busy = false;
            }
            _ => {}
        }
    }

    pub fn submit_input(&mut self) -> Option<(Id, String)> {
        if self.input.is_empty() || self.session.is_none() {
            return None;
        }
        let session_id = self.session.as_ref().unwrap().id;
        let text = self.input.clone();
        self.input_history.push(text.clone());
        self.input.clear();
        Some((session_id, text))
    }
}
