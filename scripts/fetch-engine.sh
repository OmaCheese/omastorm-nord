#!/usr/bin/env bash
# Fetch the pinned GitHub Release asset, verify its committed sha256, and
# install it under $XDG_DATA_HOME/omastorm-nord/bin (DESIGN.md, distribution).
# Ordinary launch never calls this. Plugin bootstrap does, through
# `run.sh --ensure`, only when the checkout has no debug engine.
set -euo pipefail
cd "$(dirname "$0")/.."

die() { printf '%s\n' "$@" >&2; exit 1; }
source scripts/engine-pin.sh

# Bounds on the download, so a stalled or oversized reply can neither hang
# first launch nor fill the disk before the sha256 check. The largest
# published asset is about 15 MB (x86_64); 64 MiB leaves room for growth.
# curl 8.4 and later stop a reply that has no Content-Length at the cap as
# well (test-engine-pin.sh proves it); the size test after the download
# refuses what an older curl let through. No attempt runs longer than
# max_time and no retry starts after it; 600 s still fits the engine at
# 25 kB/s. OMASTORM_ENGINE_MAX_TIME shortens it for checks.
max_bytes=$((64 * 1024 * 1024))
connect_timeout=15
max_time=${OMASTORM_ENGINE_MAX_TIME:-600}
[[ $max_time =~ ^[1-9][0-9]{0,4}$ ]] || die "OMASTORM_ENGINE_MAX_TIME is not a number of seconds: $max_time"

machine=$(engine_machine "${OMASTORM_ENGINE_MACHINE:-$(uname -m)}")
pin_file=${OMASTORM_ENGINE_PIN:-engine/release.pin}
read_engine_pin "$pin_file"
asset=${assets[$machine]:-}
sha256=${hashes[$machine]:-}
[[ -n $asset && -n $sha256 ]] || die "No pinned $machine engine for $tag yet. Publish its release asset and add its checksum to $pin_file."

data_home=${XDG_DATA_HOME:-$HOME/.local/share}
dest_dir=$data_home/omastorm-nord/bin
dest=$dest_dir/omastorm-engine
# Provenance: run.sh executes $dest, so it must be a plain file of this
# user's, and this installer replaces only a file it put there. $record
# holds the sha256 of the engine it last installed; a file at $dest is
# replaced only while it still has that hash (ours, untouched), the pin's
# (bytes this installer verifies), or one of the pin's previous_ hashes
# (engines earlier pins installed, carried forward by pin-engine-release.sh).
# Anything else is someone's own file and is refused. Every install writes
# the record, and so does a launch that finds the pinned engine already
# there, which adopts an engine installed before the record existed.
record=$dest.sha256

hash_of() { sha256sum -- "$1" | awk '{print $1}'; }
size_of() { stat -Lc %s -- "$1"; }
refuse() {
  die "Refusing to replace $dest: $1. Remove it if you do not need it (rm -- '$dest'); Omastorm then installs its engine there on the next try."
}
check_dest() {
  if [[ -L $dest ]]; then
    refuse 'it is a symlink'
  elif [[ -e $dest && ! -f $dest ]]; then
    refuse 'it is not a regular file'
  elif [[ -e $dest && ! -O $dest ]]; then
    refuse 'it is not owned by you'
  fi
}
recorded() {
  if [[ -f $record && ! -L $record && -O $record ]]; then head -c 64 -- "$record"; fi
}
check_ours() { # dest is missing, or a regular file this installer put there
  local have
  check_dest
  [[ -e $dest ]] || return 0
  have=$(hash_of "$dest")
  [[ $have == "$sha256" || $have == "$(recorded)" || " ${previous[$machine]:-} " == *" $have "* ]] \
    || refuse "it is not the engine this installer put there (no matching sha256 in $record)"
}
# Temp and rename in the same directory: a symlink at $record is replaced,
# never written through, and -T refuses a directory there.
keep_record() {
  local tmp_record
  [[ $(recorded) == "$sha256" ]] && return 0
  tmp_record=$(mktemp -- "$dest_dir/.omastorm-engine.sha256.XXXXXX") || return 1
  if printf '%s\n' "$sha256" > "$tmp_record" && chmod 644 -- "$tmp_record" \
    && mv -fT -- "$tmp_record" "$record"; then
    return 0
  fi
  rm -f -- "$tmp_record"
  return 1
}
finish() {
  [[ ${1:-} == --print-path ]] && printf '%s\n' "$dest"
  exit 0
}
current() { [[ -f $dest && -x $dest && $(hash_of "$dest") == "$sha256" ]]; }

check_dest
if current; then
  # A record that cannot be written costs only a later replacement, never
  # this launch.
  keep_record 2>/dev/null || true
  finish "$@"
fi

# One installer at a time: the plugin retries the bootstrap every 20 s, and
# downloads racing each other on a slow link would all run into max_time.
mkdir -p -- "$dest_dir"
exec {lock}< "$dest_dir"
flock -n "$lock" || die "The engine download is already running; the engine starts once it is done."
check_ours
# The download that held the lock may have just installed the engine.
if current; then
  keep_record 2>/dev/null || true
  finish "$@"
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/omastorm-engine.XXXXXX")
stage=
trap 'rm -rf "$work"; [[ -z $stage ]] || rm -f -- "$stage"' EXIT
tmp=$work/$asset

if [[ -n ${OMASTORM_ENGINE_ASSET:-} ]]; then
  [[ -f $OMASTORM_ENGINE_ASSET ]] || die "OMASTORM_ENGINE_ASSET is not a file: $OMASTORM_ENGINE_ASSET"
  (( $(size_of "$OMASTORM_ENGINE_ASSET") <= max_bytes )) \
    || die "OMASTORM_ENGINE_ASSET is larger than the $((max_bytes >> 20)) MiB cap for the engine: $OMASTORM_ENGINE_ASSET"
  cp -- "$OMASTORM_ENGINE_ASSET" "$tmp"
else
  url=${OMASTORM_ENGINE_URL:-https://github.com/$repo/releases/download/$tag/$asset}
  rc=0
  curl -fsSL --retry 2 --retry-max-time "$max_time" --connect-timeout "$connect_timeout" \
    --max-time "$max_time" --max-filesize "$max_bytes" \
    -A "omastorm-nord/$tag (fork of https://omastorm.com)" -o "$tmp" -- "$url" || rc=$?
  if (( rc == 63 )); then
    die "Refusing $asset from $url: it is larger than the $((max_bytes >> 20)) MiB cap for the engine."
  elif (( rc != 0 )); then
    # The popover shows the log's last line: the diagnosis goes last.
    die "Maintainers: publish GitHub Release $tag on $repo with that asset matching $pin_file, and make the repository public so the asset is anonymous." \
      "From a checkout with Rust: bash scripts/cargo.sh build --locked && bash run.sh" \
      "Could not download $asset from $url (curl exit $rc; a download is given up after $max_time s). Check the network; the plugin tries again every 20 s."
  fi
fi
(( $(size_of "$tmp") <= max_bytes )) || die "Refusing $asset: it is larger than the $((max_bytes >> 20)) MiB cap for the engine."

got=$(hash_of "$tmp")
if [[ $got != "$sha256" ]]; then
  die "expected $sha256" \
    "got      $got" \
    "Engine sha256 mismatch for $asset: the committed pin is the source of truth, so a substituted asset is refused."
fi

# Checked again, since the download can take minutes. Staged beside dest so
# the replacement is one rename: a half-written dest is never executable as
# the engine, and a symlink planted at dest after the check is replaced, not
# followed.
check_ours
stage=$(mktemp -- "$dest_dir/.omastorm-engine.XXXXXX")
cp -- "$tmp" "$stage"
chmod 755 -- "$stage"
mv -fT -- "$stage" "$dest"
stage=
# Without the record the next pinned engine would be refused; every launch
# tries to write it again, so this one still starts.
keep_record || echo "Installed $dest but could not write $record yet." >&2

finish "$@"
