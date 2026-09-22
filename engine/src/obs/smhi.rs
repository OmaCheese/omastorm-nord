//! SMHI's meteorological observations (`opendata-download-metobs.smhi.se`,
//! CC BY 4.0, no key). The API serves one parameter per file, so the list
//! is four bulk files of every station's latest hour: air temperature (1,
//! instantaneous, hourly), wind speed (4, 10-minute mean, hourly), wind
//! direction (3, 10-minute mean, hourly) and gust (21, hourly maximum).

use super::{Part, Station, finite};
use serde::Deserialize;
use std::collections::BTreeMap;

const BASE: &str = "https://opendata-download-metobs.smhi.se/api/version/1.0/parameter";
/// The parameters in the order `parse` reads them: temperature, wind speed,
/// wind direction, gust.
const PARAMETERS: [(u32, &str); 4] = [
    (1, "smhi-p1.json"),
    (4, "smhi-p4.json"),
    (3, "smhi-p3.json"),
    (21, "smhi-p21.json"),
];

pub fn parts() -> Vec<Part> {
    PARAMETERS
        .iter()
        .map(|(p, name)| Part {
            name,
            url: format!("{BASE}/{p}/station-set/all/period/latest-hour/data.json"),
            max_age: super::MIN_AGE,
        })
        .collect()
}

/// SMHI's stations report on the hour, so a copy fetched after half past
/// the current hour already holds this hour's reports: the next are due
/// after the next hour starts.
pub fn due(fetched: i64, now: i64) -> bool {
    const HOUR: i64 = 3_600_000;
    let hour = now - now.rem_euclid(HOUR);
    fetched < hour + HOUR / 2
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    station: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    key: String,
    name: String,
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    value: Option<Vec<Value>>,
}

#[derive(Deserialize)]
struct Value {
    date: i64,
    value: String,
    #[serde(default)]
    quality: String,
}

/// The four files (temperature, wind speed, wind direction, gust) as one
/// list: per station the newest value of each, `time` the newest of them.
pub fn parse(bodies: &[Vec<u8>]) -> Result<Vec<Station>, String> {
    let mut stations: BTreeMap<String, Station> = BTreeMap::new();
    for (index, body) in bodies.iter().enumerate().take(PARAMETERS.len()) {
        let file: File = serde_json::from_slice(body)
            .map_err(|e| format!("parameter {}: {e}", PARAMETERS[index].0))?;
        for entry in file.station {
            let newest = entry
                .value
                .unwrap_or_default()
                .into_iter()
                // G: checked; Y: suspect or aggregated. Anything else is
                // not a reading.
                .filter(|v| v.quality == "G" || v.quality == "Y")
                .filter_map(|v| Some((v.date, finite(v.value.trim().parse().ok()?)?)))
                .max_by_key(|(date, _)| *date);
            let Some((date, value)) = newest else {
                continue;
            };
            let station = stations
                .entry(entry.key.clone())
                .or_insert_with(|| Station {
                    id: format!("smhi:{}", entry.key),
                    name: entry.name.clone(),
                    provider: "smhi",
                    lat: entry.latitude,
                    lon: entry.longitude,
                    time: String::new(),
                    temp_c: None,
                    wind_ms: None,
                    wind_dir_deg: None,
                    gust_ms: None,
                    ms: 0,
                });
            station.ms = station.ms.max(date);
            let slot = match index {
                0 => &mut station.temp_c,
                1 => &mut station.wind_ms,
                2 => &mut station.wind_dir_deg,
                _ => &mut station.gust_ms,
            };
            *slot = Some(value);
        }
    }
    Ok(stations
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
    use crate::obs::{fresh, parse_iso, tests::fixture};

    fn bodies() -> Vec<Vec<u8>> {
        ["p1", "p4", "p3", "p21"]
            .iter()
            .map(|p| fixture(&format!("smhi_{p}_202609221800.json.gz")))
            .collect()
    }

    #[test]
    fn the_four_parameters_merge_per_station() {
        let stations = parse(&bodies()).unwrap();
        // 213 report temperature; one more reports only wind.
        assert_eq!(stations.len(), 214);
        let abisko = stations.iter().find(|s| s.id == "smhi:188790").unwrap();
        assert_eq!(abisko.name, "Abisko Aut");
        assert_eq!(abisko.provider, "smhi");
        assert_eq!(abisko.temp_c, Some(8.3));
        assert_eq!(abisko.time, "2026-09-22T18:00:00Z");
        assert!((abisko.lat - 68.3538).abs() < 1e-9 && (abisko.lon - 18.8164).abs() < 1e-9);
        let temps = stations.iter().filter(|s| s.temp_c.is_some()).count();
        let winds = stations
            .iter()
            .filter(|s| s.wind_ms.is_some() && s.wind_dir_deg.is_some())
            .count();
        let gusts = stations.iter().filter(|s| s.gust_ms.is_some()).count();
        assert_eq!(temps, 213);
        assert!((170..=183).contains(&winds), "{winds}");
        assert!((140..=157).contains(&gusts), "{gusts}");
        for s in &stations {
            if let Some(d) = s.wind_dir_deg {
                assert!((0.0..=360.0).contains(&d), "{} {d}", s.id);
            }
            if let Some(w) = s.wind_ms {
                assert!((0.0..=80.0).contains(&w), "{} {w}", s.id);
            }
        }
        // A station listed with no value is left out rather than sent empty.
        assert!(
            !stations
                .iter()
                .any(|s| s.id == "smhi:84340" && s.temp_c.is_some())
        );
    }

    #[test]
    fn stations_go_stale_after_ninety_minutes() {
        let stations = parse(&bodies()).unwrap();
        let n = stations.len();
        let at = |t: &str| fresh(stations.clone(), parse_iso(t).unwrap()).len();
        assert_eq!(at("2026-09-22T18:40:00Z"), n);
        assert_eq!(at("2026-09-22T19:29:00Z"), n);
        assert_eq!(at("2026-09-22T19:31:00Z"), 0);
    }

    #[test]
    fn a_copy_from_after_half_past_waits_for_the_next_hour() {
        let t = |s: &str| parse_iso(s).unwrap();
        assert!(due(t("2026-09-22T18:12:00Z"), t("2026-09-22T18:25:00Z")));
        assert!(due(t("2026-09-22T18:25:00Z"), t("2026-09-22T18:40:00Z")));
        assert!(!due(t("2026-09-22T18:31:00Z"), t("2026-09-22T18:50:00Z")));
        assert!(due(t("2026-09-22T18:31:00Z"), t("2026-09-22T19:01:00Z")));
        assert!(!due(t("2026-09-22T19:35:00Z"), t("2026-09-22T19:59:00Z")));
    }
}
