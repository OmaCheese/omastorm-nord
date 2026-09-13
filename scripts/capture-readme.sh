#!/usr/bin/env bash
# Vara stills for the user-facing README: the window and the production
# popover card, over Västra Götaland. Archive mode on the vendored SMHI
# volume (data/raw/radar_vara_qcvol_202609131055.h5), so a capture never
# polls SMHI and always shows the same scan. C.UTF-8 gives the metric scale
# bar and a 24-hour clock whatever the host locale. Isolated daemon and
# cache; the shared daemon is left alone.
set -euo pipefail
cd "$(dirname "$0")/.."
site=vara
volume=$PWD/data/raw/radar_vara_qcvol_202609131055.h5
[[ -s $volume ]] || { echo "Missing $volume (bash scripts/extract-fixtures.sh)" >&2; exit 1; }
scratch=$(mktemp -d /tmp/omastorm-readme-capture.XXXXXX)
export XDG_RUNTIME_DIR="$scratch/runtime" XDG_CACHE_HOME="$scratch/cache"
export OMASTORM_ROOT="$PWD" OMASTORM_CONFIG="$scratch/config.toml" OMASTORM_ARCHIVE="$volume"
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic
export QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl LC_ALL=C.UTF-8
mkdir -p "$XDG_RUNTIME_DIR" docs/media review
# Centre only, no locked_radar: a lock sends select_site, and the engine
# would leave the archived volume to go live on SMHI.
jq -r --arg id "$site" '.sites[] | select(.id==$id) | "center_lat = \(.lat)\ncenter_lon = \(.lon)"' engine/data/sites.json > "$OMASTORM_CONFIG"
[[ -s $OMASTORM_CONFIG ]] || { echo "No site $site in engine/data/sites.json" >&2; exit 1; }
pid=
cleanup() {
  [[ -z $pid ]] || kill "$pid" 2>/dev/null || true
  target/debug/omastorm-engine stop >/dev/null 2>&1 || true
}
trap cleanup EXIT

OMASTORM_WIDTH=1200 OMASTORM_HEIGHT=800 OMASTORM_STYLE=GLYPHS \
  OMASTORM_CAPTURE_DELAY=8000 OMASTORM_CAPTURE="$PWD/docs/media/window-live.png" \
  bash run.sh > "$scratch/window.log" 2>&1
[[ -s docs/media/window-live.png ]] || { cat "$scratch/window.log"; echo "No window-live.png" >&2; exit 1; }

# The production card on the same archived daemon, through the popover harness.
quickshell -p ui/PopoverHarness.qml > "$scratch/ui.log" 2>&1 &
pid=$!
call() { quickshell ipc --pid "$pid" call popover "$@"; }
ready=0
for _ in {1..120}; do
  if call status 2>/dev/null | jq -e --arg id "$site" '.site == $id and .condition == "archived" and (.frame | contains("loading") | not)' >/dev/null 2>&1; then ready=1; break; fi
  sleep .5
done
[[ $ready == 1 ]] || { cat "$scratch/ui.log"; call status 2>/dev/null || true; echo "No archived $site frame within 60 s" >&2; exit 1; }
sleep 2
for treatment in GLYPHS PIXELS STIPPLE; do
  call treatment "$treatment"
  sleep .3
  path="$PWD/review/readme-popover-${treatment,,}.png"
  rm -f "$path"
  call capture "$path"
  for _ in {1..50}; do [[ -s $path ]] && break; sleep .1; done
  [[ -s $path ]] || { echo "No $path" >&2; exit 1; }
done
call quit
wait "$pid" || true
pid=
if rg 'Binding loop|ReferenceError|TypeError|Unable to assign|Failed to load' "$scratch/ui.log" "$scratch/window.log"; then exit 1; fi
cp review/readme-popover-glyphs.png docs/media/popover.png
echo 'docs/media/window-live.png · popover.png'
