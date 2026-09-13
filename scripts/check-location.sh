#!/usr/bin/env bash
# Location, remembered state, and radar lock (DESIGN.md, location):
# resolution order, state writes, the picker, a centre outside a locked
# radar, restore after close, and isolation from the machine's files.
set -euo pipefail
cd "$(dirname "$0")/.."
check_dir="$PWD/target/check-location"
mkdir -p "$check_dir"
# Gothenburg as Omarchy's weather panel writes it; nearest SMHI radar is Vara.
printf '{\n  "name": "Gothenburg",\n  "latitude": 57.70716,\n  "longitude": 11.96679\n}\n' > "$check_dir/weather.json"
: > "$check_dir/none.toml"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
fail() { printf '%s\n' "$@" >&2; [[ -f $check_dir/log ]] && cat "$check_dir/log" >&2; exit 1; }
expect() { [[ "$3" == "$2" ]] || fail "$1" "Expected: $2" "Actual:   $3"; }
start() { # config, location, state
  local config=$1 location=$2 state=$3
  OMASTORM_CONFIG="$config" OMASTORM_LOCATION="$location" OMASTORM_STATE="$state" \
    bash run.sh > "$check_dir/log" 2>&1 &
  pid=$!
  trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT
  for _ in {1..100}; do quickshell ipc --pid "$pid" call keys status > /dev/null 2>&1 && return; sleep .1; done
  fail "The window never answered"
}
stop() { kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; trap - EXIT; pid=; }
call() { quickshell ipc --pid "$pid" call keys "$@"; }
field() { call field "$1"; }
until_field() {
  for _ in {1..100}; do [[ $(field "$1") == "$2" ]] && return; sleep .1; done
  fail "$1 never became $2: $(call status)"
}
# rememberView keeps a picked centre within 1e-6° (#32); assert the exact
# values the picker wrote, not a Mercator round-trip.
state_exact() { # file, lat, lon
  jq -e --argjson lat "$2" --argjson lon "$3" \
    '.lat == $lat and .lon == $lon' "$1" > /dev/null 2>&1
}

# Isolated from the machine: a remembered centre is the camera, weather is
# not read unless OMASTORM_LOCATION names a file.
cat > "$check_dir/state.json" <<'JSON'
{"lat":58.26,"lon":12.83,"span":180,"name":"Vara"}
JSON
start "$check_dir/none.toml" "$check_dir/missing.json" "$check_dir/state.json"
until_field locationSource state
until_field needsLocation false
until_field lat 58.26
until_field lon 12.83
stop

# Weather outranks a missing state; nearest radar is Vara, camera on Gothenburg.
: > "$check_dir/state-weather.json"
start "$check_dir/none.toml" "$check_dir/weather.json" "$check_dir/state-weather.json"
until_field locationSource weather
until_field site vara
until_field lat 57.707
until_field lon 11.967
until_field locked false
# Pan writes state, never config. Reset returns to the weather place.
call run pan_left
for _ in {1..30}; do grep -q '"lat"' "$check_dir/state-weather.json" 2>/dev/null && break; sleep .1; done
grep -q '"lat"' "$check_dir/state-weather.json" || fail "Pan did not write state.json" "$(cat "$check_dir/state-weather.json" 2>/dev/null || true)"
grep -q home_site "$check_dir/none.toml" && fail "Pan wrote config.toml"
panned_lon=$(field lon)
call run reset
until_field lon 11.967
[[ $panned_lon != 11.967 ]] || fail "pan_left did not move the camera"
# Location picker: labeled lat/lon fields write state; a bad latitude is named.
quickshell ipc --pid "$pid" call location open ""
for _ in {1..30}; do [[ $(quickshell ipc --pid "$pid" call location status | grep -o '"open":[a-z]*' | cut -d: -f2) == true ]] && break; sleep .1; done
quickshell ipc --pid "$pid" call location setLat 95
quickshell ipc --pid "$pid" call location setLon 12.8
err=$(quickshell ipc --pid "$pid" call location status)
[[ $err == *latitude* ]] || fail "Invalid latitude was not named: $err"
quickshell ipc --pid "$pid" call location setLat 59.33
quickshell ipc --pid "$pid" call location accept
until_field lat 59.33
until_field lon 12.8
until_field locationSource state
until_field locked false
# A queued settle from the previous camera must not pull the view back, and
# must not rewrite the pick as floating-point noise (#32).
sleep 0.6
until_field lat 59.33
until_field lon 12.8
state_exact "$check_dir/state-weather.json" 59.33 12.8 || fail "Location picker did not keep exact centre" "$(cat "$check_dir/state-weather.json")"
grep -q center_lat "$check_dir/none.toml" && fail "Location picker wrote config.toml"
# n selects the nearest radar and does not move the camera.
before_lat=$(field lat); before_lon=$(field lon)
call run nearest
until_field locked false
expect 'n left the camera' "$before_lat $before_lon" "$(field lat) $(field lon)"
# Place search through the engine.
quickshell ipc --pid "$pid" call location open göteborg
for _ in {1..40}; do
  matches=$(quickshell ipc --pid "$pid" call location matches)
  [[ $matches == *Göteborg* ]] && break
  sleep .1
done
[[ $matches == *Göteborg* ]] || fail "Place search did not find Göteborg: $matches"
quickshell ipc --pid "$pid" call location open skåre
for _ in {1..40}; do
  matches=$(quickshell ipc --pid "$pid" call location matches)
  [[ $matches == *Värmland* && $matches == *Skåne* ]] && break
  sleep .1
done
[[ $matches == *Värmland* && $matches == *Skåne* ]] || fail "Skåre results did not name their counties: $matches"
quickshell ipc --pid "$pid" call location open vara
for _ in {1..40}; do
  matches=$(quickshell ipc --pid "$pid" call location matches)
  [[ $matches == *Vara* && $matches == *Västra\ Götaland* ]] && break
  sleep .1
done
[[ $matches == *Vara* && $matches == *Västra\ Götaland* ]] || fail "Vara was not in the gazetteer: $matches"
quickshell ipc --pid "$pid" call location close
stop

# Remembered centre outranks weather.
cat > "$check_dir/state.json" <<'JSON'
{"lat":59.33,"lon":18.07,"span":180,"name":"Stockholm"}
JSON
start "$check_dir/none.toml" "$check_dir/weather.json" "$check_dir/state.json"
until_field locationSource state
until_field lat 59.33
until_field lon 18.07
stop

# Explicit centre outranks remembered state on every launch.
cat > "$check_dir/config.toml" <<'TOML'
center_lat = 57.707
center_lon = 11.967
locked_radar = "kiruna"
TOML
start "$check_dir/config.toml" "$check_dir/weather.json" "$check_dir/state.json"
until_field locationSource config
until_field lat 57.707
until_field lon 11.967
until_field site kiruna
until_field locked true
until_field outsideCoverage true
until_field lockSource config
# Unlocking lasts this session: a later pan must not restore the configured lock.
call run lock
until_field locked false
call run pan_left
until_field locked false
stop

# First-run picker keeps a configured lock; centre and radar stay independent.
cat > "$check_dir/lock-only.toml" <<'TOML'
locked_radar = "kiruna"
TOML
: > "$check_dir/lock-only-state.json"
start "$check_dir/lock-only.toml" "$check_dir/missing.json" "$check_dir/lock-only-state.json"
until_field needsLocation true
quickshell ipc --pid "$pid" call location go 57.71 11.97 Gothenburg
until_field lat 57.71
until_field lon 11.97
until_field site kiruna
until_field locked true
until_field lockSource config
stop

# Restore a view that this process actually changed: pan and zoom, persist,
# close, reopen, and match the saved camera including span.
: > "$check_dir/empty.toml"
cat > "$check_dir/state.json" <<'JSON'
{"lat":58.26,"lon":12.83,"span":180}
JSON
start "$check_dir/empty.toml" "$check_dir/missing.json" "$check_dir/state.json"
until_field lat 58.26
until_field lon 12.83
call run pan_left
call run zoom_in
sleep 1
want_lat=$(field lat)
want_lon=$(field lon)
want_span=$(field span)
grep -q '"lat"' "$check_dir/state.json" || fail "Pan did not persist state for reopen" "$(cat "$check_dir/state.json")"
stop
start "$check_dir/empty.toml" "$check_dir/missing.json" "$check_dir/state.json"
until_field lat "$want_lat"
until_field lon "$want_lon"
expect 'Reopened span matches the saved view' "$want_span" "$(field span)"
stop

# Invalid state fields are dropped; a bad pair in config is named.
printf '{"lat":"south","lock":1}\n' > "$check_dir/bad-state.json"
cat > "$check_dir/bad.toml" <<'TOML'
center_lat = 58.3
locked_radar = 4
home_site = "vara"
follow = true
TOML
start "$check_dir/bad.toml" "$check_dir/missing.json" "$check_dir/bad-state.json"
e=""
for _ in {1..50}; do e=$(call errors); [[ $e == *center_lat* ]] && break; sleep .1; done
[[ $e == *center_lat* ]] || fail "Unpaired centre was not reported: $e"
[[ $e == *locked_radar* ]] || fail "Non-string locked_radar was not reported: $e"
[[ $e == *home_site* ]] || fail "home_site was not reported unused: $e"
[[ $e == *follow* ]] || fail "follow was not reported unused: $e"
stop

if rg -q 'TypeError|ReferenceError|Unable to assign|Failed to create.*context|is not a function' "$check_dir/log"; then fail "QML errors in the log"; fi
echo "LOCATION_PASSED"
