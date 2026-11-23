#!/bin/bash
# Single-stage `lookup` script filter.
#
# Previously `lookup` chained into a second script filter through an
# Argument & Variables utility + router, and the Alfred window closed on that
# hand-off. Now the whole flow lives in one script filter:
#
#   "lookup "                -> list of imported dictionaries
#   "lookup Oxf"             -> dictionaries matching "Oxf"
#   "lookup Oxford<space>"   -> (via autocomplete) searches that dictionary
#   "lookup Oxford arm"      -> results for "arm" in that dictionary

query="$1"

source ./lib.sh

imported="$alfred_workflow_data/imported.json"
if [ ! -s "$imported" ]; then
  echo '{"items":[{"title":"No dictionaries imported yet","subtitle":"Type .dict-import to import one","valid":false}]}'
  exit 0
fi

# Start (and pre-warm) the server as soon as the user types `lookup`, so it
# is ready by the time they have picked a dictionary and typed a word.
ensure_server_started

# One jq pass decides the mode.
decision=$($JQ -c --arg q "$query" '
  .items as $dicts
  | ($q | ascii_downcase) as $lq
  | ( [ $dicts[]
        | select(. as $d | $lq | startswith(($d.title | ascii_downcase) + " ")) ]
      | sort_by(.title | length) | last ) as $hit
  | if $hit != null then
      {mode: "search", id: $hit.arg, title: $hit.title,
       q: ($q[($hit.title | length) + 1:] | ltrimstr(" "))}
    elif ($dicts | length) == 1 and ($q | length) > 0 then
      {mode: "search", id: $dicts[0].arg, title: $dicts[0].title, q: $q}
    else
      {mode: "list",
       items: [ $dicts[]
                | select(. as $d | ($lq == "") or (($d.title | ascii_downcase) | contains($lq)))
                | {title: .title,
                   subtitle: "Search \(.title)",
                   autocomplete: (.title + " "),
                   valid: false} ]}
    end' "$imported")

mode=$($JQ -r '.mode' <<< "$decision")

if [ "$mode" = "list" ]; then
  $JQ -c '{items: .items}' <<< "$decision"
  exit 0
fi

dict_id=$($JQ -r '.id' <<< "$decision")
dict_title=$($JQ -r '.title' <<< "$decision")
word=$($JQ -r '.q' <<< "$decision")

if [ -z "$word" ]; then
  $JQ -nc --arg t "$dict_title" \
    '{items: [{title: "Search \($t)…", subtitle: "Type a word", valid: false}]}'
  exit 0
fi

open -g ./AlfredExtraPane.app
search_dict "$dict_id" "$word"
