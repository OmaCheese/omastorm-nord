#!/usr/bin/env bash
# S42's captures: the weather station layers in the real window, dark and
# light, over the vendored Vara scan: temperature, wind, both; the LAYERS
# panel; zoomed out (thinned) and in; a station's hover card. The layers
# are forced with OMASTORM_LAYERS so no layers.json is read or written.
#
# The stations are live: one engine serves every capture, so each provider
# is fetched once (10-minute cache) however many captures run. RUN names a
# directory for the runtime, cache and state (default target/capture-layers);
# the PNGs go to review/s42/.
set -euo pipefail
cd "$(dirname "$0")/.."
run=${RUN:-$PWD/target/capture-layers}
out=$PWD/review/s42
mkdir -p "$run/rt" "$run/cache" "$run/state" "$out"
chmod 700 "$run/rt"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
export XDG_RUNTIME_DIR="$run/rt" XDG_CACHE_HOME="$run/cache"
export OMASTORM_ARCHIVE=${OMASTORM_ARCHIVE:-$PWD/data/raw/radar_vara_qcvol_202609131055.h5}
: > "$run/config.toml"
export OMASTORM_CONFIG="$run/config.toml" OMASTORM_STATE="$run/state/state.json"
trap 'target/debug/omastorm-engine stop >/dev/null 2>&1 || true' EXIT
dark=$(mktemp -d "$run/theme-dark.XXXXXX")
light=$(mktemp -d "$run/theme-light.XXXXXX")
printf 'background = "#1a1b26"\nforeground = "#a9b1d6"\naccent = "#7aa2f7"\ncyan = "#7dcfff"\n' > "$dark/colors.toml"
printf 'mode = "light"\nbackground = "#f5f1e8"\nforeground = "#343a48"\naccent = "#365ba8"\ncyan = "#1f7a8c"\n' > "$light/colors.toml"

# name theme layers span lat lon [ipc...]: one capture; IPC calls (each a
# quoted "target function args") run once the window answers.
capture() {
  local name=$1 theme=$2 layers=$3 span=$4 lat=$5 lon=$6
  shift 6
  printf '{"lat":%s,"lon":%s,"span":%s,"lock":"vara"}\n' "$lat" "$lon" "$span" > "$run/state/state.json"
  OMASTORM_THEME_DIR=$([[ $theme == light ]] && echo "$light" || echo "$dark") \
    OMASTORM_LAYERS="$layers" OMASTORM_WIDTH=${WIDTH:-1100} OMASTORM_HEIGHT=${HEIGHT:-820} \
    OMASTORM_CAPTURE_DELAY=9000 OMASTORM_CAPTURE="$out/$name.png" \
    bash run.sh > "$run/$name.log" 2>&1 &
  # run.sh execs Quickshell, so the launcher's PID is the window's.
  local launcher=$! pid=$!
  for _ in {1..150}; do
    quickshell ipc --pid "$pid" call layers status > /dev/null 2>&1 && break
    sleep .1
  done
  # Wait for the stations (the first capture waits for the fetch).
  for _ in {1..60}; do
    [[ $(quickshell ipc --pid "$pid" call layers status 2>/dev/null) == *'"stations":-1'* ]] || break
    sleep .1
  done
  for call in "$@"; do
    # shellcheck disable=SC2086
    quickshell ipc --pid "$pid" call $call > /dev/null
    sleep .3
  done
  echo "$name: $(quickshell ipc --pid "$pid" call layers status 2>/dev/null)"
  wait "$launcher"
}

for theme in dark light; do
  capture "$theme-temp" "$theme" radar,temp 700 58.6 14.0
  capture "$theme-wind" "$theme" radar,wind 700 58.6 14.0
  capture "$theme-both" "$theme" radar,temp,wind 700 58.6 14.0
  capture "$theme-panel" "$theme" radar,temp,wind 700 58.6 14.0 "layers panel true"
  capture "$theme-out" "$theme" radar,temp,wind 2200 61.5 17.0
  capture "$theme-in" "$theme" radar,temp,wind 160 58.3 12.9
done
capture dark-noradar dark temp,wind 700 58.6 14.0

# A station's hover card, through the harness's real pointer events: the
# pointer rests on the first station shown near the map's middle.
OMASTORM_QML=$(TMPDIR="$run" scripts/capture-harness.sh --input)
export OMASTORM_QML
printf '{"lat":58.3,"lon":13.2,"span":400,"lock":"vara"}\n' > "$run/state/state.json"
OMASTORM_THEME_DIR="$dark" OMASTORM_LAYERS=radar,temp,wind OMASTORM_WIDTH=1100 OMASTORM_HEIGHT=820 \
  OMASTORM_CAPTURE_DELAY=9000 OMASTORM_CAPTURE="$out/dark-hover.png" bash run.sh > "$run/hover.log" 2>&1 &
pid=$!
for _ in {1..150}; do quickshell ipc --pid "$pid" call layers status > /dev/null 2>&1 && break; sleep .1; done
sleep 1.5
map=$(quickshell ipc --pid "$pid" call input where map)
ids=$(quickshell ipc --pid "$pid" call layers shownIds)
best=""
for id in $(jq -r '.[]' <<< "$ids"); do
  m=$(quickshell ipc --pid "$pid" call layers mark "$id")
  [[ -z $m ]] && continue
  x=$(jq .x <<< "$m"); y=$(jq .y <<< "$m")
  if (( x > 380 && x < 620 && y > 180 && y < 420 )); then best=$m; break; fi
done
if [[ -n $best ]]; then
  quickshell ipc --pid "$pid" call input move "$(( $(jq .x <<< "$map") + $(jq .x <<< "$best") ))" "$(( $(jq .y <<< "$map") + $(jq .y <<< "$best") ))"
fi
wait "$pid"
unset OMASTORM_QML
echo "Captures in $out"
