//! Реестр pending-запросов разрешений (C1).
//!
//! Агентный цикл при `Decision::Ask` регистрирует oneshot-канал по `call_id`
//! и ждёт решения пользователя. Демон, получив `Request::PermissionAnswer`,
//! доставляет решение через [`PendingPermissions::resolve`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

/// Разделяемый реестр: call_id → отправитель решения (allowed/denied).
#[derive(Clone, Default)]
pub struct PendingPermissions {
    inner: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
}

impl PendingPermissions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Зарегистрировать ожидание решения по вызову `call_id`.
    /// Возвращает receiver, который получит `true` (allow) или `false` (deny).
    pub fn register(&self, call_id: impl Into<String>) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.inner.lock().unwrap().insert(call_id.into(), tx);
        rx
    }

    /// Доставить решение пользователя. `false` — такого call_id нет
    /// (устаревший/неизвестный ответ).
    pub fn resolve(&self, call_id: &str, allowed: bool) -> bool {
        let tx = self.inner.lock().unwrap().remove(call_id);
        match tx {
            Some(tx) => tx.send(allowed).is_ok(),
            None => false,
        }
    }

    /// Снять регистрацию (например, по таймауту ожидания).
    pub fn cancel(&self, call_id: &str) {
        self.inner.lock().unwrap().remove(call_id);
    }
}

/// Ответ на ожидающий запрос (общий реестр по request_id).
#[derive(Debug)]
pub enum Answer {
    /// Ответ на запрос разрешения (PermissionRequest).
    Permission(bool),
    /// Текстовый ответ (ask_user): текст или индекс варианта строкой.
    Text(String),
}

/// Общий реестр pending-запросов: request_id → отправитель ответа.
/// Используется ask_user (и в перспективе — permission-ответами).
#[derive(Clone, Default)]
pub struct PendingRequests {
    inner: Arc<Mutex<HashMap<String, oneshot::Sender<Answer>>>>,
}

impl PendingRequests {
    pub fn new() -> Self {
        Self::default()
    }

    /// Зарегистрировать запрос и получить приёмник ответа.
    pub fn register(&self, request_id: impl Into<String>) -> oneshot::Receiver<Answer> {
        let (tx, rx) = oneshot::channel();
        self.inner.lock().unwrap().insert(request_id.into(), tx);
        rx
    }

    /// Доставить ответ по request_id. false — запрос не найден/истёк.
    pub fn answer(&self, request_id: &str, answer: Answer) -> bool {
        let tx = self.inner.lock().unwrap().remove(request_id);
        match tx {
            Some(tx) => tx.send(answer).is_ok(),
            None => false,
        }
    }

    /// Доставить текстовый ответ (ask_user).
    pub fn answer_text(&self, request_id: &str, text: String) -> bool {
        self.answer(request_id, Answer::Text(text))
    }

    /// Доставить ответ разрешения (permission).
    pub fn answer_permission(&self, request_id: &str, allowed: bool) -> bool {
        self.answer(request_id, Answer::Permission(allowed))
    }

    /// Снять регистрацию (таймаут/отмена ожидания).
    pub fn cancel(&self, request_id: &str) {
        self.inner.lock().unwrap().remove(request_id);
    }
}
