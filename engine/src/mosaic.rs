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
//! - **Timing** (`Schedule`, `poll`): scans keyed by nominal time (a scan's
//!   start rounded to 5 minutes), a frame built once when every radar's scan
//!   for T is in or at T + 7 minutes after 30 s without an arrival, missing
//!   radars named in its provenance; radars polled only while `mymosaic` is
//!   selected (`main.rs` starts `poll`), through S27's tilt store first.
//!
//! Relation to S29's reach slider: the same unit (ground km from the
//! antenna), range (25 km to full) and meaning, but S29 clips one radar on
//! a client's screen, while `reachKm` here is applied by the engine before
//! combining, so a trimmed radar's area falls to its neighbours.

use crate::composite::{Grid, MERCATOR_R, OFFSET, PIXEL_M, SCALE};
use crate::odim::Tilt as Which;
use crate::products::{EARTH_M, Want};
use crate::protocol::{Frame, FrameKind, FrameStatus, Geometry, SiteKind, Station};
use crate::providers::{Event, ProviderId, Scan, Spec, Staleness};
use crate::sweep::Sweep;
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::f64::consts::FRAC_PI_4;
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
/// Each radar's poller reaches one volume further back than the mosaic, as
/// its newest volume is usually one cadence ahead of the newest due frame.
const RADAR_DEPTH: usize = BACKFILL + 1;
/// Every provider publishes a volume per radar every 5 minutes.
pub const CADENCE_MS: i64 = 5 * 60 * 1000;
/// The frame for T is due at T + 7 minutes (SMHI publishes 4–5 minutes
/// after the valid time; ORD's cache about as fast)...
pub const DUE_MS: i64 = 7 * 60 * 1000;
/// ...and built then once no scan has arrived for this long, so a backfill
/// still delivering is not cut short.
pub const QUIET_MS: i64 = 30 * 1000;
/// A radar whose scan reaches less than `SHORT` of its full range takes the
/// gates past that scan's edge from its latest longer scan up to this much
/// older (DMI's alternating 119.5 km Sindal scans, plan §S25).
pub const OUTER_MS: i64 = 10 * 60 * 1000;
const SHORT: f64 = 0.75;
/// The sphere the lookup rule measures ground distance and bearing on.
const SPHERE_M: f64 = 6_371_000.0;
/// Ground distances are kept in units of this many metres.
const UNIT_M: f64 = 10.0;
/// Past a radar's reach, or no ray or gate.
const NONE: u16 = u16::MAX;
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
}

impl Rule {
    pub const ALL: [Rule; 2] = [Rule::Lowest, Rule::Strongest];

    pub fn id(self) -> &'static str {
        match self {
            Rule::Lowest => "lowest",
            Rule::Strongest => "strongest",
        }
    }

    /// The display name, also `frame.productName`.
    pub fn name(self) -> &'static str {
        match self {
            Rule::Lowest => "Lowest beam",
            Rule::Strongest => "Strongest",
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

    /// The same radars, reaches and rule, in any order.
    pub fn same(&self, other: &Set) -> bool {
        self.rule == other.rule && self.key() == other.key()
    }

    /// The last part of this set's frame ids and its catalog ring: `m` and
    /// 8 hex digits of the radars, reaches and rule
    /// (`products::variant_of` knows the form).
    pub fn variant(&self) -> String {
        let mut text = String::from(self.rule.id());
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
/// its sender hears (`docs/protocol.md`, My mosaic).
pub fn choose(sites: &[Station], args: &[SiteArg], rule: Option<&str>) -> Result<Set, String> {
    let rule = match rule {
        None => Rule::Lowest,
        Some(id) => Rule::parse(id)
            .ok_or_else(|| format!("Unknown rule {id}; rules are listed in hello.mosaic."))?,
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

fn mercator_x(lon: f64) -> f64 {
    MERCATOR_R * lon.to_radians()
}

fn mercator_y(lat: f64) -> f64 {
    MERCATOR_R * (FRAC_PI_4 + lat.to_radians() / 2.0).tan().ln()
}

fn mercator_lat(y: f64) -> f64 {
    (y / MERCATOR_R).sinh().atan().to_degrees()
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
}

impl Layout {
    /// Place `set`'s radars (looked up in `sites`) on the texture over the
    /// box of their reach circles, whole `PIXEL_M` texels on the Mercator
    /// lattice.
    pub fn new(set: &Set, sites: &[Station]) -> Result<Layout, String> {
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
        let x0 = placed.iter().map(|p| p.2.x0).fold(f64::INFINITY, f64::min);
        let x1 = placed
            .iter()
            .map(|p| p.2.x1)
            .fold(f64::NEG_INFINITY, f64::max);
        let y0 = placed.iter().map(|p| p.2.y0).fold(f64::INFINITY, f64::min);
        let y1 = placed
            .iter()
            .map(|p| p.2.y1)
            .fold(f64::NEG_INFINITY, f64::max);
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
            width,
            height,
            west: (mx_west / MERCATOR_R).to_degrees(),
            east: east.to_degrees(),
            north: mercator_lat(my_north),
            south: mercator_lat(my_north - f64::from(height) * PIXEL_M),
            radars,
            set: set.clone(),
        })
    }

    /// The middle of the texture's box, (lat, lon): `frame.site`.
    pub fn centre(&self) -> (f64, f64) {
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
                    Rule::Lowest => {
                        if code == 1 {
                            continue;
                        }
                        ((i64::from(height) + (1 << 24)) as u64) << 16 | d as u64
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

/// A built frame: the grid, and what names and credits it.
pub struct Built {
    pub grid: Grid,
    /// `Set::variant`, the frame id's last part.
    pub variant: String,
    pub rule: Rule,
    /// Every owner the frame drew on, joined by `; `.
    pub attribution: String,
    /// (lat, lon) of the box's middle: `frame.site`.
    pub centre: (f64, f64),
}

/// The frame for T from each radar's `inputs` (layout order; `None` for a
/// missing radar).
pub fn build(layout: &Layout, t_ms: i64, inputs: &[Option<Input>]) -> Built {
    let rule = layout.set.rule;
    let (codes, _) = combine(layout, rule, inputs);
    let mut owners: Vec<&str> = Vec::new();
    let mut used = Vec::new();
    let mut end_ms = t_ms;
    for (radar, input) in layout.radars.iter().zip(inputs) {
        if let Some(input) = input {
            end_ms = end_ms.max(input.primary.end_ms);
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
            source_projdef: format!("My mosaic, {}: {}", rule.name(), used.join(", ")),
        },
        variant: layout.set.variant(),
        rule,
        attribution: owners.join("; "),
        centre: layout.centre(),
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
        product: template.product.clone(),
        product_name: built.rule.name().to_owned(),
        units: template.units.clone(),
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
        scale: SCALE,
        offset: OFFSET,
        site: Geometry {
            lat: built.centre.0,
            lon: built.centre.1,
            alt_m: 0.0,
        },
        palette: template.palette.clone(),
        bounds: template.bounds.clone(),
        attribution: built.attribution.clone(),
        grid: Some(grid.placement()),
    }
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// The nominal time a scan starting at `start_ms` belongs to: the nearest
/// multiple of 5 minutes (the lowest tilt starts seconds after it).
pub fn nominal_ms(start_ms: i64) -> i64 {
    (start_ms + CADENCE_MS / 2).div_euclid(CADENCE_MS) * CADENCE_MS
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
    /// When a scan last arrived, or the schedule started.
    last_arrival: i64,
}

impl Schedule {
    /// `ranges`: each radar's full range in metres; `cached`: frame times
    /// this set's catalog already holds.
    pub fn new(ranges: Vec<f64>, cached: &[i64], now: i64) -> Schedule {
        Schedule {
            ranges,
            scans: BTreeMap::new(),
            built: cached.iter().copied().collect(),
            last_arrival: now,
        }
    }

    /// The oldest frame time the selection builds.
    fn oldest(now: i64) -> i64 {
        newest_due(now) - (BACKFILL as i64 - 1) * CADENCE_MS
    }

    /// The oldest scan time worth keeping: the frames still to build, and
    /// the far rings they may take from older scans.
    pub fn floor(&self, now: i64) -> i64 {
        let due = newest_due(now);
        let pending = (Schedule::oldest(now)..=due)
            .step_by(CADENCE_MS as usize)
            .find(|t| !self.built.contains(t))
            .unwrap_or(due);
        pending - OUTER_MS
    }

    /// Radar `radar`'s scan at nominal `t`; `arrived` counts it as news
    /// (a poller's event; the tilt store's start-up load is not). An older
    /// scan than the schedule keeps is dropped.
    pub fn add(&mut self, radar: usize, t: i64, have: Have, now: i64, arrived: bool) {
        if t < self.floor(now) || radar >= self.ranges.len() {
            return;
        }
        let n = self.ranges.len();
        self.scans.entry(t).or_insert_with(|| vec![None; n])[radar] = Some(have);
        if arrived {
            self.last_arrival = now;
        }
    }

    /// Whether radar `radar` has a scan at `t`.
    #[cfg(test)]
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

    /// Whether every radar's scan for `t` is in, and for a short scan the
    /// longer one its far ring comes from: a backfill delivers newest
    /// first, so a short scan's older long one may still be on its way.
    fn complete(&self, t: i64) -> bool {
        self.scans.get(&t).is_some_and(|v| {
            v.iter().enumerate().all(|(radar, have)| {
                have.as_ref().is_some_and(|h| {
                    !self.short(radar, &h.sweep) || self.outer(radar, t, &h.sweep).is_some()
                })
            })
        })
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
    /// `BACKFILL` due times not built whose scans are all in or which have
    /// waited out the quiet, and any later time whose scans are all in.
    pub fn ready(&self, now: i64) -> Vec<i64> {
        let due = newest_due(now);
        let quiet = now - self.last_arrival >= QUIET_MS;
        let mut ready: Vec<i64> = (Schedule::oldest(now)..=due)
            .step_by(CADENCE_MS as usize)
            .filter(|t| !self.built.contains(t) && (quiet || self.complete(*t)))
            .collect();
        ready.extend(
            self.scans
                .keys()
                .copied()
                .filter(|t| *t > due && !self.built.contains(t) && self.complete(*t)),
        );
        ready.sort_unstable_by(|a, b| b.cmp(a));
        ready
    }

    /// Whether every due frame is built (the newest due included).
    pub fn caught_up(&self, now: i64) -> bool {
        self.built.contains(&newest_due(now))
    }

    pub fn is_built(&self, t: i64) -> bool {
        self.built.contains(&t)
    }

    pub fn mark_built(&mut self, t: i64) {
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
    }
}

/// A frame's provenance: the set, the radars used, what they cost, the
/// missing ones and the far rings; one `N range requests, B of T bytes` at
/// the end, or the tilt store's words when no scan cost a request, as
/// `scripts/soak-report.sh` reads them.
fn provenance(layout: &Layout, t_ms: i64, owned: &Owned, costs: &[Option<Cost>]) -> String {
    let mut parts = Vec::new();
    let mut missing = Vec::new();
    let (mut requests, mut bytes, mut total, mut fetched) = (0, 0, 0, false);
    for ((radar, input), cost) in layout.radars.iter().zip(owned).zip(costs) {
        let id = radar.station.id.as_str();
        match (input, cost) {
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
        "My mosaic {} {}, {} of {} radars at {}: {}",
        layout.set.variant(),
        layout.set.rule.name(),
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
    eprintln!(
        "{} Mosaic {STATION}: {message}",
        utc(now_ms(), "%Y-%m-%dT%H:%M:%SZ")
    );
}

/// Each radar's stored lowest scans from `floor` on, but not at `skip`
/// times: the tilt store's copies (S27), no request.
fn from_store(layout: &Layout, floor: i64, skip: &[i64]) -> Vec<(usize, i64, Sweep)> {
    let Some(store) = crate::tilts::shared() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (index, radar) in layout.radars.iter().enumerate() {
        let id = &radar.station.id;
        let volumes = store.volumes(id).unwrap_or_default();
        for (t, source) in volumes.into_iter().filter(|(t, _)| *t >= floor) {
            if skip.contains(&t) {
                continue;
            }
            let slot = crate::tilts::Slot {
                store: store.clone(),
                station: id.clone(),
                time_ms: t,
                source,
            };
            if let Ok(Some(Scan::Polar(sweep))) = slot.compose(Want::Lowest, radar.which) {
                out.push((index, t, sweep));
            }
        }
    }
    out
}

/// Build the frame for `t` on the blocking pool and send it: as the live
/// frame when it is the newest built, else as history.
async fn build_and_send(
    layout: &Arc<Layout>,
    schedule: &mut Schedule,
    t: i64,
    events: &Sender<Event>,
) -> bool {
    let (owned, costs) = schedule.inputs(t);
    let newest = schedule.newest_built().is_none_or(|n| t > n);
    schedule.mark_built(t);
    let provenance = provenance(layout, t, &owned, &costs);
    let shared = layout.clone();
    let started = Instant::now();
    let built = spawn_blocking(move || {
        let inputs: Vec<Option<Input>> = owned
            .iter()
            .map(|o| {
                o.as_ref().map(|(p, outer)| Input {
                    primary: p,
                    outer: outer.as_ref().map(|(s, _)| s.as_ref()),
                })
            })
            .collect();
        build(&shared, t, &inputs)
    })
    .await;
    let Ok(built) = built else {
        log(format_args!("building {} failed", utc(t, "%H:%MZ")));
        return true;
    };
    log(format_args!(
        "{} built in {:.0?}, {} × {}; {provenance}",
        utc(t, "%Y-%m-%dT%H:%MZ"),
        started.elapsed(),
        built.grid.width,
        built.grid.height
    ));
    let sweep = Scan::Mosaic(Box::new(built));
    let site = STATION.to_owned();
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
    events.send(event).await.is_ok()
}

/// Poll `set`'s radars and build My mosaic's frames until the task is
/// aborted (`main.rs` does, on a switch or a new set) or `events` closes.
/// `cached` holds the frame times this set's catalog already has. With no
/// radars chosen it waits, polling nothing.
pub async fn poll(set: Set, sites: Vec<Station>, events: Sender<Event>, cached: Vec<i64>) {
    if set.is_empty() {
        return std::future::pending().await;
    }
    let started = Instant::now();
    let layout = match spawn_blocking(move || Layout::new(&set, &sites)).await {
        Ok(Ok(layout)) => Arc::new(layout),
        Ok(Err(e)) => return log(format_args!("cannot place the set: {e}")),
        Err(e) => return log(format_args!("placing the set failed: {e}")),
    };
    log(format_args!(
        "{} {}: {} radars on {} × {} texels, placed in {:.0?}",
        layout.set.variant(),
        layout.set.rule.name(),
        layout.radars.len(),
        layout.width,
        layout.height,
        started.elapsed()
    ));
    let ranges = layout
        .radars
        .iter()
        .map(|r| r.station.range_km * 1000.0)
        .collect();
    let now = now_ms();
    let mut schedule = Schedule::new(ranges, &cached, now);
    // The tilt store first: every stored lowest scan the frames still to
    // build can use, with no request.
    let (floor, skip) = (schedule.floor(now), cached.clone());
    let shared = layout.clone();
    let stored = spawn_blocking(move || from_store(&shared, floor, &skip))
        .await
        .unwrap_or_default();
    let from_the_store = stored.len();
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
    log(format_args!(
        "{from_the_store} lowest scans from the tilt store; {} frames catalogued",
        cached.len()
    ));
    if schedule.caught_up(now)
        && events
            .send(Event::Current {
                site: STATION.to_owned(),
            })
            .await
            .is_err()
    {
        return;
    }
    // One lowest-scan poller per radar, reporting here.
    let (tx, mut rx) = mpsc::channel::<Event>(64);
    let mut offline = vec![false; layout.radars.len()];
    let _pollers: Vec<crate::smhi_live::AbortOnDrop> = layout
        .radars
        .iter()
        .enumerate()
        .map(|(i, radar)| {
            crate::smhi_live::AbortOnDrop(tokio::spawn(crate::providers::poll_lowest(
                radar.station.clone(),
                tx.clone(),
                schedule.known(i),
                RADAR_DEPTH,
            )))
        })
        .collect();
    drop(tx);
    loop {
        match timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(None) => return log("every radar's poller ended"),
            Ok(Some(event)) => {
                let now = now_ms();
                let index = |site: &str| layout.radars.iter().position(|r| r.station.id == site);
                // A newest volume (not history): how late it came, so the
                // due time can be judged against the providers.
                let live = matches!(event, Event::Sweep { .. });
                match event {
                    Event::Sweep {
                        site,
                        sweep: Scan::Polar(sweep),
                        provenance,
                        ..
                    }
                    | Event::Backfill {
                        site,
                        sweep: Scan::Polar(sweep),
                        provenance,
                    } => {
                        if let Some(i) = index(&site) {
                            offline[i] = false;
                            let t = nominal_ms(sweep.start_ms);
                            if live {
                                let late = (now - t) / 1000;
                                log(format_args!(
                                    "{site} {} in at T + {}:{:02}{}",
                                    utc(t, "%H:%MZ"),
                                    late / 60,
                                    late % 60,
                                    if schedule.is_built(t) {
                                        ", after its frame was built"
                                    } else {
                                        ""
                                    }
                                ));
                            }
                            let have = Have {
                                sweep: Arc::new(sweep),
                                cost: Cost::of(&provenance),
                            };
                            schedule.add(i, t, have, now, true);
                        }
                    }
                    Event::Offline { site, reason } => {
                        log(format_args!("{site}: {reason}"));
                        if let Some(i) = index(&site) {
                            offline[i] = true;
                        }
                        if offline.iter().all(|o| *o) {
                            let event = Event::Offline {
                                site: STATION.to_owned(),
                                reason: format!("every chosen radar is offline ({reason})"),
                            };
                            if events.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                    Event::Silent { site, reason } => log(format_args!("{site}: {reason}")),
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
        };
        let layout = Layout::new(&set, &sites).unwrap();
        let t = 1_800_000_000_000 - 1_800_000_000_000 % CADENCE_MS;
        let mut schedule = Schedule::new(vec![237_500.0], &[], t);
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
        assert!(provenance(&layout, t, &owned, &costs).contains("ra (far ring"));
        // A long scan needs no ring; neither does a short one with no long
        // scan within 10 minutes.
        let mut lone = Schedule::new(vec![237_500.0], &[], t);
        lone.add(0, t - 3 * CADENCE_MS, have(120, 475, 0), t, true);
        lone.add(0, t, have(90, 239, t), t, true);
        assert!(lone.inputs(t).0[0].as_ref().unwrap().1.is_none());
        // A backfill delivers newest first: a short scan whose long one is
        // not in yet does not make its frame complete; the long one does.
        let now = t + 60_000;
        let mut order = Schedule::new(vec![237_500.0], &[], now);
        order.add(0, t, have(90, 239, t), now, true);
        assert!(!order.ready(now).contains(&t), "waiting for the far ring");
        order.add(0, t - CADENCE_MS, have(120, 475, t - CADENCE_MS), now, true);
        assert!(order.ready(now).contains(&t), "the far ring is in");
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
        let mut schedule = Schedule::new(vec![5_000.0; 2], &cached, s(5));
        schedule.add(0, t, have(), s(5), true);
        assert!(schedule.ready(s(6)).is_empty(), "one of two, not due");
        schedule.add(1, t, have(), s(6), true);
        assert_eq!(schedule.ready(s(6)), [t], "all in: at once, before T + 7");
        schedule.mark_built(t);
        assert!(schedule.ready(s(6)).is_empty(), "built once");
        // T + 5: radar 1 only; due at T + 12, then only after 30 s quiet.
        schedule.add(1, t + CADENCE_MS, have(), s(11), true);
        assert!(
            schedule.ready(s(11) + 50_000).is_empty(),
            "not due before T + 12"
        );
        schedule.add(0, t - 20 * CADENCE_MS, have(), s(12), true); // too old: dropped
        assert!(!schedule.has(0, t - 20 * CADENCE_MS));
        schedule.add(0, t - CADENCE_MS, have(), s(12), true); // kept: an arrival
        assert!(
            schedule.ready(s(12) + 10_000).is_empty(),
            "an arrival 10 s ago"
        );
        assert_eq!(
            schedule.ready(s(12) + 31_000),
            [t + CADENCE_MS],
            "due and quiet"
        );
        // With nothing catalogued, the whole hour, newest first.
        let fresh = Schedule::new(vec![240_000.0; 2], &[], s(12));
        let ready = fresh.ready(s(12) + 31_000);
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
            },
            &[
                radar("ra", 58.0, 12.0, 240.0),
                radar("rb", 58.0, 13.0, 240.0),
            ],
        )
        .unwrap();
        let (owned, costs) = schedule.inputs(t + CADENCE_MS);
        assert!(owned[0].is_none() && owned[1].is_some());
        let text = provenance(&layout, t + CADENCE_MS, &owned, &costs);
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
