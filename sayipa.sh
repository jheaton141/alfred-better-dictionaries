#!/bin/bash
cd "$(dirname "$0")" || exit 1
# A downloaded workflow's binary is quarantined; clear it once so Gatekeeper lets it run.
[ -e .dequarantined ] || { xattr -dr com.apple.quarantine . 2>/dev/null; touch .dequarantined; }
exec ./bd say-ipa "$@"
