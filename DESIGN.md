# Omastorm design

How a change, feature, or fix should behave.

[README.md](README.md) is install and use. [docs/protocol.md](docs/protocol.md)
is the wire. [docs/configuration.md](docs/configuration.md) is the config keys.
[CONTRIBUTING.md](CONTRIBUTING.md) is the contribution workflow. Honor these; ask before
violating them.

## Picture

Weather occupies the view. Geography stays quiet: thin lines, sparse labels,
rings, a crosshair. Chrome follows the Omarchy theme; radar color comes only
from `frame.palette`, shared by the legend and the shader.

Pixels, Glyphs, and Stipple all stay. They sample the same cell and palette
and differ only in how the 3 px cell is painted. Glyphs is the default. A
shader or sampling change updates all three and is shown with a capture, not
described.

Show actual scan times. Label archived data. Missing, range-folded, and
below-threshold stay distinct from measured values. Whatever a change hides
is named in the legend. Keep OSM (ODbL) and Natural Earth attribution with
the data and on screen.

## Window chrome

Use these names when discussing or changing the expanded window. They are
the ids and comments in `ui/RadarWindow.qml`.

| Name | What it is |
|---|---|
| **brand row** | Mark, OMASTORM, status light, LIVE / ARCHIVED |
| **site row** | Station title, radar lock (yellow when the camera is outside that radar's rings) |
| **product stack** | Right column: product line + meta line |
| **product line** | Product name / tilt and SMHI |
| **meta line** | Age, right-aligned under the product line |
| **map stage** | Radar map frame |
| **follow chip** | Crosshair on the map (place follow); hidden until GPS is wired |
| **help chip** | Keys / `?` on the map |
| **scale bar** | Ground distance under the map, left; locale picks km or mi; label updates with zoom |
| **legend** | dBZ scale directly under the map |
| **transport** | Playback buttons |
| **tick strip** | Frame ticks on the timeline |
| **strip stamp** | Date / time / zone above the tick strip |
| **frame index** | `N / total` above the strip, right-aligned; counts available frames only |

**Bottom chrome order.** Map stage, then legend, then transport + tick
strip, with the strip stamp left-aligned and frame index right-aligned
on one row above the ticks. Playback buttons align with the track at the bottom.

**Product stack.** Compact product line (name, then SMHI). The meta
line is the age only, right-aligned under that row.

**Time.** Age on the meta line is how stale the frame on screen is. The
strip stamp is the absolute observation time (date, time, zone). Locale
picks date order and 12/24h only; dates stay numeric. Locale also picks
kilometres or miles for the scale bar and picker distances. The tick strip is
position in the loop, not a second clock. It has 60 positions when the
window is wide enough; compact widths show one tick per available frame
only (empty pads need room or they read as a dotted cliff). An extra live
sweep beyond 60 completed scans adds a selectable tick and is included in
the frame count. Available frames fill from the left; unused positions are
faint, short, and cannot be sought. Each available tick represents one
frame, without extra gap ticks or a baseline.

## Location, onboarding, and map

Map center and radar source are independent. The center is the place the
user wants to see; the radar supplies one station's exact sweep. Loading a
frame or handing off to another station never moves the camera.

After the plugin ensures the engine is available and starts it, resolve the
map center in this order:

1. Explicit `center_lat` and `center_lon` in `config.toml`.
2. The last center remembered in `state.json`.
3. Valid coordinates from Omarchy's weather location (`weather.json`).
4. The location prompt: use approximate location or choose manually.

When the first three sources have no valid center, show two actions in the
existing prompt, with no additional dialog. "Use approximate location" runs
one bounded `curl` to `wttr.in/?format=j2` only on click (UI-side; not an
engine command); the provider and public-IP use are documented, and a
successful view is labeled `IP NEAR …`. "Choose
manually" opens the existing search and coordinates.
Remember a successful estimate like any chosen view. While it is pending,
manual selection remains available and takes priority over a late reply.
Failures show a short error and allow an explicit retry. There is no IP
configuration knob; a click is the opt-in. IP never enables GPS or tracking.
Offer place search and "Enter coordinates", which reveals
labeled latitude and longitude fields with validation. Place search is an
engine `search_places` reply over GeoNames cities with population ≥ 5000
in the network envelope (county and country so two Skåres are
distinct); map labels stay Natural Earth. "Show radar" accepts the location.
No separate setup wizard or settings window is required. Keep the picker
reachable after onboarding (`Shift+H` and LOCATION). Coordinate entry
chooses a view; it does not create a permanent config override or lock a
radar. Choosing a location writes `state.json`, never `config.toml`.

Reuse Omarchy's location when available without requiring its weather plugin.
Read weather settings only; never write them. Location search is an explicit
user action handled through the engine. Approximate IP lookup is an explicit
UI action via wttr.in (`format=j2`, smaller than Omarchy weather's `j1`); the
launcher and engine perform no IP lookup. Do not use GeoClue. Archived views
never locate, and checks require the same explicit action as users.
Resolve the radar separately: an explicit `locked_radar` in config wins,
otherwise restore a remembered radar lock, otherwise choose the station
nearest the map center. A radar lock alone does not supply a map center or
bypass location onboarding. Newly chosen locations start unlocked unless a
configured radar override applies.

Remember center and zoom after movement settles, and remember changes to the
UI radar lock. Unlocked radar selection follows the center using the protocol's
nearest-station hysteresis; do not wait until the center leaves the radar's
rings. Lock pins the source; `n` releases it and selects the nearest station
without moving the camera. Choosing a station in search locks it and centres
the map on that site. Automatic hand-off and loading a frame never move the
camera. Do not persist the automatically selected station.

Closing preserves the view. Reopening restores it, with explicit config
values taking precedence. Expanding the popover preserves its center, zoom,
station, frame, and playback. An engine reconnect restores the necessary
commands without resetting the user's camera. Weather location supplies an
initial view; subsequent weather changes do not overwrite a remembered view.

Explicit coordinates are honored on every launch and do not imply a radar
lock. A configured center far from a locked radar is valid: preserve both,
show the station and lock clearly (yellow lock when coverage is outside the
view), and offer "Use nearest radar" and "Go to selected radar" when that
radar's coverage is outside the view. UI navigation
and unlocking can change the active session; explicit config applies again
on launch.

A station with no frame yet is the map without radar. Show no loading animation.
Display one radar station’s sweep at a time.

## Split

The engine fetches, decodes, caches, and rasterizes. The UI is small state
plus GPU textures. Radar values do not enter JSON or QML. Pan and zoom are
uniforms. The engine reads neither `config.toml` nor `state.json`; the UI
resolves preferences and remembered state, then sends commands.

New settings are optional, omit means default, and a bad value is named in
the status slot. Keep deliberate settings in `config.toml` and session restore in `state.json`.
The app never rewrites config because the user pans, zooms, or changes a lock.
Write `state.json` atomically. See [configuration](docs/configuration.md) for
file ownership and precedence. Do not write Omarchy, Hyprland, or system
configuration.

A product is a texture, legend, units, timestamp, and source from the engine.
SMHI's lowest-tilt reflectivity (DBZH), or its national composite, is what is
drawn; NEXRAD Level II only in archive mode.

The live poller reads SMHI's listing every minute and fetches each new
volume whole, by HTTP range requests: there are no chunks and no partial
sweeps. When the listing lags, it probes the next scan's dated file once a
scan is overdue.
Independently, if the poller task has exited, or the newest radial is thirty
minutes old and discovery has not been tried since, spawn a new poller.
Reselecting the current station is a no-op while the poller is running; if
the task has ended, start it again. Cached frames stay on screen through a
rediscovery. A rediscovery that finds only a sweep already in the catalog
leaves the frame and connection chrome alone; a newer volume still clears
UNAVAILABLE / OFFLINE.

## Loop buffer

Both front ends keep the newest complete frames of the station's timeline
loaded and play them locally (`ui/Engine.qml`, the web's `app.js`): at most
24, oldest first, starting once six are ready. What they may hold:

- **Desktop:** a quarter of the machine's `MemAvailable`, read from
  `/proc/meminfo` each time the window or popover opens, at least 96 MB and
  at most 280 MB for the window, 160 MB for the popover. The popover shows
  "ready/target · MB of cap" under the timeline while it plays.
- **Web:** 160 MB on a phone, 512 MB on a desktop, lower where
  `deviceMemory` says so. Each frame is costed on its own (a ring can mix
  frames with and without a code texture). The first six frames may use up
  to 1.6 times the cap, so that a big frame still gets a loop (a Europe frame
  without a code texture, ~41 MB). The Loop note names what the cap left out.

**Texture memory in Qt: 4 bytes a texel, and there is no R8 path from QML**
(Qt 6.11, settled in S28). `QQuickDefaultTextureFactory` keeps ARGB32, RGB32
and the float formats as they are and converts everything else, the code
PNG's `Grayscale8` included, to `ARGB32_Premultiplied` when the image loads.
The texture-file path (`.ktx`, `.pkm`, `.astc` through `Image`) maps only
compressed formats (DXT, ETC2, ASTC). An uncompressed `GL_R8` KTX fails to
load (tested). The lossy one-channel formats (BC4, EAC R11) cannot carry
exact codes. So on the desktop a code texture saves the PNG read and decode,
not memory. On the web (WebGL `R8`) it takes one byte a texel. The only way
to get one byte a code in Qt would be a packed texture from the engine (four
codes to an RGBA texel, unpacked in `radar.frag`). That would be a protocol
change, and it has not been made.

## Scope

Keep the feature set small. Prefer the weather panel, the theme, and the
engine's state over a parallel mechanism in this app. If a visual call is
open, change the running picture and look at it.
