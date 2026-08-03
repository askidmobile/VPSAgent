//! JSON-RPC сервер на Unix-сокете (права 0600).
//!
//! Протокол: построчный JSON (одна JsonRpc на строку).
//! Запросы — с id; ответы — с тем же id; server-push — notifications (без id).

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use vpsagent_core::{DaemonStatus, Event, Id, JsonRpc, Request, Response};
use vpsagent_storage::Storage;

use crate::broadcast::EventBus;
use crate::session_manager::SessionManager;

/// RPC-сервер демона.
pub struct RpcServer {
    manager: SessionManager,
    storage: Storage,
    bus: EventBus,
    started_at: std::time::Instant,
    pid: u32,
    /// Токен graceful shutdown: отменяется сигналом (SIGINT/SIGTERM) или
    /// RPC-командой DaemonStop (FR-006). Владеем shared-копией, чтобы из
    /// обработчика `DaemonStop` запустить shutdown-путь.
    shutdown: CancellationToken,
}

impl RpcServer {
    pub fn new(
        manager: SessionManager,
        storage: Storage,
        bus: EventBus,
        pid: u32,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            manager,
            storage,
            bus,
            started_at: std::time::Instant::now(),
            pid,
            shutdown,
        }
    }

    /// Запустить сервер (слушает socket_path). Блокирует.
    pub async fn serve(self: Arc<Self>, socket_path: &Path) -> anyhow::Result<()> {
        if socket_path.exists() {
            std::fs::remove_file(socket_path).ok();
        }
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let listener = UnixListener::bind(socket_path)?;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
        tracing::info!(socket=%socket_path.display(), "демон слушает");

        while let Ok((stream, _)) = listener.accept().await {
            let server = self.clone();
            tokio::spawn(async move {
                if let Err(e) = server.handle_connection(stream).await {
                    tracing::warn!(error=?e, "ошибка соединения");
                }
            });
        }
        Ok(())
    }

    /// Обработка одного соединения.
    async fn handle_connection(self: Arc<Self>, stream: UnixStream) -> anyhow::Result<()> {
        let (read_half, write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let writer = Arc::new(Mutex::new(write_half));

        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            let rpc: JsonRpc = match serde_json::from_str(&trimmed) {
                Ok(r) => r,
                Err(e) => {
                    self.send_raw(
                        &writer,
                        &JsonRpc {
                            jsonrpc: JsonRpc::VERSION.into(),
                            id: None,
                            method: None,
                            params: None,
                            result: None,
                            event: None,
                            error: Some(vpsagent_core::RpcError {
                                code: -32700,
                                message: format!("parse error: {e}"),
                            }),
                        },
                    )
                    .await?;
                    continue;
                }
            };

            if rpc.is_notification() {
                continue;
            }
            let id = rpc.id.clone();
            let req: Request = match serde_json::from_value(rpc_to_request_value(&rpc)) {
                Ok(r) => r,
                Err(e) => {
                    self.send_error(&writer, id, -32602, format!("invalid params: {e}"))
                        .await?;
                    continue;
                }
            };
            let resp = self.handle_request(req, &writer).await;
            let rpc_resp = JsonRpc::response(id.clone().unwrap_or(serde_json::Value::Null), &resp)?;
            self.send_raw(&writer, &rpc_resp).await?;
        }
        Ok(())
    }

    async fn send_raw(
        &self,
        writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
        rpc: &JsonRpc,
    ) -> anyhow::Result<()> {
        let s = serde_json::to_string(rpc)? + "\n";
        writer.lock().await.write_all(s.as_bytes()).await?;
        Ok(())
    }

    async fn send_error(
        &self,
        writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
        id: Option<serde_json::Value>,
        code: i32,
        message: String,
    ) -> anyhow::Result<()> {
        self.send_raw(
            writer,
            &JsonRpc {
                jsonrpc: JsonRpc::VERSION.into(),
                id,
                method: None,
                params: None,
                result: None,
                event: None,
                error: Some(vpsagent_core::RpcError { code, message }),
            },
        )
        .await
    }

    /// Обработать запрос.
    async fn handle_request(
        &self,
        req: Request,
        writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    ) -> Response {
        match req {
            Request::DaemonStatus => {
                let sessions = self
                    .manager
                    .list_sessions()
                    .await
                    .map(|v| v.len())
                    .unwrap_or(0);
                Response::DaemonStatus(DaemonStatus {
                    version: env!("CARGO_PKG_VERSION").into(),
                    pid: self.pid,
                    uptime_secs: self.started_at.elapsed().as_secs(),
                    sessions,
                    agents: 0,
                })
            }
            Request::SessionCreate { cwd, model } => {
                match self.manager.create_session(&cwd, model.as_deref()).await {
                    Ok(s) => Response::Session(s),
                    Err(e) => Response::Error {
                        code: -32000,
                        message: e.to_string(),
                    },
                }
            }
            Request::SessionList => match self.manager.list_sessions().await {
                Ok(v) => Response::SessionList(v),
                Err(e) => Response::Error {
                    code: -32000,
                    message: e.to_string(),
                },
            },
            // TODO(daemon-агент): использовать last_seq от клиента для инкрементального
            // replay (сейчас всегда полный replay с 0).
            Request::SessionAttach {
                session_id,
                last_seq: _,
            } => {
                let messages = self
                    .storage
                    .list_messages(session_id)
                    .await
                    .unwrap_or_default();
                let agents = self
                    .manager
                    .list_agents(session_id)
                    .await
                    .unwrap_or_default();
                let last_seq = 0;
                let (replay, rx) = match self
                    .bus
                    .subscribe(&self.storage, session_id, last_seq)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        return Response::Error {
                            code: -32000,
                            message: e.to_string(),
                        }
                    }
                };
                let writer = writer.clone();
                let storage = self.storage.clone();
                tokio::spawn(async move {
                    // Отправить событие клиенту; false — соединение закрыто.
                    async fn send_ev(
                        writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
                        ev: Event,
                    ) -> bool {
                        let s = serde_json::to_string(&JsonRpc::notification(ev)).unwrap();
                        writer
                            .lock()
                            .await
                            .write_all((s + "\n").as_bytes())
                            .await
                            .is_ok()
                    }
                    // Последний отправленный seq: дедупликация replay/live (H1).
                    let mut last_sent: u64 = 0;
                    for ev in replay {
                        last_sent = last_sent.max(ev.seq);
                        if !send_ev(&writer, ev).await {
                            return;
                        }
                    }
                    let mut rx = rx;
                    loop {
                        match rx.recv().await {
                            Ok(ev) => {
                                // Дубликат из replay-окна (H1) — пропускаем.
                                if ev.seq <= last_sent {
                                    continue;
                                }
                                last_sent = ev.seq;
                                if !send_ev(&writer, ev).await {
                                    return;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                // Буфер переполнен: молча завершать нельзя (H2) —
                                // дочитываем пропущенное из SQLite с последнего seq.
                                tracing::warn!(skipped, %session_id, "подписчик отстал, replay из SQLite");
                                match storage.events_since(session_id, last_sent).await {
                                    Ok(events) => {
                                        for ev in events {
                                            if ev.seq <= last_sent {
                                                continue;
                                            }
                                            last_sent = ev.seq;
                                            if !send_ev(&writer, ev).await {
                                                return;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(error=?e, "replay после Lagged не удался");
                                        return;
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
                Response::SessionAttach {
                    messages,
                    agents,
                    last_seq,
                }
            }
            Request::SessionDetach { session_id: _ } => Response::Ok,
            Request::MessageSend {
                session_id,
                text,
                target: _,
            } => match self.manager.send_message(session_id, text).await {
                Ok(_) => Response::Ok,
                Err(e) => Response::Error {
                    code: -32000,
                    message: e.to_string(),
                },
            },
            Request::AgentInterrupt { agent_id } => {
                // Сначала проверим субагентов, потом главного.
                self.manager.subagents().interrupt(agent_id).await;
                let _ = self.manager.interrupt_agent(agent_id).await;
                Response::Ok
            }
            Request::AgentSpawn {
                session_id,
                parent_agent_id,
                definition_name,
                task,
                fork,
            } => {
                // Канал событий субагента → шина сессии.
                let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
                crate::broadcast::spawn_forwarder(
                    self.bus.clone(),
                    self.storage.clone(),
                    session_id,
                    event_rx,
                );
                match self
                    .manager
                    .subagents()
                    .spawn(
                        session_id,
                        parent_agent_id,
                        &definition_name,
                        task,
                        fork.unwrap_or(false),
                        vec![],
                        event_tx,
                    )
                    .await
                {
                    Ok(_id) => Response::Ok,
                    Err(e) => Response::Error {
                        code: -32000,
                        message: e.to_string(),
                    },
                }
            }
            Request::AgentList { session_id } => {
                // Активные субагенты + главный из БД.
                let mut agents = self.manager.subagents().list().await;
                let mut main_agents = self
                    .manager
                    .list_agents(session_id)
                    .await
                    .unwrap_or_default();
                agents.append(&mut main_agents);
                Response::AgentList(agents)
            }
            Request::UserInputAnswer { .. } => {
                // В Фазе 8 — заглушка; полная доставка ответа ask_user — Фаза 7.
                Response::Ok
            }
            Request::PermissionAnswer { call_id, allowed } => {
                // C1: доставить решение в runtime — агент ждёт oneshot в pending-реестре.
                if self.manager.pending().resolve(&call_id, allowed) {
                    Response::Ok
                } else {
                    Response::Error {
                        code: -32000,
                        message: format!("неизвестный или устаревший call_id '{call_id}'"),
                    }
                }
            }
            Request::DaemonRestart => {
                // D4: graceful restart. В Фазе 8 — сигнал бинарю через отдельный
                // механизм; здесь просто подтверждаем. Полный exec — в бинаре.
                tracing::info!("запрошен graceful restart демона");
                // TODO: фиксация состояния в SQLite + exec нового бинаря.
                Response::Ok
            }
            Request::DaemonStop => {
                // FR-006: graceful stop через сокет. Отвечаем Ok ДО запуска
                // shutdown — клиент должен получить подтверждение, затем соединение
                // закроется по shutdown-циклу в `run()`. Небольшая задержка даёт
                // времени на запись/доставку ответа перед отменой сервера.
                tracing::info!("запрошен graceful stop демона (RPC)");
                let shutdown = self.shutdown.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    shutdown.cancel();
                });
                Response::Ok
            }
        }
    }
}

/// Собрать Value для десериализации Request из JsonRpc.
fn rpc_to_request_value(rpc: &JsonRpc) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    if let Some(method) = &rpc.method {
        obj.insert("method".into(), serde_json::Value::String(method.clone()));
    }
    if let Some(params) = &rpc.params {
        obj.insert("params".into(), params.clone());
    }
    serde_json::Value::Object(obj)
}

/// Удерживаем тип, чтобы не было unused-import предупреждений в будущем.
#[allow(dead_code)]
fn _ensure_event_type(_: Event) {}
#[allow(dead_code)]
fn _ensure_id_type(_: Id) {}
