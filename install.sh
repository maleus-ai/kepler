#!/usr/bin/env bash
set -euo pipefail

# Kepler install script
# Builds as the current user, then uses sudo for installation steps.

BIN_DIR="/usr/local/bin"
STATE_DIR="/var/lib/kepler"
SYSTEMD_DIR="/etc/systemd/system"
SERVICE_NAME="kepler"
BINARIES=("kepler" "kepler-daemon" "kepler-exec")

# Colors (disabled if not a terminal)
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

# Run a command as root (directly if already root, via sudo otherwise)
as_root() {
    if [[ $EUID -eq 0 ]]; then
        "$@"
    else
        sudo "$@"
    fi
}

usage() {
    cat <<EOF
Usage: $0 [OPTIONS]

Install or uninstall Kepler.
Build runs as the current user. Installation steps use sudo when needed.

Options:
  --no-systemd      Skip systemd service installation
  --no-build        Skip cargo build (use existing target/release binaries)
  --uninstall       Remove Kepler binaries and systemd service
  -h, --help        Show this help
EOF
}

# Parse arguments
OPT_SYSTEMD="yes"
OPT_UNINSTALL=false
OPT_BUILD=true

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-systemd)
            OPT_SYSTEMD="no"
            shift
            ;;
        --no-build)
            OPT_BUILD=false
            shift
            ;;
        --uninstall)
            OPT_UNINSTALL=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            error "Unknown option: $1"
            usage
            exit 1
            ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TARGET_DIR="${SCRIPT_DIR}/target/release"

# Acquire sudo upfront (skip if already root)
if [[ $EUID -ne 0 ]]; then
    info "Root privileges required for installation. Requesting sudo..."
    if ! sudo -v; then
        error "Failed to obtain sudo privileges"
        exit 1
    fi
fi

# --- Uninstall ---
if $OPT_UNINSTALL; then
    info "Uninstalling Kepler"

    # Stop and disable systemd service if present
    if systemctl is-active --quiet "$SERVICE_NAME" 2>/dev/null; then
        info "Stopping kepler service"
        as_root systemctl stop "$SERVICE_NAME"
    fi
    if systemctl is-enabled --quiet "$SERVICE_NAME" 2>/dev/null; then
        as_root systemctl disable "$SERVICE_NAME"
    fi
    if [[ -f "${SYSTEMD_DIR}/${SERVICE_NAME}.service" ]]; then
        info "Removing systemd service"
        as_root rm -f "${SYSTEMD_DIR}/${SERVICE_NAME}.service"
        as_root systemctl daemon-reload
    fi

    # Remove binaries
    for bin in "${BINARIES[@]}"; do
        if [[ -f "${BIN_DIR}/${bin}" ]]; then
            info "Removing ${BIN_DIR}/${bin}"
            as_root rm -f "${BIN_DIR}/${bin}"
        fi
    done

    info "Uninstall complete"
    echo ""
    warn "The kepler group and ${STATE_DIR} were NOT removed."
    echo "  To remove them manually:"
    echo "    sudo rm -rf ${STATE_DIR}"
    echo "    sudo groupdel kepler"
    exit 0
fi

# --- Install ---
info "Installing Kepler"

# Step 1: Build (as current user, no root needed)
if $OPT_BUILD; then
    if ! command -v cargo >/dev/null 2>&1; then
        error "cargo not found. Install Rust via https://rustup.rs/"
        exit 1
    fi
    info "Building release binaries"
    cargo build --release --manifest-path "${SCRIPT_DIR}/Cargo.toml"
fi

# Verify binaries exist
for bin in "${BINARIES[@]}"; do
    if [[ ! -f "${TARGET_DIR}/${bin}" ]]; then
        error "Binary not found: ${TARGET_DIR}/${bin}"
        error "Run 'cargo build --release' first, or remove --no-build"
        exit 1
    fi
done

# Step 2: Install binaries (requires root)
info "Installing binaries to ${BIN_DIR}"
as_root mkdir -p "${BIN_DIR}"
for bin in "${BINARIES[@]}"; do
    as_root cp "${TARGET_DIR}/${bin}" "${BIN_DIR}/${bin}"
    as_root chmod 755 "${BIN_DIR}/${bin}"
    echo "  ${BIN_DIR}/${bin}"
done

# Step 3: Create kepler group
if getent group kepler >/dev/null 2>&1; then
    info "Group 'kepler' already exists"
else
    info "Creating 'kepler' group"
    as_root groupadd kepler
fi

# Step 4: Create state directory
if [[ -d "${STATE_DIR}" ]]; then
    info "State directory ${STATE_DIR} already exists"
else
    info "Creating state directory ${STATE_DIR}"
    as_root mkdir -p "${STATE_DIR}"
fi
as_root chown root:kepler "${STATE_DIR}"
as_root chmod 0770 "${STATE_DIR}"

# Step 5: Systemd service
install_systemd() {
    info "Installing systemd service"
    as_root tee "${SYSTEMD_DIR}/${SERVICE_NAME}.service" >/dev/null <<UNIT
[Unit]
Description=Kepler Process Orchestrator
After=network.target

[Service]
Type=simple
ExecStart=${BIN_DIR}/kepler-daemon
ExecStop=${BIN_DIR}/kepler daemon stop
Restart=on-failure

[Install]
WantedBy=multi-user.target
UNIT

    as_root systemctl daemon-reload
    as_root systemctl enable "$SERVICE_NAME"
    info "Systemd service installed and enabled"
    echo "  Start with: sudo systemctl start kepler"
}

if [[ "$OPT_SYSTEMD" == "no" ]]; then
    info "Skipping systemd service"
elif command -v systemctl >/dev/null 2>&1; then
    install_systemd
else
    warn "systemctl not found, skipping systemd service"
fi

# Step 6: Add user to kepler group
CURRENT_USER="${SUDO_USER:-${USER:-}}"
if [[ -n "$CURRENT_USER" ]] && [[ "$CURRENT_USER" != "root" ]]; then
    if id -nG "$CURRENT_USER" 2>/dev/null | grep -qw kepler; then
        info "User '$CURRENT_USER' is already in the kepler group"
    else
        echo ""
        read -rp "Add user '$CURRENT_USER' to the kepler group? [Y/n] " answer
        case "${answer,,}" in
            n|no) ;;
            *)
                as_root usermod -aG kepler "$CURRENT_USER"
                info "Added '$CURRENT_USER' to the kepler group"
                warn "You must log out and log back in for group changes to take effect."
                ;;
        esac
    fi
else
    echo ""
    warn "No non-root user detected."
    echo "  Add users manually:  sudo usermod -aG kepler <username>"
    echo "  Then log out and log back in for group changes to take effect."
fi

# Done
echo ""
info "Installation complete!"
echo ""
echo "  Start the daemon:"
echo "    sudo systemctl start kepler   (if systemd service was installed)"
echo "    sudo kepler daemon start -d   (manual)"
echo ""
