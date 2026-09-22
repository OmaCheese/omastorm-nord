#!/usr/bin/env bash
# S41: the load as the window shows it — the centred card with named steps
# while nothing is drawn, the timeline's bar with the step and the whole
# load's percentage once a frame is, the single bar an engine older than S35
# gets (no `stages`), the card an engine older than S31 gets (no `loading`,
# `connection.status` loading, so no number), and "Starting engine" when no
# engine answers at all. Each case is the harness shell over the archived
# KTLX scan with `loading` (and, for the card, an empty `frame.scanTime`)
# laid over its state (capture-harness.sh), read back through the window's
# `loading` IPC. Offline: the archive starts no poller.
set -euo pipefail
cd "$(dirname "$0")/.."
own_runtime=
if [[ ${XDG_RUNTIME_DIR:-} != "$PWD"/target/* ]]; then
  own_runtime=$PWD/target/r-loading
  mkdir -p "$own_runtime" && chmod 700 "$own_runtime"
  mkdir -p "$own_runtime/cache" "$own_runtime/tmp"
  export XDG_RUNTIME_DIR=$own_runtime XDG_CACHE_HOME=$own_runtime/cache TMPDIR=$own_runtime/tmp
fi
export OMASTORM_ARCHIVE=${OMASTORM_ARCHIVE:-$PWD/data/raw/KTLX20130520_201643_V06.gz}
check_dir="$PWD/target/check-loading"
mkdir -p "$check_dir"
: > "$check_dir/none.toml"
: > "$check_dir/log"
qml=$(bash scripts/capture-harness.sh --input)
pid=
fake=
cleanup() {
  [[ -z $pid ]] || { kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }
  [[ -z $fake ]] || kill "$fake" 2>/dev/null || true
fake=
  rm -rf "$(dirname "$qml")" "$check_dir/no-engine"
  if [[ -n $own_runtime ]]; then
    target/debug/omastorm-engine stop > /dev/null 2>&1 || true
    rm -rf "$own_runtime"
  fi
}
trap cleanup EXIT
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
fail() { printf '%s\n' "$@" >&2; cat "$check_dir/log" >&2; exit 1; }
expect() { [[ "$3" == "$2" ]] || fail "$1" "Expected: $2" "Actual:   $3"; }
call() { quickshell ipc --pid "$pid" call "$@"; }

# open OVERRIDE: the window over the lane's daemon with OVERRIDE laid over
# every state; waits for the first state to be read.
open() {
  OMASTORM_QML="$qml" OMASTORM_CONFIG="$check_dir/none.toml" OMASTORM_STATE="$check_dir/state.json" \
    OMASTORM_WIDTH=960 OMASTORM_HEIGHT=680 OMASTORM_STATE_OVERRIDE="$1" \
    bash run.sh >> "$check_dir/log" 2>&1 &
  pid=$!
  for _ in {1..150}; do [[ $(call loading status 2>/dev/null | jq -r ".busy or (.error | length > 0)" 2>/dev/null) == true ]] && break; sleep .1; done
  sleep .5
}
close() { kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; pid=; }
status() { call loading status | jq -c "$1"; }

live='"source":"live","connection":{"status":"ok"}'
made='"stages":[{"stage":"first","share":55,"percent":40,"state":"active"},{"stage":"build","share":20,"percent":0,"state":"waiting"},{"stage":"history","share":25,"percent":0,"state":"waiting"}]'

# 1. Nothing drawn yet (the placeholder frame), My mosaic's first stage at
#    40 %: the card, its steps, 55 × 40 / 100 = 22 % overall.
open "{$live,\"frame\":{\"scanTime\":\"\"},\"loading\":{\"stage\":\"first\",\"percent\":40,\"done\":2,\"total\":5,\"unit\":\"volumes\",\"label\":\"My mosaic, Lowest beam: 1 of 2 radars in for 18:30Z, waiting for Karlskrona\",$made}}"
expect 'Before a frame the card shows' true "$(status .card)"
expect 'The overall percentage is the shares'"'"' sum' 22 "$(status .percent)"
expect 'The steps by the work, drawing after the build' '["engine:done","first:active","build:waiting","draw:waiting","history:waiting"]' "$(status .steps)"
expect 'The running step by name' '"Fetching radar data"' "$(status .step)"
expect 'Its detail is the label past the load'"'"'s name' '"1 of 2 radars in for 18:30Z, waiting for Karlskrona"' "$(status .detail)"
expect 'The load'"'"'s name' '"My mosaic, Lowest beam"' "$(status .name)"
close

# 2. A frame on screen and the build running: no card, the bar, 55 + 10.
open "{$live,\"loading\":{\"stage\":\"build\",\"percent\":50,\"done\":1,\"total\":2,\"unit\":\"steps\",\"label\":\"My mosaic, Lowest beam: drawing 18:30Z\",\"stages\":[{\"stage\":\"first\",\"share\":55,\"percent\":100,\"state\":\"done\"},{\"stage\":\"build\",\"share\":20,\"percent\":50,\"state\":\"active\"},{\"stage\":\"history\",\"share\":25,\"percent\":0,\"state\":\"waiting\"}]}}"
for _ in {1..50}; do [[ $(status .drawn) == true ]] && break; sleep .1; done
expect 'A drawn frame folds the card away' false "$(status .card)"
expect 'The timeline bar shows' true "$(status .bar)"
expect 'Three segments' 3 "$(status .segments)"
expect 'Overall through the build' 65 "$(status .percent)"
expect 'The build by name' '"Engine: building the frame"' "$(status .step)"
close

# 3. An engine older than S35: no stages, one segment, its own percent.
open "{$live,\"loading\":{\"stage\":\"history\",\"percent\":63,\"done\":7,\"total\":11,\"unit\":\"frames\",\"label\":\"Vara Reflectivity 0.5°: history 7 of 11 frames\"}}"
expect 'No stages: one segment' 1 "$(status .segments)"
expect 'No stages: the top-level percent' 63 "$(status .percent)"
expect 'No stages: the stage still by name' '"Fetching history"' "$(status .step)"
expect 'No stages: the count' '"7 of 11 frames"' "$(status .detail)"
close

# 4. An engine older than S31: no loading, the status says loading, the
#    placeholder frame: the card, with no number to show.
open "{$live,\"connection\":{\"status\":\"loading\"},\"frame\":{\"scanTime\":\"\"},\"loading\":null}"
expect 'No loading: still the card' true "$(status .card)"
expect 'No loading: no bar' false "$(status .bar)"
expect 'No loading: fetching, then drawing' '["engine:done","first:active","draw:waiting"]' "$(status .steps)"
close

# 5. No engine at all (a runtime dir with none, and no bootstrap to start
#    one): Starting engine.
mkdir -p "$check_dir/no-engine" && chmod 700 "$check_dir/no-engine"
XDG_RUNTIME_DIR="$check_dir/no-engine" OMASTORM_ROOT=/nonexistent OMASTORM_QML="$qml" OMASTORM_CONFIG="$check_dir/none.toml" \
  OMASTORM_STATE="$check_dir/state.json" OMASTORM_WIDTH=960 OMASTORM_HEIGHT=680 \
  quickshell -p "$qml" >> "$check_dir/log" 2>&1 &
pid=$!
call() { XDG_RUNTIME_DIR="$check_dir/no-engine" quickshell ipc --pid "$pid" call "$@"; }
for _ in {1..100}; do call loading status > /dev/null 2>&1 && break; sleep .1; done
sleep .5
expect 'No engine: the card' true "$(status .card)"
expect 'No engine: starting it' '"Starting engine"' "$(status .step)"
close

# 6. An engine that answers with something unreadable (review SF3): the
#    error itself, not the card saying "Starting engine" forever.
mkdir -p "$check_dir/no-engine/omastorm-se"
socat UNIX-LISTEN:"$check_dir/no-engine/omastorm-se/engine.sock",fork SYSTEM:'echo not-json; sleep 30' &
fake=$!
XDG_RUNTIME_DIR="$check_dir/no-engine" OMASTORM_ROOT=/nonexistent OMASTORM_QML="$qml" OMASTORM_CONFIG="$check_dir/none.toml" \
  OMASTORM_STATE="$check_dir/state.json" OMASTORM_WIDTH=960 OMASTORM_HEIGHT=680 \
  quickshell -p "$qml" >> "$check_dir/log" 2>&1 &
pid=$!
for _ in {1..100}; do [[ $(status .error 2>/dev/null) == *Invalid* ]] && break; sleep .1; done
expect 'A bad message: no card' false "$(status .card)"
expect 'A bad message: not busy' false "$(status .busy)"
[[ $(status .error) == '"Invalid engine message: '* ]] || fail "A bad message is shown as itself" "Actual: $(status .error)"
close
kill "$fake" 2>/dev/null || true
fake=

if rg 'Binding loop|ReferenceError|TypeError|Unable to assign' "$check_dir/log"; then exit 1; fi
echo "Loading: card, bar, overall %, named steps, no-stages and no-loading fallbacks, no engine."
