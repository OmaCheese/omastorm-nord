#!/usr/bin/env bash
# S35: the segmented loading bar, stage by stage, as review/stages-*.png and
# one sheet, review/stages-sheet.png.
#
# A load's stages cannot be scheduled on the real feed — the interesting
# moments are seconds long and come in one order — so this uses the harness
# copy of the shell (scripts/capture-harness.sh): the real UI files, over a
# real state whose `loading` is replaced. The daemon runs in a network
# namespace with no interfaces over a **copy** of the cache, so the whole
# sheet costs zero requests and never touches the live cache.
#
# The four moments are the ones §S35 is about: the first stage well short of
# the end and naming what it waits for; the build, which before S35 hid
# behind "99 %"; the backfill; and an engine older than S35 that sends no
# `stages`, where the same bar falls back to the single one S31 drew.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p review
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
site="${SITE:-vara}"
review="$PWD/review"
rm -f "$review"/stages-*.png
scratch=$(mktemp -d /tmp/omastorm-stages.XXXXXX)
harness_dir=
trap 'rm -rf "$scratch" ${harness_dir:+"$harness_dir"}' EXIT
mkdir -p "$scratch/rt" "$scratch/cache"
config="$scratch/config.toml"
jq -r --arg id "$site" '.sites[] | select(.id==$id) | "center_lat = \(.lat)\ncenter_lon = \(.lon)\nlocked_radar = \"\(.id)\""' engine/data/sites.json > "$config"

capture() { # name, delay ms, env...
  local name=$1 delay=$2
  shift 2
  env "$@" OMASTORM_WIDTH=960 OMASTORM_HEIGHT=680 OMASTORM_CAPTURE_DELAY="$delay" OMASTORM_CAPTURE="$review/stages-$name.png" bash run.sh > "$scratch/capture-$name.log" 2>&1 || true
  [[ -s "$review/stages-$name.png" ]] || { echo "No capture for $name" >&2; exit 1; }
  echo "captured $name"
}

cp -r "${XDG_CACHE_HOME:-$HOME/.cache}/omastorm-se" "$scratch/cache/"
# No interfaces: the poller reaches nothing, so the sheet costs no request.
XDG_RUNTIME_DIR="$scratch/rt" XDG_CACHE_HOME="$scratch/cache" \
  unshare -rn target/debug/omastorm-engine serve > "$scratch/rt/engine.log" 2>&1 &
for _ in $(seq 100); do [[ -S "$scratch/rt/omastorm-se/engine.sock" ]] && break; sleep .1; done
[[ -S "$scratch/rt/omastorm-se/engine.sock" ]] || { echo "Scratch daemon did not start" >&2; cat "$scratch/rt/engine.log" >&2; exit 1; }

harness=$(bash scripts/capture-harness.sh)
harness_dir=$(dirname "$harness")
shot() { # name, loading JSON
  capture "$1" 6000 XDG_RUNTIME_DIR="$scratch/rt" XDG_CACHE_HOME="$scratch/cache" \
    OMASTORM_QML="$harness" OMASTORM_CONFIG="$config" \
    OMASTORM_STATE_OVERRIDE="{\"loading\":$2}"
}

# The first stage, 33 of 39 radars in, naming the tail (S35 item 4). The
# build and the backfill are dim: their work has not started.
shot first '{"stage":"first","percent":81,"done":150,"total":185,"unit":"volumes", "label":"Nordic Rain mass: 33 of 39 radars in for 16:40Z (2 silent), waiting for Ängelholm, Bålsta, Hemse and 3 more", "stages":[{"stage":"first","share":55,"percent":81,"done":150,"total":185,"unit":"volumes","state":"active"}, {"stage":"build","share":20,"percent":0,"done":0,"total":0,"unit":"steps","state":"waiting"}, {"stage":"history","share":25,"percent":0,"done":0,"total":0,"unit":"volumes","state":"waiting"}]}'

# The build: the ten seconds S31 drew as "99 %". The first segment stays
# full behind it; the backfill is still dim.
shot build '{"stage":"build","percent":50,"done":1,"total":2,"unit":"steps", "label":"Nordic Rain mass: drawing 16:40Z", "stages":[{"stage":"first","share":55,"percent":100,"done":185,"total":185,"unit":"volumes","state":"done"}, {"stage":"build","share":20,"percent":50,"done":1,"total":2,"unit":"steps","state":"active"}, {"stage":"history","share":25,"percent":0,"done":0,"total":0,"unit":"volumes","state":"waiting"}]}'

# The backfill, with both stages behind it full.
shot history '{"stage":"history","percent":33,"done":41,"total":123,"unit":"volumes", "label":"Nordic Rain mass: history 1 of 3 frames (2 silent)", "stages":[{"stage":"first","share":55,"percent":100,"done":185,"total":185,"unit":"volumes","state":"done"}, {"stage":"build","share":20,"percent":100,"done":2,"total":2,"unit":"steps","state":"done"}, {"stage":"history","share":25,"percent":33,"done":41,"total":123,"unit":"volumes","state":"active"}]}'

# An engine older than S35: no `stages`, so the bar is the single one S31
# drew, full width, from the top-level percent.
shot s31 '{"stage":"first","percent":63,"done":52,"total":82,"unit":"volumes", "label":"Nordic Rain mass: 26 of 40 radars in for 14:25Z (1 silent)"}'

jobs -p | xargs -r kill 2>/dev/null || true

cd "$review"
magick montage -label '%t' stages-first.png stages-build.png stages-history.png stages-s31.png \
  -tile 2x -geometry 960x680+10+14 -background '#181414' -fill '#e6d9db' -pointsize 22 stages-sheet.png
echo "review/stages-sheet.png"
