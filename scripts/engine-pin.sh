#!/usr/bin/env bash
# Shared pin reader for the installer and release builder. Data, never sourced.
engine_machine() {
  case $1 in
    x86_64) printf 'x86_64\n' ;;
    aarch64|arm64) printf 'aarch64\n' ;;
    *) die "Unsupported engine architecture: $1 (supported: x86_64, aarch64)." ;;
  esac
}

read_engine_pin() {
  local file=$1 line key val arch
  [[ -f $file ]] || die "Omastorm engine pin missing: $file"
  tag='' repo=''
  declare -gA assets=() hashes=()
  local -A seen=()
  while IFS= read -r line || [[ -n $line ]]; do
    [[ $line =~ ^[[:space:]]*(#|$) ]] && continue
    key=${line%%=*}
    val=${line#*=}
    [[ -n $key && $line == *=* ]] || die "Invalid pin line in $file: $line"
    [[ -z ${seen[$key]:-} ]] || die "Duplicate key in $file: $key"
    seen[$key]=1
    case $key in
      tag|repo) printf -v "$key" '%s' "$val" ;;
      asset_x86_64|asset_aarch64) assets[${key#asset_}]=$val ;;
      sha256_x86_64|sha256_aarch64) hashes[${key#sha256_}]=$val ;;
      *) die "Unknown key in $file: $key" ;;
    esac
  done < "$file"
  # No assets is a pin for a release not yet published: fetch-engine.sh then
  # refuses every machine with "No pinned ... engine", and a checkout builds.
  [[ -n $tag && -n $repo ]] || die "Incomplete pin in $file"
  for arch in x86_64 aarch64; do
    [[ -n ${assets[$arch]:-}${hashes[$arch]:-} ]] || continue
    [[ ${assets[$arch]:-} == "omastorm-engine-$arch-unknown-linux-gnu" ]] \
      || die "Invalid $arch asset in $file"
    [[ ${hashes[$arch]:-} =~ ^[a-f0-9]{64}$ ]] \
      || die "Pin sha256 for $arch in $file is not 64 lowercase hex digits"
  done
}
