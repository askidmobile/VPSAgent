//! `vpsagent-daemon`: headless-демон.
//!
//! Singleton (flock), JSON-RPC сервер на Unix-сокете, SessionManager
//! (агент-таски tokio), EventBus (pub/sub событий).

pub mod broadcast;
pub mod client;
pub mod daemonize;
pub mod lock;
pub mod server;
pub mod session_manager;

pub use broadcast::EventBus;
pub use client::RpcClient;
pub use daemonize::Daemonize;
pub use lock::DaemonLock;
pub use server::RpcServer;
pub use session_manager::SessionManager;

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use vpsagent_core::{Config, Result};
use vpsagent_storage::Storage;

/// Запустить демон (блокирует). Вызывается из бинаря `vpsagent daemon`.
///
/// Сама `run` НЕ делает daemonize — это обязанность caller'а (бинаря), чтобы
/// разделить «отсоединение от терминала» (daemonize) и «работа демона» (run).
/// Бинарь вызывает [`crate::daemonize::daemonize`] перед `run` при необходимости.
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

    // 4. Shutdown-токен: SIGINT/SIGTERM и RPC-команда DaemonStop (FR-006) отменяют
    //    его, запуская единый graceful shutdown-путь.
    let shutdown = CancellationToken::new();

    // 5. RPC-сервер.
    let server = Arc::new(RpcServer::new(
        manager.clone(),
        storage,
        bus,
        lock.pid(),
        shutdown.clone(),
    ));

    // H24: graceful shutdown по SIGINT/SIGTERM или RPC DaemonStop.
    #[cfg(unix)]
    let shutdown_signal = async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        // SIGHUP игнорируется на уровне daemonize (SIG_IGN); здесь его не ловим.
        tokio::select! {
            res = tokio::signal::ctrl_c() => {
                if let Err(e) = res {
                    tracing::warn!(error=?e, "ctrl_c handler failed");
                }
            }
            _ = term.recv() => {}
            _ = shutdown.cancelled() => {
                tracing::info!("получен RPC-запрос DaemonStop");
            }
        }
    };
    #[cfg(not(unix))]
    let shutdown_signal = async {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = shutdown.cancelled() => {
                tracing::info!("получен RPC-запрос DaemonStop");
            }
        }
    };

    tokio::select! {
        res = server.serve(&socket_path) => {
            res.map_err(vpsagent_core::Error::Other)?;
        }
        _ = shutdown_signal => {
            tracing::info!("останавливаем демон");
            shutdown.cancel();
        }
    }

    // Чистое завершение: отменить всех агентов, закрыть сокет.
    manager.shutdown_all().await;
    std::fs::remove_file(&socket_path).ok();
    tracing::info!("демон остановлен");
    Ok(())
}
