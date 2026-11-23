#!/bin/bash
# Per-dictionary search (the keyword/hotkey objects created on import call
# this as: ./search.sh "<query>" "<dictionary id>").
# A downloaded workflow's binary is quarantined; clear it once so Gatekeeper lets it run.
[ -e .dequarantined ] || { xattr -dr com.apple.quarantine . 2>/dev/null; touch .dequarantined; }
exec ./bd search "$2" "$1"
