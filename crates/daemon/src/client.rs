//! RPC-клиент: подключение к демону по Unix-сокету, запросы, чтение событий.
//!
//! Используется бинарём (`vpsagent run`, упрощённый `attach`) и TUI.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use uuid::Uuid;
use vpsagent_core::{Event, JsonRpc, Request, Response};

/// RPC-клиент демона.
///
/// Внутренние halves под `tokio::sync::Mutex` — клиент можно обернуть в `Arc`
/// и использовать из нескольких задач (например, TUI: UI-поток + фоновый
/// `drain_events`).
pub struct RpcClient {
    read: tokio::sync::Mutex<BufReader<tokio::net::unix::OwnedReadHalf>>,
    write: tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>,
    /// Последний увиденный seq события (для инкрементального re-attach).
    last_seq: AtomicU64,
}

impl RpcClient {
    /// Подключиться к сокету демона.
    pub async fn connect(socket_path: &Path) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(socket_path).await?;
        let (read, write) = stream.into_split();
        Ok(Self {
            read: tokio::sync::Mutex::new(BufReader::new(read)),
            write: tokio::sync::Mutex::new(write),
            last_seq: AtomicU64::new(0),
        })
    }

    /// Последний увиденный seq события (0 — ничего не видели).
    pub fn last_seq(&self) -> u64 {
        self.last_seq.load(Ordering::Relaxed)
    }

    /// Зафиксировать seq события.
    fn note_event(&self, ev: &Event) {
        self.last_seq.fetch_max(ev.seq, Ordering::Relaxed);
    }

    /// Послать запрос и дождаться ответа (одноразовый, с уникальным id).
    /// Notifications (server-push), пришедшие во время ожидания, складываются
    /// в `on_event` callback.
    pub async fn request(
        &self,
        req: &Request,
        on_event: &mut impl FnMut(Event),
    ) -> anyhow::Result<Response> {
        let id = Uuid::new_v4().to_string();
        let rpc = JsonRpc::request(id.clone(), req)?;
        let s = serde_json::to_string(&rpc)? + "\n";
        self.write.lock().await.write_all(s.as_bytes()).await?;

        // Читаем строки, пока не получим response с нашим id (или notification).
        loop {
            let mut line = String::new();
            let n = self.read.lock().await.read_line(&mut line).await?;
            if n == 0 {
                anyhow::bail!("соединение закрыто демоном");
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let rpc: JsonRpc = match serde_json::from_str(trimmed) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(err=%e, "битая строка от демона: {trimmed}");
                    continue;
                }
            };
            if rpc.is_notification() {
                if let Some(ev) = rpc.event {
                    self.note_event(&ev);
                    on_event(ev);
                }
                continue;
            }
            // Чужой ответ (другой запрос на том же соединении) — пропускаем.
            if rpc.id.as_ref().and_then(|v| v.as_str()) != Some(id.as_str()) {
                tracing::warn!(?rpc.id, "ответ с чужим id, игнорирую");
                continue;
            }
            // RPC-ошибка — возвращаем как Err, а не Response::Null.
            if let Some(err) = rpc.error {
                anyhow::bail!("RPC {}: {}", err.code, err.message);
            }
            let resp: Response =
                serde_json::from_value(rpc.result.unwrap_or(serde_json::Value::Null))?;
            return Ok(resp);
        }
    }

    /// Только послать запрос (без ожидания ответа) — для fire-and-forget команд.
    pub async fn send(&self, req: &Request) -> anyhow::Result<()> {
        let id = Uuid::new_v4().to_string();
        let rpc = JsonRpc::request(id, req)?;
        let s = serde_json::to_string(&rpc)? + "\n";
        self.write.lock().await.write_all(s.as_bytes()).await?;
        Ok(())
    }

    /// Читать поток событий (после SessionAttach) в callback.
    pub async fn drain_events(&self, mut on_event: impl FnMut(Event)) -> anyhow::Result<()> {
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self.read.lock().await.read_line(&mut buf).await?;
            if n == 0 {
                return Ok(());
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }
            let rpc: JsonRpc = match serde_json::from_str(trimmed) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if let Some(ev) = rpc.event {
                self.note_event(&ev);
                on_event(ev);
            }
        }
    }
}
