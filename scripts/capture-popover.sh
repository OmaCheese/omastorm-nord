#!/usr/bin/env bash
# Live Vara in the production card, under a representative bar strip.
# Its own daemon/cache keeps the user's selected station and frame untouched.
set -euo pipefail
cd "$(dirname "$0")/.."
scratch=$(mktemp -d /tmp/omastorm-popover-capture.XXXXXX)
export XDG_RUNTIME_DIR="$scratch/runtime" XDG_CACHE_HOME="$scratch/cache"
export OMASTORM_ROOT="$PWD" OMASTORM_CONFIG="$scratch/config.toml"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
mkdir -p "$XDG_RUNTIME_DIR" review
jq -r '.sites[] | select(.id=="vara") | "center_lat = \(.lat)\ncenter_lon = \(.lon)\nlocked_radar = \"vara\""' engine/data/sites.json > "$OMASTORM_CONFIG"
pid=
cleanup() {
  [[ -z $pid ]] || kill "$pid" 2>/dev/null || true
  target/debug/omastorm-engine stop >/dev/null 2>&1 || true
}
trap cleanup EXIT
quickshell -p ui/PopoverHarness.qml > "$scratch/ui.log" 2>&1 &
pid=$!
call() { quickshell ipc --pid "$pid" call popover "$@"; }
ready=0
for _ in {1..120}; do
  if call status 2>/dev/null | jq -e '.site == "vara" and .condition == "ok" and (.frame | contains("loading") | not)' >/dev/null 2>&1; then ready=1; break; fi
  sleep .5
done
[[ $ready == 1 ]] || { cat "$scratch/ui.log"; echo "No live Vara frame within 60 s" >&2; exit 1; }
sleep 3
for treatment in GLYPHS PIXELS STIPPLE; do
  call treatment "$treatment"
  sleep .3
  path="$PWD/review/popover-${treatment,,}.png"
  rm -f "$path"
  call capture "$path"
  for _ in {1..50}; do [[ -s $path ]] && break; sleep .1; done
  [[ -s $path ]] || exit 1
done
call status > review/popover-state.json
call quit
wait "$pid"
pid=
if rg 'Binding loop|ReferenceError|TypeError|Unable to assign|Failed to load' "$scratch/ui.log"; then exit 1; fi
echo 'review/popover-glyphs.png · popover-pixels.png · popover-stipple.png'
