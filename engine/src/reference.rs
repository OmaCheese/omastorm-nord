//! Reference radars (S23): the other European weather radars, sent in
//! `hello.referenceSites` so a client can draw them as faint marks for
//! orientation. They are not stations: nothing selects them, the picker does
//! not list them, and `view_center` never hands off to them
//! (`docs/protocol.md`, `hello`). The list is a snapshot,
//! `engine/data/reference-radars.json`, written by
//! `scripts/fetch-reference-radars.sh`.

use serde::{Deserialize, Serialize};

/// One `hello.referenceSites[]` entry, exactly as the snapshot writes it.
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceSite {
    /// The ODIM node code, lowercase; never a station id or alias.
    pub id: String,
    pub name: String,
    /// ISO 3166-1 alpha-2.
    pub country: String,
    pub lat: f64,
    pub lon: f64,
    /// Where the position comes from, shown as the credit.
    pub source: String,
    pub retrieved: String,
}

#[derive(Deserialize)]
struct File {
    radars: Vec<ReferenceSite>,
}

/// The embedded snapshot's radars.
pub fn sites() -> Vec<ReferenceSite> {
    let file: File = serde_json::from_str(include_str!("../data/reference-radars.json"))
        .expect("engine/data/reference-radars.json is valid");
    file.radars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_snapshot_is_sound_and_holds_no_station() {
        let refs = sites();
        assert!(refs.len() > 100, "{} reference radars", refs.len());
        let table = crate::providers::table().sites;
        let mut taken: Vec<String> = table
            .iter()
            .flat_map(|s| std::iter::once(&s.id).chain(&s.aliases))
            .map(|id| id.to_ascii_lowercase())
            .collect();
        for r in &refs {
            assert!(
                r.id.len() == 5 && r.id.bytes().all(|b| b.is_ascii_lowercase()),
                "{:?} is not an ODIM node code",
                r.id
            );
            assert!(
                !taken.contains(&r.id),
                "{} is a station or a duplicate",
                r.id
            );
            taken.push(r.id.clone());
            assert!(!r.name.trim().is_empty(), "{} has no name", r.id);
            assert!(
                r.country.len() == 2 && r.country.bytes().all(|b| b.is_ascii_uppercase()),
                "{}: country {:?}",
                r.id,
                r.country
            );
            assert!((-90.0..=90.0).contains(&r.lat) && (-180.0..=180.0).contains(&r.lon));
            assert!(
                ["EUMETNET ORD", "EUMETNET OPERA database"].contains(&r.source.as_str()),
                "{}: source {:?}",
                r.id,
                r.source
            );
            assert!(!r.retrieved.is_empty());
        }
        // The composite-only countries come from the OPERA database.
        for cc in ["GB", "HU", "PT", "SI"] {
            assert!(refs.iter().any(|r| r.country == cc), "no {cc} radar");
        }
    }
}
