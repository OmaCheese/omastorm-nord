#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
export OMASTORM_ROOT="$PWD"
# The plugin bootstrap's log, named by ui/PluginSession.qml, lives in the
# private runtime dir, in a dir made owner-only. A symlinked or foreign dir
# or log is refused, for writing (--ensure) and for the popover's read
# (--bootstrap-log) alike.
log=${OMASTORM_BOOTSTRAP_LOG:-}
log_dir=$(dirname -- "$log")
log_refusal() { # why the log must not be used, if it must not
  if [[ $log != /* ]]; then echo "$log: it is not an absolute path"
  elif [[ -L $log_dir ]]; then echo "$log_dir: it is a symlink"
  elif [[ ! -d $log_dir ]]; then echo "$log_dir: it is not a directory"
  elif [[ ! -O $log_dir ]]; then echo "$log_dir: it is not owned by you"
  elif [[ -L $log ]]; then echo "$log: it is a symlink"
  elif [[ -e $log && ! -f $log ]]; then echo "$log: it is not a regular file"
  elif [[ -e $log && ! -O $log ]]; then echo "$log: it is not owned by you"
  fi
}
refuse_log() { printf 'Refusing the bootstrap log %s. Remove it; the next try makes a new one.\n' "$1"; exit 1; }
if [[ ${1:-} == --bootstrap-log ]]; then
  # The popover's read, on stdout: the last 4 KiB (it shows the last line),
  # or why the log is refused; nothing before the first bootstrap.
  [[ -e $log || -L $log ]] || exit 0
  why=$(log_refusal)
  [[ -z $why ]] || refuse_log "$why"
  exec tail -c 4096 -- "$log"
fi
# Plugin bootstrap: no build and no second Quickshell process. A checkout
# with a debug engine stays offline. Otherwise the pinned release installer
# fetches once, verifies the committed sha256, and installs under
# $XDG_DATA_HOME/omastorm-nord/bin (DESIGN.md, distribution).
if [[ ${1:-} == --ensure ]]; then
  # The plugin bootstrap runs detached; its stderr goes to the log, created
  # afresh under noclobber (O_EXCL), so a link planted at its name is never
  # followed. One bootstrap at a time, locked before the log is touched: the
  # plugin retries every 20 s, and a retry that replaced the log would hide
  # a slow download's outcome. The daemon must not inherit the lock.
  lock=
  if [[ -n $log ]]; then
    [[ $log != /* || -e $log_dir || -L $log_dir ]] || mkdir -m 700 -- "$log_dir"
    why=$(log_refusal)
    [[ -z $why ]] || refuse_log "$why" >&2
    exec {lock}< "$log_dir"
    flock -n "$lock" || exit 0
    rm -f -- "$log"
    umask_was=$(umask)
    umask 077
    set -C
    exec 2> "$log"
    set +C
    umask "$umask_was"
  fi
  if [[ -x target/debug/omastorm-engine ]]; then
    [[ -z $lock ]] || exec {lock}<&-
    exec target/debug/omastorm-engine ensure
  fi
  engine=$(bash scripts/fetch-engine.sh --print-path)
  [[ -z $lock ]] || exec {lock}<&-
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
