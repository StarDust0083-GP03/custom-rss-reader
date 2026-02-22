# RSS Reader

A cross-platform RSS reader desktop application built with Tauri, featuring OPML import/export, RSSHub integration, and dual-source content fetching (RSS feeds vs website content).

## Features

- **OPML Import/Export**: Support for extended OPML attributes, compatible with Feedly, Inoreader, and other RSS readers
- **RSSHub Integration**: Customize RSSHub routes per subscription
- **Dual-Source Content**: Fetch content from RSS feeds or directly from websites
- **Content Source Flagging**: Visual indicators showing whether content came from RSS or website
- **SQLite Persistence**: Local database for storing subscriptions and feed items
- **Search**: Full-text search across all feed items
- **Cross-Platform**: Works on Linux, macOS, and Windows
- **Dark/Light Theme**: Automatic theme detection based on system preferences

## Prerequisites

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
npm run tauri dev
```

This will start the Vite development server and launch the Tauri application with hot-reloading enabled.

## Project Structure

```
rss-reader/
├── src-tauri/           # Rust backend
│   ├── src/
│   │   ├── database/    # SQLite database layer
│   │   ├── feed/        # Feed fetching and parsing
│   │   ├── opml/        # OPML import/export
│   │   └── commands/    # Tauri command handlers
│   └── Cargo.toml       # Rust dependencies
├── src/                 # Frontend (TypeScript)
│   ├── main.ts          # Application logic
│   └── styles.css       # Styling
├── index.html           # Main HTML
└── package.json         # Node.js dependencies
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

## Database Schema

The application uses SQLite with the following schema:

```sql
CREATE TABLE subscriptions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    url TEXT NOT NULL UNIQUE,
    title TEXT,
    website_url TEXT,
    rsshub_url TEXT,
    use_website BOOLEAN DEFAULT 0,
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
    content TEXT,
    description TEXT,
    author TEXT,
    published_at DATETIME,
    fetched_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    is_website_content BOOLEAN DEFAULT 0,
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

### Database errors
The database is stored in:
- Linux: `~/.local/share/com.hsf.rss-reader/`
- macOS: `~/Library/Application Support/com.hsf.rss-reader/`
- Windows: `%APPDATA%\com.hsf.rss-reader\`

Delete the `rss_reader.db` file to reset the database.
