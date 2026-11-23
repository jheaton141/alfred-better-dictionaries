#!/bin/bash
# Per-dictionary search (the keyword/hotkey objects created on import call
# this as: ./search.sh "<query>" "<dictionary id>").
exec ./bd search "$2" "$1"
