#!/bin/bash

# Make a copy of the original 'factory-version' workflow
if [ ! -f info.plist.orig ]; then
  cp info.plist info.plist.orig
fi

set -uo pipefail

chmod +x ./uv
chmod +x ./bd

function deQuarantine {
  # If the extended attribute doesn't exist, don't bother
  # printing the error "No such xattr...", as those messages
  # can be mistaken as "errors".
  xattr -d com.apple.quarantine "$1" 2> /dev/null || true
}

deQuarantine ./AlfredExtraPane.app
deQuarantine ./uv
deQuarantine ./bd

mkdir -p "${alfred_workflow_data:-/dev/null}" 2> /dev/null || true

defaults write com.runningwithcrayons.Alfred experimental.presssecretary -bool YES
open ./AlfredExtraPane.app

# Stop a search server left over from versions before 0.5 (no longer used)
killall -q alfred-dict-server 2> /dev/null || true
