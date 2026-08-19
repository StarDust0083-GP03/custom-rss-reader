# Custom RSS Reader - Windows Initialization Script
# Sets up the complete development environment:
#   1. Toolchain checks: Node.js, npm, Rust, WebView2, VS C++ Build Tools
#   2. Python 3 (required by the ChromaDB server)
#   3. npm install + frontend build
#   4. `cargo check` on src-tauri — validates that `npm run tauri dev/build` compiles
#   5. ChromaDB server (venv + pip install + start)
#   6. Embedding model pre-download into ~\.rss-reader\models
#
# Usage:
#   .\scripts\init.ps1             # full setup (skips the long production build)
#   .\scripts\init.ps1 -Build      # also run `npm run tauri build`
#
# Env overrides:
#   CHROMA_PORT        ChromaDB port           (default 8000)
#   CHROMA_VENV        ChromaDB venv location  (default ~\chroma-venv)
#   CHROMA_DATA        ChromaDB data dir       (default ~\chroma-data)
#   CHROMA_MODEL_DIR   embedding model dir     (default ~\.rss-reader\models)
#   HF_ENDPOINT        HuggingFace mirror base (e.g. https://hf-mirror.com)
#   SKIP_CARGO_CHECK   set to 1 to skip `cargo check`
#   SKIP_CHROMA        set to 1 to skip ChromaDB server setup
#   SKIP_MODEL         set to 1 to skip the embedding model download

param([switch]$Build)

$ErrorActionPreference = "Stop"

function Write-Step($msg) { Write-Host ""; Write-Host "─── $msg ───" -ForegroundColor Cyan }

Write-Host "================================" -ForegroundColor Cyan
Write-Host "Custom RSS Reader - Initialization" -ForegroundColor Cyan
Write-Host "================================" -ForegroundColor Cyan

# Set the working directory to the repo root (parent of scripts\)
Set-Location (Split-Path -Parent $PSScriptRoot)

# Check if running as Administrator
$isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host "⚠️  Note: Some operations may require administrator privileges." -ForegroundColor Yellow
    Write-Host ""
}

# ---------------------------------------------------------------------------
Write-Step "1/6  Toolchain checks"
# ---------------------------------------------------------------------------

# Check if Node.js is installed
try {
    $nodeVersion = node --version
    Write-Host "✅ Node.js $nodeVersion found" -ForegroundColor Green
} catch {
    Write-Host "❌ Node.js is not installed." -ForegroundColor Red
    Write-Host "Please install Node.js from https://nodejs.org/" -ForegroundColor Yellow
    exit 1
}

# Check if npm is installed
try {
    $npmVersion = npm --version
    Write-Host "✅ npm $npmVersion found" -ForegroundColor Green
} catch {
    Write-Host "❌ npm is not installed." -ForegroundColor Red
    exit 1
}

# Check if Rust is installed
Write-Host ""
Write-Host "Checking Rust installation..." -ForegroundColor White
try {
    $rustVersion = rustc --version
    Write-Host "✅ Rust $rustVersion found" -ForegroundColor Green
} catch {
    Write-Host "⚠️  Rust is not installed." -ForegroundColor Yellow
    Write-Host "Installing Rust via rustup..." -ForegroundColor White

    $rustupUrl = "https://win.rustup.rs/x86_64"
    $rustupPath = "$env:TEMP\rustup-init.exe"

    try {
        Invoke-WebRequest -Uri $rustupUrl -OutFile $rustupPath -UseBasicParsing
        Start-Process -FilePath $rustupPath -ArgumentList "-y" -Wait

        # Refresh environment variables
        $env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")

        Write-Host "✅ Rust installed successfully" -ForegroundColor Green
    } catch {
        Write-Host "❌ Failed to install Rust automatically." -ForegroundColor Red
        Write-Host "Please install Rust manually from https://rustup.rs/" -ForegroundColor Yellow
        exit 1
    }
}

# Check for WebView2 (required for Tauri on Windows)
Write-Host ""
Write-Host "Checking WebView2 runtime..." -ForegroundColor White
$webView2RegPath = "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
if (Test-Path $webView2RegPath) {
    Write-Host "✅ WebView2 runtime found" -ForegroundColor Green
} else {
    Write-Host "⚠️  WebView2 runtime not found." -ForegroundColor Yellow
    Write-Host "Tauri requires WebView2 runtime. Download from:" -ForegroundColor White
    Write-Host "https://developer.microsoft.com/en-us/microsoft-edge/webview2/" -ForegroundColor Cyan
    Write-Host ""
    $installWebView2 = Read-Host "Do you want to continue anyway? (y/n)"
    if ($installWebView2 -ne "y" -and $installWebView2 -ne "Y") {
        exit 1
    }
}

# Check for Visual Studio C++ Build Tools
Write-Host ""
Write-Host "Checking Visual Studio C++ Build Tools..." -ForegroundColor White
try {
    $vsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vsWhere) {
        $vsInfo = & $vsWhere -latest -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -format json | ConvertFrom-Json
        if ($vsInfo) {
            Write-Host "✅ Visual Studio Build Tools found" -ForegroundColor Green
        } else {
            throw "No build tools found"
        }
    } else {
        throw "vswhere not found"
    }
} catch {
    Write-Host "⚠️  Visual Studio C++ Build Tools not found." -ForegroundColor Yellow
    Write-Host "These are required for building Rust applications." -ForegroundColor White
    Write-Host "Install from: https://visualstudio.microsoft.com/downloads/" -ForegroundColor Cyan
    Write-Host "Select 'Desktop development with C++' workload." -ForegroundColor White
    Write-Host ""
    $continueWithoutTools = Read-Host "Do you want to continue anyway? (y/n)"
    if ($continueWithoutTools -ne "y" -and $continueWithoutTools -ne "Y") {
        exit 1
    }
}

# Python 3 (required by the ChromaDB server)
Write-Host ""
Write-Host "Checking Python 3..." -ForegroundColor White
try {
    $pythonVersion = python --version
    Write-Host "✅ $pythonVersion found" -ForegroundColor Green
} catch {
    Write-Host "⚠️  Python 3 not found." -ForegroundColor Yellow
    Write-Host "ChromaDB (Semantic Search) needs Python 3. Install it from https://www.python.org/downloads/ (check 'Add python.exe to PATH')." -ForegroundColor White
    Write-Host "The rest of the setup will still proceed." -ForegroundColor Yellow
}

# ---------------------------------------------------------------------------
Write-Step "2/6  npm install + frontend build"
# ---------------------------------------------------------------------------

Write-Host ""
Write-Host "Installing npm dependencies..." -ForegroundColor White
npm install
if ($LASTEXITCODE -ne 0) {
    Write-Host "❌ Failed to install npm dependencies" -ForegroundColor Red
    exit 1
}
Write-Host "✅ Dependencies installed" -ForegroundColor Green

# Tauri CLI ships as a devDependency (@tauri-apps/cli) — no global install needed.
$localTauriCli = Join-Path (Get-Location) "node_modules\.bin\tauri.cmd"
if (Test-Path $localTauriCli) {
    Write-Host "✅ Tauri CLI found (local devDependency)" -ForegroundColor Green
} else {
    Write-Host "❌ Tauri CLI missing after 'npm install' — check the npm install output." -ForegroundColor Red
    exit 1
}

# Build the frontend
Write-Host ""
Write-Host "Building the project..." -ForegroundColor White
npm run build
if ($LASTEXITCODE -ne 0) {
    Write-Host "❌ Build failed" -ForegroundColor Red
    exit 1
}
Write-Host "✅ Build successful" -ForegroundColor Green

# ---------------------------------------------------------------------------
Write-Step "3/6  Rust backend check"
# ---------------------------------------------------------------------------

# cargo check compiles src-tauri far faster than a full build and proves that
# `npm run tauri dev` / `npm run tauri build` will initialize correctly.
if ($env:SKIP_CARGO_CHECK -ne "1") {
    Write-Host "First run compiles all Rust dependencies — this takes a few minutes." -ForegroundColor White
    Push-Location "src-tauri"
    try {
        cargo check
        if ($LASTEXITCODE -ne 0) { throw "cargo check failed" }
        Write-Host "✅ Rust backend compiles OK — 'npm run tauri dev/build' will work." -ForegroundColor Green
    } catch {
        Write-Host "❌ cargo check failed — fix the Rust errors before 'npm run tauri dev/build'." -ForegroundColor Red
        exit 1
    } finally {
        Pop-Location
    }
} else {
    Write-Host "⚠️  Skipping cargo check (SKIP_CARGO_CHECK=1)." -ForegroundColor Yellow
}

# Optional full production build
if ($Build) {
    Write-Host ""
    Write-Host "Running full production build (npm run tauri build)..." -ForegroundColor White
    npm run tauri build
    if ($LASTEXITCODE -ne 0) {
        Write-Host "❌ tauri build failed" -ForegroundColor Red
        exit 1
    }
}

# ---------------------------------------------------------------------------
Write-Step "4/6  ChromaDB server"
# ---------------------------------------------------------------------------

if ($env:SKIP_CHROMA -eq "1") {
    Write-Host "⚠️  Skipping ChromaDB setup (SKIP_CHROMA=1)." -ForegroundColor Yellow
} else {
    $venvDir  = if ($env:CHROMA_VENV)      { $env:CHROMA_VENV }      else { "$HOME\chroma-venv" }
    $dataDir  = if ($env:CHROMA_DATA)      { $env:CHROMA_DATA }      else { "$HOME\chroma-data" }
    $port     = if ($env:CHROMA_PORT)      { $env:CHROMA_PORT }      else { 8000 }
    $python   = "$venvDir\Scripts\python.exe"
    $chromaExe = "$venvDir\Scripts\chroma.exe"
    $heartbeat = "http://localhost:${port}/api/v2/heartbeat"

    # Already running?
    try {
        Invoke-RestMethod -Uri $heartbeat -TimeoutSec 3 | Out-Null
        Write-Host "✅ ChromaDB already running on port $port" -ForegroundColor Green
    } catch {
        Write-Host "Creating ChromaDB venv at $venvDir ..." -ForegroundColor White
        if (-not (Test-Path $python)) {
            New-Item -ItemType Directory -Force -Path (Split-Path $venvDir) | Out-Null
            python -m venv "$venvDir"
            if ($LASTEXITCODE -ne 0) { throw "Failed to create venv — is Python 3 on PATH?" }
        }

        & $python -c "import chromadb" 2>$null
        if ($LASTEXITCODE -ne 0) {
            Write-Host "Installing chromadb..." -ForegroundColor White
            & $python -m pip install --upgrade --quiet pip
            & $python -m pip install --quiet chromadb
            if ($LASTEXITCODE -ne 0) { throw "pip install chromadb failed" }
        }

        if (Test-Path $chromaExe) {
            Write-Host "Starting ChromaDB on port $port (data: $dataDir) ..." -ForegroundColor White
            New-Item -ItemType Directory -Force -Path $dataDir | Out-Null
            $logOut = "$HOME\.chroma-server.log"
            $logErr = "$HOME\.chroma-server.err.log"
            $proc = Start-Process -FilePath $chromaExe `
                -ArgumentList @("run", "--host", "0.0.0.0", "--port", "$port", "--path", "$dataDir") `
                -WindowStyle Hidden `
                -RedirectStandardOutput $logOut `
                -RedirectStandardError $logErr `
                -PassThru
            $proc.Id | Set-Content "$HOME\.chroma-server.pid"

            $ready = $false
            for ($i = 0; $i -lt 30; $i++) {
                Start-Sleep -Seconds 1
                try { Invoke-RestMethod -Uri $heartbeat -TimeoutSec 2 | Out-Null; $ready = $true; break } catch {}
            }
            if ($ready) {
                Write-Host "✅ ChromaDB is up on port $port" -ForegroundColor Green
            } else {
                Write-Host "⚠️  ChromaDB failed to start. Check the log:" -ForegroundColor Yellow
                if (Test-Path $logErr) { Get-Content $logErr -Tail 20 }
            }
        } else {
            Write-Host "⚠️  chroma.exe not found after install — rerun this script." -ForegroundColor Yellow
        }
    }
}

# ---------------------------------------------------------------------------
Write-Step "5/6  Embedding model pre-download"
# ---------------------------------------------------------------------------

# The app embeds documents client-side (ChromaDB 1.x has no server-side
# embedding function) with a quantized ONNX model. Pre-fetching it here means
# the first semantic search/index doesn't stall on a ~120 MB download.
# Mirrors and paths mirror src-tauri/src/chroma/embeddings.rs.
$modelRepo  = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"
$modelFiles = @("tokenizer.json", "onnx/model_quint8_avx2.onnx")
$modelBase  = if ($env:CHROMA_MODEL_DIR) { $env:CHROMA_MODEL_DIR } else { "$HOME\.rss-reader\models" }
$modelDir   = Join-Path $modelBase $modelRepo

$mirrors = @()
if ($env:HF_ENDPOINT) { $mirrors += $env:HF_ENDPOINT }
$mirrors += @("https://huggingface.co", "https://hf-mirror.com")

if ($env:SKIP_MODEL -eq "1") {
    Write-Host "⚠️  Skipping model download (SKIP_MODEL=1)." -ForegroundColor Yellow
} else {
    Write-Host "Embedding model: $modelRepo" -ForegroundColor White
    Write-Host "Target dir:      $modelDir" -ForegroundColor White
    $allOk = $true
    foreach ($file in $modelFiles) {
        $dest = Join-Path $modelDir $file
        if (Test-Path $dest) {
            Write-Host "✅ $file already present" -ForegroundColor Green
            continue
        }
        New-Item -ItemType Directory -Force -Path (Split-Path $dest) | Out-Null
        $found = $false
        foreach ($mirror in $mirrors) {
            $url = "$mirror/$modelRepo/resolve/main/$file"
            Write-Host "  downloading $file from $mirror ..." -ForegroundColor White
            try {
                Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing
                $size = (Get-Item $dest).Length / 1MB
                Write-Host "✅ $file ($([math]::Round($size,1)) MB)" -ForegroundColor Green
                $found = $true
                break
            } catch {
                Write-Host "⚠️  failed from $mirror" -ForegroundColor Yellow
                Remove-Item $dest -ErrorAction SilentlyContinue
            }
        }
        if (-not $found) {
            Write-Host "❌ could not download $file from any mirror" -ForegroundColor Red
            $allOk = $false
        }
    }
    if ($allOk) {
        Write-Host "✅ Embedding model ready — first semantic search will load it instantly." -ForegroundColor Green
    } else {
        Write-Host "⚠️  Model download failed — the app retries automatically on first semantic use." -ForegroundColor Yellow
    }
}

# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "================================" -ForegroundColor Cyan
Write-Host "✅ Initialization complete!" -ForegroundColor Green
Write-Host "================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "Available commands:" -ForegroundColor White
Write-Host "  npm run tauri dev     - Start development server (hot reload)" -ForegroundColor Cyan
Write-Host "  npm run tauri build   - Build production bundles" -ForegroundColor Cyan
Write-Host "  npm run build         - Build frontend only" -ForegroundColor Cyan
Write-Host "  npm run dev           - Start frontend dev server" -ForegroundColor Cyan
Write-Host ""
Write-Host "Semantic search:" -ForegroundColor White
$finalPort = if ($env:CHROMA_PORT) { $env:CHROMA_PORT } else { 8000 }
Write-Host "  ChromaDB server:      http://localhost:$finalPort" -ForegroundColor Cyan
Write-Host "  Model location:       $modelBase" -ForegroundColor Cyan
Write-Host ""
Write-Host "For more information, see README.md" -ForegroundColor White
Write-Host ""