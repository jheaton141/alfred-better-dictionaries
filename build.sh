#!/usr/bin/env bash
# Build the Apple-silicon workflow: ./build.sh  ->  <name>.arm64.alfredworkflow
# Needs only Rust (rustup) and, for the extra pane, AlfredExtraPane.app in this folder.
set -euo pipefail
cd "$(dirname "$0")"
(cd bd && cargo build --release)
if [ ! -d AlfredExtraPane.app ]; then
  curl -fL https://github.com/mr-pennyworth/alfred-extra-pane/releases/latest/download/AlfredExtraPane.app.zip -o /tmp/aep.zip
  unzip -q /tmp/aep.zip
fi
rm -f ./*.alfredworkflow
NAME=$(bd/target/release/bd package . wfbuild)
(cd wfbuild && zip -qr "../$NAME.arm64.alfredworkflow" .)
echo "Built: $NAME.arm64.alfredworkflow"
