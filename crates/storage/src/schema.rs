//! Миграции SQLite (idempotent, выполняются при старте демона).
//!
//! Схема соответствует §7 спецификации.

pub const MIGRATIONS: &[&str] = &[
    // 1. sessions
    "CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        title TEXT NOT NULL,
        cwd TEXT NOT NULL,
        model TEXT,
        status TEXT NOT NULL DEFAULT 'active',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    // 2. agents
    "CREATE TABLE IF NOT EXISTS agents (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        parent_id TEXT,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'waiting',
        last_action TEXT NOT NULL DEFAULT '',
        tokens_input INTEGER NOT NULL DEFAULT 0,
        tokens_output INTEGER NOT NULL DEFAULT 0,
        model TEXT NOT NULL,
        created_at TEXT NOT NULL,
        finished_at TEXT,
        FOREIGN KEY (session_id) REFERENCES sessions(id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_agents_session ON agents(session_id)",
    // 3. messages
    "CREATE TABLE IF NOT EXISTS messages (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        agent_id TEXT,
        role TEXT NOT NULL,
        content TEXT NOT NULL,
        tokens_input INTEGER,
        tokens_output INTEGER,
        created_at TEXT NOT NULL,
        FOREIGN KEY (session_id) REFERENCES sessions(id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, created_at)",
    // 4. events (для replay и инкрементальной подписки)
    "CREATE TABLE IF NOT EXISTS events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL,
        agent_id TEXT,
        seq INTEGER NOT NULL,
        kind TEXT NOT NULL,
        payload TEXT NOT NULL,
        created_at TEXT NOT NULL,
        FOREIGN KEY (session_id) REFERENCES sessions(id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_events_session_seq ON events(session_id, seq)",
    // H25: seq уникален в рамках сессии — защита от гонки при параллельных
    // коннекциях (mcp-serve открывает вторую). Старый неуникальный индекс
    // заменяем уникальным (миграции идемпотентны).
    "DROP INDEX IF EXISTS idx_events_session_seq",
    "CREATE UNIQUE INDEX IF NOT EXISTS ux_events_session_seq ON events(session_id, seq)",
];