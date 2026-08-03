//! Rewind (FR-071, D3): откат сессии к произвольной точке.
//!
//! В Фазе 8 — упрощённая версия: снапшоты диалога (сообщений) с точками
//! отката. Полный откат файлов (теневые git-коммиты / копии) — следующая итерация.

use serde::{Deserialize, Serialize};
use vpsagent_core::{Id, Message};

/// Снапшот состояния диалога сессии.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: Id,
    pub label: String,
    pub messages: Vec<Message>,
    pub created_at: String,
}

/// Хранилище снапшотов (in-memory в Фазе 8; файловое — следующая итерация).
#[derive(Default)]
pub struct RewindStore {
    snapshots: Vec<SessionSnapshot>,
}

impl RewindStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Создать снапшот (с меткой).
    pub fn snapshot(
        &mut self,
        session_id: Id,
        label: &str,
        messages: Vec<Message>,
    ) -> SessionSnapshot {
        let snap = SessionSnapshot {
            session_id,
            label: label.to_string(),
            messages,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.snapshots.push(snap.clone());
        snap
    }

    /// Откатить к снапшоту с индексом (0 = самый старый).
    pub fn rewind(&self, index: usize) -> Option<&SessionSnapshot> {
        self.snapshots.get(index)
    }

    pub fn list(&self) -> Vec<&SessionSnapshot> {
        self.snapshots.iter().collect()
    }

    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }
}
