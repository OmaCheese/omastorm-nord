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
for base in 9 12 16; do printf '[font]\nbase-size = %s\n' "$base" > "$run/user-$base.toml"; done

# The input harness (real events), plus a right button for this check.
harness_qml=$(TMPDIR="$run" scripts/capture-harness.sh --input)
harness=$(dirname "$harness_qml")
perl -0pi -e 's/(\n        function click\(x: real, y: real\): void \{[^\n]*\n)/$1        function rpress(x: real, y: real): void { inputEvents.mousePress(surface, x, y, Qt.RightButton, Qt.NoModifier, -1); }\n        function rmove(x: real, y: real): void { inputEvents.mouseMove(surface, x, y, -1, Qt.RightButton, Qt.NoModifier); }\n        function rrelease(x: real, y: real): void { inputEvents.mouseRelease(surface, x, y, Qt.RightButton, Qt.NoModifier, -1); }\n/' "$harness/RadarWindow.qml"
grep -q 'function rpress' "$harness/RadarWindow.qml"

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

# ---- captures, dark and light ------------------------------------------------
for theme in dark light; do
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
for base in 9 12 16; do
  start dark "$base" radar,temp,wind
  grab "base-$base"
  call picker open ""; sleep .4; grab "base-$base-picker"; call picker close
  call chrome menu 380 200; sleep .3; grab "base-$base-menu"; call chrome closeMenu
  call layers panel true; sleep .3; grab "base-$base-layers"; call layers panel false
  call keys run help; sleep .3; grab "base-$base-keys"; call keys run help
  echo "base $base: $(call chrome status | jq -c '{base, size}')"
  stop
done
# The popover card at each size, in its harness (PopoverHarness), offline.
for base in 9 12 16; do
  cp "$run/user-$base.toml" "$run/user.toml"
  OMASTORM_THEME_DIR="$dark" OMASTORM_USER_SHELL="$run/user.toml" quickshell -p ui/PopoverHarness.qml > "$run/popover-$base.log" 2>&1 &
  pid=$!
  for _ in {1..200}; do call popover status > /dev/null 2>&1 && break; sleep .1; done
  for _ in {1..100}; do call popover status 2>/dev/null | jq -e '.frame | contains("loading") | not' > /dev/null 2>&1 && break; sleep .2; done
  sleep 1
  rm -f "$out/popover-base-$base.png"
  call popover capture "$out/popover-base-$base.png"
  for _ in {1..50}; do [[ -s $out/popover-base-$base.png ]] && break; sleep .1; done
  echo "captured review/s46/popover-base-$base.png"
  call popover quit > /dev/null 2>&1 || true
  wait "$pid" 2>/dev/null || true; pid=
done

# ---- a live text-size change ---------------------------------------------------
start dark 12 radar
before=$(call chrome status | field .base)
tmp=$(mktemp "$run/user.XXXXXX"); printf '[font]\nbase-size = 16\n' > "$tmp"; mv "$tmp" "$run/user.toml"
after=
for _ in {1..50}; do after=$(call chrome status | field .base); [[ $after == 16 ]] && break; sleep .1; done
echo "LIVE text size: $before -> $after (file replaced, window kept open)"
[[ $after == 16 ]] || { echo "the window did not follow base-size" >&2; exit 1; }
grab live-16
stop

# ---- the wheel over every overlay ----------------------------------------------
fail=0
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
(( fail == 0 )) && echo "S46 chrome checks: PASS" || { echo "S46 chrome checks: FAIL" >&2; exit 1; }
