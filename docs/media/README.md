# README media

The pictures the README shows are not in the repository. The plugin is a full
clone of this repository, so media travels as assets on the plugin's GitHub
Release (`v26.9.0` for this fork's first), and the README links to them by
URL.

Regenerate from a working desktop OpenGL session:

```sh
bash scripts/capture-readme.sh   # window-live.png, popover.png (archived Vara volume, no SMHI polling)
bash scripts/capture-demo.sh     # omastorm-demo.mp4 and omastorm-preview.gif (live Vara; SITE=balsta for another)
```

Both write into this directory, which is ignored except for this file. They
use isolated daemons and change no desktop or system configuration. FFmpeg is
required; the demo also needs Ruby for its temporary harness. Frames are
grabbed as the scene settles, so the video runs a little faster than real
time and is not a latency measurement.

- `omastorm-demo.mp4`: one live take, about 37 s, 1280×720, H.264, no audio:
  the home view, the loop, a pan and zoom to Lake Vänern, the three
  treatments, weak returns, the picker switching station, the keys sheet.
- `omastorm-preview.gif`: the home view and the zoom, cut from the video.
- `window-live.png`, `popover.png`: Vara over Västra Götaland, from the vendored
  SMHI volume `data/raw/radar_vara_qcvol_202609131055.h5` (13 Sep 2026 12:55
  CEST), in the C.UTF-8 locale for km and a 24-hour clock. Archive mode keeps
  the pictures reproducible and SMHI unpolled; the badge reads ARCHIVED.
  `capture-demo.sh` films live Vara now; the video and GIF the README links
  are still upstream's live KJAX take until the fork's first release
  replaces them.

Upload only the generated media files with `gh release upload <tag> <files...>`
and point the README URLs at that tag. For immutable releases, upload media
while the release is still a draft; published assets cannot be replaced.

Radar: SMHI, CC BY 4.0. Map: © OpenStreetMap contributors
([ODbL](https://opendatacommons.org/licenses/odbl/1-0/)); Natural Earth, public
domain.
