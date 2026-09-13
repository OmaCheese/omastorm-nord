#!/usr/bin/env bash
# Fixture sources (development only). Copies the archived Level II volume and
# extracts the Natural Earth files that `engine/build.rs` converts into the
# embedded geography (DESIGN.md, basemap tiles: coastline, lakes, country and
# state lines at 1:10m and 1:50m, plus populated places for map labels) and
# GeoNames cities5000 for the location picker from data/fixtures/, then
# verifies data/SHA256SUMS. data/raw is ignored, so a fresh checkout runs
# this once before `cargo build`. Launch never calls it. Live upstream URLs
# are only for `scripts/refresh-fixtures.sh` (data/README.md).
#
# `--regenerate` is the other half of a refresh: after refresh-fixtures.sh
# has put the worldwide downloads in data/raw/, it cuts them to the SMHI
# network and rewrites the vendored copies and data/SHA256SUMS. The 1:10m
# lines keep every feature with a vertex in the Nordic box of
# `engine/build.rs` (3–33° E, 53–71.5° N; build.rs then clips vertex by
# vertex), and cities5000 and admin1 keep Sweden, Norway, Finland, Åland,
# Denmark and the Baltics. The 1:50m world set, the populated places and the
# archived KTLX volume (the decoder tests and OMASTORM_ARCHIVE read it until
# the ODIM golden replaces it) stay as they are. Worldwide input is detected
# and cut; input already cut passes through unchanged, so a rerun is a no-op.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p data/raw

countries='SE|NO|FI|AX|DK|EE|LV|LT'
regenerate() {
  local name
  for name in $(awk '{print $2}' data/SHA256SUMS); do
    [[ -f $name ]] || {
      printf 'Missing %s: run bash scripts/extract-fixtures.sh (or refresh-fixtures.sh) first.\n' "$name" >&2
      exit 1
    }
  done
  work=$(mktemp -d "${TMPDIR:-/tmp}/omastorm-regenerate.XXXXXX")
  trap 'rm -rf "$work"' EXIT
  awk -F '\t' -v re="^($countries)\$" '$9 ~ re' data/raw/cities5000.txt >"$work/cities5000.txt"
  awk -F '\t' -v re="^($countries)[.]" '$1 ~ re' data/raw/admin1CodesASCII.txt >"$work/admin1CodesASCII.txt"
  for theme in coastline lakes admin_0_boundary_lines_land admin_1_states_provinces_lines; do
    jq -c '
      def inside: .[0] >= 3 and .[0] <= 33 and .[1] >= 53 and .[1] <= 71.5;
      .features |= map(select([.geometry.coordinates | .. | arrays | select(length >= 2 and (.[0] | type) == "number")] | any(inside)))
    ' "data/raw/ne_10m_${theme}.geojson" >"$work/ne_10m_${theme}.geojson"
  done
  for file in "$work"/*; do
    name=$(basename "$file")
    if ! cmp -s "$file" "data/raw/$name"; then
      printf '%-50s %10d -> %10d bytes\n' "$name" "$(stat -c %s "data/raw/$name")" "$(stat -c %s "$file")"
      cp -f "$file" "data/raw/$name"
    fi
    gzip -n -9 -c "data/raw/$name" >"data/fixtures/$name.gz"
  done
  local sums=$work/SHA256SUMS
  : >"$sums"
  for name in $(awk '{print $2}' data/SHA256SUMS); do
    sha256sum "$name" >>"$sums"
  done
  diff -u data/SHA256SUMS "$sums" || true
  cp -f "$sums" data/SHA256SUMS
  sha256sum -c --quiet data/SHA256SUMS
  printf 'Regenerated. cities5000.txt: %s records.\n' "$(wc -l <data/raw/cities5000.txt)"
  echo 'Update the record count in data/README.md before committing.'
}
if [[ ${1:-} == --regenerate ]]; then
  regenerate
  exit 0
fi

if sha256sum -c data/SHA256SUMS >/dev/null 2>&1; then
  echo 'Fixtures already verified.'
  exit 0
fi
[[ -d data/fixtures ]] || {
  printf 'Missing data/fixtures/ (see data/README.md).\n' >&2
  exit 1
}
while read -r _ path; do
  [[ -n ${path:-} ]] || continue
  name=${path#data/raw/}
  dest=data/raw/$name
  if [[ -f data/fixtures/$name ]]; then
    cp -f "data/fixtures/$name" "$dest"
  elif [[ -f data/fixtures/$name.gz ]]; then
    gzip -dc "data/fixtures/$name.gz" >"$dest"
  else
    printf 'Missing vendored fixture for %s (see data/README.md).\n' "$name" >&2
    exit 1
  fi
done <data/SHA256SUMS
sha256sum -c data/SHA256SUMS
