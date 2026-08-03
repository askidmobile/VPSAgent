//! Репозиторий сессий.

use chrono::Utc;
use uuid::Uuid;
use vpsagent_core::{Id, Session, SessionStatus};

use crate::Storage;

impl Storage {
    pub async fn create_session(&self, cwd: &str, model: Option<&str>) -> anyhow::Result<Session> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let title = format!("Session {}", now.format("%Y-%m-%d %H:%M"));
        let cwd = cwd.to_string();
        let model = model.map(String::from);
        let title_for_closure = title.clone();
        let cwd_for_closure = cwd.clone();
        let model_for_closure = model.clone();
        let created_at = now.to_rfc3339();
        let updated_at = now.to_rfc3339();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO sessions (id, title, cwd, model, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    id.to_string(),
                    &title_for_closure,
                    &cwd_for_closure,
                    model_for_closure.as_deref(),
                    "active",
                    &created_at,
                    &updated_at,
                ],
            )?;
            Ok(())
        })
        .await?;
        Ok(Session {
            id,
            title,
            cwd,
            model,
            status: SessionStatus::Active,
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn list_sessions(&self) -> anyhow::Result<Vec<Session>> {
        self.call(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, title, cwd, model, status, created_at, updated_at
                 FROM sessions ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let created: String = row.get(5)?;
                let updated: String = row.get(6)?;
                Ok(Session {
                    id: Uuid::parse_str(&id).unwrap_or_default(),
                    title: row.get(1)?,
                    cwd: row.get(2)?,
                    model: row.get(3)?,
                    status: match row.get::<_, String>(4)?.as_str() {
                        "archived" => SessionStatus::Archived,
                        _ => SessionStatus::Active,
                    },
                    created_at: chrono::DateTime::parse_from_rfc3339(&created)
                        .map(|d| d.with_timezone(&Utc))
                        .unwrap_or(now()),
                    updated_at: chrono::DateTime::parse_from_rfc3339(&updated)
                        .map(|d| d.with_timezone(&Utc))
                        .unwrap_or(now()),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(tokio_rusqlite::Error::from)
        })
        .await
    }

    pub async fn get_session(&self, id: Id) -> anyhow::Result<Option<Session>> {
        self.call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, title, cwd, model, status, created_at, updated_at
                 FROM sessions WHERE id = ?1",
            )?;
            let mut rows = stmt.query_map(rusqlite::params![id.to_string()], |row| {
                let created: String = row.get(5)?;
                let updated: String = row.get(6)?;
                Ok(Session {
                    id,
                    title: row.get(1)?,
                    cwd: row.get(2)?,
                    model: row.get(3)?,
                    status: match row.get::<_, String>(4)?.as_str() {
                        "archived" => SessionStatus::Archived,
                        _ => SessionStatus::Active,
                    },
                    created_at: chrono::DateTime::parse_from_rfc3339(&created)
                        .map(|d| d.with_timezone(&Utc))
                        .unwrap_or(now()),
                    updated_at: chrono::DateTime::parse_from_rfc3339(&updated)
                        .map(|d| d.with_timezone(&Utc))
                        .unwrap_or(now()),
                })
            })?;
            rows.next().transpose().map_err(tokio_rusqlite::Error::from)
        })
        .await
    }
}

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}
