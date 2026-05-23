#!/usr/bin/env bash
# Notarize the signed Release ExtenderApp.app via Apple's notarytool + staple the ticket.
#
# Prereqs:
#   - .app must already be Developer ID signed (CODE_SIGN_IDENTITY="Developer ID Application")
#   - ASC API key on disk at $KEY_PATH (default: ~/.privatekeys/AuthKey_Z43Q26MB7Y.p8)
#   - ASC issuer ID + key ID as env vars (defaults below from vault infra/apple-asc)
set -euo pipefail

cd "$(dirname "$0")"

APP="${APP:-build/derived/Build/Products/Release/ExtenderApp.app}"
KEY_ID="${KEY_ID:-Z43Q26MB7Y}"
ISSUER_ID="${ISSUER_ID:-69a6de85-e83f-47e3-e053-5b8c7c11a4d1}"
KEY_PATH="${KEY_PATH:-$HOME/.privatekeys/AuthKey_${KEY_ID}.p8}"

[[ -d "$APP" ]]      || { echo "App not found at $APP"; exit 1; }
[[ -f "$KEY_PATH" ]] || { echo "ASC API key not found at $KEY_PATH"; exit 1; }

# Notarytool requires the app zipped (or .dmg/.pkg). Zip is simplest.
ZIP="$(dirname "$APP")/$(basename "$APP" .app).zip"
echo "==> Zipping $APP → $ZIP"
rm -f "$ZIP"
/usr/bin/ditto -c -k --keepParent "$APP" "$ZIP"

echo "==> Submitting to notary service (this typically takes 1–10 min)..."
SUB_OUT=$(xcrun notarytool submit "$ZIP" \
    --key "$KEY_PATH" \
    --key-id "$KEY_ID" \
    --issuer "$ISSUER_ID" \
    --wait \
    --output-format plist)
echo "$SUB_OUT"

# Pull the status out of the plist
STATUS=$(echo "$SUB_OUT" | /usr/bin/plutil -extract status raw - 2>/dev/null || echo "unknown")
SUB_ID=$(echo "$SUB_OUT" | /usr/bin/plutil -extract id raw - 2>/dev/null || echo "")

if [[ "$STATUS" != "Accepted" ]]; then
    echo
    echo "==> Notarization NOT Accepted (status=$STATUS). Fetching log..."
    if [[ -n "$SUB_ID" ]]; then
        xcrun notarytool log "$SUB_ID" \
            --key "$KEY_PATH" --key-id "$KEY_ID" --issuer "$ISSUER_ID" \
            /tmp/notarize-fail.json
        echo "Full log written to /tmp/notarize-fail.json. Issues:"
        /usr/bin/jq -r '.issues[]? | "  \(.severity): \(.message) (\(.path):\(.architecture // "?"))"' /tmp/notarize-fail.json 2>/dev/null || cat /tmp/notarize-fail.json
    fi
    exit 1
fi

echo "==> Stapling ticket to $APP"
xcrun stapler staple "$APP"

echo "==> Verifying"
xcrun stapler validate "$APP"
spctl -a -vvv -t install "$APP" 2>&1 | head -5

echo
echo "Notarization complete. The .app is now Gatekeeper-trusted."
