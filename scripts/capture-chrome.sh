#!/usr/bin/env bash
# S46's captures and checks of the desktop chrome, in the real window over
# the vendored Vara scan (archive; the weather layers' legends show whether
# or not stations arrive):
#   - the site picker dropped down under its chip, dark and light;
#   - the right-click menu (and its product submenu), dark and light;
#   - the enlarged legends (radar card, temperature/wind, strikes' key);
#   - the window at Omarchy text sizes 9, 12 and 16 (OMASTORM_USER_SHELL,
#     never the user's shell.toml), with the popover card beside them;
#   - a live text-size change (the file replaced as omarchy-display-text-size
#     does) re-flowing the open window;
#   - a real wheel (QtTest's TestEvent through the harness) over every
#     overlay leaves map.span unchanged, and over the bare map changes it;
#   - a right press and drag opens the menu and never pans.
# RUN names the directory for the runtime, cache, state and harness
# (default ~/Projects/omastorm-S46-run/chrome); the PNGs go to review/s46/.
# ONLY="themes sizes popover live picker ipc wheel" picks sections (default: all).
# S48 adds the picker section: its PNGs go to review/s48/.
set -euo pipefail
cd "$(dirname "$0")/.."
run=${RUN:-$HOME/Projects/omastorm-S46-run/chrome}
out=$PWD/review/s46
mkdir -p "$run/rt" "$run/cache" "$run/state" "$out"
chmod 700 "$run/rt"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
export XDG_RUNTIME_DIR="$run/rt" XDG_CACHE_HOME="$run/cache"
export OMASTORM_ARCHIVE=${OMASTORM_ARCHIVE:-$PWD/data/raw/radar_vara_qcvol_202609131055.h5}
: > "$run/config.toml"
export OMASTORM_CONFIG="$run/config.toml" OMASTORM_STATE="$run/state/state.json"
pid=
cleanup() {
  [[ -z $pid ]] || kill "$pid" 2>/dev/null || true
  target/debug/omastorm-engine stop > /dev/null 2>&1 || true
}
trap cleanup EXIT
dark=$run/theme-dark light=$run/theme-light
mkdir -p "$dark" "$light"
printf 'background = "#1a1b26"\nforeground = "#a9b1d6"\naccent = "#7aa2f7"\ncyan = "#7dcfff"\n' > "$dark/colors.toml"
printf 'mode = "light"\nbackground = "#f5f1e8"\nforeground = "#343a48"\naccent = "#365ba8"\ncyan = "#1f7a8c"\n' > "$light/colors.toml"
for base in 9 12 16 20; do printf '[font]\nbase-size = %s\n' "$base" > "$run/user-$base.toml"; done

# The input harness (real events), plus a right button for this check.
harness_qml=$(TMPDIR="$run" scripts/capture-harness.sh --input)
harness=$(dirname "$harness_qml")
perl -0pi -e 's/(\n        function click\(x: real, y: real\): void \{[^\n]*\n)/$1        function rpress(x: real, y: real): void { inputEvents.mousePress(surface, x, y, Qt.RightButton, Qt.NoModifier, -1); }\n        function rmove(x: real, y: real): void { inputEvents.mouseMove(surface, x, y, -1, Qt.RightButton, Qt.NoModifier); }\n        function rrelease(x: real, y: real): void { inputEvents.mouseRelease(surface, x, y, Qt.RightButton, Qt.NoModifier, -1); }\n/' "$harness/RadarWindow.qml"
grep -q 'function rpress' "$harness/RadarWindow.qml"
# S48: the picker's centred fallback (no chip), on demand.
perl -0pi -e 's/(\n        function rpress\(x: real, y: real\): void \{[^\n]*\n)/$1        function unanchor(): void { picker.anchorItem = null; }\n/' "$harness/RadarWindow.qml"
grep -q 'function unanchor' "$harness/RadarWindow.qml"

call() { quickshell ipc --pid "$pid" call "$@"; }
field() { jq -r "$1"; }
# start theme base layers [width height]: a window on the harness.
start() {
  local theme=$1 base=$2 layers=$3 width=${4:-1100} height=${5:-780}
  cp "$run/user-$base.toml" "$run/user.toml"
  printf '{"lat":58.6,"lon":13.4,"span":420,"lock":"vara"}\n' > "$run/state/state.json"
  OMASTORM_QML="$harness_qml" OMASTORM_THEME_DIR=$([[ $theme == light ]] && echo "$light" || echo "$dark") \
    OMASTORM_USER_SHELL="$run/user.toml" OMASTORM_LAYERS="$layers" \
    OMASTORM_WIDTH="$width" OMASTORM_HEIGHT="$height" \
    bash run.sh > "$run/window-$theme-$base.log" 2>&1 &
  pid=$!
  for _ in {1..200}; do call chrome status > /dev/null 2>&1 && break; sleep .1; done
  # The scan on screen (the archive's one frame).
  for _ in {1..100}; do [[ $(call loading status 2>/dev/null | field .drawn) == true ]] && break; sleep .1; done
  sleep 1.5
}
stop() { call keys run close > /dev/null 2>&1 || true; kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; pid=; }
grab() { rm -f "$out/$1.png"; call input grab "$out/$1.png"; for _ in {1..50}; do [[ -s $out/$1.png ]] && break; sleep .1; done; [[ -s $out/$1.png ]] || { echo "no capture $1" >&2; exit 1; }; echo "captured review/s46/$1.png"; }
centre() { local r; r=$(call chrome rect "$1"); jq -r '"\(.x + (.w / 2 | floor)) \(.y + (.h / 2 | floor))"' <<< "$r"; }
span() { call chrome status | field .span; }
want() { [[ -z ${ONLY:-} || " $ONLY " == *" $1 "* ]]; }

# ---- captures, dark and light ------------------------------------------------
want themes && for theme in dark light; do
  start "$theme" 12 radar,temp,wind,lightning
  call picker open ""; sleep .4
  echo "picker card $(call chrome rect picker) under chip $(call chrome rect chip)"
  grab "$theme-picker"
  call picker close
  read -r mx my < <(centre map)
  call chrome menu 380 220; sleep .3
  grab "$theme-menu"
  # The product submenu, from the keyboard: down to PRODUCT, then Right.
  # Down steps over the rows that act; PRODUCT is the n-th of them.
  n=$(call chrome status | jq '[.rows[] | select(. != "header" and . != "sep")] | map(startswith("PRODUCT")) | index(true)')
  for _ in $(seq 0 "$n"); do call input key Down; done
  call input key Right; sleep .3
  grab "$theme-menu-product"
  call input key Escape; call input key Escape
  call chrome legend radar; sleep .4
  grab "$theme-legend-radar"
  call chrome legend obs; sleep .4
  grab "$theme-legend-obs"
  call chrome legend lightning; sleep .4
  grab "$theme-legend-lightning"
  call chrome legend ""
  stop
done

# ---- text sizes ----------------------------------------------------------------
want sizes && for base in 9 12 16; do
  start dark "$base" radar,temp,wind
  grab "base-$base"
  call picker open ""; sleep .4; grab "base-$base-picker"; call picker close
  call chrome menu 380 200; sleep .3; grab "base-$base-menu"; call chrome closeMenu
  call layers panel true; sleep .3; grab "base-$base-layers"; call layers panel false
  call keys run help; sleep .3; grab "base-$base-keys"; call keys run help
  echo "base $base: $(call chrome status | jq -c '{base, size}')"
  stop
done
# The menu fits at 16 and 20 with four recent radars and RESET: recents go
# first (review S3); LOCATION, KEYS and RESET stay.
want sizes && for base in 16 20; do
  start dark "$base" radar
  call chrome recent "hudiksvall,kiruna,lulea,ostersund"
  call chrome menu 380 40; sleep .4
  card=$(call chrome rect menu); rows=$(call chrome shownRows)
  h=780   # start's window height
  fits=$(jq --argjson h "$h" '.y >= 0 and (.y + .h) <= $h' <<< "$card")
  keep=$(jq '(index("LOCATION…") != null) and (index("KEYS") != null) and (index("RESET (RELOAD EVERYTHING)") != null)' <<< "$rows")
  echo "MENU at base $base: card $card, rows $(jq -c 'map(select(. != "sep" and . != "header"))' <<< "$rows")"
  if [[ $fits == true && $keep == true ]]; then echo "MENU fits at base $base with RESET: PASS"; else echo "MENU at base $base: FAIL"; exit 1; fi
  grab "base-$base-menu-recents"
  call chrome closeMenu
  stop
done
# The popover card at each size, in its harness (PopoverHarness), offline.
want popover && for base in 9 12 16; do
  cp "$run/user-$base.toml" "$run/user.toml"
  # OMASTORM_ROOT: the session ensures this checkout's engine, the one the
  # windows used, instead of stopping it as another build's (review N3).
  OMASTORM_ROOT="$PWD" OMASTORM_THEME_DIR="$dark" OMASTORM_USER_SHELL="$run/user.toml" quickshell -p ui/PopoverHarness.qml > "$run/popover-$base.log" 2>&1 &
  pid=$!
  for _ in {1..200}; do call popover status > /dev/null 2>&1 && break; sleep .1; done
  for _ in {1..150}; do call popover status 2>/dev/null | jq -e '.site == "vara" and .condition == "ok" and (.frame | contains("loading") | not)' > /dev/null 2>&1 && break; sleep .2; done
  sleep 1
  rm -f "$out/popover-base-$base.png"
  call popover capture "$out/popover-base-$base.png"
  for _ in {1..50}; do [[ -s $out/popover-base-$base.png ]] && break; sleep .1; done
  echo "captured review/s46/popover-base-$base.png"
  call popover quit > /dev/null 2>&1 || true
  wait "$pid" 2>/dev/null || true; pid=
done

# ---- a live text-size change ---------------------------------------------------
if want live; then
start dark 12 radar
before=$(call chrome status | field .base)
tmp=$(mktemp "$run/user.XXXXXX"); printf '[font]\nbase-size = 16\n' > "$tmp"; mv "$tmp" "$run/user.toml"
after=
for _ in {1..50}; do after=$(call chrome status | field .base); [[ $after == 16 ]] && break; sleep .1; done
echo "LIVE text size: $before -> $after (file replaced, window kept open)"
[[ $after == 16 ]] || { echo "the window did not follow base-size" >&2; exit 1; }
grab live-16
stop
fi

# ---- S48: the picker's list scrolls ---------------------------------------------
# Every match in a list as tall as the window lets it: a real wheel over the
# list moves it and never the map, the keys keep the selection in view,
# typing goes back to the top, the card shrinks to few rows, and the load
# logs no IPC warning. PNGs in review/s48/.
out48=$PWD/review/s48
mkdir -p "$out48"
grab48() { rm -f "$out48/$1.png"; call input grab "$out48/$1.png"; for _ in {1..50}; do [[ -s $out48/$1.png ]] && break; sleep .1; done; [[ -s $out48/$1.png ]] || { echo "no capture $1" >&2; exit 1; }; echo "captured review/s48/$1.png"; }
pfail=0
check48() { if [[ $2 == true ]]; then echo "PICKER $1: PASS"; else echo "PICKER $1: FAIL ($3)"; pfail=1; fi; }
pview() { call picker view; }
# picker_run <theme> <base> [width height]
picker_run() {
  local theme=$1 base=$2 w=${3:-1100} h=${4:-780} tag v a b lx ly
  tag=$theme-$base
  [[ $w == 1100 && $h == 780 ]] || tag+="-${w}x$h"
  start "$theme" "$base" radar "$w" "$h"
  call chrome status > /dev/null || { echo "PICKER $tag: the window never answered" >&2; tail -5 "$run/window-$theme-$base.log" >&2; exit 1; }
  local ipc; ipc=$(grep -c 'IpcHandler' "$run/window-$theme-$base.log" || true)
  check48 "$tag no IPC warning on load" "$([[ $ipc == 0 ]] && echo true || echo false)" "$(grep IpcHandler "$run/window-$theme-$base.log" | head -2)"
  call picker open ""; sleep .4
  v=$(pview); echo "PICKER $tag open: $v"
  check48 "$tag lists every match ($(jq -r .footer <<< "$v"))" "$(call picker matches | jq --argjson t "$(call picker status | jq .total)" 'length == $t and $t > 4')" "$(call picker status)"
  check48 "$tag card ends 6 px above the window's bottom or earlier" "$(jq '.card.y + .card.h <= .window - 6' <<< "$v")" "$v"
  check48 "$tag list overflows and is window-tall" "$(jq --argjson h "$h" '.overflow and (.card.y + .card.h >= .window - 8)' <<< "$v")" "$v"
  # The wheel over the list, a notch at a time, to mid-way.
  read -r lx ly < <(centre pickerList)
  local maxy s0 s1 y0 y1; maxy=$(jq '.contentHeight - .height' <<< "$v")
  s0=$(span); y0=$(pview | jq .contentY)
  for _ in {1..12}; do
    [[ $(pview | jq --argjson m "$maxy" '.contentY >= $m / 2') == true ]] && break
    call input wheel "$lx" "$ly" -120; sleep .15
  done
  s1=$(span); y1=$(pview | jq .contentY)
  check48 "$tag wheel over the list scrolls it ($y0 -> $y1 of $maxy), span $s0 -> $s1" "$([[ $s0 == "$s1" && $y1 -gt $y0 ]] && echo true || echo false)" ""
  # Review SF1: the selection comes along, into the rows shown whole.
  check48 "$tag wheel keeps the selection in view" "$(pview | jq '.selectedVisible')" "$(pview)"
  grab48 "$tag-scrolled"
  call input wheel "$lx" "$ly" 120; sleep .15
  check48 "$tag wheel up scrolls back" "$(pview | jq --argjson y "$y1" '.contentY < $y')" "$(pview)"
  # Keys keep the selection in view.
  local k
  for k in End PageUp PageUp Home PageDown PageDown Down Down Down End Up; do
    call input key "$k"; sleep .08
    v=$(pview)
    [[ $(jq .selectedVisible <<< "$v") == true ]] || { check48 "$tag key $k keeps the selection in view" false "$v"; break; }
  done
  v=$(pview)
  check48 "$tag keys End/PageUp/Home/PageDown/Down/Up keep the selection in view (last $v)" "$(jq '.selectedVisible and .selected > 0' <<< "$v")" "$v"
  # Review SF2: a pointer resting over the list does not take the
  # selection when the rows slide under it.
  read -r lx ly < <(centre pickerList)
  call input key Home; sleep .08
  call input move "$lx" "$ly"; sleep .1
  call input key End; sleep .2
  check48 "$tag End with the pointer resting over the list selects the last row" "$(pview | jq --argjson n "$(call picker matches | jq length)" '.selected == $n - 1 and .selectedVisible')" "$(pview)"
  call input key Home; sleep .08
  check48 "$tag Home goes to the top" "$(pview | jq '.selected == 0 and .contentY == 0')" "$(pview)"
  call input key End; sleep .1
  # Typing filters and goes back to the top.
  call input text "no"; sleep .3
  v=$(pview); echo "PICKER $tag filtered 'no': $v"
  check48 "$tag typing resets to the top, footer '$(jq -r .footer <<< "$v")'" "$(jq '.contentY == 0 and .selected == 0 and (.footer | test("^[0-9]+ of [0-9]+$"))' <<< "$v")" "$v"
  grab48 "$tag-filtered"
  # Few rows: the card shrinks to them.
  call picker open vara; sleep .3
  v=$(pview)
  check48 "$tag few rows shrink the card" "$(jq '(.overflow | not) and .height == .contentHeight' <<< "$v")" "$v"
  if [[ $base == 12 && $w == 1100 ]]; then grab48 "$tag-few"; fi
  call picker close
  # The centred fallback (no chip) takes the same room from its own top.
  if [[ $theme == dark ]]; then
    call input unanchor; call picker open ""; sleep .4
    v=$(pview); echo "PICKER $tag centred: $v"
    check48 "$tag centred fallback: window-tall, 6 px margin, scrolls" "$(jq '.overflow and (.card.y + .card.h <= .window - 6) and (.card.y + .card.h >= .window - 8)' <<< "$v")" "$v"
    read -r lx ly < <(centre pickerList)
    s0=$(span); call input wheel "$lx" "$ly" -240; sleep .2; s1=$(span)
    check48 "$tag centred fallback wheel" "$(pview | jq --arg a "$s0" --arg b "$s1" '.contentY > 0 and $a == $b')" "$(pview)"
    grab48 "$tag-centred"
    call picker close
  fi
  stop
}
if want picker; then
  for theme in dark light; do for base in 12 16; do picker_run "$theme" "$base"; done; done
  # The compact window (under 560 px wide), and a short one.
  picker_run dark 12 520 700
  picker_run dark 16 1100 480
  if (( pfail )); then echo "S48 picker checks: FAIL" >&2; exit 1; fi
  echo "S48 picker checks: PASS"
fi

# ---- S48: the plugin's load leaves the `theme` IPC target alone -----------------
# The Omarchy shell also hosts upstream Omastorm, whose Theme registers
# `theme` first; our session (PluginSession's Theme) and the panel's window
# must not register it again, nor leave an enabled handler without a target.
if want ipc; then
  sim=$(mktemp -d "$run/plugin-sim.XXXXXX")
  cp ui/*.qml ui/*.js ui/qmldir "$sim/"
  ln -sfn "$PWD/ui/shaders" "$sim/shaders"
  cat > "$sim/sim.qml" <<'QML'
import QtQuick
import Quickshell
import Quickshell.Io
ShellRoot {
    // Upstream Omastorm's Theme, loaded first in the real shell.
    IpcHandler { target: "theme"; function reload(): void {} }
    property var session: PluginSession
    Panel {}
    IpcHandler { target: "sim"; function base(): string { return String(PluginSession.theme.snapshot.baseSize); } }
}
QML
  OMASTORM_ROOT="$PWD" OMASTORM_THEME_DIR="$dark" OMASTORM_USER_SHELL="$run/user-12.toml" quickshell -p "$sim/sim.qml" > "$run/plugin-sim.log" 2>&1 &
  pid=$!
  for _ in {1..100}; do call sim base > /dev/null 2>&1 && break; sleep .1; done
  b=$(call sim base || true); sleep 1.5
  kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; pid=
  if [[ $b == 12 ]] && ! grep -q 'IpcHandler' "$run/plugin-sim.log"; then echo "IPC plugin load (upstream 'theme' first, session + panel): no IpcHandler warning: PASS"
  else echo "IPC plugin load: base=$b, $(grep IpcHandler "$run/plugin-sim.log" | head -2): FAIL"; exit 1; fi
fi

# ---- the wheel over every overlay ----------------------------------------------
fail=0
want wheel || exit 0
# wheel <label> <x> <y>: one notch; the span must not move.
wheel_over() {
  local label=$1 x=$2 y=$3 a b
  a=$(span); call input wheel "$x" "$y" 120; sleep .25; b=$(span)
  if [[ $a == "$b" ]]; then echo "WHEEL over $label at $x,$y: span $a -> $b unchanged: PASS"
  else echo "WHEEL over $label at $x,$y: span $a -> $b CHANGED: FAIL"; fail=1; fi
}
start dark 12 radar,temp,wind,lightning
read -r mx my < <(centre map)
a=$(span); call input wheel "$mx" "$my" 120; sleep .25; b=$(span)
echo "WHEEL over the bare map (control): span $a -> $b $([[ $a != "$b" ]] && echo changed: PASS || echo 'unchanged: FAIL')"
[[ $a != "$b" ]] || fail=1
call picker open ""; sleep .3
read -r x y < <(centre picker); wheel_over "site picker card" "$x" "$y"
wheel_over "site picker scrim" "$mx" "$my"
call picker close
call location open ""; sleep .3; wheel_over "location picker" "$mx" "$my"; call location close
call keys run help; sleep .3; wheel_over "keys sheet" "$mx" "$my"; call keys run help
call layers panel true; sleep .3; wheel_over "LAYERS panel" "$mx" "$my"; call layers panel false
call keys menu true; sleep .2; wheel_over "treatment menu" "$mx" "$my"; call keys menu false
call keys productChooser true; sleep .2; wheel_over "product menu" "$mx" "$my"; call keys productChooser false
call chrome menu 380 220; sleep .3
read -r x y < <(centre menu); wheel_over "right-click menu card" "$x" "$y"
wheel_over "right-click menu scrim" "$mx" "$my"
call chrome closeMenu
call chrome legend radar; sleep .3
read -r x y < <(centre legendCard); wheel_over "enlarged radar legend" "$x" "$y"
call chrome legend ""
for name in obsLegend lightningKey help scale north credit; do
  r=$(call chrome rect "$name")
  if [[ $(jq -r .visible <<< "$r") != true ]]; then echo "WHEEL over $name: not shown, skipped"; continue; fi
  read -r x y < <(centre "$name"); wheel_over "$name" "$x" "$y"
done
call chrome legend obs; sleep .3
read -r x y < <(centre obsLegend); wheel_over "enlarged obs legend" "$x" "$y"
call chrome legend ""
call mosaic open; sleep .6
read -r x y < <(centre mosaic); wheel_over "My mosaic panel" "$x" "$y"
call mosaic close; sleep .3

# ---- FROM > GRID keeps its submenu beside its row (review S1) -------------------
call chrome menu 380 120; sleep .3
n=$(call chrome status | jq '[.rows[] | select(. != "header" and . != "sep")] | map(startswith("FROM")) | index(true)')
for _ in $(seq 0 "$n"); do call input key Down; done
call input key Right; sleep .3
before=$(call chrome rect submenu | jq -c '{x, y}')
call input key Down; call input key Return; sleep 1.5   # GRID: the rows rebuild, the menu stays
after=$(call chrome rect submenu | jq -c '{x, y}')
source_now=$(call layers status | field .source)
if [[ $before == "$after" && $source_now == grid ]]; then echo "SUBMENU FROM > GRID: stays at $after, source $source_now: PASS"
else echo "SUBMENU FROM > GRID: $before -> $after, source $source_now: FAIL"; fail=1; fi
call layers source stations; call chrome closeMenu; sleep .3

# ---- a right press over an open overlay (review M1) ------------------------------
# rclick_over <overlay> <open call> <status field test>: a right click on the
# overlay's card leaves it open with the keys (Escape then closes it) and
# the menu shut; a right click on its scrim closes it like a left one.
rclick() { call input rpress "$1" "$2"; call input rrelease "$1" "$2"; sleep .25; }
rclick_over() {
  local name=$1 open=$2 isopen=$3 x y
  # shellcheck disable=SC2086
  call $open; sleep .35
  read -r x y < <(centre "$name")
  rclick "$x" "$y"
  local shown menu
  shown=$(eval "$isopen"); menu=$(call chrome status | field .menu)
  call input key Escape; sleep .25
  local after; after=$(eval "$isopen")
  if [[ $shown == true && $menu == false && $after == false ]]; then echo "RIGHT click on the $name card: stays open, menu shut, Escape still closes it: PASS"
  else echo "RIGHT click on the $name card: open=$shown menu=$menu afterEscape=$after: FAIL"; fail=1; fi
  # shellcheck disable=SC2086
  call $open; sleep .35
  rclick 30 "$my"
  shown=$(eval "$isopen"); menu=$(call chrome status | field .menu)
  if [[ $shown == false && $menu == false ]]; then echo "RIGHT click on the $name scrim: closes it, no menu: PASS"
  else echo "RIGHT click on the $name scrim: open=$shown menu=$menu: FAIL"; fail=1; call chrome closeMenu; fi
}
rclick_over picker "picker open x" "call picker status | field .open"
rclick_over location "location open x" "call location status | field .open"
rclick_over sheet "keys run help" "call keys status | field .sheet"
rclick_over layers "layers panel true" "call layers status | field .panel"

# ---- a right press never pans ------------------------------------------------------
before=$(call chrome status | jq -c '{lat, lon, span}')
call input rpress "$mx" "$my"; call input rmove $((mx + 60)) $((my + 40)); call input rmove $((mx + 120)) $((my + 80)); call input rrelease $((mx + 120)) $((my + 80))
sleep .3
after=$(call chrome status | jq -c '{lat, lon, span}')
menu=$(call chrome status | field .menu)
echo "RIGHT press+drag: camera $before -> $after, menu open: $menu"
if [[ $before != "$after" || $menu != true ]]; then echo "RIGHT press: FAIL" >&2; fail=1; else echo "RIGHT press: PASS"; fi
call input key Escape; sleep .2
echo "menu after Escape: $(call chrome status | field .menu)"
stop
if (( fail )); then echo "S46 chrome checks: FAIL" >&2; exit 1; fi
echo "S46 chrome checks: PASS"
