#!/usr/bin/env bash
# Upgrade the installed workflow in place (keeps dictionaries, keywords, hotkeys,
# no re-import) and build the search index for already-imported dictionaries.
#   ./upgrade.sh [workflow folder] [--no-migrate] [--keep-backup]
set -euo pipefail
cd "$(dirname "$0")"
(cd bd && cargo build --release)
exec bd/target/release/bd upgrade . "$@"
