# Data

## Archived fixture

NOAA/NEXRAD **KTLX**, Oklahoma City, **2013-05-20 20:16:43 UTC**. The lowest
sweep ends at 20:17:00 UTC; its fixed elevation is approximately 0.48°
(displayed as 0.5°). This is an archived reflectivity scan, never live weather,
and the window labels it ARCHIVED.

- [Original Level II scan](https://unidata-nexrad-level2.s3.amazonaws.com/2013/05/20/KTLX/KTLX20130520_201643_V06.gz),
  9,548,976 bytes, sha256 `772e01b154a5c966982a6d0aa2fc78bc64f08a9b77165b74dc02d7aa5aa69275`.
- **Vendored: its first elevation cut** (since S13), under the original
  name: 915,788 bytes, sha256 `d2e9ab5a…b92c67f` (`data/SHA256SUMS`), made by
  `python3 scripts/trim-level2.py ORIGINAL.gz data/fixtures/KTLX20130520_201643_V06.gz`.
  Inflated, it is a byte-exact prefix of the inflated original (5,282,392 of
  44,897,336 bytes: volume header, metadata messages, and the 720 radials of
  elevation cut 1), which is all `engine/src/sweep.rs` reads before it stops
  at the first radial of cut 2. The golden files and every KTLX check pass
  on it unchanged; only `sourceSha256`/`sourceBytes` in
  `golden/ktlx-20130520/sweep0.json` were repinned, and `extractOf` there
  records the original.
- [NEXRAD on AWS](https://registry.opendata.aws/noaa-nexrad/), accessed 2026-09-04.
  The current archive bucket is `unidata-nexrad-level2`.
- Golden files under `golden/ktlx-20130520/` were decoded once with
  [Py-ART 2.2.5](https://arm-doe.github.io/pyart/API/generated/pyart.io.read_nexrad_archive.html),
  sweep 0, nearest-neighbor handling of any mixed-resolution rays. They are the
  decoder's answer key (`docs/protocol.md`, golden files).

## SMHI fixture

SMHI **Vara** (`RAD:SE49`, `NOD:sevax`) quality-controlled polar volume
`qcvol`, nominal time **2026-09-13 10:55 UTC**; the 0.5° sweep runs
10:55:03–10:55:33 UTC. ODIM_H5 2.2 `PVOL`, 10 tilts. `engine/src/odim.rs` reads
the lowest tilt's DBZH (`/dataset1/data1`, int16, 360 × 480, 500 m bins).

- [Original volume](https://opendata-download-radar.smhi.se/api/version/latest/area/vara/product/qcvol/2026/09/13/radar_vara_qcvol_202609131055.h5),
  `radar_vara_qcvol_202609131055.h5`, 14,701,179 bytes, sha256
  `af8a6cc1984b2bd863cf4be484a2959e5d6020629ab8a9aa7f5447f8b1d1e7d0`,
  downloaded 2026-09-13. SMHI's listing keeps only about a day of volumes,
  so `refresh-fixtures.sh` cannot fetch it again.
- **Vendored: its lowest-tilt extract** (since S13), under the original name:
  67,495 bytes, sha256 `751d651b…e4fd5a32` (`data/SHA256SUMS`), made by
  `uv run --no-project --with h5py python scripts/trim-odim.py ORIGINAL.h5 data/fixtures/radar_vara_qcvol_202609131055.h5`
  (h5py 3.16, HDF5 2.0.0; the output is byte-identical run to run). It
  holds the root attributes and `/what`, `/where`, `/how` unchanged (so they
  still describe all 10 tilts) and `/dataset1` with its `what`/`where`/`how`
  and DBZH only (`data1`; the other 14 quantities of the tilt, 1.35 MB, are
  dropped). Every kept chunk is the original's bytes, and the file keeps
  SMHI's superblock shape (version 1, 4-byte offsets and lengths) with the
  end-of-file address equal to the file length, which the live range reader
  relies on (`smhi_live.rs`).
- [SMHI Open Data](https://www.smhi.se/data/om-smhis-data/villkor-for-anvandning),
  [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Source: SMHI.
- Golden files under `golden/vara-20260913/` were produced with h5py 3.16
  (libhdf5), not the engine's reader, by `golden/vara-20260913/produce.py`,
  which records its own command line. They are the ODIM decoder's answer key.
  Run on the original and on the extract, `produce.py` gives the same
  `sweep0.u8` and the same `sweep0.json` apart from the source sha256, size
  and date; `extractOf` records the original.

## Fixture rule for new radars and countries

A full volume per country would add 100–300 MB to every clone across
Europe. Each new source vendors instead:

1. **One lowest-tilt extract of at most 300 KB**:
   `scripts/trim-odim.py` for ODIM_H5 (`--keep` the quantities the decoder
   reads, e.g. `DBZH` or `TH`; it fails above `--max-bytes 300000`), and a
   first-cut prefix like `scripts/trim-level2.py` for other formats. Record
   the original's URL, time, size and sha256 here, and the extract in
   `data/SHA256SUMS`.
2. **An independent answer key in `golden/<fixture>/`**, produced by h5py
   (libhdf5, not the engine's reader) with a committed `produce.py`, the way
   `golden/vara-20260913/` is.
3. A composite or grid source vendors only the chunks its box needs, under
   the same 300 KB limit.

Git history still holds the old full volumes (about 24 MB); dropping them is
OPEN-6, a rewrite only safe before the first push, and the human's call.

## Geography

Natural Earth coastline, lakes, country boundary lines on land, and
state/province lines at 1:10m and 1:50m, plus 1:10m populated places, from
[natural-earth-vector](https://github.com/nvkelso/natural-earth-vector) master.
Made with Natural Earth; [public domain](https://www.naturalearthdata.com/about/terms-of-use/).
`engine/build.rs` embeds them as polylines (the 1:10m set clipped to the NEXRAD
network envelope) and the engine strokes them into `ne` tiles at any zoom
(`docs/protocol.md`, tiles). Roads and place labels at closer zooms come from
OpenMapTiles vector tiles served by OpenFreeMap, © OpenStreetMap contributors
(ODbL), fetched by the engine at run time and attributed in the UI.

The location picker searches [GeoNames](https://www.geonames.org/)
`cities5000` (populated places with population ≥ 5000) clipped to that
same envelope, with admin-1 names from `admin1CodesASCII.txt`.
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Map labels do
not use this table. The city-list checksum covers the official 2026-09-10
snapshot (69,705 records).

## Fetching

The engine embeds the Natural Earth geography and the GeoNames gazetteer at
build time; the archived
volume is read at run time by the decoder tests and by a daemon started with
`OMASTORM_ARCHIVE` (the checks and captures). Compressed copies live in
`data/fixtures/`; `data/raw/` is ignored. A fresh checkout runs
`bash scripts/extract-fixtures.sh` once: it copies and extracts the vendored
files, then verifies `data/SHA256SUMS`. Ordinary setup, `mise check`, cargo
builds, and CI do not download these files. A build without those files stops
with a message naming the script; a shipped daemon embeds nothing archived.
Installing and launching the plugin needs none of this; only a checkout
build does.

## Refreshing

Leave the vendored bytes alone until you mean to take a new snapshot. To
refresh:

1. `bash scripts/refresh-fixtures.sh` downloads the live NEXRAD, Natural
   Earth master, and GeoNames URLs, rewrites `data/fixtures/` and
   `data/SHA256SUMS`, and extracts into `data/raw/`.
2. Review the checksum diff. Update the dates and GeoNames record count in
   this file.
3. Build the engine and run `mise check` against the new geography and
   gazetteer.
4. Commit the vendor files, `data/SHA256SUMS`, and this README together.

A changed download must be reviewed and repinned, never accepted without
verification. `scripts/extract-fixtures.sh` never hits those live URLs.

## Decoder and rendering contract

The [wire protocol](../docs/protocol.md#texture-files) defines radar codes,
palette lookup, and sampling; the [engine guide](../engine/README.md#verification)
explains verification against the golden files.
