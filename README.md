# VPSAgent

Агентская CLI-система на **Rust** — собственная реализация возможностей современных
агентских систем (Codex CLI, Claude Code) с акцентом на серверный сценарий и
экономию ресурсов.

> Один статический бинарь, никакого рантайма Node. Агент живёт на VPS, пользователь
> подключается по SSH, отключается — агенты продолжают работать.

## Ключевые особенности

- **Daemon-first архитектура** — headless-демон держит агентов и сессии, thin TUI-клиент
  подключается по Unix-сокету (attach/detach). Отключился от SSH — демоны и агенты
  продолжают работу (модель tmux/Zellij).
- **Один статический бинарь** — musl-сборка для Linux/x86_64, без внешних зависимостей.
- **Роутер моделей** — свой роутер поверх OpenAI-compatible + Anthropic API;
  OmniRoute/OpenRouter как один из endpoint'ов.
- **Субагенты** — tokio-таски внутри демона с изолированным контекстом; создание,
  тестирование, параллельный запуск для A/B-исследований.
- **Скиллы** — совместимые `SKILL.md` (экосистема Claude Code) + executable-скрипты;
  самогенерация скиллов (как Hermes).
- **A/B-раннер** — встроенный `/ab`: N агентов × разные модели, side-by-side, метрики,
  вердикт → статистика роутера.
- **MCP** — MCP-клиент (stdio + HTTP) + сам CLI как MCP-сервер.
- **Deck-обзор** — карточки всех работающих агентов + фокус-режим (полный стрим одного).
- **Мультипоток** — классификатор-роутер сообщений: новый агент / вмешательство /
  уточнение / параллельный диалог.

## Архитектура

```
┌─────────────┐   JSON-RPC    ┌──────────────────────────────┐
│  TUI-клиент │ ◄──────────► │  Daemon (headless)            │
│  (Ratatui)  │  Unix-сокет   │  ├─ SessionManager           │
└─────────────┘              │  ├─ EventBus (pub/sub)        │
                             │  ├─ Router (модели)          │
┌─────────────┐              │  ├─ SubagentManager          │
│  vpsagent   │ ◄──────────► │  └─ SQLite (сессии, события) │
│  run / MCP  │              └──────────────────────────────┘
└─────────────┘
```

- **Протокол клиент↔демон:** JSON-RPC поверх Unix-сокета (права 0600), удалённо —
  SSH-туннель; server-push стриминг событий.
- **Хранение:** SQLite в демоне (сессии, события, статистика роутера, A/B-результаты).
- **Язык:** Rust + tokio, edition 2021.

## Workspace

| Crate | Назначение |
|---|---|
| `crates/vpsagent` | Единый бинарь CLI (`vpsagent daemon`/`run`/`attach`/`mcp-serve`/`init`) |
| `crates/daemon` | Headless-демон: singleton-lock, RPC-сервер, session manager, event bus |
| `crates/core` | Конфиг, протокол (JSON-RPC), типы |
| `crates/runtime` | Цикл агента, инструменты, субагенты, скиллы, MCP, A/B |
| `crates/providers` | LLM-провайдеры (OpenAI/Anthropic-compat) + роутер + статистика |
| `crates/storage` | SQLite (схема, репозитории сессий/событий/агентов) |
| `crates/tui` | TUI на Ratatui (диалог, deck, init-мастер) |
| `crates/tests` | Интеграционные тесты |

## Установка (Linux x86_64)

Одной командой (curl|sh) — скачивает статический musl-бинарь из [GitHub Releases](https://github.com/askidmobile/VPSAgent/releases), проверяет SHA256, устанавливает в `~/.local/bin`:

```bash
curl -fsSL https://github.com/askidmobile/VPSAgent/releases/latest/download/install.sh | sh
```

Системная установка (`/usr/local/bin`, нужен sudo):

```bash
curl -fsSL https://github.com/askidmobile/VPSAgent/releases/latest/download/install.sh | sudo sh
```

Если `~/.local/bin` не в PATH — installer подскажет добавить в `~/.bashrc`/`~/.zshrc`.

> macOS и другие архитектуры: установщик не поддерживает (musl-бинарь только для Linux x86_64). Сборка из исходников — см. ниже.

## Сборка

### Локальная (разработка, macOS/Linux)

```bash
cargo build --release
```

Бинарь — `target/release/vpsagent` (динамическая линковка под текущую ОС;
для распространения не подходит — тянется glibc целевого дистрибутива).

### Релиз: статический musl-бинарь для Linux x86_64

Поставка — один статический musl-бинарь. Сборка идёт на `yttri-win` (Windows + WSL)
с Linux-тулчейном `musl-tools` (нужен `rusqlite/bundled` для компиляции SQLite под
target). Полная инструкция — [`docs/build-musl-yttri-win.md`](docs/build-musl-yttri-win.md).

Кратко (в WSL на yttri-win):

```bash
# Окружение (один раз):
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools

# Сборка бинаря + tar.gz + sha256 (имена по конвенции install.sh / vpsagent upgrade):
scripts/build-musl.sh
# → build/vpsagent-v{ВЕРСИЯ}-x86_64-unknown-linux-musl.tar.gz + .sha256
```

Артефакты публикуются в [GitHub Releases](https://github.com/askidmobile/VPSAgent/releases);
`install.sh` и `vpsagent upgrade` скачивают их по имени (см. «Установка» и «Обновление»).

## Использование

```bash
# Первичная инициализация (интерактивный TUI — провайдеры и ключи)
vpsagent init

# Headless-прогон задачи (демон поднимается автоматически, если не запущен)
vpsagent run "реши задачу из файла task.md"

# Подключиться к существующей сессии
vpsagent attach [session-id]

# Режим MCP-сервера (vpsagent как MCP-сервер для внешних агентов)
vpsagent mcp-serve
```

### Обновление

Самообновление из GitHub Releases — проверяет, скачивает и заменяет бинарь:

```bash
vpsagent upgrade           # проверить и установить новую версию
vpsagent upgrade --check   # только проверить (без установки)
vpsagent upgrade --force   # переустановить даже если версия та же
```

Команда запрашивает latest-релиз через GitHub API, сравнивает semver, скачивает
`tar.gz` с musl-бинарем, проверяет SHA256 и атомарно заменяет текущий бинарь.
Перезапустите `vpsagent` после обновления.

### Управление демоном

`vpsagent` — daemon-first: headless-демон держит агентов и сессии, переживает
отключение SSH. Управлять жизненным циклом можно через подкоманды `daemon`.

```bash
# Запустить демона. По умолчанию на Linux — daemonize (отсоединяется от
# терминала, переживает закрытие SSH). При запуске из терминала — foreground
# (логи в stderr); из pipe/без TTY — автоматически уходит в фон.
vpsagent daemon

# Форсировать режим:
vpsagent daemon --foreground    # всегда foreground (отладка, логи в stderr)
vpsagent daemon --daemonize     # всегда daemonize (фоновый режим)
vpsagent daemon --log-file PATH # свой лог-файл при daemonize

# Статус демона (через Unix-сокет):
vpsagent daemon status                    # text: version/pid/uptime/сессии
vpsagent daemon status --output-format json   # machine-readable (FR-012)

# Остановить демона (graceful shutdown через RPC):
vpsagent daemon stop

# Перезапустить (stop + новый запуск):
vpsagent daemon restart
```

**Что происходит при daemonize:** процесс делает fork + `setsid`, stdio
перенаправляется в лог-файл (`~/.vpsagent/daemon.log` по умолчанию), SIGHUP
игнорируется — демон продолжает работать после закрытия SSH. PID хранится в
`~/.vpsagent/daemon.sock.lock` (singleton-lock: повторный запуск отказывается
стартовать, если демон уже жив). На Windows/macOS daemonize недоступен —
демон работает в foreground с предупреждением.

Конфиг — `~/.vpsagent/config.toml`, секреты — `~/.vpsagent/secrets.toml` (права 0600).
См. `examples/`.

## Документация

- [`BRIEF.md`](BRIEF.md) — бриф проекта (требования, решения).
- [`docs/build-musl-yttri-win.md`](docs/build-musl-yttri-win.md) — сборка musl-бинаря на yttri-win (WSL).
- [`docs/specs/`](docs/specs/) — технические спецификации.
- [`docs/plans/`](docs/plans/) — планы реализации.

## Статус

Активная разработка. Скелет end-to-end реализован, фичи добавляются фазами.

## Лицензия

MIT OR Apache-2.0