#!/usr/bin/env bash
set -euo pipefail

# Kepler remote install script
# Usage:
#   curl -sSfL https://raw.githubusercontent.com/maleus-ai/kepler/master/get-kepler.sh | bash
#   curl -sSfL https://raw.githubusercontent.com/maleus-ai/kepler/master/get-kepler.sh | bash -s v0.1.0
#   curl -sSfL https://raw.githubusercontent.com/maleus-ai/kepler/master/get-kepler.sh | bash -s -- --no-systemd
#   curl -sSfL https://raw.githubusercontent.com/maleus-ai/kepler/master/get-kepler.sh | bash -s v0.1.0 --no-systemd

REPO="maleus-ai/kepler"

# Colors (disabled when not a terminal)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BOLD='\033[1m'
    NC='\033[0m'
else
    RED='' GREEN='' YELLOW='' BOLD='' NC=''
fi

info()  { echo -e "${GREEN}==>${NC} ${BOLD}$*${NC}"; }
warn()  { echo -e "${YELLOW}==> WARNING:${NC} $*"; }
error() { echo -e "${RED}==> ERROR:${NC} $*" >&2; }

usage() {
    cat <<EOF
Usage: get-kepler.sh [VERSION] [OPTIONS...]

Download and install Kepler from GitHub Releases.

Arguments:
  VERSION             Version tag to install (e.g. v0.1.0). Defaults to latest release.

Options:
  --gnu               Use glibc (gnu) build instead of musl (Linux only)
  --no-systemd        Skip systemd service installation (passed to install.sh)

Examples:
  # Install latest version (static musl build)
  curl -sSfL https://raw.githubusercontent.com/maleus-ai/kepler/master/get-kepler.sh | bash

  # Install specific version
  curl -sSfL https://raw.githubusercontent.com/maleus-ai/kepler/master/get-kepler.sh | bash -s v0.1.0

  # Install glibc build instead of musl
  curl -sSfL https://raw.githubusercontent.com/maleus-ai/kepler/master/get-kepler.sh | bash -s -- --gnu

  # Install specific version without systemd
  curl -sSfL https://raw.githubusercontent.com/maleus-ai/kepler/master/get-kepler.sh | bash -s v0.1.0 --no-systemd
EOF
}

# Parse arguments: first positional arg starting with 'v' is the version, rest forwarded to install.sh
VERSION=""
USE_GNU=false
INSTALL_ARGS=()

for arg in "$@"; do
    case "$arg" in
        -h|--help)
            usage
            exit 0
            ;;
        --gnu)
            USE_GNU=true
            ;;
        v*)
            if [[ -z "$VERSION" ]]; then
                VERSION="$arg"
            else
                INSTALL_ARGS+=("$arg")
            fi
            ;;
        *)
            INSTALL_ARGS+=("$arg")
            ;;
    esac
done

# Detect target triple
detect_target() {
    local arch os libc
    arch="$(uname -m)"
    os="$(uname -s)"

    case "$os" in
        Linux)
            if [[ "$USE_GNU" == true ]]; then
                libc="gnu"
            else
                libc="musl"
            fi
            case "$arch" in
                x86_64)  echo "x86_64-unknown-linux-${libc}" ;;
                aarch64) echo "aarch64-unknown-linux-${libc}" ;;
                *)
                    error "Unsupported architecture: $arch"
                    exit 1
                    ;;
            esac
            ;;
        Darwin)
            error "Pre-built binaries are not available for macOS yet."
            error "See https://github.com/${REPO}#installation for build-from-source instructions."
            exit 1
            ;;
        *)
            error "Unsupported OS: $os"
            exit 1
            ;;
    esac
}

# Detect download tool
detect_downloader() {
    if command -v curl >/dev/null 2>&1; then
        echo "curl"
    elif command -v wget >/dev/null 2>&1; then
        echo "wget"
    else
        error "Neither curl nor wget found. Please install one of them."
        exit 1
    fi
}

# Download a URL to a file
download() {
    local url="$1"
    local output="$2"
    local downloader
    downloader="$(detect_downloader)"

    case "$downloader" in
        curl) curl -sSfL -o "$output" "$url" ;;
        wget) wget -q -O "$output" "$url" ;;
    esac
}

# Resolve "latest" to an actual tag name using the GitHub API
resolve_latest_version() {
    local downloader
    downloader="$(detect_downloader)"

    local api_url="https://api.github.com/repos/${REPO}/releases/latest"
    local response

    case "$downloader" in
        curl) response="$(curl -sSfL "$api_url")" ;;
        wget) response="$(wget -q -O - "$api_url")" ;;
    esac

    # Extract tag_name without requiring jq
    local tag
    tag="$(echo "$response" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')"

    if [[ -z "$tag" ]]; then
        error "Could not determine latest release version"
        error "Check https://github.com/${REPO}/releases for available versions"
        exit 1
    fi

    echo "$tag"
}

# --- Main ---

ARCH="$(detect_target)"

if [[ -z "$VERSION" ]]; then
    info "Resolving latest version..."
    VERSION="$(resolve_latest_version)"
fi

TARBALL="kepler-${VERSION}-${ARCH}.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}"

info "Installing Kepler ${VERSION} (${ARCH})"

# Create temporary directory (cleaned up on exit)
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

# Download
info "Downloading ${TARBALL}..."
download "$DOWNLOAD_URL" "${TMPDIR}/${TARBALL}"

# Extract
info "Extracting..."
tar xzf "${TMPDIR}/${TARBALL}" -C "$TMPDIR"

# Find the extracted directory (kepler-<version>/)
EXTRACT_DIR="${TMPDIR}/kepler-${VERSION}"
if [[ ! -d "$EXTRACT_DIR" ]]; then
    error "Expected directory ${EXTRACT_DIR} not found in tarball"
    exit 1
fi

# Run install script
info "Running install script..."
chmod +x "${EXTRACT_DIR}/install.sh"
"${EXTRACT_DIR}/install.sh" --no-build "${INSTALL_ARGS[@]+"${INSTALL_ARGS[@]}"}"
