#!/bin/sh
# Build Ambition.app: the ambition binary in a small app bundle, signed with
# a code-signing certificate made on this Mac, so macOS keeps the app's
# Accessibility and Screen Recording grants from one build to the next.
#
#   macos/build-app.sh [folder]     puts Ambition.app in folder (~/Applications)
#
# The first run makes the certificate, "Ambition (this Mac)", in a keychain of
# its own (~/Library/Keychains/ambition-signing.keychain-db, its password in
# ~/Library/Application Support/Ambition/), and asks macOS to trust it for
# code signing: macOS asks for your password once. If that prompt is
# cancelled or times out, run it again: it asks until the certificate is
# trusted. AMBITION_SIGN=- signs ad hoc instead (grants then last one build).
set -eu
cd "$(dirname "$0")/.."
dest="${1:-$HOME/Applications}"
target="${CARGO_TARGET_DIR:-target}"
name="Ambition (this Mac)"
kc="$HOME/Library/Keychains/ambition-signing.keychain-db"
pwfile="$HOME/Library/Application Support/Ambition/signing-keychain"

cargo build --release
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app="$dest/Ambition.app"
mkdir -p "$app/Contents/MacOS"
cp "$target/release/ambition" "$app/Contents/MacOS/ambition"
sed "s/@VERSION@/$version/g" macos/Info.plist > "$app/Contents/Info.plist"

if [ "${AMBITION_SIGN:-}" = "-" ]; then
  codesign --force --sign - --identifier io.github.mannyfluss.ambition "$app"
  echo "Built $app (signed ad hoc)."
  exit 0
fi
# The keychain and its certificate, made once. A keychain left by a run that
# stopped before its certificate went in is made again.
make_keychain() {
  mkdir -p "$(dirname "$pwfile")"
  (umask 077; /usr/bin/openssl rand -hex 24 > "$pwfile")
  pw=$(cat "$pwfile")
  tmp=$(mktemp -d)
  security create-keychain -p "$pw" "$kc"
  security set-keychain-settings "$kc"
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=$name" \
    -keyout "$tmp/key.pem" -out "$tmp/cert.pem" \
    -addext "keyUsage=critical,digitalSignature" -addext "extendedKeyUsage=critical,codeSigning" \
    -addext "basicConstraints=critical,CA:false" 2>/dev/null
  /usr/bin/openssl pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" -out "$tmp/id.p12" -passout pass:ambition
  security import "$tmp/id.p12" -k "$kc" -P ambition -T /usr/bin/codesign >/dev/null
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$pw" "$kc" >/dev/null
  rm -rf "$tmp"
}
if [ -f "$kc" ] && [ ! -f "$pwfile" ]; then
  echo "$kc is here but its password file ($pwfile) is not. Remove the keychain (security delete-keychain \"$kc\") and run this again." >&2
  exit 1
fi
[ -f "$kc" ] || make_keychain
security unlock-keychain -p "$(cat "$pwfile")" "$kc"
if ! security find-certificate -c "$name" "$kc" >/dev/null 2>&1; then
  security delete-keychain "$kc"
  make_keychain
  security unlock-keychain -p "$(cat "$pwfile")" "$kc"
fi

# Trusted for code signing: asked whenever it is not yet, so a run after a
# cancelled or timed-out password prompt asks again.
if ! security find-identity -v -p codesigning "$kc" | grep -q "\"$name\""; then
  tmp=$(mktemp -d)
  security find-certificate -c "$name" -p "$kc" > "$tmp/cert.pem"
  echo "macOS will ask for your password once, to trust \"$name\" for signing code."
  if ! security add-trusted-cert -r trustRoot -p codeSign -k "$kc" "$tmp/cert.pem"; then
    rm -rf "$tmp"
    echo "\"$name\" is not trusted yet: the password prompt was cancelled or timed out. Run macos/build-app.sh again." >&2
    exit 1
  fi
  rm -rf "$tmp"
  if ! security find-identity -v -p codesigning "$kc" | grep -q "\"$name\""; then
    echo "\"$name\" is still not valid for signing code. Run macos/build-app.sh again." >&2
    exit 1
  fi
fi
codesign --force --keychain "$kc" --sign "$name" --identifier io.github.mannyfluss.ambition "$app"
echo "Built $app, signed as \"$name\"."
