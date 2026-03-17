#!/usr/bin/env bash
#
# Extender install script
#
# Detects macOS / Linux, downloads the correct binary from GitHub releases,
# installs it to /usr/local/bin, and sets up the system service (launchd or
# systemd).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/calibrae/extender/main/extender/packaging/install.sh | bash
#
# Environment variables:
#   EXTENDER_VERSION  - version to install (default: latest)
#   INSTALL_DIR       - binary install directory (default: /usr/local/bin)

set -euo pipefail

REPO="calibrae/extender"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
VERSION="${EXTENDER_VERSION:-latest}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

info()  { printf "\033[1;34m==> %s\033[0m\n" "$*"; }
error() { printf "\033[1;31mERROR: %s\033[0m\n" "$*" >&2; exit 1; }

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || error "Required command not found: $1"
}

detect_os() {
    case "$(uname -s)" in
        Darwin) echo "macos" ;;
        Linux)  echo "linux" ;;
        *)      error "Unsupported operating system: $(uname -s)" ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)   echo "x86_64" ;;
        arm64|aarch64)  echo "aarch64" ;;
        *)              error "Unsupported architecture: $(uname -m)" ;;
    esac
}

# ---------------------------------------------------------------------------
# Resolve version
# ---------------------------------------------------------------------------

resolve_version() {
    if [ "$VERSION" = "latest" ]; then
        need_cmd curl
        VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' | head -1 | sed 's/.*"v\(.*\)".*/\1/')
        [ -n "$VERSION" ] || error "Could not determine latest version"
    fi
    info "Installing Extender v${VERSION}"
}

# ---------------------------------------------------------------------------
# Download and install binary
# ---------------------------------------------------------------------------

install_binary() {
    local os="$1" arch="$2"
    local url="https://github.com/${REPO}/releases/download/v${VERSION}/extender-${VERSION}-${arch}-${os}.tar.gz"
    local tmpdir
    tmpdir=$(mktemp -d)

    info "Downloading from ${url}"
    need_cmd curl
    need_cmd tar

    curl -fsSL "$url" -o "${tmpdir}/extender.tar.gz" \
        || error "Download failed. Check that version v${VERSION} exists."

    tar -xzf "${tmpdir}/extender.tar.gz" -C "$tmpdir"

    info "Installing to ${INSTALL_DIR}/extender"
    install -d "$INSTALL_DIR"
    install -m 755 "${tmpdir}/extender" "${INSTALL_DIR}/extender"

    rm -rf "$tmpdir"
    info "Binary installed successfully"
}

# ---------------------------------------------------------------------------
# Linux: create user/group and install systemd service
# ---------------------------------------------------------------------------

setup_linux() {
    info "Setting up systemd service"

    # Create extender user/group if they do not exist.
    if ! id -u extender >/dev/null 2>&1; then
        info "Creating extender system user"
        if command -v useradd >/dev/null 2>&1; then
            groupadd --system extender 2>/dev/null || true
            useradd --system --no-create-home --shell /usr/sbin/nologin \
                --gid extender extender
        else
            error "useradd not found; please create the 'extender' user manually"
        fi
    fi

    # Install systemd unit.
    local service_src
    service_src="$(cd "$(dirname "$0")" && pwd)/systemd/extender.service"
    local service_dst="/etc/systemd/system/extender.service"

    if [ -f "$service_src" ]; then
        install -m 644 "$service_src" "$service_dst"
    else
        # Fetch from repo if running via curl pipe.
        curl -fsSL "https://raw.githubusercontent.com/${REPO}/main/extender/packaging/systemd/extender.service" \
            -o "$service_dst"
        chmod 644 "$service_dst"
    fi

    systemctl daemon-reload
    systemctl enable extender.service
    info "Systemd service installed and enabled (start with: systemctl start extender)"
}

# ---------------------------------------------------------------------------
# macOS: install launchd plist
# ---------------------------------------------------------------------------

setup_macos() {
    info "Setting up launchd service"

    local plist_dst="/Library/LaunchDaemons/com.extender.daemon.plist"
    local plist_src
    plist_src="$(cd "$(dirname "$0")" && pwd)/launchd/com.extender.daemon.plist"

    if [ -f "$plist_src" ]; then
        install -m 644 "$plist_src" "$plist_dst"
    else
        curl -fsSL "https://raw.githubusercontent.com/${REPO}/main/extender/packaging/launchd/com.extender.daemon.plist" \
            -o "$plist_dst"
        chmod 644 "$plist_dst"
    fi

    # Create log directory.
    mkdir -p /usr/local/var/log
    mkdir -p /usr/local/var/extender

    info "Launchd plist installed (load with: sudo launchctl load ${plist_dst})"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
    local os arch
    os=$(detect_os)
    arch=$(detect_arch)

    resolve_version
    install_binary "$os" "$arch"

    case "$os" in
        linux) setup_linux ;;
        macos) setup_macos ;;
    esac

    info "Extender v${VERSION} installed successfully!"
    info "Run 'extender --help' to get started."
}

main "$@"
