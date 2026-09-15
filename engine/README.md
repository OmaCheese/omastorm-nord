# Engine

Build and run with [CONTRIBUTING.md](../CONTRIBUTING.md). Distribution and
versioning are in [docs/RELEASING.md](../docs/RELEASING.md).

## Architecture

The binary embeds Natural Earth geography, `data/sites.json`, and
`data/fixture.json` (the product, palette, and frame template). It embeds no
archived radar. A daemon starts with no station; `select_site` starts live
polling. An `OMASTORM_ARCHIVE` scan is decoded at startup and labeled archived.
Missing build data
produces an error naming `scripts/extract-fixtures.sh`; vendored archives and checksums
are described in [data/README.md](../data/README.md).

## Runtime and storage

`XDG_RUNTIME_DIR` must name an absolute directory. The daemon owns
`omastorm-se/engine.sock`, uses `engine.lock` to serialize startup, and logs to
`omastorm-se/engine.log`. `ensure` starts a background daemon and waits up to
10 seconds for its hello. Concurrent launches share it; it outlives windows.

Hello includes the PID, protocol version, and executable fingerprint.
`ensure` replaces a daemon whose build or protocol differs, and clients
reconnect. `stop` ends the answering daemon and waits up to two seconds for
its socket and lock to be released. With no daemon, it exits successfully
without creating files. Use `stop` instead of killing the daemon.

The server uses a current-thread Tokio runtime, with reader and writer tasks
per client. Startup archive decoding precedes the runtime; tile rendering and
live texture encoding use the blocking pool. Launcher operations use standard
sockets. A stalled client is disconnected after its eight-message output queue
fills or its write exceeds two seconds.

Textures are written, synced, and renamed to unique paths. Cleanup checks
state references once a second and removes textures unreferenced for 30 seconds.
The grace period starts when observed, so a restart preserves recently served
textures. Transport, commands, state, and texture encoding are defined in
[docs/protocol.md](../docs/protocol.md).

## Radar

`src/odim.rs` decodes SMHI's ODIM HDF5 volumes with the pure-Rust `hdf5-pure`
(DEC-1): the lowest tilt's DBZH, found by `what/quantity`, rays sorted by
azimuth, requantized to u8 as `round(dBZ × 2 + 66)` with undetect and nodata
flagged. `src/composite.rs` decodes the national composite and reprojects it
to a Web Mercator grid texture (DEC-11). `src/sweep.rs` holds the
source-agnostic sweep texture and azimuth lookup, and still decodes NEXRAD
Level II for archive mode only (`OMASTORM_ARCHIVE`, DEC-10). The UI samples
these textures directly; radar arrays never enter JSON or QML JavaScript.

`src/smhi_live.rs` is the poller. SMHI publishes one whole volume per site
every 5 minutes, about 4–5 minutes after its valid time, so every frame is
complete: there is no sweep to assemble and nothing partial to paint. The
listing `area/{site}/product/qcvol.json` (`comp.json` for the composite) is
polled every 60 s with `If-Modified-Since`. A new volume is read from its
dated URL with HTTP range requests, about 7 requests and 100 KB of a 15 MB
file (DEC-2). Backfill reads the day listing, plus yesterday's just after
midnight UTC, and fetches the newest 60 volumes the catalog lacks, newest
first and paced. 429 and 5xx answers and transport errors back off,
honouring `Retry-After`; the second failure in a row reports offline. Only one
volume download runs at a time in the whole engine. Selecting another station
cancels the poller and discards its late events.

A feed becomes stale at 15 minutes and unavailable at 30 minutes without a new
frame (DEC-9: a healthy SMHI frame is already 5–10 minutes old). A station
whose newest volume is older than that is reported silent, which the UI shows
as unavailable; Leksand has published nothing since January 2026. Cached
frames remain usable under every condition.

`src/catalog.rs` stores the newest 60 complete frames per station in
`$XDG_CACHE_HOME/omastorm-se/frames/`: a SQLite WAL catalog and PNG files.
Entries retain scan geometry, times, and source provenance. The UI never reads
this store. The timeline serves cached frames through new runtime textures.
Playback loops complete frames over about ten seconds, bounded to 250 ms–1 s
per frame. New live sweeps take the screen only while the newest entry is selected.

### Tilt store

`src/tilts.rs` (S27) keeps the decoded reflectivity tilts of every radar
volume a poller reads, so every product of that volume, and later a mosaic or
a section, is composed from disk instead of fetching the volume again. The
frame catalog above is unchanged; the store sits beside it:

- **Layout.** `$XDG_CACHE_HOME/omastorm-se/tilts/index.sqlite` (its own WAL
  database) and one file per tilt,
  `tilts/<station>/<YYYYMMDDTHHMMSSZ>-<tenths>-<dataset>.u8z`: zlib of the
  scan's `Sweep` exactly as `odim.rs` decodes it (u8 codes rays × gates, each
  ray's azimuth, elevation and time, the gate geometry), byte-exact.
- **Key.** Station id, the provider's nominal time in ms (SMHI's `valid`,
  ORD's file time: known before any request) and the dataset index (0 is
  `/dataset1`); the angle in tenths is a column and part of the file name.
  Table `volumes` holds each volume's source file and its angle table (every
  scan's `where`: angle, first gate, spacing, gates, in file order) once a
  read has seen it; SMHI's lowest-scan read (`/dataset1` alone, DEC-2)
  stores its tilt without one. Another source file for the same station and
  time replaces what was kept.
- **Reads.** For each volume a poller first asks the store
  (`Slot::compose`): with the angle table and every tilt `products::needed`
  names on disk, the frame is composed with no request, and its provenance
  ends `from the tilt store, 0 range requests, 0 of 0 bytes`. Otherwise
  `tilts::decode` reads the file, decodes only the needed tilts the store
  lacks, stores them and the table, and composes; the answer is
  `products::decode_volume`'s, byte for byte. A backfill first sends every
  stored volume of the loop the catalog lacks, newest first and unpaced, then
  fetches the rest of its depth; its log line counts both.
- **Cap.** `OMASTORM_TILTS_MB` (default 256, in 10^6 bytes of `.u8z`; 0
  turns the store off, and then My mosaic's height sets, made from the
  store, report the station offline with that reason, S30). After each save the least recently used tilts go
  first (a compose counts as a use), oldest volume first on a tie, whatever
  the station. Nothing is fetched to fill it. Each save logs
  `Tilts <station>: N tilts, M MB, cap X MB`; an eviction logs
  `Tilts store: evicted …; now N tilts, M MB, cap X MB`.
- **Sizes** (deflated, the S20 fixtures): SMHI 360 × 480 about 7 KB a tilt
  (14 KB at 592 gates), so about 90 KB for a volume's ten; MET Norway
  720 × 960 74 KB, its 360-ray tilts 11–38 KB; DMI 360 × 475 6–11 KB.
- **API** for S24a, S24b and S25: `tilts::shared()` is the engine's store
  (`None` when off). `Store::volumes(station)` lists stored volumes newest
  first (time, source); `Store::volume(station, time)` gives the angle table
  and which tilts are stored (`Stored`: dataset, tenths, geometry, bytes);
  `Store::tilt(station, time, dataset)` and `Store::tilts(station, time)`
  load them as `products::Tilt`; `Store::save` adds tilts. The lowest scan
  of a station at a time is the stored tilt with the smallest angle, present
  for every product read since S26 made it free.

### Vertical products (S24a)

`products.rs` makes two products from a radar's whole volume, on the lowest
scan's rays and gates like `CMAX` (`docs/protocol.md`, storm height and rain
mass): **Storm height** (`ETOP`, `echo_top_code`), the height above sea level
of the highest beam holding 18 dBZ, with an "at least" code (odd, G bit 8 in
the texture, `mark_texture`) where no beam above it in the column says
otherwise; and **Rain mass** (`VIL`, `vil_code`), Marshall–Palmer water over
the gaps between beam centres, reflectivity capped at 56 dBZ. Each brings
its own units, palette, bounds, `scale` and `offset` (`Want::legend`;
`main.rs` `legend_of` puts them in the frame and the loading placeholder).
Far from a radar only low beams reach a column, so a storm height there is
often "at least": on 2026-09-15 45% of Hurum's drawn texels were.

**FMI's volumes.** ORD's cache holds FMI's scans as one `SCAN` file per angle
(five per nominal time). For any product but the lowest scan the ORD poller
lists every angle's file per time (`ord::choose_sets`; the newest time waits
until it has as many angles as the time before) and `fetch_set` reads them
whole, one request each, into one volume (`products::scans_of`,
`products::assemble`). The tilt store keeps the five scans as one volume
under the source `<day>/FI/<nod>/SCAN/<nod>@<time>` (`ord::set_source`) with
the full angle table, so every other product of that time is free; the
backfill's store phase makes a per-angle station's product only from such a
set. The lowest scan still reads its one file. A product switch on an FMI
radar backfills 12 volumes (`ord::SET_BACKFILL`). Measured on 2026-09-15:
Korppoo 5 requests and 604–636 KB a frame; for comparison Vara 39 requests
and 737 KB, Hurum 1 request and 679–814 KB, Sindal 1 request and 0.98–1.23 MB;
the second product of each came from the store with no request.

**Rolling back.** An engine older than S24a reads a frame id's unknown
last part as its lowest scan (`products::variant_of`), so over a cache that
holds `-etop` and `-vil` frames it shows them in the lowest-scan loop. Before
starting an older engine on such a cache, stop the engine and delete those
frames from the catalog and their PNGs (`<site>/<id>-<hash>-sweep.png`,
`-azlut.png`, `-codes.png`), with the engine's own `XDG_CACHE_HOME`:

```sh
dir=${XDG_CACHE_HOME:-~/.cache}/omastorm-se/frames
sqlite3 "$dir/catalog.sqlite" "DELETE FROM frames WHERE id LIKE '%-etop' OR id LIKE '%-vil'"
rm -f "$dir"/*/*-etop-*.png "$dir"/*/*-vil-*.png
```

or wipe the catalog (the cache refills). From S24a on, `variant_of` gives
any all-letter last part but `loading` its own ring, so a later engine's
new products roll back cleanly.

### My mosaic

`src/mosaic.rs` (S25) makes the grid station `mymosaic` from the lowest scan
of up to 12 radars a client chooses with `set_mosaic` (`docs/protocol.md`,
My mosaic). It has four parts, meant to be extended by S30 (heights across
the chosen radars) and S24b (the vertical products on `sweden`/`nordic`):

- **Set.** `mosaic::choose(sites, args, rule)` checks a `set_mosaic` and
  returns the `Set` that becomes `state.mosaic` (canonical ids, the reach in
  force, the `Rule`); `Set::variant` names it in frame ids and catalog rings
  (`m` + 8 hex, which `products::variant_of` recognizes), so `prune` keeps
  the current and the previous set's frames.
- **Placement.** `Layout::new(set, sites)` fixes the Web Mercator texture
  over the box of the reach circles (the composites' `PIXEL_M`, snapped to
  the Mercator lattice) and, per radar, a table over its part of the box:
  each texel centre's great-circle ground distance in 10 m units (`NONE`
  past the reach) and bearing in tenths of a degree, on the lookup rule's
  6,371 km sphere. Built once per set; nothing in it depends on a scan.
- **Combining.** `combine(layout, rule, inputs)` takes one `Input` per
  radar (its scan at T, and a longer scan's far ring for a short DMI scan)
  and returns every texel's code and the radar it came from. Per frame each
  scan becomes lookups: azimuth entry to ray (`sweep::GAP_DEG`), distance
  unit to gate and to beam-centre height above sea level at the scan's own
  elevation (4/3 earth). `Rule::Lowest` keys each candidate by (height in
  metres, distance), lower wins, skipping no-data gates; `Rule::Strongest`
  by code rank. `build` wraps the codes as a `composite::Grid` with the
  owners' credit; `frame` makes the protocol frame.
- **Timing.** `poll(set, sites, events, cached)` is what `main.rs` spawns
  instead of `providers::poll` while `mymosaic` is selected. It places the
  set, loads each radar's stored lowest scans from the tilt store
  (`Slot::compose(Want::Lowest, …)`, no request), then runs one
  `providers::poll_lowest` per radar (the provider's own poller, through
  the store, `BACKFILL + 1` deep via `Config.depth`) on a private channel.
  Scans are keyed by `nominal_ms` (the pollers' own rule: the
  5-minute mark at or before the start plus 1 minute).
  `Schedule` decides when: a frame time is built once, as soon as every
  radar has it (a short scan together with the longer scan its far ring
  comes from), or once it is `DUE_MS` (8 min) old and no scan has arrived
  for `QUIET_MS` (30 s); the newest `BACKFILL` (12) due times the catalog
  lacks, newest first. A time with no scan in hand is never
  built; a scan that comes after its frame was built builds it once more
  (`LATE_MS`: one of the newest two, under 12 min old). A live engine keeps
  its set in `<cache>/mosaic.json` across restarts (`save`, `load`). Frames go to `main.rs` as `Scan::Mosaic` events and
  are catalogued like a composite's (code texture included). The log says
  `Mosaic mymosaic: … built in … ms` with the frame's provenance (radars
  used, missing, far rings, and the summed `N range requests, B of T bytes`
  of the scans it read), and `… encoded in … ms, texture … KB, codes … KB`.
  Each radar's newest volume logs `<radar> HH:MMZ in at T + m:ss`, with
  `, after its frame was built` when it came too late for its frame: the
  data for judging `DUE_MS` against the providers.

**At a height (S30).** The rule `Rule::Height` (`set_mosaic` `rule`
`height`, with `heightM` and `above`) slices the chosen radars at one height,
above sea level or above the ground:

- **Whole volumes, through the tilt store.** Its pollers run with
  `Want::ColMax` (every scan of each volume; at 0.5–6 km every SMHI scan holds
  the height somewhere within 240 km, so "only the scans a height needs" is
  the whole volume anyway), `HEIGHT_BACKFILL + 1` (7) deep instead of 13 when the set has an SMHI
  radar (`Layout::backfill`; a set of ORD radars, 1 request a file, stays
  13), and the log says what the first fill should cost (`first fill: about
  N range requests …`). The
  events only say a volume is in; `build_and_send` reads each radar's scans
  back from the store (`volume_of`) on the blocking pool while it builds, so
  no decoded volume is held between frames. A stored volume counts at start
  only when every scan of it is stored (`stored_whole`). A height or `above`
  change is a new set: its frames come from the store with no range request.
- **One height rule.** `combine_height` asks, per radar and texel,
  `products::nearest_beam`, the same function a radar's `CAPPI` uses: of the
  scans whose gate at the texel's ground distance is one of theirs and whose
  beam holds the target (`|eH − e| ≤ 0.5°`), the centre nearest it, the
  lower angle on a tie. A radar whose pick is no data there drops out; of
  the rest the centre nearest the target to the metre wins, the nearer radar
  on a tie (`height_key`). Above sea level the pick depends on the distance
  alone and is tabled once per radar per frame; above the ground the target
  is `heightM + terrain − altM` per texel, the terrain read from the grid
  texel that is the mosaic's texel (`Layout::lattice`).
- Frames are `product` `CAPPI`, named `Height 3 km` or `Height 1 km above
  ground`, so clients hatch their holes ("no radar at this height").

**Terrain (S30).** `src/terrain.rs` embeds `data/terrain-nordic-2km.bin`
(1976 × 2556 texels, one mean height per 2 km Mercator texel in 10 m steps,
row-delta zlib, 1.0 MB; highest texel 2,230 m), inflated on first use.
`scripts/terrain-grid.py` makes it from Terrarium z8 tiles (sources and
licence in `data/README.md`). A radar's `CAPPI` above the ground
(`Want::CappiGround`) looks up the terrain under each output gate along its
ray on the 6,371 km sphere; `products::needed` reads every scan that could
hold a height from 0 to the grid's highest ground above the base, a superset
of what `compose` picks.

Costs: a Vara + Hurum + Sindal layout is 601 × 691 texels; release builds
place it in 16 ms and combine a frame in 6 ms; twelve radars across the
Nordics (1576 × 1915) take 78 ms and 25 ms (`cargo test --release mosaic`);
the same twelve with ten angles each slice at 2 km in 98 ms above sea level
and 192 ms above the ground (S30). A height set's Vara volume is about 39
range requests, a Norwegian or Danish one 1 request (read whole).
A radar also kept warm (`OMASTORM_WARM`) is polled twice while the mosaic
shows (its listings; its volumes come from the tilt store once either
poller has read them). `OMASTORM_WARM` refuses `mymosaic` itself.

### The composites' products (S24b)

`sweden` and `nordic` offer `LOWB`, `CAPPI`, `CMAX`, `ETOP` and `VIL` besides
`REF` (the provider's own composite). `mosaic.rs` makes them from every radar
of the network (`Layout::grid`, `Job::Grid`; `sweden` SMHI's twelve, `nordic`
all 41), each at its full range, on the composite's box on the 2 km lattice
(`nordic` 1671 × 2297, `sweden` 1365 × 1984). `main.rs` starts
`mosaic::poll_grid` in place of the composite's poller while the station
shows a product (`restart_live`). A grid station always opens on `REF`, the
choice before it kept aside for the next radar (`Shared::aside`), so a client
older than S24b, which hides the chooser on a composite, never starts one.

- **Per radar** (`combine_grid`, one radar's volumes decoded at a time):
  `CMAX`, `ETOP` and `VIL` compose the radar's own product
  (`products::compose`) and place it at its frame's elevation, the maximum
  winning (`max_radar`, `max_key`; of two equal storm heights the exact one
  over the "at least"); `CAPPI` is S30's height rule (`height_radar`, shared
  with `combine_height`); `LOWB` takes each radar's clear beam per degree of
  bearing (`clear_radar`: `products::hybrid_pick` with the blockage table,
  else the lowest scan), the lowest beam centre above sea level winning, the
  nearer radar on a tie.
- **Reads**: every scan (`Want::ColMax`) for `CMAX`, `ETOP`, `VIL` and
  `CAPPI`, so one read serves the four and `LOWB`; `LOWB` alone reads its
  clear beams' scans (`Want::Hybrid`; an FMI radar its lowest file). They
  build back 6 and 12 frame times; `OMASTORM_GRID_BACKFILL=n` (1–12) lowers
  both. The frames are built from the tilt store, so it must be on; the
  schedule keeps only each scan's geometry (`geometry`), not its rays.
- **Cost** (S24b live run, 2026-09-15): the first whole-volume fill of
  `nordic` read two volumes a radar: SMHI 1,105 requests and 18.1 MB (about
  46 a volume with the listings), ORD 314 requests and 50.2 MB, in four
  minutes. A release build of a real frame from the store took 2.2–2.8 s for
  33 radars and 2.5–4.1 s for 39 (measured beside a headless browser; `VIL`'s
  4.1 s before its Z table); about 2 s of that is the tilt store's
  per-tilt `used_ms` commit (S27's `Store::tilt`, 5.6 ms each). Without the
  store, 41 synthetic radars: placement 0.25 s, `CMAX` 0.40 s, `ETOP` 0.52 s,
  `VIL` 1.30 s, `LOWB` 0.30 s, `CAPPI` 0.31 s (`cargo test --release
  mosaic::tests::the_nordic -- --nocapture`). A frame's PNGs: texture 130–810
  KB, code texture 50–390 KB.
- **Rollback**: an engine older than S24b lists no products for the
  composites and makes none; its `variant_of` (S24a review #3) keeps
  catalogued `sweden-…-cmax`, `-etop`, `-vil`, `-cappi…`, `-lowb` frames in
  rings of their own, so they never join the composite's loop. To drop them:
  `DELETE FROM frames WHERE (id LIKE 'sweden-%' OR id LIKE 'nordic-%') AND
  id NOT LIKE '%-e0'`, and their PNGs.

The station table's source, retrieval date, and caveats are in `data/sites.json`
and hello. It includes archived and test sites; membership does not imply live
availability. An archived scan retains its measured coordinates.

## Basemap

`build.rs` converts Natural Earth lines to a compact polyline blob and embeds
populated places for map labels. GeoNames cities with population ≥ 5000,
clipped to the same envelope, are the location-picker gazetteer. The 1:50m
set is global; the 1:10m set is clipped to the Nordic envelope in `build.rs`. `src/tiles.rs` rasterizes these with `tiny-skia`,
using 1:50m below z5 and 1:10m from z5. Segments outside a tile are skipped.

`src/osm.rs` serves OpenMapTiles vector data from z7 through z14, with Natural
Earth fallback. `OMASTORM_TILES_URL` overrides the default OpenFreeMap TileJSON
URL for development. Requests use a ten-second timeout and at most four
concurrent fetches. Transport errors, 429s, and 5xx responses trigger a
30-second backoff; cached tiles still serve.

Vector tiles persist under
`$XDG_CACHE_HOME/omastorm-se/vt/<source>/<version>/<z>/<x>/<y>.pbf`.
The two newest data versions are retained. Above 512 MB, eviction removes the
least recently read tiles until usage falls below 448 MB. Rendered masks are
runtime files capped at 4,096; their names include build/data generation tags.
The protocol documents mask channels, labels, attribution, and path validation.

## Verification

Required checks and capture commands are in
[CONTRIBUTING.md](../CONTRIBUTING.md#verify-and-submit).

The decoder tests compare every moment byte, ray angle, timestamp, and gate
geometry with `golden/ktlx-20130520/`. Other tests cover partial sweep assembly,
catalog retention, deterministic tile rendering, cache eviction, protocol
validation, client isolation, daemon replacement, and texture retirement.
Recorded vector-tile fixtures have provenance in `data/vt/tiles.json`.

The rendering check compares all three treatments against the shader's sampling
rule replayed in Rust over golden codes. It checks default and zoomed views,
weak-return filtering, folded and below-threshold codes, and coverage edges.
Samples near gate or azimuth boundaries allow either neighbor to account for
GPU precision; other mismatches fail. A second test pans the full map by whole
3 px cells and checks that radar, tiles, and overlays shift together within
premultiplied rounding tolerance, without relaying out labels or crossing a
tile edge. Captures and validation reports are written to `review/`.
