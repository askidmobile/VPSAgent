//! `vpsagent-storage`: SQLite-хранилище сессий, сообщений, событий, агентов.
//!
//! Асинхронный доступ через `tokio-rusqlite` (one connection, call-паттерн).

pub mod schema;

mod repos;

use std::path::Path;

use tokio_rusqlite::Connection;

/// Хранилище: обёртка над одной асинхронной SQLite-коннекцией.
#[derive(Clone)]
pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// Открыть/создать базу по пути и применить миграции.
    pub async fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let conn = Connection::open(path).await?;
        conn.call(|c| {
            c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
            for sql in schema::MIGRATIONS {
                c.execute_batch(sql)?;
            }
            // Миграция v0.2.x → v0.3.0: колонка provider в agents.
            // ALTER TABLE ADD COLUMN IF NOT EXISTS нет в SQLite — пробуем, игнорируем ошибку.
            let _ =
                c.execute_batch("ALTER TABLE agents ADD COLUMN provider TEXT NOT NULL DEFAULT ''");
            Ok(())
        })
        .await?;
        Ok(Self { conn })
    }

    /// Выполнить замыкание на raw-коннекции (внутренний API).
    /// Замыкание должно возвращать `tokio_rusqlite::Result<T>` (т.е. `Result<T, tokio_rusqlite::Error>`).
    pub(crate) async fn call<F, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&mut rusqlite::Connection) -> tokio_rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        Ok(self.conn.call(f).await?)
    }
}
