#!/bin/bash
# `lookup` script filter. All the logic (dictionary picker, combined search,
# scoping to one dictionary) lives in the native `bd` binary.
# A downloaded workflow's binary is quarantined; clear it once so Gatekeeper lets it run.
[ -e .dequarantined ] || { xattr -dr com.apple.quarantine . 2>/dev/null; touch .dequarantined; }
exec ./bd lookup "$1"
