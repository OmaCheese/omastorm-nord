# Omastorm Nord

> **Omastorm Nord** is a fork of [Omastorm](https://github.com/wesleygrimes/omastorm)
> that shows Nordic weather radar instead of NOAA NEXRAD: 41 radars — the 12
> [SMHI](https://www.smhi.se/) radars in Sweden, 12 in Norway, 12 in Finland
> and 5 in Denmark — plus SMHI's national composite, EUMETNET OPERA's Nordic
> composite, and a mosaic of the radars you choose. Over the radar it can draw
> temperature and wind from the Nordic weather stations and MET Norway's
> MET Nordic analysis, and lightning from FMI's NORDLIS network. It
> installs beside upstream under its own plugin id (`omacheese.omastorm-nord`) and
> directories (`omastorm-nord`).

Open-source, live weather radar for the Omarchy desktop. Beta.

[![Omastorm window: live take with loop, search, keys, and treatments](https://github.com/OmaCheese/omastorm-nord/releases/download/v26.9.0/omastorm-preview.gif)](https://github.com/OmaCheese/omastorm-nord/releases/download/v26.9.0/omastorm-demo.mp4)

Live over Vara, in Västra Götaland. The media is made by
`scripts/capture-readme.sh` and `scripts/capture-demo.sh`.

A radar that lives in your bar. The popover shows the station nearest you with
the actual scan time. Click the map (or press Enter) for the full window: every
Nordic radar and both composites, reflectivity at native resolution, a
timeline you can scrub, all drawn in your Omarchy theme.

![The Omastorm window, live](https://github.com/OmaCheese/omastorm-nord/releases/download/v26.9.0/window-live.png)

![The Omastorm popover, live](https://github.com/OmaCheese/omastorm-nord/releases/download/v26.9.0/popover.png)

A headless Rust engine fetches and decodes the radars' ODIM HDF5 volumes and
the composites, reads the weather stations, the MET Nordic grid and the
lightning feed, and prepares GPU-ready textures. An Omarchy plugin built with
Quickshell/QML is the client: it displays them in the bar popover and full
window.

## Features

- **Live.** A Rust engine polls each service's open data every minute: SMHI for
  Sweden, EUMETNET's Open Radar Data cache for Norway, Finland and Denmark, and
  OPERA for the Nordic composite. Each publishes one scan per radar every 5
  minutes, a few minutes after it is taken; the scan appears whole. Stale data
  says it is stale.
- **Every radar.** Pan the map and it follows the nearest station, or search by
  station, town, or county. Pick Sweden or Nordic for a composite, or My mosaic
  for the radars you chose.
- **Timeline.** Up to 60 scans per station, cached locally. Play, step, scrub.
  Frames left from an earlier session leave the timeline once a new frame
  arrives, so the loop never jumps from this morning to now; offline, they
  stay.
- **Weather layers.** Temperature and wind from about 700 weather stations in
  Sweden, Norway, Finland and Denmark, the MET Nordic 1 km analysis under
  them, and lightning strikes from FMI's NORDLIS network with a fading
  30-minute trail. All off until you switch them on in LAYERS.
- **Loading you can see.** A cold load shows its steps and one overall
  percentage; a stuck load or layer has one Reset.
- **Three treatments.** Glyphs, Pixels, and Stipple sample the same gate and
  paint the cell differently.
- **Native.** Colors, font, and spacing come from the active Omarchy theme and
  change with it, **light themes included**: the theme says whether it is light
  or dark, and the map's ground, lines and echo follow. Text follows Omarchy's
  text size (`omarchy-display-text-size`) without a restart. Radar sites are
  drawn in their own color, so an antenna is never mistaken for a town.
- **Keyboard first.** Everything the pointer reaches is a keystroke, and every
  window key in the table below is rebindable. A right click on the map opens
  a menu of the same choices.
- **Honest.** Actual scan times. Missing and below-threshold returns are drawn
  distinctly from measured values. Displays one radar's lowest sweep at a time,
  or a composite. Each radar names the service that owns it, and the radars are
  not identical: see "The radars are not identical" below.

## Install

Omarchy 4 on x86_64 and aarch64.

```sh
omarchy plugin add https://github.com/OmaCheese/omastorm-nord.git --enable
```

This clones the plugin into `~/.config/omarchy/plugins/omacheese.omastorm-nord` and
asks which bar section to use. The first time the popover opens it downloads
the pinned engine binary from this repository's GitHub Releases, verifies its
sha256 against `engine/release.pin`, and installs it under
`~/.local/share/omastorm-nord/bin`. Runtime files, cached data, remembered view state, and configuration stay
inside Omastorm's own directories.

From a checkout instead, to work on it: `mise setup`, then `mise plugin-link`
points the bar at the checkout, which runs its own engine build.

On first use, Omastorm uses your Omarchy weather location when available.
Otherwise, choose a place manually or click **Use approximate location** to
estimate your city using your public IP via wttr.in. No IP-location request
is made before you click. To set
a fixed launch location, including during agent-assisted installation, see
[configuration and remembered state](docs/configuration.md).

To open the window from the keyboard, add one line to
`~/.config/hypr/bindings.lua`. Omastorm never writes that file.

```lua
o.bind("SUPER + SHIFT + R", "Omastorm Nord", "omarchy shell shell toggle omacheese.omastorm-nord '{}'")
```

To list Omastorm in the app launcher:

```sh
bash ~/.config/omarchy/plugins/omacheese.omastorm-nord/scripts/write-desktop-entry.sh
```

Update with `omarchy plugin update omacheese.omastorm-nord`.

## Use

Click the mark in the bar for the popover: the map at your location, LIVE or
the connection condition, the actual scan time, and step and play. Click the
map (or press Enter) to expand.
If no location is known, the popover offers “Choose a location,” which opens
the picker in the window. Click the radar or press Enter for the window; it
opens on the same station, frame, and camera. Closing preserves your view
for the next launch.

In the window, drag to pan and scroll to zoom. The map follows the nearest
station as you pan unless you lock it; a locked radar stays put even when
the camera leaves its coverage, and the lock turns yellow outside the rings.
A scale bar under the map shows ground distance in your locale (km or mi).
A station you arrive at fetches its last 60 scans (about five hours) in the
background, newest first, so there is a loop to play within seconds.
The stamp above the timeline is the absolute scan time; the meta line is how
stale that frame is. LIVE, STALE after 15 minutes, UNAVAILABLE after 30 or
when the service publishes nothing for a radar, OFFLINE when it cannot be
reached, with cached frames kept. A healthy frame is already 5 to 10 minutes
old. When you come back to a radar or product hours later, its old frames
show at once and leave the timeline when the first new frame arrives; if
nothing new comes (offline, a silent radar), they stay.

Click the station or region name at the top left, or press `/`, for the site
picker: it drops down under the name and lists every radar and composite,
grouped by country, nearest first. Type to filter by station, town, county,
country or node code; the wheel, the arrows, Page Up/Down and Home/End move
through the list, Enter chooses.

| Key | Action |
| --- | --- |
| `h` `j` `k` `l` or arrows | Pan |
| `+` `-` | Zoom |
| `0` | Reset to the configured or weather location |
| `/` or `s` | Search sites (center and lock) |
| `n` | Nearest site |
| `Shift+L` | Lock the station |
| `Shift+H` | Choose a location |
| `Shift+R` | Reset: reload the radar and the weather layers (station, product, layers and view kept) |
| `Ctrl+L` | LAYERS panel |
| `Space` | Loop the frames |
| `[` `]` | Step a frame |
| `Home` `End` | Oldest or newest frame |
| `1` `2` `3` | Pixels, Glyphs, Stipple |
| `w` | Show weak returns |
| `?` | Keys sheet |
| `Esc` | Close |

Measured returns under 5 dBZ (insects, birds, ground clutter on a clear day)
are hidden by default and the legend says so; `w` shows them.

Click a legend to enlarge it: the radar legend opens as a card with the
product's name and unit, the weather legends grow in place. Any click or
`Esc` shrinks it.

Right-click the map for a menu at the pointer: the composites, My mosaic,
the radars you visited in this window and More… (the picker); the product;
the layers; Location…, Keys, and Reset. Up, Down and Enter work in it, Right
opens a submenu, `Esc` closes it.

### Loading

While nothing is drawn yet, a card over the map lists the steps with one
overall percentage: Starting engine, Fetching radar data (with how many
files or radars are in), Engine: building the frame, Drawing, and Fetching
history. Once a frame is on screen the card folds into the bar above the
timeline, which names the step. The popover shows the percentage and the
step in its strip. Weather layers that are loading show there too
("Fetching weather stations (2 of 4 providers)").

### Reset

When something looks stuck (a load that does not finish, a layer that never
shows, a timeline that stopped moving), `Shift+R`, the RESET row in LAYERS,
the right-click menu or ↻ in the popover reloads the radar and the weather
layers with your current choices. The station, product, layers and view are
kept, and so are the volumes and frames already on disk. A second reset
within 10 seconds does nothing.

### My mosaic

My mosaic is a composite of up to 12 radars you choose, for when a distant
radar's high coverage misleads. Choose it in the site picker, or click the
RADARS chip while it is shown, to open its panel. The panel docks at the
right of the map and the map shrinks beside it, drawing every radar as a
mark: click a mark to tick or untick it. The panel lists CHOSEN (each with ✕
to remove it; click a row to centre the map on it), a filter, and ALL
RADARS by country, nearest first. Its keys: Up/Down or `j`/`k` move, Space
or `x` ticks, `/` goes to the filter, `r` steps the rule (Lowest beam,
Strongest, Height), `[` `]` step the height, Enter shows the mosaic, `Esc`
cancels. The set is remembered.

### Weather layers

The LAYERS chip beside the product line, or `Ctrl+L`, opens a panel with
four switches and a source row:

| Row | Key | What it draws |
| --- | --- | --- |
| Radar | `1` | the radar, on by default |
| Temperature | `2` | air temperature per station as a small label (“12°”) on one cold-to-warm scale; from the grid, a translucent field under the radar |
| Wind | `3` | an arrow per station pointing where the wind blows **to**, longer with speed, with the speed in m/s beside it and the gust in brackets (“7 (12)”); a ring when calm |
| Lightning | `4` | NORDLIS strikes: a cross for cloud-to-ground, a dot for in-cloud, fading from white to orange over 30 minutes. While you loop or step back, the strikes around that frame's time |
| From | `5` | where temperature and wind come from: STATIONS, GRID (the MET Nordic analysis) or BOTH, stations drawn over the grid |
| Reset | `r` or `6` | Reset, as above |

Up/Down move, Space or Enter flips a row, Left/Right move along From.
Temperature, wind and lightning are off until you switch them on, and each
switch is remembered. Stations are thinned by zoom so labels never overlap;
hover a station for its name, provider, time, temperature, wind and gust. A
legend in the corner lists the temperature scale and the wind speed bands in
m/s.

A layer that is on says under its name what it is doing: loading, how many
stations or strikes it has, or what failed (“Frost: no client ID”, “DMI
error 503”) instead of drawing nothing. Nothing is fetched for a layer until
it is switched on. Stations update at most every 10 minutes,
the grid once an hour (MET Norway writes each hour's analysis about 15
minutes after it), lightning at most once a minute.

Norway's stations come from MET Norway's Frost API, which needs a free client
ID; see [Configuration](#configuration). Without one, Norway's stations are
left out and the other three countries still show. The MET Nordic grid needs
no ID and covers Norway too.

## What each product says about the weather

| Product | What it measures | What it tells you |
| --- | --- | --- |
| `Reflectivity` at an angle | how much the drops in that one beam send back, in dBZ | the raw view. Under 20 dBZ drizzle or cloud, 20–35 light to moderate rain, 35–45 heavy, 45–55 very heavy and possibly hail, over 55 almost certainly hail |
| `Clear view` | per direction, the lowest angle the terrain does not block | the best guess at what reaches the ground, in fjords and sierras especially |
| `Height` | one altitude, above sea level or above ground | storms compared fairly at the same level. Where no beam passes, the map says “no radar at this height”, which is not “no rain” |
| `Column max` | the strongest echo anywhere above each point | finds cores, including ones still aloft. It overstates what is falling now |
| `Storm height` | the top of the 18 dBZ echo, in km | storm depth, the best single clue to vigour: 3–6 km showers, 8–10 km thunderstorms, over 10–12 km severe with hail likely. “At least” means the highest beam still found echo, so the top is higher |
| `Rain mass` | the water in the column, kg/m² | over about 25 heavy rain; very high in summer usually means hail, which reflects far more than rain. A sudden collapse over a cell suggests a downburst |
| `Lowest beam` (composites) | which radar sees a point lowest | a data-quality map: where the composite sees near the ground, and where all of it is aloft |
| A composite (`Sweden`, `Nordic`) | the provider’s combined picture | country-wide rain at a glance; each point may come from a different radar at a different height |
| `My mosaic` | only the radars you pick, combined by the lowest beam, the strongest echo, or at one height | leaves out a radar whose distant, high coverage misleads. Rain mass over your own set too |
| `Relief` | `Storm height` drawn as a lit surface | the same data made readable: cells stand up, flat rain looks flat |

What to distrust: the bright band at the melting level looks like heavy rain
and is not; hail inflates both reflectivity and rain mass; ground, sea and
insect clutter sit near the antenna; past about 150 km the beam overshoots
light rain and snow entirely, so an empty map is not proof of a dry sky; heavy
cores absorb the signal and weaken everything behind them. Rain rate is
inferred from returned energy, never measured.

### The radars are not identical

Only the beam width (about 1°) is much the same everywhere. Transmit power is
not published in the files, and reach in practice is set by the scan strategy
and the gate layout, not by power alone:

| Country | Radars | Published range | Gate | Volume | Lowest angle |
| --- | --- | --- | --- | --- | --- |
| Sweden (SMHI) | 12 | 240 km | 500 m (360 × 480) | every 5 min | 0.5° |
| Norway (MET) | 12 | 240 km | 250 m (960 gates) | every 5 min | 0.5° |
| Finland (FMI) | 12 | 250 km | 500 m (500 gates) | every 5 min | 0.1–0.5° |
| Denmark (DMI) | 5 | 237.5–238 km | 500 m | every 5 min | 0.48–0.51° |

So a Norwegian radar resolves twice as finely along the beam as a Swedish or
Danish one, and Finland starts lower, which is why its radars see furthest for
the same height. The number of angles differs too, which is why `Storm height`
and `Rain mass` read “at least” sooner on some radars than others.

Range on its own is a file header, not what a radar can see. What matters is
beam height: at 0.5° the beam centre is about 2 km up at 125 km and about 4 km
at 200 km, wherever it stands. The range circles in the window show the useful
reach of the product you are looking at, not the header number.

## Configuration

`~/.config/omastorm-nord/config.toml` holds deliberate preferences. The app saves
last map center, zoom, and UI radar lock separately in
`$XDG_STATE_HOME/omastorm-nord/state.json` (default
`~/.local/state/omastorm-nord/state.json`). Navigation never rewrites your config.
`Shift+H`, or LOCATION, opens the location picker; it writes state, not config.

Explicit center coordinates win on every launch. Without them, Omastorm
restores your last view, then falls back to weather or the location prompt. Radar selection is independent: a configured lock wins, otherwise a
remembered lock is restored, otherwise the nearest radar follows the map.

```toml
# Optional: always open here. Omit both to remember the last map position.
center_lat = 57.70716
center_lon = 11.96679
# locked_radar = "vara" # optional radar override; coordinates do not imply a lock

treatment = "GLYPHS" # PIXELS, GLYPHS, or STIPPLE at launch
weak_floor = 5       # dBZ; false draws every measured return

[keys]
pan_left = "h Left"
zoom_in = "+ ="
```

A bad value is named in the status slot and that setting stays on its default.
Every action name, the key syntax, and what each setting does are in
[docs/configuration.md](docs/configuration.md).

The weather layers' switches and My mosaic's set are remembered beside
`state.json`, in `layers.json` and `mosaic.json`.

### Norway's weather stations: a Frost client ID

MET Norway serves its station observations through the
[Frost API](https://frost.met.no), which needs a client ID. It is free:
create one at <https://frost.met.no/auth/requestCredentials.html> with an
e-mail address. Put the ID in `~/.config/omastorm-nord/frost.env` and make
the file readable only by you:

```sh
mkdir -p ~/.config/omastorm-nord
printf 'FROST_CLIENT_ID=%s\n' 'your-client-id' > ~/.config/omastorm-nord/frost.env
chmod 600 ~/.config/omastorm-nord/frost.env
```

The engine reads the file (under `$XDG_CONFIG_HOME` when that is set) each
time it updates the stations, every 30 seconds while a station layer is on,
so a new ID takes effect without a restart. A `FROST_CLIENT_ID` in the engine's environment wins over the file.
The client secret Frost also gives you is not used. The ID is sent only to
frost.met.no, as HTTP basic auth, and never written to a URL, a log or the
cache.

Without an ID, Norway's stations are left out, LAYERS says “Frost: no client
ID”, and the other countries' stations still show. The MET Nordic grid
covers Norway without one.

## Troubleshooting

If expand or the keybind does nothing after `omarchy plugin update`, the
shell still has the previous QML types. Restart it:

```sh
omarchy restart shell
```

The engine runs as one shared daemon per login. Its log is
`$XDG_RUNTIME_DIR/omastorm-nord/engine.log` (usually `/run/user/<uid>/omastorm-nord/`).
When SMHI's listing lags, the engine probes the next scan's dated file once a
scan is overdue, and it backs off on errors, keeping cached frames available.
Both are recorded in `engine.log`.

If the popover says the engine could not be installed, the download or its
sha256 check failed; the reason is in `bootstrap.log` in the same directory,
and opening the popover again retries. To restart the engine by hand:

```sh
~/.local/share/omastorm-nord/bin/omastorm-engine stop
```

The next popover or window starts it again. Please attach both logs to a
[bug report](https://github.com/OmaCheese/omastorm-nord/issues).

A load or a weather layer that seems stuck: press `Shift+R` (Reset) before
restarting anything. It reloads with your current choices and keeps what is
on disk.

A weather layer that is on but draws nothing says why under its name in
LAYERS: “Frost: no client ID” (see
[the Frost client ID](#norways-weather-stations-a-frost-client-id)), a
provider's HTTP error, or a timeout. The engine tries a failed provider
again when its retry is due; `engine.log` has an `Obs <provider>` line per
fetch (`Grid metnordic` for the grid, `Lightning fmi` for lightning). A quiet
day has no lightning to draw; that is not an error.

Text too small or too large: the window and popover follow Omarchy's text
size, `[font] base-size` in `~/.config/omarchy/shell.toml`, which
`omarchy-display-text-size <px>` sets. A change applies to an open window.

## Remove

```sh
omarchy plugin remove omacheese.omastorm-nord
~/.local/share/omastorm-nord/bin/omastorm-engine stop
rm -rf ~/.local/share/omastorm-nord ~/.cache/omastorm-nord ~/.local/state/omastorm-nord
rm -rf ~/.config/omastorm-nord                            # your config.toml and frost.env; keep them to reinstall later
rm -f ~/.local/share/applications/omastorm-nord.desktop   # if you added the launcher entry
```

Then delete the `o.bind` line if you added one.

## Feedback

This is a beta. Bugs, rough edges, and ideas about the radars, the map, or
anything else this fork changed go to
[its own issues](https://github.com/OmaCheese/omastorm-nord/issues). Something that
upstream Omastorm has too belongs
[upstream](https://github.com/wesleygrimes/omastorm/issues), where the fix helps
both.

## Data and licenses

Every source is open data; each frame and layer names its credit in the
window and the popover, as the engine sends it.

| What | From | Licence | Credit shown |
| --- | --- | --- | --- |
| Swedish radars and SMHI's national composite | [SMHI](https://www.smhi.se/) open data | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) | SMHI |
| Norwegian, Finnish and Danish radars | MET Norway, FMI and DMI, through [EUMETNET Open Radar Data](https://www.eumetnet.eu/) | CC BY 4.0 | MET Norway, FMI, DMI |
| The Nordic composite | [EUMETNET OPERA](https://www.eumetnet.eu/activities/observations-programme/current-activities/opera/), through Open Radar Data | CC BY 4.0 | EUMETNET OPERA |
| Weather stations | SMHI (metobs), FMI (open WFS), DMI (metObs), MET Norway ([Frost](https://frost.met.no)) | CC BY 4.0 | each station's provider |
| Temperature and wind grid | MET Norway's MET Nordic analysis ([thredds.met.no](https://thredds.met.no)) | CC BY 4.0 | MET Norway |
| Lightning | FMI's open WFS, the NORDLIS network | CC BY 4.0 | FMI NORDLIS |
| Basemap | © [OpenStreetMap](https://www.openstreetmap.org/copyright) contributors, tiles by [OpenFreeMap](https://openfreemap.org) (© OpenMapTiles) | [ODbL](https://opendatacommons.org/licenses/odbl/1-0/) | OpenFreeMap © OpenMapTiles Data from OpenStreetMap |
| Coastlines, borders, lakes | [Natural Earth](https://www.naturalearthdata.com/) | public domain | |
| Place names and search | [GeoNames](https://www.geonames.org/) | CC BY 4.0 | |

Terrain (heights above ground and the beam blockage behind `Clear view`):
Mapzen [Terrain Tiles](https://registry.opendata.aws/terrain-tiles/) on AWS,
whose Nordic inputs include Kartverket (Norway,
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/)), the National Land Survey
of Finland ([CC BY 4.0](https://creativecommons.org/licenses/by/4.0/)), SDFE (Denmark)
and EU-DEM, produced using Copernicus data and information funded by the European
Union; details in [data/README.md](data/README.md).

**Use approximate location** asks [wttr.in](https://wttr.in) once, only when
you click it. Upstream Omastorm reads NOAA NEXRAD Level II via the NOAA Open
Data program on AWS; archive mode here still reads those files.

Code: MIT, see [LICENSE](LICENSE).

## Contributing

This is a beta. Contributions are welcome.
[CONTRIBUTING.md](CONTRIBUTING.md) is setup, checks, and pull requests.
Open work that is ready for a first patch is labeled
[`good first issue`](https://github.com/OmaCheese/omastorm-nord/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
and [`help wanted`](https://github.com/OmaCheese/omastorm-nord/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22).

A headless Rust engine serves GPU textures over a Unix socket. The
Quickshell UI is the client. `manifest.json` is the Omarchy plugin.

- `engine/` Rust daemon: the radar providers (SMHI, Open Radar Data, OPERA) and ODIM decode (NEXRAD Level II in archive mode), the weather stations, the MET Nordic grid, lightning, the cache, and the socket protocol
- `ui/` Quickshell QML for the bar popover and window
- `scripts/` bootstrap, checks, captures, fetch, and release
- `data/` fixture provenance, checksums, and vendored archives (`data/raw/` is extracted)
- `golden/` decoder answer keys: SMHI's Vara volume and national composite, and the Level II KTLX scan the checks use
- `docs/` protocol, configuration, and releasing
- `site/` upstream's omastorm.com page, unchanged; this fork publishes no site

Read [DESIGN.md](DESIGN.md) before proposing a product change and
[docs/protocol.md](docs/protocol.md) before touching the engine/client
boundary. Maintainers cut releases with
[docs/RELEASING.md](docs/RELEASING.md).

## Contributors

Thanks to these people
([emoji key](https://allcontributors.org/docs/en/emoji-key)):

<!-- ALL-CONTRIBUTORS-LIST:START - Do not remove or modify this section -->
<!-- prettier-ignore-start -->
<!-- markdownlint-disable -->
<table>
  <tbody>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://omastorm.com/"><img src="https://avatars.githubusercontent.com/u/324308?v=4?s=100" width="100px;" alt="Wes Grimes"/><br /><sub><b>Wes Grimes</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=wesleygrimes" title="Code">💻</a> <a href="https://github.com/wesleygrimes/omastorm/commits?author=wesleygrimes" title="Documentation">📖</a> <a href="#maintenance-wesleygrimes" title="Maintenance">🚧</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/scottjones"><img src="https://avatars.githubusercontent.com/u/444693?v=4?s=100" width="100px;" alt="Scott Jones"/><br /><sub><b>Scott Jones</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=scottjones" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/shieldsworks"><img src="https://avatars.githubusercontent.com/u/92273925?v=4?s=100" width="100px;" alt="Casey Shields"/><br /><sub><b>Casey Shields</b></sub></a><br /><a href="#infra-shieldsworks" title="Infrastructure (Hosting, Build-Tools, etc)">🚇</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/airtwo"><img src="https://avatars.githubusercontent.com/u/3505656?v=4?s=100" width="100px;" alt="Chance Griffin"/><br /><sub><b>Chance Griffin</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=airtwo" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/TheFifeDawg"><img src="https://avatars.githubusercontent.com/u/226545193?v=4?s=100" width="100px;" alt="Michael Pfeifer"/><br /><sub><b>Michael Pfeifer</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=TheFifeDawg" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/nixfred"><img src="https://avatars.githubusercontent.com/u/15384894?v=4?s=100" width="100px;" alt="Fred Nix"/><br /><sub><b>Fred Nix</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=nixfred" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/nmorton13"><img src="https://avatars.githubusercontent.com/u/16527730?v=4?s=100" width="100px;" alt="Nathan Morton"/><br /><sub><b>Nathan Morton</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=nmorton13" title="Code">💻</a></td>
    </tr>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="http://mrcobas.com"><img src="https://avatars.githubusercontent.com/u/41881817?v=4?s=100" width="100px;" alt="javi"/><br /><sub><b>javi</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=mrcobas" title="Tests">⚠️</a> <a href="https://github.com/wesleygrimes/omastorm/pulls?q=is%3Apr+reviewed-by%3Amrcobas" title="Reviewed Pull Requests">👀</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://ryrob.es/"><img src="https://avatars.githubusercontent.com/u/757387?v=4?s=100" width="100px;" alt="Ryan Robitaille"/><br /><sub><b>Ryan Robitaille</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=ryrobes" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/gw7523"><img src="https://avatars.githubusercontent.com/u/199144018?v=4?s=100" width="100px;" alt="gw7523"/><br /><sub><b>gw7523</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=gw7523" title="Code">💻</a> <a href="https://github.com/wesleygrimes/omastorm/issues?q=author%3Agw7523" title="Bug reports">🐛</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/Yani3rt"><img src="https://avatars.githubusercontent.com/u/170105839?v=4?s=100" width="100px;" alt="Yaniert Pascual"/><br /><sub><b>Yaniert Pascual</b></sub></a><br /><a href="#userTesting-Yani3rt" title="User Testing">📓</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://mikeyockey.com"><img src="https://avatars.githubusercontent.com/u/306343?v=4?s=100" width="100px;" alt="Michael Yockey"/><br /><sub><b>Michael Yockey</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/issues?q=author%3Ayock" title="Bug reports">🐛</a></td>
    </tr>
  </tbody>
</table>

<!-- markdownlint-restore -->
<!-- prettier-ignore-end -->

<!-- ALL-CONTRIBUTORS-LIST:END -->

This project follows the [all-contributors](https://allcontributors.org) specification.
