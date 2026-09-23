#!/usr/bin/env bash
# S43's captures: the MET Nordic grid in the real window, dark and light,
# over the vendored Vara scan: grid temperature under the radar, grid wind,
# both sources (stations over the grid), the LAYERS panel with its FROM row,
# and the whole grid zoomed out. The layers are forced with OMASTORM_LAYERS
# (plus "grid" or "both" for the source), so no layers.json is read or
# written.
#
# The grid and the stations are live: one engine serves every capture, so
# the grid is fetched once an hour and each station provider once per
# 10 minutes however many captures run. RUN names a directory for the
# runtime, cache and state (default target/capture-grid); the PNGs go to
# review/s43/. ONLY=<glob> takes the captures it names alone.
set -euo pipefail
cd "$(dirname "$0")/.."
run=${RUN:-$PWD/target/capture-grid}
out=$PWD/review/s43
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
  # ONLY (a glob) takes some captures alone.
  # shellcheck disable=SC2053
  [[ -n ${ONLY:-} && $name != $ONLY ]] && return 0
  printf '{"lat":%s,"lon":%s,"span":%s,"lock":"vara"}\n' "$lat" "$lon" "$span" > "$run/state/state.json"
  OMASTORM_THEME_DIR=$([[ $theme == light ]] && echo "$light" || echo "$dark") \
    OMASTORM_LAYERS="$layers" OMASTORM_WIDTH=${WIDTH:-1100} OMASTORM_HEIGHT=${HEIGHT:-820} \
    OMASTORM_CAPTURE_DELAY=${DELAY:-12000} OMASTORM_CAPTURE="$out/$name.png" \
    bash run.sh > "$run/$name.log" 2>&1 &
  # run.sh execs Quickshell, so the launcher's PID is the window's.
  local launcher=$! pid=$!
  for _ in {1..150}; do
    quickshell ipc --pid "$pid" call layers status > /dev/null 2>&1 && break
    sleep .1
  done
  # Wait for the grid (the first capture waits for the fetch and drawing)
  # and, when they show, the stations.
  for _ in {1..90}; do
    local s
    s=$(quickshell ipc --pid "$pid" call layers status 2>/dev/null || true)
    if [[ $layers == *grid* || $layers == *both* ]] && [[ $s == *'"gridTime":""'* ]]; then sleep .1; continue; fi
    if [[ $layers != *grid* ]] && [[ $s == *'"stations":-1'* ]]; then sleep .1; continue; fi
    break
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
  capture "$theme-grid-temp" "$theme" radar,temp,grid 700 58.6 14.0
  capture "$theme-grid-wind" "$theme" radar,wind,grid 700 58.6 14.0
  capture "$theme-both" "$theme" radar,temp,wind,both 700 58.6 14.0
  capture "$theme-panel" "$theme" radar,temp,wind,both 700 58.6 14.0 "layers panel true"
  capture "$theme-out" "$theme" radar,temp,wind,grid 3200 63.0 15.0
done
capture dark-in dark radar,temp,wind,both 160 58.3 12.9
echo "Captures in $out"
