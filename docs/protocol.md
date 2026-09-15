# Engine to UI protocol

Version 2. The Rust engine (`omastorm-engine`) is the server. The Quickshell UI is a thin client. Radar values never
travel over this protocol; they go to the GPU as texture files.

Version 2 adds SMHI's national composite beside the polar radars. Every frame
says which `kind` of texture it carries (`polar` or `grid`), and every `hello`
station says which kind of radar it is. A version 1 client latches "unknown
version" at the first message. That is intended, because it would otherwise
draw a grid texture as a polar sweep. [Changes from version 1](#changes-from-version-1)
lists every difference.

## Transport

- Unix stream socket at `$XDG_RUNTIME_DIR/omastorm-se/engine.sock`.
- Newline-delimited JSON, UTF-8, one object per line, no pretty printing.
- Multiple clients may connect (window and popover). Every client receives every
  broadcast. Commands from any client apply to the shared state.
- Every message has `"type"`. Engine messages also carry `"v": 2`. A client that
  sees an unknown `v` shows an error and stops rendering radar.
- Key order within an object is not significant; clients read keys by name.

## Engine messages

`hello` is sent once on connect, followed immediately by a full `state`.

```json
{"type":"hello","v":2,"engine":"0.1.1",
 "sites":[{"id":"vara","name":"Vara","state":"Västra Götaland",
           "lat":58.25565,"lon":12.82602,"altM":164.0,"kind":"polar",
           "country":"SE","provider":"smhi","rangeKm":240.0,
           "attribution":"SMHI, CC BY 4.0","aliases":["sevax"]},
          {"id":"sweden","name":"Sweden","state":"",
           "lat":62.0,"lon":16.0,"altM":0.0,"kind":"grid",
           "country":"SE","provider":"smhi","rangeKm":0.0,
           "attribution":"SMHI, CC BY 4.0","aliases":[]}]}
```

Each station has a `kind`. `polar` is a single radar. `grid` is a
composite (`sweden`), which covers many radars at once. A grid station's
`lat` and `lon` mark the middle of its coverage, for the picker and for
centring the map. It has no antenna, so a client draws no station marker,
range ring, or coverage circle for it.

Every station also carries the following fields, always sent (S14). They
are additive, so the version stays 2 and an older client ignores them:

- `id`: the station id every command and `state.site.id` use (DEC-12). A
  Swedish radar's id is its SMHI area key (`vara`, `balsta`); every other
  radar's is its ODIM node code in lowercase (`nohur`, `fikor`, `dksin`); a
  composite's is a region word (`sweden`, `nordic`). Ids are unique across
  the table, aliases included. Clients treat them as opaque.
- `aliases`: other ids the engine accepts for this station in `select_site`,
  so a stored or typed id keeps working. A Swedish radar lists its ODIM node
  code (`vara` has `["sevax"]`). Empty when there are none.
- `country`: the ISO 3166-1 alpha-2 code of the network the station belongs
  to (`SE`, `NO`, `FI`, `DK`); empty for a composite that spans countries.
- `provider`: where the engine gets the station's data: `smhi` (SMHI's open
  API), and from S15/S16 `ord` (EUMETNET Open Radar Data), `fmi-s3` (FMI's
  bucket), `opera` (the OPERA composite). It is for display and grouping; a
  client never needs it to draw.
- `rangeKm`: the far edge of the lowest tilt's last gate, in kilometres of
  slant range, for markers and range circles (240 for SMHI's radars); 0
  for a grid station.
- `attribution`: the credit the data's licence asks for, shown verbatim
  (`SMHI, CC BY 4.0`). While drawing, a client credits the frame on
  screen's own `frame.attribution`.
- `products` and `elevations` (S20): what the station can show and the
  radar's own scan angles; both `[]` for a grid station
  ([Products](#products)).

`hello` also carries `products` (S20), the engine's product vocabulary in
the order a chooser lists them ([Products](#products)).

`hello` also carries `referenceSites` (S23, additive, always sent): the
other European weather radars, whose positions a client may show for
orientation. They are **not stations**: no `select_site` accepts their ids,
the picker does not list them, `view_center` never hands off to them, and a
client draws no range circle for them, only a small, faint mark with the
name on tap or hover.

```json
"referenceSites":[{"id":"frabb","name":"Abbeville","country":"FR",
                   "lat":50.13,"lon":1.83,"source":"EUMETNET ORD","retrieved":"2026-09-14"}]
```

- `id`: the radar's ODIM node code, lowercase. It never equals a station's
  `id` or alias: a radar the engine can show is in `sites`, not here.
- `name`: the site's name as its source writes it; `country`: ISO 3166-1
  alpha-2 (`GB` for the United Kingdom).
- `source`: where the position comes from, shown as the credit while the
  marks are on screen: `EUMETNET ORD` (the Open Radar Data locations
  list) or `EUMETNET OPERA database` (OPERA's radar database, for radars
  ORD does not list). `retrieved` is the snapshot's date.
- The list is a snapshot embedded in the engine
  (`engine/data/reference-radars.json`, written by
  `scripts/fetch-reference-radars.sh`); it says where radars stand, not
  whether they are running. An engine older than S23 sends no
  `referenceSites`; a client then draws no marks.

`state` is the complete current state, re-sent whenever anything in it changes.
It is small (a few KB) so clients replace rather than merge.

```json
{"type":"state","v":2,
 "source":"archived",
 "connection":{"status":"ok","ageSeconds":0},
 "site":{"id":"vara","follow":true,"locked":false},
 "frame":{"id":"vara-20260913T105503Z-e0","kind":"polar",
          "product":"REF","productName":"Reflectivity","units":"dBZ","elevationDeg":0.5,
          "scanTime":"2026-09-13T10:55:03Z","sweepEnd":"2026-09-13T10:55:33Z",
          "status":"complete",
          "texture":"tex/sweep-vara-20260913T105503Z-e0-r3.png",
          "azimuthLut":"tex/azlut-vara-20260913T105503Z-e0-r3.png",
          "rays":360,"gates":480,"firstGateM":250,"gateSpacingM":500,
          "site":{"lat":58.25565,"lon":12.82602,"altM":164.0},
          "palette":["#34465f","..."],"bounds":[-32,0,10,20,30,40,45,50,55,60,65,70,96]},
 "timeline":[{"id":"...","scanTime":"...","status":"complete"}],
 "basemap":{"ne":{"version":"5.2.0-pre"},"osm":{"status":"ok","source":"OpenFreeMap",
            "version":"20260830_080001_pt","attribution":"OpenFreeMap © OpenMapTiles Data from OpenStreetMap"}},
 "playing":false}
```

- `source`: `archived` | `live`. The daemon starts `live` with no station:
  `site.id` is empty, `connection.status` is `loading`, the frame is the
  placeholder below, and the first `select_site` goes live on a station.
  Started with `OMASTORM_ARCHIVE` naming an ODIM `.h5` volume or a NEXRAD
  Level II volume (development, captures, and the checks) it starts
  `archived` on that scan instead.
- `connection.status`: `ok` | `stale` | `unavailable` | `offline` | `loading`.
  `ageSeconds` is the age of the newest complete frame (0 while there is
  none). `connection` is the only place a lasting error condition lives; it
  describes the engine's data path and no client command can clear it. In live
  mode, the status is `loading` from a `select_site` until the station's first
  frame arrives or the poller reports; then, with the provider reachable, the
  status follows the age of the newest scan the station has published,
  judged against the station's provider's cadence (DEC-9 generalized):
  `stale` from the cadence plus 10 minutes, `unavailable` from the larger of
  30 minutes and the stale threshold plus one cadence, unless a provider
  documents its own. For SMHI (5-minute cadence) that is `ok` under 15
  minutes, `stale` from 15 minutes, `unavailable` from 30 minutes (SMHI is
  up and the station is silent: maintenance or an outage; Leksand has
  published nothing since January 2026). A healthy SMHI frame is already 5 to
  10 minutes old, which is why the thresholds are not upstream's 10 and 30
  (DEC-9). `offline` is the provider unreachable, or answering errors twice
  in a row, cleared by the next frame. Cached frames
  stay in `timeline` under every status. While live the engine re-judges
  once a second and broadcasts when anything changed, so `ageSeconds` and
  the status move on a quiet feed. A `message` is added if a status ever
  needs words.
- `timeline` is every frame a client can `seek` to, oldest first:
  the station's complete frames from its catalog (the newest 60) and, while a
  sweep is painting, that sweep as the last entry with `status` `partial`
  (`complete` otherwise). `frame` is one of them. Each entry also names its
  own textures (S19), so a client can fetch any frame of the loop without a
  `seek` ([timeline textures](#timeline-textures)). The engine owns the
  position: a new sweep replaces `frame` while it is the newest entry, and
  leaves it alone once a client stepped or sought elsewhere, until a step or
  seek lands on the newest entry again. Archived, the timeline is the one
  archived frame; before any `select_site` it is empty.
- `playing` is true while the engine advances `frame` one complete timeline
  entry at a time, oldest after newest, pacing the loop to about ten seconds
  (250 ms to 1 s per frame, by how many there are). The sweep in progress is not part of
  the loop.
- `frame.status`: `complete` | `partial`. Partial frames are live sweeps still
  being filled; the texture path changes on every republish (revision suffix).
  While a station's first live sweep loads and nothing is cached, the frame is
  a placeholder that draws nothing: `id` `<SITE>-loading`, `status` `partial`,
  `rays` 1, `gates` 1, empty `scanTime` and `sweepEnd`, the station table's
  coordinates. Before any station is selected the placeholder is `-loading`, sited
  at the middle of the contiguous network.
- Paths are relative to `$XDG_RUNTIME_DIR/omastorm-se/` and have the form
  `tex/<file>`: the literal prefix `tex/` and exactly one further segment that
  is not empty, `.`, or `..` and contains no `/`, backslash, or NUL. The file
  name is otherwise free and carries no meaning to the UI. Both ends apply this
  one rule: the engine refuses to publish a path that breaks it, and the UI
  rejects a `state` whose path breaks it.
- `frame.product` is the code commands use; `frame.productName` is its display
  name. The engine owns product, unit, and site vocabulary; the UI only cases
  and lays out what it receives, and looks the site name up in `hello.sites`.
  Since S20 a radar's frame is the chosen product ([Products](#products)):
  `REF` (`Reflectivity`, one scan angle) or a product built from several
  angles, whose `elevationDeg` places the texture but is not an angle to
  show.
- `product` (S20) is the engine's chosen product, `{"id":"REF",
  "elevationIndex":0}`, shared by every client like the station
  ([Products](#products)).
- `frame.palette` has one color per class, in class order, and `frame.bounds`
  has one more entry than `palette`: class `i` covers `bounds[i]` up to
  `bounds[i+1]` in `units`. The UI uploads `palette` to the GPU as a
  `palette.length` × 1 texture and labels the legend from `bounds` (first band
  `<bounds[1]`, last band `bounds[n-1]+`), so any band count works and radar
  and legend share one source of color.
- `frame.scale` and `frame.offset` are the moment's own encoding:
  value = (code − offset) / scale in `units`. The UI uses them to place the
  weak-return floor, a view setting in `units`, in code units for the shader
  (lookup rule, below). Both are 0 on the loading placeholder, which
  therefore has no floor.
- `frame.attribution` is the credit for this frame's data, shown verbatim
  (`SMHI, CC BY 4.0`), always sent (S14, additive). It is the station's
  `attribution` when the frame was made; a frame catalogued before S14 gets
  its station's. Before any `select_site` it is empty, and an archived
  Level II frame (the checks' KTLX) credits `NOAA NEXRAD Level II`.
- `frame.kind` is `polar` or `grid`, and is always present. A client that
  gets any other value rejects the `state`, as it would a bad texture path.
  - `polar`: a sweep texture and its azimuth lookup, placed by `rays`,
    `gates`, `firstGateM`, `gateSpacingM`, and `elevationDeg` (texture
    files, below). The loading placeholder is always `polar`.
  - `grid`: a national composite that the engine has already reprojected
    to Web Mercator, so a client draws it as one textured rectangle.
    `texture` is the grid texture and `azimuthLut` is empty (`""`). `rays`,
    `gates`, `firstGateM`, and `gateSpacingM` are 0. `elevationDeg` is the
    elevation the composite is built from (0.5 for SMHI, 0 for OPERA). A
    composite has no single scan angle, so a client shows none for a grid
    frame (`Reflectivity · composite`, S23) and must not rely on the
    value. `frame.grid` places the texture:

    ```json
    "grid":{"projection":"EPSG:3857","xsize":1364,"ysize":1983,"xscale":2000,"yscale":2000,
            "west":5.323958,"east":29.829999,"north":70.033942,"south":53.701215,
            "sourceProjdef":"+proj=stere +ellps=bessel +lat_0=90 +lon_0=14 +lat_ts=60 +towgs84=0,0,0"}
    ```

    `xsize` × `ysize` is the texture's size in pixels, and `xscale` ×
    `yscale` is one pixel's size in Web Mercator metres (sphere radius
    6,378,137 m). `west`, `east`, `north`, and `south` are the texture's
    outer edges in degrees. `sourceProjdef` names the grid the engine
    reprojected from; it is for display and debugging only. `frame.site` is
    the composite's table position, and a client uses it only as the
    reference point for the camera's Mercator offset, as it does for a
    polar frame. `frame.grid` is absent from polar frames.

`error` answers one command from one client. It goes only to the client that
sent the command, `state` does not change, and nothing is broadcast, so a
mistake from the popover cannot erase or flash a condition on the window.
`command` is the command's `type` as the client wrote it; `message` is shown
verbatim. A client shows its latest rejection beside the state until it sends
its next command, ahead of any `connection` condition; a `tiles_needed` the
map sends on its own does not count as the user's next command.

```json
{"type":"error","v":2,"command":"select_site",
 "message":"Unknown site XXXX; stations are listed in hello."}
```

`tile_ready` answers one `tiles_needed` request, tile by tile, to the client that sent it. Like
`error` it is a reply, not shared state. A tile already rendered this session
is answered at once; one the engine cannot produce is not answered, and the
lasting condition shows in `state.basemap.osm.status`. `set` says which data
drew the tile: `osm` when the vector tile was cached or fetched, `ne` (Natural
Earth) otherwise, in which case the tile is announced again under a new path
when `osm` becomes available. `labels` are the tile's places for the overlay.

```json
{"type":"tile_ready","v":2,"set":"osm","z":11,"x":470,"y":808,
 "path":"tiles/osm/11/470/808-3f9a1c2e.png",
 "labels":[{"name":"Moore","lat":35.3395,"lon":-97.4867,"class":"city","rank":8}]}
```

- `path` is relative to `$XDG_RUNTIME_DIR/omastorm-se/` and has the form
  `tiles/<set>/<z>/<x>/<file>`: the literal prefix, `ne` or `osm`, two
  decimal integers, and one further segment under the texture rule (not
  empty, `.`, or `..`; no `/`, backslash, or NUL). The file name is otherwise
  opaque; it ends in a generation tag so pixels never change under a name the
  UI has cached. Both ends apply the rule.
- `labels[].class` is `capital`, `city`, `town`, or `village`; `rank` is
  lower for more important places (OpenMapTiles `rank`, Natural Earth
  `scalerank`). An `ne` tile carries the Natural Earth places inside it whose
  `min_zoom` is at most `z + 1`, since a 512 px tile shows the ground of four
  256 px tiles one level deeper.
- `state.basemap` describes the tile sources:
  `{"ne":{"version":"5.2.0-pre"},"osm":{"status":"ok","source":"OpenFreeMap",
  "version":"20260830_080001_pt","attribution":"OpenFreeMap © OpenMapTiles Data from OpenStreetMap"}}`.
  `osm.status` is `ok`, `offline` (fetching fails; cached tiles still serve),
  or `unavailable` (no data version known and nothing cached); `osm.version`
  is empty until one is known. `source` and `attribution` come from TileJSON
  (`name`, and `attribution` with its HTML reduced to text) once it has been
  read, and name OpenFreeMap by its host before that. The UI shows
  `osm.attribution` verbatim whenever an `osm` tile is on screen. It is
  shared state: a change is broadcast like any other.

## Client commands

```json
{"type":"select_site","id":"vara"}
{"type":"follow","enabled":true}
{"type":"lock","enabled":false}
{"type":"view_center","lat":58.3,"lon":12.8}
{"type":"search_places","query":"lidköping","lat":58.3,"lon":12.8}
{"type":"play"}  {"type":"pause"}  {"type":"step","delta":-1}  {"type":"seek","id":"..."}
{"type":"set_product","product":"REF","elevationIndex":0}
{"type":"set_product","product":"CAPPI","heightM":3000}
{"type":"tiles_needed","z":11,"x0":469,"y0":807,"x1":472,"y1":810}
```

- `select_site` names a station from `hello.sites`. The engine goes
  live on it: the newest frame in its catalog (the per-station ring buffer) or
  the loading placeholder shows at once with `connection.status` `loading`,
  and a poller replaces the previous station's; the same station again changes
  nothing. `id` may be a station's `id` or one of its `aliases`, matched
  exactly first and then ignoring ASCII case; either way `state.site.id`
  becomes the station's `id` (so `select_site` `sevax` shows `vara`). This
  is how a `locked_radar`, a `state.json` lock, or a typed id keeps working
  when a station gains a new id. An id outside the table is answered with an
  `error`.
- `view_center` is sent when a pan or zoom settles and the centre moved, not
  per frame. With `follow` on and `lock` off, the engine hands off to the
  station nearest the centre by great-circle distance when that station beats
  the current one by the hysteresis rule (closer than 0.8 of the current
  station's distance and by at least 1 km, so a centre between two stations
  keeps whichever it has; the dead band is about a twentieth of the spacing
  either side of the midpoint); the hand-off is a `select_site`, so `state`
  is broadcast and an uncached station opens on the loading placeholder.
  Locked or not following, or when the current station stays nearest,
  nothing changes and nothing is sent. The engine never moves the camera:
  the centre is the user's. A latitude outside ±90 or a longitude outside
  ±180 is answered with an `error`. `lock` and `follow` are shared flags;
  releasing the lock hands off on the next settle, not at once. A `grid`
  station is never a hand-off target. While one is selected, `view_center`
  hands off to nothing: the composite already covers the view, so only a
  `select_site` leaves it.
- `search_places` ranks the embedded gazetteer (GeoNames populated places
  with population ≥ 5000 in Sweden, Norway, Finland, Åland, Denmark and the
  Baltics, clipped to the SMHI network's Nordic envelope) for the
  location picker and is answered with `places` to the sender only, like
  `tile_ready`. Map labels stay on Natural Earth. `query` is required;
  optional `lat` and `lon` order nearer matches first. Word-start matches
  beat substrings; matching ignores diacritics ("orebro" finds Örebro) and
  also tries each place's other names ("gothenburg" finds Göteborg), below
  the name as typed. Results keep the local name. At most eight results. A
  blank query returns no results.
  A latitude or longitude outside range is answered with an `error`. The
  reply is not shared state:

```json
{"type":"places","v":2,"query":"göteborg",
 "results":[{"name":"Göteborg","lat":57.70716,"lon":11.96679,"class":"city","rank":3,
             "region":"Västra Götaland","country":"SE"}]}
```
  `region` is the admin-1 name (a Swedish county, a Norwegian county);
  `country` is the ISO 3166-1 alpha-2 code. Either may be omitted when empty.
- `set_product` chooses the product radars show: `product` is an id from
  `hello.products` (or one of the aliases `CAPPI1`, `CAPPI2`),
  `elevationIndex` (optional, 0 when absent) an index into the selected
  station's `elevations`, used with `REF` only, and `heightM` (optional,
  S29) the height above sea level in metres, used with `CAPPI` only. An
  unsupported selection returns an `error` to its sender and retains the
  current frame. The rules are in [Products](#products).
- `step` moves `delta` entries along `timeline` from the frame shown, stopping
  at the ends; `seek` shows the entry with `id`. Both stop playback. A stepped
  catalogued frame shows under the textures its timeline entry names (the
  same paths every time it is shown); the sweep in progress is republished
  under new paths; an `id` outside the timeline is answered with an `error`,
  and a move that lands where it already is changes nothing. `play` starts
  the loop when the timeline holds at least two complete frames (otherwise
  nothing changes); `pause` stops it and leaves the frame shown.
- `tiles_needed` is the visible inclusive rectangle at one zoom, at
  most 64 tiles, sent when the viewport settles; it names no set (the engine
  chooses, see `tile_ready`). The engine serves it centre-out, and a newer
  request from the same client supersedes its pending tiles outside the new
  rectangle. Unknown commands are ignored and
  logged. A known command with a missing or mistyped field, or one asking for
  something this build cannot serve, is answered with an `error` event to its
  sender only. `state` is broadcast only when a command changed something, so
  repeating a command does not repeat the broadcast.

## Texture files

Directory `$XDG_RUNTIME_DIR/omastorm-se/tex/`. Every file is written to a temporary
name and renamed into place. Files are never modified after rename; a change
produces a new name, so no client cache (Qt's, a browser's) can ever show
stale pixels. The engine deletes files no `state` has referenced for 30 s;
`frame` and every `timeline` entry count as references.

Two kinds of name, both opaque to clients:

- A **catalogued frame** (every `complete` timeline entry of a live station)
  has one name per texture for as long as the catalog holds that frame:
  `tex/sweep-<frame id>-<tag>.png` and `tex/azlut-<frame id>-<tag>.png`,
  where `<tag>` is 8 hex digits derived from the texture's content (for
  frames catalogued before S19, from the catalog file's size and time).
  Showing the frame again, stepping back to it, looping over it, an engine
  restart: the name is the same, and so are its bytes. The file is a
  symbolic link into the engine's frame catalog; a client just opens the
  path. Both files stay published while the frame is in the selected
  station's `timeline`, so a client may fetch them at any time; after a
  station switch they retire 30 s later like any other unreferenced file.
- Anything else (the sweep in progress, the loading placeholder, an
  archived `OMASTORM_ARCHIVE` frame) is a copy under a name with the
  engine's process id and a nanosecond revision, new on every publish.

### Timeline textures

Additive since S19 (still version 2). Every `timeline` entry carries its
own texture paths, and a `placement` when it would draw differently from
`frame`:

```json
"timeline":[{"id":"vara-20260914T101003Z-e0","scanTime":"2026-09-14T10:10:03Z",
             "status":"complete",
             "texture":"tex/sweep-vara-20260914T101003Z-e0-3f9a1c2e.png",
             "azimuthLut":"tex/azlut-vara-20260914T101003Z-e0-3f9a1c2e.png"}]
```

- `texture` and `azimuthLut` follow `frame`'s rules for the entry's kind: a
  polar entry names both, a grid entry names its texture and an empty
  `azimuthLut`. Both are `""` when the entry has no stable textures: the
  `partial` sweep in progress, and the one frame of an archived start.
  A client that wants such a frame uses `frame` after a `seek`.
- `placement` is present only when the entry's placement differs from
  `frame`'s, and then carries all of it: `kind`, `rays`, `gates`,
  `firstGateM`, `gateSpacingM`, `elevationDeg`, `site`, and for a grid
  `grid`. Absent, the entry draws with `frame`'s. In practice a station's
  frames share one placement, so entries rarely carry one; this keeps the
  `state` broadcast (once a second while live) small. A client that
  buffers frames resolves each entry's placement against the `state` it
  came in and keeps it with the frame.
- `codes` (grid entries only, absent when empty): the entry's
  [code texture](#texture-files), one byte per texel instead of four, a
  quarter of the GPU memory and about a quarter to a half of the PNG. A
  client that can apply the class rule itself loads `codes` instead of
  `texture`; `texture` stays for every other client. Frames catalogued
  before S19 have none.
- Product, units, palette, bounds, `scale`, `offset`, and `attribution`
  are the station's and the same for every entry; they come from `frame`.
- A client may therefore fetch, decode, and upload the whole loop once,
  play it without the engine, and still `seek` when the user pauses or
  scrubs so other clients follow. The engine's own `play` keeps working
  as before.
- An engine older than S19 sends entries without `texture`; a client falls
  back to `seek` and `frame` for them.

**Sweep texture (`frame.texture`):** PNG, RGBA, width = gates, height = rays.
Rows are radials sorted by azimuth. Nearest sampling, no mipmaps. When the
radials leave a gap (a live sweep still being filled, a dropout), one blank
row (R, G, B zero) follows the radials and `rays` counts it; the azimuth
lookup names it for every entry farther than 0.75° from any radial, so
unscanned azimuths draw nothing instead of the nearest radial smeared around
the circle. A complete 0.5° or 1° cut needs no blank row.

| Channel | Meaning |
| --- | --- |
| R | palette class + 1; 0 means nothing to draw |
| G | status bits: 1 range folded, 2 below threshold, 4 outside coverage |
| B | moment byte in the Level II convention (ODIM DBZH requantized, `code = dBZ × 2 + 66`), for cursor inspection and the weak-return floor |
| A | 255 |

**Azimuth lookup (`frame.azimuthLut`):** PNG, RGBA, width 3600, height 1.
Entry `i` covers azimuth `i / 10` degrees and names the row whose azimuth is
nearest the entry's center, wrapping at 360, or the blank row when none is
within 0.75°; R and G hold the row index as a little-endian 16-bit value.

**Grid texture (`frame.texture` of a `grid` frame):** PNG, RGBA, width
`grid.xsize`, height `grid.ysize`, with the sweep texture's channels (R class
+ 1, G status bits, B raw code, A 255). Row 0 is the north edge. Texel
(c, r) covers Web Mercator x from `west` + c·`xscale` to `west` +
(c + 1)·`xscale`, and y from r·`yscale` to (r + 1)·`yscale` south of
`north`. Rows are therefore evenly spaced in Mercator y, not in latitude.
Each texel holds the source pixel that contains its centre (nearest
neighbour). Texels outside the source grid are marked like `nodata`: code 1,
G bit 4 (outside coverage). No lookup table goes with a grid texture.

**Code texture (a grid timeline entry's `codes`, S19):** PNG, 8-bit
grayscale, the grid texture's size and texel order, holding only the raw
code (the grid texture's B channel): 0 below threshold, 1 no data or off
the source grid, 2 to 255 measured. A client rebuilds the grid texel from
it: code 0 or 1 draws nothing; otherwise, with value = (code − `offset`) /
`scale` in `units`, the class is the number of `bounds` at or below value,
minus one, kept within 0 to `palette.length` − 1, and the texel is class + 1
with no status bits, exactly what the engine writes to the grid texture
(composites have no folded gates). Its stable name is
`tex/codes-<frame id>-<tag>.png`, published and retired with the entry's
other files.

**Lookup rule (UI shader, `ui/shaders/radar.frag`):** each 3 px screen cell
becomes a site-relative ground distance and an azimuth clockwise from north:
the cell's centre goes from Web Mercator to longitude and latitude and then,
on a sphere of radius 6,371 km, to the great-circle distance and initial
bearing from the site. Ground distance converts to slant range on the
4/3 effective-radius earth,
`r = R sin(s/R) / cos(elevationDeg + s/R)`, the inverse of pyart's
`antenna_to_cartesian`, so gates land where the golden reference places them.
The nearest gate is `round((r - firstGateM) / gateSpacingM)`; more than half a
gate before the first or past the last draws nothing. The azimuth's entry gives
the row, and the sweep is sampled at `(gate + 0.5) / gates, (row + 0.5) / rays`.
`rays`, `gates`, `firstGateM`, `gateSpacingM`, and `elevationDeg` travel as
shader uniforms; they are geometry, not radar values.
The weak-return floor is the one view setting the shader applies to values: `weakBelow`, a code threshold the UI
derives from `frame.scale` and `frame.offset` for the floor in `units`
(`ceil(floor × scale + offset)`), and a measured code (2 and up) below it
draws nothing, exactly as a blank cell does; 0 is no floor. Folded and
below-threshold codes are never weak, and the legend names the hidden range.

**Grid lookup rule:** the centre of each 3 px screen cell, in Web Mercator
units, becomes a texture position `u = (x − x(west)) / (x(east) − x(west))`,
`v = (y − y(north)) / (y(south) − y(north))`. Outside `[0, 1)` draws
nothing. The texel is sampled nearest at `((floor(u·xsize) + 0.5) / xsize,
(floor(v·ysize) + 0.5) / ysize)`. The palette, treatments, folded marker,
and weak-return floor then apply exactly as for a polar sweep. The shader
gets `kind` as a uniform, plus the grid's rectangle as its north-west
corner's offset from `frame.site` and its size, both in Mercator units.

**Tiles:** `$XDG_RUNTIME_DIR/omastorm-se/tiles/<set>/<z>/<x>/<y>-<gen>.png`,
Web Mercator XYZ numbering, 512 px, RGBA antialiased masks tinted by the
UI's shader (`ui/shaders/tile.frag`):

| Channel | Meaning |
| --- | --- |
| R | boundaries (country, state, province) |
| G | water (shorelines, lake shores, rivers) |
| B | minor roads |
| A | 255 − major-road coverage |

A is inverted because Qt Quick premultiplies an image by its alpha on upload,
so a texel with A = 0 loses R, G, and B. An empty tile is therefore fully
opaque, and the UI shader
recovers the straight channels by dividing by A and reads major roads as
1 − A. An `ne` tile has B zero and A 255 everywhere, since roads exist only
in `osm` data. Sets: `ne` (Natural Earth, embedded in the binary, any zoom) and
`osm` (OpenMapTiles-schema vector tiles fetched lazily from z7; roads exist
only here). Masks are rasterized on demand into the runtime directory, never
modified, and dropped oldest-first past 4,096 files. The fetched vector tiles,
not the masks, are what persists: `$XDG_CACHE_HOME/omastorm-se/vt/<source>/<version>/<z>/<x>/<y>.pbf`,
512 MB ceiling, least-recently-read evicted.

## Golden files

Committed under `golden/<fixture>/`, produced with pyart and consumed by Rust
decoder tests. pyart is not part of the project;
`sweep0.json` records the exact pyart calls and version used so fixture generation
is reproducible.

- `golden/<fixture>/sweep0.json`: `rays`, `gates`, `moment`, `scale`, `offset`,
  `firstGateM`, `gateSpacingM`, `azimuthDeg[]`, `elevationDeg[]`,
  `rayTimeBase`, `rayTimeMs[]`, site coordinates, source file hash, row order,
  and provenance.
- `golden/<fixture>/sweep0.u8`: flat uint8, row-major `rays × gates`,
  moment codes in the Level II convention, in ascending-azimuth row order
  (0 below threshold, 1 range folded in Level II or no data in ODIM,
  2..255 measured; dBZ = (code − offset) / scale). The KTLX fixture holds
  raw Level II codes; the Vara fixture holds ODIM DBZH requantized.

`rayTimeMs` counts from the first radial in decoded (file) order, before the
azimuth sort; `rayTimeBase` names that instant to the second. Angles are
written with four decimals.

Current fixture `ktlx-20130520`: 720 × 1832, first gate 2125 m, 250 m spacing,
195,199 measured gates, no range-folded gates. The Rust decoder test
(`engine/src/sweep.rs`) matches every byte, angle, and ray time exactly.

## Live frames

A live frame is the lowest tilt of the newest SMHI volume of the selected
station, reflectivity (`DBZH`). The poller reads `area/<site>/product/qcvol.json`
every minute and fetches each new volume from its dated URL with HTTP range
requests (about 7 requests and 100 KB of a 15 MB file, DEC-2): `id` is
`<site>-<scanTime compact>-e0`, `scanTime` and `sweepEnd` are the tilt's
start and end times, `elevationDeg` its angle, `site` the station table's
coordinates, and `product`, `palette`, and `bounds` the engine's reflectivity
vocabulary shared with the fixture; the values are requantized with `scale`
2 and `offset` 66. SMHI publishes whole volumes, so every frame is
`complete`; `partial` is never sent. Complete frames enter the per-station
catalog under `$XDG_CACHE_HOME/omastorm-se/frames/` (SQLite catalog plus the
PNGs; 60 per station; the UI never reads it). Selecting a station shows its
newest catalogued frame while the poller fetches the newest volume and
backfills up to 60 from the day listing, newest first, so a picture arrives
within seconds and a loop soon after.

**The composite.** Selecting `sweden` polls `area/sweden/product/comp.json`
in place of a radar's `qcvol.json`, with the same cadence, back-off,
catalog, and 60-frame backfill. Each frame is one ODIM `COMP` file. The
engine reads its `DBZH` layer (DEC-11), requantizes it with the polar byte
convention (`scale` 2, `offset` 66), and reprojects it to the grid texture.
`id` is `sweden-<time compact>-e0`, `scanTime` is the file's nominal time
(the listing's `valid`), `sweepEnd` is its end time, and `status` is always
`complete`. `connection` judges the composite's age against the same
thresholds as a radar's.

**Providers.** Each station names its `provider` (`engine/src/providers/`).
A provider supplies the station's listing and polling, its cadence (and so
its staleness thresholds), its backfill depth, its range-read budget, and
the default `attribution`, `country`, and `rangeKm` of its rows in
`engine/data/sites.json`. SMHI: the poller above, a 5-minute cadence,
60-frame backfill, and DEC-2's range reads. `opera` (S16, DEC-14): the grid
station `nordic`, EUMETNET OPERA's European composite from the Open Radar
Data 24-hour S3 cache, cut to 3–33° E, 53–71.5° N (a 1670 × 2297 texture,
larger than Sweden's 1364 × 1983 and above WebGL2's guaranteed 2048), read
11 of its 30 chunks at a time (~0.9 MB a frame), a 5-minute cadence and a
24-frame backfill, credited `EUMETNET OPERA, CC BY 4.0`, `country` empty. The
frames of every provider share this section's rules: `id` is
`<station id>-<scanTime compact>-e0`, values are requantized to `scale` 2
and `offset` 66, and complete frames enter the catalog under the station
id.

**Keep-warm stations (S19).** The engine's environment variable
`OMASTORM_WARM` names stations, by id or alias and separated by commas
(`OMASTORM_WARM=vara,sweden`), that the engine keeps polling while they are
not selected: each gets a poller of its own that backfills and follows its
catalog exactly as the selected station's does, so a client that opens it
finds its whole history and can buffer the loop at once. Their frames only
enter the catalog; `state` does not change and nothing is broadcast. While
a warm station is selected its ordinary poller covers it. Unknown ids are
logged and ignored; archived starts keep nothing warm; unset or empty keeps
nothing warm (the default, and what the desktop bar's engine runs). Meant
for the web service's engine, whose clients arrive cold; the recommended
value there is the home radar and `sweden`. Every warm station costs what
a selected one costs, all day:

| Station | Requests an hour | Data an hour |
|---|---|---|
| An SMHI radar (`vara`) | ~60 listings + ~7 range reads per volume, ~144 | ~1.2 MB |
| `sweden` (SMHI composite) | ~60 listings + 12 files | a few MB |
| `nordic` (OPERA, S3 cache) | ~12 listings + ~7 ranges per frame | ~11 MB |
| An ORD radar (S3 cache) | ~12 listings + ranges per file | ~1–2 MB |

ORD radars and `nordic` read the Open Radar Data S3 caches, not the ORD
API, so they do not count against its 200 requests an hour (DEC-13,
DEC-14); SMHI's API has no published limit, and the pollers stay as polite
as for a selected station. `nordic` is not recommended warm for its volume.

## Products

Additive since S20 (still version 2). A radar volume holds several scan
angles (SMHI 10, from 0.5° to 40°; MET Norway two alternating sets of 12
and 10; FMI's cache 5; DMI 10). A radar's frame shows one **product**
made from them; the lowest angle (`REF`, elevation index 0) is what every
frame was before S20, and stays the default. Composites have none: their
frame is the composite, whatever product is chosen.

`hello.products` is the vocabulary, in the order a chooser lists them:

```json
"products":[{"id":"REF","name":"Lowest scan"},{"id":"HYBRID","name":"Clear view"},
            {"id":"CAPPI","name":"Height"},{"id":"CMAX","name":"Column max"}]
```

Before S29 the list held `CAPPI1` "Height 1 km" and `CAPPI2` "Height 2 km"
instead of `CAPPI`; they are now aliases `set_product` still accepts (below).

Each `hello.sites[]` entry names what the station can show and its angles:

```json
"products":["REF","CAPPI","CMAX"],
"elevations":[{"deg":0.5,"beamKm50":0.6,"beamKm100":1.5},{"deg":1.0,"beamKm50":1.0,"beamKm100":2.3}]
```

- `products`: a subset of `hello.products`' ids, in that order; `[]` for a
  grid station. Every radar can make `REF`. `CAPPI` and `CMAX`
  are offered where one file holds the radar's every angle and the engine
  knows them (SMHI, MET Norway, DMI); FMI's radars, whose files hold one
  angle each in ORD's cache, offer `REF` alone, with `elevations` `[]`.
  `HYBRID` is offered only where the engine also has a blockage table.
- `elevations`: the radar's scan angles, ascending; the position is the
  `elevationIndex`. `beamKm50` and `beamKm100` are the beam centre's height
  above the antenna at 50 and 100 km ground distance, in km with one
  decimal, for labels (below). The angles are nominal: a volume's own can
  differ a little (MET Norway's two sets give 2.4° or 2.6°, DMI's lowest
  wanders 0.46–0.51°); the engine takes the volume's angle nearest the
  chosen one, and `frame.elevationDeg` says which it was. Empty when the
  engine does not know them.

`state.product` is the chosen product, `{"id":"CMAX","elevationIndex":0}`,
always sent; for `CAPPI` it also carries the height,
`{"id":"CAPPI","elevationIndex":0,"heightM":3000}` (S29; `heightM` is sent
with `CAPPI` only). It is shared engine state, like the station: one
client's `set_product` changes it for every client of that engine.

`frame.product`, `productName`, and `elevationDeg` describe the frame on
screen:

| `product` | `productName` | What | `elevationDeg` |
|---|---|---|---|
| `REF` | `Reflectivity` | one scan angle: the lowest (index 0), or the one chosen | that angle |
| `HYBRID` | `Clear view` | per azimuth, the lowest angle the terrain does not block | the lowest angle (placement only) |
| `CAPPI` | `Height 3 km`, `Height 3.5 km`, … | per gate, of the angles whose beam holds `heightM` above sea level, the one whose beam centre is nearest it; none: no data ("no radar at this height") | the lowest angle (placement only) |
| `CMAX` | `Column max` | per gate, the strongest return of any angle above it | the lowest angle (placement only) |

A client shows an angle beside the product only for `REF`. The texture
format, `units` (dBZ), palette, bounds, `scale` and `offset` are the same
for every product, so nothing else in a client changes.

**`set_product`.** `{"type":"set_product","product":"CMAX"}`,
`{"type":"set_product","product":"REF","elevationIndex":3}` for one angle,
or `{"type":"set_product","product":"CAPPI","heightM":3000}` for a height.
`heightM` is metres above sea level, 500 to 12,000 in steps of 500; absent,
it is 2,000. The aliases `CAPPI1` and `CAPPI2` are `CAPPI` at 1,000 and
2,000 m (any `heightM` sent with them is ignored), so a client that still
offers them keeps working; `state.product` then says `CAPPI`.
It is answered with an `error`, and nothing changes, when the id is neither
in `hello.products` nor an alias, no station or a grid station is selected,
the selected station cannot make it, `elevationIndex` is not an index of its
`elevations` (any index but 0 with another product), or `heightM` is outside
500–12,000, not a multiple of 500, or sent with a product other than `CAPPI`.
Otherwise, when it differs from `state.product` (another height is another
choice), `state.product` changes and the selected
radar's `timeline` becomes its history of that product: the newest frame
it has shows at once, or the loading placeholder with `connection.status`
`loading`; its poller fetches the newest volume for the product and then
backfills newest first, like a `select_site`. The same selection again
changes nothing.

**Station switches.** The product carries over a `select_site` or a
hand-off. A single angle maps to the new radar's angle nearest in degrees;
a product the new radar cannot make falls back to `REF` index 0; a height
stays the same height above sea level, so over a radar on higher ground it
reaches less far; either way `state.product` says what is in effect. While a grid station is
selected `state.product` keeps the choice for the next radar.

**History.** The catalog keeps each radar's frames per product. After a
switch the lowest scan backfills its 60 frames as before; any other
product backfills the newest 12 on an SMHI radar (an hour; each of its
volumes costs about 39 range requests) and the newest 24 on an ORD radar
(two hours, one request a file), and its ring fills to 60 as live frames
arrive. CAPPI, CMAX and HYBRID read the lowest scan anyway, so each of
their frames also adds the lowest scan's frame of the same volume to the
station's `-e0` ring, at no extra request. The engine keeps a station's
lowest scan and at most two other products (the current one and the one
before); choosing a third deletes the oldest's frames. Keep-warm stations
(`OMASTORM_WARM`) keep their lowest scan warm whatever product is chosen.
A frame's `id` ends in the product (`-e0` for the lowest scan, as before);
ids stay opaque.

**How each product is made.** Heights use the 4/3 effective earth radius,
R = 8,494,667 m, and the lookup rule's relation between ground distance
`s` and slant range `r` at elevation `e`: `r = R sin(s/R) / cos(e + s/R)`,
with the beam centre `h = R cos(e) / cos(e + s/R) − R` above the antenna.

- `REF` index *n*: the volume's scan nearest the angle `elevations[n]`,
  exactly as the lowest scan is decoded (index 0 is the lowest scan
  itself).
- The other products are drawn on the lowest scan's rays and gates:
  `rays`, `gates`, `firstGateM`, `gateSpacingM` and `elevationDeg` are the
  lowest scan's (`elevationDeg` its nominal angle rounded to 0.01°), so the
  lookup rule places them unchanged. Each output gate is at the ground
  distance `s` of its slant range at `elevationDeg`. A scan *covers* it when
  its nearest gate, `round((r − firstGateM) / gateSpacingM)` at that scan's
  `r`, is one of its gates; the scan's value there is that gate on its ray
  nearest the output ray's azimuth (within 0.75°; none: code 1).
- `CAPPI` at `heightM`: the target is `H = heightM − altM` above the
  antenna (`altM` from `hello.sites`). The point at the gate's ground
  distance `s` and height `H` is seen from the antenna at the elevation
  `eH = atan2((R + H) cos(s/R) − R, (R + H) sin(s/R))`. A scan's beam
  *holds* the height when `|eH − e| ≤ 0.5°` (half of a nominal 1° beam: the
  beam is about 1.7 km thick at 100 km). Of the scans covering the gate
  whose beam holds the height, the one whose `h` is nearest `H`, the lower
  angle on a tie; its value. None: code 1, no data, drawn as "no radar at
  this height", never code 0 ("no rain"). So a low height ends where even
  the lowest beam's lower edge passes above it (1 km at Vara: ~120 km), a
  height above the highest angle's upper edge is empty over the radar, and
  between two far-apart high angles it has gaps. The texel is code 1 with G
  bit 4, like any `nodata`; a client that knows the frame is `CAPPI` may
  draw such texels as a faint hatch (S29) rather than nothing.
- `CMAX`: of the scans covering the gate, the highest measured code (2 and
  up); none measured: 0 if any is below threshold, else 1.
- `HYBRID`: each radar's blockage table (`engine/data/blockage.json`,
  `radars.<id>`: 360 numbers, one per degree of azimuth, the lowest clear
  angle in tenths of a degree, computed offline) picks, per output ray (the
  entry at the floor of its azimuth), the lowest scan whose angle, rounded
  to tenths, is at or above it (above every scan: the highest), and the
  gate takes that scan's value where it covers it (code 1 where it does
  not).

The engine reads only the angles a product needs: one for `REF`, the scans
a pseudo-CAPPI chooses somewhere, every scan for `CMAX`, DBZH only, with
range requests where the provider allows. What a frame costs is in the
table below, measured per provider.

## Configuration

`~/.config/omastorm-se/config.toml` and
`$XDG_STATE_HOME/omastorm-se/state.json` are read by the UI, never by the engine.
Explicit preferences override remembered view state. The UI resolves the map
center and radar lock independently, then sends `select_site`, `lock`,
`follow`, and settled `view_center` commands as needed. Unlocked navigation
uses follow; locked navigation preserves the selected radar. Treatment and
the weak-return floor stay in the UI. File ownership, launch precedence,
onboarding, and validation are in [configuration.md](configuration.md).

## Implementation notes

The shell session keeps a status connection; each visible popover and expanded
window has its own connection so tile requests remain independent. Expand
uses the shared station, frame, and play state directly, sending no
select/seek/play commands, and preserves the map center and zoom. Closing
preserves the view rather than selecting another station. The session owns
remembered-view writes so surfaces do not overwrite one another's state.
On launch, the UI applies explicit config over remembered state. Reconnecting
to the engine restores the necessary selection and flags without resetting
the active camera. A change of frame or station never re-centers the map
except when the user picks a station in search, which the UI centres on.
Location picks write state.json. With no location, the popover offers the
prompt if no source supplies a center; approximate IP lookup is a UI `curl` to
wttr.in after an explicit click, not an engine command.

`hello` additionally includes `pid`, `build` (an opaque fingerprint),
`sitesSource`, `sitesRetrieved`, and `sitesNotes`. These allow the launcher to
identify an existing build and preserve the station snapshot's provenance.
The 163-site snapshot includes archived/test sites, not an availability list.
`frame.site` retains the scan's measured coordinates.

On disconnect the UI hides radar and retries; on an unknown version it
hides radar and latches the error until relaunch.

## Changes from version 1

- Every engine message carries `"v": 2`.
- `hello.sites[].kind` (`polar` or `grid`) is new and always sent. The table
  gains the composite, `sweden`.
- `state.frame.kind` (`polar` or `grid`) is new and always sent. Polar
  frames are otherwise unchanged. A `grid` frame carries `frame.grid`, an
  empty `azimuthLut`, and zero polar geometry.
- `view_center` never hands off to or from a `grid` station.

Additive since (S14, still version 2):

- `hello.sites[]` gains `country`, `provider`, `rangeKm`, `attribution`,
  and `aliases`, always sent.
- `state.frame.attribution` is new and always sent.
- `select_site` accepts a station's aliases, and ids in any ASCII case.
- Staleness thresholds follow each station's provider cadence; SMHI's stay
  15 and 30 minutes.

Additive since (S19, still version 2):

- `state.timeline[]` entries gain `texture` and `azimuthLut` (always sent,
  `""` without stable textures) and `placement` (only where it differs
  from `frame`'s). [Timeline textures](#timeline-textures).
- A grid entry catalogued since S19 also names `codes`, its one-channel
  code texture; `frame.texture` and the grid texture format are unchanged.
- A catalogued frame keeps one texture name per file while it is in the
  catalog, instead of a new revision on every show; its files stay
  published while it is in the selected station's timeline.

Additive since (S23, still version 2):

- `hello.referenceSites[]` is new and always sent: the other European
  radars' positions, for faint map marks only ([`hello`](#engine-messages)).
- A client shows no angle for a `grid` frame; its `elevationDeg` is not
  meaningful to display.

Additive since (S20, still version 2):

- `hello.products`, and `hello.sites[].products` and `elevations`, always
  sent. `state.product`, always sent.
- `set_product` is implemented; `elevationIndex` is optional.
- A radar's `frame` may be a product other than the lowest scan
  (`frame.product` other than `REF`, or `REF` at another angle); a client
  that ignores `product` still draws it correctly, and shows its
  `productName`.

Additive since (S29, still version 2):

- `hello.products` lists `CAPPI` "Height" in place of `CAPPI1` and
  `CAPPI2`, and so does `hello.sites[].products`. `set_product` still
  accepts `CAPPI1` and `CAPPI2`, as `CAPPI` at 1,000 and 2,000 m.
- `set_product` takes an optional `heightM` (500–12,000 m above sea level,
  steps of 500) with `CAPPI`; `state.product.heightM` is sent with `CAPPI`.
- A height is above sea level (the antenna's `altM` plus the beam's height),
  where `CAPPI1`/`CAPPI2` were above the antenna; where no scan's beam holds
  the height the gate is no data (code 1), where before the nearest scan
  filled it whatever its distance from the height. `frame.product` is
  `CAPPI` and `productName` names the height (`Height 3.5 km`).
- A client facing an older engine sees `CAPPI1`/`CAPPI2` in `hello` and no
  `heightM`, and offers those two heights as before.
