//! Тесты демона: singleton-lock и RPC клиент-сервер на Unix-сокете.
//!
//! Импорты и хелперы используются в телах `#[test]`-функций; под целью `--lib`
//! (где `cfg(test)=false` и тела вырезаются) это даёт ложные unused-imports.
//! `#[allow(...)]` подавляет их, не влияя на `--tests`.

#![allow(unused_imports)]

use std::sync::Arc;

use vpsagent_core::{Config, EventKind, Request, Response};
use vpsagent_daemon::{DaemonLock, EventBus, RpcServer, SessionManager};
use vpsagent_storage::Storage;

#[allow(dead_code)]
fn tmp_socket_path() -> std::path::PathBuf {
    // Unix-сокет лимит ~104 символа; temp_dir на macOS длинный.
    // Используем /tmp с коротким именем.
    std::path::PathBuf::from(format!("/tmp/vp-{}.sock", short_id()))
}

#[allow(dead_code)]
fn short_id() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{nanos:x}")
}

#[allow(dead_code)]
fn tmp_db_path() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(format!("/tmp/vp-{}", short_id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

#[test]
fn singleton_lock_acquire_then_none() {
    let path = std::path::PathBuf::from(format!("/tmp/vp-{}.lock", short_id()));
    let _ = std::fs::remove_file(&path);
    let lock1 = DaemonLock::acquire(&path).unwrap();
    assert!(lock1.is_some(), "первый acquire должен быть Some");
    let lock2 = DaemonLock::acquire(&path).unwrap();
    assert!(
        lock2.is_none(),
        "второй acquire должен быть None (демон уже работает)"
    );
    drop(lock1);
    // После drop файл удалён — новый acquire должен быть Some.
    let lock3 = DaemonLock::acquire(&path).unwrap();
    assert!(lock3.is_some(), "после drop — снова Some");
    drop(lock3);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn rpc_session_create_list_attach() {
    let socket = tmp_socket_path();
    let db = tmp_db_path();
    let _ = std::fs::remove_file(&socket);

    // Конфиг с тестовыми путями.
    let mut config = Config::default();
    config.paths.socket = socket.clone();
    config.paths.db = db.clone();
    let config = std::sync::Arc::new(config);

    // Запуск демона в фоне.
    let storage = Storage::open(&config.paths.db).await.unwrap();
    let bus = EventBus::new();
    let manager = SessionManager::new(storage.clone(), config.clone(), bus.clone());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let server = Arc::new(RpcServer::new(
        manager,
        storage,
        bus,
        std::process::id(),
        shutdown,
    ));
    let socket_clone = socket.clone();
    let server_handle = tokio::spawn(async move { server.serve(&socket_clone).await });

    // Дать серверу время стартовать.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Клиент 1: создаём сессию.
    use vpsagent_daemon::RpcClient;
    let client = RpcClient::connect(&socket).await.unwrap();
    let resp = client
        .request(
            &Request::SessionCreate {
                cwd: "/tmp".into(),
                model: None,
            },
            &mut |_| {},
        )
        .await
        .unwrap();
    let session = match resp {
        Response::Session(s) => s,
        _ => panic!("ожидал Session, got {:?}", resp),
    };
    assert_eq!(session.cwd, "/tmp");

    // Список сессий.
    let list_resp = client
        .request(&Request::SessionList, &mut |_| {})
        .await
        .unwrap();
    match list_resp {
        Response::SessionList(v) => assert_eq!(v.len(), 1),
        _ => panic!("ожидал SessionList"),
    }

    // Attach к сессии.
    let attach_resp = client
        .request(
            &Request::SessionAttach {
                session_id: session.id,
                last_seq: None,
            },
            &mut |_| {},
        )
        .await
        .unwrap();
    match attach_resp {
        Response::SessionAttach {
            messages,
            agents,
            last_seq,
        } => {
            assert!(messages.is_empty());
            assert!(agents.is_empty());
            assert_eq!(last_seq, 0);
        }
        _ => panic!("ожидал SessionAttach"),
    }

    // Клиент 2: подключается и видит ту же сессию (несколько клиентов).
    let client2 = RpcClient::connect(&socket).await.unwrap();
    let list2 = client2
        .request(&Request::SessionList, &mut |_| {})
        .await
        .unwrap();
    match list2 {
        Response::SessionList(v) => assert_eq!(v.len(), 1),
        _ => panic!("ожидал SessionList для второго клиента"),
    }

    // Останавливаем сервер.
    server_handle.abort();
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&db);
}

/// Ошибка fire-and-forget `send` (например MessageSend на несуществующей
/// сессии) должна сурфейситься через `drain_events` как `EventKind::Error`.
/// Иначе (до фикса) ответ-ошибка молча остаётся в read-буфере клиента, и
/// пользователь не видит «модель не найдена» и подобные ошибки демона.
#[tokio::test]
async fn rpc_message_send_error_surfaced() {
    let socket = tmp_socket_path();
    let db = tmp_db_path();
    let _ = std::fs::remove_file(&socket);

    let mut config = Config::default();
    config.paths.socket = socket.clone();
    config.paths.db = db.clone();
    let config = std::sync::Arc::new(config);

    let storage = Storage::open(&config.paths.db).await.unwrap();
    let bus = EventBus::new();
    let manager = SessionManager::new(storage.clone(), config.clone(), bus.clone());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let server = Arc::new(RpcServer::new(
        manager,
        storage,
        bus,
        std::process::id(),
        shutdown,
    ));
    let socket_clone = socket.clone();
    let server_handle = tokio::spawn(async move { server.serve(&socket_clone).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    use vpsagent_core::Id;
    use vpsagent_daemon::RpcClient;
    let client = RpcClient::connect(&socket).await.unwrap();

    // MessageSend на несуществующей сессии → демон вернёт Response::Error
    // (send_message → list_agents/get_session падает). send() — fire-and-forget,
    // ответ НЕ читается; ошибка должна всплыть через drain_events.
    client
        .send(&Request::MessageSend {
            session_id: Id::nil(),
            text: "привет".into(),
            target: None,
        })
        .await
        .unwrap();

    // Собираем события до 1 секунды: ждём синтетический EventKind::Error.
    use std::sync::Mutex as StdMutex;
    let errors = std::sync::Arc::new(StdMutex::new(Vec::<String>::new()));
    let errors_clone = errors.clone();
    let drain_handle = tokio::spawn(async move {
        let _ = client
            .drain_events(move |ev| {
                if let EventKind::Error { message, .. } = ev.kind {
                    errors_clone.lock().unwrap().push(message);
                }
            })
            .await;
    });

    // Poll до 1 с: drain_events блокирующая, но spawn даёт параллельность.
    for _ in 0..20 {
        if !errors.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    drain_handle.abort();

    let errs = errors.lock().unwrap().clone();
    assert!(
        !errs.is_empty(),
        "ошибка MessageSend должна сурфейситься через drain_events"
    );
    assert!(
        errs.iter()
            .any(|m| m.contains("не найдена") || !m.is_empty()),
        "ожидал осмысленное сообщение об ошибке, got {errs:?}"
    );

    server_handle.abort();
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&db);
}
