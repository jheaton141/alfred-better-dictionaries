#!/bin/bash
# `lookup` script filter. All the logic (dictionary picker, combined search,
# scoping to one dictionary) lives in the native `bd` binary.
exec ./bd lookup "$1"
