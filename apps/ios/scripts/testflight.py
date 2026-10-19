#!/usr/bin/env python3
"""Sign openADE for TestFlight; upload only when explicitly invoked with --upload."""

import argparse
import base64
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import HTTPRedirectHandler, Request, build_opener
import uuid
import zipfile


ROOT = Path(__file__).resolve().parents[1]
# The phone app shares the desktop's version number.
REPOSITORY = ROOT.parents[1]
BUNDLE_ID = "dev.openade.remote"
SECRET_NAMES = (
    "IOS_DISTRIBUTION_P12_BASE64", "IOS_DISTRIBUTION_P12_PASSWORD", "IOS_APP_STORE_PROFILE_BASE64",
    "APPLE_TEAM_ID", "APP_STORE_CONNECT_KEY_ID", "APP_STORE_CONNECT_ISSUER_ID",
    "APP_STORE_CONNECT_PRIVATE_KEY_BASE64",
)
REDACTIONS = []


def credentials(env):
    missing = [name for name in SECRET_NAMES if not env.get(name, "").strip()]
    if missing:
        raise RuntimeError("Missing GitHub Secrets: " + ", ".join(missing))
    values = {name: env[name] for name in SECRET_NAMES}
    for name in ("APPLE_TEAM_ID", "APP_STORE_CONNECT_KEY_ID"):
        if not re.fullmatch(r"[A-Z0-9]{10}", values[name]):
            raise RuntimeError(f"{name} must contain its 10-character Apple identifier")
    try:
        uuid.UUID(values["APP_STORE_CONNECT_ISSUER_ID"])
    except ValueError as error:
        raise RuntimeError("APP_STORE_CONNECT_ISSUER_ID must be the issuer UUID") from error
    for name in ("IOS_DISTRIBUTION_P12_BASE64", "IOS_APP_STORE_PROFILE_BASE64", "APP_STORE_CONNECT_PRIVATE_KEY_BASE64"):
        try:
            if not base64.b64decode("".join(values[name].split()), validate=True):
                raise ValueError("empty")
        except ValueError as error:
            raise RuntimeError(f"{name} must contain a nonempty Base64 file") from error
    return values


def redact(text):
    for value in sorted(set(REDACTIONS), key=len, reverse=True):
        if value:
            text = text.replace(value, "[REDACTED]")
    return text


def execute(label, command, env, *, log=None, data=None, check=True, timeout=1800):
    # Security commands never print their arguments or output; build logs are redacted before saving.
    print(label, flush=True)
    try:
        result = subprocess.run(command, cwd=ROOT, env=env, input=data, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        if log:
            log.write_text(redact((error.stdout or b"").decode(errors="replace")))
        if check:
            raise RuntimeError(f"{label} timed out") from error
        return b""
    if log:
        safe = redact(result.stdout.decode(errors="replace"))
        log.write_text(safe)
        print(safe, end="", flush=True)
    if check and result.returncode:
        raise RuntimeError(f"{label} failed ({result.returncode})" + (f"; see {log.name}" if log else ""))
    return result.stdout


def jwt_signature(der):
    """OpenSSL emits DER ECDSA integers; JWT requires their fixed-width concatenation."""
    if len(der) < 6 or der[0] != 0x30 or der[1] != len(der) - 2:
        raise RuntimeError("Invalid ES256 signature from OpenSSL")
    offset, result = 2, b""
    for _ in range(2):
        if offset + 2 > len(der) or der[offset] != 2:
            raise RuntimeError("Invalid ES256 integer")
        length = der[offset + 1]
        value = der[offset + 2:offset + 2 + length]
        if not value or len(value) != length or length > 33 or value[0] & 0x80:
            raise RuntimeError("Invalid ES256 integer size")
        number = int.from_bytes(value, "big")
        if not 0 < number < 2**256:
            raise RuntimeError("Invalid ES256 integer value")
        result += number.to_bytes(32, "big")
        offset += length + 2
    if offset != len(der):
        raise RuntimeError("Unexpected ES256 signature data")
    return result


def asc(values, key, env, path, body=None):
    """One App Store Connect API call with a fresh token; a body makes it a POST."""
    encode = lambda data: base64.urlsafe_b64encode(data).rstrip(b"=")
    header = {"alg": "ES256", "kid": values["APP_STORE_CONNECT_KEY_ID"], "typ": "JWT"}
    now = int(time.time())
    claims = {"iss": values["APP_STORE_CONNECT_ISSUER_ID"], "iat": now - 5,
              "exp": now + 300, "aud": "appstoreconnect-v1"}
    message = b".".join(encode(json.dumps(item, separators=(",", ":")).encode()) for item in (header, claims))
    signature = execute("Authenticate App Store Connect", ["openssl", "dgst", "-sha256", "-sign", str(key)],
                        env, data=message, timeout=30)
    token = (message + b"." + encode(jwt_signature(signature))).decode()

    class NoRedirect(HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None

    request = Request("https://api.appstoreconnect.apple.com" + path, data=body and json.dumps(body).encode(),
                      headers={"Authorization": "Bearer " + token, "Accept": "application/json",
                               "Content-Type": "application/json"})
    try:
        with build_opener(NoRedirect).open(request, timeout=30) as response:
            return json.loads(response.read() or b"null")
    except HTTPError as error:
        raise RuntimeError(f"App Store Connect API returned HTTP {error.code} for {path.split('?')[0]}; "
                           "check the team API key and app access") from error


def app_record(values, key, env):
    query = urlencode({"filter[bundleId]": BUNDLE_ID, "fields[apps]": "name,bundleId", "limit": "1"})
    apps = asc(values, key, env, "/v1/apps?" + query)["data"]
    if not apps or apps[0]["attributes"]["bundleId"] != BUNDLE_ID:
        raise RuntimeError(f"Create the openADE app record in App Store Connect with bundle ID {BUNDLE_ID} first")
    return apps[0]["id"]


def deliver(values, key, env, app_id, version, build, wait=time.sleep):
    """Wait for Apple to process the upload, then add it to every internal TestFlight group."""
    query = urlencode({"filter[app]": app_id, "filter[version]": build,
                       "filter[preReleaseVersion.version]": version, "fields[builds]": "processingState"})
    for _ in range(60):
        builds = asc(values, key, env, "/v1/builds?" + query)["data"]
        state = builds[0]["attributes"]["processingState"] if builds else "UPLOADED"
        print(f"Apple processing {version} ({build}): {state}", flush=True)
        if state == "VALID":
            break
        if state in ("FAILED", "INVALID"):
            raise RuntimeError(f"Apple marked {version} ({build}) {state}")
        wait(30)
    else:
        raise RuntimeError(f"Apple has not processed {version} ({build}) in 30 minutes; add it to a group in App Store Connect")
    groups = asc(values, key, env, "/v1/betaGroups?" + urlencode({"filter[app]": app_id, "filter[isInternalGroup]": "true",
                                                                  "fields[betaGroups]": "name"}))["data"]
    if not groups:
        raise RuntimeError("The app has no internal TestFlight group to deliver to")
    for group in groups:
        asc(values, key, env, f"/v1/betaGroups/{group['id']}/relationships/builds",
            {"data": [{"type": "builds", "id": builds[0]["id"]}]})
        print(f"Delivered {version} ({build}) to TestFlight group {group['attributes']['name']}", flush=True)


def validate_profile(profile, team, now=None):
    entitlements = profile.get("Entitlements", {})
    identifiers = {f"{prefix}.{BUNDLE_ID}" for prefix in profile.get("ApplicationIdentifierPrefix", [])}
    if team not in profile.get("TeamIdentifier", []) or entitlements.get("com.apple.developer.team-identifier") != team:
        raise RuntimeError("The provisioning profile belongs to another Apple team")
    if entitlements.get("application-identifier") not in identifiers:
        raise RuntimeError(f"The provisioning profile must explicitly match {BUNDLE_ID}")
    expiry = profile.get("ExpirationDate")
    if not isinstance(expiry, datetime) or expiry.replace(tzinfo=timezone.utc) <= (now or datetime.now(timezone.utc)):
        raise RuntimeError("The provisioning profile has expired")
    if ("iOS" not in profile.get("Platform", []) or profile.get("ProvisionedDevices") is not None
            or profile.get("ProvisionsAllDevices") or entitlements.get("get-task-allow")
            or entitlements.get("beta-reports-active") is not True):
        raise RuntimeError("Use an App Store Connect distribution profile, not Development, Ad Hoc or Enterprise")
    uuid.UUID(profile["UUID"])
    if not profile.get("DeveloperCertificates"):
        raise RuntimeError("The provisioning profile has no distribution certificate")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--preflight", action="store_true", help="check credentials, SDK, profile and the real app record, then stop")
    parser.add_argument("--upload", action="store_true", help="validate and upload the signed IPA to TestFlight")
    args = parser.parse_args()
    values = credentials(os.environ)
    REDACTIONS.extend(values.values())
    if sys.platform != "darwin":
        raise RuntimeError("Use the TestFlight GitHub Actions workflow on its macOS runner")
    env = os.environ.copy()
    public_env = {name: value for name, value in env.items() if name not in SECRET_NAMES}
    output = ROOT / "build" / "testflight"
    artifacts = output / "artifacts"
    artifacts.mkdir(parents=True, exist_ok=True)
    os.umask(0o077)
    xcode = execute("Check distribution Xcode", ["xcodebuild", "-version"], public_env).decode()
    sdk = execute("Check distribution SDK", ["xcrun", "--sdk", "iphoneos", "--show-sdk-version"], public_env).decode().strip()
    # App Store Connect takes builds made with the iOS 26 SDK or later.
    if not re.match(r"Xcode (2[6-9]|[3-9][0-9])\.", xcode) or int(sdk.split(".")[0]) < 26:
        raise RuntimeError("TestFlight needs Xcode 26 / iOS SDK 26 or later; use the pinned runner")

    with tempfile.TemporaryDirectory(prefix="openade-testflight-", dir=env.get("RUNNER_TEMP")) as directory:
        private = Path(directory)
        files = (("certificate.p12", "IOS_DISTRIBUTION_P12_BASE64"),
                 ("profile.mobileprovision", "IOS_APP_STORE_PROFILE_BASE64"),
                 (f"AuthKey_{values['APP_STORE_CONNECT_KEY_ID']}.p8", "APP_STORE_CONNECT_PRIVATE_KEY_BASE64"))
        for filename, name in files:
            (private / filename).write_bytes(base64.b64decode("".join(values[name].split()), validate=True))
        key = private / files[2][0]
        REDACTIONS.append(key.read_text())
        profile = plistlib.loads(execute("Read provisioning profile", ["security", "cms", "-D", "-i", str(private / files[1][0])], public_env))
        validate_profile(profile, values["APPLE_TEAM_ID"])
        REDACTIONS.extend([profile["UUID"], profile.get("Name", "")])
        app_id = app_record(values, key, public_env)
        print("Preflight passed: Xcode/SDK 26, App Store profile and openADE app record exist.", flush=True)
        if args.preflight:
            return

        # Only this invocation's keychain and profile are installed; existing user files remain intact.
        keychain = private / "signing.keychain-db"
        password = base64.urlsafe_b64encode(os.urandom(32)).decode()
        old_keychains = shlex.split(execute("Read keychain search list", ["security", "list-keychains", "-d", "user"], public_env).decode())
        installed = Path.home() / "Library" / "Developer" / "Xcode" / "UserData" / "Provisioning Profiles" / f"openADE-{uuid.uuid4()}.mobileprovision"
        created = False
        try:
            execute("Create temporary signing keychain", ["security", "create-keychain", "-p", password, str(keychain)], public_env)
            created = True
            execute("Configure signing keychain", ["security", "set-keychain-settings", "-lut", "21600", str(keychain)], public_env)
            execute("Unlock signing keychain", ["security", "unlock-keychain", "-p", password, str(keychain)], public_env)
            execute("Import distribution certificate", ["security", "import", str(private / files[0][0]), "-P", values["IOS_DISTRIBUTION_P12_PASSWORD"],
                    "-t", "cert", "-f", "pkcs12", "-k", str(keychain), "-T", "/usr/bin/codesign", "-T", "/usr/bin/security"], public_env)
            execute("Allow Apple signing tools", ["security", "set-key-partition-list", "-S", "apple-tool:,apple:", "-s", "-k", password, str(keychain)], public_env)
            identities = execute("Verify signing identity", ["security", "find-identity", "-v", "-p", "codesigning", str(keychain)], public_env).decode()
            # SHA-1 is the identity format used by codesign, not a new integrity algorithm.
            fingerprints = {hashlib.sha1(cert).hexdigest().upper() for cert in profile["DeveloperCertificates"]}
            matches = [(fingerprint.upper(), name) for fingerprint, name in re.findall(r'([A-Fa-f0-9]{40})\s+"((?:Apple|iPhone) Distribution:[^"]+)"', identities)
                       if fingerprint.upper() in fingerprints]
            if len(matches) != 1:
                raise RuntimeError("The P12 must contain the valid Apple Distribution private key/certificate selected by the profile")
            identity, name = matches[0]
            REDACTIONS.extend([identity, name])
            execute("Use temporary signing keychain", ["security", "list-keychains", "-d", "user", "-s", str(keychain), *old_keychains], public_env)
            installed.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(private / files[1][0], installed)

            execute("Generate the Xcode project", [sys.executable, str(ROOT / "scripts" / "generate_project.py")], public_env)
            with (REPOSITORY / "Cargo.toml").open("rb") as source:
                version = tomllib.load(source)["workspace"]["package"]["version"]
            # Above every build EAS uploaded before (up to 4), and unique per attempt.
            build = f"{100 + int(env.get('GITHUB_RUN_NUMBER', '1'))}.{env.get('GITHUB_RUN_ATTEMPT', '1')}"
            archive = output / "OpenADERemote.xcarchive"
            execute("Archive signed openADE", ["xcodebuild", "-project", str(ROOT / "ios" / "OpenADERemote.xcodeproj"), "-scheme", "OpenADERemote",
                    "-configuration", "Release", "-sdk", "iphoneos", "-destination", "generic/platform=iOS", "-archivePath", str(archive),
                    "CODE_SIGN_STYLE=Manual", "CODE_SIGNING_ALLOWED=YES", f"CODE_SIGN_IDENTITY={identity}",
                    f"DEVELOPMENT_TEAM={values['APPLE_TEAM_ID']}", f"PROVISIONING_PROFILE_SPECIFIER={profile['UUID']}",
                    f"OTHER_CODE_SIGN_FLAGS=--keychain {keychain}", f"PRODUCT_BUNDLE_IDENTIFIER={BUNDLE_ID}",
                    f"MARKETING_VERSION={version}", f"CURRENT_PROJECT_VERSION={build}", "archive"], public_env,
                    log=artifacts / "archive.log")
            options = private / "ExportOptions.plist"
            options.write_bytes(plistlib.dumps({"method": "app-store-connect", "destination": "export", "signingStyle": "manual",
                "teamID": values["APPLE_TEAM_ID"], "signingCertificate": identity,
                "provisioningProfiles": {BUNDLE_ID: profile["UUID"]}, "manageAppVersionAndBuildNumber": False}))
            exported = output / "export"
            execute("Export App Store Connect IPA", ["xcodebuild", "-exportArchive", "-archivePath", str(archive),
                    "-exportOptionsPlist", str(options), "-exportPath", str(exported)], public_env,
                    log=artifacts / "export.log")
            ipa = artifacts / "OpenADERemote.ipa"
            shutil.copyfile(exported / "OpenADERemote.ipa", ipa)
            with zipfile.ZipFile(ipa) as package:
                info = plistlib.loads(package.read("Payload/OpenADERemote.app/Info.plist"))
                if info["CFBundleIdentifier"] != BUNDLE_ID or int(re.search(r"\d+", info["DTSDKName"])[0]) < 26:
                    raise RuntimeError("Exported IPA has the wrong bundle ID or SDK")
            if args.upload:
                upload_env = {**public_env, "API_PRIVATE_KEYS_DIR": str(private)}
                auth = ["-f", str(ipa), "-t", "ios", "--apiKey", values["APP_STORE_CONNECT_KEY_ID"],
                        "--apiIssuer", values["APP_STORE_CONNECT_ISSUER_ID"], "--output-format", "json"]
                execute("Validate IPA with Apple", ["xcrun", "altool", "--validate-app", *auth], upload_env, log=artifacts / "validate.log")
                execute("Upload IPA to TestFlight", ["xcrun", "altool", "--upload-app", *auth], upload_env, log=artifacts / "upload.log")
            (artifacts / "result.json").write_text(json.dumps({"app_id": app_id, "bundle_id": BUNDLE_ID, "version": version,
                "build": build, "sdk": sdk, "uploaded": args.upload,
                "next": "The workflow adds the processed build to every internal TestFlight group." if args.upload
                        else "Run the workflow with upload to deliver a build."}, indent=2) + "\n")
        finally:
            execute("Restore keychain search list", ["security", "list-keychains", "-d", "user", "-s", *old_keychains], public_env, check=False, timeout=30)
            if created:
                execute("Delete temporary signing keychain", ["security", "delete-keychain", str(keychain)], public_env, check=False, timeout=30)
            installed.unlink(missing_ok=True)
        if args.upload:
            deliver(values, key, public_env, app_id, version, build)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, KeyError) as error:
        print(redact(f"TestFlight failed: {error}"), file=sys.stderr)
        sys.exit(1)
