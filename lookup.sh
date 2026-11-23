#!/bin/bash
# `lookup` script filter. All the logic (first-run import, combined search)
# lives in the native `bd` binary.
# A downloaded workflow's binary is quarantined; clear it once so Gatekeeper lets it run.
[ -e .dequarantined ] || { xattr -dr com.apple.quarantine . 2>/dev/null; touch .dequarantined; }
# First run: set up the extra pane and the factory-state backup (the dictionaries import themselves).
[ -e .setup-done ] || { ./setup.sh >/dev/null 2>&1 && touch .setup-done; }
exec ./bd lookup "$1"
