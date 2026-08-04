//! SessionManager: жизненный цикл сессий и агент-тасок.
//!
//! Хранит inbox-каналы активных агентов (чтобы доставлять steering-сообщения),
//! cancel-токены и handles запущенных тасок. Resume из SQLite при старте демона
//! (в Фазе 1 — минимально: создаёт сессию по запросу, агента по запросу).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vpsagent_core::{Config, EventKind, Id, PermissionMode, Session};
use vpsagent_providers::Provider;
use vpsagent_runtime::{run_agent, AgentParams, PendingPermissions, Permissions, SubagentManager};
use vpsagent_storage::Storage;

use crate::broadcast::{spawn_forwarder, EventBus};

/// Запущенный агент.
struct RunningAgent {
    inbox: mpsc::UnboundedSender<String>,
    cancel: CancellationToken,
    /// Уникальный id спавна: watcher удаляет запись, только если она его (C10).
    spawn_id: Uuid,
}

/// Менеджер сессий и агентов.
#[derive(Clone)]
pub struct SessionManager {
    storage: Storage,
    config: Arc<Config>,
    bus: EventBus,
    /// Менеджер субагентов (общий для сессии).
    subagents: SubagentManager,
    agents: Arc<Mutex<HashMap<Id, RunningAgent>>>,
    /// Per-session мьютексы: find-or-spawn и restart атомарны (H5/H15).
    spawn_locks: Arc<Mutex<HashMap<Id, Arc<Mutex<()>>>>>,
    /// Общий реестр ожидающих разрешений (C1): главные агенты + субагенты.
    pending: PendingPermissions,
    /// Общий реестр ожидающих ответов ask_user (C15).
    pending_requests: vpsagent_runtime::pending::PendingRequests,
}

impl SessionManager {
    pub fn new(storage: Storage, config: Arc<Config>, bus: EventBus) -> Self {
        let pending = PendingPermissions::new();
        let pending_requests = vpsagent_runtime::pending::PendingRequests::new();
        let subagents = SubagentManager::new(storage.clone(), config.clone())
            .with_pending(pending.clone())
            .with_pending_requests(pending_requests.clone());
        Self {
            storage,
            config,
            bus,
            subagents,
            agents: Default::default(),
            spawn_locks: Default::default(),
            pending,
            pending_requests,
        }
    }

    pub fn subagents(&self) -> &SubagentManager {
        &self.subagents
    }

    /// Реестр ожидающих разрешений (для PermissionAnswer из RPC).
    pub fn pending(&self) -> &PendingPermissions {
        &self.pending
    }

    /// Мьютекс сессии для атомарного find-or-spawn / restart (H5/H15).
    async fn session_lock(&self, session_id: Id) -> Arc<Mutex<()>> {
        let mut locks = self.spawn_locks.lock().await;
        locks
            .entry(session_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Смотритель агент-таски: по завершении (Ok/Err/паника) удаляет запись
    /// из agents — иначе send падает навсегда и restart недостижим (C10).
    fn spawn_watcher(
        &self,
        agent_id: Id,
        spawn_id: Uuid,
        handle: JoinHandle<vpsagent_core::Result<()>>,
    ) {
        let agents = self.agents.clone();
        tokio::spawn(async move {
            match handle.await {
                Ok(Ok(())) => tracing::debug!(%agent_id, "агент завершился"),
                Ok(Err(e)) => tracing::warn!(%agent_id, error=?e, "агент завершился с ошибкой"),
                Err(e) => tracing::warn!(%agent_id, error=?e, "агент-таска паниковала/отменена"),
            }
            let mut agents = agents.lock().await;
            // Удаляем только свою запись: могли уже перезапустить агента (restart_agent).
            if agents.get(&agent_id).map(|r| r.spawn_id) == Some(spawn_id) {
                agents.remove(&agent_id);
            }
        });
    }

    /// Создать сессию (без агента; агент спавнится при первом сообщении).
    pub async fn create_session(&self, cwd: &str, model: Option<&str>) -> anyhow::Result<Session> {
        self.storage.create_session(cwd, model).await
    }

    /// Список сессий.
    pub async fn list_sessions(&self) -> anyhow::Result<Vec<Session>> {
        self.storage.list_sessions().await
    }

    /// Отправить сообщение в сессию. Если агента нет — спавним главного.
    /// Обрабатывает команды мультипотока (/tell, /new, /steer) через классификатор.
    /// Возвращает agent_id (для UI).
    pub async fn send_message(&self, session_id: Id, text: String) -> anyhow::Result<Id> {
        // Команды мультипотока.
        if vpsagent_runtime::is_multiplex_command(&text) {
            let messages = self
                .storage
                .list_messages(session_id)
                .await
                .unwrap_or_default();
            let route = vpsagent_runtime::classify(&text, &messages);
            return self.handle_multiplex(session_id, route).await;
        }

        // Per-session мьютекс: find-or-spawn и restart атомарны (H5/H15).
        let session_lock = self.session_lock(session_id).await;
        let _guard = session_lock.lock().await;

        // Найти или создать агента для сессии.
        let agent_id = {
            let agents = self.storage.list_agents(session_id).await?;
            agents
                .into_iter()
                .find(|a| a.kind == vpsagent_core::AgentKind::Main)
                .map(|a| a.id)
        };
        let agent_id = match agent_id {
            Some(id) => id,
            None => self.spawn_main_agent(session_id).await?,
        };

        // Доставить сообщение в inbox агента. Если receiver дропнут (запись
        // протухла — таска умерла, watcher ещё не сработал) — перезапуск (C10).
        let need_restart = {
            let agents = self.agents.lock().await;
            match agents.get(&agent_id) {
                Some(running) => running.inbox.send(text.clone()).is_err(),
                None => true,
            }
        };
        if need_restart {
            self.restart_agent(session_id, agent_id, text).await?;
        }
        Ok(agent_id)
    }

    /// Обработать команду мультипотока.
    async fn handle_multiplex(
        &self,
        session_id: Id,
        route: vpsagent_runtime::Route,
    ) -> anyhow::Result<Id> {
        use vpsagent_runtime::Route;
        match route {
            Route::ToAgent(target) => {
                let agents = self.agents.lock().await;
                if let Some(running) = agents.get(&target) {
                    // Отправим заглушку (реальный текст команды — через MessageSend).
                    let _ = running.inbox.send(String::new());
                }
                Ok(target)
            }
            Route::SpawnNew { task } => {
                // Спавн главного с новой задачей (в Фазе 7 — новый субагент).
                let session_lock = self.session_lock(session_id).await;
                let _guard = session_lock.lock().await;
                self.spawn_main_agent_with_task(session_id, task).await
            }
            Route::Steer(id, _msg) => {
                let agents = self.agents.lock().await;
                if let Some(running) = agents.get(&id) {
                    let _ = running.inbox.send(String::new());
                }
                Ok(id)
            }
            Route::Main => {
                // Find-or-spawn под мьютексом сессии (H5): иначе два Main.
                let session_lock = self.session_lock(session_id).await;
                let _guard = session_lock.lock().await;
                let existing = self
                    .storage
                    .list_agents(session_id)
                    .await?
                    .into_iter()
                    .find(|a| a.kind == vpsagent_core::AgentKind::Main)
                    .map(|a| a.id);
                match existing {
                    Some(id) => Ok(id),
                    None => self.spawn_main_agent(session_id).await,
                }
            }
        }
    }

    /// Спавн главного агента с первым сообщением (для /new).
    async fn spawn_main_agent_with_task(&self, session_id: Id, task: String) -> anyhow::Result<Id> {
        let agent_id = self.spawn_main_agent(session_id).await?;
        // Доставить задачу в inbox напрямую (без рекурсии через send_message).
        let agents = self.agents.lock().await;
        if let Some(running) = agents.get(&agent_id) {
            running.inbox.send(task).ok();
        }
        Ok(agent_id)
    }

    /// Прервать агента.
    pub async fn interrupt_agent(&self, agent_id: Id) -> anyhow::Result<()> {
        let mut agents = self.agents.lock().await;
        if let Some(running) = agents.get_mut(&agent_id) {
            running.cancel.cancel();
        }
        Ok(())
    }

    /// Спавн главного агента сессии (с пустым inbox; ждёт первого сообщения).
    async fn spawn_main_agent(&self, session_id: Id) -> anyhow::Result<Id> {
        let agent_id = Uuid::new_v4();
        let model = self
            .storage
            .get_session(session_id)
            .await?
            .and_then(|s| s.model)
            .unwrap_or_else(|| self.config.default_model.clone());
        // Inbox: сначала отправим первое сообщение отдельно (см. send_message).
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel::<String>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<EventKind>();
        let cancel = CancellationToken::new();

        // Forwarder: EventKind → EventBus + SQLite.
        spawn_forwarder(self.bus.clone(), self.storage.clone(), session_id, event_rx);

        let endpoint = self
            .config
            .endpoint_for_model(&model)
            .ok_or_else(|| anyhow::anyhow!("модель '{model}' не найдена в конфиге"))?;
        let api_key = self.config.secret(&endpoint.api_key_ref)?;
        // Единая фабрика провайдера (OpenAI-compat / Responses / Anthropic).
        let provider: Arc<dyn Provider> = vpsagent_providers::make_provider(endpoint, &api_key);

        // cwd из сессии.
        let session = self
            .storage
            .get_session(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("сессия {session_id} не найдена"))?;
        let cwd = PathBuf::from(&session.cwd);

        let permissions = Permissions {
            mode: self.config.permission_mode,
            allow: vec![],
            deny: vec![],
        };

        let params = AgentParams {
            session_id,
            agent_id,
            model: model.clone(),
            provider_name: endpoint.name.clone(),
            cwd,
            provider,
            storage: self.storage.clone(),
            event_tx,
            inbox: inbox_rx,
            cancel: cancel.clone(),
            permissions,
            subagents: self.subagents.clone(),
            initial_history: vec![],
            allowed_tools: None,
            def_prompt: None,
            pending: self.pending.clone(),
            pending_requests: self.pending_requests.clone(),
        };
        let handle = tokio::spawn(run_agent(params));
        let spawn_id = Uuid::new_v4();
        self.agents.lock().await.insert(
            agent_id,
            RunningAgent {
                inbox: inbox_tx,
                cancel,
                spawn_id,
            },
        );
        // Смотритель: по завершении таски убрать запись (C10).
        self.spawn_watcher(agent_id, spawn_id, handle);
        Ok(agent_id)
    }

    /// Перезапуск агента (если завершился, а пользователь снова прислал сообщение).
    async fn restart_agent(
        &self,
        session_id: Id,
        agent_id: Id,
        first_message: String,
    ) -> anyhow::Result<()> {
        // Простой вариант: новый агент-таска с тем же agent_id, первое сообщение в inbox.
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel::<String>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<EventKind>();
        let cancel = CancellationToken::new();
        spawn_forwarder(self.bus.clone(), self.storage.clone(), session_id, event_rx);

        let model = self
            .storage
            .get_session(session_id)
            .await?
            .and_then(|s| s.model)
            .unwrap_or_else(|| self.config.default_model.clone());
        let endpoint = self
            .config
            .endpoint_for_model(&model)
            .ok_or_else(|| anyhow::anyhow!("модель '{model}' не найдена"))?;
        let api_key = self.config.secret(&endpoint.api_key_ref)?;
        let provider: Arc<dyn Provider> = vpsagent_providers::make_provider(endpoint, &api_key);
        let session = self
            .storage
            .get_session(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("сессия не найдена"))?;
        let cwd = PathBuf::from(&session.cwd);
        let permissions = Permissions {
            mode: self.config.permission_mode,
            allow: vec![],
            deny: vec![],
        };
        // Отправим первое сообщение в inbox до старта (чтобы agent_loop получил его через recv).
        inbox_tx.send(first_message).ok();
        let params = AgentParams {
            session_id,
            agent_id,
            model,
            provider_name: endpoint.name.clone(),
            cwd,
            provider,
            storage: self.storage.clone(),
            event_tx,
            inbox: inbox_rx,
            cancel: cancel.clone(),
            permissions,
            subagents: self.subagents.clone(),
            initial_history: vec![],
            allowed_tools: None,
            def_prompt: None,
            pending: self.pending.clone(),
            pending_requests: self.pending_requests.clone(),
        };
        let handle = tokio::spawn(run_agent(params));
        let spawn_id = Uuid::new_v4();
        {
            // Remove+insert под одним захватом: протухшая запись не уходит
            // из-под замка между операциями (H15).
            let mut agents = self.agents.lock().await;
            agents.remove(&agent_id);
            agents.insert(
                agent_id,
                RunningAgent {
                    inbox: inbox_tx,
                    cancel,
                    spawn_id,
                },
            );
        }
        self.spawn_watcher(agent_id, spawn_id, handle);
        Ok(())
    }

    /// Graceful shutdown: отменить всех агентов и субагентов (H24).
    pub async fn shutdown_all(&self) {
        for info in self.subagents.list().await {
            self.subagents.interrupt(info.id).await;
        }
        let agents = self.agents.lock().await;
        for (id, running) in agents.iter() {
            tracing::debug!(%id, "отмена агента при shutdown");
            running.cancel.cancel();
        }
    }

    /// Список агентов сессии (для deck-обзора / agent.list).
    pub async fn list_agents(
        &self,
        session_id: Id,
    ) -> anyhow::Result<Vec<vpsagent_core::AgentInfo>> {
        self.storage.list_agents(session_id).await
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
}

/// Оставить only для типов.
#[allow(dead_code)]
fn _ensure_mode() {
    let _ = PermissionMode::Default;
}
