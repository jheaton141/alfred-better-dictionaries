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
# The pane only looks for Alfred 4/5 in /Applications; point its Alfred 4 slot at Alfred 6 (same
# length) and re-sign, or it reports "installed Alfred version is v0.0.0" on Alfred 6.
PANE=AlfredExtraPane.app/Contents/MacOS/AlfredExtraPane
if grep -aq "/Applications/Alfred 4.app" "$PANE"; then
  perl -0777 -pi -e 's#/Applications/Alfred 4\.app#/Applications/Alfred 6.app#g' "$PANE"
  codesign --force --sign - "$PANE"
fi
rm -f ./*.alfredworkflow
NAME=$(bd/target/release/bd package . wfbuild)
(cd wfbuild && zip -qr "../$NAME.arm64.alfredworkflow" .)
echo "Built: $NAME.arm64.alfredworkflow"
