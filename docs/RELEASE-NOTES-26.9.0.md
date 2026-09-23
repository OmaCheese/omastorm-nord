# Omastorm Nord engine 26.9.0

The first engine release of Omastorm Nord, a fork of
[Omastorm](https://github.com/wesleygrimes/omastorm) by Wes Grimes. Where
upstream draws NOAA NEXRAD over the United States, Omastorm Nord draws European
weather radar: 41 radars in Sweden, Norway, Finland and Denmark, SMHI's
national composite, EUMETNET OPERA's Nordic composite, and a mosaic of the
radars you choose, in the Omarchy bar and window.

The engine version is CalVer (`YY.M.patch`) so it can never share a tag with
upstream's `engine-0.1.x` line.

## Install

The plugin downloads this release's engine the first time the popover opens,
checks its sha256 against `engine/release.pin`, and installs it under
`~/.local/share/omastorm-nord/bin`. Assets:

- `omastorm-engine-x86_64-unknown-linux-gnu`
- `omastorm-engine-aarch64-unknown-linux-gnu`
- `SHA256SUMS`, `release.pin`

Both binaries are built by the Engine builds workflow on Ubuntu 24.04
runners (x86_64 and ARM64) from the tagged commit.

## What is new against upstream

**Radar sources** (protocol v2, additive over upstream's v1):

- **Sweden, SMHI.** All 12 radars from SMHI's open data, the lowest 0.5°
  reflectivity (DBZH) of each quality-controlled volume, decoded from ODIM
  HDF5 in pure Rust (`hdf5-pure`, no C library). HTTP range reads fetch about
  100 KB of each ~14 MB volume, so a station's 60-scan loop (about five
  hours) backfills in seconds.
- **SMHI's national composite** (`sweden`), reprojected in the engine from
  SMHI's polar stereographic grid to Web Mercator.
- **Norway, Finland and Denmark.** 29 radars (12, 12 and 5) from EUMETNET
  Open Radar Data's public 24-hour cache, one provider for all three
  countries, 60 scans each.
- **The Nordic composite** (`nordic`), cut from EUMETNET OPERA's pan-European
  maximum-reflectivity composite: the engine reads only the 11 of 30 chunks
  that cover 3–33° E, 53–71.5° N (under 1 MB of a ~1.9 MB file per frame),
  24 frames on join, up to 60 as they arrive.
- **My mosaic**, a station built from the radars you pick, so a distant radar
  whose coverage is high and misleading can be left out.
- 44 stations in all. Swedish radars keep SMHI's names (`vara`,
  `balsta`, …) and also answer to their ODIM node codes (`sevax`); other
  radars use their node codes (`nohur`, `fikor`, `dksin`). Each
  station carries its country, provider, range and licence credit in
  `hello`.

**Engine**

- Staleness follows each source's cadence: LIVE, STALE after 15 minutes,
  UNAVAILABLE after 30 (a healthy 5-minute radar frame is already 5–10
  minutes old, so upstream's 10 would flap).
- When a listing lags, the engine probes the next scan's dated file once it
  is overdue, and backs off on errors while keeping cached frames.
- Catalogued frames keep one texture name for as long as the catalog holds
  them, so clients cache and replay a loop without refetching (smooth loop).
- Place search folds Nordic letters and knows local and English names
  (`goteborg`, `gothenburg` → Göteborg; `helsingfors` → Helsinki); map labels
  use local Latin-script names.
- Its own directories and plugin id (`omastorm-nord`, `omacheese.omastorm-nord`), so it
  installs beside upstream Omastorm without clashing.

**Products** — beyond the lowest sweep, the engine builds `Clear view` (per
direction, the lowest angle the terrain does not block), `Height` (one
altitude, above sea level or above ground), `Column max`, `Storm height`,
`Rain mass`, and `Lowest beam` on composites, plus vertical sections with a
one-column profile on long press, and `Relief`, which draws `Storm height` as
a lit surface. Where no beam passes, the map says "no radar at this height",
which is not "no rain".

**Plugin** (the Omarchy bar mark, popover and window; released separately
from the engine as the plugin's `v*` tags)

- A picker grouped by composite and country, searchable by station, town,
  county, country and node code; range circles from each radar's real range.
- Each frame names its source's credit in the window and the popover.
- Location onboarding from the Omarchy weather location, a manual place, or,
  only on request, an approximate location from your IP via wttr.in.

Archive mode (`OMASTORM_ARCHIVE`) still reads upstream's NEXRAD Level II
files, and the test suite stays on upstream's KTLX 2013 volume, so the
checks run offline.

## Credits

- **Omastorm** by Wes Grimes (MIT): the engine, the Quickshell/QML plugin,
  the treatments, and the release tooling this fork builds on.
- Radar data: **SMHI** (CC BY 4.0); **MET Norway**, **FMI** and **DMI**
  (CC BY 4.0) through **EUMETNET Open Radar Data**; the Nordic composite
  **EUMETNET OPERA** (CC BY 4.0).
- Maps: © **OpenStreetMap** contributors (ODbL), vector tiles by
  **OpenFreeMap** (© OpenMapTiles);
  **Natural Earth** (public domain); places from **GeoNames** (CC BY 4.0).

## Known limits

- Reflectivity only: no Doppler velocity, no dual-polarization products.
- The desktop's `release.pin` changes only after both architectures are
  published and verified (docs/RELEASING.md).
