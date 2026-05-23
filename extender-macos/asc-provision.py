#!/usr/bin/env python3
"""
App Store Connect API helper: register Bundle IDs, enable DriverKit capabilities,
create provisioning profiles, download them.

Bumps the extender bundle IDs to dodge macOS's permanent-cached "silent deny"
for sysextd activation (see palazzo memory 1777633948413).
"""
import base64
import json
import os
import sys
import time
import urllib.request
import urllib.error
from pathlib import Path

import jwt  # PyJWT

# --- vault-loaded ASC creds (from infra/apple-asc) ---
KEY_ID     = "Z43Q26MB7Y"
ISSUER_ID  = "69a6de85-e83f-47e3-e053-5b8c7c11a4d1"
KEY_PATH   = Path.home() / ".privatekeys" / f"AuthKey_{KEY_ID}.p8"
TEAM_ID    = "XJQQCN392F"

API = "https://api.appstoreconnect.apple.com/v1"

# --- target bundle IDs (new, never-before-seen by macOS) ---
HOST_BUNDLE_ID = "com.calibrae.extender.app"
DEXT_BUNDLE_ID = "com.calibrae.extender.app.driver"

DOWNLOAD_DIR = Path.home() / "dump"
DOWNLOAD_DIR.mkdir(parents=True, exist_ok=True)


def mint_token() -> str:
    key = KEY_PATH.read_text()
    now = int(time.time())
    payload = {
        "iss": ISSUER_ID,
        "iat": now,
        "exp": now + 1200,
        "aud": "appstoreconnect-v1",
    }
    return jwt.encode(payload, key, algorithm="ES256", headers={"kid": KEY_ID, "typ": "JWT"})


def req(method: str, path: str, body=None, params=None):
    url = API + path
    if params:
        from urllib.parse import urlencode
        url += "?" + urlencode(params, doseq=True)
    data = json.dumps(body).encode() if body is not None else None
    headers = {
        "Authorization": "Bearer " + mint_token(),
        "Accept": "application/json",
    }
    if data:
        headers["Content-Type"] = "application/json"
    r = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        err = e.read().decode()
        print(f"HTTP {e.code} on {method} {path}:\n{err}", file=sys.stderr)
        raise


def find_bundle_id(identifier: str):
    out = req("GET", "/bundleIds", params={"filter[identifier]": identifier, "limit": 50})
    for item in out.get("data", []):
        if item["attributes"]["identifier"] == identifier:
            return item
    return None


def create_bundle_id(identifier: str, name: str, platform: str = "MAC_OS"):
    body = {
        "data": {
            "type": "bundleIds",
            "attributes": {
                "identifier": identifier,
                "name": name,
                "platform": platform,
                "seedId": TEAM_ID,
            },
        }
    }
    out = req("POST", "/bundleIds", body=body)
    print(f"  created bundle id: {identifier} (id={out['data']['id']})")
    return out["data"]


def enable_capability(bundle_id_pk: str, capability_type: str, settings=None):
    """capability_type is one of Apple's enum values like 'SYSTEM_EXTENSION_INSTALL'
    or for DriverKit a custom string like 'COM_APPLE_DEVELOPER_DRIVERKIT'."""
    body = {
        "data": {
            "type": "bundleIdCapabilities",
            "attributes": {
                "capabilityType": capability_type,
            },
            "relationships": {
                "bundleId": {"data": {"type": "bundleIds", "id": bundle_id_pk}},
            },
        }
    }
    if settings:
        body["data"]["attributes"]["settings"] = settings
    try:
        out = req("POST", "/bundleIdCapabilities", body=body)
        print(f"  enabled capability {capability_type} on bundleId {bundle_id_pk}")
        return out
    except urllib.error.HTTPError:
        # already enabled / not supported — keep going
        return None


def list_devices():
    out = req("GET", "/devices", params={"limit": 200})
    return out.get("data", [])


def find_cert(name_contains: str, cert_type: str | None = None):
    out = req("GET", "/certificates", params={"limit": 200})
    for c in out.get("data", []):
        attrs = c["attributes"]
        if name_contains.lower() not in (attrs.get("name") or "").lower():
            continue
        if cert_type and attrs.get("certificateType") != cert_type:
            continue
        return c
    return None


def create_profile(name: str, profile_type: str, bundle_id_pk: str,
                   cert_ids: list[str], device_ids: list[str]):
    rels = {
        "bundleId": {"data": {"type": "bundleIds", "id": bundle_id_pk}},
        "certificates": {"data": [{"type": "certificates", "id": c} for c in cert_ids]},
    }
    if device_ids:
        rels["devices"] = {"data": [{"type": "devices", "id": d} for d in device_ids]}
    body = {
        "data": {
            "type": "profiles",
            "attributes": {
                "name": name,
                "profileType": profile_type,
            },
            "relationships": rels,
        }
    }
    out = req("POST", "/profiles", body=body)
    print(f"  created profile: {name} (type={profile_type})")
    return out["data"]


def download_profile(profile_data: dict, dest: Path):
    # profileContent comes back base64-encoded directly in the response.
    content_b64 = profile_data["attributes"]["profileContent"]
    dest.write_bytes(base64.b64decode(content_b64))
    print(f"  saved -> {dest}")


def find_or_create_bundle_id(identifier: str, name: str):
    existing = find_bundle_id(identifier)
    if existing:
        print(f"  bundle id already exists: {identifier} (id={existing['id']})")
        return existing
    return create_bundle_id(identifier, name)


def delete_profile_by_name(name: str):
    out = req("GET", "/profiles", params={"filter[name]": name, "limit": 50})
    deleted = 0
    for p in out.get("data", []):
        if p["attributes"]["name"] == name:
            try:
                req("DELETE", f"/profiles/{p['id']}")
                deleted += 1
                print(f"  deleted profile: {name} (id={p['id']})")
            except urllib.error.HTTPError:
                pass
    return deleted


def find_profile(name: str):
    out = req("GET", "/profiles", params={"filter[name]": name, "limit": 50})
    for p in out.get("data", []):
        if p["attributes"]["name"] == name:
            # Re-fetch to get profileContent (filter usually drops it)
            full = req("GET", f"/profiles/{p['id']}")
            return full["data"]
    return None


def main():
    print(f"==> ASC token mint check")
    mint_token()

    # --- 1) Bundle IDs ---
    print(f"==> Bundle ID: {HOST_BUNDLE_ID}")
    host_bid = find_or_create_bundle_id(HOST_BUNDLE_ID, "Extender App v2")

    print(f"==> Bundle ID: {DEXT_BUNDLE_ID}")
    dext_bid = find_or_create_bundle_id(DEXT_BUNDLE_ID, "Extender Driver v2")

    # --- 2) Capabilities ---
    # DriverKit capabilities aren't exposed via the ASC API — Cali clicks
    # those in the portal manually. We just verify-print here.
    print(f"==> Skipping capability enablement (DriverKit caps must be portal-clicked)")

    # --- 3) Find dev cert (Apple Development for build) + Developer ID (for distribution) ---
    print(f"==> Locating certificates")
    dev_cert = find_cert("Nico Bousquet", "DEVELOPMENT")
    developerid_cert = find_cert("Nico Bousquet", "DEVELOPER_ID_APPLICATION")
    print(f"  dev cert:         {dev_cert['attributes']['name']} (id={dev_cert['id']})" if dev_cert else "  dev cert: NOT FOUND")
    print(f"  developerid cert: {developerid_cert['attributes']['name']} (id={developerid_cert['id']})" if developerid_cert else "  developerid cert: NOT FOUND")

    if not dev_cert or not developerid_cert:
        print("Missing required certificates; aborting.")
        sys.exit(1)

    # --- 4) Devices ---
    print(f"==> Locating devices")
    all_devices = list_devices()
    targets = [d for d in all_devices if d["attributes"]["name"] in ("calimba", "speedwagon")]
    device_ids = [d["id"] for d in targets]
    for d in targets:
        print(f"  {d['attributes']['name']}: {d['id']}")
    if not device_ids:
        print("No matching devices; check device registrations in dev portal.")

    # --- 5) DriverKit Dev profile (can be created via API as MAC_APP_DEVELOPMENT) ---
    print(f"==> DriverKit App Development profile (for build validation)")
    dkdev_name = "extender-app-driverkit-dev"
    # Clean any prior attempts to avoid duplicate-name conflicts
    delete_profile_by_name(dkdev_name)
    delete_profile_by_name("extender-app-driverkit-dev-mac-app-development")
    delete_profile_by_name("extender-app-driverkit-dev-ios-app-development")
    dkdev_profile = create_profile(
        name=dkdev_name,
        profile_type="MAC_APP_DEVELOPMENT",
        bundle_id_pk=dext_bid["id"],
        cert_ids=[dev_cert["id"]],
        device_ids=device_ids,
    )

    # --- 6) Download what the API gave us ---
    print(f"==> Downloading profiles to {DOWNLOAD_DIR}")
    # Re-fetch to get profileContent
    dkdev_full = req("GET", f"/profiles/{dkdev_profile['id']}")["data"]
    download_profile(dkdev_full, DOWNLOAD_DIR / "extender-app-driverkit-dev.provisionprofile")

    # --- 7) Developer ID profiles: API doesn't support them; click in portal ---
    print()
    print("==> MANUAL CLICKS REQUIRED for Developer ID profiles (API can't create these)")
    print("    Go to https://developer.apple.com/account/resources/profiles/add")
    print("    Create two profiles, both under Distribution -> Developer ID:")
    print(f"      1) Name: extender-app-host-developerid")
    print(f"         App ID: {HOST_BUNDLE_ID}")
    print(f"         Cert: Developer ID Application")
    print(f"      2) Name: extender-app-dext-developerid")
    print(f"         App ID: {DEXT_BUNDLE_ID}")
    print(f"         Cert: Developer ID Application")
    print(f"    Save each to ~/dump/")

    print(f"==> Done. New bundle IDs: {HOST_BUNDLE_ID} + {DEXT_BUNDLE_ID}")


if __name__ == "__main__":
    main()
