#!/bin/bash
# Package target/dist/Parquetry.app (built by scripts/bundle.sh) into
#   target/dist/Parquetry-<version>.zip   (ditto; what Homebrew and notarytool consume)
#   target/dist/Parquetry-<version>.dmg   (UDZO, app + /Applications symlink)
# and print their SHA-256 (also written to target/dist/SHA256SUMS).
#
#   scripts/bundle.sh && scripts/package.sh
#
# Environment:
#   CODESIGN_IDENTITY  when a real identity (not "-"), the .dmg is signed too
#   PACKAGE_FORMATS    "zip dmg" (default), or just "zip" / "dmg"
#   CARGO_TARGET_DIR   as for bundle.sh (dist dir is <it>/dist)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$ROOT/$TARGET_DIR" ;; esac
DIST="$TARGET_DIR/dist"
APP="$DIST/Parquetry.app"
IDENTITY="${CODESIGN_IDENTITY:--}"
FORMATS="${PACKAGE_FORMATS:-zip dmg}"
VOLNAME="Parquetry"

if [ -t 2 ]; then C_B=$'\033[1;34m' C_R=$'\033[1;31m' C_0=$'\033[0m'; else C_B="" C_R="" C_0=""; fi
log() { printf '%s==>%s %s\n' "$C_B" "$C_0" "$*" >&2; }
die() { printf '%serror:%s %s\n' "$C_R" "$C_0" "$*" >&2; exit 1; }

[ -d "$APP" ] || die "$APP not found; run scripts/bundle.sh first"
codesign --verify --deep --strict "$APP" || die "$APP has an invalid signature; re-run scripts/bundle.sh"

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
ZIP="$DIST/Parquetry-$VERSION.zip"
DMG="$DIST/Parquetry-$VERSION.dmg"
OUTPUTS=()

for fmt in $FORMATS; do
	case "$fmt" in
		zip)
			log "Creating $ZIP"
			rm -f "$ZIP"
			# --keepParent: the archive contains Parquetry.app/ at its root.
			ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
			OUTPUTS+=("$ZIP")
			;;
		dmg)
			log "Creating $DMG"
			STAGE="$(mktemp -d "${TMPDIR:-/tmp}/parquetry-dmg.XXXXXX")"
			trap 'rm -rf "$STAGE"' EXIT
			ditto "$APP" "$STAGE/Parquetry.app"
			ln -s /Applications "$STAGE/Applications"
			rm -f "$DMG"
			hdiutil create -quiet -volname "$VOLNAME" -srcfolder "$STAGE" -fs HFS+ \
				-format UDZO -imagekey zlib-level=9 -ov "$DMG"
			rm -rf "$STAGE"
			trap - EXIT
			if [ "$IDENTITY" != "-" ]; then
				log "Signing $DMG"
				codesign --force --sign "$IDENTITY" --timestamp "$DMG"
			fi
			hdiutil verify -quiet "$DMG"
			OUTPUTS+=("$DMG")
			;;
		*) die "unknown format in PACKAGE_FORMATS: $fmt" ;;
	esac
done

[ "${#OUTPUTS[@]}" -gt 0 ] || die "nothing to package (PACKAGE_FORMATS is empty)"

log "SHA-256"
(
	cd "$DIST"
	for f in "$ZIP" "$DMG"; do
		if [ -f "$f" ]; then shasum -a 256 "$(basename "$f")"; fi
	done
) | tee "$DIST/SHA256SUMS"
