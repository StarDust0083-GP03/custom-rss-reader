# Initialization Scripts

This directory contains cross-platform initialization scripts for setting up the Custom RSS Reader development environment.

## Scripts

### Linux / macOS
```bash
./scripts/init.sh
```

This script will:
- Check for Node.js, npm, curl, and Python 3
- Install Rust via rustup if not present
- Install system dependencies (WebKitGTK 4.1 for Tauri v2, build tools, etc.)
- Install npm dependencies
- Build the frontend (`npm run build`)
- Validate the Rust backend with `cargo check` (proves `npm run tauri dev/build` will compile)
- Set up and start the pinned ChromaDB server and idempotently create the `rss_articles` collection (server on port 8000)
- Pre-download the architecture-specific multilingual embedding model into `~/.rss-reader/models`, pinned to an immutable revision and SHA-256 verified

Options:
```bash
./scripts/init.sh --build   # also run the full production build (npm run tauri build)
./scripts/init.sh --help    # show usage
```

Environment overrides (also understood by the app and `setup-chroma.sh`):

| Variable | Default | Purpose |
|---|---|---|
| `CHROMA_HOST` | `127.0.0.1` | ChromaDB bind address |
| `CHROMA_PORT` | `8000` | ChromaDB port |
| `CHROMA_VERSION` | `1.5.9` | ChromaDB server package version |
| `CHROMA_COLLECTION` | `rss_articles` | Collection to create |
| `CHROMA_TENANT` | server identity | Optional Chroma tenant override |
| `CHROMA_DATABASE` | `default_database`, then `default` | Optional database override |
| `CHROMA_VENV` | `~/chroma-venv` | ChromaDB Python venv location |
| `CHROMA_DATA` | `~/chroma-data` | ChromaDB data directory |
| `CHROMA_MODEL_DIR` | `~/.rss-reader/models` | Embedding model directory |
| `HF_ENDPOINT` | — | HuggingFace mirror base (e.g. `https://hf-mirror.com`) |
| `SKIP_CARGO_CHECK` | — | Set to `1` to skip `cargo check` |
| `SKIP_CHROMA` | — | Set to `1` to skip ChromaDB server setup |
| `SKIP_MODEL` | — | Set to `1` to skip the embedding model download |

### Windows
```powershell
.\scripts\init.ps1
```

This script will:
- Check for Node.js, npm, Python 3, WebView2 runtime, and Visual Studio C++ Build Tools
- Install Rust via rustup if not present
- Install npm dependencies
- Build the frontend (`npm run build`)
- Validate the Rust backend with `cargo check`
- Set up and start the ChromaDB server and idempotently create the configured collection
- Pre-download the multilingual embedding model (~\.rss-reader\models)

Options:
```powershell
.\scripts\init.ps1 -Build   # also run the full production build (npm run tauri build)
```

The same `CHROMA_HOST`, `CHROMA_PORT`, `CHROMA_VERSION`, `CHROMA_COLLECTION`, `CHROMA_TENANT`,
`CHROMA_DATABASE`, `CHROMA_VENV`, `CHROMA_DATA`, `CHROMA_MODEL_DIR`, `HF_ENDPOINT`,
`SKIP_CARGO_CHECK`, `SKIP_CHROMA`, and `SKIP_MODEL` environment overrides apply.

## What the initialization does, in detail

1. **Toolchain** — Node.js, npm, Rust (rustup), and platform prerequisites.
   Tauri v2 requires `libwebkit2gtk-4.1-dev` on Linux (the scripts install the
   4.0 package too on distros that only ship that).
2. **npm install** — installs everything including the Tauri CLI, which is a
   regular **devDependency** (no global install needed).
3. **cargo check** — compiles the Rust side of `src-tauri/` (fast "does it
   compile?" check) so that `npm run tauri dev` / `npm run tauri build` won't
   fail mid-way through on a missing dependency.
4. **ChromaDB** — creates the Python venv, installs pinned `chromadb`, starts the
   server on port 8000, and creates the `rss_articles` collection through the
   Chroma v2 API (`scripts/setup-chroma.sh` on Linux/macOS; equivalent inline
   logic on Windows). Idempotent: a running server is detected, left alone,
   and its collection is ensured. The server is not managed by npm/tauri; start/stop it with
   `bash scripts/setup-chroma.sh --stop | --status` (Linux/macOS) or kill the
   process whose PID is in `~/.chroma-server.pid` (Windows).
5. **Embedding model** — pre-downloads the matching x86-64 or ARM64 quantized
   ONNX `paraphrase-multilingual-MiniLM-L12-v2` model (tokenizer + weights,
   ~120 MB) into `~/.rss-reader/models/`. Downloads use a pinned upstream
   revision and are SHA-256 verified before installation. The app applies the
   same verification on first use. Mirrors tried in order: `$HF_ENDPOINT` (if
   set), `https://huggingface.co`, `https://hf-mirror.com`.
6. **Optional full build** — `--build` / `-Build` runs `npm run tauri build`.

## Manual Setup

If you prefer to set up the environment manually:

### Prerequisites

**All platforms:**
- Node.js 18+ and npm
- Rust (install via https://rustup.rs/)
- Python 3.9+ (only needed for the ChromaDB server)

**Linux:**
```bash
# Ubuntu/Debian
sudo apt-get install libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev python3-venv

# Fedora
sudo dnf install webkit2gtk4.1-devel.x86_64 openssl-devel curl wget file libappindicator-gtk3-devel librsvg2-devel

# Arch Linux
sudo pacman -S --needed webkit2gtk-4.1 base-devel curl wget file openssl appmenu-gtk-module libappindicator-gtk3 librsvg
```

**macOS:**
- Xcode Command Line Tools (run `xcode-select --install`)

**Windows:**
- WebView2 runtime: https://developer.microsoft.com/en-us/microsoft-edge/webview2/
- Visual Studio C++ Build Tools with "Desktop development with C++" workload
- Python 3 on PATH: https://www.python.org/downloads/

### Installation

```bash
# Install dependencies
npm install

# Build the frontend
npm run build

# Validate the Rust backend compiles
(cd src-tauri && cargo check)

# Start ChromaDB (Linux/macOS)
./scripts/setup-chroma.sh

# The app downloads the pinned, checksummed embedding model on first use.
# To pre-download it with architecture selection and verification, run:
SKIP_CHROMA=1 SKIP_CARGO_CHECK=1 ./scripts/init.sh
```

## Running the Application

**Development mode:**
```bash
npm run tauri dev
```

**Production build:**
```bash
npm run tauri build
```

**Frontend only:**
```bash
npm run dev        # Development
npm run build      # Production
```

## Troubleshooting

### Linux: WebKitGTK not found
```bash
sudo apt-get install libwebkit2gtk-4.1-dev     # Ubuntu/Debian
sudo dnf install webkit2gtk4.1-devel           # Fedora
```

### macOS: Xcode Command Line Tools
```bash
xcode-select --install
```

### Windows: WebView2 missing
Download from: https://developer.microsoft.com/en-us/microsoft-edge/webview2/

### Rust not found
Install from: https://rustup.rs/

### cargo check fails
Check which crate failed — on Linux this is usually a missing system library
(WebKitGTK, openssl, librsvg), not a code error.

### ChromaDB won't start / Semantic Search says "not reachable"
- Check the log: `~/.chroma-server.log` (Linux/macOS), `~/.chroma-server.err.log` (Windows)
- Linux/macOS: `bash scripts/setup-chroma.sh --status` and retry setup
- Confirm the port in the app's Semantic DB settings matches `CHROMA_PORT` (8000)
- The app works fine without ChromaDB — only semantic search is affected

### Permission denied (Linux/macOS)
```bash
chmod +x scripts/init.sh scripts/setup-chroma.sh
```

## Platform-Specific Notes

### Linux
- Supports Ubuntu, Debian, Fedora, Arch, and derivatives
- Automatically detects package manager (apt, dnf, pacman)
- Installs `libwebkit2gtk-4.1-dev` (Tauri v2 requirement)

### macOS
- Requires macOS 10.15 (Catalina) or later
- Homebrew is recommended but not required

### Windows
- Requires Windows 10 or later
- PowerShell 5.1 or later
- May require running PowerShell as Administrator
- Visual Studio Build Tools 2019 or later recommended

## Support

For issues or questions:
1. Check the main README.md
2. Review Tauri documentation: https://tauri.app/
3. Check GitHub issues: https://github.com/StarDust0083-GP03/custom-rss-reader/issues