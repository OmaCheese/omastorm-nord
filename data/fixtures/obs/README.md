# Weather station fixtures (S42)

Recorded 2026-09-22 around 18:33 UTC, one request each, gzipped. The
parser tests in `engine/src/obs/` read them offline.

| File | Request |
|---|---|
| `smhi_p{1,4,3,21}_202609221800.json.gz` | `opendata-download-metobs.smhi.se/api/version/1.0/parameter/<p>/station-set/all/period/latest-hour/data.json` (temperature, wind speed, wind direction, gust); SMHI, CC BY 4.0 |
| `fmi_202609221833.xml.gz` | `opendata.fmi.fi/wfs` `fmi::observations::weather::multipointcoverage`, bbox 19,59.5,31.6,70.1, `t2m,ws_10min,wd_10min,wg_10min`, starttime 17:23Z, timestep 10; FMI, CC BY 4.0 |
| `dmi_obs_202609221830.json.gz` | `opendataapi.dmi.dk/v2/metObs/collections/observation/items?period=latest-10-minutes&bbox=7.5,54.4,15.5,58.0&limit=10000`; DMI, CC BY 4.0 |
| `dmi_stations_20260922.json.gz` | `opendataapi.dmi.dk/v2/metObs/collections/station/items?bbox=7.5,54.4,15.5,58.0&limit=10000` |

No Frost (MET Norway) fixture: its API needs a client ID.
