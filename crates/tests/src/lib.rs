//! Интеграционные тесты VPSAgent.
//!
//! Покрывают ключевые свойства Фазы 1:
//! - SQLite-хранилище (создание сессии, запись/чтение сообщений, events append/replay)
//! - Singleton-lock (acquire → второй раз None)
//! - RPC клиент-сервер на Unix-сокете (SessionCreate → SessionList → SessionAttach)

mod daemon_rpc;
mod phase8;
mod runtime;
mod skills;
mod storage;
