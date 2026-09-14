//! Providers: where a station's data comes from (S14, plan §S14, DEC-12).
//!
//! Every station in the table names a `provider`. A provider owns
//! everything source-specific about its stations:
//!
//! - its listing and polling (`poll`), which reports `Event`s carrying the
//!   station's id, however the source names the station (`Station::source`);
//! - its cadence, and so its staleness thresholds (`Staleness`, DEC-9
//!   generalized);
//! - its backfill depth and its range-read budget (`RangePlan`, DEC-2);
//! - the defaults of its rows in `engine/data/sites.json`: `attribution`,
//!   `country` and `rangeKm`.
//!
//! SMHI is the only provider so far (`smhi.rs`, wrapping `smhi_live.rs`).
//! S15 adds `ord` and `fmi-s3`, S16 `opera`, each the same way: a variant of
//! `ProviderId`, a module with a `Spec` and a `poll`, one arm in `spec` and
//! one in `poll`, and rows in `sites.json`. Nothing in `main.rs` changes.

pub mod opera;
pub mod smhi;

use crate::protocol::{SiteKind, Station};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc::Sender;

// What every provider builds on. They live in `smhi_live.rs` for now, where
// they were written; importing them from here keeps a later move invisible.
pub use crate::smhi_live::{Event, RangePlan, Scan};

/// A station's provider, as `sites.json` and `hello.sites[].provider` write
/// it.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderId {
    /// SMHI's open radar API: 12 radars and the Sweden composite.
    #[default]
    Smhi,
    /// EUMETNET OPERA's European composite, cut to the Nordic box (S16,
    /// DEC-14).
    Opera,
}

/// When a reachable feed is `stale`, and when `unavailable`, by the age of
/// the newest scan the station has published.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Staleness {
    pub stale: Duration,
    pub unavailable: Duration,
}

impl Staleness {
    /// DEC-9 generalized: stale at the cadence plus 10 minutes (a healthy
    /// frame is already up to one cadence plus publishing delay old), and
    /// unavailable at 30 minutes, or one more cadence past stale if that is
    /// later. SMHI's 5 minutes give DEC-9's 15 and 30.
    pub const fn from_cadence(cadence: Duration) -> Self {
        let stale = cadence.as_secs() + 10 * 60;
        let later = stale + cadence.as_secs();
        let unavailable = if later > 30 * 60 { later } else { 30 * 60 };
        Staleness {
            stale: Duration::from_secs(stale),
            unavailable: Duration::from_secs(unavailable),
        }
    }
}

/// What a provider is: its cadence, budgets and row defaults.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "the interface S15/S16 providers read; SMHI's poller keeps the equal constants it was written with (tests::smhis_spec_is_its_pollers)"
)]
pub struct Spec {
    pub id: ProviderId,
    /// For logs and provenance.
    pub name: &'static str,
    /// The credit a row gets when it names none (`hello.sites[].attribution`).
    pub attribution: &'static str,
    /// The country a row gets when it names none; empty for none.
    pub country: &'static str,
    /// The `rangeKm` a polar row gets when it names none.
    pub range_km: f64,
    /// How often a station publishes a scan.
    pub cadence: Duration,
    pub staleness: Staleness,
    /// Frames fetched after a join, counting the live one.
    pub backfill: usize,
    /// The range reader's budget for one file.
    pub ranges: RangePlan,
}

/// The spec of a provider.
pub fn spec(id: ProviderId) -> &'static Spec {
    match id {
        ProviderId::Smhi => &smhi::SPEC,
        ProviderId::Opera => &opera::SPEC,
    }
}

/// Poll `station` until the task is aborted or the event channel closes.
/// `cached` holds the start times already catalogued for it; `skip_known`
/// is set on a respawn.
pub async fn poll(station: Station, events: Sender<Event>, cached: Vec<i64>, skip_known: bool) {
    match station.provider {
        ProviderId::Smhi => smhi::poll(station, events, cached, skip_known).await,
        ProviderId::Opera => opera::poll(station, events, cached, skip_known).await,
    }
}

/// The station `id` names: its id exactly, else one of its aliases exactly,
/// else either ignoring ASCII case (`docs/protocol.md`, `select_site`).
pub fn resolve<'a>(sites: &'a [Station], id: &str) -> Option<&'a Station> {
    if id.is_empty() {
        return None;
    }
    sites
        .iter()
        .find(|s| s.id == id)
        .or_else(|| sites.iter().find(|s| s.aliases.iter().any(|a| a == id)))
        .or_else(|| {
            sites.iter().find(|s| {
                s.id.eq_ignore_ascii_case(id)
                    || s.aliases.iter().any(|a| a.eq_ignore_ascii_case(id))
            })
        })
}

// ---------------------------------------------------------------------------
// The station table
// ---------------------------------------------------------------------------

/// `engine/data/sites.json`: provenance, then one row per station. Written
/// for Sweden by `scripts/fetch-smhi-sites.sh`.
#[derive(Deserialize)]
struct TableFile {
    source: String,
    retrieved: String,
    notes: String,
    sites: Vec<Row>,
}

/// One row of `sites.json`. Only `id`, `name`, `lat`, `lon` and `altM` are
/// required; the rest default from the provider (`Spec`). A row may carry
/// more (SMHI's `rad`, `wmo`, `newest`), which is provenance and not read.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    id: String,
    name: String,
    #[serde(default)]
    state: String,
    lat: f64,
    lon: f64,
    alt_m: f64,
    #[serde(default)]
    kind: SiteKind,
    #[serde(default)]
    provider: ProviderId,
    /// ISO 3166-1 alpha-2; the provider's country when absent.
    country: Option<String>,
    /// The provider's range when absent; 0 for a grid station.
    range_km: Option<f64>,
    /// The provider's credit when absent.
    attribution: Option<String>,
    /// Other ids `select_site` accepts.
    #[serde(default)]
    aliases: Vec<String>,
    /// The ODIM node code (`/what source` `NOD:`). It becomes an alias when
    /// it is not the id (DEC-12: `sevax` for `vara`).
    #[serde(default)]
    nod: String,
    /// The station's key at its provider (an ORD location id, an FMI site
    /// code), when it is not the id. SMHI's is always the id (DEC-12).
    #[serde(default)]
    source_id: String,
}

impl Row {
    fn station(self) -> Station {
        let spec = spec(self.provider);
        let mut aliases = self.aliases;
        let nod = self.nod.trim().to_lowercase();
        if !nod.is_empty() {
            aliases.push(nod);
        }
        let mut seen = Vec::new();
        aliases.retain(|a| {
            let keep = !a.is_empty() && *a != self.id && !seen.contains(a);
            seen.push(a.clone());
            keep
        });
        let range_km = match self.kind {
            SiteKind::Grid => 0.0,
            SiteKind::Polar => self.range_km.unwrap_or(spec.range_km),
        };
        Station {
            country: self.country.unwrap_or_else(|| spec.country.to_owned()),
            provider: self.provider,
            range_km,
            attribution: self
                .attribution
                .unwrap_or_else(|| spec.attribution.to_owned()),
            aliases,
            source: self.source_id,
            id: self.id,
            name: self.name,
            state: self.state,
            lat: self.lat,
            lon: self.lon,
            alt_m: self.alt_m,
            kind: self.kind,
        }
    }
}

/// The station table `hello` lists: `sites.json`'s rows with their
/// provider's defaults filled in, then the composites.
pub struct Table {
    pub source: String,
    pub retrieved: String,
    pub notes: String,
    pub sites: Vec<Station>,
}

pub fn table() -> Table {
    let file: TableFile = serde_json::from_str(include_str!("../../data/sites.json"))
        .expect("engine/data/sites.json is valid");
    let mut sites: Vec<Station> = file.sites.into_iter().map(Row::station).collect();
    sites.push(crate::composite::station());
    sites.push(opera::station());
    Table {
        source: file.source,
        retrieved: file.retrieved,
        notes: file.notes,
        sites,
    }
}

/// Everything wrong with a table: ids or aliases that collide (ignoring
/// case), empty ids, unknown countries, a polar station without a range, or
/// an SMHI station whose key differs from its id. Empty when it is sound.
/// The embedded table is checked by the tests, so no build ships a clash.
#[cfg(test)]
pub fn problems(sites: &[Station]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut names: Vec<(String, &str)> = Vec::new();
    for s in sites {
        if s.id.is_empty() || s.id != s.id.trim() {
            problems.push(format!("station {:?}: empty or padded id", s.id));
        }
        for name in std::iter::once(&s.id).chain(&s.aliases) {
            let folded = name.to_ascii_lowercase();
            if let Some((_, owner)) = names.iter().find(|(n, _)| *n == folded) {
                problems.push(format!("{name:?} of {} is already {owner}'s", s.id));
            }
            names.push((folded, &s.id));
        }
        if !(s.country.is_empty()
            || s.country.len() == 2 && s.country.bytes().all(|b| b.is_ascii_uppercase()))
        {
            problems.push(format!(
                "{}: country {:?} is not ISO alpha-2",
                s.id, s.country
            ));
        }
        if s.kind == SiteKind::Polar && (s.range_km.is_nan() || s.range_km <= 0.0) {
            problems.push(format!("{}: a radar needs rangeKm", s.id));
        }
        if s.attribution.trim().is_empty() {
            problems.push(format!("{}: no attribution", s.id));
        }
        if s.provider == ProviderId::Smhi && !(s.source.is_empty() || s.source == s.id) {
            problems.push(format!(
                "{}: an SMHI station's id is its area key (DEC-12)",
                s.id
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smhi_keeps_dec9s_fifteen_and_thirty_minutes() {
        let smhi = spec(ProviderId::Smhi).staleness;
        assert_eq!(smhi.stale, Duration::from_secs(15 * 60));
        assert_eq!(smhi.unavailable, Duration::from_secs(30 * 60));
        assert_eq!(smhi, Staleness::from_cadence(Duration::from_secs(300)));
    }

    #[test]
    fn smhis_spec_is_its_pollers() {
        let smhi = spec(ProviderId::Smhi);
        assert_eq!((smhi.id, smhi.name), (ProviderId::Smhi, "SMHI"));
        assert_eq!(smhi.cadence, Duration::from_secs(300));
        assert_eq!(smhi.backfill, crate::smhi_live::BACKFILL);
        assert_eq!(smhi.backfill, crate::catalog::RING);
        assert_eq!(smhi.ranges, crate::smhi_live::DEC2);
        assert_eq!(
            (smhi.ranges.prefetch, smhi.ranges.block),
            (64 * 1024, 4 * 1024)
        );
    }

    #[test]
    fn staleness_follows_each_cadence() {
        let minutes = |m: u64| Duration::from_secs(m * 60);
        // (cadence, stale, unavailable)
        for (cadence, stale, unavailable) in
            [(5, 15, 30), (10, 20, 30), (15, 25, 40), (60, 70, 130)]
        {
            let s = Staleness::from_cadence(minutes(cadence));
            assert_eq!(s.stale, minutes(stale), "cadence {cadence}");
            assert_eq!(s.unavailable, minutes(unavailable), "cadence {cadence}");
            assert!(s.unavailable > s.stale);
        }
    }

    #[test]
    fn the_table_is_smhis_twelve_radars_and_its_composite() {
        let sites = table().sites;
        // SMHI's 13, then OPERA's Nordic composite (S16).
        assert_eq!(sites.len(), 14);
        assert!(problems(&sites).is_empty(), "{:?}", problems(&sites));
        let nordic = sites.iter().find(|s| s.id == "nordic").unwrap();
        assert_eq!(
            (nordic.kind, nordic.provider, nordic.attribution.as_str()),
            (
                SiteKind::Grid,
                ProviderId::Opera,
                "EUMETNET OPERA, CC BY 4.0"
            )
        );
        assert_eq!(
            serde_json::to_string(&ProviderId::Opera).unwrap(),
            "\"opera\""
        );
        for s in sites.iter().filter(|s| s.provider == ProviderId::Smhi) {
            assert_eq!(s.provider, ProviderId::Smhi, "{}", s.id);
            assert_eq!(s.country, "SE", "{}", s.id);
            assert_eq!(s.attribution, "SMHI, CC BY 4.0", "{}", s.id);
            match s.kind {
                SiteKind::Polar => {
                    assert_eq!(s.range_km, 240.0, "{}", s.id);
                    // Each radar's node code is its one alias.
                    assert_eq!(s.aliases.len(), 1, "{}", s.id);
                    assert!(s.aliases[0].starts_with("se") && s.aliases[0].len() == 5);
                }
                SiteKind::Grid => {
                    assert_eq!((s.id.as_str(), s.range_km), ("sweden", 0.0));
                    assert!(s.aliases.is_empty());
                }
            }
        }
        let vara = sites.iter().find(|s| s.id == "vara").unwrap();
        assert_eq!(vara.aliases, ["sevax"]);
    }

    #[test]
    fn select_site_names_resolve_to_the_canonical_id() {
        let sites = table().sites;
        let id = |name: &str| resolve(&sites, name).map(|s| s.id.as_str());
        // The id as it always was (a `locked_radar = "vara"` config).
        assert_eq!(id("vara"), Some("vara"));
        assert_eq!(id("balsta"), Some("balsta"));
        assert_eq!(id("sweden"), Some("sweden"));
        assert_eq!(id("nordic"), Some("nordic"));
        assert_eq!(id("Nordic"), Some("nordic"));
        // The ODIM node code, and either in another case.
        assert_eq!(id("sevax"), Some("vara"));
        assert_eq!(id("sebaa"), Some("balsta"));
        assert_eq!(id("Vara"), Some("vara"));
        assert_eq!(id("SEVAX"), Some("vara"));
        assert_eq!(id("sekrn"), Some("kiruna"));
        // Nothing else.
        assert_eq!(id(""), None);
        assert_eq!(id("KTLX"), None);
        assert_eq!(id("vara "), None);
        assert_eq!(id("seva"), None);
    }

    fn row(json: &str) -> Station {
        serde_json::from_str::<Row>(json).unwrap().station()
    }

    #[test]
    fn a_row_takes_its_providers_defaults_and_may_override_them() {
        let bare = row(r#"{"id":"x","name":"X","lat":1,"lon":2,"altM":3}"#);
        assert_eq!(bare.provider, ProviderId::Smhi);
        assert_eq!((bare.country.as_str(), bare.range_km), ("SE", 240.0));
        assert_eq!(bare.attribution, "SMHI, CC BY 4.0");
        assert!(bare.aliases.is_empty() && bare.source.is_empty());
        let full = row(
            r#"{"id":"y","name":"Y","lat":1,"lon":2,"altM":3,"country":"NO","rangeKm":250,
                "attribution":"MET Norway, CC BY 4.0","aliases":["old","y"],"nod":"NOHUR",
                "sourceId":"0-578-0-nohur","rad":"NO41"}"#,
        );
        assert_eq!((full.country.as_str(), full.range_km), ("NO", 250.0));
        assert_eq!(full.attribution, "MET Norway, CC BY 4.0");
        // The id itself is dropped from its aliases; the node code joins them.
        assert_eq!(full.aliases, ["old", "nohur"]);
        assert_eq!(full.source, "0-578-0-nohur");
        let grid =
            row(r#"{"id":"g","name":"G","lat":1,"lon":2,"altM":0,"kind":"grid","rangeKm":9}"#);
        assert_eq!(grid.range_km, 0.0);
    }

    #[test]
    fn colliding_ids_and_aliases_are_problems() {
        let mut sites = table().sites;
        assert!(problems(&sites).is_empty());
        // A new radar whose id is a Swedish radar's node code...
        sites.push(row(
            r#"{"id":"sevax","name":"Z","lat":1,"lon":2,"altM":3,"provider":"smhi"}"#,
        ));
        // ...one whose alias is an id in another case, and one with no range.
        sites.push(row(
            r#"{"id":"w","name":"W","lat":1,"lon":2,"altM":3,"aliases":["KIRUNA"],"rangeKm":0}"#,
        ));
        sites.push(row(r#"{"id":"v","name":"V","lat":1,"lon":2,"altM":3,"country":"Norway","sourceId":"elsewhere"}"#));
        let found = problems(&sites);
        assert_eq!(found.len(), 5, "{found:?}");
        assert!(
            found[0].contains("\"sevax\" of sevax is already vara's"),
            "{found:?}"
        );
        assert!(
            found[1].contains("\"KIRUNA\" of w is already kiruna's"),
            "{found:?}"
        );
        assert!(found[2].contains("w: a radar needs rangeKm"), "{found:?}");
        assert!(found[3].contains("not ISO alpha-2"), "{found:?}");
        assert!(found[4].contains("area key"), "{found:?}");
    }
}
