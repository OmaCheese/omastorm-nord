#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# check.sh gives each lane a scratch XDG_RUNTIME_DIR under target/check. Run
# alone, this makes its own, so the daemon it starts and stops is never the
# bar's (the session's runtime dir holds the home engine).
own_runtime=
if [[ ${XDG_RUNTIME_DIR:-} != "$PWD"/target/* ]]; then
  own_runtime=$PWD/target/r-map-pick
  mkdir -p "$own_runtime" && chmod 700 "$own_runtime"
  mkdir -p "$own_runtime/cache" "$own_runtime/tmp"
  export XDG_RUNTIME_DIR=$own_runtime XDG_CACHE_HOME=$own_runtime/cache TMPDIR=$own_runtime/tmp
  export OMASTORM_ARCHIVE=${OMASTORM_ARCHIVE:-$PWD/data/raw/KTLX20130520_201643_V06.gz}
fi
# Harness outside the checkout: Omarchy rejects a shaders symlink in the plugin folder.
check_dir=$(mktemp -d -t omastorm-check-map-pick.XXXXXX)
cleanup() {
  rm -rf "$check_dir"
  if [[ -n $own_runtime ]]; then
    target/debug/omastorm-engine stop > /dev/null 2>&1 || true
    rm -rf "$own_runtime"
  fi
}
trap cleanup EXIT
mkdir -p review
rm -f review/map-pick-dark.png review/map-pick-light.png review/map-pick-dense.png
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
test -s review/map-pick-dense.png
if rg -q 'TypeError|ReferenceError|Unable to assign|Failed to create.*context' "$check_dir/result.log"; then exit 1; fi
