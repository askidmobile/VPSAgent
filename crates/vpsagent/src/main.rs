//! `vpsagent` — единый бинарь CLI.
//!
//! Подкоманды:
//!   `vpsagent`           — интерактивный TUI (attach к демону/новая сессия) [Фаза 1: упрощённый вывод]
//!   `vpsagent daemon`    — запустить headless-демон (по умолчанию daemonize на Unix, FR-001)
//!   `vpsagent daemon stop`    — graceful stop демона через RPC (FR-006)
//!   `vpsagent daemon restart` — перезапустить демон: stop + запуск (FR-020)
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
    /// Самообновление: проверить/скачать/установить новую версию из GitHub Releases.
    Upgrade {
        /// Только проверить наличие обновления (без установки).
        #[arg(long)]
        check: bool,
        /// Обновить даже если версия совпадает.
        #[arg(long)]
        force: bool,
    },
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
    /// Перезапустить демон: stop + запуск (FR-020).
    Restart,
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

    // FR-020: `vpsagent daemon restart` — stop текущего демона (RPC) + запуск нового.
    // Обрабатывается ВНЕ tokio runtime (как и запуск): запуск нового демона зовёт
    // `run_daemon_sync`, который делает fork ДО runtime — критично, чтобы child
    // не унаследовал epoll fd родительского runtime. Stop-часть выполняется в
    // короткоживущем runtime (RPC — асинхронный), затем отдельный синхронный запуск.
    if let Some(Command::Daemon {
        action: Some(DaemonCmd::Restart),
        foreground,
        daemonize,
        log_file,
    }) = &cli.command
    {
        return daemon_restart_sync(config, *foreground, *daemonize, log_file.clone());
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
                // FR-020: restart перехвачен в синхронной main ДО runtime (нужен
                // fork ДО epoll fd). Сюда мы не должны попасть.
                DaemonCmd::Restart => anyhow::bail!(
                    "restart должен обрабатываться в синхронной main до tokio runtime"
                ),
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
            Some(Command::Upgrade { check, force }) => run_upgrade(check, force).await,
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

/// `vpsagent daemon restart` (FR-020): остановить текущий демон (RPC DaemonStop)
/// и запустить новый через тот же путь, что `vpsagent daemon` (`run_daemon_sync`).
///
/// Обрабатывается ВНЕ tokio runtime (как и запуск демона): запуск нового демона
/// зовёт `run_daemon_sync`, который делает fork ДО runtime — критично, чтобы
/// child не унаследовал epoll fd родительского runtime.
///
/// Stop-часть асинхронная (RPC), поэтому выполняется в короткоживущем
/// current_thread-рантайме. Если демон не был запущен — restart всё равно
/// стартует новый (это нормальное поведение restart, не ошибка).
///
/// НЕ использует `Request::DaemonRestart` RPC (заглушка D4, сложный exec-в-демоне):
/// restart = stop + новый запуск на стороне CLI — проще и надёжнее.
fn daemon_restart_sync(
    config: Config,
    foreground: bool,
    daemonize: bool,
    log_file: Option<PathBuf>,
) -> Result<()> {
    // 1. Остановить текущий демон через RPC. Короткоживущий runtime только для
    //    stop-части; запуск нового пойдёт через run_daemon_sync (свой runtime/fork).
    let was_running = stop_for_restart(&config);

    if was_running {
        eprintln!("демон остановлен — запускаю новый");
    } else {
        eprintln!("демон не был запущен — запускаю новый");
    }

    // 2. Подождать, пока сокет исчезнет (poll до 3 сек) — иначе новый демон
    //    споткнётся о занятый сокет/lock. Если демон не был запущен, сокета
    //    уже нет (или это мусорный файл — run_daemon_sync/сервер почистят).
    wait_socket_gone(&config.paths.socket, std::time::Duration::from_secs(3));

    // 3. Запуск нового демона тем же путём, что `vpsagent daemon`.
    run_daemon_sync(config, foreground, daemonize, log_file)
}

/// Остановить демон через RPC для restart. Возвращает true, если демон был
/// запущен и мы послали ему DaemonStop; false — если демона не было (сокет
/// недоступен). В последнем случае restart просто стартует новый демон.
fn stop_for_restart(config: &Config) -> bool {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            // Без runtime мы не сможем послать RPC; но restart не должен падать
            // только из-за этого — считаем, что останавливать некого, и идём
            // запускать новый демон (run_daemon_sync создаст свой runtime).
            tracing::warn!(error=?e, "не удалось создать runtime для stop-части restart");
            return false;
        }
    };
    rt.block_on(async {
        let client = match RpcClient::connect(&config.paths.socket).await {
            Ok(c) => c,
            // Демон не запущен — restart стартует новый (не ошибка).
            Err(_) => return false,
        };
        match client.request(&Request::DaemonStop, &mut |_| {}).await {
            Ok(Response::Ok) => true,
            // Демон ответил чем-то странным — всё равно пробуем стартовать новый.
            Ok(other) => {
                tracing::warn!(?other, "неожиданный ответ на DaemonStop при restart");
                true
            }
            Err(e) => {
                tracing::warn!(error=?e, "RPC DaemonStop при restart не удался");
                // Если RPC не дошёл/упал, демон мог и не остановиться. Но мы всё
                // равно пробуем запуск: если сокет/lock занят, run_daemon_sync
                // сообщит об ошибке — пользователь увидит.
                false
            }
        }
    })
}

/// Подождать, пока файл сокета исчезнет (poll 100 мс). Возвращает true, если
/// сокет исчез за `timeout`; false — если всё ещё на месте (не фатально:
/// новый демон при запуске сам удалит занятый сокет, если он мусорный).
fn wait_socket_gone(socket: &std::path::Path, timeout: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    while socket.exists() {
        if started.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    true
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

/// Константы для самообновления (GitHub Releases).
const UPD_OWNER: &str = "askidmobile";
const UPD_REPO: &str = "VPSAgent";
const UPD_ARCH: &str = "x86_64-unknown-linux-musl";

/// `vpsagent upgrade` — самообновление из GitHub Releases.
///
/// Текущая версия — `env!("CARGO_PKG_VERSION")`. latest — через GitHub API.
/// Если latest > текущей (или `--force`) — скачать tar.gz, проверить SHA256,
/// атомарно заменить бинарь (rename), перезапустить не нужно (следующий запуск
/// подхватит новый бинарь). `--check` — только проверить, без установки.
async fn run_upgrade(check: bool, force: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    eprintln!("текущая версия: v{current}");

    // 1. Узнать latest версию через GitHub API.
    let api_url = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        UPD_OWNER, UPD_REPO
    );
    let resp = reqwest::Client::builder()
        .user_agent(concat!("vpsagent/", env!("CARGO_PKG_VERSION")))
        .build()?
        .get(&api_url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("не удалось узнать последнюю версию: {e}"))?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "GitHub API вернул {}: проверьте доступ к github.com",
            resp.status()
        );
    }
    let body: serde_json::Value = resp.json().await?;
    let latest = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("ответ GitHub API не содержит tag_name"))?
        .trim_start_matches('v')
        .to_string();
    eprintln!("последняя версия: v{latest}");

    // 2. Сравнение версий (простая семантика: major.minor.patch).
    let need_update = force || version_is_newer(&latest, current);
    if !need_update {
        eprintln!(
            "обновление не требуется (уже актуально). Используйте --force для переустановки."
        );
        return Ok(());
    }
    if check {
        eprintln!(
            "доступно обновление: v{current} → v{latest} (запустите без --check для установки)"
        );
        return Ok(());
    }

    // 3. Скачать tar.gz из ассетов релиза.
    let tarball_name = format!("vpsagent-v{latest}-{UPD_ARCH}.tar.gz");
    let checksum_name = format!("{tarball_name}.sha256");
    let assets = body
        .get("assets")
        .and_then(|a| a.as_array())
        .ok_or_else(|| anyhow::anyhow!("ответ GitHub API не содержит assets"))?;
    let find_asset = |name: &str| -> Result<String> {
        assets
            .iter()
            .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(name))
            .and_then(|a| a.get("browser_download_url"))
            .and_then(|u| u.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("ассет '{name}' не найден в релизе v{latest}"))
    };
    let tarball_url = find_asset(&tarball_name)?;
    let checksum_url = find_asset(&checksum_name).ok();

    eprintln!("скачиваю {tarball_name}…");
    let tmpdir = std::env::temp_dir().join(format!("vpsagent-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&tmpdir)?;
    let tarball_path = tmpdir.join(&tarball_name);

    let client = reqwest::Client::builder()
        .user_agent(concat!("vpsagent/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let bytes = client
        .get(&tarball_url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("не удалось скачать: {e}"))?
        .bytes()
        .await
        .map_err(|e| anyhow::anyhow!("не удалось прочитать тело: {e}"))?;
    std::fs::write(&tarball_path, &bytes)?;

    // 4. Проверка SHA256 (если доступен).
    if let Some(url) = checksum_url {
        eprintln!("проверяю SHA256…");
        let expected = client
            .get(&url)
            .send()
            .await?
            .text()
            .await
            .ok()
            .and_then(|t| t.split_whitespace().next().map(|s| s.to_string()));
        if let Some(exp) = expected {
            let actual = sha256_hex(&bytes);
            if exp != actual {
                std::fs::remove_dir_all(&tmpdir).ok();
                anyhow::bail!("несовпадение SHA256: ожидается {exp}, получено {actual}");
            }
            eprintln!("SHA256 OK");
        } else {
            eprintln!("[warn] не удалось получить контрольную сумму — пропускаю проверку");
        }
    }

    // 5. Распаковать бинарь во временный файл, атомарно заменить текущий.
    let exe = std::env::current_exe()?;
    eprintln!("устанавливаю в {}…", exe.display());

    // tar.gz содержит файл 'vpsagent' — распакуем в tmpdir.
    let output = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&tarball_path)
        .arg("-C")
        .arg(&tmpdir)
        .arg("vpsagent")
        .output()
        .map_err(|e| anyhow::anyhow!("не удалось распаковать (нужен tar): {e}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "tar завершился с ошибкой: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let extracted = tmpdir.join("vpsagent");
    if !extracted.exists() {
        anyhow::bail!("распакованный бинарь не найден: {}", extracted.display());
    }

    // Атомарная замена бинаря. На Linux fs::copy поверх запущенного бинаря
    // падает с ETXTBSY ("Text file busy"). Используем rename-стратегию:
    //   1. extracted → exe.new (рядом с exe, та же файловая система)
    //   2. exe → exe.bak (освобождаем имя; старый inode живёт, пока процесс
    //      выполняется — на Unix это безопасно)
    //   3. exe.new → exe (атомарный rename на целевое место)
    //   4. удаляем exe.bak
    // Если exe.bak остался от прошлого раза — удаляем заранее.
    let exe = std::env::current_exe()?;
    eprintln!("устанавливаю в {}…", exe.display());
    let exe_new = exe.with_extension("new");
    let exe_bak = exe.with_extension("bak");
    std::fs::remove_file(&exe_bak).ok();
    std::fs::rename(&extracted, &exe_new)
        .map_err(|e| anyhow::anyhow!("не удалось подготовить новый бинарь: {e}"))?;
    std::fs::rename(&exe, &exe_bak).map_err(|e| {
        // Если не вышло освободить exe — откатываем: возвращаем exe.new в tmp.
        let _ = std::fs::rename(&exe_new, &extracted);
        anyhow::anyhow!("не удалось освободить текущий бинарь (попробуйте под sudo): {e}")
    })?;
    std::fs::rename(&exe_new, &exe).map_err(|e| {
        // Откат: возвращаем старый бинарь на место.
        let _ = std::fs::rename(&exe_bak, &exe);
        anyhow::anyhow!("не удалось установить новый бинарь: {e}")
    })?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))?;
    std::fs::remove_file(&exe_bak).ok();
    std::fs::remove_dir_all(&tmpdir).ok();

    eprintln!("обновлено: v{current} → v{latest}");
    eprintln!("перезапустите vpsagent, чтобы использовать новую версию.");
    Ok(())
}

/// Простейшее сравнение semver: `latest > current`? (major.minor.patch).
fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .filter_map(|p| p.split('-').next().and_then(|n| n.parse().ok()))
            .collect()
    };
    let l = parse(latest);
    let c = parse(current);
    for i in 0..l.len().max(c.len()) {
        let lv = l.get(i).copied().unwrap_or(0);
        let cv = c.get(i).copied().unwrap_or(0);
        if lv > cv {
            return true;
        }
        if lv < cv {
            return false;
        }
    }
    false
}

/// SHA256 в hex — через внешний shasum/sha256sum (portable, без SHA-крейта).
/// Возвращает пустую строку, если ни один инструмент не доступен.
fn sha256_hex(bytes: &[u8]) -> String {
    for (cmd, args) in [("shasum", vec!["-a", "256"]), ("sha256sum", vec![])] {
        if let Ok(mut child) = std::process::Command::new(cmd)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(bytes);
            }
            if let Ok(output) = child.wait_with_output() {
                if let Ok(s) = std::str::from_utf8(&output.stdout) {
                    return s.split_whitespace().next().unwrap_or("").to_string();
                }
            }
        }
    }
    String::new()
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
    // FR-010: переиспользуем общий путь daemonize. Запускаем `vpsagent daemon`
    // как отсоединённый процесс (stdin → null → auto-detect даёт daemonize в
    // run_daemon_sync, который делает fork+setsid+stdio→log). Логика setsid/
    // stdio/лог-файла — в daemonize.rs, здесь только spawn + poll сокета.
    spawn_detached_daemon(config).await
}

/// Запустить демон как отсоединённый дочерний процесс и дождаться сокета.
///
/// Child — это `vpsagent daemon` (без флагов): т.к. мы подаём stdin=null, auto-detect
/// в `run_daemon_sync` выбирает daemonize, и внутри выполняется общий код
/// `vpsagent_daemon::daemonize::daemonize` (fork+setsid+stdio→log). Таким образом
/// `ensure_daemon` (авто-подъём из клиента) и прямой `vpsagent daemon` делят один
/// кодовый путь отсоединения (FR-010).
async fn spawn_detached_daemon(config: &Config) -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("daemon");
    // stdin → null: триггерит auto-daemonize в run_daemon_sync (FR-002).
    // stdout/stderr → null: дочерний процесс (parent до fork) ничего не должен
    // писать в наш терминал; после fork stdio child уходит в лог-файл.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    // На Unix отсоединяемся от управляющего терминала на уровне процесса-родителя
    // (для случая, если run_daemon_sync выберет foreground — не должно, но
    // безопасность ради). Основной setsid делает daemonize.rs в child.
    #[cfg(unix)]
    {
        // pre_exec (CommandExt) — unsafe-метод: closure выполняется в child
        // перед exec. Здесь вызываем setsid, чтобы отсоединить spawn-родителя.
        unsafe {
            cmd.pre_exec(|| {
                extern "C" {
                    fn setsid() -> i32;
                }
                setsid();
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn()?;
    // Reap дочернего процесса (parent до fork в run_daemon_sync): без wait
    // остаётся зомби. Сам демон (grandchild после fork) живёт независимо.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    // Подождём, пока сокет появится (poll до 5 сек) — общий паттерн с run_daemon_sync.
    for _ in 0..50 {
        if config.paths.socket.exists()
            && tokio::net::UnixStream::connect(&config.paths.socket)
                .await
                .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    anyhow::bail!("демон не стартовал за 5 секунд")
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
