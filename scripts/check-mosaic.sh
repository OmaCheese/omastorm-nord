#!/usr/bin/env bash
# My mosaic's docked panel (S40) in the real window, driven with real key,
# wheel and pointer events (capture-harness.sh --input) against the lane's
# daemon: opened through the site picker with Enter and through the RADARS
# chip, it holds the keyboard and Down moves the cursor one row at a time,
# also with the pointer resting over the list while it scrolls (the S40
# bug: hover put the cursor back under the pointer); the wheel over the
# panel never reaches the map, which shrinks beside the panel; `/` filters
# and Escape comes back; Space ticks; Escape cancels. The state override
# puts My mosaic on screen so the chip shows; the engine never gets a set
# (no SHOW), so nothing is fetched. The cancel steps send view_center: that
# is safe because the daemon is always a scratch one, the lane's under
# target/check, or (run alone) this check's own, stopped on exit, never the
# bar's in the session's runtime dir.
set -euo pipefail
cd "$(dirname "$0")/.."
own_runtime=
if [[ ${XDG_RUNTIME_DIR:-} != "$PWD"/target/* ]]; then
  own_runtime=$PWD/target/r-mosaic
  mkdir -p "$own_runtime" && chmod 700 "$own_runtime"
  mkdir -p "$own_runtime/cache" "$own_runtime/tmp"
  export XDG_RUNTIME_DIR=$own_runtime XDG_CACHE_HOME=$own_runtime/cache TMPDIR=$own_runtime/tmp
  export OMASTORM_ARCHIVE=${OMASTORM_ARCHIVE:-$PWD/data/raw/KTLX20130520_201643_V06.gz}
fi
check_dir="$PWD/target/check-mosaic"
mkdir -p "$check_dir"
: > "$check_dir/none.toml"
jq -c '.sites[] | select(.id=="vara") | {lat, lon, span: 210}' engine/data/sites.json > "$check_dir/state.json"
qml=$(bash scripts/capture-harness.sh --input)
cleanup() {
  kill "${pid:-}" 2>/dev/null || true
  wait "${pid:-}" 2>/dev/null || true
  rm -rf "$(dirname "$qml")"
  if [[ -n $own_runtime ]]; then
    target/debug/omastorm-engine stop > /dev/null 2>&1 || true
    rm -rf "$own_runtime"
  fi
}
trap cleanup EXIT
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
OMASTORM_QML="$qml" OMASTORM_CONFIG="$check_dir/none.toml" OMASTORM_STATE="$check_dir/state.json" \
  OMASTORM_WIDTH=960 OMASTORM_HEIGHT=680 OMASTORM_STATE_OVERRIDE='{"site":{"id":"mymosaic"}}' \
  bash run.sh > "$check_dir/log" 2>&1 &
pid=$!
call() { quickshell ipc --pid "$pid" call "$@"; }
fail() { printf '%s\n' "$@" >&2; cat "$check_dir/log" >&2; exit 1; }
expect() { [[ "$3" == "$2" ]] || fail "$1" "Expected: $2" "Actual:   $3"; }
field() { call mosaic status | jq -c "$1"; }
rect() { call input where "$1" | jq -r ".$2"; }
for _ in {1..100}; do call mosaic status > /dev/null 2>&1 && break; sleep .1; done
call mosaic status > /dev/null || fail "The window's mosaic IPC never answered"
for _ in {1..50}; do [[ $(field .rows) != null ]] && [[ $(call keys status 2>/dev/null | jq -r .site) == mymosaic ]] && break; sleep .1; done
frame_w=$(rect frame w)

# Through the site picker, with Enter as a person presses it.
call picker open "my mosaic"
for _ in {1..50}; do [[ $(call picker matches) == '["mymosaic"]' ]] && break; sleep .1; done
expect 'The site picker finds My mosaic' '["mymosaic"]' "$(call picker matches)"
call input key Return
expect 'Enter in the site picker opens the panel' true "$(field .open)"
expect 'The panel holds the keyboard' true "$(field .focused)"
expect 'The panel is docked' true "$(field .docked)"
map_w=$(field .mapWidth)
(( map_w <= frame_w - 380 )) || fail "The map did not shrink beside the panel: map $map_w of $frame_w"
# The pointer resting over the list while Down scrolls it.
px=$(( $(rect mosaic x) + $(rect mosaic w) / 2 )); py=$(( $(rect mosaic y) + $(rect mosaic h) - 90 ))
call input move "$px" "$py"
for _ in 1 2 3 4 5; do call input key Down; done
expect '5 x Down moves 5 rows (site picker)' 5 "$(field .cursor)"
for _ in {1..10}; do call input key Down; done
expect '10 more with the pointer over the scrolling list' 15 "$(field .cursor)"
for _ in 1 2 3; do call input key Up; call input key K; done
expect 'Up and k move back' 9 "$(field .cursor)"

# The map beside it is in pick mode (S39) and follows the cursor: the hot
# ring is the cursor's radar, on screen; a click on its mark ticks it and
# leaves the panel open; the pointer over another mark moves the cursor there.
expect 'The map is in pick mode' true "$(field .pickMode)"
expect "The map's hot radar is the cursor's" "$(field .hot)" "$(field .mapHot)"
# The wheel over the map still zooms it: out, so several radars show.
span0=$(field .span)
for _ in {1..8}; do call input wheel $(( $(rect map x) + $(rect map w) / 2 )) $(( $(rect map y) + $(rect map h) / 2 )) -120; done
[[ $(field .span) != "$span0" ]] || fail "The wheel over the map beside the panel did not zoom it"
call input key Down; call input key Up
hot=$(field .hot | jq -r .)
mx=$(call input mark "$hot" | jq .x); my=$(call input mark "$hot" | jq .y)
(( mx >= $(rect map x) && mx <= $(rect map x) + $(rect map w) && my >= $(rect map y) && my <= $(rect map y) + $(rect map h) )) \
  || fail "The cursor's radar $hot is off the map at $mx,$my"
call input click "$mx" "$my"
expect 'A click on the mark ticks it' "[\"$hot\"]" "$(field '[.draft.sites[].id]')"
expect 'and the panel stays open' true "$(field .open)"
expect "The map's ticks are the draft" "[\"$hot\"]" "$(field .mapPicked)"
call input click "$mx" "$my"
expect 'A second click unticks it' '[]' "$(field '[.draft.sites[].id]')"
other=
for id in $(jq -r '.sites[] | select(.kind != "grid") | .id' engine/data/sites.json); do
  [[ $id == "$hot" ]] && continue
  p=$(call input mark "$id"); x=$(jq .x <<< "$p"); y=$(jq .y <<< "$p")
  if (( x > $(rect map x) + 40 && x < $(rect map x) + $(rect map w) - 40 && y > $(rect map y) + 40 && y < $(rect map y) + $(rect map h) - 40 )); then
    other=$id; ox=$x; oy=$y; break
  fi
done
[[ -n $other ]] || fail "No other radar on the map to hover"
call input move "$ox" "$oy"
call input move "$((ox + 1))" "$oy"
expect 'The pointer over a mark puts the cursor on its row' "\"$other\"" "$(field .hot)"
call input move "$px" "$py"
call input key Home
expect 'Home goes to the first row' 0 "$(field .cursor)"
for _ in 1 2 3 4 5 6 7 8 9; do call input key Down; done

# The wheel over the panel scrolls the panel and never the map.
span=$(field .span)
for y in 20 120 $(( $(rect mosaic h) / 2 )) $(( $(rect mosaic h) - 30 )); do
  call input wheel "$px" $(( $(rect mosaic y) + y )) -120
  call input wheel "$px" $(( $(rect mosaic y) + y )) 120
done
expect 'The wheel over the panel leaves the map span' "$span" "$(field .span)"

# `/` filters, Escape goes back to the list with the filter kept, Space ticks.
call input key Slash
expect '/ goes to the filter' true "$(field .filterFocused)"
call input text "fi"
expect 'The filter holds what was typed' '"fi"' "$(field .filter)"
shown=$(field .shownRows)
(( shown > 0 && shown < $(field .rows) )) || fail "The filter did not narrow the list: $shown rows"
call input key Escape
expect 'Escape in the filter goes back to the list' true "$(field .focused)"
expect 'and keeps the filter' '"fi"' "$(field .filter)"
expect 'The window stays open' true "$(field .open)"
hot=$(field .hot)
call input key Space
expect 'Space ticks the cursor row' "[$hot]" "$(field '[.draft.sites[].id]')"
call input key Escape
expect 'Escape cancels' false "$(field .open)"

# Through the RADARS chip, which My mosaic on screen shows in the site row.
expect 'The RADARS chip shows while My mosaic is on screen' true "$(rect chip visible)"
call input click $(( $(rect chip x) + $(rect chip w) / 2 )) $(( $(rect chip y) + $(rect chip h) / 2 ))
expect 'The chip opens the panel' true "$(field .open)"
expect 'The panel holds the keyboard (chip)' true "$(field .focused)"
expect 'The cancelled tick is gone' '[]' "$(field '[.draft.sites[].id]')"
call input move "$px" "$py"
for _ in 1 2 3 4 5; do call input key Down; done
expect '5 x Down moves 5 rows (chip)' 5 "$(field .cursor)"
call input key J
expect 'j moves' 6 "$(field .cursor)"
call input key Escape
expect 'Escape closes' false "$(field .open)"
expect 'The map leaves pick mode' false "$(field .pickMode)"
expect 'The map has its width back' "$(( frame_w ))" "$(( $(field .mapWidth) ))"
if rg -q 'TypeError|ReferenceError|Unable to assign|Failed to create.*context' "$check_dir/log"; then fail "QML errors in the log"; fi
echo "MOSAIC_PANEL_PASSED"
