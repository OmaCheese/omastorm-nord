# Lightning fixtures (S44)

Gzipped response bodies, one request each. The parser tests in
`engine/src/lightning.rs` and `engine/tests/lightning.rs` read them offline;
`OMASTORM_LIGHTNING_REPLAY` replays any of them (docs/protocol.md,
"Lightning").

| File | Request |
|---|---|
| `fmi_lightning_20260923T0534Z.xml.gz` | `opendata.fmi.fi/wfs?service=WFS&version=2.0.0&request=getFeature&storedquery_id=fmi::observations::lightning::multipointcoverage&bbox=3,53,33,71.5&starttime=2026-09-22T05:34:00Z&endtime=2026-09-23T05:34:00Z` (a quiet September day: 194 rows, 190 distinct, 124 of them cloud-to-ground; the Baltic, Poland and the Russian border); FMI NORDLIS, CC BY 4.0 |
| `fmi_lightning_empty_20260923T0535Z.xml.gz` | the same query over 05:25–05:35Z: no strikes (`numberMatched="0"`, no member) |
| `smhi_lightning_20260705T11Z.json.gz` | `opendata-download-lightning.smhi.se/api/version/latest/year/2026/month/7/day/5/data.json`, cut to the objects with `hours` 11 (the day's busiest hour: 7,759 strikes, 1,473 cloud-to-ground, 56.9–68.5°N, 12.8–22.2°E), every field kept; SMHI, CC BY 4.0 |

The storm hour is the golden for captures and checks: replayed so its
newest strike falls at the engine's start, it shows a thunderstorm on a
quiet day.
