//! DMI's metObs (`opendataapi.dmi.dk/v2/metObs`, CC BY 4.0). Since DMI's
//! 2025 move off `dmigw.govcloud.dk` it needs no API key. One request
//! reads every parameter of the latest 10 minutes inside Denmark's box (a
//! GeoJSON feature per station and parameter); the station names come from
//! the station collection, fetched at most once a day.

use super::{Part, Station, finite};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

const BASE: &str = "https://opendataapi.dmi.dk/v2/metObs/collections";
/// Denmark with Bornholm; Greenland and the Faroes are outside.
const BBOX: &str = "7.5,54.4,15.5,58.0";

pub fn parts() -> Vec<Part> {
    vec![
        Part {
            name: "dmi-obs.json",
            url: format!(
                "{BASE}/observation/items?period=latest-10-minutes&bbox={BBOX}&limit=10000"
            ),
            max_age: super::MIN_AGE,
        },
        Part {
            name: "dmi-stations.json",
            url: format!("{BASE}/station/items?bbox={BBOX}&limit=10000"),
            max_age: Duration::from_secs(24 * 3600),
        },
    ]
}

#[derive(Deserialize)]
struct Collection<P> {
    #[serde(default = "Vec::new")]
    features: Vec<Feature<P>>,
}

#[derive(Deserialize)]
struct Feature<P> {
    geometry: Option<Geometry>,
    properties: P,
}

#[derive(Deserialize)]
struct Geometry {
    coordinates: Vec<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    station_id: String,
    parameter_id: String,
    value: Option<f64>,
    observed: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StationRow {
    station_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    valid_to: Option<String>,
}

/// The station collection keeps a row per validity period; the current one
/// has no `validTo`.
fn names(body: &[u8]) -> Result<HashMap<String, String>, String> {
    let rows: Collection<StationRow> =
        serde_json::from_slice(body).map_err(|e| format!("stations: {e}"))?;
    let mut names = HashMap::new();
    for f in rows.features {
        let p = f.properties;
        let Some(name) = p.name.filter(|n| !n.trim().is_empty()) else {
            continue;
        };
        if p.valid_to.is_none() || !names.contains_key(&p.station_id) {
            names.insert(p.station_id, name.trim().to_owned());
        }
    }
    Ok(names)
}

pub fn parse(observations: &[u8], stations: &[u8]) -> Result<Vec<Station>, String> {
    let names = names(stations)?;
    let rows: Collection<Observation> =
        serde_json::from_slice(observations).map_err(|e| format!("observations: {e}"))?;
    let mut out: BTreeMap<String, Station> = BTreeMap::new();
    // Per station and parameter, the time of the value kept.
    let mut times: HashMap<(String, usize), i64> = HashMap::new();
    for f in rows.features {
        let p = f.properties;
        let slot = match p.parameter_id.as_str() {
            "temp_dry" => 0,
            "wind_speed" => 1,
            "wind_dir" => 2,
            // The highest 3-second mean in the last 10 minutes.
            "wind_max" => 3,
            _ => continue,
        };
        let (Some(value), Some(ms), Some(geometry)) = (
            p.value.and_then(finite),
            super::parse_iso(&p.observed),
            f.geometry,
        ) else {
            continue;
        };
        let [lon, lat, ..] = geometry.coordinates[..] else {
            continue;
        };
        let key = (p.station_id.clone(), slot);
        if times.get(&key).is_some_and(|t| *t > ms) {
            continue;
        }
        times.insert(key, ms);
        let station = out.entry(p.station_id.clone()).or_insert_with(|| Station {
            id: format!("dmi:{}", p.station_id),
            name: names
                .get(&p.station_id)
                .cloned()
                .unwrap_or_else(|| format!("DMI {}", p.station_id)),
            provider: "dmi",
            lat,
            lon,
            time: String::new(),
            temp_c: None,
            wind_ms: None,
            wind_dir_deg: None,
            gust_ms: None,
            ms: 0,
        });
        station.ms = station.ms.max(ms);
        *match slot {
            0 => &mut station.temp_c,
            1 => &mut station.wind_ms,
            2 => &mut station.wind_dir_deg,
            _ => &mut station.gust_ms,
        } = Some(value);
    }
    Ok(out
        .into_values()
        .map(|mut s| {
            s.time = super::iso(s.ms);
            s
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::tests::fixture;

    #[test]
    fn features_merge_per_station_with_names() {
        let stations = parse(
            &fixture("dmi_obs_202609221830.json.gz"),
            &fixture("dmi_stations_20260922.json.gz"),
        )
        .unwrap();
        let temps = stations.iter().filter(|s| s.temp_c.is_some()).count();
        let winds = stations
            .iter()
            .filter(|s| s.wind_ms.is_some() && s.wind_dir_deg.is_some())
            .count();
        let gusts = stations.iter().filter(|s| s.gust_ms.is_some()).count();
        assert_eq!((temps, winds, gusts), (58, 54, 54));
        let other = stations.iter().filter(|s| !s.has_value()).count();
        assert_eq!(other, 0);
        assert!(stations.iter().all(|s| s.time == "2026-09-22T18:30:00Z"));
        assert!(
            stations.iter().all(|s| !s.name.starts_with("DMI ")),
            "every station named"
        );
        for s in &stations {
            assert!((54.4..=58.0).contains(&s.lat) && (7.5..=15.5).contains(&s.lon));
        }
    }

    #[test]
    fn the_current_name_wins() {
        let body = br#"{"features":[
          {"properties":{"stationId":"1","name":"Old","validTo":"2020-01-01T00:00:00Z"}},
          {"properties":{"stationId":"1","name":"New","validTo":null}},
          {"properties":{"stationId":"1","name":"Older","validTo":"2010-01-01T00:00:00Z"}}]}"#;
        assert_eq!(names(body).unwrap()["1"], "New");
    }
}
