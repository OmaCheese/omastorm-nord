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

## OPERA fixture (Nordic composite, S16)

EUMETNET **OPERA** CIRRUS maximum-reflectivity composite, nominal time
**2026-09-14 08:10 UTC** (data 08:00:01–08:10:00), ODIM_H5 2.4 `COMP`,
`/dataset1/data1` DBZH float64, 4400 × 3800 pixels of 1 km on
`+proj=laea +lat_0=55 +lon_0=10 +x_0=1950000 +y_0=-2100000 +ellps=WGS84`,
30 deflate-9 chunks of 760 × 880 (DEC-14). `engine/src/composite.rs` reads
only the 11 chunks under the Nordic box (3–33° E, 53–71.5° N).

- Original: `OPERA@20260914T0810@0@DBZH.h5` from the Open Radar Data 24-hour
  cache, `https://s3.waw3-1.cloudferro.com/openradar-24h/2026/09/14/OPERA/COMP/`,
  1,866,101 bytes, sha256
  `73e748236a6e206070835f151a11ada5c81bb855eeb7267013ae9313046ce881`,
  downloaded 2026-09-14. The cache keeps about a day, so it cannot be
  fetched again.
- **Vendored: a crop of its chunks**, `opera_nordic_202609140810.h5`,
  289,488 bytes, sha256 `1a9c2fc4…b3ea56ee` (`data/SHA256SUMS`), made by
  `uv run --no-project --with h5py python scripts/crop-opera.py ORIGINAL.h5 data/fixtures/opera_nordic_202609140810.h5 --keep 1,2 1,3 2,2 2,3 0,3 5,0`
  (h5py 3.16, HDF5 2.0.0). The root attributes and `/what`, `/where`,
  `/how`, `/dataset1/what` and `/dataset1/data1/what` are unchanged; the
  quality layer is dropped. Of the 11 needed chunks, the 11 verbatim come to
  882 KB (868,342 B of chunk data; shuffle does not help: 917 KB), so the
  fixture keeps five of them (central and southern Sweden, Finland, the
  Baltic; 266,732 B) and one unneeded chunk (row 5, column 0, 8,676 B) that
  a test proves is never fetched. The other chunks are unallocated and read
  as the fill value, set to the layer's nodata; the engine reads a needed
  chunk the file does not allocate as nodata too.
- [EUMETNET OPERA](https://www.eumetnet.eu/activities/observations-programme/current-activities/opera/),
  [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Source:
  EUMETNET OPERA (created by Météo-France).
- Golden files under `golden/nordic-20260914/` were produced with h5py 3.16
  and pyproj (PROJ), not the engine's reader or projection, by
  `golden/nordic-20260914/produce.py`, which records its own command line.

## Norwegian, Finnish and Danish fixtures (S15)

One file per country from EUMETNET Open Radar Data's public 24-hour S3
cache (DEC-13), each vendored as its lowest-tilt extract
(`uv run --no-project --with h5py python scripts/trim-odim.py ORIGINAL.h5 EXTRACT.h5 --keep …`,
h5py 3.16, HDF5 2.0.0), each with an h5py answer key in `golden/<id>-20260914/`
from `golden/produce-odim.py`. The cache keeps files for about a day and a
half, so none can be fetched again. Licence CC BY 4.0 ("with exceptions
noted in metadata"; none noted for these radars); credit the national owner.

| Fixture (`data/fixtures/`) | Original (`https://s3.waw3-1.cloudferro.com/openradar-24h/2026/09/14/…`) | Original bytes, sha256 | Extract bytes | Kept | Credit |
|---|---|---|---|---|---|
| `ord_nohur_202609140930.h5` | `NO/nohur/PVOL/nohur@20260914T0930@0.5_1.0_1.6_2.4_3.2_4.2_5.4_6.8_8.5_10.4_12.8_15.5@DBZH.h5` | 427,967, `78a32961…2b046c` | 112,271 | DBZH | MET Norway |
| `ord_fikor_202609140940.h5` | `FI/fikor/SCAN/fikor@20260914T0940@0.5@DBZH_TH_VRADH.h5` | 145,837, `32a83832…dc6866` | 90,964 | TH, DBZH | FMI |
| `ord_dksin_202609140940.h5` | `DK/dksin/PVOL/dksin@20260914T0940@0.49_0.66_0.96_1.47_2.37_4.81_8.42_9.98_12.99_15.01@DBZH_LDR_PHIDP_RHOHV_TH_VRAD_WRAD_ZDR.h5` | 1,293,200, `8ad2cdde…a12d` | 74,822 | DBZH, TH | DMI |

What each proves in `engine/src/odim.rs` (the decoder quirks):

- **Hurum** (MET Norway, ODIM 2.2 `PVOL`, superblock 1 with 4-byte
  offsets): one quantity per file, uint8 (gain 0.5, offset −32, nodata 255,
  undetect 0), 720 rays × 960 gates of 250 m from 0 km, no `how/startazA` or
  ray times at all (equal half-degree sectors, nominal times), `a1gate` 501,
  and a sweep that starts 54 s before the file's nominal time.
- **Korppoo** (FMI, ODIM 2.3 `SCAN`, superblock 0 with 8-byte offsets): one
  file per elevation, `TH` stored as `data1` and `DBZH` as `data2`,
  `startazA`/`stopazA` but no ray times, 360 rays × 500 gates of 500 m.
- **Sindal** (DMI, ODIM 2.0 `PVOL`, superblock 0): eight quantities in
  90 × 119 chunks, ray angles only in ODIM 2.0's `how/azangles` string,
  spelled `azangels`, and `rstart` 0.5 km (first gate at 750 m).

All three store the lowest tilt as `/dataset1`, as do all 29 radars
(`scripts/fetch-ord-sites.sh`, 2026-09-14); the decoder still picks the
lowest `elangle` (`Tilt::Lowest`). `produce-odim.py` gives the same
`sweep0.u8` from the original and from the extract.

## Spanish fixtures (S32)

Two of AEMET's radars from the same cache (DEC-16), because Spain's eleven
come in two exports: Alhaurín el Grande (Vaisala IRIS 10.5, like nine of
them) and Valladolid (IRIS 8.13, like San Sebastián). Cut by
`scripts/trim-odim.py … --keep DBZH`; answer keys in
`golden/esahr-20260915/` and `golden/eslid-20260915/` from
`golden/produce-odim.py`, Alhaurín's products from
`golden/produce-products.py` on its three-tilt extract. Credit `© AEMET`
(AEMET's reuse notice), CC BY 4.0 as ORD publishes it. Together 198,909
bytes, under the 300 KB a country may take.

| Fixture (`data/fixtures/`) | Original (`https://s3.waw3-1.cloudferro.com/openradar-24h/2026/09/15/…`) | Original bytes, sha256 | Extract bytes | Kept |
|---|---|---|---|---|
| `ord_esahr_202609151900.h5` | `ES/esahr/PVOL/esahr@20260915T1900@0.5_1.3_2.1@DBZH_TH.h5` | 213,439, `a632c07e…37dbc2` | 39,556 | DBZH, lowest tilt |
| `ord_esahr_202609151900_tilts.h5` | the same | | 102,024 | DBZH, 0.5, 1.3, 2.1° |
| `ord_eslid_202609151900.h5` | `ES/eslid/PVOL/eslid@20260915T1900@0.5_1.4_2.3@DBZH_TH.h5` | 317,446, `aec8c791…101109` | 57,329 | DBZH, lowest tilt |

What they prove in `engine/src/odim.rs`:

- **Alhaurín el Grande** (ODIM 2.4 `PVOL`): `TH` then `DBZH` as float64
  with gain 1, offset 0, `undetect` −32 and `nodata` 95.5; 360 rays with
  `startazA`/`stopazA`, 250 gates of 1 km; `rstart` 200 is **metres**
  (first gate at 700 m, `odim::rstart_km`); the key decides it from the
  pulse rate (560 Hz: 267.7 km unambiguous, where 200 km + 250 km would be).
- **Valladolid**: 450 rays of 0.8° with **no azimuths at all** (equal
  sectors from north; a clutter-against-terrain correlation put the best
  rotation at 0°), 240 gates, `rstart` 125 in metres (first gate at 625 m).

AEMET also publishes a Doppler volume (`…@0.5_1.5@DBZH_VRADH.h5`) 7 minutes
after each; the poller does not read it (`ord::one_task`), so it has no
fixture.

## Multi-angle fixtures (S20)

The products other than the lowest scan (pseudo-CAPPI, column maximum,
clear view, one other angle; `engine/src/products.rs`) read several tilts,
so each provider format with every angle in one file has a DBZH-only extract
of several tilts, made from the same originals as above by
`uv run --no-project --with h5py python scripts/trim-odim.py ORIGINAL.h5 EXTRACT.h5 --tilts …`
(positions in ascending elevation; h5py 3.16, HDF5 2.0.0), each under
300 KB. The kept tilts become `/dataset1…` in ascending elevation, their
metadata and chunks byte-identical to the original's.

| Fixture (`data/fixtures/`) | Original | Tilts kept (`--tilts`) | Bytes |
|---|---|---|---|
| `radar_vara_qcvol_202609131055_tilts.h5` | the SMHI volume above (14,701,179 B, `af8a6cc1…`) | 0.5, 1.5, 2.5, 8, 24° (`0,2,4,6,8`) | 298,733 |
| `ord_nohur_202609140930_tilts.h5` | the Hurum file above (427,967 B) | 0.5 (720 rays), 1.0, 2.4, 5.4° (`0,1,3,6`) | 227,791 |
| `ord_dksin_202609140940_tilts.h5` | the Sindal file above (1,293,200 B) | 0.49, 0.66, 1.47, 4.81, 9.98° (`0,1,3,5,7`) | 185,675 |

Together they cover both gate spacings and three reaches of SMHI's tilts,
Hurum's 720-ray lowest tilt beside 360-ray upper ones, and DMI's first gate
at 750 m.

FMI's files hold one angle each (S24a): the engine reads the five `SCAN`
files of one nominal time as one volume. Its fixture is the five files of
Korppoo at 2026-09-15 00:00 UTC, each cut to DBZH by
`scripts/trim-odim.py ORIGINAL.h5 EXTRACT.h5 --keep DBZH` (its one tilt,
metadata and chunks byte-identical to the original's), 295,022 bytes
together. The originals,
`s3.waw3-1.cloudferro.com/openradar-24h/2026/09/15/FI/fikor/SCAN/fikor@20260915T0000@<angle>@DBZH_TH_VRADH.h5`,
are named with their sizes and digests in `golden/fikor-20260915/products.json`.

| Fixture (`data/fixtures/`) | Angle | Original bytes | Bytes |
|---|---|---|---|
| `ord_fikor_202609150000_scan05.h5` | 0.5° | 208,995 | 64,368 |
| `ord_fikor_202609150000_scan07.h5` | 0.7° | 203,378 | 67,804 |
| `ord_fikor_202609150000_scan15.h5` | 1.5° | 177,736 | 65,905 |
| `ord_fikor_202609150000_scan30.h5` | 3.0° | 129,124 | 51,561 |
| `ord_fikor_202609150000_scan50.h5` | 5.0° (367 gates, 183.5 km) | 108,722 | 45,384 |

Its answer key is the assembled volume's column maximum, storm height (8
"at least" tops) and rain mass (`golden/produce-products.py --parts`);
`products::tests::an_assembled_volume_matches_its_answer_key` matches them
byte for byte. Every fixture's key also holds `etop` and `vil` (S24a). The
answer keys are
`golden/<id>/products.json` and one gzipped `<variant>.u8.gz` per product,
composed from every kept tilt by `golden/produce-products.py` (h5py, with
the contract's formulas in scalar libm arithmetic);
`products::tests::products_match_their_answer_keys` matches them byte for
byte through `decode_volume`, which reads only the tilts a product needs.

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
`engine/build.rs` embeds them as polylines (the 1:10m set clipped to the
radars' envelopes: the Nordic box, 3–33° E, 53–71.5° N, and since S32 Iberia,
11° W–5° E, 34–46° N, and the Canary Islands, 19.5–12.5° W, 25.5–31° N) and the engine strokes them into `ne` tiles at any zoom
(`docs/protocol.md`, tiles). Roads and place labels at closer zooms come from
OpenMapTiles vector tiles served by OpenFreeMap, © OpenStreetMap contributors
(ODbL), fetched by the engine at run time and attributed in the UI.

The location picker searches [GeoNames](https://www.geonames.org/)
`cities5000` (populated places with population ≥ 5000) clipped to those
same envelopes and to the gazetteer's countries (Sweden, Norway, Finland,
Åland, Denmark, Estonia, Latvia, Lithuania; since S32 Spain, Portugal,
Andorra, Gibraltar), with admin-1 names from `admin1CodesASCII.txt`.
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Map labels do
not use this table. The Nordic and Baltic rows are the official 2026-09-10
snapshot (69,705 records worldwide, 1,074 kept); S32 appended the Spanish,
Portuguese, Andorran and Gibraltar rows of the 2026-09-15 download (2,037),
3,111 rows in all, and cut the 1:10m lines of the same day's
natural-earth-vector master to the new boxes (its Nordic cut was
byte-identical to the vendored one).

### Terrain (S30)

`engine/data/terrain-nordic-2km.bin`, the ground that heights "above
ground" are measured from (`docs/protocol.md`, Terrain), is made by
`scripts/terrain-grid.py` from Mapzen's **Terrain Tiles** (Terrarium PNG
encoding, zoom 8), open data on AWS
(<https://registry.opendata.aws/terrain-tiles/>,
`s3.amazonaws.com/elevation-tiles-prod`), the same tiles
`scripts/blockage-tables.py` reads for Clear view. The tiles are a blend of
public elevation models, each with its own credit, listed in
[joerd's attribution.md](https://github.com/tilezen/joerd/blob/master/docs/attribution.md).
Over the Nordic box they include:

- Kartverket's elevation model (Norway), [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/);
- the National Land Survey of Finland's elevation model, [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/);
- SDFE's elevation model (Denmark), under SDFE's free-data terms;
- EU-DEM, "produced using Copernicus data and information funded by the
  European Union";
- and SRTM, GMTED2010 and ETOPO1 (public domain) where those are absent.

Credit, as the README's "Data and licenses" and every above-ground frame's
`attribution` give it: "terrain: Mapzen Terrain Tiles (AWS open data; Kartverket,
NLS Finland, SDFE, EU-DEM/Copernicus)".

Made on 2026-09-15: the box of every station-table radar's 250 km circle on
the Web Mercator lattice of 2,000 m (1976 × 2556 texels, lattice column 16,
north row 6034), each texel the mean of the zoom-8 pixels whose centres fall
in it, sea and land below sea level as 0, in steps of 10 m (highest 2,230 m).
729 tiles within reach of a radar were read (282 already in the blockage
tables' cache, 447 fetched, 31.0 MB); the file is 1,025,185 bytes. To remake
it: `uv run --no-project --with numpy --with pillow python
scripts/terrain-grid.py --cache <dir>`.

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
