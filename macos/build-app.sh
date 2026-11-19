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
# code signing: macOS asks for your password once. AMBITION_SIGN=- signs ad
# hoc instead (grants then last one build).
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
if [ ! -f "$kc" ]; then
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
  echo "macOS will ask for your password once, to trust \"$name\" for signing code."
  security add-trusted-cert -r trustRoot -p codeSign -k "$kc" "$tmp/cert.pem"
  rm -rf "$tmp"
fi
security unlock-keychain -p "$(cat "$pwfile")" "$kc"
codesign --force --keychain "$kc" --sign "$name" --identifier io.github.mannyfluss.ambition "$app"
echo "Built $app, signed as \"$name\"."
