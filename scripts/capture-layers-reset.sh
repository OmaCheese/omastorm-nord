#!/usr/bin/env bash
# S47's desktop captures, dark and light, in the real window (offscreen):
# the weather layers loading (the timeline bar and the credit say "Fetching
# weather stations (n of 4 providers)"), all in with the wind numbers, the
# LAYERS panel with a failed provider (DMI error 503, Frost without a
# client ID), closer in, and Reset recovering a stuck load (SMHI's
# observations hang; Reset aborts the fetch and reads them again).
#
# Nothing leaves the machine: the engine and scripts/fake-weather.py run in
# a private network namespace (`unshare -rn`), the providers are the
# fake's fixtures, the radar's feed is unreachable. FRAMES_FROM may name an
# XDG_CACHE_HOME/omastorm-se of an earlier run whose frame catalog holds
# Vara frames, so the radar shows one (the load itself cannot finish
# offline, which is honest: after a Reset it says 0 %). RUN names the run
# directory (default target/capture-s47); the PNGs go to review/s47/.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
run=${RUN:-$root/target/capture-s47}
out=$root/review/s47
mkdir -p "$run" "$out"
[[ -x target/debug/omastorm-engine ]] || { echo "Build first: bash scripts/cargo.sh build --offline" >&2; exit 1; }
dark=$(mktemp -d "$run/theme-dark.XXXXXX")
light=$(mktemp -d "$run/theme-light.XXXXXX")
printf 'background = "#1a1b26"\nforeground = "#a9b1d6"\naccent = "#7aa2f7"\ncyan = "#7dcfff"\n' > "$dark/colors.toml"
printf 'mode = "light"\nbackground = "#f5f1e8"\nforeground = "#343a48"\naccent = "#365ba8"\ncyan = "#1f7a8c"\n' > "$light/colors.toml"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
ns="" pid=""
cleanup() {
  [[ -n $pid ]] && kill "$pid" 2>/dev/null || true
  [[ -n $ns ]] && { pkill -P "$ns" 2>/dev/null || true; kill "$ns" 2>/dev/null || true; }
}
trap cleanup EXIT

# fake FAIL SLOW: the fake's failing and slow providers from now on.
fake() {
  printf '%s\n' "$1" > "$R/port.fail"
  printf '%s\n' "$2" > "$R/port.slow"
  local p
  for p in $(ps -eo pid,args | awk -v f="$R/port" '$2=="python3" && $3 ~ /fake-weather.py$/ && $4==f {print $1}'); do
    kill -USR1 "$p"
  done
}
q() { quickshell ipc --pid "$pid" call "$@"; }
# until EXPR SECONDS: wait until `layers states` matches EXPR (a jq test).
until_states() {
  local i
  for ((i = 0; i < $2 * 4; i++)); do
    q layers states 2>/dev/null | jq -e "$1" > /dev/null && return 0
    sleep .25
  done
  echo "timed out waiting for $1" >&2
}
save() { q capture save "$out/$1.png"; sleep 1; echo "$1: $(q layers states | jq -c '{stations: .stations.text, grid: .grid.text, loads, radar}')"; }

for theme in dark light; do
  R=$run/$theme
  rm -rf "$R"
  mkdir -p "$R/rt/omastorm-se" "$R/cache/omastorm-se" "$R/config" "$R/state"
  chmod 700 "$R/rt"
  if [[ -n ${FRAMES_FROM:-} ]]; then cp -r "$FRAMES_FROM/frames" "$FRAMES_FROM/tilts" "$R/cache/omastorm-se/" 2>/dev/null || true; fi
  export XDG_RUNTIME_DIR=$R/rt XDG_CACHE_HOME=$R/cache XDG_CONFIG_HOME=$R/config
  export OMASTORM_ARCHIVE=$root/data/raw/radar_vara_qcvol_202609131055.h5 OMASTORM_LIGHTNING_BASE=http://127.0.0.1:9
  unset FROST_CLIENT_ID
  unshare -rn sh -c '
    ip link set lo up
    python3 "$1/scripts/fake-weather.py" "$2/port" --log "$2/requests.log" --fail dmi --slow smhi=3,grid=5 > "$2/fake.out" 2>&1 &
    for i in $(seq 50); do [ -f "$2/port" ] && break; sleep .1; done
    OMASTORM_OBS_BASE=http://127.0.0.1:$(cat "$2/port") exec "$1/target/debug/omastorm-engine" serve
  ' sh "$root" "$R" >> "$R/rt/omastorm-se/engine.log" 2>&1 &
  ns=$!
  for _ in {1..100}; do [[ -S $R/rt/omastorm-se/engine.sock ]] && break; sleep .1; done
  : > "$R/config.toml"
  printf '{"lat":61.0,"lon":15.5,"span":1300,"lock":"vara"}\n' > "$R/state/state.json"
  printf '{"radar":true,"temp":true,"wind":true,"source":"both","lightning":false}\n' > "$R/state/layers.json"
  OMASTORM_THEME_DIR=$([[ $theme == light ]] && echo "$light" || echo "$dark") \
    OMASTORM_CONFIG=$R/config.toml OMASTORM_STATE=$R/state/state.json OMASTORM_WIDTH=1100 OMASTORM_HEIGHT=820 \
    bash run.sh > "$R/window.log" 2>&1 &
  pid=$!
  for _ in {1..150}; do q layers status > /dev/null 2>&1 && break; sleep .1; done
  sleep 2
  save "$theme-loading"
  until_states '.loads == []' 40
  sleep 1
  save "$theme-loaded"
  q layers panel true; sleep .8
  save "$theme-panel-failed"
  q layers panel false
  q keys run zoom_in; q keys run zoom_in; q keys run zoom_in; sleep 2
  save "$theme-wind"
  q keys run zoom_out; q keys run zoom_out; q keys run zoom_out; sleep 1
  # Stuck: after a Reset, SMHI's observations hang (the fetch times out
  # only after 30 s); a second Reset, once it answers again, recovers.
  fake dmi smhi=60
  q layers reset
  sleep 9
  save "$theme-stuck"
  fake dmi ""
  sleep 2
  q layers reset
  sleep 1
  save "$theme-reset"
  until_states '.stations.state == "ok"' 20
  sleep 1
  save "$theme-recovered"
  echo "$theme: requests $(sort "$R/requests.log" | uniq -c | tr '\n' ' ')"
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  pid=""
  pkill -P "$ns" 2>/dev/null || true
  kill "$ns" 2>/dev/null || true
  wait "$ns" 2>/dev/null || true
  ns=""
done
echo "Captures in $out"
