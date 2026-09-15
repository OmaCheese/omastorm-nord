//! Converts the Natural Earth GeoJSON that `scripts/extract-fixtures.sh`
//! extracts into the geography the binary embeds (DESIGN.md, basemap tiles,
//! shipped geography): one polyline blob holding the 1:50m world set and the
//! 1:10m set clipped to the radars' envelopes (Nordic, and since S32 Iberia
//! and the Canary Islands), and the populated places for low-zoom labels.
//! GeoNames cities with population ≥ 5000, clipped to the same envelopes and
//! the gazetteer's countries, become the location-picker gazetteer. Reruns
//! only when an input changes.
//!
//! Blob layout (`ne.bin`, read by `src/tiles.rs`): the magic `OMNE\x01`, then
//! for each of the two sets (1:50m, 1:10m) and each of its two layers
//! (boundaries: country and state lines; coast: coastlines and lake shores) a
//! varint polyline count, and per polyline a varint vertex count followed by
//! that many `(lon, lat)` pairs quantized to 1e-5° as zigzag varint deltas,
//! the first pair relative to the origin.

use serde_json::Value;
use std::{env, fs, path::Path, process::exit};

const RAW: &str = "../data/raw";
const THEMES: [(&str, usize); 4] = [
    ("admin_0_boundary_lines_land", 0),
    ("admin_1_states_provinces_lines", 0),
    ("coastline", 1),
    ("lakes", 1),
];
/// The boxes the radars reach with their 240–250 km range, as west, east,
/// south and north in degrees:
/// - the Nordic box, 3–33° E and 53–71.5° N, from Denmark and the Baltics
///   to North Cape;
/// - Iberia (S32), 11° W–5° E and 34–46° N: the Spanish peninsular radars'
///   reach (San Sebastián's to 45.7° N, Gelida's to 4.9° E, Alhaurín's to
///   34.4° N), the Balearics, Ceuta and Melilla, and Portugal's coast with
///   room for its radars (S33's composite);
/// - the Canary Islands (S32), 19.5–12.5° W and 25.5–31° N: Artenara's and
///   Buenavista del Norte's reach.
///
/// `scripts/extract-fixtures.sh --regenerate` pre-clips the vendored 1:10m
/// lines to the same boxes.
const ENVELOPES: [[f64; 4]; 3] = [
    [3.0, 33.0, 53.0, 71.5],
    [-11.0, 5.0, 34.0, 46.0],
    [-19.5, -12.5, 25.5, 31.0],
];
fn in_envelope(lon: f64, lat: f64) -> bool {
    ENVELOPES.iter().any(|&[west, east, south, north]| {
        (south..=north).contains(&lat) && (west..=east).contains(&lon)
    })
}
/// The gazetteer's countries (GeoNames codes): Sweden, Norway, Finland with
/// Åland, Denmark, and the Baltics; since S32 Spain, Portugal, Andorra and
/// Gibraltar. The boxes alone would also take in north Germany, Poland,
/// Belarus, north-west Russia, south-west France and the Maghreb.
const COUNTRIES: [&str; 12] = [
    "SE", "NO", "FI", "AX", "DK", "EE", "LV", "LT", "ES", "PT", "AD", "GI",
];
/// GeoNames primary names that are English exonyms, by country; prefer the
/// local spelling in the location picker. Alternatenames also hold archaic
/// forms (Hälsingborg, Döderhultsvik), so this stays an explicit list
/// (Spain and Portugal checked 2026-09-15: only Lisbon; bilingual names
/// such as "Donostia / San Sebastián" are GeoNames' own and stay).
const LOCAL_NAMES: &[(&str, &str, &str)] = &[
    ("SE", "Gothenburg", "Göteborg"),
    ("PT", "Lisbon", "Lisboa"),
];
/// Natural Earth names in the gazetteer's countries that are misspelled or
/// exonyms against GeoNames (Nordic checked 2026-09-14, Iberia 2026-09-15,
/// by nearest GeoNames place and name); the map's low-zoom labels use the
/// corrected spelling, as the tile labels do. Natural Earth's "Granada"
/// ends in an invisible left-to-right mark.
const NE_LOCAL_NAMES: &[(&str, &str)] = &[
    ("Vannersborg", "Vänersborg"),
    ("Liepaga", "Liepāja"),
    ("Panevežys", "Panevėžys"),
    ("Seville", "Sevilla"),
    ("La Coruña", "A Coruña"),
    ("Lisbon", "Lisboa"),
    ("Castello", "Castelló de la Plana"),
    ("Granada\u{200e}", "Granada"),
    ("Viana Do Castelo", "Viana do Castelo"),
    ("Andorra", "Andorra la Vella"),
];
const SCALE: f64 = 1e5;

fn main() {
    let out = env::var_os("OUT_DIR").expect("OUT_DIR");
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let raw = Path::new(&manifest).join(RAW);
    let mut inputs = vec![
        "places.geojson".to_owned(),
        "cities5000.txt".to_owned(),
        "admin1CodesASCII.txt".to_owned(),
    ];
    for scale in ["50m", "10m"] {
        for (theme, _) in THEMES {
            inputs.push(format!("ne_{scale}_{theme}.geojson"));
        }
    }
    for input in &inputs {
        let path = raw.join(input);
        println!("cargo:rerun-if-changed={}", path.display());
        if !path.is_file() {
            eprintln!(
                "\nMissing {}.\nThe engine embeds the Natural Earth geography and the GeoNames gazetteer; run `bash scripts/extract-fixtures.sh` once to extract and verify \
                 them (see data/README.md).\n",
                path.display()
            );
            exit(1);
        }
    }
    let mut blob = b"OMNE\x01".to_vec();
    for (scale, clip) in [("50m", false), ("10m", true)] {
        let mut layers: [Vec<Vec<(i32, i32)>>; 2] = [Vec::new(), Vec::new()];
        for (theme, layer) in THEMES {
            let text = fs::read_to_string(raw.join(format!("ne_{scale}_{theme}.geojson")))
                .expect("read Natural Earth file");
            let collection: Value = serde_json::from_str(&text).expect("parse GeoJSON");
            for feature in collection["features"].as_array().expect("features") {
                let Some(geometry) = feature["geometry"].as_object() else {
                    continue;
                };
                for line in lines(geometry) {
                    for piece in clipped(&line, clip) {
                        if piece.len() >= 2 {
                            layers[layer].push(piece);
                        }
                    }
                }
            }
        }
        for polylines in layers {
            varint(&mut blob, polylines.len() as u64);
            for polyline in polylines {
                varint(&mut blob, polyline.len() as u64);
                let (mut px, mut py) = (0i32, 0i32);
                for (x, y) in polyline {
                    zigzag(&mut blob, i64::from(x) - i64::from(px));
                    zigzag(&mut blob, i64::from(y) - i64::from(py));
                    (px, py) = (x, y);
                }
            }
        }
    }
    fs::write(Path::new(&out).join("ne.bin"), blob).expect("write ne.bin");

    // Populated places, worldwide: the tile's places become `tile_ready`
    // labels. Class and rank are a proposal for the overlay session: Natural
    // Earth's `scalerank` is the rank (lower is more important, as in
    // OpenMapTiles); `min_zoom` is the zoom the data authors first show the
    // place at; the class follows capital status and population.
    let text = fs::read_to_string(raw.join("places.geojson")).expect("read places");
    let collection: Value = serde_json::from_str(&text).expect("parse places");
    let mut places = Vec::new();
    for feature in collection["features"].as_array().expect("features") {
        let p = &feature["properties"];
        let (Some(name), Some(lat), Some(lon)) = (
            p["name"].as_str(),
            p["latitude"].as_f64(),
            p["longitude"].as_f64(),
        ) else {
            continue;
        };
        let name = NE_LOCAL_NAMES
            .iter()
            .find(|(ne, _)| *ne == name)
            .map_or(name, |(_, local)| *local);
        let feature_class = p["featurecla"].as_str().unwrap_or_default();
        let population = p["pop_max"].as_f64().unwrap_or_default();
        let class = if feature_class.starts_with("Admin-0 capital") {
            "capital"
        } else if population >= 100_000.0 {
            "city"
        } else if population >= 10_000.0 {
            "town"
        } else {
            "village"
        };
        let iso = p["iso_a2"].as_str().unwrap_or("");
        let country = if iso.is_empty() || iso == "-99" {
            ""
        } else {
            iso
        };
        places.push(serde_json::json!({
            "name": name,
            "lat": (lat * SCALE).round() / SCALE,
            "lon": (lon * SCALE).round() / SCALE,
            "class": class,
            "rank": p["scalerank"].as_u64().unwrap_or(10),
            "minZoom": p["min_zoom"].as_f64().unwrap_or(10.0),
            "region": p["adm1name"].as_str().unwrap_or(""),
            "country": country,
        }));
    }
    fs::write(
        Path::new(&out).join("places.json"),
        serde_json::to_vec(&places).expect("serialize places"),
    )
    .expect("write places.json");
    write_gazetteer(&raw, Path::new(&out));
}

/// GeoNames `cities5000` clipped to the Nordic envelope and countries, for
/// the location picker only. Map labels stay on Natural Earth (`places.json`).
fn write_gazetteer(raw: &Path, out: &Path) {
    let mut admin1 = std::collections::HashMap::new();
    for line in fs::read_to_string(raw.join("admin1CodesASCII.txt"))
        .expect("read admin1")
        .lines()
    {
        let mut cols = line.split('\t');
        let (Some(code), Some(name)) = (cols.next(), cols.next()) else {
            continue;
        };
        if !code.is_empty() && !name.is_empty() {
            admin1.insert(code.to_owned(), name.to_owned());
        }
    }
    let mut gazetteer = Vec::new();
    for line in fs::read_to_string(raw.join("cities5000.txt"))
        .expect("read cities5000")
        .lines()
    {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 15 {
            continue;
        }
        let (name, lat, lon, fclass, fcode, country, adm1, pop) = (
            cols[1], cols[4], cols[5], cols[6], cols[7], cols[8], cols[10], cols[14],
        );
        if fclass != "P" || name.is_empty() {
            continue;
        }
        let (Ok(lat), Ok(lon), Ok(pop)) =
            (lat.parse::<f64>(), lon.parse::<f64>(), pop.parse::<f64>())
        else {
            continue;
        };
        if !in_envelope(lon, lat) || !COUNTRIES.contains(&country) {
            continue;
        }
        let name = LOCAL_NAMES
            .iter()
            .find(|(cc, en, _)| *cc == country && *en == name)
            .map_or(name, |(_, _, local)| *local);
        // Search-only aliases: the GeoNames primary and ASCII names and the
        // Latin-script alternate names, so "Gothenburg" finds Göteborg and
        // "Helsingfors" Helsinki. Archaic forms come along too; they only
        // match below the place's own name. Codes (GOT, CPH) are dropped.
        let mut aliases: Vec<&str> = Vec::new();
        for alias in [cols[1], cols[2]].into_iter().chain(cols[3].split(',')) {
            let latin = !alias.is_empty()
                && alias.chars().all(|c| {
                    (c.is_alphabetic() && (c as u32) < 0x250) || matches!(c, ' ' | '-' | '.' | '\'')
                });
            let code = alias.len() <= 4 && alias.chars().all(|c| c.is_ascii_uppercase());
            if latin
                && !code
                && alias.to_lowercase() != name.to_lowercase()
                && !aliases.contains(&alias)
            {
                aliases.push(alias);
            }
        }
        let class = if fcode == "PPLC" {
            "capital"
        } else if pop >= 100_000.0 {
            "city"
        } else if pop >= 10_000.0 {
            "town"
        } else {
            "village"
        };
        let rank = if pop >= 5_000_000.0 {
            1
        } else if pop >= 1_000_000.0 {
            2
        } else if pop >= 500_000.0 {
            3
        } else if pop >= 100_000.0 {
            4
        } else if pop >= 50_000.0 {
            6
        } else {
            8
        };
        let region = if adm1.is_empty() {
            String::new()
        } else {
            admin1
                .get(&format!("{country}.{adm1}"))
                .cloned()
                .unwrap_or_default()
        };
        gazetteer.push(serde_json::json!({
            "name": name,
            "lat": (lat * SCALE).round() / SCALE,
            "lon": (lon * SCALE).round() / SCALE,
            "class": class,
            "rank": rank,
            "region": region,
            "country": country,
            "aliases": aliases,
        }));
    }
    // Vara hosts the golden SMHI radar but sits under GeoNames' 5000 cut.
    // Coords are the town (GeoNames 2664996), not the radar mast.
    gazetteer.push(serde_json::json!({
        "name": "Vara",
        "lat": 58.2627,
        "lon": 12.9541,
        "class": "village",
        "rank": 8,
        "region": "Västra Götaland",
        "country": "SE",
    }));
    fs::write(
        out.join("gazetteer.json"),
        serde_json::to_vec(&gazetteer).expect("serialize gazetteer"),
    )
    .expect("write gazetteer.json");
}

/// Every line in a geometry: line strings as they are, polygon rings as
/// closed lines (lakes are stroked shorelines, not fills).
fn lines(geometry: &serde_json::Map<String, Value>) -> Vec<Vec<(f64, f64)>> {
    let coordinates = &geometry["coordinates"];
    let positions = |value: &Value| -> Vec<(f64, f64)> {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|point| {
                let point = point.as_array()?;
                Some((point.first()?.as_f64()?, point.get(1)?.as_f64()?))
            })
            .collect()
    };
    let rings = |polygon: &Value| -> Vec<Vec<(f64, f64)>> {
        polygon
            .as_array()
            .into_iter()
            .flatten()
            .map(positions)
            .collect()
    };
    match geometry["type"].as_str().unwrap_or_default() {
        "LineString" => vec![positions(coordinates)],
        "MultiLineString" | "Polygon" => rings(coordinates),
        "MultiPolygon" => coordinates
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(rings)
            .collect(),
        other => panic!("unsupported geometry {other}"),
    }
}

/// Quantize a line, and when `clip` is set keep only its runs inside the
/// envelope, each extended by one vertex past the edge so the line leaves the
/// envelope instead of stopping short of it. Repeated vertices collapse.
fn clipped(line: &[(f64, f64)], clip: bool) -> Vec<Vec<(i32, i32)>> {
    let quantize =
        |(lon, lat): (f64, f64)| ((lon * SCALE).round() as i32, (lat * SCALE).round() as i32);
    let inside: Vec<bool> = line
        .iter()
        .map(|&(lon, lat)| !clip || in_envelope(lon, lat))
        .collect();
    let mut pieces = Vec::new();
    let mut current: Vec<(i32, i32)> = Vec::new();
    for (i, &point) in line.iter().enumerate() {
        let next_inside = inside.get(i + 1).copied().unwrap_or(false);
        let keep = inside[i] || (i > 0 && inside[i - 1]) || next_inside;
        if keep {
            let q = quantize(point);
            if current.last() != Some(&q) {
                current.push(q);
            }
        }
        if !inside[i] && !next_inside && !current.is_empty() {
            pieces.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
fn zigzag(out: &mut Vec<u8>, value: i64) {
    varint(out, ((value << 1) ^ (value >> 63)) as u64);
}
