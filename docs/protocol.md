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
range ring, or coverage circle for it. Since S24b the provider composites
(`sweden`, `nordic`) also offer products the engine makes from their radars'
volumes ([The composites' products](#the-composites-products)). `iberia`
(S33, OPERA's Iberian crop) offers none: its `products` is empty, so a
client shows no product chooser, and the composite is all it shows.

Every station also carries the following fields, always sent (S14). They
are additive, so the version stays 2 and an older client ignores them:

- `id`: the station id every command and `state.site.id` use (DEC-12). A
  Swedish radar's id is its SMHI area key (`vara`, `balsta`); every other
  radar's is its ODIM node code in lowercase (`nohur`, `fikor`, `dksin`); a
  composite's is a region word (`sweden`, `nordic`, `iberia`). Ids are unique across
  the table, aliases included. Clients treat them as opaque.
- `aliases`: other ids the engine accepts for this station in `select_site`,
  so a stored or typed id keeps working. A Swedish radar lists its ODIM node
  code (`vara` has `["sevax"]`). Empty when there are none.
- `country`: the ISO 3166-1 alpha-2 code of the network the station belongs
  to (`SE`, `NO`, `FI`, `DK`); empty for a composite that spans countries.
- `provider`: where the engine gets the station's data: `smhi` (SMHI's open
  API), and from S15/S16 `ord` (EUMETNET Open Radar Data), `fmi-s3` (FMI's
  bucket), `opera` (the OPERA composite), and from S25 `mosaic` (My
  mosaic, made by the engine). It is for display and grouping; a
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

`hello` also carries `mosaic` (S25, additive, always sent): what the
engine's own mosaic of chosen radars accepts ([My mosaic](#my-mosaic)).
An engine older than S25 sends none, and a client then offers no mosaic.

```json
"mosaic":{"station":"mymosaic","maxSites":12,"minReachKm":25,
          "rules":[{"id":"lowest","name":"Lowest beam"},{"id":"strongest","name":"Strongest"},
                   {"id":"height","name":"Height"}]}
```

The `height` rule is S30's (a slice at a chosen height across the chosen
radars); an engine older than S30 lists only the first two.

`hello` also carries `sections` (S24c, additive, always sent): the shape of
the vertical cuts and column profiles the engine makes on request
([Sections and profiles](#sections-and-profiles)). An engine older than
S24c sends none, and a client then offers neither.

```json
"sections":{"levels":24,"levelM":500,"columnM":2000,"maxColumns":300}
```

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
  documents its own (S32: an ORD station's cadence is its country's, 10
  minutes for Spain's radars, so `stale` from 20 minutes and `unavailable`
  from 30). For SMHI (5-minute cadence) that is `ok` under 15
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
- `mosaic` (S25) is the engine's set for My mosaic, `{"sites":[{"id":"vara",
  "reachKm":240.0}],"rule":"lowest"}`, shared by every client like the
  station; `sites` is `[]` until a client sends `set_mosaic`
  ([My mosaic](#my-mosaic)). Always sent. With the `height` rule (S30) it
  also carries `heightM` and `above`.
- `section` (S24c) is the vertical cut a client asked for with
  `set_section`, shared by every client like the station, or `null` when
  there is none ([Sections and profiles](#sections-and-profiles)). Always
  sent; an engine older than S24c sends no `section`.
- `loading` (S31) is how far the load a client is waiting for has come,
  as a percentage with what it counts, or `null` when nothing is loading
  ([Loading progress](#loading-progress)). Always sent; an engine older
  than S31 sends no `loading`, and a client then shows `connection.status`
  `loading` as before.
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
{"type":"set_product","product":"CAPPI","heightM":1000,"above":"ground"}
{"type":"set_mosaic","sites":[{"id":"vara"},{"id":"nohur","reachKm":150},{"id":"dksin"}],"rule":"lowest"}
{"type":"set_mosaic","sites":["vara","nohur","dksin"],"rule":"height","heightM":3000,"above":"sea"}
{"type":"tiles_needed","z":11,"x0":469,"y0":807,"x1":472,"y1":810}
{"type":"set_section","from":{"lat":58.26,"lon":12.83},"to":{"lat":59.93,"lon":10.72}}
{"type":"set_section"}
{"type":"profile","lat":58.70,"lon":13.40}
```

- `set_mosaic` (S25) chooses My mosaic's radars, each one's reach and the
  combine rule. The rules are in [My mosaic](#my-mosaic).
- `set_section` (S24c) sets, moves or (with neither point) clears the
  vertical cut in `state.section`; `profile` (S24c) asks for one column's
  values, answered with `profile` to the sender only, like `places`. Both
  are in [Sections and profiles](#sections-and-profiles).

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
  `select_site` leaves it. S33's exception: `iberia` (its radars in
  Portugal have no volumes) is a target with the centre inside its box
  where the composite alone shows the radars there: nearer one of its
  box's `referenceSites` (Portugal's) than to any station radar, or beyond
  1.3 × the nearest radar's `rangeKm` (Porto, Lisbon, Galicia's coast; a
  place in Spain still follows a Spanish radar). Selected, `iberia` holds
  while the centre stays in its box and hands off by the nearest-radar
  rule once it leaves.
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
  S29) the height in metres, used with `CAPPI` only, above sea level or,
  with `above` `ground` (optional, S30), above the terrain. An
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
| G | status bits: 1 range folded, 2 below threshold, 4 outside coverage; 8 (S24a, `ETOP` only, with R above 0; S24b also in an `ETOP` grid texture) the storm height is "at least" this ([Storm height and rain mass](#storm-height-and-rain-mass)) |
| B | moment byte in the Level II convention (ODIM DBZH requantized, `code = dBZ × 2 + 66`), for cursor inspection and the weak-return floor; for `ETOP` and `VIL` their own code (value = (code − `offset`) / `scale` in `units`) |
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
it: code 0 or 1 draws nothing (in a `CAPPI` frame, S30, code 1 may be hatched
as "no radar at this height", as the grid texture's G bit 4 allows); otherwise, with value = (code − `offset`) /
`scale` in `units`, the class is the number of `bounds` at or below value,
minus one, kept within 0 to `palette.length` − 1, and the texel is class + 1
with no status bits, exactly what the engine writes to the grid texture
(composites have no folded gates). In an `ETOP` grid frame (S24b) an odd
measured code is an "at least" top, which the grid texture marks with G
bit 8 like the sweep texture; a client drawing the code texture may hatch
those texels the same way. Its stable name is
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
The floor is in dBZ, so since S24a a client sends `weakBelow` 0 for a frame
whose `units` are not `dBZ` (`ETOP`, `VIL`), and the legend hides nothing.

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
60-frame backfill, and DEC-2's range reads. `ord` (S15, DEC-13; Spain since
S32, DEC-16): EUMETNET Open Radar Data's 24-hour S3 cache, credited to each
national owner (`MET Norway`, `FMI`, `DMI`, `© AEMET`, CC BY 4.0), a
5-minute cadence (Spain's AEMET radars 10 minutes; of their two volumes
every 10 minutes the long-range one is read, not the Doppler one), and a
backfill of up to 60 files over the last five hours. `opera` (S16, DEC-14): the grid
station `nordic`, EUMETNET OPERA's European composite from the Open Radar
Data 24-hour S3 cache, cut to 3–33° E, 53–71.5° N (a 1670 × 2297 texture,
larger than Sweden's 1364 × 1983 and above WebGL2's guaranteed 2048), read
11 of its 30 chunks at a time (~0.9 MB a frame), a 5-minute cadence and a
24-frame backfill, credited `EUMETNET OPERA, CC BY 4.0`, `country` empty.
Since S33 (DEC-17) `opera` also lists `iberia`, the same composite cut to
10.5° W–4.5° E, 35–44.5° N (mainland Portugal and Spain with the Balearics;
an 835 × 690 texture, 5 of the 30 chunks, ~47 KB a frame), with the same
cadence, reads and backfill, credited `EUMETNET OPERA (AEMET, IPMA,
Météo-France), CC BY 4.0` (the box takes in southern France), `country`
empty, at the middle of its box (39.75° N, 3° W). Portugal's
radars have no volumes in Open Radar Data, so they show only through it.
`iberia` offers no products (`products` empty). One read of a file serves
every OPERA box the engine is polling (the selected one and any keep-warm
one): the engine's OPERA pollers share the read and the listing, so two
boxes shown cost the listings and file opens of one, plus the other box's
chunks in the same read. The
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
`iberia` warm costs about as many requests as `nordic` but ~47 KB a frame
(~0.6 MB an hour); warm beside a selected `nordic` (or the other way round)
it adds no listing or file open, only its chunks in the same reads (S33).

## Products

Additive since S20 (still version 2). A radar volume holds several scan
angles (SMHI 10, from 0.5° to 40°; MET Norway two alternating sets of 12
and 10; FMI's cache 5; DMI 10). A radar's frame shows one **product**
made from them; the lowest angle (`REF`, elevation index 0) is what every
frame was before S20, and stays the default. On a composite `REF` is the
provider's composite itself; since S24b `sweden` and `nordic` also offer
products made from their radars ([The composites'
products](#the-composites-products)). My mosaic has none: its set's rule
says what it shows.

`hello.products` is the vocabulary, in the order a chooser lists them:

```json
"products":[{"id":"REF","name":"Lowest scan"},{"id":"HYBRID","name":"Clear view"},
            {"id":"LOWB","name":"Lowest beam"},
            {"id":"CAPPI","name":"Height","above":["sea","ground"]},{"id":"CMAX","name":"Column max"},
            {"id":"ETOP","name":"Storm height","units":"km"},{"id":"VIL","name":"Rain mass","units":"kg/m²"}]
```

`LOWB` "Lowest beam" (S24b) is offered only by the composites `sweden` and
`nordic` ([The composites' products](#the-composites-products)); no radar
lists it. An engine older than S24b does not send it.

`units` (S24a) is sent only on products whose frames are not in dBZ:
`ETOP` in km, `VIL` in kg/m² ([Storm height and rain mass](#storm-height-and-rain-mass)).
An engine older than S24a lists neither.

Before S29 the list held `CAPPI1` "Height 1 km" and `CAPPI2` "Height 2 km"
instead of `CAPPI`; they are now aliases `set_product` still accepts (below).
`above` (S30, only on `CAPPI`) lists what a height can be measured from:
`sea` level, and `ground` (the terrain, [Terrain](#terrain)). An engine
older than S30 sends no `above`: its heights are above sea level only.

Each `hello.sites[]` entry names what the station can show and its angles:

```json
"products":["REF","CAPPI","CMAX"],
"elevations":[{"deg":0.5,"beamKm50":0.6,"beamKm100":1.5},{"deg":1.0,"beamKm50":1.0,"beamKm100":2.3}]
```

- `products`: a subset of `hello.products`' ids, in that order. Since S24b
  `sweden` and `nordic` list `["REF","LOWB","CAPPI","CMAX","ETOP","VIL"]`
  (before S24b, `[]`), and `mymosaic` `[]`. Every radar can make `REF`. `CAPPI`, `CMAX`, `ETOP` and
  `VIL` are offered wherever the engine knows the radar's angles: SMHI's,
  MET Norway's and DMI's radars (one file holds every angle) and, since
  S24a, FMI's, whose files hold one angle each in ORD's cache: the engine
  reads the five files of one nominal time as one volume ([FMI's
  volumes](#fmis-volumes)). Before S24a FMI's radars offered `REF` alone,
  with `elevations` `[]`. Since S32 AEMET's eleven (Spain) offer them too,
  from their three angles (0.5, 1.3, 2.1°; Valladolid and San Sebastián
  0.5, 1.4, 2.3°), so their storm height and rain mass see little above
  a few kilometres far out. `HYBRID` is offered only where the engine also
  has a blockage table (none for FMI's radars yet).
- `above` (S32): what `CAPPI`'s height can be measured from at this
  station, a subset of `hello.products`' `above`: `["sea","ground"]` where
  the terrain grid holds the radar's whole reach (every Nordic radar, and
  the Nordic composites), `["sea"]` elsewhere (Spain's radars: the grid is
  Nordic, [Terrain](#terrain)). Sent only with `CAPPI` in `products`.
  Clients offer "above the ground" only where it is listed, and for My
  mosaic only when every chosen radar lists it. A `ground` choice on a
  station without it is made above sea level (a switch carries it over as
  `sea`). An engine older than S32 sends no `above` here: `hello.products`
  decides alone.
- `elevations`: the radar's scan angles, ascending; the position is the
  `elevationIndex`. `beamKm50` and `beamKm100` are the beam centre's height
  above the antenna at 50 and 100 km ground distance, in km with one
  decimal, for labels (below). The angles are nominal: a volume's own can
  differ a little (MET Norway's two sets give 2.4° or 2.6°, DMI's lowest
  wanders 0.46–0.51°); the engine takes the volume's angle nearest the
  chosen one, and `frame.elevationDeg` says which it was. Empty when the
  engine does not know them.

`state.product` is the chosen product, `{"id":"CMAX","elevationIndex":0}`,
always sent; for `CAPPI` it also carries the height and what it is
measured from, `{"id":"CAPPI","elevationIndex":0,"heightM":3000,"above":"sea"}`
(S29; `heightM` and, since S30, `above` are sent with `CAPPI` only). It is
shared engine state, like the station: one client's `set_product` changes
it for every client of that engine.

`frame.product`, `productName`, and `elevationDeg` describe the frame on
screen:

| `product` | `productName` | What | `elevationDeg` |
|---|---|---|---|
| `REF` | `Reflectivity` | one scan angle: the lowest (index 0), or the one chosen | that angle |
| `HYBRID` | `Clear view` | per azimuth, the lowest angle the terrain does not block | the lowest angle (placement only) |
| `CAPPI` | `Height 3 km`, `Height 3.5 km`, `Height 1 km above ground`, … | per gate, of the angles whose beam holds `heightM` above sea level (or above the ground there), the one whose beam centre is nearest it; none: no data ("no radar at this height") | the lowest angle (placement only) |
| `CMAX` | `Column max` | per gate, the strongest return of any angle above it | the lowest angle (placement only) |
| `ETOP` | `Storm height` | per gate, the height above sea level of the highest beam with 18 dBZ or more, in km; "at least" where the highest beam that reaches it still holds that much | the lowest angle (placement only) |
| `VIL` | `Rain mass` | per gate, the water in the column, kg/m², from every angle | the lowest angle (placement only) |
| `LOWB` | `Lowest beam` | composites only (S24b): per texel, the radar whose lowest clear beam is lowest there | 0 |

A client shows an angle beside the product only for `REF`. The texture
format is the same for every product. `REF`, `HYBRID`, `CAPPI` and `CMAX`
share `units` (dBZ), palette, bounds, `scale` and `offset`; `ETOP` and
`VIL` bring their own in the frame ([Storm height and rain
mass](#storm-height-and-rain-mass)), which the legend already reads.

**`set_product`.** `{"type":"set_product","product":"CMAX"}`,
`{"type":"set_product","product":"REF","elevationIndex":3}` for one angle,
or `{"type":"set_product","product":"CAPPI","heightM":3000}` for a height.
`heightM` is metres, 500 to 12,000 in steps of 500; absent, it is 2,000.
`above` (S30) is `sea` (the default when absent) or `ground`: with
`ground`, each point's height is `heightM` above the terrain under it
([Terrain](#terrain)), so over a 1,200 m plateau "1 km" is 2,200 m above
sea level. The aliases `CAPPI1` and `CAPPI2` are `CAPPI` at 1,000 and
2,000 m above sea level (any `heightM` or `above` sent with them is
ignored), so a client that still offers them keeps working; `state.product`
then says `CAPPI`.
It is answered with an `error`, and nothing changes, when the id is neither
in `hello.products` nor an alias, no station or `mymosaic` is selected,
the selected station cannot make it (not in its `hello.sites[].products`), `elevationIndex` is not an index of its
`elevations` (any index but 0 with another product), `heightM` is outside
500–12,000, not a multiple of 500, or sent with a product other than `CAPPI`,
or `above` is neither `sea` nor `ground` or is sent with another product.
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
reaches less far; either way `state.product` says what is in effect.
Selecting a grid station (S24b) always shows its composite:
`state.product` becomes `REF` index 0, and the choice before it is kept
for the next radar, as before S24b. A `set_product` while `sweden` or
`nordic` is selected picks one of its products, and that choice is then the
one the next radar gets (`LOWB`, which no radar makes, falls back to `REF`
index 0). Selecting `mymosaic` keeps the choice for the next radar too.

**History.** The catalog keeps each radar's frames per product. After a
switch the lowest scan backfills its 60 frames as before; any other
product backfills the newest 12 on an SMHI radar (an hour; each of its
volumes costs about 39 range requests), the newest 24 on a MET Norway or
DMI radar (two hours, one request a file) and the newest 12 on an FMI
radar (an hour: five files a volume, S24a), and its ring fills to 60 as
live frames arrive. CAPPI, CMAX, HYBRID, ETOP and VIL read the lowest scan
anyway, so each of their frames also adds the lowest scan's frame of the
same volume to the station's `-e0` ring, at no extra request. The engine keeps a station's
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
  With `above` `ground` (S30) the target varies from gate to gate: at the
  output ray's azimuth and the gate's ground distance `s`, the point on the
  6,371 km sphere (the lookup rule's) has terrain `T` ([Terrain](#terrain)),
  and `H = heightM + T − altM`; the rest is the same rule, gate by gate.
  The engine reads the angles that could hold any height from
  `heightM − altM` to `heightM + max(T) − altM`, so it reads at most a
  scan or two more than above sea level. The rule, "of the beams covering a
  point that hold the height, the one whose centre is nearest it, the
  lower angle on a tie", is the one My mosaic's `height` rule uses per
  radar.
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
a pseudo-CAPPI chooses somewhere, every scan for `CMAX`, `ETOP` and `VIL`,
DBZH only, with range requests where the provider allows. What a frame
costs is measured per provider in the streams' logs (`coord/log/`).

### Storm height and rain mass

Additive since S24a (still version 2). Both are drawn on the lowest scan's
rays and gates like `CMAX`. An output gate's *column* is the scans covering
it (above), ascending by angle, each with its value there and its beam
centre's height `h` above the antenna.

- `ETOP`, "Storm height": of the column's scans whose value is 18 dBZ or
  more (code 102 and up), the highest angle's; the top is
  `T = (h + altM) / 1000` km above sea level (`altM` from `hello.sites`).
  It is **at least** `T` when no scan above that one in the column has a
  value other than no data (code 1): the highest beam that reaches the
  gate still holds 18 dBZ, so the storm may reach higher. Where the top
  angles do not reach (SMHI's 24° and 40° end at 120 km, FMI's 5° at
  ~184 km) the column's highest scan is a lower one, so far from a radar a
  tall storm is often "at least"; FMI's highest angle, 5°, is the lowest of
  the four networks' (SMHI 40°, MET Norway 15.5°, DMI 15°).
  The code is `2 + 2 × round(5 T)` (the top to 0.2 km, half away from
  zero), at most 254, plus 1 when it is "at least". So with `scale` 10 and
  `offset` 2, value = (code − `offset`) / `scale` is the top within 0.1 km
  either way, an odd code is "at least", and the class rule gives an "at
  least" top its own top's class (the bounds are whole kilometres). Code 0:
  the column has a reading (any code but 1) and none reaches 18 dBZ; code
  1: none, or no scan covers the gate. `units` `km`; `bounds`
  `[0,2,3,4,5,6,7,8,9,10,12,15,26]` with twelve colours.
  In the sweep texture an "at least" texel also has G bit 8
  ([Texture files](#texture-files)); a client may draw it hatched over its
  colour. An older client draws its colour alone.
  **Relief (S24d, clients only, no protocol change):** a client may light a
  storm height as a surface, as both clients do when the user turns Relief
  on (web: options menu, which also tilts the map 50°; desktop: the product
  menu, no tilt). Each drawn texel keeps its palette colour, darkened or
  lifted by the slope its neighbours give under a light from the
  north-west, 45° up, heights exaggerated 2×. The slope is a 3 × 3 Sobel
  over neighbours at least 3 km apart on the ground, or one 3 px screen
  cell if that is more (an echo top jumps between beam heights from gate
  to gate, so a slope read over less is mostly that): for a sweep, `s`
  gates in and out along the ray on the rays `da` degrees either side (by
  the azimuth lookup); for a grid, the texels `s` apart. Heights: an even
  code's value; an "at least" (odd) code stands at its lower bound and
  keeps its hatch; code 0 is 0 km (a storm's edge is a real slope). A
  neighbour with no height to lean on (code 1, the sweep's blank row, off
  the sweep or the grid, past the client's reach) counts as level with the
  centre, so coverage edges, the reach and unscanned azimuths are never lit
  as cliffs; a texel's own colour, and so its edges, are unchanged. The composites and My
  mosaic get no special case: a grid frame is lit the same way (My mosaic
  offers no `ETOP`). Colouring by `CMAX` is not offered: a timeline
  carries one product, so the client never holds both frames.
- `VIL`, "Rain mass", vertically integrated liquid: the column's scans
  whose value is not no data, ascending. A measured code gives
  `dBZ = min((code − 66) / 2, 56)` (capped: hail would count as water) and
  `Z = 10^(dBZ / 10)` (mm⁶/m³); below threshold (code 0) gives `Z = 0`.
  Over each gap between two consecutive beam centres, Marshall–Palmer
  water `3.44e-6 × ((Z_i + Z_i+1) / 2)^(4/7)` kg/m³ times the gap
  `h_i+1 − h_i` in metres, summed upward (f64, in that order):
  kg/m². The code is `2 + round(2 × VIL)`, at most 255 (`scale` 2,
  `offset` 2: 0.5 kg/m² a code, to 126.5). Code 0: every such scan is
  below threshold, or the rain mass is under 0.25 kg/m² (drawn as
  nothing, like below threshold, so the code is 3 or more wherever there
  is any); code 1: there is none. A column with one reading has no gap and
  a VIL of 0 (code 0). `units` `kg/m²`; `bounds`
  `[0,1,2,4,7,10,15,20,25,30,40,50,70,127]` with thirteen colours.
- Both need every scan of the volume (like `CMAX`), and their frames carry
  the lowest scan along for the `-e0` ring like the other products. Frame
  ids end in `-etop` and `-vil`.
- **The weak-return floor** is in dBZ: a client applies it only to frames
  whose `units` are `dBZ`. A client older than S24a applies its floor to
  these frames in their own units (a 5 dBZ floor hides storm heights under
  5 km and rain mass under 5 kg/m²) and shows the legend's hidden part
  that way; tapping the legend shows them.

### FMI's volumes

Additive since S24a (still version 2). ORD's cache holds each FMI radar's
scans as `SCAN` files of one angle each: every nominal time, five files at
0.3° (0.5° at Korppoo, 0.1° at Luosto), 0.7°, 1.5°, 3.0° and 5.0° (every
FMI radar, ORD's listings of 2026-09-15). For any product but the lowest
scan the engine reads all the files of one nominal time as one volume
(per angle the best quantity, `DBZH` else `TH`), its scans ascending by
angle; the newest time waits until it has as many angles as the time
before. Each file is read whole in one request (75–210 KB, 0.6–0.8 MB a
volume), and the five scans are kept in the tilt store as one volume, so
every other product of that time then costs no request. The lowest scan
alone still reads its one file, as before S24a. `elevations` lists the
five angles; `REF` at another index reads the whole volume too. The 5°
scan ends at ~184 km (367 gates of 500 m), the others at 250 km.

## My mosaic

Additive since S25 (still version 2). The providers' composites (`sweden`,
`nordic`) have every radar of their network baked in. My mosaic is a
composite the engine makes itself, from the **lowest scan** of up to 12
radars a client chooses, of any provider, so a radar can be left out or
trimmed (plan §S25: over the Skagerrak Hurum's beam is 3–5 km up where
Vara's and Sindal's are under 2 km).

**The station.** `hello.sites` lists `mymosaic`, "My mosaic", after the
composites: `kind` `grid`, `provider` `mosaic`, `country` empty, `rangeKm`
0, no products and no angles. It is selected with `select_site` like any
composite and never a hand-off target. `hello.mosaic` (above) names it and
the limits: `maxSites` radars, a reach of at least `minReachKm`, and the
combine `rules` in the order a chooser lists them, with display names.

**`set_mosaic`.**

```json
{"type":"set_mosaic","sites":[{"id":"vara"},{"id":"nohur","reachKm":150},"dksin"],"rule":"lowest"}
```

- `sites`: the radars, 1 to `maxSites` of them, each an object with an
  `id` and an optional `reachKm`, or just the id as a string. An id is a
  station id or alias from `hello.sites` (as in `select_site`); it must be
  a radar (`kind` `polar`). Reference sites are not stations, so their ids
  are unknown.
- `reachKm`: how far from the antenna this radar counts, in km of ground
  distance (the great circle on the 6,371 km sphere, as the lookup rule
  measures it). At least `minReachKm`; absent, or at or past the radar's
  `rangeKm`, it is the radar's full range. The same unit, range and meaning
  as S29's per-radar reach slider, with one difference: S29's reach is a
  client's view clip of one radar on screen, while this reach is applied by
  the engine when it combines, so a trimmed radar's area falls to its
  neighbours.
- `rule`: `lowest` (the default when absent), `strongest`, or (S30)
  `height` (below).
- `heightM` and `above` (S30, with `height` only): the slice's height in
  metres, 500 to 12,000 in steps of 500 (absent: 2,000), and what it is
  measured from, `sea` (absent: sea) or `ground` ([Terrain](#terrain)),
  exactly as `set_product` takes them for one radar.
- It is answered with an `error`, and nothing changes, for an empty or
  too long list, an unknown id, a composite, the same radar twice, a
  `reachKm` below `minReachKm` or not a number, an unknown rule, a
  `heightM` or `above` that `set_product` would refuse, or either of them
  sent with a rule other than `height`.
- Otherwise it becomes `state.mosaic`: `sites` in the order sent, each
  with its canonical `id` and the reach in force (`rangeKm` for full,
  otherwise to 0.1 km), and `rule`; with `height`, also `heightM` and
  `above`, always sent then
  (`{"sites":[…],"rule":"height","heightM":3000,"above":"sea"}`). The same
  radars again, in any order, with the same reaches, rule, height and
  `above`, change nothing; another height is another set. The set is engine state,
  shared by that engine's clients like the station, and a live engine keeps
  it across restarts (`$XDG_CACHE_HOME/omastorm-se/mosaic.json`, checked
  against the station table again at start). A client also remembers its
  last set and sends it again when it connects to an engine whose
  `state.mosaic.sites` is empty.
- While `mymosaic` is selected, a new set shows its own history at once
  (or the loading placeholder) with `connection.status` `loading`, and the
  engine polls the new radars, like a `select_site`.

**Frames.** Grid frames exactly like a composite's (`frame.kind` `grid`,
the grid texture and code texture formats unchanged), so a client draws
them as it draws `sweden`:

- The texture covers the box of the chosen radars' reach circles, in Web
  Mercator texels of 2,000 m (the composites' pixel), counted from its west
  and north edges. It is typically 600–1,000 px a side for three radars and
  at most about 1,600 × 2,600 for twelve spread over the Nordics.
- `id` is `mymosaic-<T compact>-m<8 hex>`: the set (radars, reaches and
  rule, and with `height` the height and `above`) names the last part, so
  each set has a timeline of its own. The engine keeps the current set's
  frames and the previous set's.
- `scanTime` is the nominal time T the frame is for (a multiple of 5
  minutes), `sweepEnd` the end of the latest scan used.
- `product` is `REF`, and `productName` the rule's name (`Lowest beam`,
  `Strongest`); with `height` (S30), `product` is `CAPPI` and
  `productName` names the height as a radar's does (`Height 3 km`,
  `Height 1 km above ground`), so a client that hatches a `CAPPI` frame's
  no-data texels ("no radar at this height", S29) does so here too.
  `elevationDeg` is 0 and not meaningful to show.
- `attribution` names the owner of every radar the frame drew on, joined by
  `; ` (`SMHI, CC BY 4.0; MET Norway, CC BY 4.0; DMI, CC BY 4.0`). The
  station's own `attribution` in `hello` only says that.
- `frame.site` is the middle of the texture's box; `grid.sourceProjdef`
  names the rule and the radars, for display and debugging.

**How a frame is made.** For each chosen radar, its lowest scan at T: the
volume whose nominal time is T (the 5-minute mark at or before its start
plus 1 minute, the pollers' own rule). Each texel centre is placed from each radar as the lookup rule
places a screen cell: the great-circle ground distance `s` and bearing on
the 6,371 km sphere, the slant range `r = R sin(s/R) / cos(e + s/R)` on the
4/3 earth at the scan's own elevation `e`, the gate `round((r −
firstGateM) / gateSpacingM)` and the ray nearest the bearing (within
0.75°). A radar is a *candidate* at a texel when `s` is within its reach
and the gate is one of its gates. Then, with no averaging:

- `lowest` (Lowest beam): of the candidates whose gate is not no data
  (code 1), the one whose beam centre is lowest above sea level there,
  `altM + R cos(e) / cos(e + s/R) − R` to the metre, the nearer radar on a
  tie; its code, below threshold (0) included. So where Vara and Sindal
  see nothing under a layer Hurum's higher beam cuts through, the texel is
  0.
- `strongest` (Strongest): the highest measured code (2 and up) of any
  candidate; none measured: 0 if any candidate is below threshold.
- `height` (S30): each radar's whole volume, not only its lowest scan.
  For each candidate radar the texel's target is `H = heightM − altM`
  above its antenna (with `above` `ground`, `heightM + T − altM`, `T` the
  terrain of the texel, [Terrain](#terrain)), and the radar's scan is
  chosen by the height rule of `CAPPI` ([Products](#products)): of its
  scans whose gate at the texel's ground distance is one of their gates
  and whose beam holds `H` (`|eH − e| ≤ 0.5°`), the one whose beam centre
  is nearest `H`, the lower angle on a tie; its code at that gate and
  bearing. A radar with no such scan, or whose chosen scan gives code 1
  there, is not a candidate. Of the candidates, the one whose beam centre
  is nearest `H` (to the metre), the nearer radar on a tie; its code, 0
  included. So each radar's part is exactly its own `CAPPI` at that height,
  and where two overlap the one that measures closer to the height wins.
  None: code 1, "no radar at this height". Low slices have gaps between
  radars (1 km is real only within about 75 km of an SMHI radar; they
  stand 150–250 km apart); 2–3 km slices are nearly whole.
- No candidate, or only no-data gates: code 1 (no data, G bit 4), as
  outside a composite's grid. No bias correction is applied.
- A radar whose scan at T reaches less far than its full range (DMI
  alternates a 119.5 km scan with a 237.5 km one at Sindal) takes the gates
  past that scan's edge from its latest longer scan up to 10 minutes older,
  so the far ring does not blink every other frame; the provenance says so.

**Timing and cost.** The frame for T is built as soon as every chosen
radar's scan for T is in (a short DMI scan together with the longer scan
its far ring comes from, once an older scan of that radar shows it
alternates), or else from the scans that have arrived once it is T + 8
minutes and no scan has come in for 30 seconds; a time with no scan at all
is never built. A radar missing from a frame is named in its provenance,
and its area is nodata there unless another radar covers it. A missing
scan that arrives later builds the frame once more, under the same `id`
(the catalog and the clients replace it), when T is one of the two newest
frame times and less than 12 minutes old; older frames are never rebuilt.
Selecting `mymosaic` shows the set's
catalogued frames at once, then builds the newest 12 frame times the
catalog lacks (an hour), newest first: each radar's lowest scans come from
the tilt store where it has them (no request) and are otherwise fetched
as that radar's own lowest scan is, under its provider's budget. The
radars' pollers start 2 seconds apart, and each provider reads one volume
at a time, engine-wide. After that one frame every 5 minutes. The radars are polled only while `mymosaic` is
selected, and never kept warm (`OMASTORM_WARM` does not accept
`mymosaic`). A Swedish radar's lowest scan is about 7 range requests and
100 KB, a Norwegian one about 1 request and 0.4 MB (the file read whole), a
Danish one 20–25 requests and about 240 KB, so Vara + Hurum + Sindal cost
about 10 MB an hour while shown.

The `height` rule (S30) reads each radar's every scan, through the tilt
store: a Swedish volume is about 39 range requests and 740 KB, a Norwegian
or Danish one 1 request and 0.3–1 MB (read whole), so Vara + Hurum +
Sindal cost about 25 MB an hour while shown. Every height needs nearly
every scan, so the whole volume is read once and every later height, and
`above` either way, is made from the tilt store with no request. Because a
volume costs 5–6 times a lowest scan, a `height` set builds back the newest
**6** frame times (half an hour), not 12, when it has an SMHI radar (a set
of ORD radars only, whose files are 1 request each, builds back 12); its
timeline still grows as live
frames arrive. The tilts are read back from the store for each frame, so
the store must be on (`OMASTORM_TILTS_MB` above 0); with it off, a
`height` set reports the station offline with that reason. A radar whose
provider publishes one file per angle (FMI) takes part with its lowest scan
alone, which holds a height only where that one beam does.

## The composites' products

Additive since S24b (still version 2). `REF` on `sweden` and `nordic` stays
the provider's own composite (SMHI's, EUMETNET OPERA's). Their other
products are made by the engine from the radars' volumes, like My mosaic
but with every radar of the composite's network at its full range: `sweden`
from the 12 SMHI radars, `nordic` from every radar in `hello.sites` (41: 12
SMHI, 12 MET Norway, 12 FMI, 5 DMI). `mymosaic` offers none.

**Choosing.** `hello.sites[].products` of both is
`["REF","LOWB","CAPPI","CMAX","ETOP","VIL"]`, `elevations` `[]`. A grid
station always opens on its composite (`state.product` `REF` index 0; see
[Products](#products), station switches), so a product is shown only after
a `set_product` while the station is selected: `CAPPI` with `heightM` and
`above` as for a radar, `CMAX`, `ETOP`, `VIL`, or `LOWB`; any
`elevationIndex` but 0, or `HYBRID`, is refused. `REF` goes back to the
composite. A client older than S24b hides the product chooser on a grid
station, so it only ever sees the composite there.

**Frames.** Grid frames like a composite's (`frame.kind` `grid`, the grid
and code texture formats), so a client draws them as it draws `REF`:

- The texture is the composite's box on the Web Mercator lattice of 2,000 m
  texels counted from x = 0 and y = 0 (My mosaic's and the terrain grid's):
  `nordic` 3–33° E, 53–71.5° N, 1671 × 2297 texels (its composite 1670 ×
  2297); `sweden` SMHI's composite box, 5.32–29.83° E, 53.70–70.03° N,
  1365 × 1984. The west and north edges move out to the lattice, at most
  one texel. Nothing is misregistered: every frame carries its own grid
  bounds (`frame.grid`), the composite's unsnapped (3° E, 71.5° N for
  `nordic`) and a product's on the lattice, so a client places each where
  it belongs; only their texel boundaries differ, by under 2 km.
- `id` is `<station>-<T compact>-<product>`, the product part as a radar's
  (`cmax`, `etop`, `vil`, `cappi2000`, `cappi1000g`) or `lowb`; catalogued
  per product like a radar's (the composite is the `-e0` ring, and at most
  two products are kept besides).
- `product` and `productName` as a radar's (`LOWB` "Lowest beam", `CMAX`
  "Column max", `ETOP` "Storm height", `VIL` "Rain mass", `CAPPI` "Height
  2 km", "Height 1 km above ground"); `ETOP` and `VIL` bring their own
  `units`, `palette`, `bounds`, `scale` and `offset` ([Storm height and rain
  mass](#storm-height-and-rain-mass)), the others the reflectivity's. An
  `ETOP` grid texture marks "at least" texels with G bit 8.
- `scanTime` is the nominal time T (a multiple of 5 minutes), `sweepEnd`
  the end of the latest scan used, `elevationDeg` 0, `site` the station's
  position (as its composite's frames).
- `attribution` names the owner of every radar the frame drew on, joined by
  `; ` (`SMHI, CC BY 4.0; MET Norway, CC BY 4.0; FMI, CC BY 4.0; DMI, CC BY
  4.0`), plus the terrain credit above the ground; `grid.sourceProjdef`
  names the product and the radars used.

**How a texel is made.** Each radar is placed on the texture exactly as in
My mosaic (great-circle ground distance `s` and bearing on the 6,371 km
sphere, slant range and gate on the 4/3 earth at the scan's own elevation,
the ray nearest the bearing within 0.75°); a radar is a *candidate* where
`s` is within its `rangeKm` and the gate is one of its gates. No averaging:

- `CMAX`, `ETOP`, `VIL`: each radar's own product, exactly as its radar
  frame is made ([Products](#products), [Storm height and rain
  mass](#storm-height-and-rain-mass)), drawn on its lowest scan's rays and
  gates and placed at that frame's `elevationDeg`; then the **maximum** over
  the candidates. `CMAX` and `VIL`: the highest measured code (2 and up).
  `ETOP`: the highest top, and of two equal tops the exact one (an even
  code) over the "at least" one (an odd code): one radar saw above it. None
  measured: 0 if any candidate is 0, else 1.
- `CAPPI` at `heightM` above `sea` or `ground`: My mosaic's `height` rule
  over the composite's radars (each radar's scan by `CAPPI`'s height rule,
  then the radar whose beam centre is nearest the height, the nearer radar
  on a tie; none: code 1).
- `LOWB`, "Lowest beam", the mosaic's answer to rain near the ground: per
  radar the scan of its lowest **clear** beam at the texel's bearing. With a
  blockage table (`engine/data/blockage.json`, the one `HYBRID` uses:
  SMHI's, MET Norway's and DMI's radars) it is `HYBRID`'s pick for the
  degree `floor(bearing)`: the lowest scan whose angle, rounded to tenths,
  is at or above the table's entry, else the highest; without one (FMI's
  radars) the lowest scan. Its gate at the texel is placed at that scan's
  own angle, and the radar is a candidate there when that gate is not no
  data (code 1). Of the candidates, the one whose beam centre is lowest
  above sea level (`altM + R cos(e) / cos(e + s/R) − R`, to the metre), the
  nearer radar on a tie; its code, 0 included. This is My mosaic's `lowest`
  rule with each radar's clear beam in place of its lowest scan.
- No candidate: code 1 (no data, G bit 4). No bias correction is applied;
  `scripts/radar-bias.py` estimates each overlapping pair's offset offline
  and applies nothing.
- A radar whose volume at T reaches less than its full range (DMI's short
  scans) takes the texels past its lowest scan's edge from its latest longer
  volume up to 10 minutes older, as in My mosaic.

**Timing and cost.** My mosaic's timing (S25): the frame for T is built when
every radar's volume for T is in, or once it is T + 8 minutes and no volume
has come for 30 seconds; a volume that arrives after its frame was built
builds it once more under the same `id` while T is one of the two newest
frame times and under 12 minutes old. A frame before the newest that fewer
than a third of the composite's radars reached is not built at all (it
would stay in the loop for good; `engine.log` says so). A radar missing
from a frame is named
in its provenance (`engine.log`), and its area falls to its neighbours or is
no data. Each radar is read by its provider's own poller through the tilt
store, pollers 2 seconds apart, each provider one volume at a time
engine-wide; the frame is then made from the tilt store (it must be on,
`OMASTORM_TILTS_MB` above 0, else the station reports offline with that
reason).

- `CMAX`, `ETOP`, `VIL` and `CAPPI` read each radar's **whole volume**
  (an SMHI volume about 39 range requests and 740 KB, a MET Norway or DMI
  file read whole, 0.7–1.2 MB, an FMI volume five files, about 0.6 MB), so
  after one of them the other three, any height and `LOWB` are made from
  the tilt store with no request. They build back the newest **3** frame
  times (a quarter of an hour; the first fill reads four volumes a radar).
- `LOWB` reads only the scans its clear beams need (for most radars the
  lowest one or two; an FMI radar its lowest file): an SMHI volume's clear
  beams cost about 25 range requests instead of 39, while a MET Norway or
  DMI file is read whole as for `CMAX`. It builds back the
  newest **6** (half an hour).
- `OMASTORM_GRID_BACKFILL` (1–12, the engine's environment) lowers both
  depths, for an engine with less to spend; it never raises them.
- A product is polled only while its composite is selected and showing it,
  and while some client is connected: when the engine's last client
  disconnects while a composite shows a product, the engine goes back to
  the composite (`state.product` `REF` index 0; the product's frames stay
  in the catalog, and the choice is kept for the next radar), so a closed
  tab polls no radar all night. The desktop plugin, whose bar keeps one
  connection open, sends `set_product` `REF` itself when its popover and
  window are both closed. None is ever kept warm. A composite kept warm (`OMASTORM_WARM=sweden`)
  is, like any selected station, not polled by its warm poller while it is
  selected, so while one of its products is shown its `REF` ring pauses; it
  backfills the gap when `REF` is shown again or the station is left. The
  measured bytes, requests and seconds per frame are in
  `coord/log/S24b.md`.

## Sections and profiles

Additive since S24c (still version 2). A vertical cut through what the
selected station shows, along a line the user draws (`set_section`), and
one column's values at a point the user presses (`profile`; the web client
asks on a long press, ~500 ms without moving, or a right click): how tall a
storm is, and which radars' beams sampled it. Both are made by the engine from the radars' volumes in the
tilt store, for the **newest** complete frame of the timeline only. They
never make a request of their own and never change what is polled: the
engine uses the volumes it already holds for what a client is showing.

**What it is made from.** The station's radars, as its frames place them:
a radar alone at its full range, My mosaic's set with its reaches, a
composite's every radar at full range (`sweden`, `nordic`, as [its
products](#the-composites-products)). Each radar gives every scan the tilt
store holds of its volume at the frame's time T (the 5-minute mark of the
frame's `scanTime`, as My mosaic's nominal time); a radar whose volume at T
reaches less than its full range takes the distances past its farthest scan
from its latest longer volume up to 10 minutes older, as in My mosaic. A
radar with no volume at T in the store is left out. So the cut is as full
as what is shown reads:

- a composite's `CMAX`, `ETOP`, `VIL` or `CAPPI`, a radar's `CAPPI`, `CMAX`,
  `ETOP` or `VIL`, and My mosaic at a height read whole volumes: every scan;
- a composite's `LOWB`, a radar's `REF` or `HYBRID`, and My mosaic's
  lowest-scan rules read one or two scans: the cut shows those beams only;
- a composite's `REF` (the provider's own composite) reads no volume: the
  cut is `empty` unless the store still holds its radars' volumes for T.

**The grid.** Built when a client asks for a section or a profile and none
is held for the newest frame; one at a time; a transient of the engine,
never stored:

- Columns: the station's frames' box (a radar's reach circle, My mosaic's
  box, a composite's box) on the Web Mercator lattice counted from x = 0
  and y = 0, in texels of **4,000 m** (two of the products' 2,000 m texels a
  side: about 2 km on the ground at 60° N, 1.4 km at 70° N, 2.3 km at 55°
  N). Levels: **24 of 500 m** above sea level, from 0 to 12 km.
- Fill, with no averaging: each radar is placed on the 2,000 m texels
  exactly as in My mosaic (ground distance and bearing on the 6,371 km
  sphere; per scan the gate on the 4/3 earth at the scan's own angle and
  the ray nearest the bearing within 0.75°). Each gate that is not no data
  (code 1) goes to every level its beam covers there: its beam centre's
  height above sea level (`altM + R cos(e) / cos(e + s/R) − R`) plus and
  minus `r·tan(0.5°)`, a 1° beam at slant range `r`, clipped to 0–12 km. A
  cell keeps the **maximum** code (the maximum in linear Z: codes rise with
  dBZ) and counts its samples, up to 255; a 4,000 m column takes the samples
  of its four 2,000 m texels. A cell no beam reached has no samples (no
  data); one whose samples are all below threshold holds 0.
- Each cell also records which radars fed it (up to 8 per column: a column
  that more radars reach records the first 8 in the station's order per
  cell; `engine.log` counts such columns), and each column every radar
  that reached it.
- Held until the newest frame is replaced; or 3 minutes after a client
  last asked for something from it (a new or moved section, a profile);
  or, after a cut only a newer frame asked for (a section standing on
  screen), 30 seconds; then dropped. While a section is set, a new newest
  frame builds the next grid and cuts the section again; nothing else
  builds one. A Nordic grid is about 77 MB while held; its fill seconds and
  the engine's memory are in `coord/log/S24c.md`.

**`set_section`.** `from` and `to` are points (`lat` in [−90, 90], `lon` in
[−180, 180]) at least 2 km apart; anything else is answered with an
`error`. Without either point it clears the section. One section at a time,
shared by every client like the station: the latest `set_section` replaces
it. The engine drops it when the client that set it disconnects, or on a
clear; a station switch keeps the line and cuts it from the new station's
grid.

The cut follows the great circle from `from` to `to` on the 6,371 km
sphere in columns of 2,000 m of ground (the length ÷ 2,000 m rounded up, at
most 300: a line over 600 km has 300 columns of length ÷ 300). Column `i`
takes the grid column containing the point `(i + 0.5)` column lengths from
`from`; outside the grid it is no data.

```json
"section":{"from":{"lat":58.26,"lon":12.83},"to":{"lat":59.93,"lon":10.72},
           "status":"ready","message":"",
           "frameId":"nordic-20260915T120000Z-cmax","scanTime":"2026-09-15T12:00:00Z",
           "texture":"tex/section-nordic-4242-r1757937731000000000.png",
           "columns":110,"lengthKm":219.6,"levels":24,"levelM":500,
           "units":"dBZ","scale":2.0,"offset":66.0,
           "palette":["#34465f","..."],"bounds":[-32,0,10,20,30,40,45,50,55,60,65,70,96],
           "radars":["vara","nohur"]}
```

- `status`: `building` (the grid or the cut is being made; `texture` is
  `""`, and a client may keep showing its previous cut), `ready`, or `empty`
  (nothing to cut: `message` says why, `texture` is `""`).
- `frameId` and `scanTime` name the frame the cut was made for: the newest
  complete timeline entry when it was cut. A client shows the cut only
  while that frame is the newest entry and on screen, and hides it while
  the user steps or scrubs through history (sections of older frames are
  not made).
- `texture`: a PNG, 8-bit grayscale, `columns` wide and `levels` high,
  row 0 the top level (11.5–12 km), the last row the lowest (0–0.5 km),
  column 0 at `from`. Codes as a [code texture](#texture-files): 0 below
  threshold, 1 no data (no beam there), 2–255 measured, value = (code −
  `offset`) / `scale` in `units`, always reflectivity whatever product the
  frame shows, so the section carries its own `palette` and `bounds` (the
  reflectivity's) and a client applies the code texture's class rule with
  them. A new name each time it is cut; referenced by `state`, so it is
  retired 30 s after `section` stops naming it.
- `radars`: the ids of the radars whose beams reached any column of the
  cut, in the station's order; the frame's `attribution` credits them.

**`profile`.** One column at `lat`, `lon` (in range, else an `error`),
answered to the sender only, at once when the grid is held and otherwise
once it is built (a few seconds):

```json
{"type":"profile","v":2,"lat":58.70,"lon":13.40,"status":"ready","message":"",
 "frameId":"nordic-20260915T120000Z-cmax","scanTime":"2026-09-15T12:00:00Z","units":"dBZ",
 "levels":[{"bottomM":0,"topM":500,"dbz":24.5,"samples":6,"radars":["vara"]},
           {"bottomM":500,"topM":1000,"dbz":null,"samples":0,"radars":[]}],
 "radars":["vara","nohur"],"echoTopM":7500,"echoTopAtLeast":false}
```

- `levels`: 24, bottom to top. `dbz` is the cell's maximum, `null` when no
  echo was measured there; `samples` its count, 0 where no beam reached it
  (so `dbz` `null` with samples above 0 is clear air, below threshold);
  `radars` the radars whose beams sampled it, echo or clear air (review
  N3: not only those that saw echo there).
- `radars`: every radar whose beams reached the column; `echoTopM`: the top of the
  highest level at or above 18 dBZ (`ETOP`'s threshold), `null` when none;
  `echoTopAtLeast` (review S3, additive) is `true` when no beam sampled any
  level above that top, so the storm may reach higher (as `ETOP`'s "at
  least"), `false` otherwise and when there is no top.
- `status`: `ready`, `empty` (nothing to cut, as for a section, with
  `message`), or `outside` (the point is outside the grid; every level has
  no samples).

## Terrain

Additive since S30 (still version 2). Heights `above` `ground` measure from
a terrain model the engine carries: `engine/data/terrain-nordic-2km.bin`,
one mean terrain height per Web Mercator texel of 2,000 m (the composites'
and My mosaic's lattice, counted from Mercator x = 0 and y = 0, so a My
mosaic texel is exactly one terrain texel), over the box of every Nordic
radar's reach, in steps of 10 m, sea and below-sea land as 0. Made once,
offline, by `scripts/terrain-grid.py` from the Terrarium tiles `HYBRID`'s
blockage tables use (zoom 8, about 300 m a pixel at 60° N, averaged over
each texel); sources and licence in `data/README.md`. Outside the
box the terrain is 0 (sea level). A point is looked up by the texel that
contains it, nearest, with no interpolation: at 60° N a texel is about
1 km across, finer than the beam is thick.

## Loading progress

`state.loading` (S31, additive, always sent) says how far the load a client
is waiting for has come, or is `null` when nothing is loading. A load starts
with a `select_site`, a `set_product` or a `set_mosaic` that starts a new
poller; a poller restarted by the engine on its own (a quiet feed) starts
none, though its backfill may start a history stage (below).

```json
"loading":{"stage":"first","percent":63,"done":52,"total":82,"unit":"volumes",
           "label":"Nordic Rain mass: 26 of 40 radars in for 14:25Z (1 silent), waiting for Vara, Luleå",
           "stages":[
             {"stage":"first","share":55,"percent":63,"done":52,"total":82,"unit":"volumes","state":"active"},
             {"stage":"build","share":20,"percent":0,"done":0,"total":2,"unit":"steps","state":"waiting"},
             {"stage":"history","share":25,"percent":0,"done":0,"total":0,"unit":"volumes","state":"waiting"}]}
"loading":{"stage":"history","percent":58,"done":7,"total":12,"unit":"frames",
           "label":"Vara Reflectivity 1.5°: 7 of 12 frames",
           "stages":[
             {"stage":"first","share":44,"percent":100,"done":1,"total":1,"unit":"frames","state":"done"},
             {"stage":"history","share":56,"percent":58,"done":7,"total":12,"unit":"frames","state":"active"}]}
"loading":null
```

- `stage`: `first` while the volumes the load's first frame needs are on
  their way, `build` while that frame is being built, sent and drawn, then
  `history` while the frames behind it arrive. A load may have any one of
  them alone.
- `percent`: an integer, 0 to 100: `done` of `total` in `unit`, rounded
  down. Within a stage it never goes down. A stage change starts again from
  the new stage's own count (but see `stages`: the segments before it stay
  full). At 100 `loading` stays for about a second, then becomes `null` (or
  the next stage) — except when the build takes over from `first`, which
  happens at once: the first segment stays full in the bar, which is what
  that second was for, and holding its last label (`waiting for Vara`)
  over a wait it is no longer part of would say something untrue.
- `done` and `total`: what the engine counts, in `unit`, `volumes`,
  `frames` or `steps`. They are counts of what the engine fetches or builds
  anyway; showing them costs no request.
- `label`: English, shown verbatim, naming the station, the product and
  what is counted.
- `under` (in the `first` **and `build`** stages of a composite's product,
  S24b; the build was added with the stage itself, S35): the
  composite's own newest frame (`REF`), a complete `frame` object with its
  stable texture names, which the engine keeps published while it is
  here. While `state.frame` is the loading placeholder a client draws
  `under` in its place, with `under`'s own legend and name, and shows the
  progress beside it. `state.frame` stays the placeholder until the built
  frame is drawn, and the build is part of that wait, so `under` goes when
  the frame that replaces it arrives, not when the build starts. It is
  absent when the composite has no frame.
- `stages` (S35, additive): the whole load in order, one entry per stage,
  so a client can draw **one segmented bar** whose earlier segments stay
  full instead of a single number that starts again at each stage. Present
  whenever `loading` is not `null`; at least one entry.
  - `share`: the segment's width in the bar, an integer of at least 1. The
    shares of a load sum to exactly 100, so a client lays the bar out from
    them alone and needs no rule of its own. They are fixed for the kind of
    load, not a guess at how long each stage will take, so the boundaries
    do not move while it runs.
  - `percent`, `done`, `total`, `unit`: that segment's own fill and counts,
    on the same rules as the top-level fields.
  - `state`: `done` (finished, and it stays full), `active` (the one
    running), or `waiting` (not started; drawn dim, `percent` 0).
  - The top-level `stage`, `percent`, `done`, `total`, `unit` and `label`
    are **exactly** the `active` entry's, so a client that ignores `stages`
    draws the same single bar it drew before S35. While the finished load
    lingers at 100 no entry is `active` and the top level is the last
    `done` one. `under` is top-level only.
  - A stage whose work ends before it is ever published is absent (a build
    that took less than one throttle tick, for instance), so the number of
    segments is what the load really went through.

What each load counts:

- **A radar or a provider's composite** (`frames`): `first` is its first
  frame (`done` 0 of `total` 1) when the station opened on the loading
  placeholder; a station that opened on a catalogued frame skips it.
  `history` is the backfill: the frames its poller will bring after the
  first (those the tilt store holds first, then those it fetches), counted
  as each joins the timeline. A backfill that stops early (a network
  failure) ends the stage at 100. It has **no `build` stage**: its frame is
  a decode and a send its poller has already done, over before the bar
  could draw it, so planning one would only move the divisions. Its shares
  are `first` 44, `history` 56.
- **My mosaic and a composite's products** (`volumes`, S25, S24b): `first`
  counts, over the frame time the engine is closest to completing, the
  volumes each counted radar still has to deliver before that time's own
  (its poller fetches newest first), so the percentage rises while the
  newer volumes come in; volumes the tilt store already holds are done at
  once. A radar counts as in only when its scan is one the frame can
  actually be built from: a short scan whose longer predecessor has not
  arrived yet is in hand but still owes that volume, and the engine will
  not build until it comes, so the stage does not read full over that
  wait. It reaches a true 100 when every counted radar is in — the wait
  that follows is `build`, not the last percent of `first` (before S35 it
  was held at 99 until the frame was on screen, which hid the build, the
  ~1 MB frame and the draw behind one number). The label names how many
  radars are in for that time and, near the end, which radars are still
  being waited for: at most three by name, then `and n more`. `history`
  counts the volumes of the frame times still to build in the set's
  backfill window, and the label how many of those frames are built. The
  load ends once every frame time of the window is built; the next live
  frame five minutes later starts no load. Its shares are `first` 55,
  `build` 20, `history` 25.
- **The build** (`steps`, S35): a made load has one, between `first` and
  `history`; a radar's load has none (above). `total` is 2: `done` 0 while the frame is being built
  (`Nordic Rain mass: building 14:25Z`), 1 once it is built and sent
  (`… drawing 14:25Z`), and 2 when it is in the client's timeline. It ends
  on the same signal as before — the frame reaching the timeline — but
  that signal no longer depends on the client following the newest frame,
  so a client scrubbed back into the past still sees the bar advance. A
  made frame's build is the assembly of every radar's volume into one
  grid — about ten seconds for a Nordic grid — and is the segment that was
  invisible before S35. A build that is over before the bar ever draws it
  (a frame the tilt store made at once) leaves no segment.
- **A silent radar does not count**: a radar whose poller reports it
  silent (it has published nothing for the provider's `unavailable` age,
  30 minutes for SMHI and ORD; Kiruna from 07:15Z on 2026-09-15) leaves
  `total`, and a frame time no longer waits for it: the time builds when
  the other radars are in, not after `QUIET_MS` without arrivals. The
  label says how many are silent. A scan from it counts it again.

The engine changes `loading` at most once a second while counting; a
stage's start, its 100 and its end are sent at once, with the broadcast of
the change that caused them. A client older than S31 ignores `loading` and
shows `connection.status` as before (`under` is not drawn: it shows the
placeholder, which draws nothing, as before S31). A client facing an older
engine finds no `loading` and shows `connection.status` `loading` as it
did. A client older than S35 finds no `stages`, reads the top-level fields
and draws the single bar; a client that draws segments and finds no
`stages` (an engine older than S35) falls back to that single bar too.

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
- `view_center` never hands off to or from a `grid` station, except
  `iberia` (S33, above).

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

Additive since (S25, still version 2):

- `hello.sites` gains the grid station `mymosaic` (`provider` `mosaic`);
  `hello.mosaic` and `state.mosaic` are new and always sent.
- `set_mosaic` is new ([My mosaic](#my-mosaic)). An older engine ignores
  it as an unknown command; a client that finds no `hello.mosaic` offers no
  mosaic.
- An older client lists `mymosaic` among the composites and draws its
  frames as a composite's; with no set chosen it shows the loading
  placeholder.

Additive since (S30, still version 2):

- `hello.mosaic.rules` gains `height`; `set_mosaic` takes `heightM` and
  `above` with it, and `state.mosaic` echoes them for that rule only. The
  `lowest` and `strongest` sets and their frame ids are unchanged.
- `hello.products`' `CAPPI` entry gains `above: ["sea","ground"]`;
  `set_product` takes an optional `above` with `CAPPI`, and
  `state.product.above` is sent with `CAPPI` (`sea` when none was sent).
  Heights above ground are `productName` `Height 1 km above ground`; the
  frame id's product part is `cappi1000g`.
- A My mosaic frame of the `height` rule has `product` `CAPPI`.
- [Terrain](#terrain) is new. A frame above the ground (a radar's or My
  mosaic's) adds `; terrain: Mapzen Terrain Tiles (AWS open data;
  Kartverket, NLS Finland, SDFE, EU-DEM/Copernicus)` to its `attribution`,
  which clients show verbatim.
- A client may hatch a My mosaic height frame's no-data texels only inside
  the chosen radars' reach circles (`state.mosaic` and `hello.sites`);
  outside them no radar was chosen, and it draws nothing there.
- A client facing an older engine finds no `height` rule and no `above`,
  and offers neither. An older client sent a `height` set by another
  client shows its frames as a composite's, named by `productName`, and
  when it resends its remembered set, that set replaces the height one;
  one that lists the rules from `hello` may offer `Height` and send it
  without a height, which the engine takes as 2,000 m above sea level.

Additive since (S24a, still version 2):

- `hello.products` gains `ETOP` "Storm height" and `VIL` "Rain mass", each
  with `units`; `hello.sites[].products` lists them wherever `CMAX` is.
- FMI's radars offer `CAPPI`, `CMAX`, `ETOP` and `VIL` and list their five
  angles in `elevations`, read as one volume per nominal time ([FMI's
  volumes](#fmis-volumes)).
- An `ETOP` or `VIL` frame has its own `units`, `palette`, `bounds`,
  `scale` and `offset` ([Storm height and rain
  mass](#storm-height-and-rain-mass)); every other frame is unchanged. The
  sweep texture's G channel gains bit 8 ("at least", `ETOP` only).
- A client applies the weak-return floor to dBZ frames only. An older
  client lists the two products from `hello` and draws them with their own
  legend, applying its floor in their units (tap the legend to show all),
  and draws no hatch on "at least" tops. A client facing an older engine
  finds neither in `hello` and offers neither.

Additive since (S24b, still version 2):

- `hello.products` gains `LOWB` "Lowest beam" after `HYBRID`; no radar
  offers it.
- `sweden` and `nordic` list `["REF","LOWB","CAPPI","CMAX","ETOP","VIL"]`
  in `hello.sites[].products` (before: `[]`), and `set_product` is accepted
  while one of them is selected ([The composites'
  products](#the-composites-products)); `REF` stays the provider's
  composite.
- Selecting a grid station sets `state.product` to `REF` index 0 (before,
  it kept the radar's choice on display while showing the composite); the
  choice is still carried to the next radar.
- An `ETOP` grid frame's odd codes are "at least" tops (G bit 8 in its grid
  texture).
- A client older than S24b hides the product chooser on a grid station and
  so shows only the composite there; one that sees a product frame (another
  client chose it) draws it as a grid frame with its own legend. A client
  facing an older engine finds `[]` in a composite's products and offers
  nothing.

Additive since (S31, still version 2):

- `state.loading` is new and always sent: `{stage, percent, done, total,
  unit, label}` and, in the first stage of a composite's product, `under`;
  `null` when nothing loads ([Loading progress](#loading-progress)).
- A frame time of My mosaic or a composite's product no longer waits for a
  radar its poller reports silent.
- A client older than S31 ignores `loading`; a client facing an older
  engine finds none and shows `connection.status` `loading` as before.
