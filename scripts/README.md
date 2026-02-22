# Initialization Scripts

This directory contains cross-platform initialization scripts for setting up the Custom RSS Reader development environment.

## Scripts

### Linux / macOS
```bash
./scripts/init.sh
```

This script will:
- Check for Node.js and npm installation
- Install Rust via rustup if not present
- Install system dependencies (WebKitGTK, build tools, etc.)
- Install npm dependencies
- Build the project
- Install Tauri CLI globally

### Windows
```powershell
.\scripts\init.ps1
```

This script will:
- Check for Node.js and npm installation
- Install Rust via rustup if not present
- Check for WebView2 runtime
- Check for Visual Studio C++ Build Tools
- Install npm dependencies
- Build the project
- Install Tauri CLI globally

## Manual Setup

If you prefer to set up the environment manually:

### Prerequisites

**All platforms:**
- Node.js 18+ and npm
- Rust (install via https://rustup.rs/)

**Linux:**
```bash
# Ubuntu/Debian
sudo apt-get install libwebkit2gtk-4.0-dev build-essential curl wget file libxdo-dev libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev

# Fedora
sudo dnf install webkit2gtk3-devel.x86_64 openssl-devel curl wget file libappindicator-gtk3-devel librsvg2-devel

# Arch Linux
sudo pacman -S --needed webkit2gtk-4.0 base-devel curl wget file openssl appmenu-gtk-module libappindicator-gtk3 librsvg
```

**macOS:**
- Xcode Command Line Tools (run `xcode-select --install`)

**Windows:**
- WebView2 runtime: https://developer.microsoft.com/en-us/microsoft-edge/webview2/
- Visual Studio C++ Build Tools with "Desktop development with C++" workload

### Installation

```bash
# Install dependencies
npm install

# Build the project
npm run build

# (Optional) Install Tauri CLI globally
npm install -g @tauri-apps/cli
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

### Linux: WebViewGTK not found
```bash
sudo apt-get install libwebkit2gtk-4.0-dev
```

### macOS: Xcode Command Line Tools
```bash
xcode-select --install
```

### Windows: WebView2 missing
Download from: https://developer.microsoft.com/en-us/microsoft-edge/webview2/

### Rust not found
Install from: https://rustup.rs/

### Permission denied (Linux/macOS)
```bash
chmod +x scripts/init.sh
```

## Platform-Specific Notes

### Linux
- Supports Ubuntu, Debian, Fedora, Arch, and derivatives
- Automatically detects package manager (apt, dnf, pacman)

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
