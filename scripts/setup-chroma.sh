#!/bin/bash
#
# Custom RSS Reader — ChromaDB install & run helper (Linux/macOS)
#
# Sets up a Python venv with ChromaDB and starts the server, ready for the
# app's "Semantic DB" feature (host http://localhost, port 8000).
#
# Usage:
#   ./scripts/setup-chroma.sh            # install if needed, then start
#   ./scripts/setup-chroma.sh --stop     # stop the running server
#   ./scripts/setup-chroma.sh --status   # check if it's running
#
# Env overrides:
#   CHROMA_PORT   port to listen on                (default 8000)
#   CHROMA_DATA   data directory                   (default ~/chroma-data)
#   CHROMA_VENV   python venv location             (default ~/chroma-venv)

set -e

PORT="${CHROMA_PORT:-8000}"
DATA_DIR="${CHROMA_DATA:-$HOME/chroma-data}"
VENV_DIR="${CHROMA_VENV:-$HOME/chroma-venv}"
HOST="0.0.0.0"
PID_FILE="$HOME/.chroma-server.pid"
LOG_FILE="$HOME/.chroma-server.log"

red()   { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
yellow(){ printf '\033[33m%s\033[0m\n' "$*"; }

# ChromaDB 1.x removed the v1 API ("Unimplemented"), and the Rust client
# talks v2 anyway — probe the v2 heartbeat.
is_running() {
    curl -s -m 3 "http://localhost:${PORT}/api/v2/heartbeat" >/dev/null 2>&1
}

stop_server() {
    if is_running; then
        if [ -f "$PID_FILE" ]; then
            kill "$(cat "$PID_FILE")" 2>/dev/null || true
        fi
        pkill -f "chroma run.*--port ${PORT}" 2>/dev/null || true
        sleep 1
    fi
    green "ChromaDB stopped (port ${PORT})."
}

status() {
    if is_running; then
        green "ChromaDB is RUNNING on port ${PORT} (data: ${DATA_DIR})"
        exit 0
    fi
    yellow "ChromaDB is NOT running."
    exit 1
}

# --- --stop / --status shortcuts ------------------------------------------
case "${1:-}" in
    --stop)   stop_server; exit 0 ;;
    --status) status ;;
esac

echo "=================================="
echo "ChromaDB setup — RSS Reader"
echo "=================================="

# --- 0. Already running? ---------------------------------------------------
if is_running; then
    green "ChromaDB is already running on port ${PORT}. Nothing to do."
    exit 0
fi

# --- 1. Python -------------------------------------------------------------
if ! command -v python3 >/dev/null 2>&1; then
    red "python3 not found. Install it first: sudo apt install python3 python3-venv"
    exit 1
fi
echo "Using Python: $(python3 --version)"

# --- 2. Create the venv (idempotent) ---------------------------------------
#
# A venv is valid only when its python has pip. Debian/Ubuntu strip
# `ensurepip` out of python3, so `python3 -m venv` silently creates a
# broken venv there (and `python3 -m venv --help` still succeeds, which is
# why we can't use it as the check). `uv venv --seed` needs no ensurepip
# and is preferred when available.
venv_ok() {
    [ -x "$VENV_DIR/bin/python" ] && "$VENV_DIR/bin/python" -m pip --version >/dev/null 2>&1
}

if ! venv_ok; then
    # Remove a half-created venv so it can't block retries.
    rm -rf "$VENV_DIR"
    if command -v uv >/dev/null 2>&1; then
        echo "Creating venv with uv at ${VENV_DIR} ..."
        uv venv --seed "$VENV_DIR"
    else
        if ! python3 -c "import ensurepip" >/dev/null 2>&1; then
            yellow "python3-venv not available — installing it (sudo)..."
            sudo apt-get install -y python3-venv python3-pip
        fi
        echo "Creating venv at ${VENV_DIR} ..."
        python3 -m venv "$VENV_DIR"
    fi
fi

if ! venv_ok; then
    red "Failed to create a working venv at ${VENV_DIR}."
    red "Install python3-venv (sudo apt install python3-venv) or uv, then retry."
    exit 1
fi
echo "Venv ready at ${VENV_DIR}."

# --- 3. Install chromadb (idempotent) --------------------------------------
# Note: embeddings are computed client-side by the app itself (ONNX model,
# ~120 MB, downloaded into ~/.rss-reader/models on first semantic use) —
# ChromaDB 1.x has no server-side embedding function anymore, so nothing
# model-related is downloaded here.
if ! "$VENV_DIR/bin/python" -c "import chromadb" >/dev/null 2>&1; then
    echo "Installing chromadb..."
    "$VENV_DIR/bin/pip" install --upgrade pip
    "$VENV_DIR/bin/pip" install chromadb
else
    echo "chromadb already installed."
fi

# --- 4. Start the server ---------------------------------------------------
mkdir -p "$DATA_DIR"
echo "Starting ChromaDB on ${HOST}:${PORT} (data: ${DATA_DIR}) ..."

nohup "$VENV_DIR/bin/chroma" run \
    --host "$HOST" \
    --port "$PORT" \
    --path "$DATA_DIR" \
    >"$LOG_FILE" 2>&1 &

CHROMA_PID=$!
echo "$CHROMA_PID" > "$PID_FILE"
echo "PID ${CHROMA_PID} — logs: ${LOG_FILE}"

# --- 5. Wait for it to come up --------------------------------------------
echo "Waiting for the server to become ready..."
for _ in $(seq 1 30); do
    if is_running; then
        green "ChromaDB is up!"
        break
    fi
    sleep 1
done

if ! is_running; then
    red "ChromaDB failed to start. Check the log:"
    tail -20 "$LOG_FILE" 2>/dev/null || true
    exit 1
fi

cat <<EOF

--------------------------------------------------------------
Next steps in the app:
  1. Click "Semantic DB" in the items-panel header
  2. Host: http://localhost   Port: ${PORT}   Collection: rss_articles
  3. Check "Enable ChromaDB" → Save → Restart the app
  4. On restart, ALL downloaded articles are indexed automatically
     (watermark sync); "Re-Index All Items" rebuilds from scratch.

To stop:  ./scripts/setup-chroma.sh --stop
To check: ./scripts/setup-chroma.sh --status
--------------------------------------------------------------
EOF
