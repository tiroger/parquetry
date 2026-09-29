#!/bin/bash
# Notarize and staple a Developer ID-signed Parquetry build.
#
#   CODESIGN_IDENTITY="Developer ID Application: Jane Doe (TEAMID1234)" scripts/bundle.sh
#   CODESIGN_IDENTITY=... scripts/package.sh
#   CODESIGN_IDENTITY=... NOTARY_PROFILE=parquetry-notary scripts/notarize.sh
#
# Credentials (one of):
#   NOTARY_PROFILE                              keychain profile created with
#                                               `xcrun notarytool store-credentials`
#   APPLE_ID + APPLE_TEAM_ID + APPLE_APP_PASSWORD   Apple ID, team ID, app-specific password
#
# Steps:
#   1. submit Parquetry-<v>.zip (the app) and wait; staple + validate the .app
#   2. re-run scripts/package.sh so the zip and dmg contain the stapled app
#   3. submit the (signed) .dmg and wait; staple + validate the .dmg
#   4. print final SHA-256s (target/dist/SHA256SUMS)
#
# Environment: CARGO_TARGET_DIR as for bundle.sh; SKIP_DMG=1 to notarize only the app/zip.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$ROOT/$TARGET_DIR" ;; esac
DIST="$TARGET_DIR/dist"
APP="$DIST/Parquetry.app"
IDENTITY="${CODESIGN_IDENTITY:-}"

if [ -t 2 ]; then C_B=$'\033[1;34m' C_R=$'\033[1;31m' C_0=$'\033[0m'; else C_B="" C_R="" C_0=""; fi
log() { printf '%s==>%s %s\n' "$C_B" "$C_0" "$*" >&2; }
die() { printf '%serror:%s %s\n' "$C_R" "$C_0" "$*" >&2; exit 1; }

# --------------------------------------------------------------------------- #
# Preconditions
# --------------------------------------------------------------------------- #
case "$IDENTITY" in
	"Developer ID Application: "*) ;;
	"" | "-") die "CODESIGN_IDENTITY must be a 'Developer ID Application: …' identity (ad-hoc builds can't be notarized).
       Installed identities:  security find-identity -v -p codesigning" ;;
	*) die "CODESIGN_IDENTITY is '$IDENTITY'; notarization requires a 'Developer ID Application: …' identity" ;;
esac

for tool in notarytool stapler; do
	if ! xcrun --find "$tool" >/dev/null 2>&1; then
		die "xcrun can't find '$tool' (active developer dir: $(xcode-select -p 2>/dev/null || echo none)).
       notarytool and stapler ship with the Xcode Command Line Tools (xcode-select --install)
       and with Xcode. If you rely on Xcode.app, accept its license and select it:
           sudo xcodebuild -license accept
           sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
	fi
done

AUTH=()
if [ -n "${NOTARY_PROFILE:-}" ]; then
	AUTH=(--keychain-profile "$NOTARY_PROFILE")
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
	AUTH=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD")
else
	die "no notarization credentials. Set either
         NOTARY_PROFILE=<profile>   (xcrun notarytool store-credentials <profile> --apple-id … --team-id …)
       or
         APPLE_ID, APPLE_TEAM_ID and APPLE_APP_PASSWORD (an app-specific password from appleid.apple.com)"
fi

[ -d "$APP" ] || die "$APP not found; run scripts/bundle.sh and scripts/package.sh first"
SIG_INFO="$(codesign -dvv "$APP" 2>&1)"
grep -q "^Authority=Developer ID Application: " <<<"$SIG_INFO" ||
	die "$APP is not signed with a Developer ID certificate; rebuild with CODESIGN_IDENTITY set"
grep -q "flags=.*runtime" <<<"$SIG_INFO" || die "$APP was signed without the hardened runtime"
grep -q "^Timestamp=" <<<"$SIG_INFO" || die "$APP signature has no secure timestamp"

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
ZIP="$DIST/Parquetry-$VERSION.zip"
DMG="$DIST/Parquetry-$VERSION.dmg"
if [ ! -f "$ZIP" ]; then
	log "$ZIP missing; packaging"
	PACKAGE_FORMATS=zip "$ROOT/scripts/package.sh" >/dev/null
fi

# --------------------------------------------------------------------------- #
# Helpers
# --------------------------------------------------------------------------- #
WORK="$(mktemp -d "${TMPDIR:-/tmp}/parquetry-notary.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# submit FILE: upload, wait for a verdict, print the log on failure.
submit() {
	local file=$1 out="$WORK/submit.json" id status
	log "Submitting $(basename "$file") to Apple's notary service (this usually takes a few minutes)"
	if ! xcrun notarytool submit "$file" "${AUTH[@]}" --wait --timeout 1h --output-format json >"$out"; then
		cat "$out" >&2 || true
	fi
	id="$(plutil -extract id raw -o - "$out" 2>/dev/null || true)"
	status="$(plutil -extract status raw -o - "$out" 2>/dev/null || true)"
	log "Submission ${id:-?}: ${status:-unknown}"
	if [ "$status" != "Accepted" ]; then
		if [ -n "$id" ]; then
			log "Notary log:"
			xcrun notarytool log "$id" "${AUTH[@]}" >&2 || true
		fi
		die "notarization of $(basename "$file") failed (status: ${status:-unknown})"
	fi
}

# --------------------------------------------------------------------------- #
# 1. App (via zip)
# --------------------------------------------------------------------------- #
submit "$ZIP"
log "Stapling $APP"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
spctl --assess --type execute --verbose=2 "$APP"

# --------------------------------------------------------------------------- #
# 2. Re-package with the stapled app
# --------------------------------------------------------------------------- #
if [ "${SKIP_DMG:-0}" = "1" ]; then
	PACKAGE_FORMATS=zip CODESIGN_IDENTITY="$IDENTITY" "$ROOT/scripts/package.sh" >/dev/null
else
	PACKAGE_FORMATS="zip dmg" CODESIGN_IDENTITY="$IDENTITY" "$ROOT/scripts/package.sh" >/dev/null

	# ----------------------------------------------------------------------- #
	# 3. DMG
	# ----------------------------------------------------------------------- #
	submit "$DMG"
	log "Stapling $DMG"
	xcrun stapler staple "$DMG"
	xcrun stapler validate "$DMG"
	spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG"
fi

# --------------------------------------------------------------------------- #
# 4. Checksums (the dmg changed when it was stapled)
# --------------------------------------------------------------------------- #
FINAL=("$ZIP")
[ "${SKIP_DMG:-0}" = "1" ] || FINAL+=("$DMG")
log "SHA-256"
(cd "$DIST" && for f in "${FINAL[@]}"; do shasum -a 256 "$(basename "$f")"; done) | tee "$DIST/SHA256SUMS"
log "Notarized and stapled: ${FINAL[*]}"
