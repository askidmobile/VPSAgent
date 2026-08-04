#!/bin/sh
# build-musl.sh — сборка статического musl-бинаря vpsagent для Linux x86_64.
#
# Запускать ВНУТРИ WSL на yttri-win (или любой Linux x86_64 с musl-tools):
#
#   git clone https://github.com/askidmobile/VPSAgent ~/VPSAgent
#   cd ~/VPSAgent && scripts/build-musl.sh
#
# Итог в build/: vpsagent-v{VER}-x86_64-unknown-linux-musl.tar.gz + .sha256
# (имена совпадают с scripts/install.sh и `vpsagent upgrade`).
#
# Требования: rustup, target x86_64-unknown-linux-musl, musl-tools.
set -eu

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

# --- Конфигурация -----------------------------------------------------------

# Версия из Cargo.toml (workspace.package.version).
VERSION="$(awk '/^\[workspace\.package\]/{f=1} f && /^version *= *"/{gsub(/[^0-9.]/,"",$3); print $3; exit}' Cargo.toml)"
[ -n "$VERSION" ] || { echo "не удалось определить версию из Cargo.toml"; exit 1; }

TARGET="x86_64-unknown-linux-musl"
ARCH="x86_64"
OUT_DIR="$ROOT/build"
TARBALL="vpsagent-v${VERSION}-${ARCH}-unknown-linux-musl.tar.gz"
CHECKSUM="${TARBALL}.sha256"

# --- Проверки окружения -----------------------------------------------------

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mwarn:\033[0m %s\n' "$*" >&2; }
error() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

command -v cargo >/dev/null 2>&1 || error "cargo не найден — установи rustup"
command -v rustup >/dev/null 2>&1 || error "rustup не найден"

# Целевой target должен быть установлен.
if ! rustup target list --installed 2>/dev/null | grep -q "^${TARGET}$"; then
    info "добавляю target ${TARGET}..."
    rustup target add "$TARGET"
fi

# musl-tools даёт musl-gcc (C-кросс-тулчейн для rusqlite/bundled).
# На Debian/Ubuntu WSL: sudo apt-get install -y musl-tools.
if ! command -v musl-gcc >/dev/null 2>&1; then
    # cargo может найти linker сам, но проверим наличие пакета.
    if command -v dpkg >/dev/null 2>&1; then
        dpkg -s musl-tools >/dev/null 2>&1 \
            || error "musl-gcc не найден — установи: sudo apt-get install -y musl-tools"
    else
        warn "musl-gcc не найден в PATH — если сборка упадёт на link, установи musl-tools"
    fi
fi

# --- Сборка -----------------------------------------------------------------

info "собираю vpsagent v${VERSION} для ${TARGET}..."
# CC/musl-gcc: rusqlite bundled-фича компилирует SQLite из исходников,
# нужен C-компилятор под target. musl-gcc — стандартный для этого target.
export CC_x86_64_unknown_linux_musl="${CC_x86_64_unknown_linux_musl:-musl-gcc}"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="${CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER:-musl-gcc}"

cargo build --release --target "$TARGET"

BIN="$ROOT/target/${TARGET}/release/vpsagent"
[ -f "$BIN" ] || error "бинарь не найден после сборки: $BIN"

# --- Проверка бинаря --------------------------------------------------------

info "бинарь:"
file "$BIN" || true
SIZE="$(du -h "$BIN" | cut -f1)"
info "  размер: ${SIZE}"

# Должен быть статическим ELF (static-pie), без динамических зависимостей.
if command -v ldd >/dev/null 2>&1; then
    if ldd "$BIN" 2>&1 | grep -q "not a dynamic executable\|statically linked"; then
        info "  линковка: статическая ✓"
    else
        echo "  warn: бинарь не статический — проверь ldd:"
        ldd "$BIN" || true
    fi
fi

# --- Упаковка артефактов ----------------------------------------------------

mkdir -p "$OUT_DIR"
info "пакую ${TARBALL}..."
# tar.gz содержит ОДИН файл vpsagent (так же, как распаковывает install.sh:
# `tar -xzf ... -C $BINDIR vpsagent`).
tar -C "$(dirname "$BIN")" -czf "$OUT_DIR/$TARBALL" "$(basename "$BIN")"

info "считаю SHA256..."
if command -v sha256sum >/dev/null 2>&1; then
    HASH="$(sha256sum "$OUT_DIR/$TARBALL" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
    HASH="$(shasum -a 256 "$OUT_DIR/$TARBALL" | awk '{print $1}')"
else
    error "ни sha256sum, ни shasum не найдены"
fi
printf '%s  %s\n' "$HASH" "$TARBALL" > "$OUT_DIR/$CHECKSUM"

# --- Итог -------------------------------------------------------------------

info "готово. Артефакты в ${OUT_DIR}/:"
ls -1 "$OUT_DIR/$TARBALL" "$OUT_DIR/$CHECKSUM" 2>/dev/null
info "  SHA256: ${HASH}"
cat <<EOF

Загрузи артефакты в GitHub Release v${VERSION}:
  gh release create v${VERSION} --generate-notes \
    "$OUT_DIR/$TARBALL" "$OUT_DIR/$CHECKSUM"

Или вручную: https://github.com/askidmobile/VPSAgent/releases/new?tag=v${VERSION}

После публикации установка на сервер:
  curl -fsSL https://github.com/askidmobile/VPSAgent/releases/latest/download/install.sh | sh
EOF