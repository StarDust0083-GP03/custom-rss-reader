#!/bin/bash

# Custom RSS Reader - Linux/macOS Initialization Script
# This script sets up the development environment for Linux and macOS

set -e  # Exit on error

echo "================================"
echo "Custom RSS Reader - Initialization"
echo "================================"
echo ""

# Detect OS
OS="$(uname -s)"
case "${OS}" in
    Linux*)     MACHINE=Linux;;
    Darwin*)    MACHINE=Mac;;
    *)          MACHINE="UNKNOWN:${OS}"
esac

echo "Detected OS: ${MACHINE}"
echo ""

# Check if Node.js is installed
echo "Checking Node.js installation..."
if ! command -v node &> /dev/null; then
    echo "❌ Node.js is not installed."
    echo "Please install Node.js from https://nodejs.org/"
    exit 1
fi
echo "✅ Node.js $(node -v) found"

# Check if npm is installed
echo "Checking npm installation..."
if ! command -v npm &> /dev/null; then
    echo "❌ npm is not installed."
    exit 1
fi
echo "✅ npm $(npm -v) found"

# Check if Rust is installed (required for Tauri)
echo ""
echo "Checking Rust installation..."
if ! command -v rustc &> /dev/null; then
    echo "⚠️  Rust is not installed."
    echo "Installing Rust via rustup..."
    if command -v curl &> /dev/null; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        source $HOME/.cargo/env
    else
        echo "❌ curl is not installed. Please install Rust manually from https://rustup.rs/"
        exit 1
    fi
else
    echo "✅ Rust $(rustc --version) found"
fi

# Check system dependencies based on OS
echo ""
echo "Checking system dependencies..."

if [[ "${MACHINE}" == "Linux" ]]; then
    # Linux dependencies
    echo "Checking Linux dependencies..."

    # Check for libwebkit2gtk-4.0-dev (required for Tauri on Linux)
    if ! dpkg -l | grep -q libwebkit2gtk-4.0-dev; then
        echo "⚠️  WebKitGTK development libraries not found."
        echo "Installing Tauri dependencies..."

        if command -v apt-get &> /dev/null; then
            # Ubuntu/Debian
            sudo apt-get update
            sudo apt-get install -y libwebkit2gtk-4.0-dev \
                build-essential \
                curl \
                wget \
                file \
                libxdo-dev \
                libssl-dev \
                libgtk-3-dev \
                libayatana-appindicator3-dev \
                librsvg2-dev
        elif command -v dnf &> /dev/null; then
            # Fedora
            sudo dnf install -y webkit2gtk3-devel.x86_64 \
                openssl-devel \
                curl \
                wget \
                file \
                libappindicator-gtk3-devel \
                librsvg2-devel
        elif command -v pacman &> /dev/null; then
            # Arch Linux
            sudo pacman -S --needed webkit2gtk-4.0 \
                base-devel \
                curl \
                wget \
                file \
                openssl \
                appmenu-gtk-module \
                libappindicator-gtk3 \
                librsvg
        else
            echo "⚠️  Unable to automatically install dependencies."
            echo "Please install the required packages manually."
        fi
    else
        echo "✅ WebKitGTK found"
    fi

elif [[ "${MACHINE}" == "Mac" ]]; then
    # macOS dependencies
    echo "Checking macOS dependencies..."

    # Check for Homebrew
    if ! command -v brew &> /dev/null; then
        echo "⚠️  Homebrew is not installed."
        echo "Installing Homebrew..."
        /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
    fi

    # Check for Xcode Command Line Tools
    if ! command -v xcode-select &> /dev/null; then
        echo "⚠️  Xcode Command Line Tools not found."
        echo "Installing Xcode Command Line Tools..."
        xcode-select --install
    fi

    echo "✅ macOS dependencies OK"
fi

# Install npm dependencies
echo ""
echo "Installing npm dependencies..."
npm install

# Build the project
echo ""
echo "Building the project..."
npm run build

# Check if Tauri CLI is installed
echo ""
echo "Checking Tauri CLI..."
if ! npm list -g @tauri-apps/cli &> /dev/null; then
    echo "Installing Tauri CLI globally..."
    npm install -g @tauri-apps/cli
else
    echo "✅ Tauri CLI found"
fi

echo ""
echo "================================"
echo "✅ Initialization complete!"
echo "================================"
echo ""
echo "Available commands:"
echo "  npm run tauri dev     - Start development server"
echo "  npm run tauri build   - Build for production"
echo "  npm run build         - Build frontend only"
echo "  npm run dev           - Start frontend dev server"
echo ""
echo "For more information, see README.md"
