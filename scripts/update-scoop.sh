#!/bin/bash
# Render the Scoop manifest (packaging/scoop/parquetry.json) for a release.
#
#   scripts/update-scoop.sh <version> <zip> [output]
#   scripts/update-scoop.sh 0.2.0 target/dist/Parquetry-0.2.0-windows-x64.zip   # -> target/dist/parquetry.json
#   GITHUB_OWNER=octocat scripts/update-scoop.sh 0.2.0 dist/Parquetry-0.2.0-windows-x64.zip ../scoop-bucket/bucket/parquetry.json
#
# Environment:
#   GITHUB_OWNER   GitHub user/org hosting the parquetry repo and its releases
#                  (default: owner parsed from `repository` in Cargo.toml)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMPLATE="$ROOT/packaging/scoop/parquetry.json"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[ $# -ge 2 ] && [ $# -le 3 ] || die "usage: $0 <version> <zip> [output]"
VERSION="${1#v}"
ZIP="$2"
OUT="${3:-${CARGO_TARGET_DIR:-$ROOT/target}/dist/parquetry.json}"

[[ $VERSION =~ ^[0-9]+(\.[0-9]+)*([.-][0-9A-Za-z.-]+)?$ ]] || die "invalid version: $VERSION"
[ -f "$ZIP" ] || die "zip not found: $ZIP"

OWNER="${GITHUB_OWNER:-}"
if [ -z "$OWNER" ]; then
	repo="$(awk -F'"' '/^repository *=/ { print $2; exit }' "$ROOT/Cargo.toml")"
	if [[ $repo =~ github\.com/([^/]+)/ ]]; then OWNER="${BASH_REMATCH[1]}"; fi
fi
[ -n "$OWNER" ] || die "set GITHUB_OWNER (could not infer it from Cargo.toml)"
[[ $OWNER =~ ^[A-Za-z0-9-]+$ ]] || die "invalid GITHUB_OWNER: $OWNER"

if command -v sha256sum >/dev/null 2>&1; then
	SHA256="$(sha256sum "$ZIP" | awk '{ print $1 }')"
else
	SHA256="$(shasum -a 256 "$ZIP" | awk '{ print $1 }')"
fi

mkdir -p "$(dirname "$OUT")"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@SHA256@/$SHA256/g" -e "s/@GITHUB_OWNER@/$OWNER/g" \
	"$TEMPLATE" >"$OUT"

if grep -q '@[A-Z_0-9]*@' "$OUT"; then die "unrendered placeholders left in $OUT"; fi
if command -v python3 >/dev/null 2>&1; then
	python3 -m json.tool "$OUT" >/dev/null || die "$OUT is not valid JSON"
fi

printf 'rendered %s (version %s, sha256 %s, owner %s)\n' "$OUT" "$VERSION" "$SHA256" "$OWNER" >&2
echo "$OUT"
