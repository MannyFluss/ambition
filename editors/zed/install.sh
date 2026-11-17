#!/bin/sh
# Build the Zed extension and link it into Zed as a dev extension.
# Run it again after changing src/lib.rs or extension.toml.
set -e
cd "$(dirname "$0")"
rustup target add wasm32-wasip2 >/dev/null 2>&1 || true
cargo build --release --target wasm32-wasip2
cp target/wasm32-wasip2/release/ambition_zed.wasm extension.wasm
case "$(uname -s)" in
  Darwin) Z="$HOME/Library/Application Support/Zed/extensions/installed" ;;
  *) Z="${XDG_DATA_HOME:-$HOME/.local/share}/zed/extensions/installed" ;;
esac
mkdir -p "$Z"
ln -sfn "$(pwd)" "$Z/ambition"
echo "installed: $Z/ambition -> $(pwd)"
echo "Zed loads it on its next start, or with the command 'zed: reload extensions'."
