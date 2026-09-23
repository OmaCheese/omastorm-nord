# Omastorm Nord

> **Omastorm Nord** is a fork of [Omastorm](https://github.com/wesleygrimes/omastorm)
> that shows Nordic weather radar instead of NOAA NEXRAD: 41 radars — the 12
> [SMHI](https://www.smhi.se/) radars in Sweden, 12 in Norway, 12 in Finland
> and 5 in Denmark — plus SMHI's national composite, EUMETNET OPERA's Nordic
> composite, and a mosaic of the radars you choose. It
> installs beside upstream under its own plugin id (`omacheese.omastorm-nord`) and
> directories (`omastorm-nord`).

Open-source, live weather radar for the Omarchy desktop. Beta.

[![Omastorm window: live take with loop, search, keys, and treatments](https://github.com/OmaCheese/omastorm-nord/releases/download/v26.9.0/omastorm-preview.gif)](https://github.com/OmaCheese/omastorm-nord/releases/download/v26.9.0/omastorm-demo.mp4)

Live over Vara, in Västra Götaland. The media is made by
`scripts/capture-readme.sh` and `scripts/capture-demo.sh`.

A radar that lives in your bar. The popover shows the station nearest you with
the actual scan time. Click the map (or press Enter) for the full window: every
SMHI radar and the national composite, reflectivity at native resolution, a
timeline you can scrub, all drawn in your Omarchy theme.

![The Omastorm window, live](https://github.com/OmaCheese/omastorm-nord/releases/download/v26.9.0/window-live.png)

![The Omastorm popover, live](https://github.com/OmaCheese/omastorm-nord/releases/download/v26.9.0/popover.png)

A headless Rust engine fetches and decodes SMHI's ODIM HDF5 volumes and
national composite and prepares GPU-ready radar textures. An Omarchy plugin built with Quickshell/QML is the
client: it displays those textures in the bar popover and full window.

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
- **Three treatments.** Glyphs, Pixels, and Stipple sample the same gate and
  paint the cell differently.
- **Native.** Colors, font, and spacing come from the active Omarchy theme and
  change with it, **light themes included**: the theme says whether it is light
  or dark, and the map's ground, lines and echo follow. Radar sites are drawn
  in their own color, so an antenna is never mistaken for a town.
- **Keyboard first.** Everything the pointer reaches is a keystroke, and every
  key is rebindable.
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
when SMHI publishes nothing for a radar, OFFLINE when SMHI cannot be reached,
with cached frames kept. A healthy SMHI frame is already 5 to 10 minutes old.

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
| `Space` | Loop the frames |
| `[` `]` | Step a frame |
| `Home` `End` | Oldest or newest frame |
| `1` `2` `3` | Pixels, Glyphs, Stipple |
| `w` | Show weak returns |
| `?` | Keys sheet |
| `Esc` | Close |

Measured returns under 5 dBZ (insects, birds, ground clutter on a clear day)
are hidden by default and the legend says so; `w` shows them.

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
| `My mosaic` | only the radars you pick | leaves out a radar whose distant, high coverage misleads. Rain mass over your own set too |
| A section, and a profile on long press | a vertical slice, and one column’s numbers | structure: a flat layer with a bright band at 1–3 km is steady rain melting; a narrow column to 8–12 km is convection; echo that stops short of the ground is rain evaporating on the way down |
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

## Remove

```sh
omarchy plugin remove omacheese.omastorm-nord
~/.local/share/omastorm-nord/bin/omastorm-engine stop
rm -rf ~/.local/share/omastorm-nord ~/.cache/omastorm-nord ~/.local/state/omastorm-nord
rm -rf ~/.config/omastorm-nord                            # your config.toml; keep it to reinstall later
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

Radar: [SMHI](https://www.smhi.se/) open data,
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Upstream Omastorm reads
NOAA NEXRAD Level II via the NOAA Open Data program on AWS. Basemap: ©
OpenStreetMap contributors, [ODbL](https://opendatacommons.org/licenses/odbl/1-0/),
tiles by [OpenFreeMap](https://openfreemap.org); Natural Earth, public domain.
Location search: [GeoNames](https://www.geonames.org/),
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
Terrain (heights above ground): Mapzen [Terrain Tiles](https://registry.opendata.aws/terrain-tiles/)
on AWS, whose Nordic inputs include Kartverket (Norway,
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/)), the National Land Survey
of Finland ([CC BY 4.0](https://creativecommons.org/licenses/by/4.0/)), SDFE (Denmark)
and EU-DEM, produced using Copernicus data and information funded by the European
Union; details in [data/README.md](data/README.md).
Code: MIT, see [LICENSE](LICENSE).

## Contributing

This is a beta. Contributions are welcome.
[CONTRIBUTING.md](CONTRIBUTING.md) is setup, checks, and pull requests.
Open work that is ready for a first patch is labeled
[`good first issue`](https://github.com/wesleygrimes/omastorm/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
and [`help wanted`](https://github.com/wesleygrimes/omastorm/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22).

A headless Rust engine serves GPU textures over a Unix socket. The
Quickshell UI is the client. `manifest.json` is the Omarchy plugin.

- `engine/` Rust daemon: SMHI polling and ODIM decode (NEXRAD Level II in archive mode), cache, and the socket protocol
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
