#!/bin/bash
#
# Custom RSS Reader — initialization (Linux/macOS)
#
# Sets up the complete dev environment:
#   1. Toolchain checks: Node.js, npm, Rust, system deps (Tauri prerequisites)
#   2. Python 3 (required by the ChromaDB server)
#   3. npm install + frontend build
#   4. `cargo check` on src-tauri — validates that `npm run tauri dev/build` compiles
#   5. ChromaDB server: venv + pip install + start (via scripts/setup-chroma.sh)
#   6. Embedding model pre-download into ~/.rss-reader/models (mirrors HF,
#      huggingface.co, hf-mirror.com) so the first semantic search is instant
#
# Usage:
#   ./scripts/init.sh            # full setup (skips the long production build)
#   ./scripts/init.sh --build    # also run `npm run tauri build`
#
# Env overrides (passed through to setup-chroma.sh / the Rust backend):
#   CHROMA_PORT        ChromaDB port           (default 8000)
#   CHROMA_VERSION     server package version  (default 1.5.9)
#   CHROMA_COLLECTION  collection to create    (default rss_articles)
#   CHROMA_TENANT      tenant override         (default from server identity)
#   CHROMA_DATABASE    database override       (default_database, then default)
#   CHROMA_VENV        ChromaDB venv location  (default ~/chroma-venv)
#   CHROMA_DATA        ChromaDB data dir       (default ~/chroma-data)
#   CHROMA_MODEL_DIR   embedding model dir     (default ~/.rss-reader/models)
#   HF_ENDPOINT        HuggingFace mirror base (e.g. https://hf-mirror.com)
#   SKIP_CARGO_CHECK   set to 1 to skip `cargo check`
#   SKIP_CHROMA        set to 1 to skip ChromaDB server setup
#   SKIP_MODEL         set to 1 to skip the embedding model download

set -e

# --- CLI flags -------------------------------------------------------------

DO_BUILD=0
for arg in "$@"; do
    case "$arg" in
        --build)   DO_BUILD=1 ;;
        --help|-h)
            sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
            exit 0 ;;
        *) echo "Unknown option: $arg (see ./scripts/init.sh --help)"; exit 1 ;;
    esac
done

# --- Output helpers --------------------------------------------------------

red()    { printf '\033[31m%s\033[0m\n' "$*"; }
green()  { printf '\033[32m%s\033[0m\n' "$*"; }
yellow() { printf '\033[33m%s\033[0m\n' "$*"; }
step()   { echo ""; echo "─── $* ───"; }

# This script must run from the repo root.
cd "$(dirname "$0")/.."

echo "========================================"
echo "Custom RSS Reader — Initialization"
echo "========================================"

# Detect OS
OS="$(uname -s)"
case "${OS}" in
    Linux*)  MACHINE=Linux ;;
    Darwin*) MACHINE=Mac ;;
    *)       red "Unsupported OS: ${OS}"; exit 1 ;;
esac
green "Detected OS: ${MACHINE}"

# ---------------------------------------------------------------------------
step "1/6  Toolchain checks"
# ---------------------------------------------------------------------------

# Node.js
if ! command -v node >/dev/null 2>&1; then
    red "Node.js is not installed. Install it from https://nodejs.org/"
    exit 1
fi
green "Node.js $(node -v) found"

# npm
if ! command -v npm >/dev/null 2>&1; then
    red "npm is not installed. Install it from https://nodejs.org/"
    exit 1
fi
green "npm $(npm -v) found"

# curl (needed for rustup + the embedding model download)
if ! command -v curl >/dev/null 2>&1; then
    yellow "curl not found — the embedding model download will be skipped."
fi

# Rust (required for Tauri)
if ! command -v rustc >/dev/null 2>&1; then
    yellow "Rust is not installed — installing via rustup..."
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        # shellcheck disable=SC1090
        source "$HOME/.cargo/env"
    else
        red "curl is not installed. Install Rust manually from https://rustup.rs/"
        exit 1
    fi
fi
green "Rust $(rustc --version) found"

# System dependencies (Tauri prerequisites)
step "2/6  System dependencies"
if [[ "${MACHINE}" == "Linux" ]]; then
    # Tauri v2 links against webkit2gtk-4.1 (older distros still carry 4.0).
    if [[ -x /usr/bin/apt-get ]]; then
        # Ubuntu / Debian
        if dpkg -l libwebkit2gtk-4.1-dev >/dev/null 2>&1; then
            green "WebKitGTK 4.1 dev libraries found"
        else
            yellow "Installing Tauri dependencies (apt)..."
            sudo apt-get update
            local_apt_pkgs="libwebkit2gtk-4.1-dev build-essential curl wget file \
                libxdo-dev libssl-dev libgtk-3-dev libayatana-appindicator3-dev \
                librsvg2-dev python3 python3-venv python3-pip"
            # shellcheck disable=SC2086
            if ! sudo apt-get install -y $local_apt_pkgs; then
                # Older distros (Ubuntu <22.04, Debian <12) only ship webkit2gtk-4.0
                yellow "webkit2gtk-4.1 unavailable — falling back to webkit2gtk-4.0"
                # shellcheck disable=SC2086
                sudo apt-get install -y ${local_apt_pkgs/libwebkit2gtk-4.1-dev/libwebkit2gtk-4.0-dev}
            fi
        fi
    elif command -v dnf >/dev/null 2>&1; then
        # Fedora
        if ! rpm -q webkit2gtk4.1-devel >/dev/null 2>&1; then
            yellow "Installing Tauri dependencies (dnf)..."
            sudo dnf install -y \
                webkit2gtk4.1-devel \
                openssl-devel \
                curl wget file \
                libappindicator-gtk3-devel \
                librsvg2-devel
        else
            green "WebKitGTK dev libraries found"
        fi
    elif command -v pacman >/dev/null 2>&1; then
        # Arch Linux
        if ! pacman -Q webkit2gtk-4.1 >/dev/null 2>&1; then
            yellow "Installing Tauri dependencies (pacman)..."
            sudo pacman -S --needed \
                webkit2gtk-4.1 \
                base-devel curl wget file \
                openssl \
                libappindicator-gtk3 \
                librsvg
        else
            green "WebKitGTK found"
        fi
    else
        yellow "No apt/dnf/pacman found — install the packages from https://tauri.app/start/prerequisites/ manually."
    fi
elif [[ "${MACHINE}" == "Mac" ]]; then
    if ! command -v brew >/dev/null 2>&1; then
        yellow "Homebrew not found — installing..."
        /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
    else
        green "Homebrew found"
    fi
    if ! xcode-select -p >/dev/null 2>&1; then
        yellow "Xcode Command Line Tools missing — run 'xcode-select --install' and retry."
    else
        green "Xcode Command Line Tools found"
    fi
fi

# Python 3 (required by the ChromaDB server)
step "3/6  Python"
if command -v python3 >/dev/null 2>&1; then
    green "python3 $(python3 --version 2>&1 | cut -d' ' -f2) found"
else
    yellow "python3 not found. Install it first: sudo apt install python3 python3-venv" \
           "(or via your package manager), then rerun this script."
fi

# ---------------------------------------------------------------------------
step "4/6  npm install + frontend build"
# ---------------------------------------------------------------------------

npm install

# The Tauri CLI ships as a devDependency (@tauri-apps/cli) — no global install.
if [[ -x node_modules/.bin/tauri ]]; then
    green "Tauri CLI v$(node_modules/.bin/tauri --version) found (local devDependency)"
else
    red "Tauri CLI missing after 'npm install' — check the npm install output."
    exit 1
fi

npm run build

# ---------------------------------------------------------------------------
step "5/6  Rust backend check"
# ---------------------------------------------------------------------------

# `cargo check` compiles src-tauri far faster than a full build and proves that
# `npm run tauri dev` / `npm run tauri build` will initialize correctly.
if [[ "${SKIP_CARGO_CHECK:-0}" != "1" ]]; then
    echo "First run compiles all Rust dependencies — this takes a few minutes."
    (cd src-tauri && cargo check)
    green "Rust backend compiles OK — 'npm run tauri dev/build' will work."
else
    yellow "Skipping cargo check (SKIP_CARGO_CHECK=1)."
fi

# Optional full production build
if [[ "$DO_BUILD" == "1" ]]; then
    echo ""
    echo "Running full production build (npm run tauri build)..."
    npm run tauri build
fi

# ---------------------------------------------------------------------------
step "6/6  ChromaDB + embedding model"
# ---------------------------------------------------------------------------

# ChromaDB server (venv + chromadb install + start on port 8000).
# setup-chroma.sh is idempotent: it no-ops if the server is already running.
if [[ "${SKIP_CHROMA:-0}" != "1" ]]; then
    if bash scripts/setup-chroma.sh; then
        green "ChromaDB is set up and running."
    else
        yellow "ChromaDB setup failed — the Semantic Search feature will be disabled."
        yellow "Rerun 'bash scripts/setup-chroma.sh' after fixing the issue."
    fi
else
    yellow "Skipping ChromaDB setup (SKIP_CHROMA=1)."
fi

# Embedding model pre-download.
# The app embeds documents client-side (ChromaDB 1.x has no server-side
# embedding function) with a quantized ONNX model. Pre-fetching it here means
# the first semantic search/index doesn't stall on a ~120 MB download.
# Mirrors and paths mirror src-tauri/src/chroma/embeddings.rs.
MODEL_REPO="sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"
MODEL_FILES=("tokenizer.json" "onnx/model_quint8_avx2.onnx")
MODEL_DIR="${CHROMA_MODEL_DIR:-$HOME/.rss-reader/models}/$MODEL_REPO"

model_mirrors() {
    [[ -n "${HF_ENDPOINT:-}" ]] && echo "$HF_ENDPOINT"
    echo "https://huggingface.co"
    echo "https://hf-mirror.com"
}

download_model() {
    local file dest url got mirror
    for file in "${MODEL_FILES[@]}"; do
        dest="$MODEL_DIR/$file"
        if [[ -f "$dest" ]]; then
            green "  ✓ $file already present ($(du -h "$dest" | cut -f1))"
            continue
        fi
        mkdir -p "$(dirname "$dest")"
        got=0
        for mirror in $(model_mirrors); do
            url="$mirror/$MODEL_REPO/resolve/main/$file"
            echo "  downloading $file from $mirror ..."
            if curl -fL --retry 3 --connect-timeout 15 -o "$dest.part" "$url" 2>/dev/null; then
                mv "$dest.part" "$dest"
                green "  ✓ $file ($(du -h "$dest" | cut -f1))"
                got=1
                break
            else
                yellow "    failed from $mirror"
                rm -f "$dest.part"
            fi
        done
        if [[ "$got" != "1" ]]; then
            red "  ✗ could not download $file from any mirror"
            return 1
        fi
    done
}

if [[ "${SKIP_MODEL:-0}" != "1" ]]; then
    if command -v curl >/dev/null 2>&1; then
        echo "Embedding model: $MODEL_REPO"
        echo "Target dir:      $MODEL_DIR"
        if download_model; then
            green "Embedding model ready — first semantic search will load it instantly."
        else
            yellow "Model download failed — the app will retry automatically on first semantic use."
        fi
    else
        yellow "curl not installed — skipping model pre-download (the app downloads it on demand)."
    fi
else
    yellow "Skipping model download (SKIP_MODEL=1)."
fi

# ---------------------------------------------------------------------------
echo ""
echo "================================"
echo "✅ Initialization complete!"
echo "================================"
echo ""
echo "Available commands:"
echo "  npm run tauri dev     - Start development server (hot reload)"
echo "  npm run tauri build   - Build production bundles"
echo "  npm run build         - Build frontend only"
echo "  npm run dev           - Start frontend dev server"
echo ""
echo "Semantic search:"
echo "  ChromaDB server:      http://localhost:${CHROMA_PORT:-8000}"
echo "  ChromaDB collection:  ${CHROMA_COLLECTION:-rss_articles}"
echo "  Model location:       ${CHROMA_MODEL_DIR:-$HOME/.rss-reader/models}"
echo "  Stop/status ChromaDB: bash scripts/setup-chroma.sh --stop | --status"
echo ""
echo "For more information, see README.md"