//! Репозиторий сообщений и событий.

use chrono::Utc;
use serde_json::from_str;
use uuid::Uuid;
use vpsagent_core::{
    ContentBlock, Event, EventKind, Id, Message, Role, TokenUsage,
};

use crate::Storage;

impl Storage {
    pub async fn insert_message(&self, msg: &Message) -> anyhow::Result<()> {
        let id = msg.id.to_string();
        let session_id = msg.session_id.to_string();
        let agent_id = msg.agent_id.map(|i| i.to_string());
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let content = serde_json::to_string(&msg.content)?;
        let tokens_in = msg.tokens.map(|t| t.input as i64);
        let tokens_out = msg.tokens.map(|t| t.output as i64);
        let created = msg.created_at.to_rfc3339();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO messages (id, session_id, agent_id, role, content, tokens_input, tokens_output, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![id, session_id, agent_id, role, content, tokens_in, tokens_out, created],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn list_messages(&self, session_id: Id) -> anyhow::Result<Vec<Message>> {
        self.call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, agent_id, role, content, tokens_input, tokens_output, created_at
                 FROM messages WHERE session_id = ?1 ORDER BY created_at",
            )?;
            let rows = stmt.query_map(rusqlite::params![session_id.to_string()], |row| {
                let id: String = row.get(0)?;
                let sid: String = row.get(1)?;
                let agent_id: Option<String> = row.get(2)?;
                let role: String = row.get(3)?;
                let content: String = row.get(4)?;
                let tokens_in: Option<i64> = row.get(5)?;
                let tokens_out: Option<i64> = row.get(6)?;
                let created: String = row.get(7)?;
                let blocks: Vec<ContentBlock> = from_str(&content).unwrap_or_default();
                Ok(Message {
                    id: Uuid::parse_str(&id).unwrap_or_default(),
                    session_id: Uuid::parse_str(&sid).unwrap_or_default(),
                    agent_id: agent_id.and_then(|s| Uuid::parse_str(&s).ok()),
                    role: match role.as_str() {
                        "user" => Role::User,
                        "tool" => Role::Tool,
                        _ => Role::Assistant,
                    },
                    content: blocks,
                    tokens: tokens_in.zip(tokens_out).map(|(i, o)| TokenUsage {
                        input: i as u32,
                        output: o as u32,
                    }),
                    created_at: chrono::DateTime::parse_from_rfc3339(&created)
                        .map(|d| d.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(tokio_rusqlite::Error::from)
        })
        .await
    }

    /// Записать событие и вернуть его с присвоенным seq.
    pub async fn append_event(
        &self,
        session_id: Id,
        agent_id: Option<Id>,
        kind: EventKind,
    ) -> anyhow::Result<Event> {
        let payload = serde_json::to_value(&kind)?;
        let payload_str = serde_json::to_string(&payload)?;
        let kind_str = serde_json::to_string(&payload["kind"])?;
        let created = Utc::now().to_rfc3339();
        let created_for_closure = created.clone();
        let session_id_str = session_id.to_string();
        let agent_id_str = agent_id.map(|i| i.to_string());
        let seq: i64 = self
            .call(move |conn| {
                // seq = max+1 в рамках сессии (атомарно в транзакции).
                let tx = conn.transaction()?;
                let max: i64 = tx
                    .query_row(
                        "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1",
                        rusqlite::params![session_id_str],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                let seq = max + 1;
                tx.execute(
                    "INSERT INTO events (session_id, agent_id, seq, kind, payload, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![session_id_str, agent_id_str, seq, kind_str, payload_str, created_for_closure],
                )?;
                tx.commit()?;
                Ok(seq)
            })
            .await?;
        Ok(Event {
            seq: seq as u64,
            session_id,
            kind,
            created_at: chrono::DateTime::parse_from_rfc3339(&created)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        })
    }

    /// События сессии начиная с last_seq+1 (для replay при attach).
    pub async fn events_since(
        &self,
        session_id: Id,
        last_seq: u64,
    ) -> anyhow::Result<Vec<Event>> {
        self.call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT session_id, agent_id, seq, kind, payload, created_at
                 FROM events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![session_id.to_string(), last_seq as i64],
                |row| {
                    let sid: String = row.get(0)?;
                    let _agent_id: Option<String> = row.get(1)?;
                    let seq: i64 = row.get(2)?;
                    let payload: String = row.get(4)?;
                    let created: String = row.get(5)?;
                    let kind: EventKind =
                        serde_json::from_str(&payload).unwrap_or(EventKind::Error {
                            agent_id: None,
                            message: "битое событие".into(),
                        });
                    Ok(Event {
                        seq: seq as u64,
                        session_id: Uuid::parse_str(&sid).unwrap_or_default(),
                        kind,
                        created_at: chrono::DateTime::parse_from_rfc3339(&created)
                            .map(|d| d.with_timezone(&Utc))
                            .unwrap_or_else(|_| Utc::now()),
                    })
                },
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(tokio_rusqlite::Error::from)
        })
        .await
    }
}