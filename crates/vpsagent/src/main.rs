//! `vpsagent` — единый бинарь CLI.
//!
//! Подкоманды:
//!   `vpsagent`           — интерактивный TUI (attach к демону/новая сессия) [Фаза 1: упрощённый вывод]
//!   `vpsagent daemon`    — запустить headless-демон (по умолчанию daemonize на Unix, FR-001)
//!   `vpsagent daemon stop`    — graceful stop демона через RPC (FR-006)
//!   `vpsagent daemon status`  — статус демона через RPC (FR-007)
//!   `vpsagent run "задача"` — headless-прогон: создать сессию, отправить задачу, ждать завершения, exit code
//!   `vpsagent attach [id]`  — подключиться к существующей сессии демона

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use vpsagent_core::{Config, EventKind, Request, Response};
use vpsagent_daemon::{Daemonize, RpcClient};

#[derive(Parser)]
#[command(name = "vpsagent", version, about = "Агентская CLI-система на Rust")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Запустить headless-демон или управлять его жизненным циклом.
    Daemon {
        /// Подкоманда управления (stop/status). Без неё — запуск демона.
        #[command(subcommand)]
        action: Option<DaemonCmd>,
        /// Форсировать foreground: логи в stderr, не отсоединяться от терминала.
        /// Имеет приоритет над `--daemonize` и авто-определением.
        #[arg(long)]
        foreground: bool,
        /// Форсировать daemonize (отсоединиться от терминала, логи в файл).
        /// Имеет приоритет над авто-определением, но ниже `--foreground`.
        #[arg(long)]
        daemonize: bool,
        /// Переопределить файл лога демона при daemonize (FR-003).
        #[arg(long)]
        log_file: Option<PathBuf>,
    },
    /// Headless-прогон задачи: создать сессию, отправить, дождаться, exit code.
    Run {
        /// Задача (текст).
        task: String,
        /// Модель (переопределяет конфиг).
        #[arg(long)]
        model: Option<String>,
        /// Формат вывода: text | json | stream-json.
        #[arg(long, default_value = "text")]
        output_format: String,
    },
    /// Подключиться к существующей сессии демона (упрощённый вывод в Фазе 1).
    Attach {
        /// ID сессии (если не указан — список доступных).
        session_id: Option<String>,
    },
    /// Режим MCP-сервера: vpsagent как MCP-сервер для внешних агентов (FR-058).
    McpServe,
    /// Мастер инициализации: настройка провайдеров и ключей (интерактивный TUI).
    Init,
}

/// Подкоманды управления жизненным циклом демона (`vpsagent daemon <cmd>`).
#[derive(Subcommand)]
enum DaemonCmd {
    /// Graceful stop демона через RPC (FR-006).
    Stop,
    /// Статус демона: version/pid/uptime/сессии/агенты (FR-007).
    Status {
        /// Формат вывода: text | json (FR-012).
        #[arg(long, default_value = "text")]
        output_format: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut config = Config::load()?;

    // tracing-подписчик ставится в runtime каждого процесса; для daemon-режима
    // запуска ниже — отдельный путь (см. run_daemon).

    // Специальный путь: `vpsagent daemon` (запуск) может потребовать daemonize
    // (fork) ДО создания tokio-рантайма — иначе fork унаследует epoll/kqueue fd
    // и в child упадёт "failed to wake I/O driver: Bad file descriptor".
    // Поэтому обрабатываем запуск демона отдельно, без `#[tokio::main]`.
    if let Some(Command::Daemon {
        action: None,
        foreground,
        daemonize,
        log_file,
    }) = &cli.command
    {
        return run_daemon_sync(config, *foreground, *daemonize, log_file.clone());
    }

    // Все остальные команды — в tokio runtime.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        init_tracing();

        // Автозапуск инициализации при первом запуске (нет секретов) — только интерактивный терминал.
        // Не запускаем для daemon stop/status — они не требуют секретов.
        let needs_init = matches!(
            cli.command,
            None | Some(Command::Run { .. })
                | Some(Command::Attach { .. })
                | Some(Command::McpServe)
                | Some(Command::Init)
        ) && !matches!(
            cli.command,
            Some(Command::Daemon {
                action: Some(_),
                ..
            })
        );
        if needs_init && config.needs_init() && std::io::stdin().is_terminal() {
            config = vpsagent_tui::run_init().await?;
        }

        match cli.command {
            None => {
                // Без подкоманды — TUI (если терминал). Иначе упрощённый текстовый attach.
                ensure_daemon(&config).await?;
                let cwd = std::env::current_dir()?.to_string_lossy().to_string();
                if std::io::stdin().is_terminal() {
                    vpsagent_tui::run(&config, &cwd).await
                } else {
                    run_attach(&config, None).await
                }
            }
            Some(Command::Daemon { action, .. }) => match action.unwrap() {
                DaemonCmd::Stop => daemon_stop(&config).await,
                DaemonCmd::Status { output_format } => daemon_status(&config, &output_format).await,
            },
            Some(Command::Run {
                task,
                model,
                output_format,
            }) => run_headless(&config, &task, model, &output_format).await,
            Some(Command::Attach { session_id }) => {
                run_attach(&config, session_id.as_deref()).await
            }
            Some(Command::McpServe) => {
                // MCP-сервер: требует запущенный демон (для storage/session).
                ensure_daemon(&config).await?;
                let storage = vpsagent_storage::Storage::open(&config.paths.db)
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let subagents = vpsagent_runtime::SubagentManager::new(
                    storage.clone(),
                    std::sync::Arc::new(config.clone()),
                );
                let cwd = std::env::current_dir()?;
                vpsagent_runtime::serve_mcp(&cwd, uuid::Uuid::nil(), subagents, storage)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                Ok(())
            }
            Some(Command::Init) => {
                let cfg = vpsagent_tui::run_init().await?;
                eprintln!(
                    "Инициализация завершена. Конфиг: {}",
                    cfg.paths.data_dir.display()
                );
                Ok(())
            }
        }
    })
}

/// Инициализация tracing-подписчика (вызывается в каждом runtime).
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vpsagent=info".into()),
        )
        .with_target(false)
        .compact()
        .init();
}

/// Синхронный путь запуска демона: daemonize (fork) ВНЕ tokio runtime, затем child строит runtime.
///
/// fork ДО runtime — критично: иначе child унаследует epoll fd родительского
/// runtime и упадёт "failed to wake I/O driver: Bad file descriptor".
///
/// Определяет режим, при daemonize — fork (до runtime), затем в child/foreground
/// строит tokio runtime и зовёт `vpsagent_daemon::run`.
fn run_daemon_sync(
    config: Config,
    foreground: bool,
    daemonize: bool,
    log_file: Option<PathBuf>,
) -> Result<()> {
    let log_path = log_file.unwrap_or_else(|| config.paths.log_file.clone());

    // Режим (FR-002): --foreground > --daemonize > auto (терминал → fg, pipe → dz).
    let do_daemonize = if foreground {
        false
    } else if daemonize {
        true
    } else {
        !std::io::stdin().is_terminal()
    };

    if do_daemonize {
        // FR-001: fork ДО tokio runtime. tracing в child настроится после.
        match vpsagent_daemon::daemonize::daemonize(&log_path)? {
            Daemonize::Child => {
                // Мы — демон (child после fork). stdio уже в лог-файле.
                // Строим собственный runtime; родительский runtime не существует
                // (мы в синхронной main), так что epoll fd чист.
            }
            Daemonize::Parent { child_pid } => {
                // Мы — родитель. Печатаем подтверждение (FR-004), ждём сокет, выходим.
                eprintln!(
                    "демон запущен: pid={child_pid} сокет={} лог={}",
                    config.paths.socket.display(),
                    log_path.display()
                );
                // Poll готовности сокета синхронно (без runtime) до 5 сек.
                for _ in 0..50 {
                    if config.paths.socket.exists() {
                        // Попытка подключения — через отдельный короткоживущий runtime.
                        let probe = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .ok();
                        if let Some(rt) = probe {
                            let ok = rt.block_on(async {
                                tokio::net::UnixStream::connect(&config.paths.socket)
                                    .await
                                    .is_ok()
                            });
                            if ok {
                                return Ok(());
                            }
                        } else if config.paths.socket.exists() {
                            // Сокет появился — считаем демона стартовавшим.
                            return Ok(());
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                anyhow::bail!(
                    "демон стартовал, но сокет не появился за 5 сек — проверьте лог: {}",
                    log_path.display()
                );
            }
            Daemonize::Unsupported => {
                // FR-008: не-Unix — foreground с предупреждением.
                eprintln!(
                    "[warn] daemonize не поддерживается — foreground; логи в stderr (log_file={})",
                    log_path.display()
                );
            }
        }
    }

    // foreground или мы — child после daemonize: строим runtime и запускаем демон.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        init_tracing();
        vpsagent_daemon::run(config)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    })
}

/// `vpsagent daemon stop` (FR-006): подключиться к сокету, послать DaemonStop.
async fn daemon_stop(config: &Config) -> Result<()> {
    let client = match RpcClient::connect(&config.paths.socket).await {
        Ok(c) => c,
        Err(_) => {
            eprintln!(
                "демон не запущен (сокет {} недоступен)",
                config.paths.socket.display()
            );
            std::process::exit(1);
        }
    };
    match client.request(&Request::DaemonStop, &mut |_| {}).await {
        Ok(Response::Ok) => {
            eprintln!("демон остановлен");
            Ok(())
        }
        Ok(other) => anyhow::bail!("неожиданный ответ на DaemonStop: {other:?}"),
        Err(e) => anyhow::bail!("ошибка RPC: {e}"),
    }
}

/// `vpsagent daemon status` (FR-007): статус демона, text/json (FR-012).
async fn daemon_status(config: &Config, output_format: &str) -> Result<()> {
    let client = match RpcClient::connect(&config.paths.socket).await {
        Ok(c) => c,
        Err(_) => {
            eprintln!(
                "демон не запущен (сокет {} недоступен)",
                config.paths.socket.display()
            );
            std::process::exit(1);
        }
    };
    let resp = client.request(&Request::DaemonStatus, &mut |_| {}).await?;
    let status = match resp {
        Response::DaemonStatus(s) => s,
        other => anyhow::bail!("неожиданный ответ на DaemonStatus: {other:?}"),
    };
    if output_format == "json" {
        println!("{}", serde_json::to_string(&status)?);
    } else {
        println!("vpsagent {}", status.version);
        println!("pid:      {}", status.pid);
        println!("uptime:   {}", format_uptime(status.uptime_secs));
        println!("сессий:   {}   агентов: {}", status.sessions, status.agents);
    }
    Ok(())
}

/// Человеко-читаемый uptime (1d 2h 3m / 2h 14m / 45m / 30s).
fn format_uptime(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Headless-прогон: убедиться что демон работает, создать сессию, отправить задачу,
/// дождаться AgentFinished, вывести результат, exit code 0/1.
async fn run_headless(
    config: &Config,
    task: &str,
    model: Option<String>,
    output_format: &str,
) -> Result<()> {
    ensure_daemon(config).await?;
    let client = RpcClient::connect(&config.paths.socket).await?;

    let cwd = std::env::current_dir()?.to_string_lossy().to_string();
    let resp = client
        .request(&Request::SessionCreate { cwd, model }, &mut |_| {})
        .await?;
    let session = match resp {
        Response::Session(s) => s,
        Response::Error { code, message } => anyhow::bail!("SessionCreate error {code}: {message}"),
        _ => anyhow::bail!("неожиданный ответ на SessionCreate"),
    };

    if output_format == "json" {
        println!("{}", serde_json::to_string(&session)?);
    } else {
        eprintln!("Сессия: {} ({})", session.id, session.title);
    }

    // Отправляем задачу.
    client
        .send(&Request::MessageSend {
            session_id: session.id,
            text: task.to_string(),
            target: None,
        })
        .await?;

    // Подключаемся к стриму событий.
    let attach_resp = client
        .request(
            &Request::SessionAttach {
                session_id: session.id,
                last_seq: None,
            },
            &mut |_| {},
        )
        .await?;
    if output_format == "stream-json" {
        if let Response::SessionAttach { messages, .. } = &attach_resp {
            for m in messages {
                println!("{}", serde_json::to_string(m)?);
            }
        }
    }

    // Читаем события до AgentFinished.
    let mut finished = false;
    let mut failed = false;
    let mut last_text = String::new();
    client
        .drain_events(|ev| match ev.kind {
            EventKind::TextDelta { text, .. } => {
                if output_format == "text" {
                    print!("{text}");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                } else if output_format == "stream-json" {
                    let text = text.clone();
                    println!(
                        "{}",
                        serde_json::to_string(
                            &serde_json::json!({ "type": "text_delta", "text": text })
                        )
                        .unwrap()
                    );
                }
                last_text.push_str(&text);
            }
            EventKind::UserMessage { text, .. } => {
                if output_format == "text" {
                    println!("\n--- агент ---\n{text}");
                }
            }
            EventKind::AgentFinished { .. } => {
                finished = true;
            }
            EventKind::Error { message, .. } => {
                eprintln!("\n[ошибка] {message}");
                finished = true;
                failed = true;
            }
            _ => {}
        })
        .await?;

    if !finished {
        eprintln!("\n[соединение закрыто до завершения]");
    }
    if failed {
        std::process::exit(1);
    } else if finished {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// Упрощённый attach (Фаза 1): выводит события текстом. Полноценный TUI — Фаза 2.
async fn run_attach(config: &Config, session_id: Option<&str>) -> Result<()> {
    ensure_daemon(config).await?;
    let client = RpcClient::connect(&config.paths.socket).await?;

    let session_id = match session_id {
        Some(id) => vpsagent_core::Id::parse_str(id)?,
        None => {
            // Список сессий.
            let resp = client.request(&Request::SessionList, &mut |_| {}).await?;
            let sessions = match resp {
                Response::SessionList(v) => v,
                _ => vec![],
            };
            if sessions.is_empty() {
                eprintln!("Нет сохранённых сессий. Используйте: vpsagent run \"задача\"");
                return Ok(());
            }
            eprintln!("Доступные сессии:");
            for (i, s) in sessions.iter().enumerate() {
                eprintln!("  [{}] {} — {} ({})", i, s.id, s.title, s.cwd);
            }
            eprintln!("\nУкажите ID: vpsagent attach <id>");
            return Ok(());
        }
    };

    let resp = client
        .request(
            &Request::SessionAttach {
                session_id,
                last_seq: None,
            },
            &mut |_| {},
        )
        .await?;
    if let Response::SessionAttach { messages, .. } = &resp {
        for m in messages {
            println!("{}: {}", m.role_text(), m.text());
        }
    }
    println!("\n[live-стрим событий. Ctrl+C — отключиться (агент продолжит работать)]\n");

    client
        .drain_events(|ev| match ev.kind {
            EventKind::TextDelta { text, .. } => print!("{text}"),
            EventKind::UserMessage { text, .. } => println!("\n--- агент ---\n{text}"),
            EventKind::ToolCallStart { name, .. } => println!("\n[tool: {name}]"),
            EventKind::ToolCallEnd {
                is_error, output, ..
            } => {
                if is_error {
                    println!("[ошибка инструмента] {output}");
                }
            }
            EventKind::AgentFinished { .. } => println!("\n[агент завершён]"),
            EventKind::Error { message, .. } => println!("\n[ошибка] {message}"),
            _ => {}
        })
        .await?;
    Ok(())
}

/// Убедиться, что демон запущен; если нет — запустить в фоне.
async fn ensure_daemon(config: &Config) -> Result<()> {
    if config.paths.socket.exists()
        && tokio::net::UnixStream::connect(&config.paths.socket)
            .await
            .is_ok()
    {
        return Ok(());
    }
    // Сокет есть, но демон не отвечает — почистим.
    if config.paths.socket.exists() {
        std::fs::remove_file(&config.paths.socket).ok();
    }
    // Запускаем демон как дочерний процесс.
    let exe = std::env::current_exe()?;
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("daemon");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    // На Unix отсоединяемся от терминала (setsid) — демон переживёт SSH.
    #[cfg(unix)]
    {
        unsafe {
            cmd.pre_exec(|| {
                libc_setsid();
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn()?;
    // Reap дочернего демона: без wait процесс остаётся зомби после выхода.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    // Подождём, пока сокет появится (poll до 5 сек).
    for _ in 0..50 {
        if config.paths.socket.exists() {
            if tokio::net::UnixStream::connect(&config.paths.socket)
                .await
                .is_ok()
            {
                return Ok(());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    anyhow::bail!("демон не стартовал за 5 секунд")
}

#[cfg(unix)]
fn libc_setsid() {
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe {
        setsid();
    }
}

/// Хелпер для отображения роли.
trait RoleText {
    fn role_text(&self) -> &'static str;
}
impl RoleText for vpsagent_core::Message {
    fn role_text(&self) -> &'static str {
        match self.role {
            vpsagent_core::Role::User => "ты",
            vpsagent_core::Role::Assistant => "агент",
            vpsagent_core::Role::Tool => "tool",
        }
    }
}
