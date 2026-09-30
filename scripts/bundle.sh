#!/bin/bash
# Build and assemble target/dist/Parquetry.app (signed ad-hoc by default).
#
#   scripts/bundle.sh
#   UNIVERSAL=1 scripts/bundle.sh
#   SKIP_BUILD=1 SKIP_QUICKLOOK=1 scripts/bundle.sh
#   CODESIGN_IDENTITY="Developer ID Application: Jane Doe (TEAMID1234)" scripts/bundle.sh
#
# Environment:
#   PROFILE            cargo profile: release (default) | debug/dev | any custom profile
#   UNIVERSAL=1        build aarch64 + x86_64 and lipo them into one binary
#   SKIP_BUILD=1       don't run cargo (or the Quick Look build); use existing binaries
#   CODESIGN_IDENTITY  signing identity (default "-" = ad-hoc). A real identity also
#                      gets a secure timestamp, which notarization requires.
#   SKIP_QUICKLOOK=1   don't build or embed the Quick Look extension
#   DUCKDB_EXT_LAYOUT  how to bundle DuckDB extensions (see packaging/README.md):
#                        none (default) don't bundle; DuckDB downloads extensions on first use.
#                             Required for notarization: Apple rejects DuckDB's own extension
#                             signatures, even inside .gz files.
#                        repo Resources/duckdb_extensions/<duckdb-version>/<platform>/<ext>.duckdb_extension.gz
#                        raw  Resources/duckdb_extensions/<platform>/<ext>.duckdb_extension
#   DUCKDB_EXTENSIONS  space-separated extensions to bundle (default: httpfs)
#   DUCKDB_VERSION     e.g. v1.5.5 (default: derived from libduckdb-sys in Cargo.lock)
#   BUILD_NUMBER       CFBundleVersion (default: git commit count, else a UTC date stamp).
#                      Sparkle compares this number, so it must grow with every release.
#   SKIP_SPARKLE=1     don't embed Sparkle (no automatic updates)
#   SPARKLE_VERSION    Sparkle release to embed (default: 2.10.0)
#   SPARKLE_FEED_URL   appcast URL baked into Info.plist
#                      (default: https://github.com/tiroger/parquetry/releases/latest/download/appcast.xml)
#   SPARKLE_PUBLIC_KEY EdDSA public key for update signatures (default: packaging/sparkle_public_key.txt)
#   CARGO_TARGET_DIR   cargo target dir (default: <repo>/target); output goes to <it>/dist
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$ROOT/$TARGET_DIR" ;; esac
export CARGO_TARGET_DIR="$TARGET_DIR"

PROFILE="${PROFILE:-release}"
UNIVERSAL="${UNIVERSAL:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"
SKIP_QUICKLOOK="${SKIP_QUICKLOOK:-0}"
IDENTITY="${CODESIGN_IDENTITY:--}"
EXT_LAYOUT="${DUCKDB_EXT_LAYOUT:-none}"
EXTENSIONS="${DUCKDB_EXTENSIONS:-httpfs}"
SKIP_SPARKLE="${SKIP_SPARKLE:-0}"
SPARKLE_VERSION="${SPARKLE_VERSION:-2.10.0}"
SPARKLE_FEED_URL="${SPARKLE_FEED_URL:-https://github.com/tiroger/parquetry/releases/latest/download/appcast.xml}"

APP_NAME="Parquetry"
BIN_NAME="parquetry"
DIST="$TARGET_DIR/dist"
APP="$DIST/$APP_NAME.app"
CONTENTS="$APP/Contents"
PACKAGING="$ROOT/packaging"
ENTITLEMENTS="$PACKAGING/Parquetry.entitlements"
QL_NAME="ParquetryQuickLook.appex"
QL_APPEX="$TARGET_DIR/quicklook/$QL_NAME"
QL_ENTITLEMENTS="$ROOT/quicklook/QuickLook.entitlements"
EXT_CACHE="$TARGET_DIR/duckdb_extensions_cache"
SPARKLE_CACHE="$TARGET_DIR/sparkle_cache"

if [ -t 2 ]; then C_B=$'\033[1;34m' C_Y=$'\033[1;33m' C_R=$'\033[1;31m' C_0=$'\033[0m'; else C_B="" C_Y="" C_R="" C_0=""; fi
log() { printf '%s==>%s %s\n' "$C_B" "$C_0" "$*" >&2; }
warn() { printf '%swarning:%s %s\n' "$C_Y" "$C_0" "$*" >&2; }
die() { printf '%serror:%s %s\n' "$C_R" "$C_0" "$*" >&2; exit 1; }

if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
	# shellcheck disable=SC1091
	source "$HOME/.cargo/env"
fi

# --------------------------------------------------------------------------- #
# Versions
# --------------------------------------------------------------------------- #
VERSION="$(awk '
	/^\[/ { in_section = ($0 == "[workspace.package]") }
	in_section && $1 == "version" { gsub(/[" ]/, "", $3); print $3; exit }
' "$ROOT/Cargo.toml")"
[ -n "$VERSION" ] || die "could not read [workspace.package] version from Cargo.toml"

if [ -n "${BUILD_NUMBER:-}" ]; then
	BUILD="$BUILD_NUMBER"
elif BUILD="$(git -C "$ROOT" rev-list --count HEAD 2>/dev/null)" && [ -n "$BUILD" ]; then
	:
else
	BUILD="$(date -u +%Y%m%d.%H%M)"
fi

duckdb_version() {
	if [ -n "${DUCKDB_VERSION:-}" ]; then
		echo "$DUCKDB_VERSION"
		return
	fi
	# duckdb-rs encodes DuckDB 1.5.5 as crate version 1.10505.0 (MAJOR.MNNPP.x).
	local crate
	crate="$(awk '/^name = "libduckdb-sys"$/ { getline; gsub(/[" ]/, "", $0); sub(/^version=/, ""); print; exit }' "$ROOT/Cargo.lock" 2>/dev/null || true)"
	if [[ $crate =~ ^1\.([0-9])([0-9]{2})([0-9]{2})\. ]]; then
		printf 'v%d.%d.%d\n' "${BASH_REMATCH[1]}" "$((10#${BASH_REMATCH[2]}))" "$((10#${BASH_REMATCH[3]}))"
	else
		echo "v1.5.5"
	fi
}

# --------------------------------------------------------------------------- #
# Build
# --------------------------------------------------------------------------- #
case "$PROFILE" in
	debug | dev) CARGO_PROFILE="dev"; PROFILE_DIR="debug" ;;
	*) CARGO_PROFILE="$PROFILE"; PROFILE_DIR="$PROFILE" ;;
esac

TRIPLES=()
if [ "$UNIVERSAL" = "1" ]; then
	TRIPLES=(aarch64-apple-darwin x86_64-apple-darwin)
	if [ "$SKIP_BUILD" != "1" ]; then
		command -v rustup >/dev/null 2>&1 || die "UNIVERSAL=1 needs rustup to cross-compile"
		installed="$(rustup target list --installed)"
		missing=()
		for t in "${TRIPLES[@]}"; do
			grep -qx "$t" <<<"$installed" || missing+=("$t")
		done
		if [ "${#missing[@]}" -gt 0 ]; then
			die "missing Rust target(s): ${missing[*]}
       add them with:  rustup target add ${missing[*]}"
		fi
	fi
fi

if [ "$SKIP_BUILD" != "1" ]; then
	command -v cargo >/dev/null 2>&1 || die "cargo not found (install Rust from https://rustup.rs)"
	if [ "${#TRIPLES[@]}" -eq 0 ]; then
		log "Building $BIN_NAME ($CARGO_PROFILE)"
		cargo build --manifest-path "$ROOT/Cargo.toml" --profile "$CARGO_PROFILE" -p "$BIN_NAME"
	else
		for t in "${TRIPLES[@]}"; do
			log "Building $BIN_NAME ($CARGO_PROFILE, $t)"
			cargo build --manifest-path "$ROOT/Cargo.toml" --profile "$CARGO_PROFILE" -p "$BIN_NAME" --target "$t"
		done
	fi
else
	log "SKIP_BUILD=1: using existing binaries"
fi

BINARIES=()
if [ "${#TRIPLES[@]}" -eq 0 ]; then
	BINARIES=("$TARGET_DIR/$PROFILE_DIR/$BIN_NAME")
else
	for t in "${TRIPLES[@]}"; do BINARIES+=("$TARGET_DIR/$t/$PROFILE_DIR/$BIN_NAME"); done
fi
for b in "${BINARIES[@]}"; do
	[ -x "$b" ] || die "binary not found: $b (build it, or unset SKIP_BUILD)"
done

# --------------------------------------------------------------------------- #
# Assemble
# --------------------------------------------------------------------------- #
log "Assembling $APP ($VERSION, build $BUILD)"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources/bin"

if [ "${#BINARIES[@]}" -eq 1 ]; then
	cp "${BINARIES[0]}" "$CONTENTS/MacOS/$BIN_NAME"
else
	lipo -create "${BINARIES[@]}" -output "$CONTENTS/MacOS/$BIN_NAME"
fi
chmod 755 "$CONTENTS/MacOS/$BIN_NAME"
ARCHS="$(lipo -archs "$CONTENTS/MacOS/$BIN_NAME")"
log "Executable architectures: $ARCHS"

SPARKLE_PUBLIC_KEY="${SPARKLE_PUBLIC_KEY:-$(tr -d '[:space:]' <"$PACKAGING/sparkle_public_key.txt")}"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@BUILD@/$BUILD/g" -e "s/@YEAR@/$(date +%Y)/g" \
	-e "s|@SPARKLE_FEED_URL@|$SPARKLE_FEED_URL|g" -e "s|@SPARKLE_PUBLIC_KEY@|$SPARKLE_PUBLIC_KEY|g" \
	"$PACKAGING/Info.plist.template" >"$CONTENTS/Info.plist"
plutil -lint -s "$CONTENTS/Info.plist" || die "rendered Info.plist is invalid"
printf 'APPL????' >"$CONTENTS/PkgInfo"

if [ -f "$ROOT/assets/icon/AppIcon.icns" ]; then
	cp "$ROOT/assets/icon/AppIcon.icns" "$CONTENTS/Resources/AppIcon.icns"
else
	warn "assets/icon/AppIcon.icns missing (run: uv run --with pillow python assets/icon/make_icon.py)"
fi

install -m 755 "$PACKAGING/bin/parquetry" "$CONTENTS/Resources/bin/parquetry"

# --------------------------------------------------------------------------- #
# DuckDB extensions
# --------------------------------------------------------------------------- #
duckdb_platform() {
	case "$1" in
		arm64) echo "osx_arm64" ;;
		x86_64) echo "osx_amd64" ;;
		*) return 1 ;;
	esac
}

# fetch_extension VERSION PLATFORM NAME -> prints the path of the cached .gz
fetch_extension() {
	local ver=$1 platform=$2 name=$3
	local dir="$EXT_CACHE/$ver/$platform"
	local gz="$dir/$name.duckdb_extension.gz"
	local url="http://extensions.duckdb.org/$ver/$platform/$name.duckdb_extension.gz"
	mkdir -p "$dir"
	if [ ! -s "$gz" ] || ! gzip -t "$gz" 2>/dev/null; then
		log "Downloading $url"
		curl -fsSL --retry 3 --connect-timeout 20 -o "$gz.part" "$url" ||
			{ rm -f "$gz.part"; return 1; }
		gzip -t "$gz.part" || { rm -f "$gz.part"; return 1; }
		mv "$gz.part" "$gz"
	fi
	echo "$gz"
}

if [ "$EXT_LAYOUT" != "none" ] && [ -n "$EXTENSIONS" ]; then
	DUCKDB_VER="$(duckdb_version)"
	EXT_ROOT="$CONTENTS/Resources/duckdb_extensions"
	log "Bundling DuckDB $DUCKDB_VER extensions ($EXTENSIONS) as '$EXT_LAYOUT'"
	for arch in $ARCHS; do
		platform="$(duckdb_platform "$arch")" || { warn "no DuckDB platform for $arch"; continue; }
		for ext in $EXTENSIONS; do
			if ! gz="$(fetch_extension "$DUCKDB_VER" "$platform" "$ext")"; then
				warn "could not download $ext for $platform ($DUCKDB_VER); the app will download it on first use"
				continue
			fi
			raw="${gz%.gz}"
			if [ ! -s "$raw" ] || [ "$gz" -nt "$raw" ]; then
				gunzip -c "$gz" >"$raw.part" && mv "$raw.part" "$raw"
			fi
			ext_archs="$(lipo -archs "$raw" 2>/dev/null || true)"
			[ "$ext_archs" = "$arch" ] || die "$raw is '$ext_archs', expected $arch"
			case "$EXT_LAYOUT" in
				raw)
					mkdir -p "$EXT_ROOT/$platform"
					cp "$raw" "$EXT_ROOT/$platform/$ext.duckdb_extension"
					;;
				repo)
					mkdir -p "$EXT_ROOT/$DUCKDB_VER/$platform"
					cp "$gz" "$EXT_ROOT/$DUCKDB_VER/$platform/$ext.duckdb_extension.gz"
					;;
				*) die "unknown DUCKDB_EXT_LAYOUT: $EXT_LAYOUT (raw | repo | none)" ;;
			esac
		done
	done
	[ -d "$EXT_ROOT" ] && printf '%s\n' "$DUCKDB_VER" >"$EXT_ROOT/VERSION"
fi

# --------------------------------------------------------------------------- #
# Quick Look extension
# --------------------------------------------------------------------------- #
if [ "$SKIP_QUICKLOOK" != "1" ]; then
	QL_SCRIPT="$ROOT/scripts/build-quicklook.sh"
	if [ "$SKIP_BUILD" != "1" ] && [ -f "$QL_SCRIPT" ]; then
		log "Building Quick Look extension"
		ql_archs=""
		for arch in $ARCHS; do ql_archs="${ql_archs:+$ql_archs }$arch"; done
		if ! ARCHS="$ql_archs" CODESIGN_IDENTITY="$IDENTITY" bash "$QL_SCRIPT"; then
			warn "scripts/build-quicklook.sh failed; continuing without a fresh Quick Look build"
		fi
	elif [ ! -f "$QL_SCRIPT" ]; then
		warn "scripts/build-quicklook.sh not found"
	fi
	if [ -d "$QL_APPEX" ]; then
		mkdir -p "$CONTENTS/PlugIns"
		ditto "$QL_APPEX" "$CONTENTS/PlugIns/$QL_NAME"
		ql_bin_archs="$(lipo -archs "$CONTENTS/PlugIns/$QL_NAME/Contents/MacOS/"* 2>/dev/null || true)"
		for arch in $ARCHS; do
			case " $ql_bin_archs " in *" $arch "*) ;; *) warn "Quick Look extension lacks $arch (has: ${ql_bin_archs:-?})" ;; esac
		done
		log "Embedded $QL_NAME"
	else
		warn "$QL_APPEX not found; bundling without Quick Look previews"
	fi
fi

# --------------------------------------------------------------------------- #
# Sparkle (automatic updates)
# --------------------------------------------------------------------------- #
# fetch_sparkle -> prints the directory holding the unpacked Sparkle release
fetch_sparkle() {
	local dir="$SPARKLE_CACHE/$SPARKLE_VERSION"
	if [ ! -d "$dir/Sparkle.framework" ]; then
		mkdir -p "$dir"
		local archive="$SPARKLE_CACHE/Sparkle-$SPARKLE_VERSION.tar.xz"
		if [ ! -f "$archive" ]; then
			local url="https://github.com/sparkle-project/Sparkle/releases/download/$SPARKLE_VERSION/Sparkle-$SPARKLE_VERSION.tar.xz"
			log "Downloading $url"
			curl -fsSL -o "$archive.part" "$url" || die "couldn't download Sparkle $SPARKLE_VERSION"
			mv "$archive.part" "$archive"
		fi
		tar -xf "$archive" -C "$dir"
	fi
	echo "$dir"
}

if [ "$SKIP_SPARKLE" = "1" ]; then
	log "SKIP_SPARKLE=1: building without automatic updates"
else
	SPARKLE_DIR="$(fetch_sparkle)"
	mkdir -p "$CONTENTS/Frameworks"
	ditto "$SPARKLE_DIR/Sparkle.framework" "$CONTENTS/Frameworks/Sparkle.framework"
	# The XPC services are only needed by sandboxed apps; Parquetry isn't sandboxed.
	rm -rf "$CONTENTS/Frameworks/Sparkle.framework/Versions/B/XPCServices" \
		"$CONTENTS/Frameworks/Sparkle.framework/XPCServices"
	log "Embedded Sparkle $SPARKLE_VERSION (feed: $SPARKLE_FEED_URL)"
fi

# --------------------------------------------------------------------------- #
# Code signing (inside-out)
# --------------------------------------------------------------------------- #
SIGN=(codesign --force --sign "$IDENTITY")
if [ "$IDENTITY" = "-" ]; then
	TS=(--timestamp=none)
	log "Signing ad-hoc (local testing only; not distributable)"
else
	TS=(--timestamp)
	log "Signing with: $IDENTITY"
fi

if [ -d "$CONTENTS/PlugIns/$QL_NAME" ]; then
	if [ -f "$QL_ENTITLEMENTS" ]; then
		"${SIGN[@]}" "${TS[@]}" --options runtime --entitlements "$QL_ENTITLEMENTS" "$CONTENTS/PlugIns/$QL_NAME"
	else
		warn "$QL_ENTITLEMENTS not found; keeping the appex's existing entitlements"
		"${SIGN[@]}" "${TS[@]}" --options runtime --preserve-metadata=entitlements "$CONTENTS/PlugIns/$QL_NAME"
	fi
fi

if [ -d "$CONTENTS/Frameworks/Sparkle.framework" ]; then
	# Sparkle's helpers first, then the framework (Sparkle's documented order).
	SPARKLE_B="$CONTENTS/Frameworks/Sparkle.framework/Versions/B"
	"${SIGN[@]}" "${TS[@]}" --options runtime "$SPARKLE_B/Autoupdate"
	"${SIGN[@]}" "${TS[@]}" --options runtime "$SPARKLE_B/Updater.app"
	"${SIGN[@]}" "${TS[@]}" --options runtime "$CONTENTS/Frameworks/Sparkle.framework"
fi

# DuckDB extensions are NOT re-signed: DuckDB verifies its own RSA signature over
# the file bytes, and codesign refuses them anyway ("main executable failed
# strict validation") because of that trailing signature block. They are sealed
# as resources by the app signature below. See packaging/README.md.

"${SIGN[@]}" "${TS[@]}" --options runtime --entitlements "$ENTITLEMENTS" "$APP"

log "Verifying signature"
codesign --verify --deep --strict --verbose=2 "$APP"

echo "$APP"
