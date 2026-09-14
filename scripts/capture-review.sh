#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p review
export OMASTORM_ARCHIVE=${OMASTORM_ARCHIVE:-$PWD/data/raw/radar_vara_qcvol_202609131055.h5} # the vendored Vara scan; the checks keep KTLX (DEC-10)
export QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic
export QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl
scratch=$(mktemp -d /tmp/omastorm-review.XXXXXX)
export XDG_RUNTIME_DIR="$scratch/runtime" XDG_CACHE_HOME="$scratch/cache"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CACHE_HOME"
trap 'target/debug/omastorm-engine stop >/dev/null 2>&1 || true' EXIT
for spec in 'minimum 360 360 PIXELS' 'compact 400 420 PIXELS' 'quarter 960 680 PIXELS' 'half 960 1200 PIXELS' 'full 1920 1200 PIXELS' 'glyphs 960 680 GLYPHS' 'stipple 960 680 STIPPLE'; do
  read -r name width height treatment <<< "$spec"
  OMASTORM_WIDTH="$width" OMASTORM_HEIGHT="$height" OMASTORM_STYLE="$treatment" OMASTORM_CAPTURE="$PWD/review/$name.png" bash run.sh
done

# Change only temporary inputs while the SAME Quickshell instance runs.
review_theme_dir=$(mktemp -d /tmp/omastorm-theme.XXXXXX)
printf 'background = "#1a1b26"\nforeground = "#a9b1d6"\naccent = "#7aa2f7"\n' > "$review_theme_dir/colors.toml"
OMASTORM_THEME_DIR="$review_theme_dir" OMASTORM_WIDTH=960 OMASTORM_HEIGHT=680 OMASTORM_CAPTURE_DELAY=5500 OMASTORM_CAPTURE="$PWD/review/theme-change.png" bash run.sh &
review_capture_pid=$!
sleep 1
printf 'background = "#f5f1e8"\nforeground = "#343a48"\naccent = "#365ba8"\n' > "$review_theme_dir/colors.toml"
wait "$review_capture_pid"
# Pixel checks use ImageMagick so the capture review needs no Python.
corner=$(magick review/theme-change.png -format '%[pixel:p{0,0}]' info:)
[[ "$corner" == *'(245,241,232'* ]] || { echo "Theme change not applied: corner is $corner" >&2; exit 1; }
# Crop to the map so legend swatches cannot stand in for the radar: the shader
# must paint the same colours the legend draws. Both are tinted by the theme,
# so the swatches are read back from the capture rather than hard-coded; any
# band will do, since which bands a scan fills depends on the weather in it.
histogram=$(magick review/theme-change.png -crop 900x430+20+120 +repage -define histogram:unique-colors=true -format %c histogram:info:-)
found=
for band in $(seq 1 11); do
  x=$(( 20 + 920 * (2 * band + 1) / 24 )) # the band's centre in the 12-band legend strip
  swatch=$(magick review/theme-change.png -format "%[fx:round(255*p{$x,597}.r)],%[fx:round(255*p{$x,597}.g)],%[fx:round(255*p{$x,597}.b)]" info:)
  # Histogram entries read (r,g,b) or, for a capture with alpha, (r,g,b,a).
  if grep -Eq "\($swatch(,[0-9]+)?\)" <<< "$histogram"; then found=$band; break; fi
done
[[ -n $found ]] || { echo 'No legend swatch colour on the map after theme change' >&2; exit 1; }
echo 'Live theme change and fixed radar palette: PASS'
