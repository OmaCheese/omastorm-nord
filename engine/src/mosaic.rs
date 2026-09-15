//! My mosaic (S25, plan §S25, `docs/protocol.md` "My mosaic"): a composite
//! the engine makes itself from the **lowest scan** of up to `MAX_SITES`
//! radars a client chooses, of any provider, so a radar can be left out of
//! it or trimmed. The providers' composites (`sweden`, `nordic`) have every
//! radar of their network baked in; this one has the ones the human ticked.
//!
//! The module has four parts, each meant to be extended by S30 (heights
//! across the chosen radars) and S24b (the vertical products on `sweden` and
//! `nordic`):
//!
//! - **The set** (`Set`, `choose`): radars, each one's reach and the
//!   combine `Rule`, as `set_mosaic` sends them and `state.mosaic` echoes
//!   them. `Set::variant` names the set in frame ids and catalog rings.
//! - **Placement** (`Layout`): the Web Mercator texture over the box of the
//!   chosen radars' reach circles, at the composites' `PIXEL_M`, and per
//!   radar a table over its own part of that box: each texel centre's
//!   great-circle ground distance (10 m units) and bearing (tenths of a
//!   degree), `NONE` past the radar's reach. Built once per set; nothing in
//!   it depends on a scan. A per-height table (S30) goes beside it.
//! - **Combining** (`combine`, `build`): per frame, each radar's scan turns
//!   into lookups (azimuth to ray, ground distance to gate and beam height,
//!   at the scan's own elevation and gates), and every texel takes, with no
//!   averaging, the candidate the rule picks. `Rule` is the place for a new
//!   rule (S30: nearest beam to a height).
//! - **Timing** (`Schedule`, `poll`): scans keyed by nominal time (the
//!   pollers' own rule, `nominal_ms`); a frame built when every radar's
//!   scan for T is in, or at T + 8 minutes after 30 s without an arrival
//!   (never with no scan at all), and once more for a late scan while it is
//!   one of the newest two; missing radars named in its provenance; radars
//!   polled only while `mymosaic` is selected (`main.rs` starts `poll`),
//!   through S27's tilt store first.
//!
//! At a height (S30): the `height` rule makes a slice at the set's height,
//! above sea level or above the ground (`terrain.rs`), from each radar's
//! **whole volume**: its pollers read every scan into the tilt store
//! (`Want::ColMax`), the frame reads them back from the store when it is
//! built (a volume is megabytes decoded; nothing but the lowest scan is
//! held in memory), and per texel each radar's scan is chosen by the same
//! height rule a radar's own `CAPPI` uses (`products::nearest_beam`), then
//! the radar whose beam centre is nearest the height, the nearer on a tie
//! (`combine_height`). A `height` set builds back `HEIGHT_BACKFILL` frame
//! times, not `BACKFILL`, as a volume costs 5–6 times a lowest scan.
//!
//! Relation to S29's reach slider: the same unit (ground km from the
//! antenna), range (25 km to full) and meaning, but S29 clips one radar on
//! a client's screen, while `reachKm` here is applied by the engine before
//! combining, so a trimmed radar's area falls to its neighbours.

use crate::composite::{
    Grid, LonLatBox, MERCATOR_R, OFFSET, PIXEL_M, SCALE, mercator_lat, mercator_y,
};
use crate::odim::Tilt as Which;
use crate::products::{Above, EARTH_M, Legend, Tilt, TiltInfo, Want, elevation_to, nearest_beam};
use crate::protocol::{Frame, FrameKind, FrameStatus, Geometry, SiteKind, Station};
use crate::providers::{Event, ProviderId, Scan, Spec, Staleness};
use crate::sweep::Sweep;
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::{self, Sender};
use tokio::task::spawn_blocking;
use tokio::time::timeout;

/// The station's id, `hello.mosaic.station`.
pub const STATION: &str = "mymosaic";
/// The most radars a set holds.
pub const MAX_SITES: usize = 12;
/// The shortest reach a radar may be given, km.
pub const MIN_REACH_KM: f64 = 25.0;
/// Frame times a selection builds back, counting the newest: an hour.
pub const BACKFILL: usize = 12;
/// A `height` set's with an SMHI radar (S30, `Layout::backfill`): an SMHI
/// whole volume is ~39 range requests, 5–6 times its lowest scan
/// (`docs/protocol.md`), so half an hour: twelve SMHI radars with an empty
/// tilt store then read 12 × 7 volumes, about 3,300 requests, instead of
/// 12 × 13, about 6,100. An FMI radar's volume is five files, one per
/// angle, so a set with one counts the same (S24a review #2); a set of
/// one-file ORD radars only (1 request each) builds back `BACKFILL`.
pub const HEIGHT_BACKFILL: usize = 6;
/// Range requests an SMHI whole volume costs, for the first-fill estimate.
const SMHI_VOLUME_REQUESTS: usize = 39;
/// ...and the scans of its clear beams (`LOWB`, `Want::Hybrid`), read like
/// any product but the lowest scan through `smhi_live::PRODUCT_PLAN`'s 16 KiB
/// blocks: about 25 range requests for one or two tilts (review S4; one
/// other angle alone is 25 there). A radar without a blockage table reads
/// its lowest scan, ~7; every SMHI radar has one.
const SMHI_CLEAR_REQUESTS: usize = 25;

/// The engine's environment variable that lowers how far a composite's
/// products build back (S24b, `grid_backfill`): 1 to `BACKFILL` frame times.
pub const GRID_BACKFILL_ENV: &str = "OMASTORM_GRID_BACKFILL";

/// Frame times a composite's product reading whole volumes builds back
/// (review S1): a quarter of an hour; its first fill is then 4 volumes a
/// radar, about 1,900 SMHI requests for `nordic`, in ~5 minutes.
pub const GRID_WHOLE_BACKFILL: usize = 3;
/// ...and `LOWB`, which reads only the scans of each radar's clear beams:
/// half an hour.
pub const GRID_LOWB_BACKFILL: usize = 6;

/// Frame times a composite's product builds back (S24b, `docs/protocol.md`,
/// the composites' products): `GRID_LOWB_BACKFILL` for `LOWB`,
/// `GRID_WHOLE_BACKFILL` for the others. `OMASTORM_GRID_BACKFILL` lowers
/// either (a cap, never a raise).
pub fn grid_backfill(want: Want) -> usize {
    let base = if want == Want::LowestBeam {
        GRID_LOWB_BACKFILL
    } else {
        GRID_WHOLE_BACKFILL
    };
    let cap = std::env::var(GRID_BACKFILL_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| (1..=BACKFILL).contains(n));
    cap.map_or(base, |n| base.min(n))
}

/// SMHI's composite's box, the lon/lat extent of its stereographic grid
/// (`golden/sweden-20260913`): `sweden`'s products are drawn over it.
pub const SWEDEN_BOX: LonLatBox = LonLatBox {
    west: 5.323_958_486,
    east: 29.829_999_437,
    south: 53.701_215_354,
    north: 70.033_941_791,
};

/// The radars a composite's products are made from (S24b): `sweden` from
/// SMHI's twelve, `nordic` (OPERA's) from every radar in the table.
pub fn grid_radars(station: &Station, sites: &[Station]) -> Vec<Station> {
    sites
        .iter()
        .filter(|s| {
            s.kind == SiteKind::Polar
                && match station.provider {
                    ProviderId::Opera => true,
                    provider => s.provider == provider,
                }
        })
        .cloned()
        .collect()
}

/// The box a composite's products are drawn over: its composite's own; also
/// whether the composite offers them (`products::for_station`, review N3).
pub(crate) fn grid_box(station: &Station) -> Option<LonLatBox> {
    match (station.kind, station.provider) {
        (SiteKind::Grid, ProviderId::Opera) => Some(crate::providers::opera::NORDIC),
        (SiteKind::Grid, ProviderId::Smhi) => Some(SWEDEN_BOX),
        _ => None,
    }
}

/// Requests one whole volume of this radar costs (S24a review #2): an SMHI
/// volume ~`SMHI_VOLUME_REQUESTS` range requests; a per-angle ORD station
/// (FMI) one file per nominal angle, read whole; any other ORD volume 1.
fn volume_requests(station: &Station) -> usize {
    match station.provider {
        ProviderId::Smhi => SMHI_VOLUME_REQUESTS,
        _ if crate::providers::ord::per_angle(&station.source) => {
            crate::products::nominal_angles(station).len().max(1)
        }
        _ => 1,
    }
}
/// Every provider publishes a volume per radar every 5 minutes.
pub const CADENCE_MS: i64 = 5 * 60 * 1000;
/// The frame for T is due at T + 8 minutes (SMHI publishes 4–5 minutes
/// after the valid time, its listing sometimes later: Vara's 01:05 volume
/// came at T + 7:05 on 2026-09-15; ORD's cache about as fast)...
pub const DUE_MS: i64 = 8 * 60 * 1000;
/// ...and built then once no scan has arrived for this long, so a backfill
/// still delivering is not cut short.
pub const QUIET_MS: i64 = 30 * 1000;
/// A scan for T that comes after T was built builds T once more, while T
/// is one of the newest two frame times and younger than this.
pub const LATE_MS: i64 = 12 * 60 * 1000;
/// Between starting one radar's poller and the next, so a set's listings
/// and backfills do not all begin at once. Each provider already reads one
/// volume at a time, engine-wide (`smhi_live::FETCHER`, `ord`'s own).
const STAGGER: Duration = Duration::from_secs(2);
/// A radar whose scan reaches less than `SHORT` of its full range takes the
/// gates past that scan's edge from its latest longer scan up to this much
/// older (DMI's alternating 119.5 km Sindal scans, plan §S25).
pub const OUTER_MS: i64 = 10 * 60 * 1000;
const SHORT: f64 = 0.75;
/// The sphere the lookup rule measures ground distance and bearing on.
const SPHERE_M: f64 = 6_371_000.0;
/// Ground distances are kept in units of this many metres.
pub(crate) const UNIT_M: f64 = 10.0;
/// Past a radar's reach, or no ray or gate.
pub(crate) const NONE: u16 = u16::MAX;
/// A texture side longer than this is refused (the Nordic composite is
/// 2297; twelve radars across the Nordics stay under about 2,600).
const MAX_SIDE: u32 = 4096;
/// The credit `hello` lists; each frame names the owners it drew on.
pub const ATTRIBUTION: &str = "The chosen radars' owners, named on each frame";

// ---------------------------------------------------------------------------
// The set
// ---------------------------------------------------------------------------

/// How a texel chooses among the radars that see it; no averaging.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Rule {
    /// The radar whose beam centre is lowest above sea level there, the
    /// nearer radar on a tie (the default: plan §S25's Hurum case).
    #[default]
    Lowest,
    /// The strongest return of any radar there.
    Strongest,
    /// A slice at the set's height (S30, `Set::height_m`, `Set::above`):
    /// per radar the scan the height rule picks, then the radar whose beam
    /// centre is nearest the height, the nearer on a tie.
    Height,
}

impl Rule {
    pub const ALL: [Rule; 3] = [Rule::Lowest, Rule::Strongest, Rule::Height];

    pub fn id(self) -> &'static str {
        match self {
            Rule::Lowest => "lowest",
            Rule::Strongest => "strongest",
            Rule::Height => "height",
        }
    }

    /// The display name, also `frame.productName` (for `Height` the frame
    /// names its height instead, `Set::product`).
    pub fn name(self) -> &'static str {
        match self {
            Rule::Lowest => "Lowest beam",
            Rule::Strongest => "Strongest",
            Rule::Height => "Height",
        }
    }

    pub fn parse(id: &str) -> Option<Rule> {
        Rule::ALL.into_iter().find(|r| r.id() == id)
    }
}

/// One `hello.mosaic.rules` entry.
#[derive(Serialize, PartialEq, Debug)]
pub struct RuleInfo {
    pub id: &'static str,
    pub name: &'static str,
}

/// `hello.mosaic`: the station and what `set_mosaic` accepts.
#[derive(Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub station: &'static str,
    pub max_sites: usize,
    pub min_reach_km: f64,
    pub rules: Vec<RuleInfo>,
}

pub fn info() -> Info {
    Info {
        station: STATION,
        max_sites: MAX_SITES,
        min_reach_km: MIN_REACH_KM,
        rules: Rule::ALL
            .into_iter()
            .map(|r| RuleInfo {
                id: r.id(),
                name: r.name(),
            })
            .collect(),
    }
}

/// One radar of `state.mosaic.sites`: its canonical id and the reach in
/// force, km of ground distance (its `rangeKm` for full).
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SiteReach {
    pub id: String,
    pub reach_km: f64,
}

/// `state.mosaic`: the chosen radars in the order sent, and the rule.
/// Empty until a client sends `set_mosaic`.
#[derive(Serialize, Clone, PartialEq, Debug, Default)]
pub struct Set {
    pub sites: Vec<SiteReach>,
    pub rule: Rule,
    /// The `height` rule's height, metres, and what it is above (S30);
    /// `None`, and not sent, for the other rules.
    #[serde(rename = "heightM", skip_serializing_if = "Option::is_none")]
    pub height_m: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub above: Option<Above>,
}

/// 64-bit FNV-1a: names a set, nothing cryptographic.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

impl Set {
    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    /// The radars with their reach in tenths of a km, in id order: what
    /// makes two sets the same whatever order they were sent in.
    fn key(&self) -> Vec<(&str, i64)> {
        let mut key: Vec<(&str, i64)> = self
            .sites
            .iter()
            .map(|s| (s.id.as_str(), (s.reach_km * 10.0).round() as i64))
            .collect();
        key.sort_unstable();
        key
    }

    /// The same radars, reaches and rule (and height), in any order.
    pub fn same(&self, other: &Set) -> bool {
        self.rule == other.rule
            && self.height_m == other.height_m
            && self.above == other.above
            && self.key() == other.key()
    }

    /// `frame.product` and `productName`: `REF` and the rule's name, or for
    /// a height (S30) `CAPPI` and the height's name, as a radar's.
    pub fn product(&self) -> (&'static str, String) {
        match (self.rule, self.height_m) {
            (Rule::Height, Some(m)) => (
                crate::products::CAPPI,
                crate::products::height_name(m, self.above.unwrap_or_default()),
            ),
            _ => (crate::products::REF, self.rule.name().to_owned()),
        }
    }

    /// The last part of this set's frame ids and its catalog ring: `m` and
    /// 8 hex digits of the radars, reaches and rule, and a height set's
    /// height and what it is above (`products::variant_of` knows the form).
    /// A set without a height is named as before S30.
    pub fn variant(&self) -> String {
        let mut text = String::from(self.rule.id());
        if let Some(m) = self.height_m {
            text.push_str(&format!(":{m}:{}", self.above.unwrap_or_default().id()));
        }
        for (id, tenths) in self.key() {
            text.push_str(&format!("|{id}:{tenths}"));
        }
        let hash = fnv1a(text.as_bytes());
        format!("m{:08x}", (hash >> 32) as u32 ^ hash as u32)
    }
}

/// One `set_mosaic` site as a client may send it: an id, or an object with
/// an `id` and an optional `reachKm` (checked by `choose`, so a bad value is
/// answered in words).
#[derive(Deserialize, PartialEq, Debug, Clone)]
#[serde(untagged)]
pub enum SiteArg {
    Id(String),
    Site {
        id: String,
        #[serde(default, rename = "reachKm")]
        reach_km: Option<Value>,
    },
}

/// What `set_mosaic` asks for, as `state.mosaic` will hold it, or the error
/// its sender hears (`docs/protocol.md`, My mosaic): `choose_with` and no
/// height (the tests'; `main.rs` passes every field to `choose_with`).
#[cfg(test)]
pub fn choose(sites: &[Station], args: &[SiteArg], rule: Option<&str>) -> Result<Set, String> {
    choose_with(sites, args, rule, None, None)
}

/// `choose` with the `height` rule's `heightM` and `above` (S30), checked
/// exactly as `set_product` checks them for one radar (`products::choose`),
/// and refused with any other rule.
pub fn choose_with(
    sites: &[Station],
    args: &[SiteArg],
    rule: Option<&str>,
    height_m: Option<u32>,
    above: Option<&str>,
) -> Result<Set, String> {
    let rule = match rule {
        None => Rule::Lowest,
        Some(id) => Rule::parse(id)
            .ok_or_else(|| format!("Unknown rule {id}; rules are listed in hello.mosaic."))?,
    };
    let (height_m, above) = if rule == Rule::Height {
        let choice = crate::products::choose(crate::products::CAPPI, 0, height_m, above)?;
        (choice.height_m, choice.above)
    } else if height_m.is_some() || above.is_some() {
        return Err(format!(
            "heightM and above go with the height rule only, not {}.",
            rule.id()
        ));
    } else {
        (None, None)
    };
    if args.is_empty() {
        return Err("Choose at least one radar for My mosaic.".into());
    }
    if args.len() > MAX_SITES {
        return Err(format!(
            "My mosaic takes at most {MAX_SITES} radars; {} were sent.",
            args.len()
        ));
    }
    let mut chosen: Vec<SiteReach> = Vec::with_capacity(args.len());
    for arg in args {
        let (id, reach) = match arg {
            SiteArg::Id(id) => (id, None),
            SiteArg::Site { id, reach_km } => (id, reach_km.as_ref()),
        };
        let station = crate::providers::resolve(sites, id)
            .ok_or_else(|| format!("Unknown site {id}; stations are listed in hello."))?;
        if station.kind != SiteKind::Polar {
            return Err(format!(
                "{} is a composite; My mosaic is made of radars.",
                station.name
            ));
        }
        if chosen.iter().any(|s| s.id == station.id) {
            return Err(format!("{} is in the list twice.", station.name));
        }
        let full = station.range_km;
        let reach_km = match reach {
            None | Some(Value::Null) => full,
            Some(value) => {
                let km = value
                    .as_f64()
                    .filter(|km| km.is_finite())
                    .ok_or_else(|| format!("reachKm of {id} must be a number of km."))?;
                if km < MIN_REACH_KM {
                    return Err(format!(
                        "reachKm {km} of {id} is under the {MIN_REACH_KM} km minimum."
                    ));
                }
                if km >= full {
                    full
                } else {
                    (km * 10.0).round() / 10.0
                }
            }
        };
        chosen.push(SiteReach {
            id: station.id.clone(),
            reach_km,
        });
    }
    Ok(Set {
        sites: chosen,
        rule,
        height_m,
        above,
    })
}

/// The station as `hello` lists it, after the composites. Its position is
/// only for the picker: each frame is placed by its own box.
pub fn station() -> Station {
    Station {
        id: STATION.to_owned(),
        name: "My mosaic".to_owned(),
        state: String::new(),
        lat: 62.0,
        lon: 16.0,
        alt_m: 0.0,
        kind: SiteKind::Grid,
        country: String::new(),
        provider: ProviderId::Mosaic,
        range_km: 0.0,
        attribution: ATTRIBUTION.to_owned(),
        aliases: Vec::new(),
        source: String::new(),
    }
}

/// Where the engine keeps its set across restarts (review #11): under the
/// cache root, beside the frame catalog and the tilt store.
fn saved_path() -> Option<std::path::PathBuf> {
    crate::osm::cache_root()
        .ok()
        .map(|root| root.join("mosaic.json"))
}

/// Keep `set` for the next live start; a failure is only logged.
pub fn save(set: &Set) {
    let Some(path) = saved_path() else { return };
    let temp = path.with_extension("json.tmp");
    let written = serde_json::to_vec(set)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&temp, bytes))
        .and_then(|()| std::fs::rename(&temp, &path));
    if let Err(e) = written {
        log(format_args!("keeping the set: {e}"));
    }
}

/// The set a previous live run kept, checked again against today's table
/// (`choose`); empty when there is none or it no longer holds.
pub fn load(sites: &[Station]) -> Set {
    let Some(bytes) = saved_path().and_then(|p| std::fs::read(p).ok()) else {
        return Set::default();
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Set::default();
    };
    let args: Vec<SiteArg> = value["sites"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|s| {
                    Some(SiteArg::Site {
                        id: s["id"].as_str()?.to_owned(),
                        reach_km: s.get("reachKm").cloned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let height = value["heightM"]
        .as_u64()
        .and_then(|m| u32::try_from(m).ok());
    match choose_with(
        sites,
        &args,
        value["rule"].as_str(),
        height,
        value["above"].as_str(),
    ) {
        Ok(set) => {
            log(format_args!("kept set {} restored", set.variant()));
            set
        }
        Err(e) => {
            log(format_args!("the kept set no longer holds: {e}"));
            Set::default()
        }
    }
}

const CADENCE: Duration = Duration::from_millis(CADENCE_MS as u64);

/// The provider entry: a frame every 5 minutes, so SMHI's thresholds.
pub const SPEC: Spec = Spec {
    id: ProviderId::Mosaic,
    name: "My mosaic",
    attribution: ATTRIBUTION,
    country: "",
    range_km: 0.0,
    cadence: CADENCE,
    staleness: Staleness::from_cadence(CADENCE),
    backfill: BACKFILL,
    // Unused: the radars read under their own providers' plans.
    ranges: crate::smhi_live::DEC2,
};

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

pub(crate) fn mercator_x(lon: f64) -> f64 {
    MERCATOR_R * lon.to_radians()
}

/// The point `d` metres from (`lat`, `lon`) along the initial bearing
/// `bearing` (degrees), on the lookup rule's sphere.
fn destination(lat: f64, lon: f64, bearing: f64, d: f64) -> (f64, f64) {
    let (phi, lambda, theta, delta) = (
        lat.to_radians(),
        lon.to_radians(),
        bearing.to_radians(),
        d / SPHERE_M,
    );
    let phi2 = (phi.sin() * delta.cos() + phi.cos() * delta.sin() * theta.cos()).asin();
    let lambda2 = lambda
        + (theta.sin() * delta.sin() * phi.cos()).atan2(delta.cos() - phi.sin() * phi2.sin());
    (phi2.to_degrees(), lambda2.to_degrees())
}

/// A box in Web Mercator metres: west, east, south, north.
#[derive(Clone, Copy, Debug)]
struct MercBox {
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
}

/// The box of the circle of radius `d` metres around (`lat`, `lon`).
fn circle_box(lat: f64, lon: f64, d: f64) -> MercBox {
    let mut b = MercBox {
        x0: f64::INFINITY,
        x1: f64::NEG_INFINITY,
        y0: f64::INFINITY,
        y1: f64::NEG_INFINITY,
    };
    for step in 0..360 {
        let (la, lo) = destination(lat, lon, f64::from(step), d);
        let (x, y) = (mercator_x(lo), mercator_y(la));
        b.x0 = b.x0.min(x);
        b.x1 = b.x1.max(x);
        b.y0 = b.y0.min(y);
        b.y1 = b.y1.max(y);
    }
    b
}

/// One chosen radar placed on the texture.
pub struct Radar {
    pub station: Station,
    pub reach_m: f64,
    /// Which dataset its provider's lowest scan is (SMHI `/dataset1`, ORD
    /// the lowest angle): how the tilt store's copy is found.
    pub which: Which,
    /// Its part of the texture: columns `c0..c0 + w`, rows `r0..r0 + h`.
    c0: usize,
    r0: usize,
    w: usize,
    h: usize,
    /// Per texel of its part, row by row: ground distance in `UNIT_M`
    /// (`NONE` past the reach) and bearing in tenths of a degree.
    dist: Vec<u16>,
    az: Vec<u16>,
}

impl Radar {
    /// S24c (`grid3d.rs`): every texel of its part within the reach, as
    /// (column, row) on the layout's texture, ground distance in `UNIT_M`
    /// and bearing in tenths of a degree.
    pub(crate) fn texels(&self) -> impl Iterator<Item = (usize, usize, usize, u16)> + '_ {
        (0..self.h).flat_map(move |rr| {
            (0..self.w).filter_map(move |cc| {
                let k = rr * self.w + cc;
                let d = self.dist[k];
                (d != NONE).then(|| (self.c0 + cc, self.r0 + rr, usize::from(d), self.az[k]))
            })
        })
    }
}

/// What a layout is made for (S24b): My mosaic's set, or a provider
/// composite's product, made from every radar of its network.
#[derive(Clone)]
pub enum Job {
    Mine,
    Grid { station: Box<Station>, want: Want },
}

/// How a frame's texels are chosen.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Make {
    /// My mosaic's lowest-scan rules (`combine`).
    Scan(Rule),
    /// A slice at the set's height (`combine_height`): My mosaic's (S30) or
    /// a composite's `CAPPI` (S24b).
    Height,
    /// A composite's `CMAX`, `ETOP` or `VIL`: each radar's own product,
    /// then the maximum (`max_radar`).
    Max(Want),
    /// A composite's `LOWB`: each radar's lowest clear beam, then the lowest
    /// (`clear_radar`).
    Clear,
}

/// The texture of one set and every radar's placement on it.
pub struct Layout {
    pub width: u32,
    pub height: u32,
    /// Outer edges, degrees.
    pub west: f64,
    pub east: f64,
    pub north: f64,
    pub south: f64,
    pub radars: Vec<Radar>,
    pub set: Set,
    /// The Web Mercator lattice index of the west edge and of the north
    /// edge (x = `.0` × `PIXEL_M`, y = `.1` × `PIXEL_M`): where the terrain
    /// grid's texels (`terrain.rs`, the same lattice) meet this texture's.
    pub lattice: (i64, i64),
    /// My mosaic, or a composite's product (S24b).
    pub job: Job,
}

impl Layout {
    /// Place `set`'s radars (looked up in `sites`) on the texture over the
    /// box of their reach circles, whole `PIXEL_M` texels on the Mercator
    /// lattice.
    pub fn new(set: &Set, sites: &[Station]) -> Result<Layout, String> {
        Layout::place(set, sites, None, Job::Mine)
    }

    /// A composite's product (S24b): every radar of its network
    /// (`grid_radars`) at its full range, placed on the composite's own box
    /// (`grid_box`) on the same lattice.
    pub fn grid(station: &Station, want: Want, sites: &[Station]) -> Result<Layout, String> {
        let bbox = grid_box(station).ok_or_else(|| format!("{} has no products", station.id))?;
        let radars = grid_radars(station, sites);
        let (height_m, above) = match want {
            Want::Cappi(_, m) => (Some(m), Some(Above::Sea)),
            Want::CappiGround(_, m, _) => (Some(m), Some(Above::Ground)),
            _ => (None, None),
        };
        let set = Set {
            sites: radars
                .iter()
                .map(|s| SiteReach {
                    id: s.id.clone(),
                    reach_km: s.range_km,
                })
                .collect(),
            rule: if height_m.is_some() {
                Rule::Height
            } else {
                Rule::Lowest
            },
            height_m,
            above,
        };
        let job = Job::Grid {
            station: Box::new(station.clone()),
            want,
        };
        Layout::place(&set, sites, Some(bbox), job)
    }

    /// `new`'s placement over `bbox` when given, else over the box of the
    /// radars' reach circles; a radar is placed over its part of the box.
    fn place(
        set: &Set,
        sites: &[Station],
        bbox: Option<LonLatBox>,
        job: Job,
    ) -> Result<Layout, String> {
        let mut placed: Vec<(Station, f64, MercBox)> = Vec::new();
        for site in &set.sites {
            let station = crate::providers::resolve(sites, &site.id)
                .filter(|s| s.kind == SiteKind::Polar)
                .ok_or_else(|| format!("{} is not a radar", site.id))?
                .clone();
            let reach_m = site.reach_km * 1000.0;
            let b = circle_box(station.lat, station.lon, reach_m);
            placed.push((station, reach_m, b));
        }
        if placed.is_empty() {
            return Err("no radars".into());
        }
        let (x0, x1, y0, y1) = match bbox {
            Some(b) => (
                mercator_x(b.west),
                mercator_x(b.east),
                mercator_y(b.south),
                mercator_y(b.north),
            ),
            None => (
                placed.iter().map(|p| p.2.x0).fold(f64::INFINITY, f64::min),
                placed
                    .iter()
                    .map(|p| p.2.x1)
                    .fold(f64::NEG_INFINITY, f64::max),
                placed.iter().map(|p| p.2.y0).fold(f64::INFINITY, f64::min),
                placed
                    .iter()
                    .map(|p| p.2.y1)
                    .fold(f64::NEG_INFINITY, f64::max),
            ),
        };
        let mx_west = (x0 / PIXEL_M).floor() * PIXEL_M;
        let my_north = (y1 / PIXEL_M).ceil() * PIXEL_M;
        let width = ((x1 - mx_west) / PIXEL_M).ceil() as u32;
        let height = ((my_north - y0) / PIXEL_M).ceil() as u32;
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(format!("a {width} × {height} texture is out of bounds"));
        }
        let lon_of = |c: f64| (mx_west + c * PIXEL_M) / MERCATOR_R;
        let lat_of = |r: f64| mercator_lat(my_north - r * PIXEL_M).to_radians();
        let radars = placed
            .into_iter()
            .map(|(station, reach_m, b)| {
                let c0 = (((b.x0 - mx_west) / PIXEL_M).floor().max(0.0)) as usize;
                let c1 = (((b.x1 - mx_west) / PIXEL_M).ceil() as usize).min(width as usize);
                let r0 = (((my_north - b.y1) / PIXEL_M).floor().max(0.0)) as usize;
                let r1 = (((my_north - b.y0) / PIXEL_M).ceil() as usize).min(height as usize);
                let (w, h) = (c1.saturating_sub(c0), r1.saturating_sub(r0));
                let (phi1, lambda1) = (station.lat.to_radians(), station.lon.to_radians());
                let (sin1, cos1) = phi1.sin_cos();
                // Per column: the longitude difference's parts.
                let columns: Vec<(f64, f64, f64)> = (c0..c0 + w)
                    .map(|c| {
                        let dl = lon_of(c as f64 + 0.5) - lambda1;
                        let (s, co) = dl.sin_cos();
                        let half = (dl / 2.0).sin();
                        (s, co, half * half)
                    })
                    .collect();
                let mut dist = Vec::with_capacity(w * h);
                let mut az = Vec::with_capacity(w * h);
                for r in r0..r0 + h {
                    let phi2 = lat_of(r as f64 + 0.5);
                    let (sin2, cos2) = phi2.sin_cos();
                    let half = ((phi2 - phi1) / 2.0).sin();
                    let hav_lat = half * half;
                    for &(sin_dl, cos_dl, hav_dl) in &columns {
                        let hav = (hav_lat + cos1 * cos2 * hav_dl).clamp(0.0, 1.0);
                        let s = 2.0 * SPHERE_M * hav.sqrt().asin();
                        if s > reach_m {
                            dist.push(NONE);
                            az.push(0);
                            continue;
                        }
                        let bearing = (sin_dl * cos2)
                            .atan2(cos1 * sin2 - sin1 * cos2 * cos_dl)
                            .to_degrees()
                            .rem_euclid(360.0);
                        dist.push((s / UNIT_M).round() as u16);
                        az.push(((bearing * 10.0).floor() as u16) % 3600);
                    }
                }
                let which = match station.provider {
                    ProviderId::Smhi => Which::First,
                    _ => Which::Lowest,
                };
                Radar {
                    station,
                    reach_m,
                    which,
                    c0,
                    r0,
                    w,
                    h,
                    dist,
                    az,
                }
            })
            .collect();
        let east = (mx_west + f64::from(width) * PIXEL_M) / MERCATOR_R;
        Ok(Layout {
            lattice: (
                (mx_west / PIXEL_M).round() as i64,
                (my_north / PIXEL_M).round() as i64,
            ),
            width,
            height,
            west: (mx_west / MERCATOR_R).to_degrees(),
            east: east.to_degrees(),
            north: mercator_lat(my_north),
            south: mercator_lat(my_north - f64::from(height) * PIXEL_M),
            radars,
            set: set.clone(),
            job,
        })
    }

    /// The station the frames are for: `mymosaic`, or the composite.
    pub fn station_id(&self) -> &str {
        match &self.job {
            Job::Mine => STATION,
            Job::Grid { station, .. } => &station.id,
        }
    }

    /// What names the frames in the provenance and `sourceProjdef`.
    fn name(&self) -> String {
        match &self.job {
            Job::Mine => "My mosaic".to_owned(),
            Job::Grid { station, .. } => station.name.clone(),
        }
    }

    /// The last part of the frame ids and the catalog ring: the set's, or a
    /// composite product's own (`cmax`, `lowb`, `cappi2000`, …).
    pub fn variant(&self) -> String {
        match &self.job {
            Job::Mine => self.set.variant(),
            Job::Grid { want, .. } => want.variant(),
        }
    }

    /// `frame.product` and `productName`.
    pub fn product(&self) -> (&'static str, String) {
        match &self.job {
            Job::Mine => self.set.product(),
            Job::Grid { want, .. } => want.product(),
        }
    }

    /// How the texels are chosen.
    pub fn make(&self) -> Make {
        match (&self.job, self.set.rule) {
            (_, Rule::Height) => Make::Height,
            (Job::Mine, rule) => Make::Scan(rule),
            (
                Job::Grid {
                    want: Want::LowestBeam,
                    ..
                },
                _,
            ) => Make::Clear,
            (Job::Grid { want, .. }, _) => Make::Max(*want),
        }
    }

    /// Whether the frames are made from the tilt store (a height set, S30;
    /// every composite product, S24b) rather than the lowest scans in hand.
    pub fn stored(&self) -> bool {
        !matches!(self.make(), Make::Scan(_))
    }

    /// What `station`'s poller reads for this layout: the lowest scan; every
    /// scan for a height or a column product (every height needs nearly
    /// every scan, and one read then serves every other product from the
    /// store); for `LOWB` the scans its clear beams need (`Want::Hybrid`,
    /// where the engine has a blockage table), else the lowest. A radar with
    /// no known angles gives its lowest scan.
    pub fn read_want(&self, station: &Station) -> Want {
        if crate::products::nominal_angles(station).is_empty() {
            return Want::Lowest;
        }
        match self.make() {
            Make::Scan(_) => Want::Lowest,
            Make::Height | Make::Max(_) => Want::ColMax,
            Make::Clear => {
                crate::products::blockage(&station.id).map_or(Want::Lowest, Want::Hybrid)
            }
        }
    }

    /// Frame times this set builds back (review #8): `BACKFILL`, or
    /// `HEIGHT_BACKFILL` for a height set with a radar whose whole volume
    /// costs more than one request: SMHI's ~39 range requests, an FMI
    /// radar's file per angle (S24a review #2). A one-file ORD volume is 1.
    /// A composite's product: `grid_backfill` (S24b).
    pub fn backfill(&self) -> usize {
        if let Job::Grid { want, .. } = &self.job {
            return grid_backfill(*want);
        }
        let costly = self.radars.iter().any(|r| volume_requests(&r.station) > 1);
        if self.set.rule == Rule::Height && costly {
            HEIGHT_BACKFILL
        } else {
            BACKFILL
        }
    }

    /// The middle of the texture's box, (lat, lon): `frame.site`; a
    /// composite's product is sited as the composite's frames (S24b).
    pub fn centre(&self) -> (f64, f64) {
        if let Job::Grid { station, .. } = &self.job {
            return (station.lat, station.lon);
        }
        let y = (mercator_y(self.north) + mercator_y(self.south)) / 2.0;
        (mercator_lat(y), (self.west + self.east) / 2.0)
    }
}

// ---------------------------------------------------------------------------
// Combining
// ---------------------------------------------------------------------------

/// One radar's scans for one frame: its scan at T, and for a short scan
/// the longer one its far ring comes from.
#[derive(Clone, Copy)]
pub struct Input<'a> {
    pub primary: &'a Sweep,
    pub outer: Option<&'a Sweep>,
}

/// A scan turned into lookups over one radar's ground distances.
struct Lookup<'a> {
    sweep: &'a Sweep,
    /// Per tenth-degree entry, the ray nearest its centre within
    /// `sweep::GAP_DEG`, as the azimuth lookup names it.
    rows: Vec<u16>,
    /// Per ground distance unit up to the reach: the gate (`NONE` off the
    /// scan's gates) and the beam centre's height above sea level, metres.
    gate: Vec<u16>,
    height: Vec<i32>,
    /// The first distance unit at or past the scan's far edge.
    edge: usize,
}

/// The azimuth lookup's rows (`Sweep::lut_rows`, `docs/protocol.md`).
fn rows_of(sweep: &Sweep) -> Vec<u16> {
    let azimuths: Vec<f32> = sweep.rays.iter().map(|r| r.azimuth_deg).collect();
    let n = azimuths.len();
    (0..3600u32)
        .map(|entry| {
            if n == 0 {
                return NONE;
            }
            let center = (entry as f32 + 0.5) / 10.0;
            let after = azimuths.partition_point(|&a| a <= center) % n;
            let before = (after + n - 1) % n;
            let distance = |row: usize| {
                let d = (azimuths[row] - center).abs();
                d.min(360.0 - d)
            };
            let (row, d) = if distance(before) <= distance(after) {
                (before, distance(before))
            } else {
                (after, distance(after))
            };
            if d > crate::sweep::GAP_DEG {
                NONE
            } else {
                row as u16
            }
        })
        .collect()
}

impl<'a> Lookup<'a> {
    fn new(sweep: &'a Sweep, alt_m: f64, reach_m: f64) -> Lookup<'a> {
        let units = (reach_m / UNIT_M).round() as usize + 1;
        let e = sweep.elevation_deg().to_radians();
        let (first, spacing) = (
            f64::from(sweep.first_gate_m),
            f64::from(sweep.gate_spacing_m.max(1)),
        );
        let gates = f64::from(sweep.gates);
        let mut gate = Vec::with_capacity(units);
        let mut height = Vec::with_capacity(units);
        let mut edge = units;
        for d in 0..units {
            let theta = d as f64 * UNIT_M / EARTH_M;
            let r = EARTH_M * theta.sin() / (e + theta).cos();
            let g = ((r - first) / spacing).round();
            if g >= gates && edge == units {
                edge = d;
            }
            gate.push(if g >= 0.0 && g < gates {
                g as u16
            } else {
                NONE
            });
            height.push((alt_m + EARTH_M * e.cos() / (e + theta).cos() - EARTH_M).round() as i32);
        }
        Lookup {
            sweep,
            rows: rows_of(sweep),
            gate,
            height,
            edge,
        }
    }

    /// The code at distance unit `d` and bearing entry `az`, with the beam
    /// height there; `None` where the scan has no gate or ray.
    fn at(&self, d: usize, az: u16) -> Option<(u8, i32)> {
        let g = *self.gate.get(d)?;
        let row = self.rows[usize::from(az)];
        if g == NONE || row == NONE {
            return None;
        }
        let code = self.sweep.rays[usize::from(row)].codes[usize::from(g)];
        Some((code, self.height[d]))
    }
}

/// Every texel's code under `rule` from each radar's `inputs` (in the
/// layout's radar order, `None` for a radar with no scan), and which radar
/// gave it (`u8::MAX` for none). Codes: 0 below threshold, 1 no data, 2..255
/// measured (`docs/protocol.md`, My mosaic).
pub fn combine(layout: &Layout, rule: Rule, inputs: &[Option<Input>]) -> (Vec<u8>, Vec<u8>) {
    let width = layout.width as usize;
    let n = width * layout.height as usize;
    let mut codes = vec![1u8; n];
    let mut owner = vec![u8::MAX; n];
    // Lowest beam: (height + offset) << 16 | distance, lower wins; so the
    // lower beam, then the nearer radar. Strongest: the code's rank.
    let mut best = vec![u64::MAX; n];
    for (index, (radar, input)) in layout.radars.iter().zip(inputs).enumerate() {
        let Some(input) = input else { continue };
        let alt = radar.station.alt_m;
        let primary = Lookup::new(input.primary, alt, radar.reach_m);
        let outer = input
            .outer
            .map(|sweep| Lookup::new(sweep, alt, radar.reach_m));
        for rr in 0..radar.h {
            let row_base = (radar.r0 + rr) * width + radar.c0;
            for cc in 0..radar.w {
                let k = rr * radar.w + cc;
                let d = radar.dist[k];
                if d == NONE {
                    continue;
                }
                let d = usize::from(d);
                let lookup = match &outer {
                    Some(o) if d >= primary.edge => o,
                    _ => &primary,
                };
                let Some((code, height)) = lookup.at(d, radar.az[k]) else {
                    continue;
                };
                let t = row_base + cc;
                let key = match rule {
                    // A height set is combined by `combine_height`; here it
                    // would take the lowest beam.
                    Rule::Lowest | Rule::Height => {
                        if code == 1 {
                            continue;
                        }
                        lowest_key(height, d)
                    }
                    // Lower is better here too: measured by code, then
                    // below threshold, then no data.
                    Rule::Strongest => match code {
                        1 => u64::MAX - 1,
                        0 => u64::MAX - 2,
                        c => 255 - u64::from(c),
                    },
                };
                if key < best[t] {
                    best[t] = key;
                    codes[t] = code;
                    owner[t] = if code == 1 { u8::MAX } else { index as u8 };
                }
            }
        }
    }
    (codes, owner)
}

/// Lowest beam's order of two candidates: the lower beam centre (metres
/// above sea level) first, then the nearer radar (distance units); the
/// lower key wins, and an exact tie keeps the radar earlier in the set.
fn lowest_key(height_m: i32, d: usize) -> u64 {
    (((i64::from(height_m) + (1 << 24)) as u64) << 16) | d as u64
}

// ---------------------------------------------------------------------------
// At a height (S30)
// ---------------------------------------------------------------------------

/// One scan of a radar's volume turned into lookups for the height rule:
/// per ground distance unit up to the reach, its gate (`NONE` off its
/// gates) and its beam centre's height above the antenna, metres, at its
/// own angle (`where/elangle`, as a radar's `CAPPI` takes it).
pub(crate) struct Beam<'a> {
    pub(crate) deg: f64,
    pub(crate) sweep: &'a Sweep,
    pub(crate) rows: Vec<u16>,
    pub(crate) gate: Vec<u16>,
    pub(crate) height: Vec<f64>,
    /// The first distance unit at or past its far edge.
    pub(crate) edge: usize,
}

impl<'a> Beam<'a> {
    pub(crate) fn new(tilt: &'a Tilt, reach_m: f64) -> Beam<'a> {
        let units = (reach_m / UNIT_M).round() as usize + 1;
        let e = tilt.elangle.to_radians();
        let sweep = &tilt.sweep;
        let (first, spacing) = (
            f64::from(sweep.first_gate_m),
            f64::from(sweep.gate_spacing_m.max(1)),
        );
        let gates = f64::from(sweep.gates);
        let (mut gate, mut height, mut edge) =
            (Vec::with_capacity(units), Vec::with_capacity(units), units);
        for d in 0..units {
            let theta = d as f64 * UNIT_M / EARTH_M;
            let c = (e + theta).cos();
            if c <= 1e-9 {
                edge = edge.min(d);
                gate.push(NONE);
                height.push(f64::INFINITY);
                continue;
            }
            let g = ((EARTH_M * theta.sin() / c - first) / spacing).round();
            if g >= gates {
                edge = edge.min(d);
            }
            gate.push(if g >= 0.0 && g < gates {
                g as u16
            } else {
                NONE
            });
            height.push(EARTH_M * e.cos() / c - EARTH_M);
        }
        Beam {
            deg: tilt.elangle,
            sweep,
            rows: rows_of(sweep),
            gate,
            height,
            edge,
        }
    }
}

/// A radar's volume for one frame at a height: its scans, ascending by
/// angle.
struct Stack<'a> {
    beams: Vec<Beam<'a>>,
    /// Where its lowest scan ends: past it a short volume's far ring comes
    /// from the longer one.
    edge: usize,
}

impl<'a> Stack<'a> {
    fn new(tilts: &'a [Tilt], reach_m: f64) -> Stack<'a> {
        let beams: Vec<Beam> = tilts.iter().map(|t| Beam::new(t, reach_m)).collect();
        let edge = beams.first().map_or(0, |b| b.edge);
        Stack { beams, edge }
    }

    /// The height rule (`products::nearest_beam`) at distance unit `d` for
    /// `target` metres above the antenna: the scan's index, `u8::MAX` for
    /// none.
    fn pick(&self, d: usize, target: f64) -> u8 {
        let seen = elevation_to(d as f64 * UNIT_M, target);
        let covering = self.beams.iter().enumerate().filter_map(|(k, b)| {
            let g = *b.gate.get(d)?;
            (g != NONE).then(|| (k, b.deg, b.height[d]))
        });
        nearest_beam(seen, target, covering).map_or(u8::MAX, |(k, _)| k as u8)
    }

    /// The pick at every distance unit for one target: above sea level it
    /// depends on the distance alone.
    fn picks(&self, target: f64, units: usize) -> Vec<u8> {
        (0..units).map(|d| self.pick(d, target)).collect()
    }
}

/// One radar's volumes for a frame at a height: its volume at T, scans
/// ascending by angle, and for a short volume the longer one its far ring
/// comes from.
#[derive(Clone, Copy)]
pub struct Volumes<'a> {
    pub primary: &'a [Tilt],
    pub outer: Option<&'a [Tilt]>,
}

/// Every texel's code under the `height` rule at `height_m` metres above
/// sea level, or with `ground` above the terrain of each texel, from each
/// radar's `inputs` (layout order; `None` for a radar with no volume), and
/// which radar gave it (`u8::MAX` for none) (`docs/protocol.md`, My mosaic):
/// per radar the scan the height rule picks at the texel; a radar with none,
/// or whose pick is no data there, is no candidate; of the candidates, the
/// one whose beam centre is nearest the height, to the metre, the nearer
/// radar on a tie, and an exact tie the radar earlier in the set.
pub fn combine_height(
    layout: &Layout,
    height_m: u32,
    ground: bool,
    inputs: &[Option<Volumes>],
) -> (Vec<u8>, Vec<u8>) {
    let mut pick = Pick::new(layout);
    let terrain = terrain_for(ground);
    for (index, (radar, input)) in layout.radars.iter().zip(inputs).enumerate() {
        let Some(input) = input else { continue };
        height_radar(&mut pick, layout, index, radar, *input, height_m, terrain);
    }
    (pick.codes, pick.owner)
}

/// The terrain grid for a height above the ground; `None` above sea level.
fn terrain_for(ground: bool) -> Option<&'static crate::terrain::Grid> {
    let terrain = if ground { crate::terrain::grid() } else { None };
    if ground && terrain.is_none() {
        // Never with the embedded grid (terrain.rs parses it in its tests).
        debug_assert!(
            false,
            "no terrain grid: above the ground falls back to sea level"
        );
        log("the terrain grid did not load: this frame is above sea level, not the ground");
    }
    terrain
}

/// Every texel's choice so far: its code, the radar that gave it
/// (`u8::MAX` for none) and that choice's key, the lower the better.
struct Pick {
    codes: Vec<u8>,
    owner: Vec<u8>,
    best: Vec<u64>,
}

impl Pick {
    fn new(layout: &Layout) -> Pick {
        let n = layout.width as usize * layout.height as usize;
        Pick {
            codes: vec![1; n],
            owner: vec![u8::MAX; n],
            best: vec![u64::MAX; n],
        }
    }

    /// Radar `index` offers `code` at texel `t` under `key`: the lower key
    /// wins, and an exact tie keeps the radar earlier in the layout.
    fn offer(&mut self, t: usize, key: u64, code: u8, index: usize) {
        if key < self.best[t] {
            self.best[t] = key;
            self.codes[t] = code;
            self.owner[t] = index as u8;
        }
    }
}

/// One radar's part of a slice at `height_m` (S30's `combine_height`; a
/// composite's `CAPPI`, S24b): per texel the scan the height rule picks; no
/// such scan, or no data there, is no candidate; the beam centre nearest
/// the height wins, then the nearer radar (`height_key`).
fn height_radar(
    pick: &mut Pick,
    layout: &Layout,
    index: usize,
    radar: &Radar,
    input: Volumes,
    height_m: u32,
    terrain: Option<&crate::terrain::Grid>,
) {
    if input.primary.is_empty() {
        return;
    }
    let width = layout.width as usize;
    // The target above this antenna, before the ground under a texel.
    let base = f64::from(height_m) - radar.station.alt_m;
    let primary = Stack::new(input.primary, radar.reach_m);
    let outer = input
        .outer
        .filter(|o| !o.is_empty())
        .map(|o| Stack::new(o, radar.reach_m));
    let units = (radar.reach_m / UNIT_M).round() as usize + 1;
    let flat: [Vec<u8>; 2] = match terrain {
        None => [
            primary.picks(base, units),
            outer
                .as_ref()
                .map_or_else(Vec::new, |o| o.picks(base, units)),
        ],
        Some(_) => [Vec::new(), Vec::new()],
    };
    for rr in 0..radar.h {
        let row = radar.r0 + rr;
        let north = layout.lattice.1 - row as i64;
        for cc in 0..radar.w {
            let k = rr * radar.w + cc;
            if radar.dist[k] == NONE {
                continue;
            }
            let d = usize::from(radar.dist[k]);
            let (stack, side) = match &outer {
                Some(o) if d >= primary.edge => (o, 1),
                _ => (&primary, 0),
            };
            let (chosen, target) = match terrain {
                None => (flat[side][d], base),
                Some(grid) => {
                    let col = layout.lattice.0 + (radar.c0 + cc) as i64;
                    let target = base + grid.texel(col, north);
                    (stack.pick(d, target), target)
                }
            };
            if chosen == u8::MAX {
                continue;
            }
            let beam = &stack.beams[usize::from(chosen)];
            let ray = beam.rows[usize::from(radar.az[k])];
            let code = if ray == NONE {
                1
            } else {
                beam.sweep.rays[usize::from(ray)].codes[usize::from(beam.gate[d])]
            };
            if code == 1 {
                continue;
            }
            let key = height_key((beam.height[d] - target).abs(), d);
            pick.offer(row * width + radar.c0 + cc, key, code, index);
        }
    }
}

// ---------------------------------------------------------------------------
// The composites' products (S24b)
// ---------------------------------------------------------------------------

/// Offer one radar's codes over its part of the texture from its lookups:
/// `near` up to its scan's far edge, `far` (a short scan's far ring from its
/// longer predecessor) past it. `key(code, beam centre above sea level,
/// distance unit)` orders a candidate; `None` is no candidate.
fn place(
    pick: &mut Pick,
    layout: &Layout,
    index: usize,
    radar: &Radar,
    near: &Lookup,
    far: Option<&Lookup>,
    key: impl Fn(u8, i32, usize) -> Option<u64>,
) {
    let width = layout.width as usize;
    for rr in 0..radar.h {
        let row_base = (radar.r0 + rr) * width + radar.c0;
        for cc in 0..radar.w {
            let k = rr * radar.w + cc;
            let d = radar.dist[k];
            if d == NONE {
                continue;
            }
            let d = usize::from(d);
            let lookup = match far {
                Some(o) if d >= near.edge => o,
                _ => near,
            };
            let Some((code, height)) = lookup.at(d, radar.az[k]) else {
                continue;
            };
            if let Some(key) = key(code, height, d) {
                pick.offer(row_base + cc, key, code, index);
            }
        }
    }
}

/// The maximum's order, the lower key the better: a measured code by its
/// value (a storm height by its top, and of two equal tops the exact one, an
/// even code, over the "at least" one), then below threshold; no data is no
/// candidate (`docs/protocol.md`, the composites' products).
fn max_key(code: u8, etop: bool) -> Option<u64> {
    match code {
        1 => None,
        0 => Some(u64::MAX - 1),
        c if etop => {
            let top = u64::from(c - 2) / 2;
            let exact = u64::from(c.is_multiple_of(2));
            Some(1_000 - (2 * top + exact))
        }
        c => Some(255 - u64::from(c)),
    }
}

/// One radar's part of a composite's `CMAX`, `ETOP` or `VIL`: its own
/// product, made exactly as its radar frame is (`products::compose`, on its
/// lowest scan's rays and gates; a short volume's far ring from its longer
/// predecessor's), placed at that frame's elevation; the maximum wins.
fn max_radar(
    pick: &mut Pick,
    layout: &Layout,
    index: usize,
    radar: &Radar,
    want: Want,
    volumes: RadarVolumes,
) {
    let alt = radar.station.alt_m;
    // A storm height is above sea level: each radar's own antenna.
    let own = match want {
        Want::EchoTop(_) => Want::EchoTop(alt),
        other => other,
    };
    let (primary, outer) = volumes;
    let Ok(near) = crate::products::compose(own, primary) else {
        return;
    };
    let far = outer
        .filter(|o| !o.is_empty())
        .and_then(|o| crate::products::compose(own, o).ok());
    let near = Lookup::new(&near, alt, radar.reach_m);
    let far = far.as_ref().map(|s| Lookup::new(s, alt, radar.reach_m));
    let etop = matches!(own, Want::EchoTop(_));
    place(
        pick,
        layout,
        index,
        radar,
        &near,
        far.as_ref(),
        |code, _, _| max_key(code, etop),
    );
}

/// A volume's beams for `LOWB`, in its order (ascending by angle), and per
/// degree of azimuth the index of its clear beam: the blockage table's pick
/// (`products::hybrid_pick`), or its lowest scan without a table.
fn clear_beams<'a>(
    tilts: &'a [Tilt],
    table: Option<&crate::products::Blockage>,
    reach_m: f64,
) -> (Vec<Beam<'a>>, [u8; 360]) {
    let beams: Vec<Beam> = tilts.iter().map(|t| Beam::new(t, reach_m)).collect();
    let infos: Vec<TiltInfo> = tilts.iter().map(Tilt::info).collect();
    let lowest = (0..infos.len())
        .min_by(|&a, &b| infos[a].elangle.total_cmp(&infos[b].elangle))
        .unwrap_or(0);
    let mut picks = [lowest as u8; 360];
    if let Some(table) = table {
        for (degree, chosen) in picks.iter_mut().enumerate() {
            *chosen = crate::products::hybrid_pick(&infos, table.tenths[degree]) as u8;
        }
    }
    (beams, picks)
}

/// One radar's part of a composite's `LOWB`: per texel the scan of its
/// lowest clear beam at the texel's bearing (the degree `floor(bearing)`),
/// placed at that scan's own angle; a candidate where that gate is not no
/// data; the lowest beam centre above sea level wins, then the nearer radar
/// (`lowest_key`, My mosaic's `lowest` rule).
fn clear_radar(
    pick: &mut Pick,
    layout: &Layout,
    index: usize,
    radar: &Radar,
    primary: &[Tilt],
    outer: Option<&[Tilt]>,
) {
    if primary.is_empty() {
        return;
    }
    let table = crate::products::blockage(&radar.station.id);
    let (near, near_picks) = clear_beams(primary, table, radar.reach_m);
    let far = outer
        .filter(|o| !o.is_empty())
        .map(|o| clear_beams(o, table, radar.reach_m));
    // Past the lowest scan's edge a short volume's far ring comes from the
    // longer one (as `Stack`).
    let edge = near
        .iter()
        .min_by(|a, b| a.deg.total_cmp(&b.deg))
        .map_or(0, |b| b.edge);
    let (alt, width) = (radar.station.alt_m, layout.width as usize);
    for rr in 0..radar.h {
        let row_base = (radar.r0 + rr) * width + radar.c0;
        for cc in 0..radar.w {
            let k = rr * radar.w + cc;
            if radar.dist[k] == NONE {
                continue;
            }
            let d = usize::from(radar.dist[k]);
            let (beams, picks) = match &far {
                Some((beams, picks)) if d >= edge => (beams, picks),
                _ => (&near, &near_picks),
            };
            let az = radar.az[k];
            let beam = &beams[usize::from(picks[usize::from(az / 10) % 360])];
            let (Some(&g), ray) = (beam.gate.get(d), beam.rows[usize::from(az)]) else {
                continue;
            };
            if g == NONE || ray == NONE {
                continue;
            }
            let code = beam.sweep.rays[usize::from(ray)].codes[usize::from(g)];
            if code == 1 {
                continue;
            }
            let height = (alt + beam.height[d]).round() as i32;
            pick.offer(row_base + cc, lowest_key(height, d), code, index);
        }
    }
}

/// One radar's volumes for a composite's frame, owned: its volume at T,
/// scans ascending by angle, and a short volume's longer predecessor.
pub type RadarVolumes = (Vec<Tilt>, Option<Vec<Tilt>>);

/// A composite's product at every texel.
pub struct Combined {
    pub codes: Vec<u8>,
    /// Which radar gave each texel, `u8::MAX` for none (the answer-key
    /// tests read it; a frame credits every radar it read, `ends`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub owner: Vec<u8>,
    /// Each radar's latest scan end; `None` when it was not used.
    pub ends: Vec<Option<i64>>,
}

/// A composite's product (S24b, `docs/protocol.md`, the composites'
/// products), radar by radar: `load(i)` gives radar `i`'s volumes (`None`:
/// missing), held only while that radar is combined, so a Nordic frame
/// never holds 41 decoded volumes at once.
pub fn combine_grid(
    layout: &Layout,
    mut load: impl FnMut(usize) -> Option<RadarVolumes>,
) -> Combined {
    let mut pick = Pick::new(layout);
    let mut ends = vec![None; layout.radars.len()];
    let make = layout.make();
    let height_m = layout
        .set
        .height_m
        .unwrap_or(crate::products::HEIGHT_DEFAULT_M);
    let terrain = terrain_for(make == Make::Height && layout.set.above == Some(Above::Ground));
    for (index, radar) in layout.radars.iter().enumerate() {
        let Some((primary, outer)) = load(index) else {
            continue;
        };
        if primary.is_empty() {
            continue;
        }
        ends[index] = primary.iter().map(|t| t.sweep.end_ms).max();
        match make {
            Make::Height => {
                let input = Volumes {
                    primary: &primary,
                    outer: outer.as_deref(),
                };
                height_radar(&mut pick, layout, index, radar, input, height_m, terrain);
            }
            Make::Max(want) => max_radar(&mut pick, layout, index, radar, want, (primary, outer)),
            Make::Clear => clear_radar(&mut pick, layout, index, radar, &primary, outer.as_deref()),
            // My mosaic's lowest-scan rules are `combine`'s.
            Make::Scan(_) => {}
        }
    }
    Combined {
        codes: pick.codes,
        owner: pick.owner,
        ends,
    }
}

/// The height rule's order of two radars: the beam centre nearer the
/// height (to the metre) first, then the nearer radar (distance units); the
/// lower key wins.
fn height_key(miss_m: f64, d: usize) -> u64 {
    ((miss_m.round().min(f64::from(u32::MAX)) as u64) << 24) | d as u64
}

/// A built frame: the grid, and what names and credits it.
pub struct Built {
    pub grid: Grid,
    /// `Set::variant`, the frame id's last part.
    pub variant: String,
    /// `frame.product` and `productName` (`Set::product`).
    pub product: &'static str,
    pub product_name: String,
    /// Every owner the frame drew on, joined by `; `.
    pub attribution: String,
    /// (lat, lon) of the box's middle: `frame.site`.
    pub centre: (f64, f64),
    /// The product's own legend and coding when not the reflectivity's
    /// (S24b: a composite's `ETOP` and `VIL`).
    pub legend: Option<&'static Legend>,
    pub scale: f32,
    pub offset: f32,
}

/// A product's own legend and coding (S24a's storm height and rain mass),
/// else the reflectivity's.
fn coding(product: &str) -> (Option<&'static Legend>, f32, f32) {
    use crate::products as p;
    match product {
        p::ETOP => (Some(&p::ETOP_LEGEND), p::ETOP_SCALE, p::ETOP_OFFSET),
        p::VIL => (Some(&p::VIL_LEGEND), p::VIL_SCALE, p::VIL_OFFSET),
        _ => (None, SCALE, OFFSET),
    }
}

/// The frame for T from each radar's `inputs` (layout order; `None` for a
/// missing radar), under the lowest-scan rules.
pub fn build(layout: &Layout, t_ms: i64, inputs: &[Option<Input>]) -> Built {
    let (codes, _) = combine(layout, layout.set.rule, inputs);
    let ends: Vec<Option<i64>> = inputs
        .iter()
        .map(|i| i.as_ref().map(|i| i.primary.end_ms))
        .collect();
    built(layout, t_ms, codes, &ends)
}

/// The frame for T at the set's height (S30) from each radar's volumes.
pub fn build_height(layout: &Layout, t_ms: i64, inputs: &[Option<Volumes>]) -> Built {
    let height = layout
        .set
        .height_m
        .unwrap_or(crate::products::HEIGHT_DEFAULT_M);
    let ground = layout.set.above == Some(Above::Ground);
    let (codes, _) = combine_height(layout, height, ground, inputs);
    let ends: Vec<Option<i64>> = inputs
        .iter()
        .map(|i| {
            i.as_ref().map(|v| {
                v.primary
                    .iter()
                    .map(|t| t.sweep.end_ms)
                    .max()
                    .unwrap_or(t_ms)
            })
        })
        .collect();
    built(layout, t_ms, codes, &ends)
}

/// A frame of `codes`, credited to the radars that had a scan (`ends`,
/// each one's latest end).
fn built(layout: &Layout, t_ms: i64, codes: Vec<u8>, ends: &[Option<i64>]) -> Built {
    let (product, product_name) = layout.product();
    let (legend, scale, offset) = coding(product);
    let mut owners: Vec<&str> = Vec::new();
    let mut used = Vec::new();
    let mut end_ms = t_ms;
    for (radar, end) in layout.radars.iter().zip(ends) {
        if let Some(end) = end {
            end_ms = end_ms.max(*end);
            used.push(radar.station.id.as_str());
            if !owners.contains(&radar.station.attribution.as_str()) {
                owners.push(&radar.station.attribution);
            }
        }
    }
    Built {
        grid: Grid {
            width: layout.width,
            height: layout.height,
            codes,
            start_ms: t_ms,
            end_ms,
            elevation_deg: 0.0,
            west: layout.west,
            east: layout.east,
            north: layout.north,
            south: layout.south,
            source_projdef: format!("{}, {product_name}: {}", layout.name(), used.join(", ")),
        },
        variant: layout.variant(),
        product,
        product_name,
        // A height above the ground credits the terrain too (S30).
        attribution: if layout.set.rule == Rule::Height && layout.set.above == Some(Above::Ground) {
            format!("{}; {}", owners.join("; "), crate::terrain::CREDIT)
        } else {
            owners.join("; ")
        },
        centre: layout.centre(),
        legend,
        scale,
        offset,
    }
}

fn utc(ms: i64, format: &str) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|t| t.format(format).to_string())
        .unwrap_or_default()
}

/// The frame for a built mosaic: a grid frame like a composite's
/// (`composite::frame`), named by the set and credited to its owners.
pub fn frame(template: &Frame, station: &Station, built: &Built) -> Frame {
    let grid = &built.grid;
    Frame {
        id: format!(
            "{}-{}-{}",
            station.id,
            utc(grid.start_ms, "%Y%m%dT%H%M%SZ"),
            built.variant
        ),
        kind: FrameKind::Grid,
        // A height slice is `CAPPI` (S30), so clients hatch its holes.
        product: if built.product == crate::products::REF {
            template.product.clone()
        } else {
            built.product.to_owned()
        },
        product_name: built.product_name.clone(),
        // S24b: a composite's storm height and rain mass bring their own.
        units: built
            .legend
            .map_or_else(|| template.units.clone(), |l| l.units.to_owned()),
        elevation_deg: 0.0,
        scan_time: utc(grid.start_ms, "%Y-%m-%dT%H:%M:%SZ"),
        sweep_end: utc(grid.end_ms, "%Y-%m-%dT%H:%M:%SZ"),
        status: FrameStatus::Complete,
        texture: String::new(),
        azimuth_lut: String::new(),
        rays: 0,
        gates: 0,
        first_gate_m: 0,
        gate_spacing_m: 0,
        scale: built.scale,
        offset: built.offset,
        site: Geometry {
            lat: built.centre.0,
            lon: built.centre.1,
            alt_m: 0.0,
        },
        palette: built.legend.map_or_else(
            || template.palette.clone(),
            |l| l.palette.iter().map(|&c| c.to_owned()).collect(),
        ),
        bounds: built
            .legend
            .map_or_else(|| template.bounds.clone(), |l| l.bounds.to_vec()),
        attribution: built.attribution.clone(),
        grid: Some(grid.placement()),
    }
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// The nominal time a scan starting at `start_ms` belongs to, by the
/// pollers' own rule (`smhi_live::covered`: a start from 1 minute before
/// the valid time to 4 minutes after it): the 5-minute mark at or before
/// `start + 1 min`.
pub fn nominal_ms(start_ms: i64) -> i64 {
    (start_ms + 60_000).div_euclid(CADENCE_MS) * CADENCE_MS
}

/// The newest frame time due at `now`: T + `DUE_MS` has passed.
pub fn newest_due(now: i64) -> i64 {
    (now - DUE_MS).div_euclid(CADENCE_MS) * CADENCE_MS
}

/// What one scan cost to read.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Cost {
    pub requests: u64,
    pub bytes: u64,
    pub total: u64,
    pub from_store: bool,
}

impl Cost {
    /// Two reads of one volume together (S30: a product and its free
    /// lowest scan). From the store when neither made a request and either
    /// came from it: the free scan's provenance reads as 0 requests, not as
    /// the store's (review #5).
    pub fn plus(self, other: Cost) -> Cost {
        let requests = self.requests + other.requests;
        Cost {
            requests,
            bytes: self.bytes + other.bytes,
            total: self.total.max(other.total),
            from_store: requests == 0 && (self.from_store || other.from_store),
        }
    }

    /// A poller's provenance: `…: N range requests, B of T bytes`, or the
    /// tilt store's.
    pub fn of(provenance: &str) -> Cost {
        if provenance.ends_with(crate::tilts::FROM_STORE) {
            return Cost {
                from_store: true,
                ..Cost::default()
            };
        }
        let tail = provenance.rsplit_once(": ").map_or(provenance, |(_, t)| t);
        let words: Vec<&str> = tail.split_whitespace().collect();
        let num = |i: usize| words.get(i).and_then(|w| w.parse().ok()).unwrap_or(0);
        // "N range requests, B of T bytes"
        Cost {
            requests: num(0),
            bytes: num(3),
            total: num(5),
            from_store: false,
        }
    }
}

/// The far edge of a scan's last gate, metres of slant range.
fn edge_m(s: &Sweep) -> f64 {
    f64::from(s.first_gate_m) + f64::from(s.gates) * f64::from(s.gate_spacing_m)
}

/// One radar's scan at one time.
#[derive(Clone)]
pub struct Have {
    pub sweep: Arc<Sweep>,
    pub cost: Cost,
}

/// A frame time's inputs, as `build` takes them owned.
type Owned = Vec<Option<(Arc<Sweep>, Option<(Arc<Sweep>, i64)>)>>;

/// The scans in hand and the frames built, by nominal time, and the rule
/// that says which frame to build when.
pub struct Schedule {
    /// The chosen radars' full ranges (their `rangeKm`), metres of slant.
    ranges: Vec<f64>,
    scans: BTreeMap<i64, Vec<Option<Have>>>,
    built: BTreeSet<i64>,
    /// Built frame times a late scan has come for: to build once more.
    late: BTreeSet<i64>,
    /// Frame times built that second time; never a third.
    rebuilt: BTreeSet<i64>,
    /// When a scan last arrived (or the tilt store gave some); `None`
    /// before any: the quiet path needs one (review #1).
    last_arrival: Option<i64>,
    /// Frame times a selection builds back (`Set::backfill`).
    depth: usize,
    /// S31: radars their poller calls silent (nothing published for the
    /// provider's `unavailable` age): no frame time waits for them, and
    /// the loading progress leaves them out.
    silent: Vec<bool>,
    /// S31: frame times given up on (`forget`), done as far as the fill's
    /// progress is concerned.
    dropped: BTreeSet<i64>,
}

/// S31: how far a fill has come (`Schedule::fill`): volumes `done` of
/// `total` for `stage`, over frame time `t`, with the words' numbers.
#[derive(Clone, PartialEq, Debug)]
pub struct Fill {
    pub stage: crate::loading::Stage,
    pub done: u32,
    pub total: u32,
    pub t: i64,
    /// Counted radars with `t`'s scan (the first stage).
    pub radars_in: usize,
    pub counted: usize,
    pub silent: usize,
    /// Frame times of the window built, and the window's length.
    pub built: usize,
    pub window: usize,
}

impl Fill {
    /// `state.loading`'s report for the load named `name`: `Nordic Rain
    /// mass: 26 of 40 radars in for 14:25Z (1 silent)`, or `… history 1
    /// of 3 frames`.
    pub fn progress(&self, name: &str) -> crate::loading::Progress {
        let silent = if self.silent > 0 {
            format!(" ({} silent)", self.silent)
        } else {
            String::new()
        };
        let label = match self.stage {
            crate::loading::Stage::First => format!(
                "{name}: {} of {} radars in for {}{silent}",
                self.radars_in,
                self.counted,
                utc(self.t, "%H:%MZ")
            ),
            crate::loading::Stage::History => format!(
                "{name}: history {} of {} frames{silent}",
                self.built, self.window
            ),
        };
        crate::loading::Progress {
            stage: self.stage,
            done: self.done,
            total: self.total,
            label,
        }
    }
}

impl Schedule {
    /// `ranges`: each radar's full range in metres; `cached`: frame times
    /// this set's catalog already holds. Builds back `BACKFILL` frame
    /// times; `with_depth` for another depth.
    pub fn new(ranges: Vec<f64>, cached: &[i64]) -> Schedule {
        Schedule {
            silent: vec![false; ranges.len()],
            dropped: BTreeSet::new(),
            ranges,
            scans: BTreeMap::new(),
            built: cached.iter().copied().collect(),
            late: BTreeSet::new(),
            rebuilt: BTreeSet::new(),
            last_arrival: None,
            depth: BACKFILL,
        }
    }

    /// The same, building back `depth` frame times (S30: a height set's).
    pub fn with_depth(mut self, depth: usize) -> Schedule {
        self.depth = depth.max(1);
        self
    }

    /// The oldest frame time the selection builds.
    fn oldest(&self, now: i64) -> i64 {
        newest_due(now) - (self.depth as i64 - 1) * CADENCE_MS
    }

    /// The oldest scan time worth keeping: the frames still to build, and
    /// the far rings they may take from older scans.
    pub fn floor(&self, now: i64) -> i64 {
        let due = newest_due(now);
        let pending = (self.oldest(now)..=due)
            .step_by(CADENCE_MS as usize)
            .find(|t| !self.built.contains(t))
            .unwrap_or(due);
        pending - OUTER_MS
    }

    /// Radar `radar`'s scan at nominal `t`; `arrived` counts it as news (a
    /// poller's event; the tilt store's start-up load is `touch`). An older
    /// scan than the schedule keeps is dropped. A scan a built frame went
    /// without marks that frame for one more build (the decided timing
    /// rule), while it is one of the newest two frame times and younger
    /// than `LATE_MS`; never a third.
    pub fn add(&mut self, radar: usize, t: i64, have: Have, now: i64, arrived: bool) {
        if t < self.floor(now) || radar >= self.ranges.len() {
            return;
        }
        let n = self.ranges.len();
        let slot = &mut self.scans.entry(t).or_insert_with(|| vec![None; n])[radar];
        let new = slot.is_none();
        *slot = Some(have);
        if !arrived {
            return;
        }
        self.last_arrival = Some(now);
        let recent = self
            .built
            .last()
            .is_some_and(|&newest| t >= newest - CADENCE_MS);
        if new
            && recent
            && now < t + LATE_MS
            && self.built.contains(&t)
            && !self.rebuilt.contains(&t)
        {
            self.late.insert(t);
        }
    }

    /// Scans are in hand from the tilt store: the quiet clock starts.
    pub fn touch(&mut self, now: i64) {
        self.last_arrival.get_or_insert(now);
    }

    /// Radar `radar`'s scan at `t` came again (S30: a height set's volume
    /// arrives as its product and its free lowest scan): its cost counts
    /// too, and it is an arrival.
    pub fn add_cost(&mut self, radar: usize, t: i64, cost: Cost, now: i64) {
        let held = self
            .scans
            .get_mut(&t)
            .and_then(|v| v.get_mut(radar))
            .and_then(Option::as_mut);
        if let Some(have) = held {
            have.cost = have.cost.plus(cost);
            self.last_arrival = Some(now);
        }
    }

    /// Whether radar `radar` has a scan at `t`.
    pub fn has(&self, radar: usize, t: i64) -> bool {
        self.scans
            .get(&t)
            .is_some_and(|v| v.get(radar).is_some_and(Option::is_some))
    }

    /// The nominal times radar `radar`'s poller need not fetch: the scans
    /// in hand and the frames already built.
    pub fn known(&self, radar: usize) -> Vec<i64> {
        self.scans
            .iter()
            .filter(|(_, v)| v[radar].is_some())
            .map(|(t, _)| *t)
            .chain(self.built.iter().copied())
            .collect()
    }

    /// Whether every radar's scan for `t` is in and ready to build with
    /// (`settled`).
    /// A radar its poller calls silent is not waited for (S31); a time with
    /// no scan at all is never complete.
    fn complete(&self, t: i64) -> bool {
        self.scans.get(&t).is_some_and(|v| {
            v.iter().any(Option::is_some)
                && v.iter().enumerate().all(|(radar, have)| {
                    have.as_ref()
                        .map_or(self.silent[radar], |h| self.settled(radar, t, &h.sweep))
                })
        })
    }

    /// S31: radar `radar`'s poller called it silent, or it delivered a scan
    /// the schedule keeps again.
    pub fn set_silent(&mut self, radar: usize, silent: bool) {
        if let Some(s) = self.silent.get_mut(radar) {
            *s = silent;
        }
    }

    /// S31: how far the fill has come, or `None` once every frame time of
    /// the window (`oldest` to the newest due) is built or given up.
    ///
    /// Until the first of them is built it is the `first` stage: over the
    /// time closest to completion (newest on a tie; the window's and any
    /// later time with a scan), per counted radar the volumes it delivers
    /// down to that time's own, newest first as its poller fetches them,
    /// from its newest scan in hand (else one cadence past the newest due,
    /// where a poller's newest usually is); a radar with that time's scan
    /// is done. Then `history`: the window's times, a built one done for
    /// every counted radar, an unbuilt one per scan in hand.
    pub fn fill(&self, now: i64) -> Option<Fill> {
        let due = newest_due(now);
        let oldest = self.oldest(now);
        let window: Vec<i64> = (oldest..=due).step_by(CADENCE_MS as usize).collect();
        let counted: Vec<usize> = (0..self.ranges.len())
            .filter(|r| !self.silent[*r])
            .collect();
        let silent = self.ranges.len() - counted.len();
        let finished = |t: &i64| self.built.contains(t) || self.dropped.contains(t);
        let n = counted.len() as u32;
        if self.built.range(oldest..).next().is_some() {
            let open: Vec<i64> = window.iter().copied().filter(|t| !finished(t)).collect();
            if open.is_empty() {
                return None;
            }
            let built = (window.len() - open.len()) as u32;
            let held: u32 = open
                .iter()
                .map(|&t| counted.iter().filter(|&&r| self.has(r, t)).count() as u32)
                .sum();
            return Some(Fill {
                stage: crate::loading::Stage::History,
                done: built * n + held,
                total: window.len() as u32 * n,
                t: open[open.len() - 1],
                radars_in: 0,
                counted: counted.len(),
                silent,
                built: built as usize,
                window: window.len(),
            });
        }
        let newest_in_hand = |r: usize| {
            self.scans
                .iter()
                .rev()
                .find(|(_, v)| v[r].is_some())
                .map(|(t, _)| *t)
        };
        let later = self.scans.keys().copied().filter(|t| *t > due);
        let mut best: Option<(u32, u32, i64)> = None;
        for t in window.iter().copied().chain(later).filter(|t| !finished(t)) {
            let (mut done, mut total) = (0u32, 0u32);
            for &r in &counted {
                let top = newest_in_hand(r)
                    .filter(|n| *n >= t)
                    .unwrap_or((due + CADENCE_MS).max(t));
                let need = ((top - t) / CADENCE_MS) as u32 + 1;
                total += need;
                done += if self.has(r, t) {
                    need
                } else if top > t {
                    self.scans
                        .range(t + 1..=top)
                        .filter(|(_, v)| v[r].is_some())
                        .count() as u32
                } else {
                    0
                };
            }
            // Closest to completion; the newer on a tie.
            let closer = best.is_none_or(|(d, tot, _)| {
                u64::from(done) * u64::from(tot.max(1)) >= u64::from(d) * u64::from(total.max(1))
            });
            if closer {
                best = Some((done, total, t));
            }
        }
        let (done, total, t) = best?;
        Some(Fill {
            stage: crate::loading::Stage::First,
            done,
            total,
            t,
            radars_in: counted.iter().filter(|&&r| self.has(r, t)).count(),
            counted: counted.len(),
            silent,
            built: 0,
            window: window.len(),
        })
    }

    /// Whether radar `radar`'s scan at `t` is ready to build with: one
    /// that reaches its full range; a short one with the longer scan its
    /// far ring comes from; or a short one once an older scan of the radar
    /// is in hand and none of its scans in hand is longer (a radar that
    /// always scans short, review #3). A short scan with no older one yet
    /// waits: a backfill delivers newest first, and a long predecessor may
    /// be on its way.
    fn settled(&self, radar: usize, t: i64, sweep: &Sweep) -> bool {
        if !self.short(radar, sweep) || self.outer(radar, t, sweep).is_some() {
            return true;
        }
        let reach = edge_m(sweep);
        let older = self
            .scans
            .range(t - OUTER_MS..t)
            .any(|(_, v)| v[radar].is_some());
        let longer = self.scans.values().any(|v| {
            v[radar]
                .as_ref()
                .is_some_and(|o| edge_m(&o.sweep) > reach * 1.25)
        });
        older && !longer
    }

    /// A scan that reaches less than `SHORT` of its radar's full range.
    fn short(&self, radar: usize, sweep: &Sweep) -> bool {
        edge_m(sweep) < SHORT * self.ranges[radar]
    }

    /// For radar `radar`'s scan `sweep` at `t`: its latest clearly longer
    /// scan up to `OUTER_MS` older, with that scan's time.
    fn outer(&self, radar: usize, t: i64, sweep: &Sweep) -> Option<(Arc<Sweep>, i64)> {
        let reach = edge_m(sweep);
        self.scans
            .range(t - OUTER_MS..t)
            .rev()
            .find_map(|(when, v)| {
                v[radar]
                    .as_ref()
                    .filter(|o| edge_m(&o.sweep) > reach * 1.25)
                    .map(|o| (o.sweep.clone(), *when))
            })
    }

    /// The frame times to build now, newest first: each of the last
    /// `BACKFILL` due times not built whose scans are all in, or, once no
    /// scan has arrived for `QUIET_MS`, of which at least one scan is in
    /// (never one with none, review #1); any later time whose scans are
    /// all in; and a built time a late scan came for, once all are in or
    /// quiet.
    pub fn ready(&self, now: i64) -> Vec<i64> {
        let due = newest_due(now);
        let quiet = self.last_arrival.is_some_and(|at| now - at >= QUIET_MS);
        let held = |t: i64| {
            self.scans
                .get(&t)
                .is_some_and(|v| v.iter().any(Option::is_some))
        };
        let mut ready: Vec<i64> = (self.oldest(now)..=due)
            .step_by(CADENCE_MS as usize)
            .filter(|&t| !self.built.contains(&t) && (self.complete(t) || (quiet && held(t))))
            .collect();
        ready.extend(
            self.scans
                .keys()
                .copied()
                .filter(|t| *t > due && !self.built.contains(t) && self.complete(*t)),
        );
        ready.extend(
            self.late
                .iter()
                .copied()
                .filter(|&t| quiet || self.complete(t)),
        );
        ready.sort_unstable_by(|a, b| b.cmp(a));
        ready.dedup();
        ready
    }

    /// Whether every due frame is built (the newest due included).
    pub fn caught_up(&self, now: i64) -> bool {
        self.built.contains(&newest_due(now))
    }

    pub fn is_built(&self, t: i64) -> bool {
        self.built.contains(&t)
    }

    /// Whether `t` is to be built again for a late scan.
    pub fn is_late(&self, t: i64) -> bool {
        self.late.contains(&t)
    }

    /// Give up on frame time `t` (S30: the tilt store holds none of its
    /// volumes): its scans go, so it is not ready again.
    pub fn forget(&mut self, t: i64) {
        self.scans.remove(&t);
        self.late.remove(&t);
        self.dropped.insert(t);
    }

    /// `t` was built and sent: the first time, or its one late rebuild.
    pub fn mark_built(&mut self, t: i64) {
        if self.late.remove(&t) {
            self.rebuilt.insert(t);
        }
        self.built.insert(t);
    }

    /// The newest frame time built.
    pub fn newest_built(&self) -> Option<i64> {
        self.built.last().copied()
    }

    /// Each radar's scans for `t`: its scan at `t`, and when that scan
    /// reaches less than `SHORT` of its full range, its latest clearly
    /// longer scan up to `OUTER_MS` older with that scan's time; plus each
    /// radar's cost at `t`.
    pub fn inputs(&self, t: i64) -> (Owned, Vec<Option<Cost>>) {
        let n = self.ranges.len();
        let mut owned = Vec::with_capacity(n);
        let mut costs = Vec::with_capacity(n);
        for radar in 0..n {
            let Some(have) = self.scans.get(&t).and_then(|v| v[radar].as_ref()) else {
                owned.push(None);
                costs.push(None);
                continue;
            };
            let outer = self
                .short(radar, &have.sweep)
                .then(|| self.outer(radar, t, &have.sweep))
                .flatten();
            owned.push(Some((have.sweep.clone(), outer)));
            costs.push(Some(have.cost));
        }
        (owned, costs)
    }

    /// Forget scans older than the schedule keeps.
    pub fn prune(&mut self, now: i64) {
        let floor = self.floor(now);
        self.scans.retain(|t, _| *t >= floor);
        self.late.retain(|t| *t >= floor);
        self.rebuilt.retain(|t| *t >= floor);
    }
}

/// A frame's provenance: the set, the radars used, what they cost, the
/// missing ones and the far rings; one `N range requests, B of T bytes` at
/// the end, or the tilt store's words when no scan cost a request, as
/// `scripts/soak-report.sh` reads them.
/// `unread` names radars whose volume the tilt store no longer had when a
/// height frame was built (S30): missing too.
fn provenance(
    layout: &Layout,
    t_ms: i64,
    owned: &Owned,
    costs: &[Option<Cost>],
    unread: &[String],
) -> String {
    let mut parts = Vec::new();
    let mut missing = Vec::new();
    let (mut requests, mut bytes, mut total, mut fetched) = (0, 0, 0, false);
    for ((radar, input), cost) in layout.radars.iter().zip(owned).zip(costs) {
        let id = radar.station.id.as_str();
        match (input, cost) {
            (Some(_), Some(_)) if unread.iter().any(|u| u == id) => missing.push(id),
            (Some((_, outer)), Some(cost)) => {
                let mut part = id.to_owned();
                if cost.from_store {
                    part.push_str(" stored");
                } else {
                    fetched = true;
                    requests += cost.requests;
                    bytes += cost.bytes;
                    total += cost.total;
                }
                if let Some((_, when)) = outer {
                    part.push_str(&format!(" (far ring {})", utc(*when, "%H:%MZ")));
                }
                parts.push(part);
            }
            _ => missing.push(id),
        }
    }
    let mut text = format!(
        "{} {} {}, {} of {} radars at {}: {}",
        layout.name(),
        layout.variant(),
        layout.product().1,
        parts.len(),
        layout.radars.len(),
        utc(t_ms, "%H:%MZ"),
        parts.join(", ")
    );
    if !missing.is_empty() {
        text.push_str(&format!("; missing {}", missing.join(", ")));
    }
    if fetched {
        text.push_str(&format!(
            ": {requests} range requests, {bytes} of {total} bytes"
        ));
    } else {
        text.push_str(&format!(": {}", crate::tilts::FROM_STORE));
    }
    text
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn log(message: impl std::fmt::Display) {
    log_for(STATION, message);
}

/// An engine.log line for the mosaic of `site`: `mymosaic`, or a composite
/// whose product is being made (S24b).
fn log_for(site: &str, message: impl std::fmt::Display) {
    eprintln!(
        "{} Mosaic {site}: {message}",
        utc(now_ms(), "%Y-%m-%dT%H:%M:%SZ")
    );
}

/// A scan the schedule keeps without its rays (S24b): a composite's frame
/// reads its volumes back from the tilt store when it is built, so only a
/// scan's geometry and times are needed until then (41 radars' lowest scans
/// over 13 frame times would hold about 100 MB).
fn geometry(sweep: Sweep) -> Sweep {
    Sweep {
        rays: Vec::new(),
        ..sweep
    }
}

/// Each radar's stored lowest scans from `floor` on: the tilt store's
/// copies (S27), no request. Built frames' times too (review #2): a newer
/// frame's far ring may come from them. When a radar's poller reads more
/// than its lowest scan (`Layout::read_want`: a height set's whole volumes,
/// S30; a composite product's, S24b), only volumes the store holds every
/// scan of that read; its poller reads the rest. A composite's are kept as
/// their geometry alone (`geometry`).
fn from_store(layout: &Layout, floor: i64) -> Vec<(usize, i64, Sweep)> {
    let Some(store) = crate::tilts::shared() else {
        return Vec::new();
    };
    let grid = matches!(layout.job, Job::Grid { .. });
    let mut out = Vec::new();
    for (index, radar) in layout.radars.iter().enumerate() {
        let id = &radar.station.id;
        let volumes = store.volumes(id).unwrap_or_default();
        let want = layout.read_want(&radar.station);
        for (t, source) in volumes.into_iter().filter(|(t, _)| *t >= floor) {
            if !want.is_lowest() && !stored_has(&store, id, t, want) {
                continue;
            }
            let slot = crate::tilts::Slot {
                store: store.clone(),
                station: id.clone(),
                time_ms: t,
                source,
            };
            if let Ok(Some(Scan::Polar(sweep))) = slot.compose(Want::Lowest, radar.which) {
                out.push((index, t, if grid { geometry(sweep) } else { sweep }));
            }
        }
    }
    out
}

/// Whether the tilt store holds every scan `want` reads of `station`'s
/// volume at `t` (S30: a height frame reads them all; S24b: `LOWB` the
/// scans of its clear beams).
fn stored_has(store: &crate::tilts::Store, station: &str, t: i64, want: Want) -> bool {
    let Ok(Some(volume)) = store.volume(station, t) else {
        return false;
    };
    let Some(angles) = &volume.angles else {
        return false;
    };
    crate::products::needed(want, angles)
        .iter()
        .all(|k| volume.tilts.iter().any(|s| s.dataset == *k))
}

/// `station`'s volume at `t` from the tilt store, its scans ascending by
/// angle (in the decoder's order on a tie, as `products::compose` sorts
/// them); `None` when the store has none of it.
fn volume_of(store: &crate::tilts::Store, station: &str, t: i64) -> Option<Vec<Tilt>> {
    let mut tilts: Vec<Tilt> = store
        .tilts(station, t)
        .ok()?
        .into_iter()
        .map(|(_, tilt)| tilt)
        .collect();
    if tilts.is_empty() {
        return None;
    }
    tilts.sort_by(|a, b| a.elangle.total_cmp(&b.elangle));
    Some(tilts)
}

/// A height frame's volumes (S30): each radar's at `t`, and its far ring's
/// when `owned` names one, read back from the tilt store.
type Loaded = Vec<Option<(Vec<Tilt>, Option<Vec<Tilt>>)>>;

fn load_volumes(
    layout: &Layout,
    t: i64,
    owned: &Owned,
    store: Option<&crate::tilts::Store>,
) -> Loaded {
    layout
        .radars
        .iter()
        .zip(owned)
        .map(|(radar, input)| {
            let (_, outer) = input.as_ref()?;
            let store = store?;
            let id = &radar.station.id;
            let primary = volume_of(store, id, t)?;
            let outer = outer
                .as_ref()
                .and_then(|(_, when)| volume_of(store, id, *when));
            Some((primary, outer))
        })
        .collect()
}

/// The height frame for `t` (S30) from the volumes `store` holds of each
/// radar's `owned` scans, with the radars whose volume it lacks; `None`
/// when it holds none of them (review #6: never an empty frame).
fn assemble_height(
    layout: &Layout,
    t: i64,
    owned: &Owned,
    store: Option<&crate::tilts::Store>,
) -> (Option<Built>, Vec<String>) {
    let loaded = load_volumes(layout, t, owned, store);
    let unread: Vec<String> = layout
        .radars
        .iter()
        .zip(owned.iter().zip(&loaded))
        .filter(|(_, (input, volume))| input.is_some() && volume.is_none())
        .map(|(radar, _)| radar.station.id.clone())
        .collect();
    if loaded.iter().all(Option::is_none) {
        return (None, unread);
    }
    let inputs: Vec<Option<Volumes>> = loaded
        .iter()
        .map(|v| {
            v.as_ref().map(|(primary, outer)| Volumes {
                primary,
                outer: outer.as_deref(),
            })
        })
        .collect();
    (Some(build_height(layout, t, &inputs)), unread)
}

/// A composite's frame for `t` (S24b) from the volumes `store` holds of
/// each radar's `owned` scans, read back radar by radar, with the radars
/// whose volume it lacks; `None` when it holds none of them.
fn assemble_grid(
    layout: &Layout,
    t: i64,
    owned: &Owned,
    store: Option<&crate::tilts::Store>,
) -> (Option<Built>, Vec<String>) {
    let mut unread = Vec::new();
    let combined = combine_grid(layout, |i| {
        let (_, outer) = owned.get(i)?.as_ref()?;
        let id = &layout.radars[i].station.id;
        let Some(primary) = store.and_then(|s| volume_of(s, id, t)) else {
            unread.push(id.clone());
            return None;
        };
        let outer = outer
            .as_ref()
            .and_then(|(_, when)| store.and_then(|s| volume_of(s, id, *when)));
        Some((primary, outer))
    });
    if combined.ends.iter().all(Option::is_none) {
        return (None, unread);
    }
    (
        Some(built(layout, t, combined.codes, &combined.ends)),
        unread,
    )
}

/// Build the frame for `t` on the blocking pool and send it: as the live
/// frame when it is the newest built, else as history. A late rebuild keeps
/// its id, so the catalog and every client replace it. `t` is marked built
/// only once sent (review #8), and a time with no scan at all is never
/// built (review #1). A height frame (S30) reads its volumes back from the
/// tilt store here, and holds them only while it is built.
/// Whether a composite's frame from `have` of its radars is left unbuilt
/// (review S5): a history frame (not the newest) under a third of them.
/// My mosaic's, and the newest frame, are always built.
fn too_sparse(layout: &Layout, have: usize, newest: bool) -> bool {
    matches!(layout.job, Job::Grid { .. }) && !newest && have * 3 < layout.radars.len()
}

async fn build_and_send(
    layout: &Arc<Layout>,
    schedule: &mut Schedule,
    t: i64,
    events: &Sender<Event>,
) -> bool {
    let (owned, costs) = schedule.inputs(t);
    if owned.iter().all(Option::is_none) {
        return true;
    }
    let again = schedule.is_late(t);
    let newest = schedule.newest_built().is_none_or(|n| t >= n);
    let have = owned.iter().filter(|o| o.is_some()).count();
    if too_sparse(layout, have, newest) {
        // Review S5: it would stay in the loop for good (only the newest two
        // frame times are built again); marked done, so nothing reads it.
        log_for(
            layout.station_id(),
            format_args!(
                "{} not built: {have} of {} radars, under a third, for a history frame",
                utc(t, "%Y-%m-%dT%H:%MZ"),
                layout.radars.len()
            ),
        );
        schedule.mark_built(t);
        return true;
    }
    let shared = layout.clone();
    let started = Instant::now();
    let site = layout.station_id().to_owned();
    let built = spawn_blocking(move || {
        if let Job::Grid { .. } = shared.job {
            // S24b: every composite product is made from the tilt store,
            // radar by radar.
            let store = crate::tilts::shared();
            let (built, unread) = assemble_grid(&shared, t, &owned, store.as_deref());
            (built, owned, unread)
        } else if shared.set.rule == Rule::Height {
            let store = crate::tilts::shared();
            let (built, unread) = assemble_height(&shared, t, &owned, store.as_deref());
            (built, owned, unread)
        } else {
            let inputs: Vec<Option<Input>> = owned
                .iter()
                .map(|o| {
                    o.as_ref().map(|(p, outer)| Input {
                        primary: p,
                        outer: outer.as_ref().map(|(s, _)| s.as_ref()),
                    })
                })
                .collect();
            let built = build(&shared, t, &inputs);
            (Some(built), owned, Vec::new())
        }
    })
    .await;
    let Ok((built, owned, unread)) = built else {
        log_for(&site, format_args!("building {} failed", utc(t, "%H:%MZ")));
        return true;
    };
    let Some(built) = built else {
        // Review #6: every volume gone from the store; nothing is sent, and
        // the time is given up rather than tried every second.
        log_for(
            &site,
            format_args!(
                "{} built with no radar: the tilt store holds none of its volumes ({}); not sent",
                utc(t, "%Y-%m-%dT%H:%MZ"),
                unread.join(", ")
            ),
        );
        schedule.forget(t);
        return true;
    };
    let provenance = provenance(layout, t, &owned, &costs, &unread);
    if !unread.is_empty() {
        log_for(
            &site,
            format_args!(
                "{}: the tilt store no longer holds {}'s volume",
                utc(t, "%H:%MZ"),
                unread.join(", ")
            ),
        );
    }
    log_for(
        &site,
        format_args!(
            "{} built{} in {:.2?}, {} × {}; {provenance}",
            utc(t, "%Y-%m-%dT%H:%MZ"),
            if again {
                " again, for a late scan,"
            } else {
                ""
            },
            started.elapsed(),
            built.grid.width,
            built.grid.height
        ),
    );
    let sweep = Scan::Mosaic(Box::new(built));
    let event = if newest {
        Event::Sweep {
            site,
            sweep,
            complete: true,
            provenance,
        }
    } else {
        Event::Backfill {
            site,
            sweep,
            provenance,
        }
    };
    if events.send(event).await.is_err() {
        return false;
    }
    schedule.mark_built(t);
    true
}

/// Poll `set`'s radars and build My mosaic's frames until the task is
/// aborted (`main.rs` does, on a switch or a new set) or `events` closes.
/// `cached` holds the frame times this set's catalog already has. With no
/// radars chosen it waits, polling nothing.
pub async fn poll(set: Set, sites: Vec<Station>, events: Sender<Event>, cached: Vec<i64>) {
    if set.is_empty() {
        return std::future::pending().await;
    }
    let place = move || Layout::new(&set, &sites);
    run(
        STATION.to_owned(),
        "My mosaic".into(),
        place,
        events,
        cached,
    )
    .await;
}

/// A composite's product (S24b): `main.rs` starts it while `station`
/// (`sweden`, `nordic`) is selected showing `want`, and aborts it on any
/// switch. Its radars are polled and its frames built as My mosaic's, from
/// the tilt store; the frames are the composite's.
pub async fn poll_grid(
    station: Station,
    want: Want,
    sites: Vec<Station>,
    events: Sender<Event>,
    cached: Vec<i64>,
) {
    let (site, name) = (station.id.clone(), station.name.clone());
    let place = move || Layout::grid(&station, want, &sites);
    run(site, name, place, events, cached).await;
}

/// Place the radars, then poll them and build the frames of `site` (named
/// `name` in words) until aborted.
async fn run(
    site: String,
    name: String,
    place: impl FnOnce() -> Result<Layout, String> + Send + 'static,
    events: Sender<Event>,
    cached: Vec<i64>,
) {
    let started = Instant::now();
    let placed = spawn_blocking(place)
        .await
        .map_err(|e| e.to_string())
        .and_then(|placed| placed);
    let layout = match placed {
        Ok(layout) => Arc::new(layout),
        Err(e) => {
            // Said to the clients (review #7), once; then the task waits,
            // so the engine does not start it again every second.
            let reason = format!("{name} cannot place its radars: {e}");
            log_for(&site, &reason);
            let _ = events.send(Event::Offline { site, reason }).await;
            return std::future::pending().await;
        }
    };
    log_for(
        &site,
        format_args!(
            "{} {}: {} radars on {} × {} texels, placed in {:.0?}",
            layout.variant(),
            layout.product().1,
            layout.radars.len(),
            layout.width,
            layout.height,
            started.elapsed()
        ),
    );
    // A height set's frames (S30) and a composite product's (S24b) are made
    // from the tilt store.
    let height = layout.stored();
    let grid = matches!(layout.job, Job::Grid { .. });
    if height && crate::tilts::shared().is_none() {
        let reason = format!(
            "{name}'s {} is made from the tilt store, which is off (OMASTORM_TILTS_MB=0)",
            layout.product().1
        );
        log_for(&site, &reason);
        let _ = events.send(Event::Offline { site, reason }).await;
        return std::future::pending().await;
    }
    let ranges = layout
        .radars
        .iter()
        .map(|r| r.station.range_km * 1000.0)
        .collect();
    let now = now_ms();
    let depth = layout.backfill();
    let mut schedule = Schedule::new(ranges, &cached).with_depth(depth);
    // The tilt store first: every stored lowest scan (for a height set,
    // every whole volume) the frames still to build can use, with no
    // request.
    let floor = schedule.floor(now);
    let shared = layout.clone();
    let stored = spawn_blocking(move || from_store(&shared, floor))
        .await
        .unwrap_or_default();
    let from_the_store = stored.len();
    if height {
        // What the first fill should cost (review #8): per radar the volumes
        // of its depth the store lacks; an SMHI whole volume ~39 range
        // requests (its clear beams' scans ~25), an FMI volume a file per angle,
        // any other ORD volume 1 file (`volume_requests`); listings besides.
        let mut held = vec![0usize; layout.radars.len()];
        for (radar, _, _) in &stored {
            held[*radar] += 1;
        }
        let (mut smhi, mut smhi_requests, mut ord, mut ord_files) = (0, 0, 0, 0);
        for (radar, have) in layout.radars.iter().zip(&held) {
            let missing = (depth + 1).saturating_sub(*have);
            let whole = layout.read_want(&radar.station) == Want::ColMax;
            if radar.station.provider == ProviderId::Smhi {
                smhi += missing;
                smhi_requests += missing
                    * if whole {
                        SMHI_VOLUME_REQUESTS
                    } else {
                        SMHI_CLEAR_REQUESTS
                    };
            } else {
                ord += missing;
                ord_files += missing
                    * if whole {
                        volume_requests(&radar.station)
                    } else {
                        1
                    };
            }
        }
        log_for(
            &site,
            format_args!(
                "first fill: about {} requests ({smhi} SMHI volumes in ~{smhi_requests} range \
                 requests, {ord} ORD volumes in {ord_files} files) plus listings; \
                 {from_the_store} volumes already in the tilt store",
                smhi_requests + ord_files
            ),
        );
    }
    for (radar, t, sweep) in stored {
        let have = Have {
            sweep: Arc::new(sweep),
            cost: Cost {
                from_store: true,
                ..Cost::default()
            },
        };
        schedule.add(radar, t, have, now, false);
    }
    if from_the_store > 0 {
        schedule.touch(now);
    }
    log_for(
        &site,
        format_args!(
            "{from_the_store} {} from the tilt store; {} frames catalogued; building back {depth}",
            if height { "volumes" } else { "lowest scans" },
            cached.len()
        ),
    );
    if schedule.caught_up(now)
        && events
            .send(Event::Current { site: site.clone() })
            .await
            .is_err()
    {
        return;
    }
    // One lowest-scan poller per radar, reporting here, started `STAGGER`
    // apart (review #5).
    let (tx, mut rx) = mpsc::channel::<Event>(64);
    let mut offline = vec![false; layout.radars.len()];
    let _pollers: Vec<crate::smhi_live::AbortOnDrop> = layout
        .radars
        .iter()
        .enumerate()
        .map(|(i, radar)| {
            let (station, tx, known) = (radar.station.clone(), tx.clone(), schedule.known(i));
            // A height set, or a composite's column product, reads each
            // whole volume of a radar whose angles the engine knows (every
            // height needs nearly every scan, so every later height or
            // product is free); for an FMI radar that is all five of a
            // time's per-angle files, read as one volume (S24a). `LOWB`
            // reads its clear beams' scans (S24b). `Layout::read_want`.
            let want = layout.read_want(&station);
            crate::smhi_live::AbortOnDrop(tokio::spawn(async move {
                tokio::time::sleep(STAGGER * i as u32).await;
                // One volume further back than the mosaic, as its newest is
                // usually one cadence ahead of the newest due frame.
                crate::providers::poll_lowest(station, tx, known, depth + 1, want).await;
            }))
        })
        .collect();
    drop(tx);
    // S31: the fill's progress for `state.loading`, at most once a second
    // (a stage change at once), until the window is built.
    let load_name = match &layout.job {
        Job::Mine => format!("My mosaic, {}", layout.product().1),
        Job::Grid { .. } => format!("{} {}", layout.name(), layout.product().1),
    };
    let variant = layout.variant();
    let mut filling = true;
    let mut reported: Option<Option<crate::loading::Progress>> = None;
    let mut reported_at: Option<Instant> = None;
    loop {
        match timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(None) => return log_for(&site, "every radar's poller ended"),
            Ok(Some(event)) => {
                let now = now_ms();
                let index = |site: &str| layout.radars.iter().position(|r| r.station.id == site);
                // A newest volume (not history): how late it came, so the
                // due time can be judged against the providers.
                let live = matches!(event, Event::Sweep { .. });
                match event {
                    Event::Sweep {
                        site,
                        sweep: Scan::Polar(sweep) | Scan::Product(sweep, ..),
                        provenance,
                        ..
                    }
                    | Event::Backfill {
                        site,
                        sweep: Scan::Polar(sweep) | Scan::Product(sweep, ..),
                        provenance,
                    } => {
                        let Some(i) = index(&site) else { continue };
                        offline[i] = false;
                        let t = nominal_ms(sweep.start_ms);
                        // S31: a scan the schedule keeps counts a silent
                        // radar again (not its hours-old last volume).
                        if t >= schedule.floor(now) {
                            schedule.set_silent(i, false);
                        }
                        if height && schedule.has(i, t) {
                            // A height set's volume comes as its product and
                            // the free lowest scan of the same read (S26).
                            schedule.add_cost(i, t, Cost::of(&provenance), now);
                        } else {
                            if live {
                                let late = (now - t) / 1000;
                                log_for(
                                    layout.station_id(),
                                    format_args!(
                                        "{site} {} in at T + {}:{:02}{}",
                                        utc(t, "%H:%MZ"),
                                        late / 60,
                                        late % 60,
                                        if schedule.is_built(t) {
                                            ", after its frame was built"
                                        } else {
                                            ""
                                        }
                                    ),
                                );
                            }
                            // A composite's frame reads its volumes back
                            // from the store: only the geometry is kept.
                            let sweep = if grid { geometry(sweep) } else { sweep };
                            let have = Have {
                                sweep: Arc::new(sweep),
                                cost: Cost::of(&provenance),
                            };
                            schedule.add(i, t, have, now, true);
                        }
                    }
                    Event::Offline {
                        site: radar,
                        reason,
                    } => {
                        log_for(&site, format_args!("{radar}: {reason}"));
                        if let Some(i) = index(&radar) {
                            offline[i] = true;
                        }
                        if offline.iter().all(|o| *o) {
                            let event = Event::Offline {
                                site: site.clone(),
                                reason: format!("every radar is offline ({reason})"),
                            };
                            if events.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                    Event::Silent {
                        site: radar,
                        reason,
                    } => {
                        log_for(&site, format_args!("{radar}: {reason}; not waited for"));
                        // S31: no frame time waits for it, and the
                        // progress leaves it out.
                        if let Some(i) = index(&radar) {
                            schedule.set_silent(i, true);
                        }
                    }
                    _ => {}
                }
            }
            Err(_) => {}
        }
        let now = now_ms();
        for t in schedule.ready(now) {
            if !build_and_send(&layout, &mut schedule, t, &events).await {
                return;
            }
        }
        schedule.prune(now);
        if filling {
            let report = schedule.fill(now_ms()).map(|f| f.progress(&load_name));
            let stage = |r: &Option<crate::loading::Progress>| r.as_ref().map(|p| p.stage);
            let staged = reported.as_ref().map(stage) != Some(stage(&report));
            let due = reported_at.is_none_or(|at| at.elapsed() >= crate::loading::THROTTLE);
            if reported.as_ref() != Some(&report) && (staged || due) {
                filling = report.is_some();
                let event = Event::Progress {
                    site: site.clone(),
                    variant: variant.clone(),
                    progress: report.clone(),
                };
                if events.send(event).await.is_err() {
                    return;
                }
                reported = Some(report);
                reported_at = Some(Instant::now());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sweep::Ray;

    fn table() -> Vec<Station> {
        crate::providers::table().sites
    }

    fn arg(id: &str, reach: Option<f64>) -> SiteArg {
        SiteArg::Site {
            id: id.to_owned(),
            reach_km: reach.map(|r| serde_json::json!(r)),
        }
    }

    #[test]
    fn a_set_is_checked_normalized_and_named() {
        let sites = table();
        let set = choose(
            &sites,
            &[
                SiteArg::Id("sevax".into()),
                arg("nohur", Some(150.04)),
                arg("dksin", Some(900.0)),
            ],
            None,
        )
        .unwrap();
        assert_eq!(set.rule, Rule::Lowest);
        let got: Vec<(&str, f64)> = set
            .sites
            .iter()
            .map(|s| (s.id.as_str(), s.reach_km))
            .collect();
        // An alias names the radar; a reach at or past the range is full.
        assert_eq!(got, [("vara", 240.0), ("nohur", 150.0), ("dksin", 238.0)]);
        // The same set in another order is the same set, and names itself
        // the same; another reach or rule is another set.
        let again = choose(
            &sites,
            &[
                arg("dksin", None),
                SiteArg::Id("vara".into()),
                arg("nohur", Some(150.0)),
            ],
            Some("lowest"),
        )
        .unwrap();
        assert!(set.same(&again));
        assert_eq!(set.variant(), again.variant());
        assert_eq!(
            crate::products::variant_of(&format!("mymosaic-x-{}", set.variant())),
            set.variant()
        );
        let trimmed = choose(
            &sites,
            &[
                SiteArg::Id("vara".into()),
                arg("nohur", Some(125.0)),
                arg("dksin", None),
            ],
            None,
        )
        .unwrap();
        assert!(!set.same(&trimmed));
        assert_ne!(set.variant(), trimmed.variant());
        let strongest = choose(
            &sites,
            &[
                SiteArg::Id("vara".into()),
                arg("nohur", Some(150.0)),
                arg("dksin", None),
            ],
            Some("strongest"),
        )
        .unwrap();
        assert_ne!(set.variant(), strongest.variant());
        // state.mosaic as sent.
        assert_eq!(
            serde_json::to_value(&set).unwrap(),
            serde_json::json!({"sites":[{"id":"vara","reachKm":240.0},{"id":"nohur","reachKm":150.0},
                                        {"id":"dksin","reachKm":238.0}],"rule":"lowest"})
        );
        // Refusals.
        let error = |args: &[SiteArg], rule: Option<&str>| choose(&sites, args, rule).unwrap_err();
        assert!(error(&[], None).contains("at least one"));
        let many: Vec<SiteArg> = sites
            .iter()
            .filter(|s| s.kind == SiteKind::Polar)
            .take(13)
            .map(|s| SiteArg::Id(s.id.clone()))
            .collect();
        assert!(error(&many, None).contains("at most 12"));
        assert!(error(&[SiteArg::Id("ktlx".into())], None).contains("Unknown site"));
        assert!(error(&[SiteArg::Id("sweden".into())], None).contains("composite"));
        assert!(error(&[SiteArg::Id("mymosaic".into())], None).contains("composite"));
        assert!(error(&[SiteArg::Id("frabb".into())], None).contains("Unknown site"));
        assert!(
            error(
                &[SiteArg::Id("vara".into()), SiteArg::Id("sevax".into())],
                None
            )
            .contains("twice")
        );
        assert!(error(&[arg("vara", Some(24.9))], None).contains("minimum"));
        let text = SiteArg::Site {
            id: "vara".into(),
            reach_km: Some(serde_json::json!("far")),
        };
        assert!(error(&[text], None).contains("must be a number"));
        assert!(error(&[SiteArg::Id("vara".into())], Some("mean")).contains("Unknown rule"));
        // Both forms deserialize.
        let parsed: Vec<SiteArg> =
            serde_json::from_str(r#"["vara",{"id":"nohur","reachKm":150},{"id":"dksin"}]"#)
                .unwrap();
        assert_eq!(parsed[0], SiteArg::Id("vara".into()));
        assert_eq!(parsed[2], arg("dksin", None));
    }

    /// A synthetic radar: 360 one-degree rays of `gates` 500 m gates, all
    /// `code`, at `deg` elevation.
    fn sweep(code: u8, gates: u16, deg: f32, start_ms: i64) -> Sweep {
        Sweep {
            rays: (0..360)
                .map(|a| Ray {
                    azimuth_deg: a as f32 + 0.5,
                    elevation_deg: deg,
                    time_ms: start_ms,
                    codes: vec![code; usize::from(gates)],
                })
                .collect(),
            start_ms,
            end_ms: start_ms + 20_000,
            gates,
            first_gate_m: 250,
            gate_spacing_m: 500,
            scale: 2.0,
            offset: 66.0,
            code1_status: crate::sweep::OUTSIDE_COVERAGE,
        }
    }

    fn radar(id: &str, lat: f64, lon: f64, range_km: f64) -> Station {
        Station {
            id: id.to_owned(),
            name: id.to_owned(),
            lat,
            lon,
            alt_m: 100.0,
            kind: SiteKind::Polar,
            range_km,
            attribution: format!("{id} owner"),
            ..Station::default()
        }
    }

    /// Ground distance on the lookup rule's sphere, written independently
    /// of `Layout` (spherical law of cosines through vectors).
    fn ground_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
        let v = |lat: f64, lon: f64| {
            let (la, lo) = (lat.to_radians(), lon.to_radians());
            [la.cos() * lo.cos(), la.cos() * lo.sin(), la.sin()]
        };
        let (a, b) = (v(lat1, lon1), v(lat2, lon2));
        let cross = [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ];
        let sin = (cross[0].powi(2) + cross[1].powi(2) + cross[2].powi(2)).sqrt();
        let cos = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        SPHERE_M * sin.atan2(cos)
    }

    /// Each texel's centre, (lat, lon), from the layout's published edges
    /// (as a client places a grid frame).
    fn texel(layout: &Layout, c: u32, r: u32) -> (f64, f64) {
        let (x0, y0) = (mercator_x(layout.west), mercator_y(layout.north));
        let lon = (x0 + (f64::from(c) + 0.5) * PIXEL_M) / MERCATOR_R;
        let lat = mercator_lat(y0 - (f64::from(r) + 0.5) * PIXEL_M);
        (lat, lon.to_degrees())
    }

    #[test]
    fn two_radars_lowest_beam_picks_the_nearer_and_strongest_the_higher() {
        // A and B 60 km apart on the 58th parallel, same height and angle,
        // each 100 km of data: over the overlap the nearer radar's beam is
        // the lower one.
        let (a, b) = (
            radar("ra", 58.0, 12.0, 100.0),
            radar("rb", 58.0, 13.0, 100.0),
        );
        let sites = vec![a.clone(), b.clone()];
        let set = |rule, reach_a: f64| Set {
            sites: vec![
                SiteReach {
                    id: "ra".into(),
                    reach_km: reach_a,
                },
                SiteReach {
                    id: "rb".into(),
                    reach_km: 100.0,
                },
            ],
            rule,
            ..Set::default()
        };
        let (sa, sb) = (sweep(100, 200, 0.5, 0), sweep(150, 200, 0.5, 0));
        let inputs = [
            Some(Input {
                primary: &sa,
                outer: None,
            }),
            Some(Input {
                primary: &sb,
                outer: None,
            }),
        ];
        let lowest = Layout::new(&set(Rule::Lowest, 100.0), &sites).unwrap();
        let strongest = Layout::new(&set(Rule::Strongest, 100.0), &sites).unwrap();
        let (low, owner) = combine(&lowest, Rule::Lowest, &inputs);
        let (strong, _) = combine(&strongest, Rule::Strongest, &inputs);
        let (mut checked, mut overlap) = (0, 0);
        for r in 0..lowest.height {
            for c in 0..lowest.width {
                let (lat, lon) = texel(&lowest, c, r);
                let (da, db) = (
                    ground_m(lat, lon, a.lat, a.lon),
                    ground_m(lat, lon, b.lat, b.lon),
                );
                let i = (r * lowest.width + c) as usize;
                // A texel's centre is placed to the metre; keep 1 km off
                // every edge the rule turns on.
                let (in_a, in_b) = (da < 99_000.0, db < 99_000.0);
                if (da - 100_000.0).abs() < 1_000.0
                    || (db - 100_000.0).abs() < 1_000.0
                    || (da - db).abs() < 1_000.0
                {
                    continue;
                }
                checked += 1;
                let expected_low = match (in_a, in_b) {
                    (true, true) if da < db => 100,
                    (true, true) => 150,
                    (true, false) => 100,
                    (false, true) => 150,
                    (false, false) => 1,
                };
                assert_eq!(
                    low[i], expected_low,
                    "lowest beam at {lat:.3} {lon:.3} ({da:.0} m, {db:.0} m)"
                );
                assert_eq!(
                    owner[i],
                    match expected_low {
                        100 => 0,
                        150 => 1,
                        _ => u8::MAX,
                    }
                );
                let expected_strong = if in_b {
                    150
                } else if in_a {
                    100
                } else {
                    1
                };
                assert_eq!(strong[i], expected_strong, "strongest at {lat:.3} {lon:.3}");
                overlap += usize::from(in_a && in_b);
            }
        }
        assert!(
            checked > 40_000 && overlap > 5_000,
            "{checked} texels, {overlap} in the overlap"
        );

        // Below threshold from the lower beam wins over rain from a higher
        // one (the Hurum case); no data falls through to the other radar.
        let (dry, gone) = (sweep(0, 200, 0.5, 0), sweep(1, 200, 0.5, 0));
        for (a_sweep, expect) in [(&dry, 0u8), (&gone, 150u8)] {
            let inputs = [
                Some(Input {
                    primary: a_sweep,
                    outer: None,
                }),
                Some(Input {
                    primary: &sb,
                    outer: None,
                }),
            ];
            let (low, _) = combine(&lowest, Rule::Lowest, &inputs);
            let (strong, _) = combine(&strongest, Rule::Strongest, &inputs);
            // Half way from A towards B, 20 km from A: both see it.
            let (lat, lon) = destination(a.lat, a.lon, 90.0, 20_000.0);
            let (c, r) = locate(&lowest, lat, lon);
            let i = (r * lowest.width + c) as usize;
            assert_eq!(low[i], expect);
            assert_eq!(strong[i], 150, "strongest takes the rain");
        }

        // A's reach trimmed to 30 km: its far overlap falls to B, and its
        // far side outside B's reach is no data.
        let trimmed = Layout::new(&set(Rule::Lowest, 30.0), &sites).unwrap();
        let (low, _) = combine(&trimmed, Rule::Lowest, &inputs);
        let at = |bearing: f64, km: f64| {
            let (lat, lon) = destination(a.lat, a.lon, bearing, km * 1000.0);
            let (c, r) = locate(&trimmed, lat, lon);
            low[(r * trimmed.width + c) as usize]
        };
        assert_eq!(at(90.0, 20.0), 100, "inside A's reach, A is nearer");
        assert_eq!(at(90.0, 40.0), 150, "past A's reach, within B's: B");
        assert_eq!(at(330.0, 60.0), 1, "past A's reach, outside B's: no data");
    }

    fn locate(layout: &Layout, lat: f64, lon: f64) -> (u32, u32) {
        let c = ((mercator_x(lon) - mercator_x(layout.west)) / PIXEL_M).floor() as u32;
        let r = ((mercator_y(layout.north) - mercator_y(lat)) / PIXEL_M).floor() as u32;
        (c, r)
    }

    const RAW: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../data/raw/");

    fn lowest_of(fixture: &str, which: Which) -> Sweep {
        let file = std::fs::File::open(format!("{RAW}{fixture}")).unwrap();
        crate::odim::decode_lowest(file, which).unwrap()
    }

    /// The lookup rule for one radar at one point, written from the
    /// protocol text without `Lookup`'s tables: the code and the beam
    /// centre's height above sea level, or `None` off its gates or rays.
    fn sample(station: &Station, sweep: &Sweep, lat: f64, lon: f64) -> Option<(u8, f64, f64)> {
        let s = ground_m(station.lat, station.lon, lat, lon);
        let (p1, p2) = (station.lat.to_radians(), lat.to_radians());
        let dl = (lon - station.lon).to_radians();
        let bearing = (dl.sin() * p2.cos())
            .atan2(p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos())
            .to_degrees()
            .rem_euclid(360.0);
        let e = sweep.elevation_deg().to_radians();
        let theta = s / EARTH_M;
        let r = EARTH_M * theta.sin() / (e + theta).cos();
        let gate = ((r - f64::from(sweep.first_gate_m)) / f64::from(sweep.gate_spacing_m)).round();
        if gate < 0.0 || gate >= f64::from(sweep.gates) {
            return None;
        }
        let entry = (bearing * 10.0).floor() as usize % 3600;
        let row = rows_of(sweep)[entry];
        if row == NONE {
            return None;
        }
        let h = station.alt_m + EARTH_M * e.cos() / (e + theta).cos() - EARTH_M;
        Some((sweep.rays[usize::from(row)].codes[gate as usize], h, s))
    }

    #[test]
    fn s20s_vara_hurum_and_sindal_fixtures_are_placed_on_the_grid() {
        let sites = table();
        let set = choose(
            &sites,
            &[
                SiteArg::Id("vara".into()),
                SiteArg::Id("nohur".into()),
                SiteArg::Id("dksin".into()),
            ],
            None,
        )
        .unwrap();
        let started = Instant::now();
        let layout = Layout::new(&set, &sites).unwrap();
        let placed = started.elapsed();
        let sweeps = [
            lowest_of("radar_vara_qcvol_202609131055_tilts.h5", Which::First),
            lowest_of("ord_nohur_202609140930_tilts.h5", Which::Lowest),
            lowest_of("ord_dksin_202609140940_tilts.h5", Which::Lowest),
        ];
        // Each scan's nominal time, from its first ray.
        let hm = |s: &Sweep| utc(nominal_ms(s.start_ms), "%d %H:%M");
        assert_eq!(
            sweeps.iter().map(hm).collect::<Vec<_>>(),
            ["13 10:55", "14 09:30", "14 09:40"]
        );
        let inputs: Vec<Option<Input>> = sweeps
            .iter()
            .map(|s| {
                Some(Input {
                    primary: s,
                    outer: None,
                })
            })
            .collect();
        let started = Instant::now();
        let (codes, owner) = combine(&layout, Rule::Lowest, &inputs);
        let combined = started.elapsed();
        let (strong, _) = combine(&layout, Rule::Strongest, &inputs);
        eprintln!(
            "vara+nohur+dksin: {} × {} texels, placed in {placed:?}, combined in {combined:?}",
            layout.width, layout.height
        );
        assert!((500..1200).contains(&layout.width) && (500..1400).contains(&layout.height));
        // Every 7th texel against the protocol's rule, computed per point.
        let stations: Vec<&Station> = layout.radars.iter().map(|r| &r.station).collect();
        let (mut checked, mut off, mut measured) = (0, 0, 0);
        for r in (0..layout.height).step_by(7) {
            for c in (0..layout.width).step_by(7) {
                let (lat, lon) = texel(&layout, c, r);
                let candidates: Vec<(usize, u8, f64, f64)> = stations
                    .iter()
                    .zip(&sweeps)
                    .enumerate()
                    .filter_map(|(i, (st, sw))| {
                        let (code, h, s) = sample(st, sw, lat, lon)?;
                        (s <= st.range_km * 1000.0).then_some((i, code, h, s))
                    })
                    .collect();
                let low = candidates
                    .iter()
                    .filter(|c| c.1 != 1)
                    .min_by(|a, b| {
                        a.2.round()
                            .total_cmp(&b.2.round())
                            .then(a.3.total_cmp(&b.3))
                    })
                    .map_or(1, |c| c.1);
                let rank = |c: u8| match c {
                    1 => 0,
                    0 => 1,
                    c => u16::from(c) + 2,
                };
                let high = candidates
                    .iter()
                    .map(|c| c.1)
                    .max_by_key(|&c| rank(c))
                    .unwrap_or(1);
                let i = (r * layout.width + c) as usize;
                checked += 1;
                measured += usize::from(codes[i] >= 2);
                if codes[i] != low || strong[i] != high {
                    off += 1;
                }
            }
        }
        // Ground distances are kept to 10 m, so a point within 5 m of a
        // gate's edge may take its neighbour; nothing else may differ.
        eprintln!("{checked} texels checked, {off} differ, {measured} measured");
        assert!(checked > 8_000, "{checked}");
        assert!(
            off * 1000 <= checked,
            "{off} of {checked} texels differ from the rule"
        );
        // Near each antenna its own beam is the lowest: 25 km out, wherever
        // it has data, it owns the texel.
        for (i, (st, sw)) in stations.iter().zip(&sweeps).enumerate() {
            let mut owned = 0;
            for step in 0..36 {
                let (lat, lon) = destination(st.lat, st.lon, f64::from(step) * 10.0, 25_000.0);
                let (c, r) = locate(&layout, lat, lon);
                let o = owner[(r * layout.width + c) as usize];
                // The texel's centre, not the point, is what was placed.
                let (tl, tn) = texel(&layout, c, r);
                if sample(st, sw, tl, tn).is_some_and(|s| s.0 != 1) {
                    assert_eq!(o, i as u8, "{} at {}°", st.id, step * 10);
                    owned += 1;
                }
            }
            assert!(owned >= 18, "{}: {owned} of 36 with data", st.id);
        }
        // Plan §S25: at the Skagerrak midpoint (57.87 N 11.48 E) Vara and
        // Sindal see about 1.3 km up, Hurum about 4.2 km; Lowest beam never
        // takes Hurum there.
        let heights: Vec<f64> = stations
            .iter()
            .zip(&sweeps)
            .map(|(st, sw)| sample(st, sw, 57.87, 11.48).map_or(f64::NAN, |s| s.1))
            .collect();
        eprintln!("beam heights at the midpoint: {heights:?}");
        assert!(
            (1_000.0..1_700.0).contains(&heights[0]),
            "vara {}",
            heights[0]
        );
        assert!(
            (3_800.0..4_700.0).contains(&heights[1]),
            "hurum {}",
            heights[1]
        );
        assert!(
            (1_000.0..1_700.0).contains(&heights[2]),
            "sindal {}",
            heights[2]
        );
        let (c, r) = locate(&layout, 57.87, 11.48);
        assert_ne!(
            owner[(r * layout.width + c) as usize],
            1,
            "Hurum at the midpoint"
        );
        // Every owner is credited.
        let built = build(&layout, nominal_ms(sweeps[0].start_ms), &inputs);
        assert_eq!(
            built.attribution,
            "SMHI, CC BY 4.0; MET Norway, CC BY 4.0; DMI, CC BY 4.0"
        );
        let template: Frame = serde_json::from_str(include_str!("../data/fixture.json")).unwrap();
        let f = frame(&template, &station(), &built);
        assert_eq!(f.id, format!("mymosaic-20260913T105500Z-{}", set.variant()));
        assert_eq!(
            (f.kind, f.product_name.as_str()),
            (FrameKind::Grid, "Lowest beam")
        );
        let g = f.grid.unwrap();
        assert_eq!(
            (g.xsize, g.ysize, g.xscale),
            (layout.width, layout.height, PIXEL_M)
        );
        assert!(g.west < 10.0 && g.east > 12.9 && g.north > 59.7 && g.south < 57.4);
    }

    #[test]
    fn a_short_scan_takes_its_far_ring_from_the_last_long_one() {
        // DMI's alternating Sindal scans: 239 gates (119.5 km) then 475.
        let a = radar("ra", 57.5, 10.0, 237.5);
        let sites = vec![a.clone()];
        let set = Set {
            sites: vec![SiteReach {
                id: "ra".into(),
                reach_km: 237.5,
            }],
            rule: Rule::Lowest,
            ..Set::default()
        };
        let layout = Layout::new(&set, &sites).unwrap();
        let t = 1_800_000_000_000 - 1_800_000_000_000 % CADENCE_MS;
        let mut schedule = Schedule::new(vec![237_500.0], &[]);
        let have = |code, gates, start| Have {
            sweep: Arc::new(sweep(code, gates, 0.5, start)),
            cost: Cost::default(),
        };
        schedule.add(0, t - CADENCE_MS, have(120, 475, t - CADENCE_MS), t, true);
        schedule.add(0, t, have(90, 239, t), t, true);
        let (owned, costs) = schedule.inputs(t);
        let outer = owned[0].as_ref().unwrap().1.as_ref().map(|(_, when)| *when);
        assert_eq!(outer, Some(t - CADENCE_MS));
        let inputs: Vec<Option<Input>> = owned
            .iter()
            .map(|o| {
                o.as_ref().map(|(p, o)| Input {
                    primary: p,
                    outer: o.as_ref().map(|(s, _)| s.as_ref()),
                })
            })
            .collect();
        let (codes, _) = combine(&layout, Rule::Lowest, &inputs);
        let at = |km: f64| {
            let (lat, lon) = destination(a.lat, a.lon, 45.0, km * 1000.0);
            let (c, r) = locate(&layout, lat, lon);
            codes[(r * layout.width + c) as usize]
        };
        assert_eq!(
            (at(60.0), at(110.0)),
            (90, 90),
            "the new scan where it reaches"
        );
        assert_eq!(
            (at(130.0), at(220.0)),
            (120, 120),
            "the long scan past its edge"
        );
        assert!(provenance(&layout, t, &owned, &costs, &[]).contains("ra (far ring"));
        // A long scan needs no ring; neither does a short one with no long
        // scan within 10 minutes.
        let mut lone = Schedule::new(vec![237_500.0], &[]);
        lone.add(0, t - 3 * CADENCE_MS, have(120, 475, 0), t, true);
        lone.add(0, t, have(90, 239, t), t, true);
        assert!(lone.inputs(t).0[0].as_ref().unwrap().1.is_none());
        // A backfill delivers newest first: a short scan whose long one is
        // not in yet does not make its frame complete; the long one does.
        let now = t + 60_000;
        let mut order = Schedule::new(vec![237_500.0], &[]);
        order.add(0, t, have(90, 239, t), now, true);
        assert!(!order.ready(now).contains(&t), "waiting for the far ring");
        order.add(0, t - CADENCE_MS, have(120, 475, t - CADENCE_MS), now, true);
        assert!(order.ready(now).contains(&t), "the far ring is in");
    }

    #[test]
    fn a_silent_radar_is_not_waited_for_and_the_fill_counts_what_is_in() {
        let t = 1_800_000_000_000 - 1_800_000_000_000 % CADENCE_MS;
        let have = || Have {
            sweep: Arc::new(sweep(100, 10, 0.5, t)),
            cost: Cost::default(),
        };
        let now = t + DUE_MS; // T is the newest due time
        let mut s = Schedule::new(vec![5_000.0; 3], &[]).with_depth(3);
        // Nothing in: each radar is expected to bring its newest (one
        // cadence past T) and T itself.
        let fill = s.fill(now).unwrap();
        assert_eq!(
            (fill.stage, fill.done, fill.total, fill.t),
            (crate::loading::Stage::First, 0, 6, t)
        );
        // Radar 0's newest is T + 5: that time is now the closest to
        // completion (1 of 3; it builds first once every radar's newest is
        // in), where T would be 1 of 6.
        s.add(0, t + CADENCE_MS, have(), now, true);
        let fill = s.fill(now).unwrap();
        assert_eq!((fill.done, fill.total, fill.t), (1, 3, t + CADENCE_MS));
        // Radar 0 brings T, radar 1's newest is T: 3 of 5 volumes, 2 of 3 radars.
        s.add(0, t, have(), now, true);
        s.add(1, t, have(), now, true);
        let fill = s.fill(now).unwrap();
        assert_eq!(
            (fill.done, fill.total, fill.radars_in, fill.counted),
            (3, 5, 2, 3)
        );
        assert_eq!(
            fill.progress("Nordic Rain mass").label,
            format!(
                "Nordic Rain mass: 2 of 3 radars in for {}",
                utc(t, "%H:%MZ")
            )
        );
        assert!(s.ready(now).is_empty(), "radar 2 missing and no quiet yet");
        // Radar 2 is silent (Kiruna): it leaves the count, and T builds at
        // once instead of after QUIET_MS without arrivals.
        s.set_silent(2, true);
        let fill = s.fill(now).unwrap();
        assert_eq!((fill.done, fill.total, fill.silent), (3, 3, 1));
        assert_eq!(
            fill.progress("Nordic Rain mass").label,
            format!(
                "Nordic Rain mass: 2 of 2 radars in for {} (1 silent)",
                utc(t, "%H:%MZ")
            )
        );
        assert_eq!(s.ready(now), [t], "built without the silent radar");
        s.mark_built(t);
        // History: the window's three times, T built for both counted
        // radars, T - 5 with one scan in.
        s.add(1, t - CADENCE_MS, have(), now, true);
        let fill = s.fill(now).unwrap();
        assert_eq!(
            (fill.stage, fill.done, fill.total, fill.built, fill.window),
            (crate::loading::Stage::History, 3, 6, 1, 3)
        );
        assert_eq!(
            fill.progress("Nordic Rain mass").label,
            "Nordic Rain mass: history 1 of 3 frames (1 silent)"
        );
        // A scan counts the radar again; a time given up on is done.
        s.set_silent(2, false);
        assert_eq!(s.fill(now).unwrap().total, 9);
        s.mark_built(t - CADENCE_MS);
        s.forget(t - 2 * CADENCE_MS);
        assert_eq!(s.fill(now), None, "the window is built: the fill is over");
        // Every radar silent and nothing in: nothing to build.
        let mut none = Schedule::new(vec![5_000.0], &[]);
        none.set_silent(0, true);
        none.touch(now);
        assert!(none.ready(now + QUIET_MS).is_empty());
    }

    #[test]
    fn frames_are_built_once_when_all_are_in_or_when_due_and_quiet() {
        let t = 1_800_000_000_000 - 1_800_000_000_000 % CADENCE_MS;
        let have = || Have {
            sweep: Arc::new(sweep(100, 10, 0.5, t)),
            cost: Cost::of("SMHI radar_vara_qcvol_x: 7 range requests, 98304 of 15000000 bytes"),
        };
        assert_eq!(
            have().cost,
            Cost {
                requests: 7,
                bytes: 98_304,
                total: 15_000_000,
                from_store: false
            }
        );
        let s = |m: i64| t + m * 60_000;
        // Two radars; the catalog holds the hour before T.
        let cached: Vec<i64> = (1..=BACKFILL as i64).map(|k| t - k * CADENCE_MS).collect();
        let mut schedule = Schedule::new(vec![5_000.0; 2], &cached);
        schedule.add(0, t, have(), s(5), true);
        assert!(schedule.ready(s(6)).is_empty(), "one of two, not due");
        schedule.add(1, t, have(), s(6), true);
        assert_eq!(schedule.ready(s(6)), [t], "all in: at once, before T + 8");
        schedule.mark_built(t);
        assert!(schedule.ready(s(6)).is_empty(), "built once");
        // T + 5: radar 1 only; due at T + 13, then only after 30 s quiet.
        schedule.add(1, t + CADENCE_MS, have(), s(11), true);
        assert!(
            schedule.ready(s(12) + 50_000).is_empty(),
            "not due before T + 13"
        );
        schedule.add(0, t - 20 * CADENCE_MS, have(), s(13), true); // too old: dropped
        assert!(!schedule.has(0, t - 20 * CADENCE_MS));
        schedule.add(0, t - CADENCE_MS, have(), s(13), true); // kept: an arrival
        assert!(
            schedule.ready(s(13) + 10_000).is_empty(),
            "an arrival 10 s ago"
        );
        assert_eq!(
            schedule.ready(s(13) + 31_000),
            [t + CADENCE_MS],
            "due and quiet"
        );
        // Review #1: with no scan in hand nothing is built, however quiet;
        // with one radar's scans in, the quiet builds the hour, newest first.
        let mut fresh = Schedule::new(vec![5_000.0; 2], &[]);
        assert!(fresh.ready(s(13) + 31_000).is_empty(), "no scan, no frame");
        fresh.touch(s(13));
        assert!(
            fresh.ready(s(13) + 31_000).is_empty(),
            "quiet, still no scan"
        );
        for k in 0..BACKFILL as i64 {
            fresh.add(0, t + CADENCE_MS - k * CADENCE_MS, have(), s(13), true);
        }
        let ready = fresh.ready(s(13) + 31_000);
        assert_eq!(ready.len(), BACKFILL);
        assert_eq!(ready[0], t + CADENCE_MS);
        assert!(ready.windows(2).all(|w| w[0] - w[1] == CADENCE_MS));
        // Radar 0 is missing at T + 5, and named.
        let layout = Layout::new(
            &Set {
                sites: vec![
                    SiteReach {
                        id: "ra".into(),
                        reach_km: 50.0,
                    },
                    SiteReach {
                        id: "rb".into(),
                        reach_km: 50.0,
                    },
                ],
                rule: Rule::Lowest,
                ..Set::default()
            },
            &[
                radar("ra", 58.0, 12.0, 240.0),
                radar("rb", 58.0, 13.0, 240.0),
            ],
        )
        .unwrap();
        let (owned, costs) = schedule.inputs(t + CADENCE_MS);
        assert!(owned[0].is_none() && owned[1].is_some());
        let text = provenance(&layout, t + CADENCE_MS, &owned, &costs, &[]);
        assert!(
            text.contains("1 of 2 radars") && text.contains("missing ra"),
            "{text}"
        );
        assert!(
            text.ends_with(": 7 range requests, 98304 of 15000000 bytes"),
            "{text}"
        );
        // What each poller need not fetch: its scans and the built frames.
        let known = schedule.known(0);
        assert!(known.contains(&t) && known.contains(&(t - CADENCE_MS)));
        assert_eq!(nominal_ms(t + 2 * 60_000 + 29_000), t);
        assert_eq!(nominal_ms(t - 3_000), t);
    }

    #[test]
    fn a_radar_that_always_scans_short_needs_no_far_ring() {
        // Review #3: once an older scan of the radar is in and none of its
        // scans is longer, its short scan is complete.
        let t = 1_800_000_000_000 - 1_800_000_000_000 % CADENCE_MS;
        let now = t + 60_000;
        let short = |start| Have {
            sweep: Arc::new(sweep(90, 239, 0.5, start)),
            cost: Cost::default(),
        };
        let mut s = Schedule::new(vec![237_500.0], &[]);
        s.add(0, t, short(t), now, true);
        assert!(
            !s.ready(now).contains(&t),
            "no older scan yet: it may alternate"
        );
        s.add(0, t - CADENCE_MS, short(t - CADENCE_MS), now, true);
        assert!(
            s.ready(now).contains(&t),
            "short twice: no far ring to wait for"
        );
        assert!(s.inputs(t).0[0].as_ref().unwrap().1.is_none());
    }

    #[test]
    fn a_late_scan_rebuilds_one_of_the_newest_two_frames_once() {
        let t = 1_800_000_000_000 - 1_800_000_000_000 % CADENCE_MS;
        let m = |x: i64| t + x * 60_000;
        let have = || Have {
            sweep: Arc::new(sweep(100, 10, 0.5, t)),
            cost: Cost::default(),
        };
        let cached: Vec<i64> = (1..=BACKFILL as i64).map(|k| t - k * CADENCE_MS).collect();
        let mut s = Schedule::new(vec![5_000.0; 3], &cached);
        s.add(0, t, have(), m(6), true);
        // Due at T + 8, quiet 30 s after the last arrival: radar 0 alone.
        assert!(s.ready(m(8) - 1_000).is_empty(), "not due before T + 8");
        assert_eq!(s.ready(m(8) + 31_000), [t]);
        s.mark_built(t);
        // Radar 1's scan for T at T + 9: T is built once more, after the
        // quiet (radar 2 may still come).
        s.add(1, t, have(), m(9), true);
        assert!(s.is_late(t));
        assert!(s.ready(m(9)).is_empty());
        assert_eq!(s.ready(m(9) + 31_000), [t]);
        s.mark_built(t);
        assert!(!s.is_late(t) && s.ready(m(9) + 31_000).is_empty());
        // Radar 2's at T + 10: no third build.
        s.add(2, t, have(), m(10), true);
        assert!(!s.is_late(t) && s.ready(m(10) + 31_000).is_empty());
        // A late scan for a frame older than the newest two builds nothing,
        // nor one that comes after T + 12.
        s.add(1, t - 2 * CADENCE_MS, have(), m(10), true);
        assert!(!s.is_late(t - 2 * CADENCE_MS));
        let mut old = Schedule::new(vec![5_000.0; 2], &cached);
        old.add(0, t, have(), m(6), true);
        old.mark_built(t);
        old.add(1, t, have(), m(12) + 1_000, true);
        assert!(!old.is_late(t));
    }

    #[test]
    fn a_farther_radar_with_a_lower_beam_wins_and_a_tie_goes_to_the_nearer() {
        // The order itself: the lower beam first, then the nearer radar.
        assert!(
            lowest_key(500, 100) < lowest_key(500, 200),
            "a tie: the nearer"
        );
        assert!(
            lowest_key(499, 900) < lowest_key(500, 100),
            "lower beats nearer"
        );
        assert!(
            lowest_key(-20, 5) < lowest_key(0, 5),
            "below sea level still orders"
        );
        // A 35 km west of B. Ten km east of A (25 km from B), B's 0.5° beam
        // centre is ~0.36 km up; A's is ~1.6 km with A on a 1,500 m
        // mountain, or ~0.8 km with A at 100 m scanning 4°. B wins both,
        // though farther.
        let b = radar("rb", 58.0, 12.6, 100.0);
        let mountain = Station {
            alt_m: 1500.0,
            ..radar("ra", 58.0, 12.0, 100.0)
        };
        for (a, deg_a) in [(mountain, 0.5f32), (radar("ra", 58.0, 12.0, 100.0), 4.0)] {
            let sites = vec![a.clone(), b.clone()];
            let set = Set {
                sites: vec![
                    SiteReach {
                        id: "ra".into(),
                        reach_km: 100.0,
                    },
                    SiteReach {
                        id: "rb".into(),
                        reach_km: 100.0,
                    },
                ],
                rule: Rule::Lowest,
                ..Set::default()
            };
            let layout = Layout::new(&set, &sites).unwrap();
            let (sa, sb) = (sweep(100, 200, deg_a, 0), sweep(150, 200, 0.5, 0));
            let inputs = [
                Some(Input {
                    primary: &sa,
                    outer: None,
                }),
                Some(Input {
                    primary: &sb,
                    outer: None,
                }),
            ];
            let (codes, owner) = combine(&layout, Rule::Lowest, &inputs);
            let (lat, lon) = destination(a.lat, a.lon, 90.0, 10_000.0);
            let (c, r) = locate(&layout, lat, lon);
            let i = (r * layout.width + c) as usize;
            assert_eq!(
                (codes[i], owner[i]),
                (150, 1),
                "A at {} m, {deg_a}°",
                a.alt_m
            );
            // And 5 km west of A, 40 km from B: A is lower there at 0.5°
            // from sea level only; on the mountain B still wins.
            let (lat, lon) = destination(a.lat, a.lon, 270.0, 5_000.0);
            let (c, r) = locate(&layout, lat, lon);
            let i = (r * layout.width + c) as usize;
            let expect = if a.alt_m > 1000.0 { 1 } else { 0 };
            assert_eq!(owner[i], expect, "west of A at {} m, {deg_a}°", a.alt_m);
        }
        // An exact tie (two radars on one mast, same angle): the first in
        // the set keeps the texel.
        let sites = vec![
            radar("ra", 58.0, 12.0, 100.0),
            radar("rb", 58.0, 12.0, 100.0),
        ];
        let set = Set {
            sites: vec![
                SiteReach {
                    id: "ra".into(),
                    reach_km: 100.0,
                },
                SiteReach {
                    id: "rb".into(),
                    reach_km: 100.0,
                },
            ],
            rule: Rule::Lowest,
            ..Set::default()
        };
        let layout = Layout::new(&set, &sites).unwrap();
        let (sa, sb) = (sweep(100, 200, 0.5, 0), sweep(150, 200, 0.5, 0));
        let inputs = [
            Some(Input {
                primary: &sa,
                outer: None,
            }),
            Some(Input {
                primary: &sb,
                outer: None,
            }),
        ];
        let (codes, owner) = combine(&layout, Rule::Lowest, &inputs);
        assert!(owner.iter().all(|&o| o == 0 || o == u8::MAX));
        assert!(codes.contains(&100) && !codes.contains(&150));
    }

    /// A synthetic volume: one scan per (angle, marker code), 360 rays of
    /// `gates` 500 m gates.
    fn volume(markers: &[(f64, u8)], gates: u16) -> Vec<Tilt> {
        markers
            .iter()
            .map(|&(deg, code)| Tilt {
                elangle: deg,
                sweep: sweep(code, gates, deg as f32, 0),
            })
            .collect()
    }

    #[test]
    fn a_height_set_is_checked_and_named() {
        let sites = table();
        let args = [SiteArg::Id("vara".into()), SiteArg::Id("nohur".into())];
        let ground =
            choose_with(&sites, &args, Some("height"), Some(3000), Some("ground")).unwrap();
        assert_eq!(
            (ground.rule, ground.height_m, ground.above),
            (Rule::Height, Some(3000), Some(Above::Ground))
        );
        let json = serde_json::to_value(&ground).unwrap();
        assert_eq!(
            (&json["rule"], &json["heightM"], &json["above"]),
            (
                &serde_json::json!("height"),
                &serde_json::json!(3000),
                &serde_json::json!("ground")
            )
        );
        let plain = choose_with(&sites, &args, Some("height"), None, None).unwrap();
        assert_eq!(
            (plain.height_m, plain.above),
            (Some(2000), Some(Above::Sea))
        );
        // Another height, or the same above sea level, is another set.
        let sea = choose_with(&sites, &args, Some("height"), Some(3000), Some("sea")).unwrap();
        assert!(!ground.same(&sea) && !sea.same(&plain));
        assert_ne!(ground.variant(), sea.variant());
        assert_ne!(sea.variant(), plain.variant());
        // A set without a height keeps its S25 name and sends no height.
        let lowest = choose(&sites, &args, None).unwrap();
        let text: String = lowest
            .key()
            .iter()
            .map(|(id, t)| format!("|{id}:{t}"))
            .collect();
        let hash = fnv1a(format!("lowest{text}").as_bytes());
        assert_eq!(
            lowest.variant(),
            format!("m{:08x}", (hash >> 32) as u32 ^ hash as u32)
        );
        assert!(
            serde_json::to_value(&lowest)
                .unwrap()
                .get("heightM")
                .is_none()
        );
        // Frames are named as a radar's height.
        assert_eq!(
            ground.product(),
            ("CAPPI", "Height 3 km above ground".to_owned())
        );
        assert_eq!(sea.product().1, "Height 3 km");
        assert_eq!(lowest.product(), ("REF", "Lowest beam".to_owned()));
        // Review #8: half an hour with an SMHI radar; a set of ORD radars,
        // whose whole files are 1 request each, the full hour.
        let layout_of = |s: &Set| Layout::new(s, &sites).unwrap();
        assert_eq!(layout_of(&ground).backfill(), HEIGHT_BACKFILL);
        assert_eq!(layout_of(&lowest).backfill(), BACKFILL);
        let ord_only = [SiteArg::Id("nohur".into()), SiteArg::Id("dksin".into())];
        let ord_set = choose_with(&sites, &ord_only, Some("height"), None, None).unwrap();
        assert_eq!(layout_of(&ord_set).backfill(), BACKFILL);
        // S24a review #2: an FMI radar's volume is a file per angle, so a
        // height set with one builds back half an hour like SMHI's.
        let fmi = [SiteArg::Id("nohur".into()), SiteArg::Id("fikor".into())];
        let fmi_set = choose_with(&sites, &fmi, Some("height"), None, None).unwrap();
        let fmi_layout = layout_of(&fmi_set);
        assert_eq!(fmi_layout.backfill(), HEIGHT_BACKFILL);
        let costs: Vec<(String, usize)> = fmi_layout
            .radars
            .iter()
            .map(|r| (r.station.id.clone(), volume_requests(&r.station)))
            .collect();
        assert!(costs.contains(&("fikor".into(), 5)), "{costs:?}");
        assert!(costs.contains(&("nohur".into(), 1)), "{costs:?}");
        let vara = sites.iter().find(|s| s.id == "vara").unwrap();
        assert_eq!(volume_requests(vara), SMHI_VOLUME_REQUESTS);
        for (rule, m, above, says) in [
            (Some("height"), Some(750), None, "heightM 750"),
            (Some("height"), None, Some("space"), "sea or ground"),
            (Some("lowest"), Some(3000), None, "height rule only"),
            (None, None, Some("sea"), "height rule only"),
        ] {
            let e = choose_with(&sites, &args, rule, m, above).unwrap_err();
            assert!(e.contains(says), "{e}");
        }
        let rules: Vec<&str> = info().rules.iter().map(|r| r.id).collect();
        assert_eq!(rules, ["lowest", "strongest", "height"]);
        // A height set builds back half an hour.
        let now = 1_800_000_000_000;
        let t = newest_due(now);
        let have = || Have {
            sweep: Arc::new(sweep(100, 10, 0.5, t)),
            cost: Cost::default(),
        };
        let mut s = Schedule::new(vec![5_000.0], &[]).with_depth(HEIGHT_BACKFILL);
        for k in 0..12 {
            s.add(0, t - k * CADENCE_MS, have(), now, true);
        }
        assert_eq!(s.ready(now + QUIET_MS + 1_000).len(), HEIGHT_BACKFILL);
        // A second read of the same volume adds its cost.
        s.add_cost(
            0,
            t,
            Cost {
                requests: 3,
                bytes: 10,
                total: 20,
                from_store: false,
            },
            now,
        );
        assert_eq!(s.inputs(t).1[0].unwrap().requests, 3);
    }

    /// The answer key (plan §S30): a tower of heights, 0.5 to 8 km, over
    /// one texel between two radars with four marked angles each. At each
    /// height the rule as the protocol words it, written out here, names
    /// the radar and angle, or no radar; heights where the answer turns on
    /// less than a hundredth of a degree or 3 m are skipped. Above sea
    /// level over the Swedish lowlands, and above the ground in Norway's
    /// mountains, where the ground changes the answer.
    #[test]
    fn a_two_radar_tower_picks_the_known_radar_at_each_height() {
        let angles = [0.5, 1.5, 4.0, 10.0];
        let va = volume(&[(0.5, 11), (1.5, 12), (4.0, 13), (10.0, 14)], 240);
        let vb = volume(&[(0.5, 21), (1.5, 22), (4.0, 23), (10.0, 24)], 240);
        let inputs = [
            Some(Volumes {
                primary: &va,
                outer: None,
            }),
            Some(Volumes {
                primary: &vb,
                outer: None,
            }),
        ];
        let half = 0.5f64.to_radians();
        // One radar's pick at `s` metres for `h` above sea level: (code,
        // miss), and whether it is clear of every edge.
        let answer = |st: &Station, first: u8, s: f64, h: f64| -> (Option<(u8, f64)>, bool) {
            let target = h - st.alt_m;
            let th = s / EARTH_M;
            let seen =
                ((EARTH_M + target) * th.cos() - EARTH_M).atan2((EARTH_M + target) * th.sin());
            let (mut holders, mut clear) = (Vec::new(), true);
            for (k, &deg) in angles.iter().enumerate() {
                let e = f64::to_radians(deg);
                let c = (e + th).cos();
                let gate = ((EARTH_M * th.sin() / c - 250.0) / 500.0).round();
                if !(0.0..240.0).contains(&gate) {
                    continue;
                }
                let off = (seen - e).abs() - half;
                clear &= off.abs().to_degrees() > 0.01;
                if off <= 0.0 {
                    let miss = (EARTH_M * e.cos() / c - EARTH_M - target).abs();
                    holders.push((miss, first + k as u8));
                }
            }
            holders.sort_by(|x, y| x.0.total_cmp(&y.0));
            if holders.len() > 1 {
                clear &= holders[1].0 - holders[0].0 > 3.0;
            }
            (holders.first().map(|&(m, c)| (c, m)), clear)
        };
        // Radars `a` and `b` (reach 120 km), a tower 25 km east of `a`:
        // the picks per height, `None` where skipped.
        let run = |a: &Station, b: &Station, above: Above| -> Vec<Option<u8>> {
            let sites = vec![a.clone(), b.clone()];
            let (plat, plon) = destination(a.lat, a.lon, 90.0, 25_000.0);
            let mut picks = Vec::new();
            for m in (500..=8000).step_by(500) {
                let set = Set {
                    sites: vec![
                        SiteReach {
                            id: a.id.clone(),
                            reach_km: 120.0,
                        },
                        SiteReach {
                            id: b.id.clone(),
                            reach_km: 120.0,
                        },
                    ],
                    rule: Rule::Height,
                    height_m: Some(m),
                    above: Some(above),
                };
                let layout = Layout::new(&set, &sites).unwrap();
                let (c, r) = locate(&layout, plat, plon);
                let (tlat, tlon) = texel(&layout, c, r);
                let ground = if above == Above::Ground {
                    crate::terrain::at(tlat, tlon)
                } else {
                    0.0
                };
                let h = f64::from(m) + ground;
                let (pa, ca) = answer(a, 11, ground_m(a.lat, a.lon, tlat, tlon), h);
                let (pb, cb) = answer(b, 21, ground_m(b.lat, b.lon, tlat, tlon), h);
                let expect = match (pa, pb) {
                    (Some(x), Some(y)) if (x.1.round() - y.1.round()).abs() < 3.0 => None,
                    (Some(x), Some(y)) => Some(if x.1 < y.1 { x.0 } else { y.0 }),
                    (Some(x), None) => Some(x.0),
                    (None, Some(y)) => Some(y.0),
                    (None, None) => Some(1),
                };
                let expect = expect.filter(|_| ca && cb);
                let (codes, owner) = combine_height(&layout, m, above == Above::Ground, &inputs);
                let i = (r * layout.width + c) as usize;
                if let Some(code) = expect {
                    assert_eq!(codes[i], code, "{m} m {above:?} ({ground} m of ground)");
                    let who = match code {
                        11..=14 => 0,
                        21..=24 => 1,
                        _ => u8::MAX,
                    };
                    assert_eq!(owner[i], who);
                }
                picks.push(expect);
            }
            picks
        };
        let count = |picks: &[Option<u8>], low: u8, high: u8| {
            picks
                .iter()
                .filter(|p| p.is_some_and(|c| (low..=high).contains(&c)))
                .count()
        };
        // The lowlands: A at 100 m, B at 400 m, 70 km apart.
        let a = Station {
            alt_m: 100.0,
            ..radar("ra", 58.0, 12.0, 120.0)
        };
        let b = Station {
            alt_m: 400.0,
            ..radar("rb", 58.0, 13.19, 120.0)
        };
        let flat = run(&a, &b, Above::Sea);
        assert!(
            flat.iter().flatten().count() >= 12
                && count(&flat, 11, 14) > 0
                && count(&flat, 21, 24) > 0
                && count(&flat, 1, 1) > 0,
            "{flat:?}"
        );
        // The mountains: the same towers above sea level and above ground
        // pick differently.
        let (a, b) = (
            Station {
                alt_m: 1000.0,
                ..radar("ma", 61.0, 8.0, 120.0)
            },
            Station {
                alt_m: 800.0,
                ..radar("mb", 61.0, 9.2, 120.0)
            },
        );
        let (tower_lat, tower_lon) = destination(a.lat, a.lon, 90.0, 25_000.0);
        assert!(crate::terrain::at(tower_lat, tower_lon) > 500.0);
        let sea = run(&a, &b, Above::Sea);
        let ground = run(&a, &b, Above::Ground);
        assert!(ground.iter().flatten().count() >= 10, "{ground:?}");
        assert!(
            sea.iter()
                .zip(&ground)
                .any(|(s, g)| s.is_some() && g.is_some() && s != g),
            "{sea:?} {ground:?}"
        );
    }

    /// One rule, not two copies: Vara alone at a height draws Vara's own
    /// Height product (S29, placed by the lookup rule) at nearly every
    /// texel of its reach; the rest are gate edges, where a texel's own
    /// distance and the product's 500 m gates round differently.
    #[test]
    fn one_radar_at_a_height_is_its_own_cappi() {
        let sites = table();
        let vara = sites.iter().find(|s| s.id == "vara").unwrap().clone();
        let tilts = || {
            let file = std::fs::File::open(format!("{RAW}radar_vara_qcvol_202609131055_tilts.h5"))
                .unwrap();
            let mut t =
                crate::odim::decode_tilts(file, |infos| (0..infos.len()).collect()).unwrap();
            t.sort_by(|a, b| a.elangle.total_cmp(&b.elangle));
            t
        };
        let held = tilts();
        assert!(held.len() >= 3, "{} scans", held.len());
        for m in [1000u32, 3000, 6000] {
            let set = choose_with(
                &sites,
                &[SiteArg::Id("vara".into())],
                Some("height"),
                Some(m),
                None,
            )
            .unwrap();
            let layout = Layout::new(&set, &sites).unwrap();
            let input = [Some(Volumes {
                primary: &held,
                outer: None,
            })];
            let (codes, _) = combine_height(&layout, m, false, &input);
            let product =
                crate::products::compose(Want::Cappi(f64::from(m) - vara.alt_m, m), tilts())
                    .unwrap();
            let (mut same, mut total, mut holes) = (0, 0, 0);
            for r in (0..layout.height).step_by(2) {
                for c in (0..layout.width).step_by(2) {
                    let (lat, lon) = texel(&layout, c, r);
                    if ground_m(vara.lat, vara.lon, lat, lon) > 235_000.0 {
                        continue;
                    }
                    let expect = sample(&vara, &product, lat, lon).map_or(1, |x| x.0);
                    let got = codes[(r * layout.width + c) as usize];
                    total += 1;
                    same += usize::from(got == expect);
                    holes += usize::from(got == 1);
                }
            }
            eprintln!("Vara at {m} m: {same} of {total} texels as its own CAPPI, {holes} holes");
            assert!(same * 100 >= total * 98, "{m} m: {same} of {total}");
            assert!(holes > 0 && holes < total, "{m} m: {holes} holes");
        }
    }

    // -----------------------------------------------------------------------
    // The composites' products (S24b)
    // -----------------------------------------------------------------------

    /// Every scan of one of S20's `_tilts` fixtures, ascending by angle.
    fn tilts_of(fixture: &str) -> Vec<Tilt> {
        let file = std::fs::File::open(format!("{RAW}{fixture}")).unwrap();
        let mut tilts = crate::products::scans_of(file).unwrap();
        tilts.sort_by(|a, b| a.elangle.total_cmp(&b.elangle));
        tilts
    }

    /// The protocol's lookup of one scan placed at `deg` (written from the
    /// text, not `Lookup`/`Beam`): its code and beam centre above sea level
    /// at distance unit `d` and bearing entry `entry` (`rows`: the scan's
    /// azimuth lookup).
    fn look(
        sweep: &Sweep,
        rows: &[u16],
        alt: f64,
        deg: f64,
        d: usize,
        entry: usize,
    ) -> Option<(u8, f64)> {
        let (e, theta) = (deg.to_radians(), d as f64 * UNIT_M / EARTH_M);
        let c = (e + theta).cos();
        let gate = ((EARTH_M * theta.sin() / c - f64::from(sweep.first_gate_m))
            / f64::from(sweep.gate_spacing_m))
        .round();
        if gate < 0.0 || gate >= f64::from(sweep.gates) || rows[entry] == NONE {
            return None;
        }
        let h = alt + (EARTH_M * e.cos() / c - EARTH_M);
        Some((sweep.rays[usize::from(rows[entry])].codes[gate as usize], h))
    }

    /// The maximum's rule written out (`docs/protocol.md`, the composites'
    /// products): the highest measured value (a storm height's top, the
    /// exact over the "at least"), else 0 if any is 0, else 1.
    fn maximum(codes: &[u8], etop: bool) -> u8 {
        let rank = |c: u8| {
            if etop {
                (u32::from(c) - 2) / 2 * 2 + u32::from(c.is_multiple_of(2))
            } else {
                u32::from(c)
            }
        };
        codes
            .iter()
            .copied()
            .filter(|&c| c >= 2)
            .max_by_key(|&c| rank(c))
            .unwrap_or(if codes.contains(&0) { 0 } else { 1 })
    }

    const S20: [(&str, &str); 3] = [
        ("vara", "radar_vara_qcvol_202609131055_tilts.h5"),
        ("nohur", "ord_nohur_202609140930_tilts.h5"),
        ("dksin", "ord_dksin_202609140940_tilts.h5"),
    ];

    /// The plan's answer key (§S24b, tests): S20's Vara, Hurum and Sindal
    /// volumes (their times differ; this is the geometry and the combine
    /// rules, not timing) placed on the real `nordic` grid, every other radar
    /// missing; each product checked point by point against the protocol's
    /// rule written out here, from each radar's own product (`CMAX`, `ETOP`,
    /// `VIL`: `products::compose`, S20/S24a's byte-exact keys) or its own
    /// scans (`LOWB`).
    #[test]
    fn the_composites_products_on_the_nordic_grid_from_s20s_fixtures() {
        let sites = table();
        let nordic = sites.iter().find(|s| s.id == "nordic").unwrap().clone();
        let template: Frame = serde_json::from_str(include_str!("../data/fixture.json")).unwrap();
        let stations: Vec<Station> = S20
            .iter()
            .map(|(id, _)| sites.iter().find(|s| s.id == *id).unwrap().clone())
            .collect();
        let t = nominal_ms(tilts_of(S20[0].1)[0].sweep.start_ms);
        for want in [
            Want::ColMax,
            Want::EchoTop(0.0),
            Want::Vil,
            Want::LowestBeam,
        ] {
            let started = Instant::now();
            let layout = Layout::grid(&nordic, want, &sites).unwrap();
            let placed = started.elapsed();
            assert_eq!((layout.width, layout.height), (1671, 2297));
            assert_eq!(layout.radars.len(), 41);
            // The composite's box on the 2 km lattice (to a nanodegree).
            let west = ((mercator_x(3.0) / PIXEL_M).floor() * PIXEL_M / MERCATOR_R).to_degrees();
            let north = mercator_lat((mercator_y(71.5) / PIXEL_M).ceil() * PIXEL_M);
            assert!(
                (layout.west - west).abs() < 1e-9 && (layout.north - north).abs() < 1e-9,
                "{} {}",
                layout.west,
                layout.north
            );
            assert!(layout.west <= 3.0 && layout.north >= 71.5);
            let started = Instant::now();
            let combined = combine_grid(&layout, |i| {
                let id = &layout.radars[i].station.id;
                let (_, fixture) = S20.iter().find(|(f, _)| f == id)?;
                Some((tilts_of(fixture), None))
            });
            eprintln!(
                "nordic {}: 41 radars placed in {placed:?}; 3 fixtures combined in {:?}",
                want.variant(),
                started.elapsed()
            );
            let mut used: Vec<&str> = layout
                .radars
                .iter()
                .zip(&combined.ends)
                .filter(|(_, e)| e.is_some())
                .map(|(r, _)| r.station.id.as_str())
                .collect();
            used.sort_unstable();
            assert_eq!(
                used,
                ["dksin", "nohur", "vara"],
                "only the radars with a volume"
            );
            // What the rule is made of, per radar: its own product, or for
            // `LOWB` its scans and blockage table.
            let etop = matches!(want, Want::EchoTop(_));
            let own: Vec<Sweep> = stations
                .iter()
                .zip(&S20)
                .map(|(st, (_, f))| {
                    let w = match want {
                        Want::EchoTop(_) => Want::EchoTop(st.alt_m),
                        Want::LowestBeam => Want::Lowest,
                        w => w,
                    };
                    crate::products::compose(w, tilts_of(f)).unwrap()
                })
                .collect();
            let scans: Vec<Vec<Tilt>> = S20.iter().map(|(_, f)| tilts_of(f)).collect();
            let infos: Vec<Vec<TiltInfo>> = scans
                .iter()
                .map(|v| v.iter().map(Tilt::info).collect())
                .collect();
            let own_rows: Vec<Vec<u16>> = own.iter().map(rows_of).collect();
            let scan_rows: Vec<Vec<Vec<u16>>> = scans
                .iter()
                .map(|v| v.iter().map(|t| rows_of(&t.sweep)).collect())
                .collect();
            // Each radar's texels' ground distance and bearing as the layout
            // placed them (placement is S25's, checked against the sphere in
            // its own test); what is checked here is the combine rules.
            let placed: Vec<&Radar> = stations
                .iter()
                .map(|st| {
                    layout
                        .radars
                        .iter()
                        .find(|r| r.station.id == st.id)
                        .unwrap()
                })
                .collect();
            let (mut checked, mut off, mut measured) = (0usize, 0usize, 0usize);
            for r in (0..layout.height as usize).step_by(5) {
                for c in (0..layout.width as usize).step_by(5) {
                    // (radar, distance unit, bearing entry) of each in reach.
                    let near: Vec<(usize, usize, usize)> = placed
                        .iter()
                        .enumerate()
                        .filter_map(|(i, p)| {
                            if c < p.c0 || c >= p.c0 + p.w || r < p.r0 || r >= p.r0 + p.h {
                                return None;
                            }
                            let k = (r - p.r0) * p.w + (c - p.c0);
                            (p.dist[k] != NONE)
                                .then(|| (i, usize::from(p.dist[k]), usize::from(p.az[k])))
                        })
                        .collect();
                    if near.is_empty() {
                        continue;
                    }
                    let expected = if want == Want::LowestBeam {
                        // Each radar's clear beam at this degree (its
                        // blockage table, else its lowest scan), placed at
                        // its own angle; the lowest beam centre, then the
                        // nearer radar.
                        near.iter()
                            .filter_map(|&(i, d, entry)| {
                                let st = &stations[i];
                                let k = crate::products::blockage(&st.id).map_or(0, |b| {
                                    crate::products::hybrid_pick(
                                        &infos[i],
                                        b.tenths[entry / 10 % 360],
                                    )
                                });
                                let tilt = &scans[i][k];
                                let (code, h) = look(
                                    &tilt.sweep,
                                    &scan_rows[i][k],
                                    st.alt_m,
                                    tilt.elangle,
                                    d,
                                    entry,
                                )?;
                                (code != 1).then_some((code, h.round(), d))
                            })
                            .min_by(|a, b| a.1.total_cmp(&b.1).then(a.2.cmp(&b.2)))
                            .map_or(1, |c| c.0)
                    } else {
                        // Each radar's own product at its frame's angle; the
                        // maximum.
                        let codes: Vec<u8> = near
                            .iter()
                            .filter_map(|&(i, d, entry)| {
                                let deg = own[i].elevation_deg();
                                look(&own[i], &own_rows[i], stations[i].alt_m, deg, d, entry)
                                    .map(|x| x.0)
                            })
                            .collect();
                        maximum(&codes, etop)
                    };
                    let got = combined.codes[r * layout.width as usize + c];
                    checked += 1;
                    measured += usize::from(got >= 2);
                    off += usize::from(got != expected);
                }
            }
            eprintln!(
                "nordic {}: {checked} texels checked, {off} differ, {measured} measured",
                want.variant()
            );
            assert!(checked > 10_000, "{checked}");
            // Rain mass under 0.25 kg/m² draws nothing (S24a review #4), so
            // these volumes have little of it.
            assert!(
                measured > if want == Want::Vil { 10 } else { 100 },
                "{measured}"
            );
            // Review N1: the rule written out gives every texel exactly.
            assert_eq!(off, 0, "{off} of {checked} differ from the rule");
            // The frame: the composite's, named by the product, crediting
            // every owner it drew on, with the product's own legend.
            let built = built(&layout, t, combined.codes.clone(), &combined.ends);
            let f = frame(&template, &nordic, &built);
            assert_eq!(f.id, format!("nordic-20260913T105500Z-{}", want.variant()));
            assert_eq!(f.kind, FrameKind::Grid);
            assert_eq!(
                f.attribution,
                "SMHI, CC BY 4.0; MET Norway, CC BY 4.0; DMI, CC BY 4.0"
            );
            assert_eq!((f.site.lat, f.site.lon), (nordic.lat, nordic.lon));
            let g = f.grid.as_ref().unwrap();
            assert_eq!((g.xsize, g.ysize), (1671, 2297));
            match want {
                Want::EchoTop(_) => {
                    assert_eq!((f.product.as_str(), f.units.as_str()), ("ETOP", "km"));
                    assert_eq!((f.scale, f.offset), (10.0, 2.0));
                    // "At least" tops: G bit 8 in the grid texture, on
                    // exactly the odd measured codes.
                    let mut pixels =
                        built
                            .grid
                            .texture_coded(&f.bounds, f.palette.len(), f.scale, f.offset);
                    crate::products::mark_texture(&f.product, &mut pixels);
                    let marked = pixels.chunks(4).filter(|p| p[1] & 8 != 0).count();
                    let odd = built
                        .grid
                        .codes
                        .iter()
                        .filter(|&&c| c >= 2 && c % 2 == 1)
                        .count();
                    eprintln!("nordic etop: {odd} 'at least' texels");
                    assert_eq!(marked, odd);
                }
                Want::Vil => assert_eq!(
                    (f.product.as_str(), f.units.as_str(), f.scale),
                    ("VIL", "kg/m²", 2.0)
                ),
                Want::ColMax => assert_eq!(
                    (f.product.as_str(), f.units.as_str(), f.scale),
                    ("CMAX", "dBZ", 2.0)
                ),
                _ => {
                    assert_eq!(
                        (f.product.as_str(), f.product_name.as_str()),
                        ("LOWB", "Lowest beam")
                    );
                    // Plan §S25: at the Skagerrak midpoint Hurum's lowest
                    // beam is ~4 km up, Vara's and Sindal's under 2 km.
                    let (c, r) = locate(&layout, 57.87, 11.48);
                    let o = combined.owner[(r * layout.width + c) as usize];
                    assert_ne!(
                        o as usize,
                        layout
                            .radars
                            .iter()
                            .position(|r| r.station.id == "nohur")
                            .unwrap()
                    );
                }
            }
        }
        // CAPPI is My mosaic's height rule over the composite's radars: the
        // same codes and owners as `combine_height` on the same volumes.
        let layout = Layout::grid(&nordic, Want::Cappi(2000.0, 2000), &sites).unwrap();
        assert_eq!(layout.make(), Make::Height);
        let volumes: Vec<Option<Vec<Tilt>>> = layout
            .radars
            .iter()
            .map(|r| {
                S20.iter()
                    .find(|(id, _)| *id == r.station.id)
                    .map(|(_, f)| tilts_of(f))
            })
            .collect();
        let inputs: Vec<Option<Volumes>> = volumes
            .iter()
            .map(|v| {
                v.as_ref().map(|v| Volumes {
                    primary: v,
                    outer: None,
                })
            })
            .collect();
        let (codes, owner) = combine_height(&layout, 2000, false, &inputs);
        let combined = combine_grid(&layout, |i| {
            let id = &layout.radars[i].station.id;
            S20.iter()
                .find(|(f, _)| f == id)
                .map(|(_, f)| (tilts_of(f), None))
        });
        assert!(codes == combined.codes && owner == combined.owner);
        assert!(codes.iter().any(|&c| c >= 2));
        assert_eq!(layout.variant(), "cappi2000");
        assert_eq!(layout.product(), ("CAPPI", "Height 2 km".to_owned()));
    }

    /// Review N1: a short volume (DMI's alternating scans) takes the texels
    /// past its lowest scan's edge from its longer predecessor, in the
    /// maximum's and the lowest beam's paths alike.
    #[test]
    fn a_composites_short_volume_takes_its_far_ring_from_the_longer_one() {
        let nordic = table().into_iter().find(|s| s.id == "nordic").unwrap();
        let a = radar("ra", 60.0, 10.0, 240.0);
        let sites = vec![a.clone(), nordic.clone()];
        // The short volume: 100 gates of 500 m (50 km); the longer one 480.
        let load = |_: usize| {
            Some((
                volume(&[(0.5, 100), (1.5, 110)], 100),
                Some(volume(&[(0.5, 150), (1.5, 160)], 480)),
            ))
        };
        let code_at = |want: Want, km: f64| {
            let layout = Layout::grid(&nordic, want, &sites).unwrap();
            let combined = combine_grid(&layout, load);
            let (lat, lon) = destination(a.lat, a.lon, 90.0, km * 1000.0);
            let (c, r) = locate(&layout, lat, lon);
            combined.codes[(r * layout.width + c) as usize]
        };
        assert_eq!(code_at(Want::ColMax, 20.0), 110, "inside the short volume");
        assert_eq!(code_at(Want::ColMax, 150.0), 160, "its far ring");
        assert_eq!(
            code_at(Want::LowestBeam, 20.0),
            100,
            "inside the short volume"
        );
        assert_eq!(code_at(Want::LowestBeam, 150.0), 150, "its far ring");
        assert_eq!(code_at(Want::ColMax, 250.0), 1, "past the range");
    }

    #[test]
    fn a_composites_lowest_beam_takes_the_nearer_radar_and_each_ones_clear_beam() {
        let nordic = table().into_iter().find(|s| s.id == "nordic").unwrap();
        // A and B 111 km apart on the 60th parallel, the same scans: 0.5°
        // (A 100, B 150) and 1.5° (A 110, B 160); no blockage tables.
        let (a, b) = (
            radar("ra", 60.0, 10.0, 240.0),
            radar("rb", 60.0, 12.0, 240.0),
        );
        let sites = vec![a.clone(), b.clone(), nordic.clone()];
        let point =
            |st: &Station, bearing: f64, km: f64| destination(st.lat, st.lon, bearing, km * 1000.0);
        let run = |want: Want| {
            let layout = Layout::grid(&nordic, want, &sites).unwrap();
            assert_eq!(layout.radars.len(), 2);
            let combined = combine_grid(&layout, |i| {
                let (low, high) = if i == 0 { (100, 110) } else { (150, 160) };
                Some((volume(&[(0.5, low), (1.5, high)], 480), None))
            });
            (layout, combined)
        };
        let at = |layout: &Layout, combined: &Combined, (lat, lon): (f64, f64)| {
            let (c, r) = locate(layout, lat, lon);
            let i = (r * layout.width + c) as usize;
            (combined.codes[i], combined.owner[i])
        };
        let (layout, lowest) = run(Want::LowestBeam);
        assert_eq!(
            at(&layout, &lowest, point(&a, 90.0, 30.0)),
            (100, 0),
            "A is nearer: its beam is lower"
        );
        assert_eq!(
            at(&layout, &lowest, point(&b, 270.0, 30.0)),
            (150, 1),
            "B is nearer"
        );
        assert_eq!(
            at(&layout, &lowest, point(&a, 270.0, 200.0)).0,
            100,
            "A alone"
        );
        assert_eq!(
            at(&layout, &lowest, point(&a, 270.0, 250.0)).0,
            1,
            "past A's range, and B's"
        );
        // The maximum: B's 1.5° wherever it reaches, A's alone past it.
        let (layout, max) = run(Want::ColMax);
        assert_eq!(at(&layout, &max, point(&a, 90.0, 30.0)).0, 160);
        assert_eq!(at(&layout, &max, point(&a, 270.0, 200.0)).0, 110);
        // ETOP's order: a higher top wins, and of two equal tops the exact.
        assert!(
            max_key(22, true) < max_key(23, true),
            "exact 2.0 km over 'at least' 2.0 km"
        );
        assert!(
            max_key(25, true) < max_key(22, true),
            "'at least' 2.2 km over exact 2.0 km"
        );
        assert!(max_key(2, true) < max_key(0, true) && max_key(1, true).is_none());
        assert!(max_key(3, false) < max_key(2, false) && max_key(2, false) < max_key(0, false));
        // A radar's clear beam (S20's blockage table): where its table
        // clears only above 0.5°, `LOWB` takes its 1.5° scan there.
        let (id, degree_blocked, degree_clear) = crate::providers::table()
            .sites
            .iter()
            .filter_map(|s| {
                let b = crate::products::blockage(&s.id)?;
                let blocked = (0..360).find(|&d| {
                    (6..=15).contains(&b.tenths[d])
                        && (6..=15).contains(&b.tenths[(d + 1) % 360])
                        && (6..=15).contains(&b.tenths[(d + 359) % 360])
                })?;
                let clear = (0..360).find(|&d| {
                    b.tenths[d] <= 5
                        && b.tenths[(d + 1) % 360] <= 5
                        && b.tenths[(d + 359) % 360] <= 5
                })?;
                Some((s.id.clone(), blocked, clear))
            })
            .next()
            .expect("a radar whose table has a blocked and a clear degree");
        let real = table().into_iter().find(|s| s.id == id).unwrap();
        let sites = vec![real.clone(), nordic.clone()];
        let layout = Layout::grid(&nordic, Want::LowestBeam, &sites).unwrap();
        let combined = combine_grid(&layout, |_| {
            Some((volume(&[(0.5, 100), (1.5, 150)], 480), None))
        });
        let code_at = |degree: usize| {
            let (lat, lon) = destination(real.lat, real.lon, degree as f64 + 0.5, 40_000.0);
            at(&layout, &combined, (lat, lon)).0
        };
        assert_eq!(
            code_at(degree_blocked),
            150,
            "{id} at {degree_blocked}°: blocked below 1.5°"
        );
        assert_eq!(
            code_at(degree_clear),
            100,
            "{id} at {degree_clear}°: clear at 0.5°"
        );
    }

    /// Review S2: seconds per real Nordic frame, read back from a copy of a
    /// live run's tilt store (`OMASTORM_BENCH_STORE`) at one frame time
    /// (`OMASTORM_BENCH_TIME`, RFC 3339), every product, as `build_and_send`
    /// makes it (`assemble_grid`):
    /// `OMASTORM_BENCH_STORE=… cargo test --release mosaic::tests::a_real -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a copy of a live run's tilt store"]
    fn a_real_nordic_frame_from_a_tilt_store() {
        let dir = std::env::var("OMASTORM_BENCH_STORE").expect("OMASTORM_BENCH_STORE");
        let when = std::env::var("OMASTORM_BENCH_TIME")
            .unwrap_or_else(|_| "2026-09-15T12:00:00Z".to_owned());
        let t = DateTime::parse_from_rfc3339(&when)
            .unwrap()
            .timestamp_millis();
        let store = crate::tilts::Store::open(dir.into(), u64::MAX).unwrap();
        let sites = table();
        let nordic = sites.iter().find(|s| s.id == "nordic").unwrap();
        for want in [
            Want::ColMax,
            Want::EchoTop(0.0),
            Want::Vil,
            Want::Cappi(2000.0, 2000),
            Want::LowestBeam,
        ] {
            let layout = Layout::grid(nordic, want, &sites).unwrap();
            let owned: Owned = layout
                .radars
                .iter()
                .map(|r| {
                    store
                        .volume(&r.station.id, t)
                        .ok()
                        .flatten()
                        .map(|_| (Arc::new(sweep(1, 1, 0.5, t)), None))
                })
                .collect();
            let present = owned.iter().filter(|o| o.is_some()).count();
            let started = Instant::now();
            let (built, unread) = assemble_grid(&layout, t, &owned, Some(&store));
            eprintln!(
                "real nordic {} at {when}: {present} of 41 radars, built in {:.2?}; unread {unread:?}",
                want.variant(),
                started.elapsed()
            );
            assert!(built.is_some());
        }
    }

    /// Review S5: a composite's history frame from under a third of its
    /// radars is left out; its newest frame, and My mosaic's, never.
    #[test]
    fn a_composites_sparse_history_frame_is_left_out() {
        let sites = table();
        let nordic = sites.iter().find(|s| s.id == "nordic").unwrap();
        let layout = Layout::grid(nordic, Want::LowestBeam, &sites).unwrap();
        assert_eq!(layout.radars.len(), 41);
        assert!(too_sparse(&layout, 1, false), "11:45Z from 1 of 41");
        assert!(too_sparse(&layout, 13, false));
        assert!(!too_sparse(&layout, 14, false), "14 of 41 is a third");
        assert!(!too_sparse(&layout, 1, true), "the newest frame is kept");
        let set = choose(
            &sites,
            &[
                SiteArg::Id("vara".into()),
                SiteArg::Id("nohur".into()),
                SiteArg::Id("dksin".into()),
            ],
            None,
        )
        .unwrap();
        let mine = Layout::new(&set, &sites).unwrap();
        assert!(!too_sparse(&mine, 0, false), "My mosaic");
    }

    /// Review S1: a composite's whole-volume products build back 3 frame
    /// times, `LOWB` 6 (with `OMASTORM_GRID_BACKFILL` unset, as in tests).
    #[test]
    fn a_composites_products_build_back_a_quarter_and_half_an_hour() {
        assert!(std::env::var(GRID_BACKFILL_ENV).is_err());
        assert_eq!(grid_backfill(Want::ColMax), 3);
        assert_eq!(grid_backfill(Want::Vil), 3);
        assert_eq!(grid_backfill(Want::Cappi(2000.0, 2000)), 3);
        assert_eq!(grid_backfill(Want::LowestBeam), 6);
        let sites = table();
        let nordic = sites.iter().find(|s| s.id == "nordic").unwrap();
        let layout = Layout::grid(nordic, Want::EchoTop(0.0), &sites).unwrap();
        assert_eq!(layout.backfill(), GRID_WHOLE_BACKFILL);
    }

    #[test]
    fn the_nordic_composites_products_combine_quickly() {
        // All 41 radars with their providers' angles (plan §S24b: engine CPU
        // per Nordic mosaic frame under 3 s in release; `cargo test
        // --release mosaic::tests::the_nordic -- --nocapture`). The volumes
        // are made as each radar is combined, as the store read is.
        let sites = table();
        let nordic = sites.iter().find(|s| s.id == "nordic").unwrap().clone();
        for want in [
            Want::ColMax,
            Want::EchoTop(0.0),
            Want::Vil,
            Want::LowestBeam,
            Want::Cappi(2000.0, 2000),
        ] {
            let started = Instant::now();
            let layout = Layout::grid(&nordic, want, &sites).unwrap();
            let placed = started.elapsed();
            let started = Instant::now();
            let combined = combine_grid(&layout, |i| {
                let station = &layout.radars[i].station;
                let code = 110 + (i % 40) as u8;
                let markers: Vec<(f64, u8)> = crate::products::nominal_angles(station)
                    .iter()
                    .map(|&d| (d, code))
                    .collect();
                Some((volume(&markers, 480), None))
            });
            let took = started.elapsed();
            let drawn = combined.codes.iter().filter(|&&c| c >= 2).count();
            eprintln!(
                "nordic {}: 41 radars on {} × {} texels, placed in {placed:?}, combined in {took:?}, {drawn} texels drawn",
                want.variant(),
                layout.width,
                layout.height
            );
            assert!(drawn > 1_000_000, "{drawn}");
            assert_eq!(combined.ends.iter().filter(|e| e.is_some()).count(), 41);
            if !cfg!(debug_assertions) {
                assert!(took < Duration::from_secs(3), "{took:?}");
            }
        }
    }

    #[test]
    fn twelve_radars_at_a_height_combine_quickly() {
        // Twelve radars with ten angles each: the plan's budget is a slice
        // in under 1 s in release, above sea level or the ground.
        let sites = table();
        let ids = [
            "vara",
            "nohur",
            "dksin",
            "angelholm",
            "karlskrona",
            "hudiksvall",
            "ostersund",
            "lulea",
            "kiruna",
            "nosta",
            "fikor",
            "fivim",
        ];
        let chosen = choose(&sites, &ids.map(|id| SiteArg::Id(id.into())), None).unwrap();
        let angles = [0.5, 1.0, 1.5, 2.0, 2.5, 4.0, 8.0, 14.0, 24.0, 40.0];
        for above in [Above::Sea, Above::Ground] {
            let set = Set {
                sites: chosen.sites.clone(),
                rule: Rule::Height,
                height_m: Some(2000),
                above: Some(above),
            };
            let layout = Layout::new(&set, &sites).unwrap();
            let volumes: Vec<Vec<Tilt>> = (0..layout.radars.len())
                .map(|i| volume(&angles.map(|d| (d, 60 + i as u8 * 10)), 480))
                .collect();
            let inputs: Vec<Option<Volumes>> = volumes
                .iter()
                .map(|v| {
                    Some(Volumes {
                        primary: v,
                        outer: None,
                    })
                })
                .collect();
            let started = Instant::now();
            let built = build_height(&layout, 0, &inputs);
            eprintln!(
                "12 radars at 2 km {above:?}: {} × {} texels, built in {:?}",
                layout.width,
                layout.height,
                started.elapsed()
            );
            assert!(built.grid.codes.iter().any(|&c| c >= 60));
            assert!(built.grid.codes.contains(&1), "holes between the radars");
            assert_eq!(built.product, "CAPPI");
            // Review #3: a frame above the ground credits the terrain.
            assert_eq!(
                built.attribution.contains(crate::terrain::CREDIT),
                above == Above::Ground,
                "{}",
                built.attribution
            );
        }
    }

    /// The height path's store reads (review #4, #5, #6): a whole volume and
    /// the longer one its far ring comes from, a volume the store holds only
    /// the lowest scan of (a radar's lowest-scan run), and one it does not
    /// hold; a frame time with none of its volumes in the store is not built
    /// and is given up; a store-made volume's cost stays the store's.
    #[test]
    fn height_frames_read_their_volumes_from_the_store() {
        let dir = std::env::temp_dir().join(format!("omastorm-s30-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = crate::tilts::Store::open(dir.clone(), u64::MAX).unwrap();
        let (a, b, c) = (
            radar("ra", 58.0, 12.0, 120.0),
            radar("rb", 58.0, 13.2, 120.0),
            radar("rc", 58.6, 12.6, 120.0),
        );
        let sites = vec![a.clone(), b, c];
        let set = Set {
            sites: ["ra", "rb", "rc"]
                .map(|id| SiteReach {
                    id: id.into(),
                    reach_km: 120.0,
                })
                .to_vec(),
            rule: Rule::Height,
            height_m: Some(2000),
            above: Some(Above::Sea),
        };
        let layout = Layout::new(&set, &sites).unwrap();
        let t = 1_800_000_000_000 - 1_800_000_000_000 % CADENCE_MS;
        let save = |id: &str, when: i64, v: &[Tilt], keep: &[usize]| {
            let infos: Vec<crate::products::TiltInfo> = v.iter().map(Tilt::info).collect();
            let tilts: Vec<(usize, &Tilt)> = keep.iter().map(|&k| (k, &v[k])).collect();
            store.save(id, when, "src", Some(&infos), &tilts).unwrap();
        };
        // A: a short volume at T (50 km) and a long one 5 minutes before.
        let short = volume(&[(0.5, 11), (1.5, 12), (4.0, 13)], 100);
        let long = volume(&[(0.5, 21), (1.5, 22), (4.0, 23)], 240);
        save("ra", t, &short, &[0, 1, 2]);
        save("ra", t - CADENCE_MS, &long, &[0, 1, 2]);
        // B: only its lowest scan. C: nothing.
        let vb = volume(&[(0.5, 31), (1.5, 32), (4.0, 33)], 240);
        save("rb", t, &vb, &[0]);
        let whole = |id: &str, t: i64| stored_has(&store, id, t, Want::ColMax);
        assert!(whole("ra", t) && whole("ra", t - CADENCE_MS));
        assert!(!whole("rb", t) && !whole("rc", t));
        assert_eq!(volume_of(&store, "rb", t).map(|v| v.len()), Some(1));
        assert!(volume_of(&store, "rc", t).is_none());
        let scan = || Arc::new(sweep(11, 100, 0.5, t));
        let owned: Owned = vec![
            Some((scan(), Some((scan(), t - CADENCE_MS)))),
            Some((scan(), None)),
            Some((scan(), None)),
        ];
        let (built, unread) = assemble_height(&layout, t, &owned, Some(&store));
        assert_eq!(unread, ["rc"]);
        let built = built.expect("A and B are in the store");
        let at = |bearing: f64, km: f64| {
            let (lat, lon) = destination(a.lat, a.lon, bearing, km * 1000.0);
            let (col, row) = locate(&layout, lat, lon);
            built.grid.codes[(row * layout.width + col) as usize]
        };
        // 2 km above sea (1.9 km above A): 30 km west, inside the short
        // volume's 50 km, its 4° beam (seen at ~3.5°); 80 km west, past it,
        // the long volume's 1.5° beam (seen at ~1.1°): the far ring.
        assert_eq!(at(270.0, 30.0), 13, "the volume at T");
        assert_eq!(at(270.0, 80.0), 22, "the far ring from the longer volume");
        // C is missing from the provenance; B took part with its lowest scan.
        let costs = vec![Some(Cost::default()); 3];
        let text = provenance(&layout, t, &owned, &costs, &unread);
        assert!(
            text.contains("missing rc") && text.contains("2 of 3 radars"),
            "{text}"
        );
        // Review #6: none of the volumes in the store, or no store: no frame.
        let only_c: Owned = vec![None, None, Some((scan(), None))];
        let (none, unread) = assemble_height(&layout, t, &only_c, Some(&store));
        assert!(none.is_none() && unread == ["rc"]);
        assert!(assemble_height(&layout, t, &owned, None).0.is_none());
        let now = t + DUE_MS + 60_000;
        let mut schedule = Schedule::new(vec![120_000.0; 3], &[]);
        let have = Have {
            sweep: scan(),
            cost: Cost::default(),
        };
        schedule.add(2, t, have, now, true);
        assert_eq!(schedule.ready(now + QUIET_MS + 1_000), [t]);
        schedule.forget(t);
        assert!(
            schedule.ready(now + QUIET_MS + 1_000).is_empty(),
            "given up"
        );
        // Review #5: a volume from the store and its free lowest scan (whose
        // provenance reads 0 range requests) stay the store's.
        let stored = Cost::of(&format!("x: {}", crate::tilts::FROM_STORE));
        let free = Cost::of("x, its lowest scan: 0 range requests, 0 of 5 bytes");
        assert!(stored.plus(free).from_store && free.plus(stored).from_store);
        let fetched = Cost::of("x: 39 range requests, 700000 of 800000 bytes");
        assert!(!fetched.plus(free).from_store && !stored.plus(fetched).from_store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn twelve_radars_combine_quickly() {
        // Twelve of the table's radars across Sweden, Norway and Finland at
        // full reach: the plan's budget is under 1 s a frame in release.
        let sites = table();
        let ids = [
            "vara",
            "nohur",
            "dksin",
            "angelholm",
            "karlskrona",
            "hudiksvall",
            "ostersund",
            "lulea",
            "kiruna",
            "nosta",
            "fikor",
            "fivim",
        ];
        let set = choose(&sites, &ids.map(|id| SiteArg::Id(id.into())), None).unwrap();
        let started = Instant::now();
        let layout = Layout::new(&set, &sites).unwrap();
        let placed = started.elapsed();
        let scans: Vec<Sweep> = layout
            .radars
            .iter()
            .enumerate()
            .map(|(i, _)| sweep(60 + i as u8 * 10, 480, 0.5, 0))
            .collect();
        let inputs: Vec<Option<Input>> = scans
            .iter()
            .map(|s| {
                Some(Input {
                    primary: s,
                    outer: None,
                })
            })
            .collect();
        let started = Instant::now();
        let built = build(&layout, 0, &inputs);
        eprintln!(
            "12 radars: {} × {} texels, placed in {placed:?}, built in {:?}",
            layout.width,
            layout.height,
            started.elapsed()
        );
        assert!(layout.width <= MAX_SIDE && layout.height <= MAX_SIDE);
        assert!(built.grid.codes.iter().any(|&c| c >= 60));
    }
}
