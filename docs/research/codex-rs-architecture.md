# Выжимка: архитектура codex-rs (OpenAI Codex CLI, Rust)

**Дата:** 2026-08-03 · **Источник:** `research/codex` (openai/codex, shallow clone) · **Лицензия:** Apache-2.0 (заимствование кода разрешено с атрибуцией/NOTICE)
**Назначение:** референс для VPSAgent. Чистый Rust, ratatui, tokio — наш стек.

---

## 1. Общая картина

Workspace `codex-rs/` — **~100 крейтов** (сборка: Cargo + Bazel). Ядро `core` —
51k строк только верхнего уровня `src/*.rs`. Много OpenAI-специфики (cloud tasks,
connectors, realtime audio, ChatGPT login) — нам нужны паттерны, не объём.

Ключевая находка: **у них есть та же архитектура, что мы выбрали** — клиент-сервер
по JSON-RPC (крейты `app-server*`, транспорты stdio / Unix-socket / websocket).
Наш выбор daemon + thin client + JSON-RPC/UDS подтверждён боевым референсом.

## 2. Агентное ядро (`core`)

### 2.1. Основные типы
- `Session` (`core/src/session/session.rs`) — сессия: сервисы, состояние, очередь ввода
- `TurnContext` (`core/src/session/turn_context.rs`) — контекст витка (модель, cwd, окно контекста)
- `SessionTask` (`core/src/tasks/mod.rs`) — трейт задачи сессии; виды: `RegularTask`,
  `ReviewTask` (встроенный /review!), `CompactTask`, `UserShellCommandTask`
- `CodexThread` (`core/src/codex_thread.rs`, 2k+ строк) — управление потоком/агентом

### 2.2. Цикл витка
`run_turn` (`core/src/session/turn.rs:149`):
1. Pre-turn компактификация (`run_pre_sampling_compact`) до сэмплинга
2. Цикл: `ModelClientSession` стримит → события → tool calls → результаты в историю
3. **Steering**: `RegularTask::run` (`core/src/tasks/regular.rs:81`) после витка
   проверяет `sess.input_queue.has_pending_input()` — новые сообщения пользователя
   становятся следующим витком без остановки сессии. Точно наш дизайн мультипотока.
4. Отмена: `tokio_util::sync::CancellationToken` + child-token на виток;
   graceful interruption timeout 100 мс (`tasks/mod.rs:107`)

### 2.3. Клиент модели (`core/src/client.rs`, 2.4k строк)
- SSE-стрим, ретраи с backoff, отдельный retry при 401 (`PendingUnauthorizedRetry`)
- Событие `EventMsg::ModelReroute` — переключение модели на лету (пригодится роутеру)
- `compact_remote*.rs` — компактификация через отдельный вызов модели

## 3. Протокол ядро ↔ клиент (`protocol`)

Разделение **команды / события** — как наш JSON-RPC request / server-push:
- `Op` (`protocol/src/protocol.rs:531`) — команды ядру: `UserInput` (с
  `additional_context: BTreeMap<String, AdditionalContextEntry>` — клиенты инжектят
  контекст: @файлы, env), `Interrupt`, `ExecApproval`, `PatchApproval`,
  `ResolveElicitation`, `UserInputAnswer`, `RequestPermissionsResponse`,
  `InterAgentCommunication` (агент↔агент!), `ThreadSettings`
- `EventMsg` (~70 вариантов, `:1288`) — события наружу: `TurnStarted/TurnComplete`,
  `AgentMessage`, `ExecCommandBegin/OutputDelta/End`, `McpToolCallBegin/End`,
  `WebSearchBegin/End`, `ApplyPatchApprovalRequest`, `RequestUserInput`,
  `ContextCompacted`, **`ThreadRolledBack`** (rewind!), `TokenCount`, `ModelReroute`
- Инструменты `request_user_input` (наш `ask_user`) и `request_permissions`
  (агент просит эскалацию прав) — встроены в протокол

## 4. Клиент-сервер (`app-server*`) — главный референс для нашего демона

- `app-server-protocol/src/rpc.rs` — чистый **JSON-RPC 2.0**
  (`JSONRPCMessage: Request/Notification/Response/Error`), протокол версионируется (v1, v2)
- Транспорты (`app-server-transport/src/transport/`): `stdio.rs`, `unix_socket.rs`,
  `websocket.rs` + `auth.rs`; крейт `stdio-to-uds` — мост (клиент говорит stdio,
  мост пробрасывает в Unix-сокет демона — **аналог нашего SSH-туннеля**)
- `app-server-daemon` — управление установкой, update loop, remote control
- TUI подключается к app-server (`tui/src/app_server_session.rs`) — их TUI тоже thin client

**Вывод для нас:** архитектура 1-в-1 с нашей спекой (демон, JSON-RPC/UDS, thin TUI).
Разница: они держат и stdio-транспорт (headless/IDE), нам тоже пригодится для `vpsagent run`.

## 5. Инструменты (`tools` + core)

- `tools/src/tool_executor.rs`, `tool_definition.rs`, `tool_spec.rs` — реестр/исполнение
- **`tool_search` / `tool_discovery` / `dynamic_tool`** — динамическое открытие
  инструментов (модель ищет инструменты по описанию; снижает размер контекста)
- `apply_patch` (`core/src/apply_patch.rs`) — их фирменный инструмент правок:
  **patch-формат вместо точной замены строк**. Устойчивее к ошибкам модели.
  Рассмотреть как основной edit-инструмент наряду с `edit`
- `exec` + `UnifiedExec*` (`tools/src/tool_config.rs`) — единый exec-инструмент
  с режимами (интерактивные сессии shell, фон)
- `request_plugin_install` — агент сам просит доустановить плагин
- MCP-инструменты: `tools/src/mcp_tool.rs`, `core/src/mcp_tool_call.rs` (2.2k строк),
  elicitation (`core/src/elicitation.rs`) — MCP-сервер может спрашивать пользователя

## 6. Персистентность (`rollout`)

- Формат: **JSONL + zstd-компрессия** (`.jsonl.zst`, `rollout/src/compression.rs:43`)
- Раскладка по датам `sessions/yyyy/mm/dd`, пагинация списка (`rollout/src/list.rs`)
- `rollout_reconstruction` — восстановление состояния сессии из rollout-файла (resume)
- Rewind = событие `ThreadRolledBack` + откат rollout
- `thread-store`, `message-history` — история и индексы

**Для нас:** мы выбрали SQLite — оставляем. Заимствуем: zstd-сжатие старых сессий
и reconstruction-подход (состояние = редукция событий, наши `events` в SQLite это дают).

## 7. Сандбоксинг и права

- Linux: `bwrap` + `landlock` (`sandboxing/`, `linux-sandbox/`); Windows: свой крейт
- **`execpolicy`** — отдельный крейт-политик: парсит команду и решает, можно ли без
  approval (наш allow/deny + hooks — та же идея, у них вынесена в политику со схемой)
- `shell-escalation`, `process-hardening`, `guardian/` (доп. проверка команд, событие
  `GuardianWarning`) — для нас избыточно на старте, но концепт policy-engine — берём

## 8. Скиллы, хуки, плагины, память

- **Скиллы (`core-skills`)**: `SKILL.md` с YAML frontmatter (`loader.rs:65`
  `SkillFrontmatter`, `:139 SKILLS_FILENAME = "SKILL.md"`), loader + root_loader
  (иерархия каталогов), `injection.rs` — внедрение в контекст, `service.rs`.
  **Формат совместим с экосистемой Claude Code — наш выбор D1 подтверждён**
- **Хуки (`hooks`)**: `declarations`, `engine`, `events`, `schema`; `hook_runtime.rs`
  в core (959 строк). `output_spill.rs` — **большие выводы сливаются в файл** —
  ровно наш `truncate.rs`, идея подтверждена
- **Плагины (`plugin`)**: `manifest.rs`, `provider.rs` — манифест + провайдеры
  (наш FR-076, референс структуры манифеста)
- **Память (`memories/`)**: read/write крейты + `core/src/agents_md.rs` +
  `agents_md_manager.rs` — иерархия AGENTS.md (наш FR-018)

## 9. Мультиагентность

- `core/src/agent/`: `registry.rs` (реестр типов агентов), `builtins/`,
  `role.rs`, `status.rs`, `control.rs`: `SpawnAgentOptions` (fork-режимы!
  `SpawnAgentForkMode` — субагент может форкать контекст родителя), `LiveAgent`,
  `AgentStatus`, `AgentExecutionGuard`
- `core/src/session/multi_agents.rs`, `MultiAgentVersion` (V1/V2)
- Сообщения между агентами — `Op::InterAgentCommunication` с provenance
  (mailbox attribution, `tasks/mod.rs:115`)

**Для нас:** их модель «агент = thread» тяжелее нашей (tokio-task), но fork-режим
(субагент с копией контекста родителя vs чистый контекст) — отличная идея,
добавить опцию `fork: bool` в определение субагента.

## 10. TUI (`tui`)

- Стек: ratatui + crossterm (`event-stream`), markdown: **pulldown-cmark**,
  подсветка: **syntect + two-face** — совпадает с нашим выбором зависимостей
- Структура: `app.rs` + `chatwidget/` (главный виджет), `bottom_pane/` (композер:
  textarea, slash-автокомплит, `@`-поиск файлов через `file_search.rs`),
  `history_cell/`, `exec_cell/` (рендер команд), `diff_render.rs` + `diff_model.rs`
  (подсвеченные diff'ы правок)
- **Стриминг markdown**: `markdown_stream.rs` + `markdown_render/streaming.rs` —
  инкрементальный парсинг неполного markdown (наша боль — берём подход)
- `app_backtrack.rs` — rewind UI (backtrack по истории)
- Approval-диалоги: `approval_events.rs`, конвертация из app-server событий
- `frames.rs` + `custom_terminal.rs` — управление кадрами/перерисовкой

## 11. Что заимствуем (действия для спеки/плана)

| Паттерн codex-rs | Куда у нас | Действие |
|---|---|---|
| JSON-RPC daemon + UDS + stdio-мост | daemon, `vpsagent run` | уже в плане; добавить stdio-транспорт для headless |
| `Op`/`EventMsg` разделение | core/protocol | уже в плане (request/server-push) |
| Steering через input_queue между витками | agent_loop | уже в плане (FR-027) |
| CancellationToken + child tokens | runtime | уже в плане |
| `apply_patch`-формат правок | tools/fs | **добавить** инструмент `apply_patch` (фаза 2) |
| `execpolicy` (политика команд) | permissions | учесть при реализации пайплайна (фаза 1) |
| `output_spill` (вывод → файл) | truncate.rs | уже в плане |
| `additional_context` в UserInput | protocol | **добавить** в `message.send` (фаза 2, @упоминания файлов) |
| SKILL.md loader + injection | skills | уже в плане (D1 подтверждён) |
| fork-режим субагентов | subagent | **добавить** `fork: bool` в AgentDefinition (фаза 2) |
| rollout JSONL+zstd + reconstruction | storage | zstd-архивация старых сессий (фаза 8) |
| `ThreadRolledBack` + reconstruction для rewind | rewind | уже в плане (D3) |
| markdown_stream (инкрементальный парсер) | tui | уже в плане; читать их `markdown_render/streaming.rs` при реализации |
| tool_search (динамическое открытие инструментов) | tools | **идея в backlog** (P2+, снижение контекста при множестве MCP-инструментов) |
| ModelReroute-событие | router | учесть в событиях роутера (фаза 4) |

## 12. Что НЕ берём (OpenAI-специфика)

realtime audio (`realtime_conversation*`), connectors, cloud-tasks, ChatGPT login,
`guardian`, `code_mode`/v8, attestation, OTel/analytics (нам достаточно tracing),
Responses-API-специфичные wire-форматы (мы делаем OpenAI-compatible + Anthropic).

## 13. Оценка масштаба

codex-rs — продукт команды с ~100 крейтами и сотнями KLOC. Наш MVP — 9 крейтов.
Заимствуем паттерны и отдельные файлы (с NOTICE), не архитектурную массу.
Риск «переусложнить по образцу» — держим фазовую дисциплину (см. риски плана).

## Приложение: другие референсы

- **warpdotdev/warp** (Rust, 63.9K★) — открыт в 2025, но **AGPL-3.0**: читать можно,
  код заимствовать нельзя. Полноценный терминал + agentic environment; пригоден как
  UX-референс для deck-обзора. Отложен до необходимости.
- Wave Terminal (Go+TS) — не наш стек, пропускаем.
- Claude Code — поведенческий аудит отдельной задачей (clean-room, Фаза 0 плана).
