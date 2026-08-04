//! Репозиторий агентов.

use chrono::Utc;
use uuid::Uuid;
use vpsagent_core::{AgentInfo, AgentKind, AgentStatus, Id, TokenUsage};

use crate::Storage;

impl Storage {
    pub async fn upsert_agent(&self, agent: &AgentInfo) -> anyhow::Result<()> {
        let id = agent.id.to_string();
        let session_id = agent.session_id.to_string();
        let parent_id = agent.parent_id.map(|i| i.to_string());
        let kind = match agent.kind {
            AgentKind::Main => "main",
            AgentKind::Sub => "sub",
        };
        let status = match agent.status {
            AgentStatus::Working => "working",
            AgentStatus::Waiting => "waiting",
            AgentStatus::Done => "done",
            AgentStatus::Failed => "failed",
            AgentStatus::Interrupted => "interrupted",
        };
        let tokens_in = agent.tokens.input as i64;
        let tokens_out = agent.tokens.output as i64;
        let model = agent.model.clone();
        let name = agent.name.clone();
        let last_action = agent.last_action.clone();
        let provider = agent.provider.clone();
        let created = Utc::now().to_rfc3339();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO agents (id, session_id, parent_id, name, kind, status, last_action,
                                     tokens_input, tokens_output, model, provider, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(id) DO UPDATE SET
                    status=excluded.status,
                    last_action=excluded.last_action,
                    tokens_input=excluded.tokens_input,
                    tokens_output=excluded.tokens_output,
                    provider=excluded.provider",
                rusqlite::params![
                    id,
                    session_id,
                    parent_id,
                    name,
                    kind,
                    status,
                    last_action,
                    tokens_in,
                    tokens_out,
                    model,
                    provider,
                    created
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn list_agents(&self, session_id: Id) -> anyhow::Result<Vec<AgentInfo>> {
        self.call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, parent_id, name, kind, status, last_action,
                        tokens_input, tokens_output, model, provider
                 FROM agents WHERE session_id = ?1 ORDER BY created_at",
            )?;
            let rows = stmt.query_map(rusqlite::params![session_id.to_string()], |row| {
                let id: String = row.get(0)?;
                let sid: String = row.get(1)?;
                let parent_id: Option<String> = row.get(2)?;
                let kind: String = row.get(4)?;
                let status: String = row.get(5)?;
                let tokens_in: i64 = row.get(7).unwrap_or(0);
                let tokens_out: i64 = row.get(8).unwrap_or(0);
                Ok(AgentInfo {
                    id: Uuid::parse_str(&id).unwrap_or_default(),
                    session_id: Uuid::parse_str(&sid).unwrap_or_default(),
                    parent_id: parent_id.and_then(|s| Uuid::parse_str(&s).ok()),
                    name: row.get(3)?,
                    kind: if kind == "main" {
                        AgentKind::Main
                    } else {
                        AgentKind::Sub
                    },
                    status: match status.as_str() {
                        "working" => AgentStatus::Working,
                        "done" => AgentStatus::Done,
                        "failed" => AgentStatus::Failed,
                        "interrupted" => AgentStatus::Interrupted,
                        _ => AgentStatus::Waiting,
                    },
                    last_action: row.get(6)?,
                    tokens: TokenUsage {
                        input: tokens_in as u32,
                        output: tokens_out as u32,
                    },
                    model: row.get(9)?,
                    provider: row.get(10).unwrap_or_default(),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(tokio_rusqlite::Error::from)
        })
        .await
    }
}
