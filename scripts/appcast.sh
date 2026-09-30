#!/bin/bash
# Generate the Sparkle update feed (appcast.xml) for a release zip.
#
#   scripts/appcast.sh                      # uses target/dist/Parquetry-<version>.zip
#   scripts/appcast.sh path/to/Parquetry-1.2.0.zip
#
# The zip must contain the final (signed, notarized, stapled) Parquetry.app. The
# feed lists that one release; Sparkle only needs the newest. Publish appcast.xml
# as an asset of the same GitHub release as the zip: the app's feed URL is
# https://github.com/tiroger/parquetry/releases/latest/download/appcast.xml
#
# Environment:
#   SPARKLE_ED_KEY_FILE    EdDSA private key (default: ~/.parquetry/sparkle_ed25519_private.key)
#   SPARKLE_PRIVATE_KEY    the key itself (base64), used instead of the file (CI)
#   DOWNLOAD_URL_PREFIX    where the zip is published
#                          (default: https://github.com/tiroger/parquetry/releases/download/v<version>/)
#   SPARKLE_VERSION        Sparkle release whose tools to use (default: 2.10.0)
#   CARGO_TARGET_DIR       as for bundle.sh; the feed is written to <it>/dist/appcast.xml
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
DIST="$TARGET_DIR/dist"
SPARKLE_VERSION="${SPARKLE_VERSION:-2.10.0}"
SPARKLE_BIN="$TARGET_DIR/sparkle_cache/$SPARKLE_VERSION/bin"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' "$ROOT/Cargo.toml" | head -1)"
ZIP="${1:-$DIST/Parquetry-$VERSION.zip}"
[ -f "$ZIP" ] || die "$ZIP not found (run scripts/bundle.sh and scripts/package.sh first)"
[ -x "$SPARKLE_BIN/generate_appcast" ] || die "Sparkle tools missing in $SPARKLE_BIN (scripts/bundle.sh downloads them)"
PREFIX="${DOWNLOAD_URL_PREFIX:-https://github.com/tiroger/parquetry/releases/download/v$VERSION/}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/archives"
cp "$ZIP" "$WORK/archives/"

if [ -n "${SPARKLE_PRIVATE_KEY:-}" ]; then
	KEY_FILE="$WORK/key"
	printf '%s' "$SPARKLE_PRIVATE_KEY" >"$KEY_FILE"
	chmod 600 "$KEY_FILE"
else
	KEY_FILE="${SPARKLE_ED_KEY_FILE:-$HOME/.parquetry/sparkle_ed25519_private.key}"
fi
[ -f "$KEY_FILE" ] || die "no signing key: set SPARKLE_PRIVATE_KEY or SPARKLE_ED_KEY_FILE"

# generate_appcast merges into an existing feed; start fresh so it lists only this zip.
rm -f "$DIST/appcast.xml"
"$SPARKLE_BIN/generate_appcast" \
	--ed-key-file "$KEY_FILE" \
	--download-url-prefix "$PREFIX" \
	--link "https://github.com/tiroger/parquetry" \
	-o "$DIST/appcast.xml" \
	"$WORK/archives" >&2

grep -q 'sparkle:edSignature' "$DIST/appcast.xml" || die "appcast has no EdDSA signature"
echo "$DIST/appcast.xml"
