# Сборка musl-бинаря на yttri-win (WSL)

VPSAgent поставляется как **один статический musl-бинарь** для Linux x86_64.
Сборка идёт на машине `yttri-win` (Windows + WSL), где доступен Linux-тулчейн
с `musl-tools`. Это воспроизводимо и не требует macOS/Docker.

> На macOS arm64 кросс-сборка `x86_64-unknown-linux-musl` затруднена: `rusqlite`
> с фичей `bundled` компилирует SQLite из исходников и требует C-кросс-тулчейн
> под target. WSL даёт нативный Linux-тулчейн — проще и надёжнее.

## One-shot: подготовка окружения на yttri-win (один раз)

В WSL (Ubuntu/Debian) на yttri-win:

```bash
# 1. Rust (если нет).
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 2. Target + musl C-тулчейн.
rustup target add x86_64-unknown-linux-musl
sudo apt-get update && sudo apt-get install -y musl-tools
```

`musl-tools` ставит `musl-gcc` — C-кросс-компилятор, нужный `rusqlite/bundled`.

## Сборка релиза

```bash
# Склонировать репозиторий (или обновить существующий клон).
cd ~/VPSAgent
git pull origin master

# Собрать musl-бинарь + tar.gz + sha256.
scripts/build-musl.sh
```

Скрипт `scripts/build-musl.sh`:
1. Читает версию из `Cargo.toml` (`workspace.package.version`).
2. Проверяет target `x86_64-unknown-linux-musl` и `musl-gcc`.
3. `cargo build --release --target x86_64-unknown-linux-musl`
   (CC/linker = `musl-gcc`, чтобы `rusqlite/bundled` скомпилировал SQLite под target).
4. Проверяет, что бинарь статический (`ldd` → statically linked).
5. Пакует `build/vpsagent-v{ВЕРСИЯ}-x86_64-unknown-linux-musl.tar.gz` (внутри —
   один файл `vpsagent`, как ожидает `scripts/install.sh`).
6. Считает SHA256 → `build/vpsagent-v{ВЕРСИЯ}-x86_64-unknown-linux-musl.tar.gz.sha256`.

## Публикация в GitHub Releases

Артефакты названы по конвенции, которую ждут `scripts/install.sh` и
`vpsagent upgrade`:

```
build/vpsagent-v0.2.0-x86_64-unknown-linux-musl.tar.gz
build/vpsagent-v0.2.0-x86_64-unknown-linux-musl.tar.gz.sha256
```

Загрузка через `gh` (если установлен):

```bash
gh release create v0.2.0 --generate-notes \
  build/vpsagent-v0.2.0-x86_64-unknown-linux-musl.tar.gz \
  build/vpsagent-v0.2.0-x86_64-unknown-linux-musl.tar.gz.sha256
```

Или вручную: https://github.com/askidmobile/VPSAgent/releases/new → тег `v0.2.0`
→ прикрепить оба файла из `build/` → Publish.

## Установка на сервер (после публикации релиза)

```bash
# В ~/.local/bin (без sudo):
curl -fsSL https://github.com/askidmobile/VPSAgent/releases/latest/download/install.sh | sh

# Системно в /usr/local/bin (sudo):
curl -fsSL https://github.com/askidmobile/VPSAgent/releases/latest/download/install.sh | sudo sh
```

`install.sh` скачивает tar.gz, проверяет SHA256, распаковывает `vpsagent` в
`BINDIR` (по умолчанию `~/.local/bin`; под sudo — `/usr/local/bin`), делает
исполняемым и проверяет `--version`.

Для обновления уже установленного бинаря (без переустановки):

```bash
vpsagent upgrade           # проверить и установить новую версию
vpsagent upgrade --check   # только проверить
vpsagent upgrade --force   # переустановить даже если версия та же
```

## Установка без GitHub (прямой перенос tar.gz)

Если релиз не публикуется, можно скопировать артефакты с yttri-win на сервер
через `scp`:

```bash
# С yttri-win (WSL) на сервер — оба файла рядом.
scp build/vpsagent-v0.2.0-x86_64-unknown-linux-musl.tar.gz* user@server:/tmp/

# На сервере:
tar -xzf /tmp/vpsagent-v0.2.0-x86_64-unknown-linux-musl.tar.gz -C ~/.local/bin vpsagent
chmod +x ~/.local/bin/vpsagent
~/.local/bin/vpsagent --version
```

## Сборка из исходников (не-musl, нативный Linux/macOS)

Для локальной/development-сборки без musl:

```bash
cargo build --release
# бинарь: target/release/vpsagent (динамическая линковка под текущую ОС)
```

Это удобно на macOS/разработке, но для распространения нужен статический
musl-бинарь (см. выше) — иначе тянется glibc целевого дистрибутива.

## История

- Ретроспектива daemon-self-daemonize (2026-08-03): сборка
  `x86_64-unknown-linux-musl` на yttri-win WSL — exit 0, 1м 09с, бинарь
  ELF 64-bit static-pie 9.8 МБ stripped. Smoke (daemonize/status/SIGHUP/stop)
  — всё работает в musl. SHA256 того бинаря зафиксирован в
  `docs/plans/2026-08-03-daemon-self-daemonize.md`.