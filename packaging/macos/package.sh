#!/usr/bin/env bash
# Build, sign and (optionally) notarize the macOS release artifacts for MetricCraft:
#
#   $DIST/metriccraft-<version>-macos-<arch>.dmg    MetricCraft.app on a drag-to-Applications DMG
#   $DIST/metriccraft-cli-<version>-macos-<arch>.zip   the headless CLI with licence files
#
# Usage: packaging/macos/package.sh [--arch universal|aarch64|x86_64] [--skip-build | --build-only] [--no-run]
#
# Options:
#   --arch         Target architecture(s): universal (default), aarch64, or x86_64
#   --skip-build   Package binaries already present in $CARGO_TARGET_DIR/<target>/release
#   --build-only   Compile targets and exit before signing/packaging
#   --no-run       Skip the final CLI smoke check (for isolated signing jobs)
#
# Signing (env):
#   MACOS_SIGN_IDENTITY   codesign identity (name or SHA-1). Default "-" = ad-hoc signing.
#   MACOS_KEYCHAIN        keychain holding MACOS_SIGN_IDENTITY (optional)
# Notarization (env; requires a real identity plus all three):
#   APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/macos"

ARCH=universal
SKIP_BUILD=0
BUILD_ONLY=0
NO_RUN=0
while [ $# -gt 0 ]; do
  case "$1" in
    --arch)
      if [ $# -lt 2 ]; then echo "missing value for --arch" >&2; exit 2; fi
      ARCH="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --build-only) BUILD_ONLY=1; shift ;;
    --no-run) NO_RUN=1; shift ;;
    -h | --help) sed -n '2,19p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$ARCH" in
  universal) TARGETS=(aarch64-apple-darwin x86_64-apple-darwin); WANT=(arm64 x86_64) ;;
  aarch64) TARGETS=(aarch64-apple-darwin); WANT=(arm64) ;;
  x86_64) TARGETS=(x86_64-apple-darwin); WANT=(x86_64) ;;
  *) echo "unknown --arch $ARCH" >&2; exit 2 ;;
esac
if [ "$SKIP_BUILD" = 1 ] && [ "$BUILD_ONLY" = 1 ]; then
  echo "--skip-build and --build-only can't be used together" >&2
  exit 2
fi

export MACOSX_DEPLOYMENT_TARGET=11.0
IDENTITY="${MACOS_SIGN_IDENTITY:--}"
SHORT_VERSION="${VERSION%%[-+]*}"
case "$SHORT_VERSION" in
  ''|*[!0-9.]*)
    echo "error: version '$VERSION' does not start with a numeric CFBundleShortVersionString prefix" >&2
    exit 2
    ;;
esac

# Validate version, bundle identifier, deployment target, and build SHA before Info.plist substitution.
for value in "$APP_ID" "$VERSION" "$SHORT_VERSION" "$MACOSX_DEPLOYMENT_TARGET" "${METRICCRAFT_BUILD_SHA:-unknown}"; do
  case "$value" in
    *[!0-9A-Za-z._+-]*)
      echo "error: Info.plist substitution values may contain only letters, digits, '.', '_', '+' and '-': $value" >&2
      exit 2
      ;;
  esac
done

escape_sed_repl() {
  printf '%s' "$1" | sed -e 's/[\\/&]/\\&/g'
}

# Keep in sync with LSMinimumSystemVersion in Info.plist.in.
WORK="$CARGO_TARGET_DIR/macos-package"
APP="$WORK/MetricCraft.app"
DMG="$DIST/metriccraft-$VERSION-macos-$ARCH.dmg"
CLI_ZIP="$DIST/metriccraft-cli-$VERSION-macos-$ARCH.zip"

NOTARIZE=0
SIGNING=adhoc
if [ "$IDENTITY" = "-" ]; then
  warn "macOS: ad-hoc signing (no MACOS_SIGN_IDENTITY); artifacts are not notarized"
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  NOTARIZE=1
  SIGNING=notarized
else
  SIGNING=signed
  warn "macOS: APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID incomplete; signed but not notarized"
fi

echo "==> MetricCraft $VERSION for macOS ($ARCH), identity: $IDENTITY, notarize: $NOTARIZE"

# ---- build -------------------------------------------------------------------------------------
if [ "$SKIP_BUILD" = 0 ]; then
  for t in "${TARGETS[@]}"; do
    if ! (cd "$ROOT" && cargo build --release --locked -p metriccraft -p metriccraft-cli --target "$t"); then
      echo "error: cargo build failed for $t; aborting packaging" >&2
      exit 1
    fi
  done
fi

if [ "$BUILD_ONLY" = 1 ]; then
  echo "==> built; signing and packaging come next"
  exit 0
fi

rm -rf "$WORK"
mkdir -p "$WORK/bin"
for bin in metriccraft metriccraft-cli; do
  inputs=()
  for t in "${TARGETS[@]}"; do inputs+=("$CARGO_TARGET_DIR/$t/release/$bin"); done
  lipo -create -output "$WORK/bin/$bin" "${inputs[@]}"
  lipo -info "$WORK/bin/$bin"
  for arch in "${WANT[@]}"; do lipo "$WORK/bin/$bin" -verify_arch "$arch"; done
done

# Generate third-party dependency licence texts once for both packages.
if ! (cd "$ROOT" && cargo about generate --locked about.hbs -o "$WORK/THIRD-PARTY-LICENSES.html"); then
  echo "error: cargo-about failed; install it with 'cargo install cargo-about --locked --version 0.9.2'" >&2
  exit 1
fi

# ---- signing helpers ---------------------------------------------------------------------------
sign() {
  local ts=(--timestamp)
  [ "$IDENTITY" = "-" ] && ts=(--timestamp=none)
  local kc=()
  [ -n "${MACOS_KEYCHAIN:-}" ] && kc=(--keychain "$MACOS_KEYCHAIN")
  codesign --force --sign "$IDENTITY" ${kc[@]+"${kc[@]}"} "${ts[@]}" "$@"
}

notarize() {
  local file="$1" out id status
  echo "==> notarizing $(basename "$file") (this can take a few minutes)"
  out="$(xcrun notarytool submit "$file" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" \
    --team-id "$APPLE_TEAM_ID" --wait --timeout 1h --output-format json)" || true
  echo "$out"
  id="$(printf '%s' "$out" | plutil -extract id raw -o - - 2>/dev/null || true)"
  status="$(printf '%s' "$out" | plutil -extract status raw -o - - 2>/dev/null || true)"
  if [ "$status" != "Accepted" ]; then
    if [ -n "$id" ]; then
      xcrun notarytool log "$id" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" || true
    fi
    echo "error: notarization of $(basename "$file") failed (status: ${status:-unknown})" >&2
    exit 1
  fi
}

# hdiutil attaches a device while working, which can briefly fail with "Resource busy" in CI.
hdiutil_retry() {
  local what="$1" attempt
  for attempt in 1 2 3 4 5; do
    if hdiutil "$@"; then return 0; fi
    [ "$attempt" = 5 ] && { echo "hdiutil $what failed 5 times" >&2; return 1; }
    echo "hdiutil $what failed (attempt $attempt of 5); retrying in $((attempt * 5))s" >&2
    sleep $((attempt * 5))
  done
}

# ---- MetricCraft.app ----------------------------------------------------------------------------
echo "==> assembling $APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/Licenses"
cp "$WORK/bin/metriccraft" "$APP/Contents/MacOS/MetricCraft"
cp "$WORK/bin/metriccraft-cli" "$APP/Contents/MacOS/metriccraft-cli"
copy_docs "$APP/Contents/Resources/Licenses"
cp "$WORK/THIRD-PARTY-LICENSES.html" "$APP/Contents/Resources/Licenses/"

escaped_app_id="$(escape_sed_repl "$APP_ID")"
escaped_version="$(escape_sed_repl "$VERSION")"
escaped_short_version="$(escape_sed_repl "$SHORT_VERSION")"
escaped_build_sha="$(escape_sed_repl "${METRICCRAFT_BUILD_SHA:-unknown}")"
escaped_min_macos="$(escape_sed_repl "$MACOSX_DEPLOYMENT_TARGET")"

sed -e "s/@APP_ID@/$escaped_app_id/g" -e "s/@VERSION@/$escaped_version/g" \
  -e "s/@SHORT_VERSION@/$escaped_short_version/g" -e "s/@BUILD_SHA@/$escaped_build_sha/g" \
  -e "s/@MIN_MACOS@/$escaped_min_macos/g" \
  "$HERE/Info.plist.in" >"$APP/Contents/Info.plist"

ICON_SRC=""
for candidate in "$ROOT/assets/app-icon/metriccraft.icns" "$ROOT/assets/app-icon/icon.icns"; do
  if [ -f "$candidate" ]; then ICON_SRC="$candidate"; break; fi
done
if [ -n "$ICON_SRC" ]; then
  cp "$ICON_SRC" "$APP/Contents/Resources/MetricCraft.icns"
  /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string MetricCraft" "$APP/Contents/Info.plist"
fi

plutil -lint "$APP/Contents/Info.plist"
printf 'APPL????' >"$APP/Contents/PkgInfo"

# Sign inside-out: nested executables first, then the bundle itself.
sign --options runtime --identifier "$APP_ID.cli" "$APP/Contents/MacOS/metriccraft-cli"
sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP/Contents/MacOS/MetricCraft"
sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP"
codesign --verify --strict --deep --verbose=2 "$APP"

if [ "$NOTARIZE" = 1 ]; then
  ditto -c -k --keepParent "$APP" "$WORK/MetricCraft-notarize.zip"
  notarize "$WORK/MetricCraft-notarize.zip"
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
  spctl --assess --type execute -vvv "$APP"
fi

# ---- DMG ---------------------------------------------------------------------------------------
echo "==> building $DMG"
STAGE="$WORK/dmg"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/MetricCraft.app"
ln -s /Applications "$STAGE/Applications"
if [ -f "$HERE/dmg/background.tiff" ] && [ -f "$HERE/dmg/dmg-layout.DS_Store" ]; then
  mkdir -p "$STAGE/.background"
  cp "$HERE/dmg/background.tiff" "$STAGE/.background/background.tiff"
  cp "$HERE/dmg/dmg-layout.DS_Store" "$STAGE/.DS_Store"
fi
rm -f "$DMG"
hdiutil_retry create -volname "MetricCraft" -srcfolder "$STAGE" -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$DMG"
sign "$DMG"
codesign --verify --strict --verbose=2 "$DMG"
if [ "$NOTARIZE" = 1 ]; then
  notarize "$DMG"
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
  spctl --assess --type open --context context:primary-signature -vvv "$DMG"
fi

# ---- CLI ---------------------------------------------------------------------------------------
echo "==> building $CLI_ZIP"
CLI_DIR="$WORK/metriccraft-cli-$VERSION-macos-$ARCH"
mkdir -p "$CLI_DIR"
cp "$WORK/bin/metriccraft-cli" "$CLI_DIR/"
copy_docs "$CLI_DIR"
cp "$WORK/THIRD-PARTY-LICENSES.html" "$CLI_DIR/"
sign --options runtime --identifier "$APP_ID.cli" "$CLI_DIR/metriccraft-cli"
codesign --verify --strict --verbose=2 "$CLI_DIR/metriccraft-cli"
rm -f "$CLI_ZIP"
ditto -c -k --norsrc --noextattr --keepParent "$CLI_DIR" "$CLI_ZIP"
if [ "$NOTARIZE" = 1 ]; then notarize "$CLI_ZIP"; fi

if [ "$NO_RUN" = 1 ]; then
  echo "==> skipping metriccraft-cli smoke test (--no-run)"
else
  "$CLI_DIR/metriccraft-cli" --version
fi
if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "signing=$SIGNING" >>"$GITHUB_OUTPUT"; fi
echo "==> done (signing: $SIGNING)"
ls -lh "$DMG" "$CLI_ZIP"
