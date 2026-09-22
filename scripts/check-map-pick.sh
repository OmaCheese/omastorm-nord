#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Harness outside the checkout: Omarchy rejects a shaders symlink in the plugin folder.
check_dir=$(mktemp -d -t omastorm-check-map-pick.XXXXXX)
trap 'rm -rf "$check_dir"' EXIT
mkdir -p review
rm -f review/map-pick-dark.png review/map-pick-light.png
cp ui/RadarMap.qml ui/Engine.qml "$check_dir/"
cp tests/map-pick.qml "$check_dir/shell.qml"
ln -sfn "$PWD/ui/shaders" "$check_dir/shaders"
OMASTORM_QML="$check_dir/shell.qml" OMASTORM_REVIEW="$PWD/review" \
 QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic QT_QUICK_BACKEND=rhi QSG_RHI_BACKEND=opengl \
 timeout 40 bash run.sh > "$check_dir/result.log" 2>&1
cat "$check_dir/result.log"
rg -q MAP_PICK_PASSED "$check_dir/result.log"
test -s review/map-pick-dark.png
test -s review/map-pick-light.png
if rg -q 'TypeError|ReferenceError|Unable to assign|Failed to create.*context' "$check_dir/result.log"; then exit 1; fi
