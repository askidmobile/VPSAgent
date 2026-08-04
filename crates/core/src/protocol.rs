//! JSON-RPC протокол между TUI-клиентом и демоном.
//!
//! Разделение «команды ядру / события наружу» по мотивам codex-rs
//! (Op / EventMsg). Запросы — [`Request`]; сервер-push — [`Event`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::types::{AgentInfo, DaemonStatus, Id, Session};

/// Версия протокола (на случай эволюции).
pub const PROTOCOL_VERSION: &str = "v1";

/// Запрос от клиента к демону. Каждый метод — отдельный enum-вариант.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// Получить статус демона.
    DaemonStatus,
    /// Создать новую сессию.
    SessionCreate { cwd: String, model: Option<String> },
    /// Список сессий.
    SessionList,
    /// Подписаться на события сессии (возвращает snapshot + дальнейший push).
    /// last_seq — последний увиденный клиентом seq (инкрементальный replay);
    /// None/0 — полный replay. Серверная сторона пока игнорирует (TODO).
    SessionAttach {
        session_id: Id,
        #[serde(default)]
        last_seq: Option<u64>,
    },
    /// Отписаться от событий сессии.
    SessionDetach { session_id: Id },
    /// Отправить сообщение в сессию.
    /// target — явный адресат (агент); если None — классификатор (Фаза 7).
    MessageSend {
        session_id: Id,
        text: String,
        target: Option<Id>,
    },
    /// Спавн субагента из определения.
    AgentSpawn {
        session_id: Id,
        parent_agent_id: Id,
        definition_name: String,
        task: String,
        /// Скопировать контекст родителя (fork-режим, по мотивам codex-rs).
        fork: Option<bool>,
    },
    /// Прервать агента (мягкая остановка).
    AgentInterrupt { agent_id: Id },
    /// Список агентов сессии (для deck-обзора).
    AgentList { session_id: Id },
    /// Ответ пользователя на ask_user от агента.
    UserInputAnswer {
        /// id из события UserInputRequest.
        request_id: String,
        session_id: Id,
        /// Индекс выбранного варианта (0-based) или текстовый ответ.
        choice_index: Option<usize>,
        text: Option<String>,
    },
    /// Ответ пользователя на PermissionRequest (разрешить/запретить tool call).
    PermissionAnswer {
        /// call_id из события PermissionRequest.
        call_id: String,
        /// Разрешить вызов?
        allowed: bool,
    },
    /// Graceful restart демона (D4): сохранить состояние, exec нового бинаря.
    DaemonRestart,
    /// Graceful stop демона (FR-006): отменить агентов, закрыть сокет, exit 0.
    /// Инициируется командой `vpsagent daemon stop` через Unix-сокет.
    DaemonStop,
}

/// Ответ на запрос.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum Response {
    DaemonStatus(DaemonStatus),
    Session(Session),
    SessionList(Vec<Session>),
    /// Snapshot сессии при attach: история + активные агенты.
    SessionAttach {
        messages: Vec<crate::types::Message>,
        agents: Vec<AgentInfo>,
        /// С какого seq продолжается live-push.
        last_seq: u64,
    },
    /// Список агентов сессии (для deck-обзора).
    AgentList(Vec<AgentInfo>),
    Ok,
    Error {
        code: i32,
        message: String,
    },
}

/// Сервер-push событие (стриминг).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EventKind {
    /// Дельта текста ответа модели.
    TextDelta { agent_id: Id, text: String },
    /// Дельта reasoning/thinking-контента (Kimi K2, DeepSeek-R1, Claude
    /// thinking, GPT-5 reasoning summary). Display-only: в историю диалога
    /// не сохраняется (ContentBlock::Thinking — future). Идёт ПЕРЕД TextDelta.
    ThinkingDelta { agent_id: Id, text: String },
    /// Начало вызова инструмента.
    ToolCallStart {
        agent_id: Id,
        call_id: String,
        name: String,
        input: Value,
    },
    /// Завершение вызова инструмента.
    ToolCallEnd {
        agent_id: Id,
        call_id: String,
        output: String,
        is_error: bool,
    },
    /// Изменение статуса агента (для deck-обзора).
    AgentStatusChange {
        agent_id: Id,
        status: crate::types::AgentStatus,
        last_action: String,
    },
    /// Спавн агента.
    AgentSpawned(crate::types::AgentInfo),
    /// Агент завершил работу.
    AgentFinished { agent_id: Id },
    /// Обновление токенов.
    TokenTick {
        agent_id: Id,
        tokens: crate::types::TokenUsage,
    },
    /// Запрос разрешения на tool call (режим default).
    PermissionRequest {
        agent_id: Id,
        call_id: String,
        name: String,
        summary: String,
    },
    /// Агент задаёт вопрос пользователю (инструмент ask_user).
    UserInputRequest {
        request_id: String,
        agent_id: Id,
        prompt: String,
        /// Варианты ответов (если есть).
        options: Vec<String>,
    },
    /// Ответ агента на ask_user (пока как общий диалог).
    UserMessage { agent_id: Id, text: String },
    /// Ошибка агента.
    Error {
        agent_id: Option<Id>,
        message: String,
    },
}

/// Полное событие: seq + payload + timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub session_id: Id,
    pub kind: EventKind,
    pub created_at: DateTime<Utc>,
}

/// Универсальная JSON-RPC 2.0 обёртка.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpc {
    pub jsonrpc: String,
    /// id запроса (None для notifications — server-push).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// Запрос: method + params.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// Ответ: result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Notification: event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<Event>,
    /// Ошибка.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl JsonRpc {
    pub const VERSION: &'static str = "2.0";

    /// Запрос с id.
    pub fn request(id: impl Into<Value>, req: &Request) -> serde_json::Result<Self> {
        let (method, params) = match serde_json::to_value(req)? {
            Value::Object(mut m) => {
                let method = m
                    .remove("method")
                    .and_then(|v| v.as_str().map(str::to_owned));
                let params = m.remove("params");
                (method, params)
            }
            v => (None, Some(v)),
        };
        Ok(Self {
            jsonrpc: Self::VERSION.into(),
            id: Some(id.into()),
            method,
            params,
            result: None,
            event: None,
            error: None,
        })
    }

    /// Ответ на запрос.
    pub fn response(id: impl Into<Value>, resp: &Response) -> serde_json::Result<Self> {
        let value = serde_json::to_value(resp)?;
        let (result, error) = match &value {
            Value::Object(m) if m.get("result").and_then(|v| v.as_str()) == Some("error") => {
                let code = m
                    .get("data")
                    .and_then(|d| d.get("code"))
                    .and_then(|c| c.as_i64())
                    .unwrap_or(-1) as i32;
                let message = m
                    .get("data")
                    .and_then(|d| d.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                (None, Some(RpcError { code, message }))
            }
            _ => (Some(value), None),
        };
        Ok(Self {
            jsonrpc: Self::VERSION.into(),
            id: Some(id.into()),
            method: None,
            params: None,
            result,
            event: None,
            error,
        })
    }

    /// Server-push notification (без id).
    pub fn notification(event: Event) -> Self {
        Self {
            jsonrpc: Self::VERSION.into(),
            id: None,
            method: None,
            params: None,
            result: None,
            event: Some(event),
            error: None,
        }
    }

    /// Маркер «это notification».
    pub fn is_notification(&self) -> bool {
        self.id.is_none() && self.event.is_some()
    }
}

/// JSON-RPC ошибка.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

/// Идентификатор запроса (для мультиплексации).
pub type RequestId = Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    /// Serde round-trip для `Request::DaemonStop` — FR-006: метод должен
    /// корректно сериализоваться/десериализоваться через JSON-RPC-протокол.
    #[test]
    fn daemon_stop_serde_roundtrip() {
        let req = Request::DaemonStop;
        // Request сериализуется с #[serde(tag = "method", content = "params")].
        let json = serde_json::to_string(&req).unwrap();
        // method = "daemon_stop" (snake_case), params — null (unit variant).
        assert!(
            json.contains("\"method\":\"daemon_stop\""),
            "ожидался method=daemon_stop в: {json}"
        );
        let back: Request = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Request::DaemonStop), "ожидался DaemonStop");
    }

    /// `Request::DaemonStatus` — round-trip (существующий метод, используется
    /// командой `vpsagent daemon status`).
    #[test]
    fn daemon_status_serde_roundtrip() {
        let req = Request::DaemonStatus;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"method\":\"daemon_status\""), "в: {json}");
        let back: Request = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Request::DaemonStatus));
    }

    /// `Request::DaemonRestart` — round-trip (существующая заглушка D4).
    #[test]
    fn daemon_restart_serde_roundtrip() {
        let req = Request::DaemonRestart;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"method\":\"daemon_restart\""), "в: {json}");
        let back: Request = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Request::DaemonRestart));
    }

    /// `DaemonStatus` (структура ответа) — round-trip: поля сохраняются.
    #[test]
    fn daemon_status_struct_roundtrip() {
        use crate::types::DaemonStatus;
        let s = DaemonStatus {
            version: "0.1.0".into(),
            pid: 12345,
            uptime_secs: 3600,
            sessions: 3,
            agents: 5,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: DaemonStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, "0.1.0");
        assert_eq!(back.pid, 12345);
        assert_eq!(back.uptime_secs, 3600);
        assert_eq!(back.sessions, 3);
        assert_eq!(back.agents, 5);
    }

    /// `Response::DaemonStatus` — round-trip через JSON-RPC-обёртку.
    #[test]
    fn response_daemon_status_roundtrip() {
        use crate::types::DaemonStatus;
        let resp = Response::DaemonStatus(DaemonStatus {
            version: "0.1.0".into(),
            pid: 99,
            uptime_secs: 10,
            sessions: 0,
            agents: 0,
        });
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"result\":\"daemon_status\""), "в: {json}");
        let back: Response = serde_json::from_str(&json).unwrap();
        match back {
            Response::DaemonStatus(s) => assert_eq!(s.pid, 99),
            other => panic!("ожидался DaemonStatus, получили {other:?}"),
        }
    }

    /// `EventKind::ThinkingDelta` — serde round-trip. Сериализуется с
    /// `#[serde(tag = "kind", content = "data", rename_all = "snake_case")]`
    /// как `{"kind":"thinking_delta","data":{"agent_id":...,"text":...}}`.
    /// Storage agnostic к вариантам через serde, но фиксирует контракт имени.
    #[test]
    fn eventkind_thinking_delta_roundtrip() {
        let agent_id = crate::Id::nil();
        let ev = EventKind::ThinkingDelta {
            agent_id,
            text: "размышляю".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"kind\":\"thinking_delta\""),
            "ожидал kind=thinking_delta в: {json}"
        );
        assert!(
            json.contains("размышляю"),
            "text должен попасть в json: {json}"
        );

        let back: EventKind = serde_json::from_str(&json).unwrap();
        match back {
            EventKind::ThinkingDelta {
                agent_id: aid,
                text,
            } => {
                assert_eq!(aid, agent_id);
                assert_eq!(text, "размышляю");
            }
            other => panic!("ожидал ThinkingDelta, получили {other:?}"),
        }
    }
}
