#!/usr/bin/env bash
# Clean-install rehearsal (P1): what `omarchy plugin add <fork> --enable` and
# the first popover do, in a throwaway HOME, against a LOCAL engine candidate.
#
#   1. git clone this repository into the throwaway HOME's plugins dir
#      ($XDG_CONFIG_HOME/omarchy/plugins/rb.omastorm-se), as the plugin
#      manager does, and write the candidate pin into the clone's
#      engine/release.pin, as P2's pin commit will;
#   2. refuse a substituted asset (sha256 mismatch, nothing installed);
#   3. run the plugin bootstrap exactly as ui/PluginSession.qml does
#      (env -C $HOME OMASTORM_BOOTSTRAP_LOG=... bash run.sh --ensure), with
#      OMASTORM_ENGINE_ASSET standing in for the GitHub download;
#   4. read hello (protocol v2, the candidate's version, 43 stations) and
#      require the installed binary to hash to the pin;
#   5. stop the engine, remove everything as README "Remove" says, and list
#      leftovers: in the throwaway dirs, as omastorm-engine processes, and as
#      new omastorm entries in the real HOME's XDG dirs and /run/user/<uid>.
#
# Usage: bash scripts/rehearse-install.sh [asset] [candidate-pin]
#   defaults: target/dist/omastorm-engine-<arch>-unknown-linux-gnu and
#   target/dist/release.pin (bash scripts/build-engine-release.sh).
#   REHEARSE_HOME (default /tmp/p1.home) and REHEARSE_RUNTIME (default
#   /tmp/p1.rt) must both be under /tmp; the real HOME is refused. Clones the
#   current branch's committed HEAD, so commit first. Nothing is uploaded or
#   downloaded, and ~/.config/omarchy, the real engines and systemd are never
#   touched. The engine starts live with no location for a few seconds.
set -euo pipefail
cd "$(dirname "$0")/.."
die() { printf 'rehearse-install: %s\n' "$@" >&2; exit 1; }

arch=$(uname -m)
asset=$(realpath -e "${1:-target/dist/omastorm-engine-$arch-unknown-linux-gnu}") || die 'No candidate asset; run bash scripts/build-engine-release.sh'
pin=$(realpath -e "${2:-target/dist/release.pin}") || die 'No candidate pin; run bash scripts/build-engine-release.sh'
home=$(realpath -m "${REHEARSE_HOME:-/tmp/p1.home}")
rt=$(realpath -m "${REHEARSE_RUNTIME:-/tmp/p1.rt}")
real_home=$(realpath -e "$(getent passwd "$(id -un)" | cut -d: -f6)")
uid=$(id -u)

# Refuse the real HOME, anything inside it, and anything outside /tmp.
for dir in "$home" "$rt"; do
  [[ $dir == /tmp/?* ]] || die "$dir is not under /tmp"
  [[ $dir != "$real_home" && $dir != "$real_home"/* ]] || die "$dir is the real HOME or inside it"
done
[[ $home != "$rt" && $home != "$rt"/* && $rt != "$home"/* ]] || die 'REHEARSE_HOME and REHEARSE_RUNTIME must not nest'
[[ $(realpath -m "$HOME") != "$home" ]] || die 'Run from a normal shell: HOME is already the rehearsal HOME'

src=$(git rev-parse --path-format=absolute --git-common-dir)
branch=$(git branch --show-current)
[[ -n $branch ]] || die 'Detached HEAD; check out a branch'
version=$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)
protocol=$(rg -o 'message\.v !== ([0-9]+)' -r '$1' ui/Engine.qml)
want_sites=${REHEARSE_SITES:-43}
source scripts/engine-pin.sh
read_engine_pin "$pin"
pin_sha=${hashes[$(engine_machine "$arch")]:-}
[[ -n $pin_sha ]] || die "Candidate pin has no $arch asset: $pin"
[[ $tag == "engine-$version" ]] || die "Candidate pin names $tag; Cargo.toml is $version"

# Leftover detection outside /tmp: omastorm entries (names only) in the real
# XDG dirs and the real runtime dir, before and after. The human's own
# engines write inside their existing dirs and never add a top-level name.
real_dirs=("$real_home/.config" "$real_home/.local/share" "$real_home/.local/state"
  "$real_home/.cache" "$real_home/.local/share/applications" "$real_home/.config/omarchy/plugins"
  "/run/user/$uid")
snapshot() {
  for d in "${real_dirs[@]}"; do
    [[ -d $d ]] && find "$d" -maxdepth 1 -iname '*omastorm*' 2>/dev/null
  done | sort
  # Every omastorm-engine process with its executable (exact name, no -f).
  pgrep -x omastorm-engine | while read -r p; do
    printf 'pid %s %s\n' "$p" "$(readlink "/proc/$p/exe" 2>/dev/null || echo '?')"
  done | sort
}
before=$(snapshot)

rm -rf -- "$home" "$rt"
mkdir -p -- "$home" "$rt/tmp"
chmod 700 -- "$rt"
# A clean login's environment: no OMASTORM_* override leaks in.
for v in "${!OMASTORM_@}"; do unset "$v"; done
export HOME=$home XDG_CONFIG_HOME=$home/.config XDG_DATA_HOME=$home/.local/share
export XDG_CACHE_HOME=$home/.cache XDG_STATE_HOME=$home/.local/state
export XDG_RUNTIME_DIR=$rt TMPDIR=$rt/tmp
plugin=$XDG_CONFIG_HOME/omarchy/plugins/rb.omastorm-se
engine=$XDG_DATA_HOME/omastorm-se/bin/omastorm-engine
sock=$XDG_RUNTIME_DIR/omastorm-se/engine.sock
bootstrap_log=$XDG_RUNTIME_DIR/omastorm-se/bootstrap.log
stop_engine() { [[ -x $engine ]] && "$engine" stop >/dev/null 2>&1 || true; }
trap stop_engine EXIT

step() { printf '== %s\n' "$*"; }
step "clone $src ($branch) -> $plugin"
mkdir -p -- "$(dirname "$plugin")"
git clone -q --no-local --branch "$branch" -- "$src" "$plugin"
printf '   HEAD %s, %s tracked files, %s\n' "$(git -C "$plugin" rev-parse --short HEAD)" \
  "$(git -C "$plugin" ls-files | wc -l)" "$(du -sh "$plugin" | cut -f1)"
[[ ! -e $plugin/target ]] || die 'The clone has a target/; --ensure would not take the release path'
# P2 commits the verified pin; the rehearsal writes the candidate in its place.
install -m 644 -- "$pin" "$plugin/engine/release.pin"

step 'substituted asset is refused'
printf 'not-the-engine\n' > "$rt/tmp/bogus"
if OMASTORM_ENGINE_ASSET=$rt/tmp/bogus bash "$plugin/scripts/fetch-engine.sh" 2> "$rt/tmp/bogus.err"; then
  die 'fetch-engine.sh accepted a sha256 mismatch'
fi
rg -q 'sha256 mismatch' "$rt/tmp/bogus.err" || die "unclear refusal: $(cat "$rt/tmp/bogus.err")"
[[ ! -e $engine ]] || die 'A refused asset was installed'
echo '   refused, nothing installed'

step 'plugin bootstrap: run.sh --ensure (fetch-engine from the local candidate)'
env -C "$HOME" OMASTORM_ENGINE_ASSET="$asset" OMASTORM_BOOTSTRAP_LOG="$bootstrap_log" \
  bash "$plugin/run.sh" --ensure || die "bootstrap failed: $(cat "$bootstrap_log" 2>/dev/null)"
[[ -x $engine ]] || die "No engine at $engine"
got=$(sha256sum -- "$engine" | awk '{print $1}')
[[ $got == "$pin_sha" ]] || die "Installed engine $got does not match the pin $pin_sha"
printf '   installed %s\n   sha256 %s (matches the pin)\n' "$engine" "$got"

step 'hello'
hello=
for _ in {1..20}; do
  hello=$(timeout 2 socat -t0.2 - "UNIX-CONNECT:$sock" < /dev/null 2>/dev/null | head -n1 || true)
  [[ -n $hello ]] && break
  sleep .25
done
jq -e --arg version "$version" --argjson v "$protocol" --argjson n "$want_sites" \
  '.type == "hello" and .v == $v and .engine == $version and (.sites | length) == $n' <<< "$hello" > /dev/null \
  || die "hello does not match v$protocol / $version / $want_sites stations: ${hello:0:300}"
jq -r '"   v\(.v) engine \(.engine), \(.sites | length) stations: " + ([.sites[] | .provider] | group_by(.) | map("\(.[0]) \(length)") | join(", "))' <<< "$hello"
running=$(pgrep -x omastorm-engine | while read -r p; do [[ $(readlink "/proc/$p/exe" 2>/dev/null) == "$engine" ]] && echo "$p"; done || true)
printf '   engine pid %s\n' "${running:-none}"

step 'stop and remove (README "Remove")'
"$engine" stop > /dev/null
for _ in {1..20}; do
  pgrep -x omastorm-engine | while read -r p; do readlink "/proc/$p/exe" 2>/dev/null; done | rg -qxF "$engine" || break
  sleep .25
done
rm -rf -- "$plugin"
rm -rf -- "$XDG_DATA_HOME/omastorm-se" "$XDG_CACHE_HOME/omastorm-se" "$XDG_STATE_HOME/omastorm-se"
rm -rf -- "$XDG_CONFIG_HOME/omastorm-se"
rm -f -- "$XDG_DATA_HOME/applications/omastorm-se.desktop"
trap - EXIT

step 'leftovers'
indent() { while IFS= read -r line; do printf '     %s\n' "$line"; done <<< "$1"; }
# Inside the throwaway dirs: files only (empty parent dirs are expected).
inside=$(find "$home" "$rt" -mindepth 1 ! -type d ! -path "$rt/tmp/*" 2>/dev/null || true)
if [[ -n $inside ]]; then printf '   in the throwaway dirs (removed next):\n'; indent "$inside"; else echo '   throwaway dirs: none'; fi
after=$(snapshot)
ours=$(pgrep -x omastorm-engine | while read -r p; do e=$(readlink "/proc/$p/exe" 2>/dev/null || true); [[ $e == "$home"/* ]] && echo "pid $p $e"; done || true)
outside=$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | rg -v '^pid ' || true)
gone=$(comm -23 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | rg '^pid ' || true)
status=0
if [[ -n $outside ]]; then printf '   OUTSIDE /tmp:\n'; indent "$outside"; status=1; else echo '   outside /tmp: none'; fi
if [[ -n $ours ]]; then printf '   rehearsal engine still running: %s\n' "$ours"; status=1; else echo '   rehearsal engine: stopped'; fi
if [[ -n $gone ]]; then printf '   an engine running before the rehearsal is gone:\n'; indent "$gone"; status=1; else echo '   engines running before: untouched'; fi
rm -rf -- "$home" "$rt"
(( status == 0 )) || die 'leftovers found'
echo "Rehearsal PASS: clone, refuse, bootstrap install ($tag, sha256 ${pin_sha:0:12}…), hello v$protocol, $want_sites stations, clean removal"
