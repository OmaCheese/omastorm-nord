#!/usr/bin/env bash
# Pin already-built CI/local assets only after verifying their public release.
# Run as `mise engine-pin`. `mise engine-verify` is `--verify-only`.
#   --verify-only  require published bytes; leave engine/release.pin unchanged
set -euo pipefail
cd "$(dirname "$0")/.."
die() { printf '%s\n' "$@" >&2; exit 1; }
source scripts/engine-pin.sh
verify_only=0
candidate=target/dist/release.pin
for arg in "$@"; do
  case $arg in
    --verify-only) verify_only=1 ;;
    -*) die "Unknown option: $arg (use --verify-only)" ;;
    *) candidate=$arg ;;
  esac
done
work=$(mktemp -d "${TMPDIR:-/tmp}/omastorm-release.XXXXXX")
trap 'rm -rf "$work"' EXIT
# fetch-engine.sh replaces an engine at its install path only when it knows
# the bytes. Every hash the pin being replaced accepted (its own and its
# previous_ lines) is carried forward as previous_sha256_<arch>, so an
# engine an older plugin installed before its sha256 record existed is
# still upgraded, and nobody has to remember the list.
declare -A carry=()
if [[ -f engine/release.pin ]]; then
  read_engine_pin engine/release.pin
  for arch in x86_64 aarch64; do
    carry[$arch]="${hashes[$arch]:-} ${previous[$arch]:-}"
  done
fi
cp -- "$candidate" "$work/release.pin"
read_engine_pin "$work/release.pin"
for arch in x86_64 aarch64; do
  [[ -n ${assets[$arch]:-} ]] || continue
  url=https://github.com/$repo/releases/download/$tag/${assets[$arch]}
  curl -fsSL --retry 2 -o "$work/asset" -- "$url" \
    || die "Publish ${assets[$arch]} on $tag before updating engine/release.pin."
  got=$(sha256sum -- "$work/asset" | awk '{print $1}')
  [[ $got == "${hashes[$arch]}" ]] \
    || die "Published ${assets[$arch]} does not match the candidate checksum; engine/release.pin was not changed."
done
if (( verify_only )); then
  printf 'Verified published assets; engine/release.pin unchanged.\n'
  exit 0
fi
carried=()
declare -A known=()
for arch in x86_64 aarch64; do
  read -ra olds <<< "${carry[$arch]:-}"
  read -ra kept <<< "${previous[$arch]:-} ${hashes[$arch]:-}"
  for sum in "${kept[@]}"; do known[$arch:$sum]=1; done
  for sum in "${olds[@]}"; do
    [[ -z ${known[$arch:$sum]:-} ]] || continue
    known[$arch:$sum]=1
    carried+=("previous_sha256_$arch=$sum")
  done
done
if (( ${#carried[@]} )); then
  {
    printf '# Engines earlier pins installed; fetch-engine.sh still replaces them.\n'
    printf '%s\n' "${carried[@]}"
  } >> "$work/release.pin"
  read_engine_pin "$work/release.pin"
fi
cp -- "$work/release.pin" engine/release.pin
printf 'Verified published assets and updated engine/release.pin\n'
