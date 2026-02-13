#!/usr/bin/env bash
set -euo pipefail

# Kepler install script
# Builds as the current user, then uses sudo for installation steps.

BIN_DIR="/usr/local/bin"
STATE_DIR="/var/lib/kepler"
SYSTEMD_DIR="/etc/systemd/system"
LAUNCHD_DIR="/Library/LaunchDaemons"
PLIST_NAME="com.kepler.daemon"
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

# OS detection
case "$(uname -s)" in
    Linux)  OS="linux" ;;
    Darwin) OS="darwin" ;;
    *)
        error "Unsupported operating system: $(uname -s)"
        exit 1
        ;;
esac

# Run a command as root (directly if already root, via sudo otherwise)
as_root() {
    if [[ $EUID -eq 0 ]]; then
        "$@"
    else
        sudo "$@"
    fi
}

# --- OS-aware group/user helpers ---
group_exists() {
    if [[ "$OS" == "linux" ]]; then
        getent group kepler >/dev/null 2>&1
    else
        dscl . -read /Groups/kepler >/dev/null 2>&1
    fi
}

create_group() {
    if [[ "$OS" == "linux" ]]; then
        as_root groupadd kepler
    else
        as_root dseditgroup -o create kepler
    fi
}

add_user_to_group() {
    local user="$1"
    if [[ "$OS" == "linux" ]]; then
        as_root usermod -aG kepler "$user"
    else
        as_root dseditgroup -o edit -a "$user" -t user kepler
    fi
}

user_in_group() {
    local user="$1"
    id -nG "$user" 2>/dev/null | grep -qw kepler
}

usage() {
    cat <<EOF
Usage: $0 [OPTIONS]

Install or uninstall Kepler.
Build runs as the current user. Installation steps use sudo when needed.

Options:
  --no-service      Skip service installation (systemd on Linux, launchd on macOS)
  --no-systemd      Alias for --no-service (backwards compatibility)
  --no-build        Skip cargo build (use existing target/release binaries)
  --uninstall       Remove Kepler binaries and service
  -h, --help        Show this help
EOF
}

# Parse arguments
OPT_SERVICE="yes"
OPT_UNINSTALL=false
OPT_BUILD=true

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-service|--no-systemd)
            OPT_SERVICE="no"
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

# Auto-detect binaries next to install.sh (e.g. extracted tarball)
if [[ -f "${SCRIPT_DIR}/kepler" ]] && [[ -f "${SCRIPT_DIR}/kepler-daemon" ]] && [[ -f "${SCRIPT_DIR}/kepler-exec" ]]; then
    TARGET_DIR="$SCRIPT_DIR"
else
    TARGET_DIR="${SCRIPT_DIR}/target/release"
fi

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

    if [[ "$OS" == "linux" ]]; then
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
    else
        # Unload and remove launchd plist if present
        if [[ -f "${LAUNCHD_DIR}/${PLIST_NAME}.plist" ]]; then
            info "Unloading launchd service"
            as_root launchctl bootout system/"${PLIST_NAME}" 2>/dev/null || true
            info "Removing launchd plist"
            as_root rm -f "${LAUNCHD_DIR}/${PLIST_NAME}.plist"
        fi
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
    if [[ "$OS" == "linux" ]]; then
        echo "    sudo groupdel kepler"
    else
        echo "    sudo dseditgroup -o delete kepler"
    fi
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

# Step 2: Stop running daemon before replacing binaries
WAS_RUNNING=false
if [[ "$OPT_SERVICE" != "no" ]]; then
    if [[ "$OS" == "linux" ]] && systemctl is-active --quiet "$SERVICE_NAME" 2>/dev/null; then
        WAS_RUNNING=true
        info "Stopping running kepler daemon"
        as_root systemctl stop "$SERVICE_NAME"
    elif [[ "$OS" == "darwin" ]] && launchctl print system/"${PLIST_NAME}" >/dev/null 2>&1; then
        WAS_RUNNING=true
        info "Stopping running kepler daemon"
        as_root launchctl bootout system/"${PLIST_NAME}"
    fi
fi

# Step 3: Install binaries (requires root)
info "Installing binaries to ${BIN_DIR}"
as_root mkdir -p "${BIN_DIR}"
for bin in "${BINARIES[@]}"; do
    as_root cp "${TARGET_DIR}/${bin}" "${BIN_DIR}/${bin}"
    as_root chmod 755 "${BIN_DIR}/${bin}"
    echo "  ${BIN_DIR}/${bin}"
done

# Step 4: Create kepler group
if group_exists; then
    info "Group 'kepler' already exists"
else
    info "Creating 'kepler' group"
    create_group
fi

# Step 5: Create state directory
if [[ -d "${STATE_DIR}" ]]; then
    info "State directory ${STATE_DIR} already exists"
else
    info "Creating state directory ${STATE_DIR}"
    as_root mkdir -p "${STATE_DIR}"
fi
as_root chown root:kepler "${STATE_DIR}"
as_root chmod 0770 "${STATE_DIR}"

# Step 6: Service installation
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

install_launchd() {
    info "Installing launchd service"
    as_root tee "${LAUNCHD_DIR}/${PLIST_NAME}.plist" >/dev/null <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>${PLIST_NAME}</string>
    <key>ProgramArguments</key>
    <array>
        <string>${BIN_DIR}/kepler-daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>/var/log/kepler.log</string>
    <key>StandardErrorPath</key>
    <string>/var/log/kepler.log</string>
</dict>
</plist>
PLIST

    as_root launchctl bootstrap system/ "${LAUNCHD_DIR}/${PLIST_NAME}.plist"
    info "Launchd service installed and loaded"
    echo "  The daemon will start automatically at boot."
    echo "  Start now with: sudo launchctl kickstart system/${PLIST_NAME}"
}

if [[ "$OPT_SERVICE" == "no" ]]; then
    info "Skipping service installation"
elif [[ "$OS" == "linux" ]]; then
    if command -v systemctl >/dev/null 2>&1; then
        install_systemd
    else
        warn "systemctl not found, skipping systemd service"
    fi
elif [[ "$OS" == "darwin" ]]; then
    if command -v launchctl >/dev/null 2>&1; then
        install_launchd
    else
        warn "launchctl not found, skipping launchd service"
    fi
fi

# Step 7: Add user to kepler group
CURRENT_USER="${SUDO_USER:-${USER:-}}"
if [[ -n "$CURRENT_USER" ]] && [[ "$CURRENT_USER" != "root" ]]; then
    if user_in_group "$CURRENT_USER"; then
        info "User '$CURRENT_USER' is already in the kepler group"
    else
        echo ""
        if [ -t 0 ]; then
            read -rp "Add user '$CURRENT_USER' to the kepler group? [Y/n] " answer
            case "$answer" in
                [nN]|[nN][oO]) ;;
                *)
                    add_user_to_group "$CURRENT_USER"
                    info "Added '$CURRENT_USER' to the kepler group"
                    warn "You must log out and log back in for group changes to take effect."
                    ;;
            esac
        else
            warn "Non-interactive shell — skipping group prompt."
            if [[ "$OS" == "linux" ]]; then
                echo "  Add user manually: sudo usermod -aG kepler $CURRENT_USER"
            else
                echo "  Add user manually: sudo dseditgroup -o edit -a $CURRENT_USER -t user kepler"
            fi
        fi
    fi
else
    echo ""
    warn "No non-root user detected."
    if [[ "$OS" == "linux" ]]; then
        echo "  Add users manually:  sudo usermod -aG kepler <username>"
    else
        echo "  Add users manually:  sudo dseditgroup -o edit -a <username> -t user kepler"
    fi
    echo "  Then log out and log back in for group changes to take effect."
fi

# Step 8: Restart daemon if it was running before install
if $WAS_RUNNING; then
    info "Restarting kepler daemon"
    if [[ "$OS" == "linux" ]]; then
        as_root systemctl start "$SERVICE_NAME"
    else
        as_root launchctl kickstart system/"${PLIST_NAME}"
    fi
fi

# Done
echo ""
info "Installation complete!"
if ! $WAS_RUNNING; then
    echo ""
    echo "  Start the daemon:"
    if [[ "$OS" == "linux" ]]; then
        echo "    sudo systemctl start kepler   (if systemd service was installed)"
    else
        echo "    sudo launchctl kickstart system/${PLIST_NAME}   (if launchd service was installed)"
    fi
    echo "    sudo kepler daemon start -d   (manual)"
fi
echo ""
