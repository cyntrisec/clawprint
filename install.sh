#!/usr/bin/env bash
# Clawprint installer
# Usage: curl -fsSL https://raw.githubusercontent.com/cyntrisec/clawprint/master/install.sh | bash

set -euo pipefail

REPO="cyntrisec/clawprint"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# Detect OS and architecture
detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os="linux" ;;
        Darwin) os="macos" ;;
        *)      echo "Error: Unsupported OS: $os"; exit 1 ;;
    esac

    case "$arch" in
        x86_64|amd64)   arch="x86_64" ;;
        aarch64|arm64)  arch="aarch64" ;;
        *)              echo "Error: Unsupported architecture: $arch"; exit 1 ;;
    esac

    echo "${os}-${arch}"
}

main() {
    local platform
    platform="$(detect_platform)"
    echo "Detected platform: $platform"

    # Get latest release tag
    local latest_tag
    latest_tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"

    if [ -z "$latest_tag" ]; then
        echo "No release found. Building from source instead..."
        build_from_source
        return
    fi

    echo "Latest release: $latest_tag"

    local asset_name="clawprint-${platform}"
    local download_url="https://github.com/$REPO/releases/download/$latest_tag/$asset_name"

    echo "Downloading $download_url ..."
    if curl -fsSL -o /tmp/clawprint "$download_url"; then
        chmod +x /tmp/clawprint
        mkdir -p "$INSTALL_DIR"
        mv /tmp/clawprint "$INSTALL_DIR/clawprint"
        echo ""
        echo "Installed clawprint to $INSTALL_DIR/clawprint"
        check_path
    else
        echo "Binary download failed. Building from source instead..."
        build_from_source
    fi
}

build_from_source() {
    if ! command -v cargo &>/dev/null; then
        echo "Rust not found. Install from https://rustup.rs first:"
        echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        exit 1
    fi

    echo "Building from source..."
    local tmpdir
    tmpdir="$(mktemp -d)"
    git clone --depth 1 "https://github.com/$REPO.git" "$tmpdir/clawprint"
    cd "$tmpdir/clawprint"
    cargo build --release
    mkdir -p "$INSTALL_DIR"
    cp target/release/clawprint "$INSTALL_DIR/clawprint"
    rm -rf "$tmpdir"
    echo ""
    echo "Installed clawprint to $INSTALL_DIR/clawprint"
    check_path
}

check_path() {
    if ! echo "$PATH" | tr ':' '\n' | grep -q "^$INSTALL_DIR$"; then
        echo ""
        echo "Add $INSTALL_DIR to your PATH:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        echo ""
        echo "Add this to your ~/.bashrc or ~/.zshrc to make it permanent."
    fi
    echo ""
    echo "Verify: clawprint --version"
}

main
