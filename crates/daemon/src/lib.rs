//! `vpsagent-daemon`: headless-демон.
//!
//! Singleton (flock), JSON-RPC сервер на Unix-сокете, SessionManager
//! (агент-таски tokio), EventBus (pub/sub событий).

pub mod broadcast;
pub mod client;
pub mod lock;
pub mod session_manager;
pub mod server;

pub use broadcast::EventBus;
pub use client::RpcClient;
pub use lock::DaemonLock;
pub use session_manager::SessionManager;
pub use server::RpcServer;

use std::sync::Arc;

use vpsagent_core::{Config, Result};
use vpsagent_storage::Storage;

/// Запустить демон (блокирует). Вызывается из бинаря `vpsagent daemon`.
pub async fn run(config: Config) -> Result<()> {
    let socket_path = config.paths.socket.clone();
    // 1. Singleton-lock.
    let lock_path = socket_path.with_extension("lock");
    let lock = DaemonLock::acquire(&lock_path)?;
    let lock = match lock {
        Some(l) => l,
        None => {
            return Err(vpsagent_core::Error::Other(anyhow::anyhow!(
                "демон уже запущен (lock занят): {}",
                lock_path.display()
            )));
        }
    };
    tracing::info!(pid = lock.pid(), "запуск демона");

    // 2. Хранилище.
    std::fs::create_dir_all(&config.paths.data_dir).ok();
    let storage = Storage::open(&config.paths.db)
        .await
        .map_err(|e| vpsagent_core::Error::Storage(e.to_string()))?;

    // 3. Менеджер сессий + EventBus.
    let config = Arc::new(config);
    let bus = EventBus::new();
    let manager = SessionManager::new(storage.clone(), config.clone(), bus.clone());

    // 4. RPC-сервер.
    let server = Arc::new(RpcServer::new(manager.clone(), storage, bus, lock.pid()));

    // H24: graceful shutdown по SIGINT/SIGTERM.
    #[cfg(unix)]
    let shutdown = async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            res = tokio::signal::ctrl_c() => {
                if let Err(e) = res {
                    tracing::warn!(error=?e, "ctrl_c handler failed");
                }
            }
            _ = term.recv() => {}
        }
    };
    #[cfg(not(unix))]
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    tokio::select! {
        res = server.serve(&socket_path) => {
            res.map_err(vpsagent_core::Error::Other)?;
        }
        _ = shutdown => {
            tracing::info!("получен сигнал завершения: останавливаем демон");
        }
    }

    // Чистое завершение: отменить всех агентов, закрыть сокет.
    manager.shutdown_all().await;
    std::fs::remove_file(&socket_path).ok();
    tracing::info!("демон остановлен");
    Ok(())
}