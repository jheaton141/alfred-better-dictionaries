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
  if pgrep -x alfred-dict-server > /dev/null; then
    return 0
  fi
  mkdir -p "$alfred_workflow_data"
  nohup ./alfred-dict-server \
    --db-path "$alfred_workflow_data/db" \
    --http-payload-size-limit 1000000000 \
    --http-addr "$IP:$PORT" \
    --no-analytics \
    < /dev/null > "$alfred_workflow_data/db.log" 2>&1 &
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
  echo "$out" | $JQ -c '{items: .hits}'
}
