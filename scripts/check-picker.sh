#!/usr/bin/env bash
# The site picker (DESIGN.md, picker as built) in the real window, driven
# through its IPC handler against the fixture daemon: the tiers and the
# distance order, the four-row cut, the empty and the hopeless query, the
# selection keys, and Enter selecting and locking the station for real. Run
# through check.sh, whose scratch daemon it leaves on the chosen station; it
# comes last there for that reason.
set -euo pipefail
cd "$(dirname "$0")/.."
check_dir="$PWD/target/check-picker"
mkdir -p "$check_dir"
: > "$check_dir/none.toml"
# Camera on Vara so the empty query ranks the SMHI neighbours by distance.
jq -c '.sites[] | select(.id=="vara") | {lat, lon, span: 210}' engine/data/sites.json > "$check_dir/state.json"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
OMASTORM_CONFIG="$check_dir/none.toml" OMASTORM_STATE="$check_dir/state.json" bash run.sh > "$check_dir/log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT
call() { quickshell ipc --pid "$pid" call picker "$@"; }
fail() { printf '%s\n' "$@" >&2; cat "$check_dir/log" >&2; exit 1; }
expect() { [[ "$3" == "$2" ]] || fail "$1" "Expected: $2" "Actual:   $3"; }
for _ in {1..100}; do call status > /dev/null 2>&1 && break; sleep .1; done
call status > /dev/null || fail "The window's picker IPC never answered"
call open ""
for _ in {1..50}; do [[ $(call matches) != '[]' ]] && break; sleep .1; done
# Vara first, then the next-nearest SMHI sites (Åtvidaberg, Ängelholm, …).
m=$(call matches)
[[ $m == '["vara","atvidaberg",'* || $m == '["vara","angelholm",'* ]] || fail "Empty query did not list the nearest stations first: $m"
# 13: the 12 radars and the national composite (protocol v2, S8).
expect 'Empty query counts the whole table' '{"open":true,"query":"","selected":0,"total":13,"focused":true}' "$(call status)"
call open vara
expect 'ID prefix ranks first' '"vara"' "$(call matches | cut -d, -f1 | tr -d '[]')"
call open ostersund
expect 'Folded city word start ranks first' '"ostersund"' "$(call matches | cut -d, -f1 | tr -d '[]')"
call open väst
# County word start: Västra Götaland → Vara before other hits.
m=$(call matches)
[[ $m == '["vara",'* ]] || fail "Tier order for 'väst' was wrong: $m"
call open zzzq
expect 'A hopeless query shows nothing' '[]' "$(call matches)"
expect 'A hopeless query counts nothing' '{"open":true,"query":"zzzq","selected":0,"total":0,"focused":true}' "$(call status)"
call close
expect 'Close clears the picker' '{"open":false,"query":"","selected":0,"total":0,"focused":false}' "$(call status)"
call open a
call move 1; call move 5
expect 'Down stops at the last of four rows' '3' "$(call status | grep -o '"selected":[0-9]*' | cut -d: -f2)"
call move -9
expect 'Up stops at the first row' '0' "$(call status | grep -o '"selected":[0-9]*' | cut -d: -f2)"
call open balsta
first=$(call matches | cut -d, -f1 | tr -d '[]"')
expect 'balsta finds Bålsta' balsta "$first"
call accept
expect 'Enter closes the picker' 'false' "$(call status | grep -o '"open":[a-z]*' | cut -d: -f2)"
# The field must let go of the keyboard, or the next `/` types into it instead of reopening.
expect 'Enter hands the keyboard back' 'false' "$(call status | grep -o '"focused":[a-z]*' | cut -d: -f2)"
sock="$XDG_RUNTIME_DIR/omastorm-se/engine.sock"
for _ in {1..50}; do
  site=$(timeout 2 socat -t0.2 - "UNIX-CONNECT:$sock" < /dev/null | sed -n 2p | grep -o '"site":{[^}]*}' || true)
  [[ $site == *"\"id\":\"$first\""* && $site == *'"locked":true'* ]] && break
  sleep .1
done
[[ $site == *"\"id\":\"$first\""* && $site == *'"locked":true'* ]] || fail "Enter did not select and lock $first: $site"
want_lat=$(jq -r --arg id "$first" '.sites[] | select(.id==$id) | ((.lat * 1000) | round) / 1000' engine/data/sites.json)
want_lon=$(jq -r --arg id "$first" '.sites[] | select(.id==$id) | ((.lon * 1000) | round) / 1000' engine/data/sites.json)
for _ in {1..50}; do
  lat=$(quickshell ipc --pid "$pid" call keys field lat)
  lon=$(quickshell ipc --pid "$pid" call keys field lon)
  [[ $lat == "$want_lat" && $lon == "$want_lon" ]] && break
  sleep .1
done
[[ $lat == "$want_lat" && $lon == "$want_lon" ]] || fail "Enter did not centre the map on $first" "Expected: $want_lat $want_lon" "Actual:   $lat $lon"
if rg -q 'TypeError|ReferenceError|Unable to assign|Failed to create.*context' "$check_dir/log"; then fail "QML errors in the log"; fi
echo "PICKER_PASSED"
