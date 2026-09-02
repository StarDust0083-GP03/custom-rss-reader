# RSS Reader · Current Project Overview

> **Source of truth:** this document describes the repository as of 2026-08-31. Paths below are checked against the current worktree. Update this file when a module moves; do not copy an old tree into a new section.

## 1. Product and runtime

RSS Reader is a Tauri 2 desktop application. The frontend is a Vite-built TypeScript/HTML/CSS WebView; the Rust process owns SQLite, network fetching, LLM calls, OPML parsing, and optional ChromaDB synchronization.

```text
┌──────────────────────────────────────────────────────────┐
│ Tauri main window / WebView                              │
│  main.ts → features/* → ui/* → api.ts → invoke()         │
└───────────────────────────────┬──────────────────────────┘
                                │ typed IPC boundary
┌───────────────────────────────▼──────────────────────────┐
│ Rust application state                                   │
│  commands → services → repositories → SQLite             │
│             │             │                              │
│             ├── AI service / LLM                         │
│             ├── FeedFetcher / websites                   │
│             └── ChromaHolder / sync state                │
└──────────────────────────────────────────────────────────┘
```

## 2. Technology and commands

| Area | Implementation |
|---|---|
| Desktop shell | Tauri 2.x |
| Frontend | TypeScript, Vite, vanilla DOM |
| Backend | Rust 2021, Tokio |
| Database | SQLite through SQLx |
| Feed parsing | feed-rs, quick-xml |
| HTML extraction | scraper, html2md |
| HTTP | reqwest |
| AI | OpenAI-compatible API through `LlmAiService` |
| Semantic search | ChromaDB server + client-side ONNX embeddings |

```bash
npm ci
npm run dev
npm run typecheck
npm test
npm run build
npm run verify
```

`npm run verify` is the merge gate: frontend build, frontend tests, Clippy with `-D warnings`, and Rust tests. Live Chroma/LLM E2E remains explicitly ignored unless its external services are configured.

## 3. Repository map

```text
rss-reader/
├── index.html                     # HTML shell and modals
├── src/
│   ├── main.ts                    # DOM wiring and application bootstrap
│   ├── api.ts                     # typed frontend → Tauri command adapter
│   ├── state.ts                   # shared frontend state
│   ├── types.ts                   # frontend data contracts
│   ├── features/
│   │   ├── actions.ts             # item/subscription actions and search
│   │   ├── ai.ts                  # translation, classification, AI settings
│   │   ├── chroma.ts               # semantic search and Chroma settings
│   │   └── tags.ts                 # tag catalog and local cluster manager
│   ├── ui/
│   │   ├── render.ts              # list/detail/sidebar rendering
│   │   ├── filters.ts             # filter state and tag picker
│   │   ├── layout.ts              # layout interactions
│   │   ├── menu.ts                # context and overflow menus
│   │   └── status.ts              # loading and activity status
│   ├── iframe.ts                  # sandboxed website/Markdown iframe loader
│   ├── markdown.ts                # shared Markdown renderer
│   ├── sanitize.ts                # DOM and URL sanitization
│   ├── styles.css                 # application styles
│   ├── toast.ts                   # notifications
│   └── assets/                    # Vite assets
├── src-tauri/
│   ├── src/lib.rs                 # Tauri setup and AppState wiring
│   ├── src/main.rs                # native binary entry point
│   ├── src/commands/              # IPC handlers; no SQL/business orchestration
│   │   ├── ai_commands.rs
│   │   ├── chroma_commands.rs
│   │   ├── feed_commands.rs       # feeds and OPML commands
│   │   ├── item_commands.rs
│   │   ├── streaming.rs           # translation events
│   │   ├── subscription_commands.rs
│   │   ├── tag_commands.rs        # catalog, local clusters, and mappings
│   │   └── webview.rs
│   ├── src/services/              # business orchestration
│   │   ├── feed_service.rs
│   │   ├── subscription_service.rs
│   │   └── tag_matcher.rs         # snaps generated tags onto the catalog (local ONNX)
│   ├── src/repositories/          # SQLite data access traits and impls
│   │   ├── feed_item_repo.rs      # item and tag catalog operations
│   │   └── subscription_repo.rs
│   ├── src/models/                # persisted domain models
│   │   └── tag.rs                 # snake_case normalization rules
│   ├── src/database/              # initialization and migrations
│   │   ├── mod.rs
│   │   └── migrations.rs
│   ├── src/feed/                  # feed HTTP and RSS/Atom parsing
│   │   ├── fetcher.rs
│   │   └── parser.rs
│   ├── src/ai/                    # AI contract, config, LLM implementation
│   │   ├── mod.rs
│   │   └── service.rs
│   ├── src/chroma/                # embeddings, service, backfill, sync
│   ├── src/content_processor/     # HTML/Markdown cleanup pipeline
│   └── src/tests/                 # Rust unit/integration-style tests
├── scripts/setup-chroma.sh        # pinned local Chroma helper
├── .github/workflows/verify.yml   # build, tests, Clippy, shell syntax
├── package.json                   # frontend scripts and dev tools
├── vite.config.ts                 # Vite/Tauri dev server configuration
└── src-tauri/tauri.conf.json      # window, CSP, bundle configuration
```

## 4. Core interfaces

### Frontend IPC

All new frontend backend calls should go through `src/api.ts`. The wrapper normalizes errors and makes payload types visible at the call site.

```text
DOM event
  → feature action
  → api.ts adapter
  → #[tauri::command]
  → service/repository
  → typed response
```

Classification uses a structured payload:

```json
{
  "itemId": 42,
  "tags": ["rust", "rss"],
  "category": null
}
```

The Rust command passes the global tag catalog to the LLM. `TagMatcher` then embeds any returned name that is not already a catalog name or alias, snaps it onto the closest catalog tag when the cosine similarity meets the user-configured threshold (`~/.rss-reader/tag_config.json`, default 0.85), and records the match as an alias. Finally the repository normalizes, resolves aliases, drops blocked names, and serializes the canonical `tags` array. Auto-classification uses the same matcher and repository path. Tag clustering reuses the matcher's embedding cache and is review-only; merges and deletes of existing tags are explicit user operations.

### Backend layering

- **Commands** extract Tauri parameters and return `Result<T>`.
- **Services** own domain rules and side-effect ordering.
- **Repositories** own SQL and return domain models/projections.
- **Adapters** own external systems: `FeedFetcher`, `LlmAiService`, and `ChromaService`.

`FeedService` receives a shared replaceable AI service slot. Saving AI settings replaces the slot, so automatic classification does not require an app restart.

## 5. Data and synchronization

SQLite lives at `~/.rss-reader/rss_reader.db`. Migrations add missing columns, remove duplicate GUID rows, create indexes, and create `tag_catalog`, `tag_aliases`, and `blocked_tags`. Legacy tag arrays are normalized into the catalog during migration. Migration errors are returned instead of allowing a partially known schema to start.

Chroma uses two durable mechanisms:

- `~/.rss-reader/chroma_sync.json` stores SQLite/collection identities, the watermark, reconciliation state, and pending upsert/delete work.
- A database or collection identity change resets the watermark. A completed rebuild removes orphaned `item_*` vectors while preserving unrelated collection entries.
- Delete tombstones are persisted before subscription rows are cascade-deleted. SQLite foreign keys are enabled on every connection; sync also checks whether a tombstoned item still exists before deleting its vector, making a crash before the SQLite delete safe.

The default Chroma helper binds to `127.0.0.1` and installs `chromadb==1.5.9`. Remote binding must be an explicit operator choice.

## 6. Security invariants

- AI configuration directory is `0700` and the API-key file is written atomically with mode `0600` on Unix.
- `get_ai_config` returns only a mask. Blank or masked input means “keep the existing key”; the mask is never accepted as a live key.
- Website fetching uses a separate client that rejects non-HTTP(S), localhost, private/link-local IP literals, and redirects to those destinations.
- The frontend iframe applies the same scheme and local/private-host guard before navigation.
- External browser opening parses the URL and requires HTTP(S) with a host.
- Markdown and HTML display paths pass through the shared sanitizer.
- Tauri CSP restricts scripts to the bundled origin and limits object/base sources.
- Local embedding downloads use an immutable model revision, architecture-specific weights, and SHA-256 verification before ONNX load.
- Chroma setup binds to loopback by default and refuses broad process kills or unsafe virtual-environment deletion paths.

These controls do not claim to solve DNS rebinding or a compromised local machine; those require a stronger network sandbox or OS-level policy.

## 7. Verification map

| Surface | Guard |
|---|---|
| AI key masking/preservation | `src-tauri/src/commands/ai_commands.rs` unit tests |
| Private config permissions | `src-tauri/src/commands/ai_commands.rs` Unix test |
| Malformed subscription URLs | `src-tauri/src/tests/subscription_tests.rs` |
| Search over Markdown | `src-tauri/src/tests/feed_item_tests.rs` |
| Subscription-scoped Favorites/Read Later | `src-tauri/src/tests/feed_item_tests.rs` |
| Legacy migration | `src-tauri/src/database/migrations.rs` test |
| Generated-tag matching and alias persistence | `src-tauri/src/tests/tag_matcher_tests.rs` |
| Website URL/redirect policy | `src-tauri/src/feed/fetcher.rs` tests |
| iframe URL and IPC payload contracts | `tests/frontend-contracts.test.ts` |
| Full merge gate | `npm run verify` and `.github/workflows/verify.yml` |

## 8. Safe change workflow

1. Trace the path from UI action to command and repository before editing.
2. Add or update a focused regression test for the boundary being changed.
3. Run `npm test` and the narrow Rust test first.
4. Run `npm run verify` before pushing.
5. Do not commit API keys, local databases, `target/`, `dist/`, or personal reports.
