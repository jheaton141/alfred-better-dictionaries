#!/bin/bash
# Shared helpers for search.sh and lookup.sh (source this file).

IP=127.0.0.1
PORT=6789
BASE_URL="http://$IP:$PORT"

# The bundled jq lives next to the scripts; fall back to PATH (dev/testing).
if [ -x ./jq ]; then JQ=./jq; else JQ=jq; fi

# Cheap liveness probe (also proves the HTTP port is actually serving).
function is_server_healthy() {
  curl -sf --max-time 1 "$BASE_URL/health" > /dev/null 2>&1
}

# Pre-fault each index's pages into the OS page cache by running a dummy
# query. This is what makes the very first real search feel instant, instead
# of paying the cold-disk cost while the user is waiting.
function prewarm_indexes() {
  local imported="$alfred_workflow_data/imported.json"
  [ -f "$imported" ] || return 0
  local id
  for id in $($JQ -r '.items[].arg' "$imported" 2> /dev/null); do
    curl -s --max-time 30 "$BASE_URL/indexes/$id/search" \
      -H 'Content-Type: application/json' \
      --data '{"q":"a","limit":1}' > /dev/null 2>&1
  done
}

# Start the server (if not already running) and return immediately.
# Safe to call on every keystroke.
function ensure_server_started() {
  # Health check first; failing that, a pidfile (a server that is still
  # booting is not healthy yet, but must not be started a second time).
  local pidfile="$alfred_workflow_data/server.pid"
  if is_server_healthy; then return 0; fi
  if [ -f "$pidfile" ] && kill -0 "$(cat "$pidfile")" 2> /dev/null; then
    return 0
  fi
  mkdir -p "$alfred_workflow_data"
  nohup ./alfred-dict-server \
    --db-path "$alfred_workflow_data/db" \
    --http-payload-size-limit 1000000000 \
    --http-addr "$IP:$PORT" \
    --no-analytics \
    < /dev/null > "$alfred_workflow_data/db.log" 2>&1 &
  echo $! > "$pidfile"
  prune_cache &
  # Warm the caches in the background, once the server is accepting requests.
  (
    for _ in $(seq 1 200); do
      is_server_healthy && break
      sleep 0.1
    done
    prewarm_indexes
  ) > /dev/null 2>&1 < /dev/null &
}

# Block (briefly) until the server answers. Polls fast so a warm start costs
# one iteration and a cold start costs only as long as the server takes.
function wait_for_server() {
  local i
  for i in $(seq 1 300); do
    is_server_healthy && return 0
    sleep 0.05
  done
  return 1
}

# materialize_hits <dict_id> <search-response-json>
# Definitions live in one gzip-member-per-entry pack file. Alfred (quicklook,
# the extra pane, "open") needs a real .html file, so write the handful of
# hits it is about to show -- only if not already cached.
function materialize_hits() {
  local dict_id="$1" json="$2"
  local pack="$alfred_workflow_data/$dict_id/defs.pack"
  [ -f "$pack" ] || return 0
  local path off len
  while IFS=$'\t' read -r path off len; do
    [ -f "$path" ] && continue
    mkdir -p "${path%/*}"
    {
      printf '%s' '<!DOCTYPE html><html lang="en"><head><meta http-equiv="Content-Type" content="text/html; charset=utf-8"/></head><body>'
      tail -c +$((off + 1)) "$pack" | head -c "$len" | gzip -dc
      printf '%s' '</body></html>'
    } > "$path.tmp" && mv "$path.tmp" "$path"
  done < <($JQ -r '.hits[] | select(.off != null) | [.arg, .off, .len] | @tsv' <<< "$json")
}

# Drop cached HTML not touched for a week (it is regenerated on demand).
function prune_cache() {
  [ -d "$alfred_workflow_data/cache" ] &&
    find "$alfred_workflow_data/cache" -type f -mtime +7 -delete 2> /dev/null
  return 0
}

# search_dict <dict_id> <query>  -> prints Alfred script-filter JSON
function search_dict() {
  local dict_id="$1" query="$2" body out
  local url="$BASE_URL/indexes/$dict_id/search"
  body=$($JQ -n --arg q "$query" '{q: $q, limit: 9}')

  ensure_server_started
  out=$(curl -sf --max-time 10 "$url" \
        -H 'Content-Type: application/json' --data "$body") \
    || { wait_for_server && out=$(curl -sf --max-time 10 "$url" \
        -H 'Content-Type: application/json' --data "$body"); }

  if [ -z "$out" ]; then
    echo '{"items":[{"title":"Search server is starting…","subtitle":"Keep typing; results appear in a moment","valid":false}],"rerun":0.5}'
    return 0
  fi
  materialize_hits "$dict_id" "$out"
  echo "$out" | $JQ -c '{items: (.hits | map(del(.off, .len)))}'
}

# search_all <query>  -> one merged result list across every imported dictionary
# (Meilisearch multi-search, merged by ranking score). Each subtitle is
# prefixed with the dictionary it came from.
function search_all() {
  local query="$1" body out
  local imported="$alfred_workflow_data/imported.json"
  local url="$BASE_URL/multi-search"
  body=$($JQ -n --arg q "$query" --slurpfile d "$imported" \
    '{queries: [ $d[0].items[] | {indexUid: .arg, q: $q, limit: 9, showRankingScore: true} ]}')

  ensure_server_started
  out=$(curl -sf --max-time 10 "$url" \
        -H 'Content-Type: application/json' --data "$body") \
    || { wait_for_server && out=$(curl -sf --max-time 10 "$url" \
        -H 'Content-Type: application/json' --data "$body"); }

  if [ -z "$out" ]; then
    echo '{"items":[{"title":"Search server is starting…","subtitle":"Keep typing; results appear in a moment","valid":false}],"rerun":0.5}'
    return 0
  fi

  local merged
  merged=$($JQ -c --slurpfile d "$imported" '
    ([$d[0].items[] | {key: .arg, value: .title}] | from_entries) as $names
    | [ .results[] | .indexUid as $i | .hits[] | . + {_idx: $i} ]
    | sort_by(-(._rankingScore // 0)) | .[:9]
    | map(.subtitle = "[\($names[._idx])] " + (.subtitle // ""))' <<< "$out")

  # Materialize HTML for the merged hits, one dictionary at a time.
  local idx
  for idx in $($JQ -r '.[]._idx' <<< "$merged" | sort -u); do
    materialize_hits "$idx" "$($JQ -c --arg i "$idx" '{hits: map(select(._idx == $i))}' <<< "$merged")"
  done
  $JQ -c '{items: map(del(.off, .len, ._idx, ._rankingScore))}' <<< "$merged"
}
