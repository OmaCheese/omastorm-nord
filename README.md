# Omastorm SE

> **Omastorm SE** is a fork of [Omastorm](https://github.com/wesleygrimes/omastorm)
> that shows Swedish weather radar from [SMHI](https://www.smhi.se/) instead of
> NOAA NEXRAD: the 12 SMHI radars and SMHI's national composite. It installs
> beside upstream under its own plugin id (`rb.omastorm-se`) and directories
> (`omastorm-se`).

Open-source, live weather radar for the Omarchy desktop. Beta.

[![Omastorm window: live take with loop, search, keys, and treatments](https://github.com/wesleygrimes/omastorm/releases/download/media-2026-09-10/omastorm-preview.gif)](https://github.com/wesleygrimes/omastorm/releases/download/media-2026-09-10/omastorm-demo.mp4)

Upstream Omastorm's demo, live over Jacksonville (NEXRAD). The fork's own
media, over Vara, is made by `scripts/capture-readme.sh` and
`scripts/capture-demo.sh` and replaces these links with its first release.

A radar that lives in your bar. The popover shows the station nearest you with
the actual scan time. Click the map (or press Enter) for the full window: every
SMHI radar and the national composite, reflectivity at native resolution, a
timeline you can scrub, all drawn in your Omarchy theme.

![The Omastorm window, live](https://github.com/wesleygrimes/omastorm/releases/download/media-2026-09-10/window-live.png)

![The Omastorm popover, live](https://github.com/wesleygrimes/omastorm/releases/download/media-2026-09-10/popover.png)

A headless Rust engine fetches and decodes SMHI's ODIM HDF5 volumes and
national composite and prepares GPU-ready radar textures. An Omarchy plugin built with Quickshell/QML is the
client: it displays those textures in the bar popover and full window.

## Features

- **Live.** A Rust engine polls SMHI's open data every minute. SMHI publishes
  one scan per radar every 5 minutes, about 5 minutes after it is taken; the
  scan appears whole. Stale data says it is stale.
- **Every radar.** Pan the map and it follows the nearest station, or search by
  station, town, or county. Pick Sweden for the national composite.
- **Timeline.** Up to 60 scans per station, cached locally. Play, step, scrub.
- **Three treatments.** Glyphs, Pixels, and Stipple sample the same gate and
  paint the cell differently.
- **Native.** Colors, font, and spacing come from the active Omarchy theme and
  change with it.
- **Keyboard first.** Everything the pointer reaches is a keystroke, and every
  key is rebindable.
- **Honest.** Actual scan times. Missing and below-threshold returns are drawn
  distinctly from measured values. Displays one radar's lowest sweep at a time,
  or SMHI's composite.

## Install

Omarchy 4 on x86_64 and aarch64.

The fork is not on GitHub yet, and has published no engine release. Until it
has, install from a checkout: `mise setup`, then `mise plugin-link` links the
plugin to it and the bar runs the checkout's own engine build. Once published:

```sh
omarchy plugin add https://github.com/<owner>/omastorm-se.git --enable
```

This clones the plugin into `~/.config/omarchy/plugins/rb.omastorm-se` and
asks which bar section to use. The first time the popover opens it downloads
the pinned engine binary from this repository's GitHub Releases, verifies its
sha256 against `engine/release.pin`, and installs it under
`~/.local/share/omastorm-se/bin`. Runtime files, cached data, remembered view state, and configuration stay
inside Omastorm's own directories.

On first use, Omastorm uses your Omarchy weather location when available.
Otherwise, choose a place manually or click **Use approximate location** to
estimate your city using your public IP via wttr.in. No IP-location request
is made before you click. To set
a fixed launch location, including during agent-assisted installation, see
[configuration and remembered state](docs/configuration.md).

To open the window from the keyboard, add one line to
`~/.config/hypr/bindings.lua`. Omastorm never writes that file.

```lua
o.bind("SUPER + SHIFT + R", "Omastorm SE", "omarchy shell shell toggle rb.omastorm-se '{}'")
```

To list Omastorm in the app launcher:

```sh
bash ~/.config/omarchy/plugins/rb.omastorm-se/scripts/write-desktop-entry.sh
```

Update with `omarchy plugin update rb.omastorm-se`.

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
| `Space` | Loop the frames |
| `[` `]` | Step a frame |
| `Home` `End` | Oldest or newest frame |
| `1` `2` `3` | Pixels, Glyphs, Stipple |
| `w` | Show weak returns |
| `?` | Keys sheet |
| `Esc` | Close |

Measured returns under 5 dBZ (insects, birds, ground clutter on a clear day)
are hidden by default and the legend says so; `w` shows them.

## Configuration

`~/.config/omastorm-se/config.toml` holds deliberate preferences. The app saves
last map center, zoom, and UI radar lock separately in
`$XDG_STATE_HOME/omastorm-se/state.json` (default
`~/.local/state/omastorm-se/state.json`). Navigation never rewrites your config.
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
`$XDG_RUNTIME_DIR/omastorm-se/engine.log` (usually `/run/user/<uid>/omastorm-se/`).
When SMHI's listing lags, the engine probes the next scan's dated file once a
scan is overdue, and it backs off on errors, keeping cached frames available.
Both are recorded in `engine.log`.

If the popover says the engine could not be installed, the download or its
sha256 check failed; the reason is in `bootstrap.log` in the same directory,
and opening the popover again retries. To restart the engine by hand:

```sh
~/.local/share/omastorm-se/bin/omastorm-engine stop
```

The next popover or window starts it again. Please attach both logs to a
[bug report](https://github.com/wesleygrimes/omastorm/issues).

## Remove

```sh
omarchy plugin remove rb.omastorm-se
~/.local/share/omastorm-se/bin/omastorm-engine stop
rm -rf ~/.local/share/omastorm-se ~/.cache/omastorm-se ~/.local/state/omastorm-se
rm -rf ~/.config/omastorm-se                            # your config.toml; keep it to reinstall later
rm -f ~/.local/share/applications/omastorm-se.desktop   # if you added the launcher entry
```

Then delete the `o.bind` line if you added one.

## Feedback

This is a beta. Bugs, rough edges, and ideas go to
[GitHub issues](https://github.com/wesleygrimes/omastorm/issues) when upstream
Omastorm has them too; anything SMHI-specific belongs to this fork's own
tracker once it is published.

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
