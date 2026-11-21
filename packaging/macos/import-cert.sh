#!/usr/bin/env bash
# CI only: import the Developer ID certificate into a temporary keychain for codesign.
#
# Inputs (env): APPLE_CERTIFICATE (base64 .p12), APPLE_CERTIFICATE_PASSWORD, KEYCHAIN_PASSWORD
# (optional; random if unset), RUNNER_TEMP (or TMPDIR).
# Outputs: MACOS_SIGN_IDENTITY (SHA-1) and MACOS_KEYCHAIN, appended to $GITHUB_ENV in Actions.
#
# Missing certificate: fails in GitHub Actions unless MACOS_ALLOW_ADHOC=1; locally warns and exits 0 so package.sh falls back to ad-hoc signing.
set -euo pipefail
umask 077
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

if [ -z "${APPLE_CERTIFICATE:-}" ]; then
  if [ -n "${GITHUB_ACTIONS:-}" ] && [ "${MACOS_ALLOW_ADHOC:-0}" != 1 ]; then
    echo "error: APPLE_CERTIFICATE is not set; refusing to build an unsigned release" >&2
    exit 1
  fi
  warn "APPLE_CERTIFICATE is not set: macOS builds will be ad-hoc signed and not notarized"
  exit 0
fi

TMP="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
KEYCHAIN="$TMP/metriccraft-signing-$$.keychain-db"
CERT="$TMP/metriccraft-signing-$$.p12"
KC_PASS="${KEYCHAIN_PASSWORD:-$(openssl rand -hex 24)}"
KEEP_KEYCHAIN=0
KEYCHAIN_LIST_MODIFIED=0
existing_keychains=()

cleanup() {
  rm -f "$CERT"
  if [ "$KEEP_KEYCHAIN" != 1 ]; then
    if [ "$KEYCHAIN_LIST_MODIFIED" = 1 ]; then
      security list-keychains -d user -s ${existing_keychains[@]+"${existing_keychains[@]}"} 2>/dev/null || true
    fi
    security delete-keychain "$KEYCHAIN" 2>/dev/null || rm -f "$KEYCHAIN"
  fi
}
trap cleanup EXIT

printf '%s' "$APPLE_CERTIFICATE" | base64 -D >"$CERT"
security delete-keychain "$KEYCHAIN" 2>/dev/null || rm -f "$KEYCHAIN"
security create-keychain -p "$KC_PASS" "$KEYCHAIN"
security set-keychain-settings -lut 21600 "$KEYCHAIN"
security unlock-keychain -p "$KC_PASS" "$KEYCHAIN"
security import "$CERT" -P "${APPLE_CERTIFICATE_PASSWORD:-}" -T /usr/bin/codesign -T /usr/bin/security -t cert -f pkcs12 -k "$KEYCHAIN"
rm -f "$CERT"
# Allow codesign to access the private key without a GUI prompt.
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KC_PASS" "$KEYCHAIN" >/dev/null
# Put it on the search list alongside existing user keychains.
if ! keychain_list="$(security list-keychains -d user)"; then
  echo "error: failed to read existing user keychains" >&2
  exit 1
fi
existing_keychains=()
while IFS= read -r line; do
  [ -n "$line" ] && existing_keychains+=("$line")
done < <(printf '%s\n' "$keychain_list" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"//' -e 's/"$//')
KEYCHAIN_LIST_MODIFIED=1
security list-keychains -d user -s "$KEYCHAIN" ${existing_keychains[@]+"${existing_keychains[@]}"}

IDENTITY="$(security find-identity -v -p codesigning "$KEYCHAIN" | awk '/Developer ID Application/ { print $2; exit }')"
if [ -z "$IDENTITY" ]; then
  security find-identity -v -p codesigning "$KEYCHAIN" >&2
  echo "error: no 'Developer ID Application' identity in APPLE_CERTIFICATE" >&2
  exit 1
fi
echo "Imported signing identity $IDENTITY"
if [ -n "${GITHUB_ENV:-}" ]; then
  { echo "MACOS_SIGN_IDENTITY=$IDENTITY"; echo "MACOS_KEYCHAIN=$KEYCHAIN"; } >>"$GITHUB_ENV"
  KEEP_KEYCHAIN=1
else
  echo "MACOS_SIGN_IDENTITY=$IDENTITY"
  echo "MACOS_KEYCHAIN=$KEYCHAIN"
  echo "note: temporary keychain will be removed on exit" >&2
fi
