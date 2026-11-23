#!/usr/bin/env bash
set -euo pipefail
set -o xtrace

ARCH="${TARGET_ARCH:-$(uname -m)}"

UV_RELEASE_URL="https://github.com/astral-sh/uv/releases"
UV_VERSION="0.10.9"
UV_INTEL="$UV_RELEASE_URL/download/$UV_VERSION/uv-x86_64-apple-darwin.tar.gz"
UV_APPLE="$UV_RELEASE_URL/download/$UV_VERSION/uv-aarch64-apple-darwin.tar.gz"

# Build the Rust importer (bd). arm64 runners build natively.
if [ "$ARCH" = "x86_64" ]; then BD_TARGET=x86_64-apple-darwin; else BD_TARGET=aarch64-apple-darwin; fi
rustup target add "$BD_TARGET" || true
(cd bd && cargo build --release --target "$BD_TARGET")
cp "bd/target/$BD_TARGET/release/bd" ./bd
chmod +x bd

if [ "$ARCH" = "x86_64" ]; then
  curl -L $UV_INTEL -o uv.tar.gz
  UV_DIR="uv-x86_64-apple-darwin"
elif [ "$ARCH" = "arm64" ] || [ "$ARCH" = "aarch64" ]; then
  curl -L $UV_APPLE -o uv.tar.gz
  UV_DIR="uv-aarch64-apple-darwin"
else
  echo "Unsupported TARGET_ARCH: $ARCH" >&2
  exit 1
fi

tar -xzf uv.tar.gz
cp "$UV_DIR/uv" ./uv
chmod +x uv
rm -rf uv.tar.gz uv-*-apple-darwin
