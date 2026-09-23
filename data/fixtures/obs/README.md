# Weather station fixtures (S42)

Recorded 2026-09-22 around 18:33 UTC, one request each, gzipped. The
parser tests in `engine/src/obs/` read them offline.

| File | Request |
|---|---|
| `smhi_p{1,4,3,21}_202609221800.json.gz` | `opendata-download-metobs.smhi.se/api/version/1.0/parameter/<p>/station-set/all/period/latest-hour/data.json` (temperature, wind speed, wind direction, gust); SMHI, CC BY 4.0 |
| `fmi_202609221833.xml.gz` | `opendata.fmi.fi/wfs` `fmi::observations::weather::multipointcoverage`, bbox 19,59.5,31.6,70.1, `t2m,ws_10min,wd_10min,wg_10min`, starttime 17:23Z, timestep 10; FMI, CC BY 4.0 |
| `dmi_obs_202609221830.json.gz` | `opendataapi.dmi.dk/v2/metObs/collections/observation/items?period=latest-10-minutes&bbox=7.5,54.4,15.5,58.0&limit=10000`; DMI, CC BY 4.0 |
| `dmi_stations_20260922.json.gz` | `opendataapi.dmi.dk/v2/metObs/collections/station/items?bbox=7.5,54.4,15.5,58.0&limit=10000` |

| `frost_sources_20260922.json.gz` | `frost.met.no/sources/v0.jsonld?types=SensorSystem&country=NO&validtime=now&elements=air_temperature&wmoid=*&fields=id,name,geometry,masl,wmoid` (19:21 UTC); MET Norway, CC BY 4.0 |
| `frost_obs_202609221920.json.gz` | `frost.met.no/observations/v0.jsonld?sources=<the list's first 124 ids>&referencetime=latest&maxage=PT1H&levels=default&timeoffsets=default&elements=air_temperature,wind_speed,wind_from_direction,max(wind_speed_of_gust PT10M)` |

Frost needs a client ID (basic auth); these are response bodies only, and
were checked to hold neither the ID nor the secret.

## MET Nordic grid (S43)

Recorded 2026-09-23 05:03 UTC (the 04:00 UTC analysis), gzipped, one
request each to
`thredds.met.no/thredds/dodsC/metpplatest/met_analysis_1_0km_nordic_latest.nc.dods`;
MET Norway, CC BY 4.0. The tests in `engine/src/obs/grid.rs` read them
offline.

| File | Query |
|---|---|
| `metnordic_20260923T04Z.dods.gz` | `time,x[0:60:1795],y[0:60:2320]`, `air_temperature_2m[0][0:60:2320][0:60:1795]`, `wind_speed_10m` and `wind_direction_10m` `[0][0:240:2320][0:240:1795]` (a small stride of the engine's 3/24 subset: 30 × 39 temperature, 8 × 10 wind) |
| `metnordic_time_20260923T04Z.dods.gz` | `time` (the probe) |
