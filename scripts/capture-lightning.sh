#!/usr/bin/env bash
# S44's captures: lightning strikes in the real window, dark and light, from
# the vendored storm hour (SMHI, 5 July 2026 11-12Z) replayed so its newest
# strike falls at the engine's start (OMASTORM_LIGHTNING_REPLAY), over the
# live Vara radar: the live trail fading over 30 minutes; the loop stepped
# back 15, 30 and 45 minutes (the strikes follow the frame's time); zoomed
# in (ground crosses vs cloud dots); the LAYERS panel. The layers are forced
# with OMASTORM_LAYERS so no layers.json is read or written.
#
# One engine serves every capture (the replay's "now" is its start). The
# radar is live, so the first run backfills Vara's history. RUN names a
# directory for the runtime, cache and state (default target/capture-lightning);
# the PNGs go to review/s44/.
set -euo pipefail
cd "$(dirname "$0")/.."
run=${RUN:-$PWD/target/capture-lightning}
out=$PWD/review/s44
mkdir -p "$run/rt" "$run/cache" "$run/state" "$out"
chmod 700 "$run/rt"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
export XDG_RUNTIME_DIR="$run/rt" XDG_CACHE_HOME="$run/cache"
unset OMASTORM_ARCHIVE
export OMASTORM_LIGHTNING_REPLAY=${OMASTORM_LIGHTNING_REPLAY:-$PWD/data/fixtures/lightning/smhi_lightning_20260705T11Z.json.gz}
: > "$run/config.toml"
export OMASTORM_CONFIG="$run/config.toml" OMASTORM_STATE="$run/state/state.json"
trap 'target/debug/omastorm-engine stop >/dev/null 2>&1 || true' EXIT
dark=$(mktemp -d "$run/theme-dark.XXXXXX")
light=$(mktemp -d "$run/theme-light.XXXXXX")
printf 'background = "#1a1b26"\nforeground = "#a9b1d6"\naccent = "#7aa2f7"\ncyan = "#7dcfff"\n' > "$dark/colors.toml"
printf 'mode = "light"\nbackground = "#f5f1e8"\nforeground = "#343a48"\naccent = "#365ba8"\ncyan = "#1f7a8c"\n' > "$light/colors.toml"
# Frames the loop captures need behind the newest.
need=${NEED_FRAMES:-10}

# name theme layers span lat lon [ipc...]: one capture; IPC calls (each a
# quoted "target function args") run once the strikes and frames are in.
capture() {
  local name=$1 theme=$2 layers=$3 span=$4 lat=$5 lon=$6
  shift 6
  printf '{"lat":%s,"lon":%s,"span":%s,"lock":"vara"}\n' "$lat" "$lon" "$span" > "$run/state/state.json"
  OMASTORM_THEME_DIR=$([[ $theme == light ]] && echo "$light" || echo "$dark") \
    OMASTORM_LAYERS="$layers" OMASTORM_WIDTH=${WIDTH:-1100} OMASTORM_HEIGHT=${HEIGHT:-820} \
    OMASTORM_CAPTURE_DELAY=${DELAY:-12000} OMASTORM_CAPTURE="$out/$name.png" \
    bash run.sh > "$run/$name.log" 2>&1 &
  local pid=$!
  for _ in {1..150}; do
    quickshell ipc --pid "$pid" call layers lightning > /dev/null 2>&1 && break
    sleep .1
  done
  # The strikes and enough of the radar's history.
  [[ $layers == *lightning* ]] && for _ in {1..80}; do
    local s
    s=$(quickshell ipc --pid "$pid" call layers lightning 2>/dev/null || true)
    [[ $s == *'"count":0'* || $s != *'"count"'* ]] && { sleep .1; continue; }
    (( $(jq .frames <<< "$s") >= need )) && break
    sleep .1
  done
  # The paused position is the engine's, shared: start from the newest.
  quickshell ipc --pid "$pid" call keys run newest > /dev/null
  sleep .4
  for call in "$@"; do
    # shellcheck disable=SC2086
    quickshell ipc --pid "$pid" call $call > /dev/null
    sleep .4
  done
  sleep 1
  echo "$name: $(quickshell ipc --pid "$pid" call layers lightning 2>/dev/null) $(quickshell ipc --pid "$pid" call layers status 2>/dev/null | jq -r .status)"
  wait "$pid"
}

back() { local n=$1 out=(); for ((i = 0; i < n; i++)); do out+=("keys run previous_frame"); done; printf '%s\n' "${out[@]}"; }

# Warm up: the first window starts the engine and backfills Vara.
DELAY=${WARM:-40000} capture warmup dark radar,lightning 700 58.9 15.3
for theme in dark light; do
  capture "$theme-live" "$theme" radar,lightning 700 58.9 15.3
  capture "$theme-panel" "$theme" radar,lightning 700 58.9 15.3 "layers panel true"
  mapfile -t b3 < <(back 3); capture "$theme-loop-15" "$theme" radar,lightning 700 58.9 15.3 "${b3[@]}"
  mapfile -t b6 < <(back 6); capture "$theme-loop-30" "$theme" radar,lightning 700 58.9 15.3 "${b6[@]}"
  mapfile -t b9 < <(back 9); capture "$theme-loop-45" "$theme" radar,lightning 700 58.9 15.3 "${b9[@]}"
  capture "$theme-in" "$theme" radar,lightning 160 59.85 17.9
done
capture dark-off dark radar 700 58.9 15.3
capture dark-noradar dark lightning 700 58.9 15.3
rm -f "$out/warmup.png"
echo "Captures in $out"
