#!/bin/sh
# install.sh — установщик vpsagent (curl|sh паттерн).
#
# Установка:
#   curl -fsSL https://github.com/askidmobile/VPSAgent/releases/latest/download/install.sh | sh
#
# Или конкретная версия:
#   curl -fsSL https://github.com/askidmobile/VPSAgent/releases/download/v0.1.0/install.sh | sh
#
# Установка в /usr/local/bin (нужен sudo):
#   curl -fsSL .../install.sh | sudo sh
#
# Указать свой каталог:
#   curl -fsSL .../install.sh | BINDIR=/opt/vpsagent sh
set -eu

# --- Конфигурация -----------------------------------------------------------

OWNER="askidmobile"
REPO="VPSAgent"
VERSION="${VPSAGENT_VERSION:-latest}"
BINDIR="${BINDIR:-${HOME}/.local/bin}"

# Если запущены под sudo — ставим системно в /usr/local/bin (если BINDIR не задан).
if [ "$(id -u)" = "0" ] && [ -z "${BINDIR:-}" ]; then
    BINDIR="/usr/local/bin"
fi

# --- Утилиты ----------------------------------------------------------------

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mwarn:\033[0m %s\n' "$*" >&2; }
error() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || error "не найдена команда '$1' — нужна для установки"
}

# --- Проверки окружения -----------------------------------------------------

info "Установка vpsagent"

# Архитектура: только x86_64 (musl-сборка).
ARCH="$(uname -m)"
case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    *)
        error "неподдерживаемая архитектура: $ARCH (релиз собирается только для x86_64). " \
              "Соберите из исходников: cargo build --release --target x86_64-unknown-linux-musl"
        ;;
esac

# ОС: только Linux (musl-статический бинарь).
OS="$(uname -s)"
case "$OS" in
    Linux) ;;
    Darwin)
        warn "macOS: musl-бинарь не запустится на macOS. Используйте сборку из исходников:"
        warn "  cargo build --release"
        error "установка прервана (macOS не поддерживается этим installer)"
        ;;
    *) error "неподдерживаемая ОС: $OS (нужен Linux)" ;;
esac

# Нужные команды.
need_cmd curl
need_cmd tar
need_cmd shasum 2>/dev/null || need_cmd sha256sum 2>/dev/null || true
HAS_SHASUM=0
command -v shasum >/dev/null 2>&1 && HAS_SHASUM=1
HAS_SHA256SUM=0
command -v sha256sum >/dev/null 2>&1 && HAS_SHA256SUM=1

# --- Определение URL релиза -------------------------------------------------

# Тег версии: если latest — узнаём через GitHub API, иначе используем как есть.
if [ "$VERSION" = "latest" ]; then
    info "определяю последнюю версию..."
    VERSION="$(curl -fsSL "https://api.github.com/repos/${OWNER}/${REPO}/releases/latest" \
        | grep -m1 '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')" \
        || error "не удалось узнать последнюю версию через GitHub API"
    [ -n "$VERSION" ] || error "пустой тег версии"
fi
info "версия: ${VERSION}"

BASE_URL="https://github.com/${OWNER}/${REPO}/releases/download/${VERSION}"
TARBALL="vpsagent-${VERSION}-${ARCH}-unknown-linux-musl.tar.gz"
CHECKSUM="vpsagent-${VERSION}-${ARCH}-unknown-linux-musl.tar.gz.sha256"
TARBALL_URL="${BASE_URL}/${TARBALL}"
CHECKSUM_URL="${BASE_URL}/${CHECKSUM}"

# --- Временный каталог ------------------------------------------------------

TMPDIR="$(mktemp -d 2>/dev/null || mktemp -d -t vpsagent)"
cleanup() { rm -rf "$TMPDIR"; }
trap cleanup EXIT INT TERM

# --- Скачивание --------------------------------------------------------------

info "скачиваю ${TARBALL}..."
curl -fSL "$TARBALL_URL" -o "${TMPDIR}/${TARBALL}" \
    || error "не удалось скачать ${TARBALL_URL}"

# --- Проверка SHA256 (если доступен) ----------------------------------------

if [ "$HAS_SHASUM" = "1" ] || [ "$HAS_SHA256SUM" = "1" ]; then
    info "проверяю SHA256..."
    curl -fsSL "$CHECKSUM_URL" -o "${TMPDIR}/${CHECKSUM}" 2>/dev/null || true
    if [ -f "${TMPDIR}/${CHECKSUM}" ]; then
        EXPECTED="$(awk '{print $1}' "${TMPDIR}/${CHECKSUM}" | head -1)"
        if [ "$HAS_SHASUM" = "1" ]; then
            ACTUAL="$(shasum -a 256 "${TMPDIR}/${TARBALL}" | awk '{print $1}')"
        else
            ACTUAL="$(sha256sum "${TMPDIR}/${TARBALL}" | awk '{print $1}')"
        fi
        if [ "$EXPECTED" != "$ACTUAL" ]; then
            error "несовпадение SHA256: ожидается ${EXPECTED}, получено ${ACTUAL}"
        fi
        info "SHA256 OK"
    else
        warn "файл контрольной суммы недоступен — пропускаю проверку"
    fi
else
    warn "ни shasum, ни sha256sum не найдены — пропускаю проверку целостности"
fi

# --- Установка --------------------------------------------------------------

mkdir -p "$BINDIR" || error "не удалось создать каталог $BINDIR"

info "устанавливаю в ${BINDIR}..."
tar -xzf "${TMPDIR}/${TARBALL}" -C "$BINDIR" vpsagent \
    || error "не удалось распаковать архив"

chmod +x "${BINDIR}/vpsagent" || error "не удалось сделать бинарь исполняемым"

# --- Проверка PATH ----------------------------------------------------------

case ":${PATH}:" in
    *":${BINDIR}:"*) ;;
    *)
        warn "${BINDIR} не в PATH. Добавьте в ~/.bashrc или ~/.zshrc:"
        warn "  export PATH=\"${BINDIR}:\$PATH\""
        ;;
esac

# --- Финал ------------------------------------------------------------------

INSTALLED="${BINDIR}/vpsagent"
info "установка завершена: ${INSTALLED}"

# Проверка запуска (не падаем, если не сработает — например, нет прав на запись в cwd).
if "${INSTALLED}" --version >/dev/null 2>&1; then
    info "$("${INSTALLED}" --version)"
else
    warn "не удалось запустить ${INSTALLED} — проверьте права или архитектуру"
fi

cat <<EOF

Команды:
  vpsagent init              первичная настройка (провайдеры, ключи)
  vpsagent daemon            запустить демона (переживает SSH)
  vpsagent daemon status     статус
  vpsagent daemon stop       остановить
  vpsagent run "задача"       headless-прогон
  vpsagent attach [id]        подключиться к сессии

Документация: https://github.com/${OWNER}/${REPO}
EOF