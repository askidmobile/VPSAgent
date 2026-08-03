//! `vpsagent-tui`: интерактивный терминальный клиент.
//!
//! Фаза 1: минимальный — фокус-панель стрима агента + ввод + статус-бар.
//! Фаза 2 добавит deck-обзор, markdown-рендер, AskDialog, slash-команды.

pub mod app;
pub mod init;
pub mod registry;
pub mod ui;

pub use init::run_init;
pub use registry::{endpoint_kind, load_registry, RegistryProvider};

use std::io::stdout;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use vpsagent_core::{Config, Event, Request, Response};
use vpsagent_daemon::RpcClient;

use app::App;

/// Запустить TUI: подключиться к демону, создать/выбрать сессию, основной цикл.
pub async fn run(config: &Config, cwd: &str) -> Result<()> {
    // Подключение к демону.
    let socket = config.paths.socket.clone();
    if !socket.exists() {
        anyhow::bail!("демон не запущен. Запустите: vpsagent daemon");
    }
    // Arc: клиент разделяется между UI-циклом и фоновой задачей стрима событий.
    let client = Arc::new(RpcClient::connect(&socket).await?);

    // Создаём новую сессию (Фаза 1 — простая семантика: одна сессия на запуск TUI).
    let resp = client
        .request(&Request::SessionCreate { cwd: cwd.to_string(), model: None }, &mut |_| {})
        .await?;
    let session = match resp {
        Response::Session(s) => s,
        Response::Error { code, message } => anyhow::bail!("SessionCreate error {code}: {message}"),
        _ => anyhow::bail!("неожиданный ответ на SessionCreate"),
    };

    // Attach к стриму событий.
    let _ = client
        .request(
            &Request::SessionAttach { session_id: session.id, last_seq: None },
            &mut |_| {},
        )
        .await?;

    // Фоновая задача: читает события демона и шлёт в канал UI-цикла.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
    {
        let client = client.clone();
        tokio::spawn(async move {
            if let Err(e) = client
                .drain_events(move |ev| {
                    let _ = event_tx.send(ev);
                })
                .await
            {
                tracing::warn!(err=%e, "стрим событий завершился с ошибкой");
            }
        });
    }

    // Терминал.
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::default();
    app.session = Some(session.clone());
    app.attached = true;
    app.status = format!("сессия {} создана. Введите задачу.", session.id);

    let result = main_loop(&mut terminal, &client, &mut event_rx, &mut app, &socket).await;

    // Восстановление терминала.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
}

async fn main_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    client: &Arc<RpcClient>,
    event_rx: &mut mpsc::UnboundedReceiver<Event>,
    app: &mut App,
    socket: &Path,
) -> Result<()> {
    let mut events = EventStream::new();
    loop {
        terminal.draw(|f| ui::render(f, app))?;

        tokio::select! {
            maybe_ev = events.next() => {
                let Some(Ok(ev)) = maybe_ev else { break; };
                if !handle_key(ev, app, client, socket).await? {
                    break;
                }
            }
            event = event_rx.recv() => {
                let Some(event) = event else { break; };
                app.apply_event(event.kind);
            }
        }
    }
    Ok(())
}

async fn handle_key(
    ev: CrosstermEvent,
    app: &mut App,
    client: &Arc<RpcClient>,
    socket: &Path,
) -> Result<bool> {
    match ev {
        CrosstermEvent::Key(KeyEvent { code, modifiers, .. }) => {
            // Диалог подтверждения разрешения: перехватывает y/n.
            if app.pending_permission.is_some() {
                match code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('n') | KeyCode::Char('N') => {
                        let allowed = matches!(code, KeyCode::Char('y') | KeyCode::Char('Y'));
                        if let Some(p) = app.pending_permission.take() {
                            client
                                .send(&Request::PermissionAnswer { call_id: p.call_id, allowed })
                                .await?;
                            app.status = if allowed {
                                format!("разрешено: {}", p.name)
                            } else {
                                format!("запрещено: {}", p.name)
                            };
                        }
                        return Ok(true);
                    }
                    _ => return Ok(true),
                }
            }
            match (code, modifiers) {
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(false),
                (KeyCode::Char('d'), KeyModifiers::CONTROL) => return Ok(false),
                (KeyCode::Enter, _) => {
                    if let Some((session_id, text)) = app.submit_input() {
                        app.agent_text.push_str(&format!("\n\n--- ты ---\n{text}\n"));
                        client
                            .send(&Request::MessageSend { session_id, text, target: None })
                            .await?;
                        app.status = "отправлено, ожидаю ответ…".into();
                    }
                }
                (KeyCode::Char(c), _) => app.input.push(c),
                (KeyCode::Backspace, _) => {
                    app.input.pop();
                }
                (KeyCode::Up, _) => {
                    if let Some(last) = app.input_history.last() {
                        app.input = last.clone();
                    }
                }
                (KeyCode::Tab, _) => {
                    // Переключение выбранного агента в deck.
                    if !app.agents.is_empty() {
                        let idx = app
                            .agents
                            .iter()
                            .position(|a| Some(a.id) == app.selected_agent)
                            .map(|i| (i + 1) % app.agents.len())
                            .unwrap_or(0);
                        let id = app.agents[idx].id;
                        app.select_agent(Some(id));
                        app.status = format!("выбран агент: {}", app.agents[idx].name);
                    }
                }
                (KeyCode::F(1), _) => {
                    // Обновить список агентов с демона.
                    // Отдельное одноразовое подключение: основной read-half
                    // занят фоновой задачей drain_events.
                    if let Some(s) = &app.session {
                        let sid = s.id;
                        let agents = match RpcClient::connect(socket).await {
                            Ok(one_shot) => match one_shot
                                .request(&Request::AgentList { session_id: sid }, &mut |_| {})
                                .await
                            {
                                Ok(Response::AgentList(a)) => a,
                                _ => vec![],
                            },
                            Err(_) => vec![],
                        };
                        app.agents = agents;
                        app.status = format!("обновлён список: {} агентов", app.agents.len());
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
    Ok(true)
}

#[allow(dead_code)]
fn _ensure_path(_: &PathBuf) {}
