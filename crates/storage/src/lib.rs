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