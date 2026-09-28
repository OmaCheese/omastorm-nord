#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
export OMASTORM_ROOT="$PWD"
# Plugin bootstrap: no build and no second Quickshell process. A checkout
# with a debug engine stays offline. Otherwise the pinned release installer
# fetches once, verifies the committed sha256, and installs under
# $XDG_DATA_HOME/omastorm-nord/bin (DESIGN.md, distribution).
if [[ ${1:-} == --ensure ]]; then
  # The plugin bootstrap runs detached; its stderr goes to the log it names,
  # in the private runtime dir. The dir is made owner-only, a symlinked or
  # foreign dir or log is refused, and the log is created afresh under
  # noclobber (O_EXCL), so a link planted at its name is never followed.
  if [[ -n ${OMASTORM_BOOTSTRAP_LOG:-} ]]; then
    log=$OMASTORM_BOOTSTRAP_LOG
    log_dir=$(dirname -- "$log")
    refuse_log() {
      printf 'Refusing the bootstrap log %s: %s. Remove it; the next try makes a new one.\n' "$1" "$2" >&2
      exit 1
    }
    [[ $log == /* ]] || { echo "OMASTORM_BOOTSTRAP_LOG is not an absolute path: $log" >&2; exit 1; }
    [[ -e $log_dir || -L $log_dir ]] || mkdir -m 700 -- "$log_dir"
    if [[ -L $log_dir ]]; then
      refuse_log "$log_dir" 'it is a symlink'
    elif [[ ! -d $log_dir ]]; then
      refuse_log "$log_dir" 'it is not a directory'
    elif [[ ! -O $log_dir ]]; then
      refuse_log "$log_dir" 'it is not owned by you'
    elif [[ -L $log ]]; then
      refuse_log "$log" 'it is a symlink'
    elif [[ -e $log && ! -f $log ]]; then
      refuse_log "$log" 'it is not a regular file'
    elif [[ -e $log && ! -O $log ]]; then
      refuse_log "$log" 'it is not owned by you'
    fi
    rm -f -- "$log"
    umask_was=$(umask)
    umask 077
    set -C
    exec 2> "$log"
    set +C
    umask "$umask_was"
  fi
  if [[ -x target/debug/omastorm-engine ]]; then
    exec target/debug/omastorm-engine ensure
  fi
  engine=$(bash scripts/fetch-engine.sh --print-path)
  exec "$engine" ensure
fi
if [[ ! -f ui/shaders/radar.frag.qsb || ! -f ui/shaders/tile.frag.qsb ]]; then
  echo 'Missing shader packages. Run bash scripts/build-shader.sh (see data/README.md).' >&2
  exit 1
fi
# Launch is strictly offline. Fetch build dependencies explicitly during setup.
bash scripts/cargo.sh build --offline --locked --quiet
target/debug/omastorm-engine ensure
# A tty launch names this checkout and which files apply, so a leftover
# archive daemon or the installed plugin is obvious. Captures are not a tty.
if [[ -t 1 ]]; then
  mode=live
  [[ -n ${OMASTORM_ARCHIVE:-} ]] && mode="archive $OMASTORM_ARCHIVE"
  config=${OMASTORM_CONFIG:-$HOME/.config/omastorm-nord/config.toml}
  if [[ -n ${OMASTORM_STATE:-} ]]; then
    state=$OMASTORM_STATE
  elif [[ -n ${OMASTORM_CONFIG:-} ]]; then
    state="(not read; OMASTORM_CONFIG is set)"
  else
    state=${XDG_STATE_HOME:-$HOME/.local/state}/omastorm-nord/state.json
  fi
  if [[ -n ${OMASTORM_LOCATION:-} ]]; then
    location=$OMASTORM_LOCATION
  elif [[ -n ${OMASTORM_CONFIG:-} ]]; then
    location="(not read; OMASTORM_CONFIG is set)"
  else
    location=$HOME/.local/state/omarchy/settings/weather.json
  fi
  bar=$(bash scripts/link-plugin.sh --status)
  printf 'Omastorm %s\n  qml    %s\n  engine %s\n  bar    %s\n  config %s\n  state  %s\n  place  %s\n' \
    "$PWD" "${OMASTORM_QML:-ui/shell.qml}" "$mode" "$bar" "$config" "$state" "$location"
fi
# mise start / restart / onboard restart the Omarchy shell when this checkout
# is linked, so the bar popover matches. Captures and checks leave it alone.
if [[ -n ${OMASTORM_RESCAN_PLUGIN:-} ]]; then
  bash scripts/link-plugin.sh --rescan
fi
exec quickshell -p "${OMASTORM_QML:-ui/shell.qml}" "$@"
