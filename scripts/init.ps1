# Custom RSS Reader - Windows Initialization Script
# This script sets up the development environment for Windows

$ErrorActionPreference = "Stop"

Write-Host "================================" -ForegroundColor Cyan
Write-Host "Custom RSS Reader - Initialization" -ForegroundColor Cyan
Write-Host "================================" -ForegroundColor Cyan
Write-Host ""

# Check if running as Administrator
$isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host "⚠️  Note: Some operations may require administrator privileges." -ForegroundColor Yellow
    Write-Host ""
}

# Check if Node.js is installed
Write-Host "Checking Node.js installation..." -ForegroundColor White
try {
    $nodeVersion = node --version
    Write-Host "✅ Node.js $nodeVersion found" -ForegroundColor Green
} catch {
    Write-Host "❌ Node.js is not installed." -ForegroundColor Red
    Write-Host "Please install Node.js from https://nodejs.org/" -ForegroundColor Yellow
    exit 1
}

# Check if npm is installed
Write-Host "Checking npm installation..." -ForegroundColor White
try {
    $npmVersion = npm --version
    Write-Host "✅ npm $npmVersion found" -ForegroundColor Green
} catch {
    Write-Host "❌ npm is not installed." -ForegroundColor Red
    exit 1
}

# Check if Rust is installed (required for Tauri)
Write-Host ""
Write-Host "Checking Rust installation..." -ForegroundColor White
try {
    $rustVersion = rustc --version
    Write-Host "✅ Rust $rustVersion found" -ForegroundColor Green
} catch {
    Write-Host "⚠️  Rust is not installed." -ForegroundColor Yellow
    Write-Host "Installing Rust via rustup..." -ForegroundColor White

    # Download and run rustup-init
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

# Install npm dependencies
Write-Host ""
Write-Host "Installing npm dependencies..." -ForegroundColor White
npm install
if ($LASTEXITCODE -ne 0) {
    Write-Host "❌ Failed to install npm dependencies" -ForegroundColor Red
    exit 1
}
Write-Host "✅ Dependencies installed" -ForegroundColor Green

# Build the project
Write-Host ""
Write-Host "Building the project..." -ForegroundColor White
npm run build
if ($LASTEXITCODE -ne 0) {
    Write-Host "❌ Build failed" -ForegroundColor Red
    exit 1
}
Write-Host "✅ Build successful" -ForegroundColor Green

# Check if Tauri CLI is installed
Write-Host ""
Write-Host "Checking Tauri CLI..." -ForegroundColor White
try {
    $tauriVersion = npm list -g @tauri-apps/cli
    if ($LASTEXITCODE -eq 0) {
        Write-Host "✅ Tauri CLI found" -ForegroundColor Green
    } else {
        throw "Not installed"
    }
} catch {
    Write-Host "Installing Tauri CLI globally..." -ForegroundColor White
    npm install -g @tauri-apps/cli
    if ($LASTEXITCODE -eq 0) {
        Write-Host "✅ Tauri CLI installed" -ForegroundColor Green
    } else {
        Write-Host "⚠️  Failed to install Tauri CLI globally" -ForegroundColor Yellow
        Write-Host "You can still use 'npx tauri' commands" -ForegroundColor White
    }
}

Write-Host ""
Write-Host "================================" -ForegroundColor Cyan
Write-Host "✅ Initialization complete!" -ForegroundColor Green
Write-Host "================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "Available commands:" -ForegroundColor White
Write-Host "  npm run tauri dev     - Start development server" -ForegroundColor Cyan
Write-Host "  npm run tauri build   - Build for production" -ForegroundColor Cyan
Write-Host "  npm run build         - Build frontend only" -ForegroundColor Cyan
Write-Host "  npm run dev           - Start frontend dev server" -ForegroundColor Cyan
Write-Host ""
Write-Host "For more information, see README.md" -ForegroundColor White
Write-Host ""
