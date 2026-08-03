//! Тесты хранилища: сессии, сообщения, события (append + replay).

use vpsagent_core::{ContentBlock, EventKind, Role, SessionStatus};
use vpsagent_storage::Storage;
fn tmp_db() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vpsagent-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

#[tokio::test]
async fn create_and_list_session() {
    let db = tmp_db();
    let storage = Storage::open(&db).await.unwrap();

    let s1 = storage
        .create_session("/tmp", Some("gpt-4o-mini"))
        .await
        .unwrap();
    assert_eq!(s1.cwd, "/tmp");
    assert_eq!(s1.model.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(s1.status, SessionStatus::Active);

    let s2 = storage.create_session("/home", None).await.unwrap();
    assert_eq!(s2.model, None);

    let list = storage.list_sessions().await.unwrap();
    assert_eq!(list.len(), 2);
    // newest first
    assert_eq!(list[0].id, s2.id);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn insert_and_list_messages() {
    let db = tmp_db();
    let storage = Storage::open(&db).await.unwrap();

    let session = storage.create_session("/tmp", None).await.unwrap();
    let msg = vpsagent_core::Message {
        id: uuid::Uuid::new_v4(),
        session_id: session.id,
        agent_id: None,
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "привет".into(),
        }],
        tokens: None,
        created_at: chrono::Utc::now(),
    };
    storage.insert_message(&msg).await.unwrap();

    let list = storage.list_messages(session.id).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].text(), "привет");

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn events_append_and_replay() {
    let db = tmp_db();
    let storage = Storage::open(&db).await.unwrap();

    let session = storage.create_session("/tmp", None).await.unwrap();
    let ev1 = storage
        .append_event(
            session.id,
            None,
            EventKind::TextDelta {
                agent_id: session.id,
                text: "chunk1".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(ev1.seq, 1);

    let ev2 = storage
        .append_event(
            session.id,
            None,
            EventKind::TextDelta {
                agent_id: session.id,
                text: "chunk2".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(ev2.seq, 2);

    // Replay с 0 → оба события.
    let all = storage.events_since(session.id, 0).await.unwrap();
    assert_eq!(all.len(), 2);

    // Replay с 1 → только второе.
    let tail = storage.events_since(session.id, 1).await.unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].seq, 2);

    let _ = std::fs::remove_file(&db);
}
