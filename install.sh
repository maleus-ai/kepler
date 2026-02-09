#!/usr/bin/env bash
set -euo pipefail

# Kepler install script
# Builds, installs binaries, creates kepler group, sets up state directory,
# and optionally installs a systemd service.

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

usage() {
    cat <<EOF
Usage: sudo $0 [OPTIONS]

Install or uninstall Kepler.

Options:
  --systemd         Install systemd service (non-interactive)
  --no-systemd      Skip systemd service (non-interactive)
  --no-build        Skip cargo build (use existing target/release binaries)
  --uninstall       Remove Kepler binaries and systemd service
  -h, --help        Show this help
EOF
}

# Parse arguments
OPT_SYSTEMD=""
OPT_UNINSTALL=false
OPT_BUILD=true

while [[ $# -gt 0 ]]; do
    case "$1" in
        --systemd)
            OPT_SYSTEMD="yes"
            shift
            ;;
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

# Require root
if [[ $EUID -ne 0 ]]; then
    error "This script must be run as root (sudo $0)"
    exit 1
fi

# --- Uninstall ---
if $OPT_UNINSTALL; then
    info "Uninstalling Kepler"

    # Stop and disable systemd service if present
    if systemctl is-active --quiet "$SERVICE_NAME" 2>/dev/null; then
        info "Stopping kepler service"
        systemctl stop "$SERVICE_NAME"
    fi
    if systemctl is-enabled --quiet "$SERVICE_NAME" 2>/dev/null; then
        systemctl disable "$SERVICE_NAME"
    fi
    if [[ -f "${SYSTEMD_DIR}/${SERVICE_NAME}.service" ]]; then
        info "Removing systemd service"
        rm -f "${SYSTEMD_DIR}/${SERVICE_NAME}.service"
        systemctl daemon-reload
    fi

    # Remove binaries
    for bin in "${BINARIES[@]}"; do
        if [[ -f "${BIN_DIR}/${bin}" ]]; then
            info "Removing ${BIN_DIR}/${bin}"
            rm -f "${BIN_DIR}/${bin}"
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

# Step 1: Build
if $OPT_BUILD; then
    info "Building release binaries"
    SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
    cargo build --release --manifest-path "${SCRIPT_DIR}/Cargo.toml"
    TARGET_DIR="${SCRIPT_DIR}/target/release"
else
    TARGET_DIR="$(cd "$(dirname "$0")" && pwd)/target/release"
fi

# Verify binaries exist
for bin in "${BINARIES[@]}"; do
    if [[ ! -f "${TARGET_DIR}/${bin}" ]]; then
        error "Binary not found: ${TARGET_DIR}/${bin}"
        error "Run 'cargo build --release' first, or remove --no-build"
        exit 1
    fi
done

# Step 2: Install binaries
info "Installing binaries to ${BIN_DIR}"
mkdir -p "${BIN_DIR}"
for bin in "${BINARIES[@]}"; do
    cp "${TARGET_DIR}/${bin}" "${BIN_DIR}/${bin}"
    chmod 755 "${BIN_DIR}/${bin}"
    echo "  ${BIN_DIR}/${bin}"
done

# Step 3: Create kepler group
if getent group kepler >/dev/null 2>&1; then
    info "Group 'kepler' already exists"
else
    info "Creating 'kepler' group"
    groupadd kepler
fi

# Step 4: Create state directory
if [[ -d "${STATE_DIR}" ]]; then
    info "State directory ${STATE_DIR} already exists"
else
    info "Creating state directory ${STATE_DIR}"
    mkdir -p "${STATE_DIR}"
fi
chown root:kepler "${STATE_DIR}"
chmod 0770 "${STATE_DIR}"

# Step 5: Systemd service
install_systemd() {
    info "Installing systemd service"
    cat > "${SYSTEMD_DIR}/${SERVICE_NAME}.service" <<UNIT
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

    systemctl daemon-reload
    systemctl enable "$SERVICE_NAME"
    info "Systemd service installed and enabled"
    echo "  Start with: sudo systemctl start kepler"
}

if [[ "$OPT_SYSTEMD" == "yes" ]]; then
    install_systemd
elif [[ "$OPT_SYSTEMD" == "no" ]]; then
    info "Skipping systemd service"
else
    # Interactive prompt
    if command -v systemctl >/dev/null 2>&1; then
        echo ""
        read -rp "Install systemd service? [Y/n] " answer
        case "${answer,,}" in
            n|no) info "Skipping systemd service" ;;
            *)    install_systemd ;;
        esac
    else
        warn "systemctl not found, skipping systemd service"
    fi
fi

# Step 6: Add user to kepler group
SUDO_USER="${SUDO_USER:-}"
if [[ -n "$SUDO_USER" ]] && [[ "$SUDO_USER" != "root" ]]; then
    if id -nG "$SUDO_USER" 2>/dev/null | grep -qw kepler; then
        info "User '$SUDO_USER' is already in the kepler group"
    else
        echo ""
        read -rp "Add user '$SUDO_USER' to the kepler group? [Y/n] " answer
        case "${answer,,}" in
            n|no) ;;
            *)
                usermod -aG kepler "$SUDO_USER"
                info "Added '$SUDO_USER' to the kepler group"
                warn "You must log out and log back in for group changes to take effect."
                ;;
        esac
    fi
else
    echo ""
    warn "No non-root user detected (run with sudo to auto-detect)."
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
