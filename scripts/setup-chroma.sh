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
#   CHROMA_HOST   address to listen on             (default 127.0.0.1)
#   CHROMA_VERSION server package version          (default 1.5.9)
#   CHROMA_PORT   port to listen on                (default 8000)
#   CHROMA_COLLECTION collection to create          (default rss_articles)
#   CHROMA_TENANT tenant override                   (default from server identity)
#   CHROMA_DATABASE database override               (default_database, then default)
#   CHROMA_DATA   data directory                   (default ~/chroma-data)
#   CHROMA_VENV   python venv location             (default ~/chroma-venv)

set -e

PORT="${CHROMA_PORT:-8000}"
CHROMA_VERSION="${CHROMA_VERSION:-1.5.9}"
COLLECTION="${CHROMA_COLLECTION:-rss_articles}"
DATA_DIR="${CHROMA_DATA:-$HOME/chroma-data}"
VENV_DIR="${CHROMA_VENV:-$HOME/chroma-venv}"
HOST="${CHROMA_HOST:-127.0.0.1}"
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
    if ! is_running; then
        rm -f "$PID_FILE"
        green "ChromaDB is already stopped (port ${PORT})."
        return 0
    fi
    if [ ! -f "$PID_FILE" ]; then
        red "A server is using port ${PORT}, but it was not started by this helper."
        red "Refusing to kill an unrelated process. Stop it manually."
        return 1
    fi

    local pid command_line
    pid=$(cat "$PID_FILE")
    case "$pid" in
        ''|*[!0-9]*) red "Invalid PID file: ${PID_FILE}"; return 1 ;;
    esac
    command_line=$(ps -p "$pid" -o command= 2>/dev/null || true)
    if [[ "$command_line" != *"$VENV_DIR/bin/chroma"* \
        || "$command_line" != *" run "* \
        || "$command_line" != *"--port $PORT"* ]]; then
        red "PID ${pid} is not this helper's ChromaDB process. Refusing to kill it."
        return 1
    fi

    kill "$pid"
    rm -f "$PID_FILE"
    sleep 1
    if is_running; then
        red "ChromaDB did not stop; inspect ${LOG_FILE}."
        return 1
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
    --stop)   stop_server; exit $? ;;
    --status) status ;;
esac

echo "=================================="
echo "ChromaDB setup — RSS Reader"
echo "=================================="

# Ensure the app's collection exists in the server's v2 database. The Rust
# client does the same lazily, but creating it here makes a freshly initialized
# server immediately inspectable and keeps Docker/manual server setups equal
# to the local helper.
url_encode() {
    python3 - "$1" <<'PY'
import sys
from urllib.parse import quote
print(quote(sys.argv[1], safe=""))
PY
}

ensure_collection() {
    local base_url="http://localhost:${PORT}"
    local identity tenant payload tenant_path database_path database

    identity=$(curl -fsS -m 5 "${base_url}/api/v2/auth/identity") || return 1
    if [ -n "${CHROMA_TENANT:-}" ]; then
        tenant="$CHROMA_TENANT"
    else
        tenant=$(printf '%s' "$identity" | python3 -c '
import json
import sys
info = json.load(sys.stdin)
tenant = info.get("tenant") or "default_tenant"
print("default_tenant" if tenant == "*" else tenant)
') || return 1
    fi

    payload=$(python3 - "$COLLECTION" <<'PY'
import json
import sys
print(json.dumps({"name": sys.argv[1], "metadata": None, "get_or_create": True}))
PY
    ) || return 1
    tenant_path=$(url_encode "$tenant") || return 1

    local databases=("${CHROMA_DATABASE:-default_database}")
    if [ -z "${CHROMA_DATABASE:-}" ]; then
        databases+=("default")
    fi

    for database in "${databases[@]}"; do
        database_path=$(url_encode "$database") || return 1
        if curl -fsS -m 10 -X POST \
            -H "Content-Type: application/json" \
            --data "$payload" \
            "${base_url}/api/v2/tenants/${tenant_path}/databases/${database_path}/collections" \
            >/dev/null 2>&1; then
            green "Collection '${COLLECTION}' is ready (database: ${database})."
            return 0
        fi
    done

    return 1
}

ensure_collection_with_retry() {
    for _ in $(seq 1 10); do
        if ensure_collection; then
            return 0
        fi
        sleep 1
    done
    return 1
}

# --- 0. Already running? ---------------------------------------------------
if is_running; then
    green "ChromaDB is already running on port ${PORT}."
    if ! command -v python3 >/dev/null 2>&1; then
        red "python3 is required to initialize the ChromaDB collection."
        exit 1
    fi
    if ! ensure_collection_with_retry; then
        red "Failed to initialize collection '${COLLECTION}'."
        exit 1
    fi
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
    # Only remove a directory that is recognizably a virtual environment.
    # CHROMA_VENV is user-controlled; blindly `rm -rf`-ing it could erase a
    # home or project directory after a typo.
    case "$VENV_DIR" in
        ''|'/'|'.'|'..'|"$HOME"|"$HOME/")
            red "Unsafe CHROMA_VENV path: ${VENV_DIR}"
            exit 1
            ;;
    esac
    if [ -e "$VENV_DIR" ]; then
        if [ ! -f "$VENV_DIR/pyvenv.cfg" ]; then
            red "${VENV_DIR} exists but is not a recognizable Python venv."
            red "Refusing to delete it; move/remove it manually, then retry."
            exit 1
        fi
        rm -rf -- "$VENV_DIR"
    fi
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
chroma_version_ok() {
    "$VENV_DIR/bin/python" -c "import chromadb; raise SystemExit(0 if chromadb.__version__ == '${CHROMA_VERSION}' else 1)" >/dev/null 2>&1
}

if ! chroma_version_ok; then
    echo "Installing chromadb==${CHROMA_VERSION}..."
    "$VENV_DIR/bin/pip" install --upgrade pip
    "$VENV_DIR/bin/pip" install "chromadb==${CHROMA_VERSION}"
else
    echo "chromadb==${CHROMA_VERSION} already installed."
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

if ! ensure_collection_with_retry; then
    red "ChromaDB is running, but collection '${COLLECTION}' could not be initialized."
    red "Check the server log and CHROMA_TENANT/CHROMA_DATABASE overrides."
    exit 1
fi

cat <<EOF

--------------------------------------------------------------
Next steps in the app:
  1. Click "Semantic DB" in the items-panel header
  2. Host: http://localhost   Port: ${PORT}   Collection: ${COLLECTION}
  3. Check "Enable ChromaDB" → click "Enable & Index"
  4. The app verifies the server, creates the collection if needed,
     and indexes all downloaded articles without a restart.
     "Re-Index All Items" rebuilds from scratch.

To stop:  ./scripts/setup-chroma.sh --stop
To check: ./scripts/setup-chroma.sh --status
--------------------------------------------------------------
EOF
