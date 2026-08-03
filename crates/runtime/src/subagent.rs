//! SubagentManager: спавн и мониторинг субагентов (tokio-таски).
//!
//! Субагент — изолированная tokio-таска `run_agent` со своим контекстом
//! (по умолчанию пустым; при `fork=true` — копия контекста родителя) и
//! суженным набором инструментов. Лимит глубины вложенности — 3.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vpsagent_core::{AgentInfo, AgentKind, AgentStatus, Config, EventKind, Id, TokenUsage};
use vpsagent_providers::Provider;
use vpsagent_storage::Storage;

use crate::agent_loop::run_agent;
use crate::definition::{AgentDefinition, DefinitionRegistry};
use crate::pending::{PendingPermissions, PendingRequests};
use crate::permissions::Permissions;

/// Запущенный субагент.
pub struct SubAgentHandle {
    pub info: AgentInfo,
    inbox: mpsc::UnboundedSender<String>,
    cancel: CancellationToken,
    #[allow(dead_code)]
    handle: JoinHandle<vpsagent_core::Result<()>>,
    /// Накопленный вывод (для task_output).
    pub output: Arc<tokio::sync::Mutex<String>>,
}

impl SubAgentHandle {
    pub fn send(&self, msg: String) -> bool {
        self.inbox.send(msg).is_ok()
    }
    pub fn interrupt(&self) {
        self.cancel.cancel();
    }
}

/// Менеджер субагентов (разделяемый).
pub struct SubagentManager {
    storage: Storage,
    config: Arc<Config>,
    definitions: DefinitionRegistry,
    /// Доп. определения (встроенные критики и т.п.), регистрируются в runtime.
    extra_defs: std::sync::Mutex<std::collections::HashMap<String, AgentDefinition>>,
    /// agent_id → handle.
    agents: Arc<Mutex<HashMap<Id, Arc<SubAgentHandle>>>>,
    /// Реестр ожидающих разрешений (C1), общий с демоном.
    pending: PendingPermissions,
    /// Реестр ожидающих ответов ask_user (C15), общий с демоном.
    pending_requests: PendingRequests,
}

impl SubagentManager {
    pub fn new(storage: Storage, config: Arc<Config>) -> Self {
        Self {
            storage,
            config,
            definitions: DefinitionRegistry::load_defaults(),
            extra_defs: Default::default(),
            agents: Default::default(),
            pending: PendingPermissions::new(),
            pending_requests: PendingRequests::new(),
        }
    }

    /// Подключить общий реестр разрешений (от демона/сессии).
    pub fn with_pending(mut self, pending: PendingPermissions) -> Self {
        self.pending = pending;
        self
    }

    /// Подключить общий реестр ask_user (от демона/сессии).
    pub fn with_pending_requests(mut self, pending_requests: PendingRequests) -> Self {
        self.pending_requests = pending_requests;
        self
    }

    pub fn pending(&self) -> &PendingPermissions {
        &self.pending
    }

    pub fn definitions(&self) -> &DefinitionRegistry {
        &self.definitions
    }

    /// Временно зарегистрировать определение (для встроенных критиков и т.п.).
    pub fn register_definition(&self, def: AgentDefinition) {
        // Мутируем через unsafe/Arc<RwLock>? Реестр внутри Arc, не мутируемый.
        // Вместо этого держим доп. слот в отдельном Arc<Mutex<HashMap>>.
        // Упрощение: используем отдельный runtime-реестр.
        self.extra_defs
            .lock()
            .unwrap()
            .insert(def.name.clone(), def);
    }

    pub fn get_definition(&self, name: &str) -> Option<AgentDefinition> {
        if let Some(d) = self.definitions.get(name) {
            return Some(d.clone());
        }
        self.extra_defs.lock().unwrap().get(name).cloned()
    }

    /// Глубина вложенности агента (0 — главный). Считаем через БД по цепочке parent_id.
    pub async fn depth_of(&self, agent_id: Id, session_id: Id) -> usize {
        let mut depth = 0usize;
        let mut current = Some(agent_id);
        let agents = self
            .storage
            .list_agents(session_id)
            .await
            .unwrap_or_default();
        while let Some(id) = current {
            if depth >= 3 {
                return 3;
            }
            let parent = agents.iter().find(|a| a.id == id).and_then(|a| a.parent_id);
            match parent {
                Some(p) => {
                    depth += 1;
                    current = Some(p);
                }
                None => break,
            }
        }
        depth
    }

    /// Спавн субагента из определения.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        &self,
        session_id: Id,
        parent_agent_id: Id,
        def_name: &str,
        task: String,
        fork: bool,
        fork_context: Vec<vpsagent_core::Message>,
        event_tx: mpsc::UnboundedSender<EventKind>,
    ) -> anyhow::Result<Id> {
        let def = self
            .get_definition(def_name)
            .ok_or_else(|| anyhow::anyhow!("субагент '{def_name}' не найден"))?;
        let depth = self.depth_of(parent_agent_id, session_id).await;
        if depth >= 3 {
            anyhow::bail!("превышена глубина вложенности субагентов (max 3)");
        }

        let agent_id = Uuid::new_v4();
        let model = def
            .model
            .clone()
            .unwrap_or_else(|| self.config.default_model.clone());
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel::<String>();
        let cancel = CancellationToken::new();
        let output = Arc::new(tokio::sync::Mutex::new(String::new()));
        // Первое сообщение субагента — его задача.
        let _ = inbox_tx.send(task);

        // Провайдер для субагента.
        let endpoint = self
            .config
            .endpoint_for_model(&model)
            .ok_or_else(|| anyhow::anyhow!("модель '{model}' не найдена"))?;
        let api_key = self.config.secret(&endpoint.api_key_ref)?;
        // Единая фабрика провайдера (поддерживает OpenAI-compat / Responses / Anthropic).
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

        // Регистрируем в БД до старта.
        let info = AgentInfo {
            id: agent_id,
            session_id,
            name: def.name.clone(),
            kind: AgentKind::Sub,
            status: AgentStatus::Working,
            last_action: "старт".into(),
            tokens: TokenUsage::default(),
            model: model.clone(),
            parent_id: Some(parent_agent_id),
        };
        self.storage.upsert_agent(&info).await?;
        let _ = event_tx.send(EventKind::AgentSpawned(info.clone()));

        let handle = tokio::spawn(run_agent(crate::agent_loop::AgentParams {
            session_id,
            agent_id,
            model,
            cwd,
            provider,
            storage: self.storage.clone(),
            event_tx,
            inbox: inbox_rx,
            cancel: cancel.clone(),
            permissions,
            subagents: self.clone(),
            // fork-контекст передаём через отдельный механизм.
            // Пока в Фазе 2 — субагент стартует с пустой историей + task-сообщение.
            initial_history: if fork { fork_context } else { vec![] },
            def_prompt: Some(def.prompt.clone()),
            allowed_tools: if def.tools.is_empty() {
                None
            } else {
                Some(def.tools.clone())
            },
            pending: self.pending.clone(),
            pending_requests: self.pending_requests.clone(),
        }));

        let handle = Arc::new(SubAgentHandle {
            info,
            inbox: inbox_tx,
            cancel,
            handle,
            output,
        });
        self.agents.lock().await.insert(agent_id, handle);
        Ok(agent_id)
    }

    /// Список активных субагентов (для task_list и deck).
    pub async fn list(&self) -> Vec<AgentInfo> {
        let agents = self.agents.lock().await;
        agents.values().map(|h| h.info.clone()).collect()
    }

    pub async fn get(&self, id: Id) -> Option<Arc<SubAgentHandle>> {
        self.agents.lock().await.get(&id).cloned()
    }

    pub async fn send_message(&self, id: Id, msg: String) -> bool {
        if let Some(h) = self.get(id).await {
            h.send(msg)
        } else {
            false
        }
    }

    pub async fn interrupt(&self, id: Id) {
        if let Some(h) = self.get(id).await {
            h.interrupt();
        }
    }

    pub async fn output(&self, id: Id) -> String {
        if let Some(h) = self.get(id).await {
            h.output.lock().await.clone()
        } else {
            String::new()
        }
    }

    pub async fn append_output(&self, id: Id, text: &str) {
        if let Some(h) = self.get(id).await {
            let mut out = h.output.lock().await;
            if out.len() > 64 * 1024 {
                out.clear();
            }
            out.push_str(text);
        }
    }
}

impl Clone for SubagentManager {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            config: self.config.clone(),
            definitions: DefinitionRegistry {
                defs: self.definitions.defs.clone(),
            },
            extra_defs: Default::default(),
            agents: self.agents.clone(),
            pending: self.pending.clone(),
            pending_requests: self.pending_requests.clone(),
        }
    }
}
