//! MET Norway's Frost API (`frost.met.no`, CC BY 4.0). Every request needs
//! a client ID, sent as HTTP basic auth with the ID as the user name and an
//! empty password (the client secret is for OAuth2 and not used). The ID
//! comes from `FROST_CLIENT_ID`, else from `FROST_CLIENT_ID=` in
//! `$XDG_CONFIG_HOME/omastorm-nord/frost.env` (default
//! `~/.config/omastorm-nord/frost.env`); with neither, Norway is skipped and
//! says so in `obs.providers`. The ID never goes into a URL, a cache file or
//! a log line.
//!
//! Frost has no "every station" query and refuses URLs over 2048
//! characters, so the list is the Norwegian WMO stations with air
//! temperature (about 250, from `sources`, fetched once a day), and their
//! latest observations in chunks of ids that keep each URL short (two
//! requests today), at most every 10 minutes.

use super::{Part, Station, finite};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

pub const ENV: &str = "FROST_CLIENT_ID";
const BASE: &str = "https://frost.met.no";
const SOURCES: &str = "frost-sources.json";
/// The elements, in the order `parse` fills temperature, wind speed, wind
/// direction, gust; `%20` is the space in Frost's gust element name.
const ELEMENTS: [&str; 4] = [
    "air_temperature",
    "wind_speed",
    "wind_from_direction",
    "max(wind_speed_of_gust%20PT10M)",
];
/// The ids per observations request: 124 ids of 7 to 8 characters keep the
/// URL near 1200 characters, well under Frost's 2048.
const CHUNK: usize = 124;

/// The client ID from the environment or the config file, or `None`.
pub fn client_id() -> Option<String> {
    if let Ok(id) = std::env::var(ENV)
        && !id.trim().is_empty()
    {
        return Some(id.trim().to_owned());
    }
    let dir = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(base) if Path::new(&base).is_absolute() => std::path::PathBuf::from(base),
        _ => std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    id_in(&std::fs::read_to_string(dir.join("omastorm-nord/frost.env")).ok()?)
}

/// `FROST_CLIENT_ID=…` in an env file (optionally quoted, `export` allowed).
fn id_in(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        let value = line.strip_prefix("FROST_CLIENT_ID=")?.trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'').trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// The station list and, once it is cached in `dir`, the observations in
/// chunks; or why Norway is skipped.
pub fn parts(dir: &Path) -> Result<Vec<Part>, String> {
    if client_id().is_none() {
        return Err(format!(
            "no Frost client ID ({ENV} or ~/.config/omastorm-nord/frost.env)"
        ));
    }
    let mut parts = vec![Part {
        name: SOURCES,
        url: format!(
            "{BASE}/sources/v0.jsonld?types=SensorSystem&country=NO&validtime=now\
             &elements=air_temperature&wmoid=*&fields=id,name,geometry"
        ),
        max_age: Duration::from_secs(24 * 3600),
        auth: true,
    }];
    let ids = std::fs::read(dir.join(SOURCES))
        .ok()
        .and_then(|body| sources(&body).ok())
        .map(|s| s.into_keys().collect::<Vec<_>>())
        .unwrap_or_default();
    for (i, chunk) in ids.chunks(CHUNK).enumerate().take(NAMES.len()) {
        parts.push(Part {
            name: NAMES[i],
            url: format!(
                "{BASE}/observations/v0.jsonld?sources={}&referencetime=latest&maxage=PT1H\
                 &levels=default&timeoffsets=default&elements={}",
                chunk.join(","),
                ELEMENTS.join(",")
            ),
            max_age: super::MIN_AGE,
            auth: true,
        });
    }
    Ok(parts)
}
/// Cache names for the observation chunks; four cover 496 stations.
const NAMES: [&str; 4] = [
    "frost-obs-0.json",
    "frost-obs-1.json",
    "frost-obs-2.json",
    "frost-obs-3.json",
];

#[derive(Deserialize)]
struct Response<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

#[derive(Deserialize)]
struct Source {
    id: String,
    #[serde(default)]
    name: String,
    geometry: Option<Geometry>,
}

#[derive(Deserialize)]
struct Geometry {
    coordinates: Vec<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    source_id: String,
    reference_time: String,
    #[serde(default)]
    observations: Vec<Observation>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    element_id: String,
    value: Option<f64>,
}

/// Whether a sources body reads and lists at least one placed station.
pub fn station_list_ok(body: &[u8]) -> bool {
    sources(body).is_ok_and(|s| !s.is_empty())
}

/// Station id → (name, lat, lon), ordered by id.
fn sources(body: &[u8]) -> Result<BTreeMap<String, (String, f64, f64)>, String> {
    let list: Response<Source> =
        serde_json::from_slice(body).map_err(|e| format!("sources: {e}"))?;
    Ok(list
        .data
        .into_iter()
        .filter_map(|s| {
            let [lon, lat, ..] = s.geometry?.coordinates[..] else {
                return None;
            };
            Some((s.id, (title(&s.name), lat, lon)))
        })
        .collect())
}

/// Frost's names are upper case ("OSLO - BLINDERN"): each word capitalised.
fn title(name: &str) -> String {
    name.split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first
                    .to_uppercase()
                    .chain(chars.flat_map(char::to_lowercase))
                    .collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The station list (`bodies[0]`) and the observation chunks as stations:
/// per element the newest value; `time` the newest of them.
pub fn parse(bodies: &[Vec<u8>]) -> Result<Vec<Station>, String> {
    let Some((list, chunks)) = bodies.split_first() else {
        return Err("nothing to read".into());
    };
    if chunks.is_empty() {
        return Err("no observations yet".into());
    }
    let places = sources(list)?;
    let mut newest: HashMap<String, [Option<(i64, f64)>; 4]> = HashMap::new();
    for body in chunks {
        let items: Response<Item> =
            serde_json::from_slice(body).map_err(|e| format!("observations: {e}"))?;
        for item in items.data {
            let id = item
                .source_id
                .split(':')
                .next()
                .unwrap_or_default()
                .to_owned();
            let Some(ms) = super::parse_iso(&item.reference_time) else {
                continue;
            };
            let slot = newest.entry(id).or_default();
            for o in item.observations {
                let i = match o.element_id.as_str() {
                    "air_temperature" => 0,
                    "wind_speed" => 1,
                    "wind_from_direction" => 2,
                    "max(wind_speed_of_gust PT10M)" => 3,
                    _ => continue,
                };
                let Some(value) = o.value.and_then(finite) else {
                    continue;
                };
                if slot[i].is_none_or(|(t, _)| ms >= t) {
                    slot[i] = Some((ms, value));
                }
            }
        }
    }
    Ok(places
        .into_iter()
        .filter_map(|(id, (name, lat, lon))| {
            let slot = newest.get(&id)?;
            let ms = slot.iter().flatten().map(|(t, _)| *t).max()?;
            let v = |i: usize| slot[i].map(|(_, v)| v);
            Some(Station {
                id: format!("frost:{id}"),
                name,
                provider: "frost",
                lat,
                lon,
                time: super::iso(ms),
                temp_c: v(0),
                wind_ms: v(1),
                wind_dir_deg: v(2),
                gust_ms: v(3),
                ms,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::{fresh, parse_iso, tests::fixture};

    #[test]
    fn the_env_file_names_the_id() {
        assert_eq!(
            id_in("FROST_CLIENT_ID=abc-123\n").as_deref(),
            Some("abc-123")
        );
        assert_eq!(
            id_in("# x\nexport FROST_CLIENT_ID=\"q w\"\nFROST_CLIENT_SECRET=s").as_deref(),
            Some("q w")
        );
        assert_eq!(id_in("FROST_CLIENT_SECRET=s\nFROST_CLIENT_ID=\n"), None);
        assert_eq!(id_in(""), None);
    }

    #[test]
    fn sources_and_one_chunk_read_as_stations() {
        let list = fixture("frost_sources_20260922.json.gz");
        let places = sources(&list).unwrap();
        assert_eq!(places.len(), 248);
        assert_eq!(places["SN18700"].0, "Oslo - Blindern");
        let stations = parse(&[list, fixture("frost_obs_202609221920.json.gz")]).unwrap();
        // The recorded chunk is the first 124 ids; 120 of them answered.
        assert_eq!(stations.len(), 120);
        let temps = stations.iter().filter(|s| s.temp_c.is_some()).count();
        let winds = stations
            .iter()
            .filter(|s| s.wind_ms.is_some() && s.wind_dir_deg.is_some())
            .count();
        let gusts = stations.iter().filter(|s| s.gust_ms.is_some()).count();
        assert!(
            temps >= 110 && winds >= 90 && gusts >= 60,
            "{temps} {winds} {gusts}"
        );
        assert!(
            stations
                .iter()
                .all(|s| s.provider == "frost" && s.id.starts_with("frost:SN"))
        );
        let blindern = stations.iter().find(|s| s.id == "frost:SN18700").unwrap();
        assert!((blindern.lat - 59.9423).abs() < 1e-9 && (blindern.lon - 10.72).abs() < 1e-9);
        assert!(blindern.temp_c.is_some());
        for s in &stations {
            if let Some(d) = s.wind_dir_deg {
                assert!((0.0..=360.0).contains(&d), "{} {d}", s.id);
            }
        }
        // SN64700 reported 20:00Z at 19:21Z: an observation from the
        // future is left out rather than drawn.
        let now = parse_iso("2026-09-22T19:30:00Z").unwrap();
        let kept = fresh(stations.clone(), now);
        assert_eq!(kept.len(), stations.len() - 1);
        assert!(!kept.iter().any(|s| s.id == "frost:SN64700"));
    }

    #[test]
    fn chunks_keep_every_url_short() {
        let list = fixture("frost_sources_20260922.json.gz");
        let ids: Vec<String> = sources(&list).unwrap().into_keys().collect();
        for chunk in ids.chunks(CHUNK) {
            let url = format!(
                "{BASE}/observations/v0.jsonld?sources={}&referencetime=latest&maxage=PT1H\
                 &levels=default&timeoffsets=default&elements={}",
                chunk.join(","),
                ELEMENTS.join(",")
            );
            assert!(url.len() < 1600, "{}", url.len());
        }
        assert!(ids.len() <= CHUNK * NAMES.len());
    }
}
