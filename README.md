# RSS Reader

A cross-platform RSS reader desktop application built with Tauri, featuring OPML import/export, RSSHub integration, AI-powered bilingual translation and article classification, and optional ChromaDB-backed semantic search.

## Features

- **OPML Import/Export**: Support for extended OPML attributes, compatible with Feedly, Inoreader, and other RSS readers
- **RSSHub Integration**: Customize RSSHub routes per subscription
- **Dual-Source Content**: Fetch content from RSS feeds or directly from websites
- **AI Translation**: Streaming bilingual (original + Chinese) article translation
- **AI Classification**: Automatic article tagging by title, batched (20 articles per LLM call) to stay under API rate limits
- **AI Task Status**: The bottom status bar shows whether the model is translating, classifying, recommending, testing, or waiting in the queue
- **AI Picks** *(manual)*: "★ Picks" button — the LLM plays editor, reading your recent unread titles/snippets and picking the 5 most worthwhile articles with a one-line reason each
- **Semantic Search** *(optional)*: ChromaDB-backed "search by meaning" and similar-article discovery
- **SQLite Persistence**: Local database for storing subscriptions and feed items
- **Search**: Full-text search across all feed items
- **Cross-Platform**: Works on Linux, macOS, and Windows
- **Dark/Light Theme**: Automatic theme detection based on system preferences

## Prerequisites

- **Node.js ≥ 18** — <https://nodejs.org> (or via nvm/fnm)
- **Rust ≥ 1.77** — stable toolchain via rustup

### Linux (Ubuntu/Debian)

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install system dependencies
sudo apt update
sudo apt install libwebkit2gtk-4.1-dev \
    build-essential \
    curl \
    wget \
    file \
    libxdo-dev \
    libssl-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev
```

### macOS

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install Xcode Command Line Tools
xcode-select --install
```

### Windows

```bash
# Install Rust from https://rustup.rs/
# Install Microsoft C++ Build Tools from https://visualstudio.microsoft.com/visual-cpp-build-tools/
# Install WebView2 Runtime from https://developer.microsoft.com/en-us/microsoft-edge/webview2/
```

## Installation

1. Clone the repository:
```bash
git clone <repository-url>
cd rss-reader
```

2. Install Node.js dependencies:
```bash
npm install
```

3. Build the application:
```bash
npm run tauri build
```

The built application will be in `src-tauri/target/release/bundle/`.

## Development

Run the development server:

```bash
npm run tauri:dev     # Tauri dev loop with devtools feature
# or
npm run dev           # frontend only (Vite, in the browser — backend calls unavailable)
```

Useful scripts:

```bash
npm run typecheck     # tsc --noEmit over the strict TS config
npm test              # frontend contract/security tests
npm run verify        # frontend build + tests + Clippy + Rust tests
cargo test --lib      # Rust unit tests (in src-tauri/)
```

## Project Structure

```
rss-reader/
├── src-tauri/               # Rust backend
│   └── src/
│       ├── ai/              # LLM service (translation, classification, rate limiting)
│       ├── chroma/          # ChromaDB service + incremental sync engine
│       ├── commands/        # Tauri command handlers
│       ├── content_processor/ # HTML cleaning / HTML→Markdown pipeline
│       ├── database/        # SQLite init + migrations
│       ├── feed/            # Feed fetching and parsing
│       ├── models/          # Domain models
│       ├── repositories/    # Data access layer (trait + SQLite impl)
│       ├── services/        # Business logic (feed/subscription services)
│       └── tests/           # Rust unit tests (in-memory SQLite)
├── src/                     # Frontend (TypeScript, vanilla DOM)
│   ├── api.ts               # Typed invoke wrappers for every backend command
│   ├── features/            # Actions, AI features, Chroma features
│   ├── ui/                  # Rendering, filters, status bar
│   ├── iframe.ts            # Sandboxed article iframe
│   ├── sanitize.ts          # HTML sanitizer (allowlist)
│   ├── state.ts             # Central app state store
│   └── assets/fonts/        # Self-hosted fonts (offline, CSP-safe)
├── index.html               # Main HTML
└── package.json             # Node.js dependencies
```

## Usage

### Adding Subscriptions

1. Click the "+" button in the sidebar
2. Enter the feed URL (required)
3. Optionally provide:
   - Title
   - Website URL
   - Custom RSSHub route
   - Enable "Fetch from website" to get content directly from the website instead of RSS

### Importing OPML

1. Click the "Import" button in the sidebar
2. Select an OPML file from your computer
3. Subscriptions will be imported automatically

### Exporting OPML

1. Click the "Export" button in the sidebar
2. Choose a location to save the OPML file
3. All subscriptions will be exported with their settings

### Refreshing Feeds

- Click the "↻" button to refresh all subscriptions
- Items are automatically deduplicated using GUIDs

## AI Features (optional)

Configure an OpenAI-compatible API (default: DeepSeek) via the **AI** button in the detail panel.

- **Translate** — streaming bilingual (original + 中文) translation of the selected article, cached for 3 days
- **Tags** — classify the selected article into tags/category
- **Auto-classify** — new items are tagged automatically when the subscription has AI enabled; requests are batched (20 titles per LLM call) and globally rate-limited (serialized, ≥1.2s spacing) to avoid API 429s

Configuration is stored in `~/.rss-reader/ai_config.json`. On Unix, the directory is protected as `0700` and the API-key file is written atomically as `0600`; the file still contains a secret and should not be copied or committed.

## Semantic Search with ChromaDB (optional)

Semantic search ("search by meaning" and "Similar articles") requires a running ChromaDB server.

### 1. Run ChromaDB

Docker (recommended):

```bash
docker run -d --name chromadb -p 127.0.0.1:8000:8000 chromadb/chroma:1.5.9
```

Or via pip (the helper script pins the same version):

```bash
pip install chromadb==1.5.9
chroma run --host 127.0.0.1 --port 8000
```

Repository helper (also works against an already-running local server):

```bash
./scripts/setup-chroma.sh
```

The helper pins ChromaDB `1.5.9` and idempotently creates the `rss_articles`
collection through the v2 API. Set `CHROMA_COLLECTION` when using a different
collection name.

### 2. Enable in the app

Open **Semantic DB** in the items-panel header, set host/port/collection (defaults `http://localhost:8000`, collection `rss_articles`), check **Enable ChromaDB**, and click **Enable & Index**. The app verifies the server, ensures the collection exists, saves the configuration, and performs a full initial index — no restart is required. If ChromaDB is unavailable, the configuration remains disabled. The configured collection must match the one initialized by the helper.

### 3. How indexing stays in sync

The app maintains the index automatically — no manual maintenance:

- **At fetch time** — every new item is indexed immediately (subscription refresh).
- **Incremental sync** — on every app start and after each bulk refresh, a watermark-based sync (`~/.rss-reader/chroma_sync.json`) indexes anything newer than the last synced id. If ChromaDB was down during a fetch, those items are queued and picked up on the next sync — nothing is lost silently.
- **Deletions** — removing a subscription writes durable tombstones before the SQLite cascade; vectors are deleted immediately when possible and retried by the next sync when Chroma is unavailable.
- **Memory-safe** — sync pages through a lightweight projection (keyset pagination, text columns truncated to 2000 chars), so index rebuilds stay bounded regardless of library size.
- **Enable & Index** — the first enable validates the server, ensures the collection, and performs a full rebuild via the same mechanism (idempotent upserts); live progress is shown in the status bar.
- **Re-Index All Items** — the button in the ChromaDB settings dialog repeats that full rebuild when needed; configuration changes apply without restarting.

## Database Schema

The application uses SQLite (see `src-tauri/src/database/migrations.rs` for the live DDL). Core tables:

```sql
CREATE TABLE subscriptions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    url TEXT NOT NULL UNIQUE,
    title TEXT,
    website_url TEXT,
    rsshub_url TEXT,
    use_website BOOLEAN DEFAULT 0,
    auto_classify BOOLEAN DEFAULT 1,
    opml_attributes TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE feed_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    subscription_id INTEGER NOT NULL,
    guid TEXT,
    title TEXT NOT NULL,
    link TEXT,
    content TEXT,               -- raw RSS HTML / website HTML
    content_md TEXT,            -- cached Markdown (lazy or website-fetched)
    description TEXT,
    author TEXT,
    published_at DATETIME,
    fetched_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    is_website_content BOOLEAN DEFAULT 0,
    is_read BOOLEAN DEFAULT 0,
    is_favorite BOOLEAN DEFAULT 0,
    is_read_later BOOLEAN DEFAULT 0,
    is_ignored BOOLEAN DEFAULT 0,
    tags TEXT,                  -- JSON array, e.g. ["rust","programming"]
    category TEXT,
    translated_title TEXT,
    translated_content TEXT,    -- cached bilingual HTML
    translated_at DATETIME,
    FOREIGN KEY (subscription_id) REFERENCES subscriptions(id) ON DELETE CASCADE
);
```

## Configuration

### RSSHub

To use a custom RSSHub instance:
1. When adding a subscription, enter the RSSHub route in the "RSSHub URL" field
2. The route will be appended to the default RSSHub instance (https://rsshub.app)

### Content Fetching

- **RSS Mode (default)**: Fetches content from the RSS feed
- **Website Mode**: Fetches the full article content from the website URL
  - Uses content extraction heuristics to find the main article content
  - Useful for truncated RSS feeds

## Building for Distribution

### Current Platform
```bash
npm run tauri build
```

### Specific Platforms

```bash
# Linux
cargo tauri build --target x86_64-unknown-linux-gnu

# macOS (Intel)
cargo tauri build --target x86_64-apple-darwin

# macOS (Apple Silicon)
cargo tauri build --target aarch64-apple-darwin

# Windows
cargo tauri build --target x86_64-pc-windows-msvc
```

## Technology Stack

- **Backend**: Rust (Tauri 2.x)
- **Frontend**: TypeScript + Vanilla JavaScript
- **Database**: SQLite via SQLx
- **Feed Parsing**: feed-rs
- **OPML Parsing**: quick-xml
- **HTTP Client**: reqwest
- **HTML Parsing**: scraper

## License

MIT License

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Troubleshooting

### "Rust not found" error
Install Rust using rustup: https://rustup.rs/

### "webkit2gtk not found" error (Linux)
Install the required system dependencies (see Prerequisites section)

### Where is my data?

All app data lives under `~/.rss-reader/` on every platform:

| File | Purpose |
|---|---|
| `~/.rss-reader/rss_reader.db` | SQLite database (subscriptions + items) |
| `~/.rss-reader/ai_config.json` | AI/LLM configuration (contains API key) |
| `~/.rss-reader/chroma_config.json` | ChromaDB connection settings |
| `~/.rss-reader/chroma_sync.json` | Semantic-index sync state (watermark + retry queues) |

Delete `rss_reader.db` to reset the database. Delete `chroma_sync.json` only if you also drop the ChromaDB collection (otherwise re-run "Re-Index All Items" to rebuild).

### Semantic search returns nothing / "Is ChromaDB running?"
Run the **Health Check** button in the ChromaDB settings dialog. If it fails, the server isn't reachable at the configured host/port. If it succeeds but search is empty, click **Re-Index All Items** once — after that, the incremental sync keeps the index current automatically.

### API rate limits (HTTP 429) during classification/translation
The app serializes LLM calls and enforces a ≥1.2s spacing globally. If you still hit limits, raise the interval (`LLM_MIN_INTERVAL_MS` in `src-tauri/src/ai/service.rs`) or reduce `CLASSIFY_BATCH_SIZE` (in `src-tauri/src/ai/mod.rs`).
