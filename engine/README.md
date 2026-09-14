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
