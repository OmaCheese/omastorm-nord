//! FMI's open WFS (`opendata.fmi.fi/wfs`, CC BY 4.0, no key): one
//! `fmi::observations::weather::multipointcoverage` request over Finland
//! and Åland for the last 40 minutes at 10-minute steps, parameters `t2m`,
//! `ws_10min`, `wd_10min`, `wg_10min`.
//!
//! The answer is GML: the stations (`gml:Point` with `gml:name` and
//! `gml:pos`), then `gmlcov:positions` (one `lat lon epoch` row per station
//! and step) and `gml:doubleOrNilReasonTupleList` (one row of values per
//! position row, `NaN` when missing), in the order `swe:field` names them.
//! The document's shape is fixed, so a small scanner reads it; there is no
//! XML library in the engine.

use super::{Part, Station, finite};
use std::collections::HashMap;

const BBOX: &str = "19,59.5,31.6,70.1,epsg::4326";
const PARAMETERS: [&str; 4] = ["t2m", "ws_10min", "wd_10min", "wg_10min"];

pub fn parts(now: i64) -> Vec<Part> {
    // 40 minutes back, on a whole minute: four steps, enough for a station
    // that missed the newest one.
    let start = now - 40 * 60 * 1000;
    let start = start - start.rem_euclid(60_000);
    let url = format!(
        "https://opendata.fmi.fi/wfs?service=WFS&version=2.0.0&request=getFeature\
         &storedquery_id=fmi::observations::weather::multipointcoverage\
         &bbox={BBOX}&parameters={}&starttime={}&timestep=10",
        PARAMETERS.join(","),
        super::iso(start)
    );
    vec![Part {
        name: "fmi.xml",
        url,
        max_age: super::MIN_AGE,
        auth: false,
    }]
}

/// The text of every `<tag ...>text</tag>` in `doc`, with the opening
/// tag's attributes.
fn elements<'a>(doc: &'a str, tag: &str) -> Vec<(&'a str, &'a str)> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = doc;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        // `<gml:pos>` must not match `<gml:positions>`.
        if !after.starts_with([' ', '>', '\n', '\t', '\r']) {
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        let attrs = &after[..gt];
        let body = &after[gt + 1..];
        let Some(end) = body.find(&close) else { break };
        out.push((attrs, &body[..end]));
        rest = &body[end + close.len()..];
    }
    out
}

fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let at = attrs.find(&key)? + key.len();
    let len = attrs[at..].find('"')?;
    Some(&attrs[at..at + len])
}

/// A parameter's newest value and its time (ms).
type Newest = Option<(i64, f64)>;

/// `lat lon` as the key rows are matched on, rounded to 1e-5°.
fn key(lat: f64, lon: f64) -> (i64, i64) {
    ((lat * 1e5).round() as i64, (lon * 1e5).round() as i64)
}

pub fn parse(body: &[u8]) -> Result<Vec<Station>, String> {
    let doc = std::str::from_utf8(body).map_err(|e| format!("not UTF-8: {e}"))?;
    if doc.contains("ExceptionReport") {
        let text = elements(doc, "ExceptionText")
            .first()
            .map_or("an exception", |(_, t)| t.trim());
        return Err(format!("WFS: {text}"));
    }
    // The columns, in the order the values are listed.
    let fields: Vec<&str> = {
        let mut out = Vec::new();
        let mut rest = doc;
        while let Some(at) = rest.find("<swe:field ") {
            let tail = &rest[at..];
            let end = tail.find('>').unwrap_or(tail.len());
            if let Some(name) = attr(&tail[..end], "name") {
                out.push(name);
            }
            rest = &tail[end..];
        }
        out
    };
    let column = |name: &str| fields.iter().position(|f| *f == name);
    let columns = PARAMETERS.map(column);
    if columns.iter().all(Option::is_none) {
        return Err("no known parameter in the answer".into());
    }
    // The stations by position.
    let mut by_pos: HashMap<(i64, i64), (String, String, f64, f64)> = HashMap::new();
    for (attrs, inner) in elements(doc, "gml:Point") {
        let Some(id) = attr(attrs, "gml:id").and_then(|id| id.strip_prefix("point-")) else {
            continue;
        };
        let name = elements(inner, "gml:name")
            .first()
            .map_or(String::new(), |(_, t)| t.trim().to_owned());
        let Some((_, pos)) = elements(inner, "gml:pos").into_iter().next() else {
            continue;
        };
        let mut it = pos.split_whitespace().filter_map(|t| t.parse::<f64>().ok());
        let (Some(lat), Some(lon)) = (it.next(), it.next()) else {
            continue;
        };
        by_pos.insert(key(lat, lon), (id.to_owned(), name, lat, lon));
    }
    let positions = elements(doc, "gmlcov:positions");
    let values = elements(doc, "gml:doubleOrNilReasonTupleList");
    let (Some((_, positions)), Some((_, values))) = (positions.first(), values.first()) else {
        return Err("no positions or values".into());
    };
    let rows = positions.lines().map(str::trim).filter(|l| !l.is_empty());
    let cells = values.lines().map(str::trim).filter(|l| !l.is_empty());
    // Per station: for each parameter the newest (epoch, value).
    let mut newest: HashMap<(i64, i64), [Newest; 4]> = HashMap::new();
    for (row, cell) in rows.zip(cells) {
        let mut p = row.split_whitespace();
        let (Some(lat), Some(lon), Some(epoch)) = (
            p.next().and_then(|t| t.parse::<f64>().ok()),
            p.next().and_then(|t| t.parse::<f64>().ok()),
            p.next().and_then(|t| t.parse::<i64>().ok()),
        ) else {
            continue;
        };
        let row_values: Vec<Option<f64>> = cell
            .split_whitespace()
            .map(|t| t.parse::<f64>().ok().and_then(finite))
            .collect();
        let slot = newest.entry(key(lat, lon)).or_default();
        for (i, col) in columns.iter().enumerate() {
            let Some(value) = col.and_then(|c| row_values.get(c).copied().flatten()) else {
                continue;
            };
            let ms = epoch * 1000;
            if slot[i].is_none_or(|(t, _)| ms >= t) {
                slot[i] = Some((ms, value));
            }
        }
    }
    let mut stations: Vec<Station> = newest
        .into_iter()
        .filter_map(|(pos, slot)| {
            let (id, name, lat, lon) = by_pos.get(&pos)?.clone();
            let ms = slot.iter().flatten().map(|(t, _)| *t).max()?;
            let v = |i: usize| slot[i].map(|(_, v)| v);
            Some(Station {
                id: format!("fmi:{id}"),
                name,
                provider: "fmi",
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
        .collect();
    stations.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(stations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::{fresh, parse_iso, tests::fixture};

    #[test]
    fn the_coverage_reads_as_stations() {
        let stations = parse(&fixture("fmi_202609221833.xml.gz")).unwrap();
        assert!((180..=189).contains(&stations.len()), "{}", stations.len());
        let porvoo = stations.iter().find(|s| s.id == "fmi:100683").unwrap();
        assert_eq!(porvoo.name, "Porvoo Kilpilahti satama");
        assert_eq!(porvoo.time, "2026-09-22T18:30:00Z");
        // The newest row: 11.3 3.5 7.0 5.0.
        assert_eq!(
            (
                porvoo.temp_c,
                porvoo.wind_ms,
                porvoo.wind_dir_deg,
                porvoo.gust_ms
            ),
            (Some(11.3), Some(3.5), Some(7.0), Some(5.0))
        );
        assert!((porvoo.lat - 60.30373).abs() < 1e-9);
        let temps = stations.iter().filter(|s| s.temp_c.is_some()).count();
        let winds = stations.iter().filter(|s| s.wind_ms.is_some()).count();
        assert!(temps > 150 && winds > 120, "{temps} {winds}");
        let now = parse_iso("2026-09-22T18:40:00Z").unwrap();
        assert_eq!(fresh(stations.clone(), now).len(), stations.len());
    }

    #[test]
    fn an_exception_report_is_an_error() {
        let body = br#"<ExceptionReport><Exception><ExceptionText>Invalid parameter</ExceptionText></Exception></ExceptionReport>"#;
        assert_eq!(parse(body).unwrap_err(), "WFS: Invalid parameter");
    }

    #[test]
    fn the_request_asks_for_forty_minutes() {
        let now = parse_iso("2026-09-22T18:33:27Z").unwrap();
        let parts = parts(now);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].url.contains("starttime=2026-09-22T17:53:00Z"));
        assert!(
            parts[0]
                .url
                .contains("parameters=t2m,ws_10min,wd_10min,wg_10min")
        );
    }
}
