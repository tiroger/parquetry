#!/bin/bash
# Render the Homebrew cask (packaging/homebrew/parquetry.rb) for a release.
#
#   scripts/update-cask.sh <version> <zip> [output]
#   scripts/update-cask.sh 0.1.0 target/dist/Parquetry-0.1.0.zip            # -> target/dist/parquetry.rb
#   GITHUB_OWNER=octocat scripts/update-cask.sh 0.1.0 dist/Parquetry-0.1.0.zip ../homebrew-tap/Casks/parquetry.rb
#
# Environment:
#   GITHUB_OWNER   GitHub user/org hosting the parquetry repo and its releases
#                  (default: owner parsed from `repository` in Cargo.toml)
#   OUTPUT         output path (overridden by the 3rd argument; default target/dist/parquetry.rb)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMPLATE="$ROOT/packaging/homebrew/parquetry.rb"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[ $# -ge 2 ] && [ $# -le 3 ] || die "usage: $0 <version> <zip> [output]"
VERSION="${1#v}"
ZIP="$2"
OUT="${3:-${OUTPUT:-${CARGO_TARGET_DIR:-$ROOT/target}/dist/parquetry.rb}}"

[[ $VERSION =~ ^[0-9]+(\.[0-9]+)*([.-][0-9A-Za-z.-]+)?$ ]] || die "invalid version: $VERSION"
[ -f "$ZIP" ] || die "zip not found: $ZIP"

OWNER="${GITHUB_OWNER:-}"
if [ -z "$OWNER" ]; then
	repo="$(awk -F'"' '/^repository *=/ { print $2; exit }' "$ROOT/Cargo.toml")"
	if [[ $repo =~ github\.com/([^/]+)/ ]]; then OWNER="${BASH_REMATCH[1]}"; fi
fi
[ -n "$OWNER" ] || die "set GITHUB_OWNER (could not infer it from Cargo.toml)"
[[ $OWNER =~ ^[A-Za-z0-9-]+$ ]] || die "invalid GITHUB_OWNER: $OWNER"

SHA256="$(shasum -a 256 "$ZIP" | awk '{ print $1 }')"

mkdir -p "$(dirname "$OUT")"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@SHA256@/$SHA256/g" -e "s/@GITHUB_OWNER@/$OWNER/g" \
	"$TEMPLATE" >"$OUT"

if grep -q '@[A-Z_]*@' "$OUT"; then die "unrendered placeholders left in $OUT"; fi
if command -v ruby >/dev/null 2>&1; then ruby -c "$OUT" >/dev/null || die "$OUT is not valid Ruby"; fi

printf 'rendered %s (version %s, sha256 %s, owner %s)\n' "$OUT" "$VERSION" "$SHA256" "$OWNER" >&2
echo "$OUT"
