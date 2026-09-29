#!/bin/bash
# Build the Parquetry Quick Look preview extension (ParquetryQuickLook.appex).
#
#   scripts/build-quicklook.sh
#
# Environment:
#   CARGO_TARGET_DIR   Cargo target dir (default: <repo>/target). The appex is written to
#                      $CARGO_TARGET_DIR/quicklook/ParquetryQuickLook.appex
#   ARCHS              Architectures to build, e.g. "arm64 x86_64" for a universal binary
#                      (default: host architecture).
#   CODESIGN_IDENTITY  Signing identity (default: "-", ad-hoc).
#   MACOSX_DEPLOYMENT_TARGET  Minimum macOS (default: 13.0).
#
# Only needs the Xcode Command Line Tools (swiftc, clang, codesign) and a Rust toolchain.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$ROOT/$TARGET_DIR" ;; esac
export CARGO_TARGET_DIR="$TARGET_DIR"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"

MODULE_NAME="ParquetryQuickLook"
QL_SRC="$ROOT/quicklook"
OUT_DIR="$TARGET_DIR/quicklook"
APPEX="$OUT_DIR/$MODULE_NAME.appex"
BUILD_DIR="$OUT_DIR/build"
IDENTITY="${CODESIGN_IDENTITY:--}"

# Make cargo/rustup available when the script is run from a non-login shell.
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

die() { echo "error: $*" >&2; exit 1; }

command -v cargo >/dev/null 2>&1 || die "cargo not found (install Rust from https://rustup.rs)"
command -v xcrun >/dev/null 2>&1 || die "xcrun not found (install the Xcode Command Line Tools: xcode-select --install)"

SDK="$(xcrun --sdk macosx --show-sdk-path)"
# SDKROOT must be exported: without it the linker can't determine the SDK version and stamps
# the binary with sdk == deployment target (which enables old-SDK compatibility behaviours).
export SDKROOT="$SDK"
SWIFTC=(xcrun --sdk macosx swiftc)

HOST_ARCH="$(uname -m)"
read -r -a ARCH_LIST <<< "${ARCHS:-$HOST_ARCH}"
[ "${#ARCH_LIST[@]}" -ge 1 ] || die "ARCHS is empty"
[ "${#ARCH_LIST[@]}" -le 2 ] || die "ARCHS supports at most two entries (arm64 x86_64), got: ${ARCHS}"

rust_triple() {
  case "$1" in
    arm64|aarch64) echo "aarch64-apple-darwin" ;;
    x86_64) echo "x86_64-apple-darwin" ;;
    *) die "unsupported architecture: $1 (use arm64 or x86_64)" ;;
  esac
}
swift_arch() {
  case "$1" in
    arm64|aarch64) echo "arm64" ;;
    x86_64) echo "x86_64" ;;
  esac
}

# Check Rust targets up front so a universal build fails fast with a clear message.
if command -v rustup >/dev/null 2>&1; then
  INSTALLED_TARGETS="$(rustup target list --installed)"
  for arch in "${ARCH_LIST[@]}"; do
    triple="$(rust_triple "$arch")"
    if ! grep -qx "$triple" <<< "$INSTALLED_TARGETS"; then
      die "Rust target $triple is not installed; run: rustup target add $triple (or build only the host arch by unsetting ARCHS)"
    fi
  done
fi

rm -rf "$APPEX" "$BUILD_DIR"
mkdir -p "$BUILD_DIR" "$APPEX/Contents/MacOS"

SLICES=()
for arch in "${ARCH_LIST[@]}"; do
  triple="$(rust_triple "$arch")"
  sarch="$(swift_arch "$arch")"

  echo "==> Building Rust static library ($triple, release)"
  cargo build --release -p parquetry-quicklook --target "$triple" \
    --manifest-path "$ROOT/Cargo.toml"
  LIB_DIR="$TARGET_DIR/$triple/release"
  [ -f "$LIB_DIR/libparquetry_quicklook.a" ] || die "missing $LIB_DIR/libparquetry_quicklook.a"

  # Native libs required by the Rust staticlib (`--print native-static-libs`):
  # CoreFoundation (iana-time-zone, used by arrow's timestamp formatting), libiconv, libSystem.
  echo "==> Compiling Swift extension ($sarch)"
  slice="$BUILD_DIR/$MODULE_NAME-$sarch"
  "${SWIFTC[@]}" \
    -sdk "$SDK" \
    -target "$sarch-apple-macos$MACOSX_DEPLOYMENT_TARGET" \
    -O -whole-module-optimization \
    -parse-as-library \
    -application-extension \
    -module-name "$MODULE_NAME" \
    -module-cache-path "$BUILD_DIR/module-cache" \
    -I "$QL_SRC/module" \
    "$QL_SRC/PreviewProvider.swift" \
    -L "$LIB_DIR" -lparquetry_quicklook \
    -framework QuickLookUI -framework Foundation -framework UniformTypeIdentifiers \
    -framework CoreFoundation -liconv \
    -Xlinker -e -Xlinker _NSExtensionMain \
    -Xlinker -dead_strip \
    -o "$slice"
  SLICES+=("$slice")
done

BIN="$APPEX/Contents/MacOS/$MODULE_NAME"
if [ "${#SLICES[@]}" -eq 1 ]; then
  cp "${SLICES[0]}" "$BIN"
else
  echo "==> Creating universal binary"
  lipo -create "${SLICES[@]}" -output "$BIN"
fi
strip -x "$BIN"

cp "$QL_SRC/Info.plist" "$APPEX/Contents/Info.plist"
printf 'XPC!????' > "$APPEX/Contents/PkgInfo"

echo "==> Signing ($IDENTITY)"
SIGN_ARGS=(--force --sign "$IDENTITY" --entitlements "$QL_SRC/QuickLook.entitlements" --options runtime)
if [ "$IDENTITY" = "-" ]; then
  SIGN_ARGS+=(--timestamp=none)
else
  SIGN_ARGS+=(--timestamp)
fi
codesign "${SIGN_ARGS[@]}" "$APPEX"
codesign --verify --strict "$APPEX"

rm -rf "$BUILD_DIR"
echo "$APPEX"
