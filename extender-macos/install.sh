#!/usr/bin/env bash
# Build, sign, install the Extender app + DriverKit system extension on this Mac.
#
# One-time setup before this works:
#   1. Register THIS device in your Apple Developer portal:
#      https://developer.apple.com/account/resources/devices/list
#      Provisioning UDID for this machine:
set -euo pipefail

cd "$(dirname "$0")"

PROVISIONING_UDID=$(system_profiler SPHardwareDataType 2>/dev/null | awk -F': ' '/Provisioning UDID/ {print $2}')
HOSTNAME_=$(scutil --get LocalHostName 2>/dev/null || hostname)
TEAM_ID="XJQQCN392F"
SCHEME="ExtenderApp"
CONFIG="Release"
APP_NAME="Extender.app"
DEXT_BUNDLE="com.calibrae.extender.driver"

usage() {
    cat <<EOF
Usage: $0 [--check | --build | --install | --activate | --status | --uninstall]

  --check      Verify prerequisites (certs, profiles, device registration)
  --build      Build the Release app (re-runs xcodegen first)
  --install    Build then copy the .app to /Applications
  --activate   Launch the installed .app; it will request system extension activation
  --status     Show systemextensionsctl list for our dext
  --uninstall  Uninstall the system extension and remove the .app

  No arg = --check then prompt to proceed with --install + --activate.

Setup checklist:
  1. Register this Mac in the Apple Developer portal:
       https://developer.apple.com/account/resources/devices/list
     Provisioning UDID: $PROVISIONING_UDID
     Name suggestion:   $HOSTNAME_
  2. Confirm DriverKit entitlements are granted on the App ID
     ($DEXT_BUNDLE) — they were approved 2026-03-31.
  3. Open Xcode at least once and sign in with the team account
     ($TEAM_ID) so Xcode can fetch/create the provisioning profile.
EOF
}

cmd_check() {
    echo "==> Codesigning identities:"
    security find-identity -v -p codesigning | grep -E "Apple Development|Developer ID|Apple Distribution" || echo "  (none found)"
    echo
    echo "==> Provisioning profiles for our bundle IDs:"
    local found=0
    for p in ~/Library/MobileDevice/Provisioning\ Profiles/*.mobileprovision; do
        [[ -f "$p" ]] || continue
        local plist
        plist=$(security cms -D -i "$p" 2>/dev/null)
        if echo "$plist" | grep -qE "com\.calibrae\.extender(\.driver)?"; then
            local name
            name=$(echo "$plist" | plutil -extract Name raw - 2>/dev/null)
            echo "  $(basename "$p"): $name"
            found=$((found + 1))
        fi
    done
    [[ $found -eq 0 ]] && echo "  (none — Xcode will create on first build with -allowProvisioningUpdates)"
    echo
    echo "==> Device registration:"
    echo "  Hostname:           $HOSTNAME_"
    echo "  Provisioning UDID:  $PROVISIONING_UDID"
    echo "  Register at: https://developer.apple.com/account/resources/devices/list"
    echo
    echo "==> Currently installed system extensions:"
    systemextensionsctl list 2>/dev/null | grep -E "$DEXT_BUNDLE|enabled|active" | head -5 || echo "  (none)"
}

cmd_build() {
    command -v xcodegen >/dev/null || { echo "xcodegen not installed (brew install xcodegen)"; exit 1; }
    echo "==> Regenerating Xcode project"
    xcodegen generate

    echo "==> Building $SCHEME ($CONFIG)"
    xcodebuild \
        -project Extender.xcodeproj \
        -scheme "$SCHEME" \
        -configuration "$CONFIG" \
        -derivedDataPath build/derived \
        -allowProvisioningUpdates \
        DEVELOPMENT_TEAM="$TEAM_ID" \
        build
}

cmd_install() {
    cmd_build

    local built_app
    built_app="build/derived/Build/Products/$CONFIG/$APP_NAME"
    [[ -d "$built_app" ]] || { echo "Built app not found at $built_app"; exit 1; }

    local target="/Applications/$APP_NAME"
    if [[ -d "$target" ]]; then
        echo "==> Removing existing $target"
        rm -rf "$target"
    fi
    echo "==> Copying $built_app -> $target"
    cp -R "$built_app" "$target"

    echo "==> Re-signing in /Applications (may need keychain unlock)"
    codesign --verify --deep --strict --verbose=1 "$target" || true

    echo
    echo "Installed. Run '$0 --activate' to launch and trigger the system extension prompt."
}

cmd_activate() {
    local target="/Applications/$APP_NAME"
    [[ -d "$target" ]] || { echo "$target not found — run --install first."; exit 1; }
    echo "==> Launching $APP_NAME (it will request system extension activation)"
    open "$target"
    echo
    echo "Next steps (in macOS UI):"
    echo "  1. macOS will prompt: 'System Extension Blocked'"
    echo "  2. Open System Settings > General > Login Items & Extensions > Driver Extensions"
    echo "  3. Toggle Extender on"
    echo "  4. Confirm with Touch ID / password"
    echo
    echo "Then check status with: $0 --status"
}

cmd_status() {
    systemextensionsctl list
}

cmd_uninstall() {
    echo "==> Uninstalling $DEXT_BUNDLE"
    systemextensionsctl uninstall "$TEAM_ID" "$DEXT_BUNDLE" || true
    rm -rf "/Applications/$APP_NAME"
    echo "Done."
}

case "${1:-}" in
    --check)     cmd_check ;;
    --build)     cmd_build ;;
    --install)   cmd_install ;;
    --activate)  cmd_activate ;;
    --status)    cmd_status ;;
    --uninstall) cmd_uninstall ;;
    -h|--help)   usage ;;
    "")
        cmd_check
        echo
        read -r -p "Proceed with --install + --activate? [y/N] " ans
        if [[ "$ans" =~ ^[Yy]$ ]]; then
            cmd_install
            cmd_activate
        fi
        ;;
    *)
        echo "Unknown argument: $1"
        usage
        exit 1
        ;;
esac
