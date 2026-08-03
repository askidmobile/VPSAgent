//! Broadcast pub/sub событий сессии: подписчики по session_id, replay с last_seq.
//!
//! Каждый подключённый клиент (attach) создаёт подписку. События от агентов
//! сохраняются в SQLite и форвардятся живым подписчикам. При attach клиент
//! получает replay пропущенных событий (events_since) + live-стрим.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, Mutex};
use vpsagent_core::{Event, EventKind, Id};
use vpsagent_storage::Storage;

/// Шина событий: сессия → broadcaster.
#[derive(Clone, Default)]
pub struct EventBus {
    channels: Arc<Mutex<HashMap<Id, broadcast::Sender<Event>>>>,
    /// Буфер событий для подписчиков, которые пришли позже (помимо SQLite replay).
    buffer_size: usize,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            channels: Default::default(),
            buffer_size: 256,
        }
    }

    /// Получить или создать sender для сессии.
    async fn sender(&self, session_id: Id) -> broadcast::Sender<Event> {
        let mut channels = self.channels.lock().await;
        channels
            .entry(session_id)
            .or_insert_with(|| {
                let (tx, _rx) = broadcast::channel(self.buffer_size);
                tx
            })
            .clone()
    }

    /// Опубликовать событие (запись в SQLite + форвард живым подписчикам).
    pub async fn publish(
        &self,
        storage: &Storage,
        session_id: Id,
        agent_id: Option<Id>,
        kind: EventKind,
    ) -> anyhow::Result<()> {
        let event = storage.append_event(session_id, agent_id, kind).await?;
        let sender = self.sender(session_id).await;
        // send ошибается, если нет подписчиков — это нормально, событие уже в SQLite.
        let _ = sender.send(event);
        Ok(())
    }

    /// Подписаться на события сессии с replay пропущенных.
    /// Возвращает Vec<Event> (replay) + Receiver для live.
    /// Подписка оформляется ДО чтения replay — события, опубликованные
    /// между replay и подпиской, не теряются (H1); дубликаты отсекаются
    /// потребителем по seq (см. server.rs: пропуск seq <= last_replay_seq).
    pub async fn subscribe(
        &self,
        storage: &Storage,
        session_id: Id,
        last_seq: u64,
    ) -> anyhow::Result<(Vec<Event>, broadcast::Receiver<Event>)> {
        // 1. Live-подписка первой: всё, что опубликуется дальше, попадёт в буфер.
        let sender = self.sender(session_id).await;
        let rx = sender.subscribe();
        // 2. Replay из SQLite (может пересечься с буфером — дедупликация по seq).
        let replay = storage.events_since(session_id, last_seq).await?;
        Ok((replay, rx))
    }

    /// Закрыть шину сессии (при завершении/архивации).
    pub async fn close(&self, session_id: Id) {
        let mut channels = self.channels.lock().await;
        channels.remove(&session_id);
    }
}

/// Хелпер: запустить.forwarding-таску, которая тянет EventKind из mpsc и публикует в шину.
pub fn spawn_forwarder(
    bus: EventBus,
    storage: Storage,
    session_id: Id,
    mut rx: mpsc::UnboundedReceiver<EventKind>,
) {
    tokio::spawn(async move {
        while let Some(kind) = rx.recv().await {
            if let Err(e) = bus.publish(&storage, session_id, None, kind).await {
                tracing::warn!(error=?e, "publish event failed");
            }
        }
    });
}