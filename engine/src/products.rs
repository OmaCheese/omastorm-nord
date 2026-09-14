//! Products (S20, `docs/protocol.md`, products): what a radar's frame shows,
//! made from the scan angles of one volume.
//!
//! - `REF` at elevation index 0 is the lowest scan, decoded as before S20
//!   (`Want::Lowest`); `REF` at another index is that one angle as it is
//!   (`Want::Angle`).
//! - `CAPPI1` and `CAPPI2` are pseudo-CAPPIs at 1 and 2 km above the
//!   antenna, `CMAX` the column maximum, `HYBRID` ("Clear view") the lowest
//!   angle the terrain does not block, per azimuth. These are drawn on the
//!   lowest scan's rays and gates (`compose`), so the texture, the azimuth
//!   lookup and the shader's lookup rule are unchanged: `elevationDeg` is the
//!   lowest scan's, and places the gates.
//!
//! Only the angles a product needs are read (`needed`), from the volume's
//! `where` groups alone, before any data: the decoder (`odim::decode_tilts`)
//! calls it with every scan's geometry. `needed` makes the same per-gate
//! choices `compose` does, without the data, so composing from the needed
//! scans alone gives exactly what composing from all of them would; the
//! answer keys (`golden/produce-products.py`) compose from all of them.
//!
//! Geometry is the lookup rule's (`docs/protocol.md`): the 4/3 effective
//! earth radius, and between ground distance `s` and slant range `r` at
//! elevation `e`, `r = R sin(s/R) / cos(e + s/R)`; the beam centre is
//! `h = R cos(e) / cos(e + s/R) − R` above the antenna.

use crate::protocol::{SiteKind, Station};
use crate::providers::ProviderId;
use crate::sweep::{OUTSIDE_COVERAGE, Ray, Sweep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;

/// The effective earth radius of the lookup rule (`ui/shaders/radar.frag`).
pub const EARTH_M: f64 = 6_371_000.0 * 4.0 / 3.0;
/// A scan's ray farther than this from an output ray's azimuth does not
/// reach it (the texture's own gap rule, `sweep::GAP_DEG`).
pub const GAP_DEG: f64 = 0.75;
/// Products other than the lowest scan a station keeps in its catalog: the
/// current one and the one before (`catalog::Catalog::keep`).
pub const KEPT: usize = 2;

pub const REF: &str = "REF";
pub const HYBRID: &str = "HYBRID";
pub const CAPPI1: &str = "CAPPI1";
pub const CAPPI2: &str = "CAPPI2";
pub const CMAX: &str = "CMAX";

/// One entry of `hello.products`.
#[derive(Serialize, PartialEq, Clone, Debug)]
pub struct Info {
    pub id: &'static str,
    pub name: &'static str,
}

/// `hello.products`: the vocabulary, in the order a chooser lists it.
pub const VOCABULARY: [Info; 5] = [
    Info {
        id: REF,
        name: "Lowest scan",
    },
    Info {
        id: HYBRID,
        name: "Clear view",
    },
    Info {
        id: CAPPI1,
        name: "Height 1 km",
    },
    Info {
        id: CAPPI2,
        name: "Height 2 km",
    },
    Info {
        id: CMAX,
        name: "Column max",
    },
];

/// `state.product`, and what a `set_product` asks for.
#[derive(Serialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub id: String,
    pub elevation_index: u32,
}

impl Default for Choice {
    /// The lowest scan: what every frame was before S20.
    fn default() -> Self {
        Choice {
            id: REF.to_owned(),
            elevation_index: 0,
        }
    }
}

/// A radar's blockage table (`engine/data/blockage.json`): per degree of
/// azimuth, the lowest angle the terrain does not block, in tenths of a
/// degree.
#[derive(PartialEq, Debug)]
pub struct Blockage {
    pub tenths: [u8; 360],
}

/// What a poller makes of each volume.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Want {
    /// The lowest scan, decoded as before S20.
    Lowest,
    /// The scan nearest this angle, in degrees.
    Angle(f64),
    /// A pseudo-CAPPI this many metres above the antenna.
    Cappi(f64),
    /// The column maximum.
    ColMax,
    /// Per azimuth, the lowest angle clear of the terrain.
    Hybrid(&'static Blockage),
}

impl Want {
    /// The product a choice names on a station with these nominal angles
    /// and blockage table; `None` when the station cannot make it.
    pub fn of(
        choice: &Choice,
        elevations: &[f64],
        blockage: Option<&'static Blockage>,
    ) -> Option<Want> {
        let index = choice.elevation_index as usize;
        match choice.id.as_str() {
            REF if index == 0 => Some(Want::Lowest),
            REF => elevations.get(index).map(|&deg| Want::Angle(deg)),
            _ if index != 0 => None,
            CAPPI1 => Some(Want::Cappi(1000.0)),
            CAPPI2 => Some(Want::Cappi(2000.0)),
            CMAX => Some(Want::ColMax),
            HYBRID => blockage.map(Want::Hybrid),
            _ => None,
        }
    }

    pub fn is_lowest(&self) -> bool {
        *self == Want::Lowest
    }

    /// The product's id and display name (`frame.product`, `productName`).
    pub fn product(&self) -> (&'static str, &'static str) {
        match self {
            Want::Lowest | Want::Angle(_) => (REF, "Reflectivity"),
            Want::Cappi(h) if *h < 1500.0 => (CAPPI1, "Height 1 km"),
            Want::Cappi(_) => (CAPPI2, "Height 2 km"),
            Want::ColMax => (CMAX, "Column max"),
            Want::Hybrid(_) => (HYBRID, "Clear view"),
        }
    }

    /// The last part of a frame id, and the catalog's key for the product:
    /// `e0` for the lowest scan (the id every frame had before S20), `a40`
    /// for the angle 4.0°, and so on.
    pub fn variant(&self) -> String {
        match self {
            Want::Lowest => "e0".to_owned(),
            Want::Angle(deg) => format!("a{}", (deg * 10.0).round() as i64),
            Want::Cappi(h) => format!("cappi{}", (h / 1000.0).round() as i64),
            Want::ColMax => "cmax".to_owned(),
            Want::Hybrid(_) => "clear".to_owned(),
        }
    }

    /// Frames a switch backfills, by the provider's depths (S26): `lowest`
    /// for the lowest scan, `product` (at most `lowest`) for any other. A
    /// product's ring still fills to `catalog::RING` as live frames arrive.
    pub fn backfill(&self, lowest: usize, product: usize) -> usize {
        if self.is_lowest() {
            lowest
        } else {
            product.min(lowest)
        }
    }
}

/// The product a frame id names (`Want::variant`): its last `-` part when
/// that is a product other than the lowest scan (`cappi1`, `cmax`, `clear`,
/// `a40`, …), else `e0`. So a frame catalogued before S20, or one whose id
/// names no product at all, is the lowest scan, as it always was.
pub fn variant_of(frame_id: &str) -> &str {
    let last = frame_id.rsplit('-').next().unwrap_or_default();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let product = matches!(last, "cmax" | "clear")
        || last.strip_prefix("cappi").is_some_and(digits)
        || last.strip_prefix('a').is_some_and(digits);
    if product { last } else { "e0" }
}

// ---------------------------------------------------------------------------
// Stations: their angles and products
// ---------------------------------------------------------------------------

/// One `hello.sites[].elevations` entry: a nominal angle and its beam
/// centre's height above the antenna at 50 and 100 km, in km to 0.1.
#[derive(Serialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Elevation {
    pub deg: f64,
    pub beam_km50: f64,
    pub beam_km100: f64,
}

/// SMHI's ten angles, the same at every radar (the Vara volume of
/// 2026-09-13, and the key listing of the others).
const SMHI_ANGLES: [f64; 10] = [0.5, 1.0, 1.5, 2.0, 2.5, 4.0, 8.0, 14.0, 24.0, 40.0];
/// MET Norway's 12-angle set (`nohur@20260914T0930@0.5_1.0_…_15.5@DBZH`); a
/// 10-angle set alternates with it, and the volume's own nearest is used.
const NO_ANGLES: [f64; 12] = [
    0.5, 1.0, 1.6, 2.4, 3.2, 4.2, 5.4, 6.8, 8.5, 10.4, 12.8, 15.5,
];
/// DMI's ten (`dksin@20260914T0940@0.49_0.66_…_15.01`), to 0.1°.
const DK_ANGLES: [f64; 10] = [0.5, 0.7, 1.0, 1.5, 2.4, 4.8, 8.4, 10.0, 13.0, 15.0];

/// A station's nominal angles, ascending: empty for a composite, and for
/// a radar whose provider publishes one file per angle (FMI in ORD's
/// cache), whose products would need a file per angle per frame.
pub fn nominal_angles(station: &Station) -> &'static [f64] {
    match (station.kind, station.provider, station.country.as_str()) {
        (SiteKind::Grid, ..) => &[],
        (_, ProviderId::Smhi, _) => &SMHI_ANGLES,
        (_, ProviderId::Ord, "NO") => &NO_ANGLES,
        (_, ProviderId::Ord, "DK") => &DK_ANGLES,
        _ => &[],
    }
}

/// `hello.sites[].products` and `elevations`: nothing for a grid station;
/// the lowest scan for any radar; every product built from several angles
/// where the engine knows the radar's angles, `HYBRID` only where it also
/// has a blockage table.
pub fn for_station(station: &Station) -> (Vec<&'static str>, Vec<Elevation>) {
    if station.kind == SiteKind::Grid {
        return (Vec::new(), Vec::new());
    }
    let angles = nominal_angles(station);
    let km = |m: f64| (m / 100.0).round() / 10.0;
    let elevations = angles
        .iter()
        .map(|&deg| Elevation {
            deg,
            beam_km50: km(beam_height_m(deg, 50_000.0)),
            beam_km100: km(beam_height_m(deg, 100_000.0)),
        })
        .collect();
    let products = VOCABULARY
        .iter()
        .map(|p| p.id)
        .filter(|&id| match id {
            REF => true,
            HYBRID => !angles.is_empty() && blockage(&station.id).is_some(),
            _ => !angles.is_empty(),
        })
        .collect();
    (products, elevations)
}

/// A station as `hello.sites[]` sends it.
pub fn site_entry(station: Station) -> crate::protocol::SiteEntry {
    let (products, elevations) = for_station(&station);
    crate::protocol::SiteEntry {
        station,
        products,
        elevations,
    }
}

/// What `choice` makes on `station`; `None` when the station cannot make it
/// (a grid station makes nothing to choose).
pub fn want_for(station: &Station, choice: &Choice) -> Option<Want> {
    let (products, _) = for_station(station);
    if !products.contains(&choice.id.as_str()) {
        return None;
    }
    Want::of(choice, nominal_angles(station), blockage(&station.id))
}

/// The angle a `REF` choice names on `station`, in degrees; `None` for any
/// other choice.
pub fn angle_deg(station: &Station, choice: &Choice) -> Option<f64> {
    (choice.id == REF && choice.elevation_index > 0)
        .then(|| {
            nominal_angles(station)
                .get(choice.elevation_index as usize)
                .copied()
        })
        .flatten()
}

/// The choice after switching to `to` (`docs/protocol.md`, station
/// switches): kept on a grid station for the next radar; a single angle
/// (`deg`, from the radar before) becomes `to`'s nearest in degrees; a
/// product `to` cannot make falls back to the lowest scan.
pub fn carry(choice: &Choice, deg: Option<f64>, to: &Station) -> Choice {
    if to.kind == SiteKind::Grid {
        return choice.clone();
    }
    if choice.id == REF {
        let index = deg
            .and_then(|d| nearest_index(nominal_angles(to), d))
            .unwrap_or(0);
        return Choice {
            id: REF.to_owned(),
            elevation_index: index as u32,
        };
    }
    if want_for(to, choice).is_some() {
        choice.clone()
    } else {
        Choice::default()
    }
}

/// `engine/data/blockage.json`: per radar id, 360 bytes (`Blockage`),
/// written by `scripts/blockage-tables.py`.
#[derive(Deserialize)]
struct BlockageFile {
    radars: HashMap<String, Vec<u8>>,
}

static BLOCKAGE: LazyLock<HashMap<String, &'static Blockage>> = LazyLock::new(|| {
    let file: BlockageFile = serde_json::from_str(include_str!("../data/blockage.json"))
        .expect("engine/data/blockage.json is valid");
    file.radars
        .into_iter()
        .filter_map(|(id, tenths)| {
            let tenths: [u8; 360] = tenths.try_into().ok()?;
            Some((id, &*Box::leak(Box::new(Blockage { tenths }))))
        })
        .collect()
});

/// A radar's blockage table, when the engine has one.
pub fn blockage(id: &str) -> Option<&'static Blockage> {
    BLOCKAGE.get(id).copied()
}

/// The index into `elevations` nearest `deg` (ties: the lower angle).
pub fn nearest_index(elevations: &[f64], deg: f64) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, &e) in elevations.iter().enumerate() {
        let d = (e - deg).abs();
        if best.is_none_or(|(_, held)| d < held) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// The beam centre's height above the antenna, in metres, at ground
/// distance `ground_m` for elevation `deg` (`hello.sites[].elevations`).
pub fn beam_height_m(deg: f64, ground_m: f64) -> f64 {
    let (e, theta) = (deg.to_radians(), ground_m / EARTH_M);
    EARTH_M * e.cos() / (e + theta).cos() - EARTH_M
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// A scan's geometry, from its `where` group, as its `Sweep` will carry it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TiltInfo {
    /// `where/elangle`, degrees.
    pub elangle: f64,
    pub first_gate_m: u32,
    pub gate_spacing_m: u32,
    pub gates: u16,
}

/// One decoded scan of a volume.
pub struct Tilt {
    pub elangle: f64,
    pub sweep: Sweep,
}

impl Tilt {
    pub fn info(&self) -> TiltInfo {
        TiltInfo {
            elangle: self.elangle,
            first_gate_m: self.sweep.first_gate_m,
            gate_spacing_m: self.sweep.gate_spacing_m,
            gates: self.sweep.gates,
        }
    }
}

/// The output grid's elevation: the lowest scan's nominal angle rounded to
/// 0.01°, as `frame.elevationDeg` carries it to the shader.
fn placement_deg(elangle: f64) -> f64 {
    (elangle * 100.0).round() / 100.0
}

/// Ground distance of slant range `r` at elevation `e` (radians): the
/// inverse of the lookup rule's `r = R sin(s/R) / cos(e + s/R)`.
fn ground_of(r: f64, e: f64) -> f64 {
    EARTH_M * (r * e.cos()).atan2(EARTH_M + r * e.sin())
}

/// Where one scan meets an output gate: the gate it covers it with, and
/// its beam centre's height there.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Hit {
    gate: usize,
    height: f64,
}

/// Per scan, per output gate, where it covers the gate (`docs/protocol.md`,
/// how each product is made). `lowest` indexes the scan whose grid is the
/// output's.
fn hits(infos: &[TiltInfo], lowest: usize) -> Vec<Vec<Option<Hit>>> {
    let base = infos[lowest];
    let e0 = placement_deg(base.elangle).to_radians();
    let grounds: Vec<f64> = (0..usize::from(base.gates))
        .map(|g| {
            let r = f64::from(base.first_gate_m) + g as f64 * f64::from(base.gate_spacing_m);
            ground_of(r, e0)
        })
        .collect();
    infos
        .iter()
        .map(|info| {
            let e = info.elangle.to_radians();
            grounds
                .iter()
                .map(|&s| {
                    let theta = s / EARTH_M;
                    let c = (e + theta).cos();
                    if c <= 1e-9 {
                        return None;
                    }
                    let r = EARTH_M * theta.sin() / c;
                    let gate = ((r - f64::from(info.first_gate_m))
                        / f64::from(info.gate_spacing_m))
                    .round();
                    (gate >= 0.0 && gate < f64::from(info.gates)).then(|| Hit {
                        gate: gate as usize,
                        height: EARTH_M * e.cos() / c - EARTH_M,
                    })
                })
                .collect()
        })
        .collect()
}

/// The index of the lowest scan (ties: the first).
fn lowest_of(infos: &[TiltInfo]) -> usize {
    let mut best = 0;
    for (i, info) in infos.iter().enumerate() {
        if info.elangle < infos[best].elangle {
            best = i;
        }
    }
    best
}

/// The index of the scan nearest `deg` (ties: the lower angle, then the
/// first).
fn nearest_of(infos: &[TiltInfo], deg: f64) -> usize {
    let mut best = 0;
    for (i, info) in infos.iter().enumerate() {
        let (d, held) = (
            (info.elangle - deg).abs(),
            (infos[best].elangle - deg).abs(),
        );
        if d < held || (d == held && info.elangle < infos[best].elangle) {
            best = i;
        }
    }
    best
}

/// The pseudo-CAPPI's scan at one output gate: of those covering it, the
/// one whose beam centre is nearest `height`; the lower angle on a tie.
fn cappi_pick(
    hits: &[Vec<Option<Hit>>],
    infos: &[TiltInfo],
    g: usize,
    height: f64,
) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (k, per_gate) in hits.iter().enumerate() {
        let Some(hit) = per_gate[g] else { continue };
        let d = (hit.height - height).abs();
        let better = match best {
            None => true,
            Some((held, held_d)) => {
                d < held_d || (d == held_d && infos[k].elangle < infos[held].elangle)
            }
        };
        if better {
            best = Some((k, d));
        }
    }
    best.map(|(k, _)| k)
}

/// The clear view's scan for one azimuth degree: the lowest at or above
/// the table's angle, else the highest.
fn hybrid_pick(infos: &[TiltInfo], tenths: u8) -> usize {
    let mut order: Vec<usize> = (0..infos.len()).collect();
    order.sort_by(|&a, &b| infos[a].elangle.total_cmp(&infos[b].elangle));
    order
        .iter()
        .copied()
        .find(|&k| (infos[k].elangle * 10.0).round() >= f64::from(tenths))
        .unwrap_or(*order.last().unwrap())
}

/// The scans `want` reads from a volume with these scans, ascending by
/// index: the same choices `compose` makes, from the geometry alone. Empty
/// for an empty volume.
pub fn needed(want: Want, infos: &[TiltInfo]) -> Vec<usize> {
    if infos.is_empty() {
        return Vec::new();
    }
    let lowest = lowest_of(infos);
    let mut picked = vec![false; infos.len()];
    picked[lowest] = true;
    match want {
        Want::Lowest => {}
        Want::Angle(deg) => {
            picked[lowest] = false;
            picked[nearest_of(infos, deg)] = true;
        }
        Want::Cappi(height) => {
            let hits = hits(infos, lowest);
            for g in 0..usize::from(infos[lowest].gates) {
                if let Some(k) = cappi_pick(&hits, infos, g, height) {
                    picked[k] = true;
                }
            }
        }
        Want::ColMax => {
            let hits = hits(infos, lowest);
            for (k, per_gate) in hits.iter().enumerate() {
                if per_gate.iter().any(Option::is_some) {
                    picked[k] = true;
                }
            }
        }
        Want::Hybrid(table) => {
            for &tenths in &table.tenths {
                picked[hybrid_pick(infos, tenths)] = true;
            }
        }
    }
    (0..infos.len()).filter(|&k| picked[k]).collect()
}

// ---------------------------------------------------------------------------
// Composing
// ---------------------------------------------------------------------------

/// Per output ray, the index of the scan's ray nearest its azimuth, within
/// `GAP_DEG` (ties: the first).
fn ray_map(output: &[Ray], scan: &[Ray]) -> Vec<Option<usize>> {
    output
        .iter()
        .map(|out| {
            let a = f64::from(out.azimuth_deg);
            let mut best: Option<(usize, f64)> = None;
            for (j, ray) in scan.iter().enumerate() {
                let d = (f64::from(ray.azimuth_deg) - a).abs();
                let d = d.min(360.0 - d);
                if best.is_none_or(|(_, held)| d < held) {
                    best = Some((j, d));
                }
            }
            best.filter(|&(_, d)| d <= GAP_DEG).map(|(j, _)| j)
        })
        .collect()
}

/// What `want` makes of a volume's decoded scans (at least the ones
/// `needed` named). `Lowest` and `Angle` return one scan as it is; the
/// other products are drawn on the lowest scan's rays and gates.
pub fn compose(want: Want, mut tilts: Vec<Tilt>) -> Result<Sweep, String> {
    if tilts.is_empty() {
        return Err("the volume holds no reflectivity scans".into());
    }
    // Ascending by angle, stable, so ties keep the decoder's order.
    tilts.sort_by(|a, b| a.elangle.total_cmp(&b.elangle));
    let infos: Vec<TiltInfo> = tilts.iter().map(Tilt::info).collect();
    match want {
        Want::Lowest => return Ok(tilts.swap_remove(0).sweep),
        Want::Angle(deg) => return Ok(tilts.swap_remove(nearest_of(&infos, deg)).sweep),
        _ => {}
    }
    let hits = hits(&infos, 0);
    let base = &tilts[0].sweep;
    let gates = usize::from(base.gates);
    let maps: Vec<Vec<Option<usize>>> = tilts
        .iter()
        .map(|t| ray_map(&base.rays, &t.sweep.rays))
        .collect();
    // The value scan `k` gives output ray `i` at output gate `g`.
    let value = |k: usize, i: usize, g: usize| -> Option<u8> {
        let hit = hits[k][g]?;
        Some(match maps[k][i] {
            Some(j) => tilts[k].sweep.rays[j].codes[hit.gate],
            None => 1,
        })
    };
    let per_gate_pick: Vec<Option<usize>> = match want {
        Want::Cappi(height) => (0..gates)
            .map(|g| cappi_pick(&hits, &infos, g, height))
            .collect(),
        _ => Vec::new(),
    };
    let elevation = placement_deg(tilts[0].elangle) as f32;
    let mut rays = Vec::with_capacity(base.rays.len());
    for (i, out) in base.rays.iter().enumerate() {
        let hybrid = match want {
            Want::Hybrid(table) => {
                let degree = (f64::from(out.azimuth_deg).floor() as i64).rem_euclid(360) as usize;
                Some(hybrid_pick(&infos, table.tenths[degree]))
            }
            _ => None,
        };
        let codes: Vec<u8> = (0..gates)
            .map(|g| match want {
                Want::Cappi(_) => per_gate_pick[g].and_then(|k| value(k, i, g)).unwrap_or(1),
                Want::Hybrid(_) => hybrid.and_then(|k| value(k, i, g)).unwrap_or(1),
                Want::ColMax => {
                    let (mut top, mut below) = (None::<u8>, false);
                    for k in 0..tilts.len() {
                        match value(k, i, g) {
                            Some(code) if code >= 2 => {
                                top = Some(top.map_or(code, |t| t.max(code)))
                            }
                            Some(0) => below = true,
                            _ => {}
                        }
                    }
                    top.unwrap_or(if below { 0 } else { 1 })
                }
                Want::Lowest | Want::Angle(_) => unreachable!("returned above"),
            })
            .collect();
        rays.push(Ray {
            azimuth_deg: out.azimuth_deg,
            elevation_deg: elevation,
            time_ms: out.time_ms,
            codes,
        });
    }
    Ok(Sweep {
        rays,
        start_ms: base.start_ms,
        end_ms: tilts
            .iter()
            .map(|t| t.sweep.end_ms)
            .max()
            .unwrap_or(base.end_ms),
        gates: base.gates,
        first_gate_m: base.first_gate_m,
        gate_spacing_m: base.gate_spacing_m,
        scale: base.scale,
        offset: base.offset,
        code1_status: OUTSIDE_COVERAGE,
    })
}

/// What `want` makes of one ODIM volume: the lowest scan as the provider has
/// always decoded it (`lowest`: SMHI's first dataset, ORD's lowest angle),
/// or the scans `needed` names, composed (`Scan::Product`).
pub fn decode_volume<R: std::io::Read + std::io::Seek + Send + 'static>(
    reader: R,
    want: Want,
    lowest: crate::odim::Tilt,
) -> Result<crate::smhi_live::Scan, String> {
    use crate::smhi_live::Scan;
    if want.is_lowest() {
        return crate::odim::decode_lowest(reader, lowest)
            .map(Scan::Polar)
            .map_err(|e| e.to_string());
    }
    // SMHI's lowest scan is `/dataset1` (`odim::Tilt::First`); its free
    // copy below is only that scan when `/dataset1` is the lowest angle.
    let mut first_is_lowest = false;
    let tilts = crate::odim::decode_tilts(reader, |infos| {
        first_is_lowest = lowest_of(infos) == 0;
        needed(want, infos)
    })
    .map_err(|e| e.to_string())?;
    // CAPPI, CMAX and HYBRID read the lowest scan for their rays and gates
    // (`needed`), so it comes free (S26): the sweep `decode_lowest` makes of
    // the same volume (the lowest angle, ties the first dataset, as
    // `odim::tilt_path` picks), for the station's `e0` ring.
    let free = match want {
        Want::Lowest | Want::Angle(_) => None,
        _ if lowest == crate::odim::Tilt::First && !first_is_lowest => None,
        _ => tilts
            .iter()
            .min_by(|a, b| a.elangle.total_cmp(&b.elangle))
            .map(|t| Box::new(copy_sweep(&t.sweep))),
    };
    compose(want, tilts).map(|sweep| Scan::Product(sweep, want, free))
}

/// A field-by-field copy (`Sweep` is not `Clone`).
fn copy_sweep(sweep: &Sweep) -> Sweep {
    Sweep {
        rays: sweep
            .rays
            .iter()
            .map(|r| Ray {
                azimuth_deg: r.azimuth_deg,
                elevation_deg: r.elevation_deg,
                time_ms: r.time_ms,
                codes: r.codes.clone(),
            })
            .collect(),
        start_ms: sweep.start_ms,
        end_ms: sweep.end_ms,
        gates: sweep.gates,
        first_gate_m: sweep.first_gate_m,
        gate_spacing_m: sweep.gate_spacing_m,
        scale: sweep.scale,
        offset: sweep.offset,
        code1_status: sweep.code1_status,
    }
}

/// ` cappi1` after a provenance's file name for a product other than the
/// lowest scan, so engine.log's bytes per frame say which product paid them.
pub fn provenance_tag(want: Want) -> String {
    if want.is_lowest() {
        String::new()
    } else {
        format!(" [{}]", want.variant())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scan of `rays` rays and `gates` gates at `elangle`, every code
    /// `code` (or `f(ray, gate)`).
    fn scan(
        elangle: f64,
        rays: usize,
        gates: u16,
        spacing: u32,
        f: impl Fn(usize, usize) -> u8,
    ) -> Tilt {
        Tilt {
            elangle,
            sweep: Sweep {
                rays: (0..rays)
                    .map(|i| Ray {
                        azimuth_deg: ((i as f64 + 0.5) * 360.0 / rays as f64) as f32,
                        elevation_deg: elangle as f32,
                        time_ms: 1000 + i as i64,
                        codes: (0..usize::from(gates)).map(|g| f(i, g)).collect(),
                    })
                    .collect(),
                start_ms: 1000,
                end_ms: 2000 + (elangle * 10.0) as i64,
                gates,
                first_gate_m: spacing / 2,
                gate_spacing_m: spacing,
                scale: 2.0,
                offset: 66.0,
                code1_status: OUTSIDE_COVERAGE,
            },
        }
    }

    /// SMHI's ten angles and their gates (the Vara volume of 2026-09-13).
    fn smhi() -> Vec<TiltInfo> {
        [
            (0.5, 480, 500),
            (1.0, 480, 500),
            (1.5, 480, 500),
            (2.0, 480, 500),
            (2.5, 592, 250),
            (4.0, 592, 250),
            (8.0, 592, 250),
            (14.0, 592, 250),
            (24.0, 480, 250),
            (40.0, 480, 250),
        ]
        .into_iter()
        .map(|(elangle, gates, spacing)| TiltInfo {
            elangle,
            first_gate_m: spacing / 2,
            gate_spacing_m: spacing,
            gates,
        })
        .collect()
    }

    #[test]
    fn the_vocabulary_and_choices() {
        let ids: Vec<&str> = VOCABULARY.iter().map(|p| p.id).collect();
        assert_eq!(ids, ["REF", "HYBRID", "CAPPI1", "CAPPI2", "CMAX"]);
        let elevations = [0.5, 1.0, 1.5];
        let choice = |id: &str, index| Choice {
            id: id.into(),
            elevation_index: index,
        };
        assert_eq!(
            Want::of(&Choice::default(), &elevations, None),
            Some(Want::Lowest)
        );
        assert_eq!(
            Want::of(&choice("REF", 2), &elevations, None),
            Some(Want::Angle(1.5))
        );
        assert_eq!(Want::of(&choice("REF", 3), &elevations, None), None);
        assert_eq!(
            Want::of(&choice("CAPPI1", 0), &[], None),
            Some(Want::Cappi(1000.0))
        );
        assert_eq!(
            Want::of(&choice("CAPPI2", 1), &elevations, None),
            None,
            "an index with another product"
        );
        assert_eq!(
            Want::of(&choice("HYBRID", 0), &elevations, None),
            None,
            "no blockage table"
        );
        assert_eq!(Want::of(&choice("VIL", 0), &elevations, None), None);
        for (want, variant, id) in [
            (Want::Lowest, "e0", "REF"),
            (Want::Angle(4.0), "a40", "REF"),
            (Want::Angle(0.7), "a7", "REF"),
            (Want::Cappi(1000.0), "cappi1", "CAPPI1"),
            (Want::Cappi(2000.0), "cappi2", "CAPPI2"),
            (Want::ColMax, "cmax", "CMAX"),
        ] {
            assert_eq!((want.variant().as_str(), want.product().0), (variant, id));
        }
        assert_eq!(variant_of("vara-20260913T105503Z-cappi1"), "cappi1");
        assert_eq!(variant_of("vara-20260913T105503Z-e0"), "e0");
        assert_eq!(variant_of("vara-20260913T105503Z-a240"), "a240");
        assert_eq!(variant_of("vara-20260913T105503Z-clear"), "clear");
        // An id that names no product is the lowest scan (a pre-S20 or
        // synthetic frame, scripts/check-popover.sh's `popover-test-0`).
        assert_eq!(variant_of("popover-test-0"), "e0");
        assert_eq!(variant_of("vara-loading"), "e0");
        assert_eq!(variant_of("x-abc"), "e0");
        use crate::providers::ord;
        use crate::smhi_live as smhi;
        let smhi_depths = (smhi::BACKFILL, smhi::PRODUCT_BACKFILL);
        assert_eq!(Want::Lowest.backfill(smhi_depths.0, smhi_depths.1), 60);
        assert_eq!(Want::ColMax.backfill(smhi_depths.0, smhi_depths.1), 12);
        assert_eq!(
            Want::ColMax.backfill(ord::BACKFILL, ord::PRODUCT_BACKFILL),
            24
        );
        assert_eq!(
            Want::ColMax.backfill(10, 24),
            10,
            "never deeper than the lowest scan"
        );
        assert_eq!(nearest_index(&[0.5, 1.0, 2.4, 3.2], 2.6), Some(2));
        assert_eq!(
            nearest_index(&[0.5, 1.0], 0.75),
            Some(0),
            "a tie takes the lower"
        );
        assert_eq!(nearest_index(&[], 1.0), None);
    }

    /// The plan's table (§S20), rounded by hand, within 0.2 km: beam centre
    /// at 50 and 100 km. The exact values are the contract's formula (8° is
    /// 7.18 and 14.67 km, where the plan wrote 7.1 and 14.5).
    #[test]
    fn beam_heights_match_the_plans_table() {
        for (deg, at50, at100) in [
            (0.5, 0.6, 1.5),
            (1.0, 1.0, 2.3),
            (2.0, 1.9, 4.1),
            (4.0, 3.6, 7.6),
            (8.0, 7.1, 14.5),
        ] {
            let km = |m: f64| m / 1000.0;
            let at = |ground: f64| km(beam_height_m(deg, ground));
            assert!(
                (at(50_000.0) - at50).abs() <= 0.2,
                "{deg}° at 50 km: {}",
                at(50_000.0)
            );
            assert!(
                (at(100_000.0) - at100).abs() <= 0.2,
                "{deg}° at 100 km: {}",
                at(100_000.0)
            );
        }
        assert_eq!((beam_height_m(8.0, 50_000.0) / 10.0).round() / 100.0, 7.18);
    }

    #[test]
    fn the_output_grid_maps_back_onto_the_lowest_scan() {
        let infos = smhi();
        let hits = hits(&infos, 0);
        for (g, hit) in hits[0].iter().enumerate() {
            assert_eq!(
                hit.map(|h| h.gate),
                Some(g),
                "the lowest scan covers its own gates"
            );
        }
        // 2.5° reaches 148 km (592 gates of 250 m): about the first 296
        // output gates of 500 m.
        let reach = hits[4].iter().filter(|h| h.is_some()).count();
        assert!((294..=298).contains(&reach), "{reach}");
        // Heights grow with angle and distance.
        let at = |k: usize, g: usize| hits[k][g].unwrap().height;
        assert!(at(0, 100) < at(1, 100) && at(1, 100) < at(3, 100));
        assert!((at(0, 99) - beam_height_m(0.5, 49_750.0)).abs() < 1.0);
    }

    #[test]
    fn needed_angles_per_product() {
        let infos = smhi();
        assert_eq!(needed(Want::Lowest, &infos), [0]);
        assert_eq!(needed(Want::Angle(4.0), &infos), [5]);
        assert_eq!(
            needed(Want::Angle(3.0), &infos),
            [4],
            "2.5° is nearer 3.0° than 4.0°"
        );
        assert_eq!(needed(Want::ColMax, &infos), (0..10).collect::<Vec<_>>());
        let cappi1 = needed(Want::Cappi(1000.0), &infos);
        let cappi2 = needed(Want::Cappi(2000.0), &infos);
        assert_eq!(cappi1[0], 0);
        assert!(
            cappi1.contains(&3) && cappi2.contains(&6),
            "{cappi1:?} {cappi2:?}"
        );
        // Near the radar only the high angles reach 1-2 km.
        assert!(cappi1.contains(&9) || cappi1.contains(&8), "{cappi1:?}");
        let blocked = Box::leak(Box::new(Blockage {
            tenths: std::array::from_fn(|d| if d < 90 { 15 } else { 0 }),
        }));
        assert_eq!(needed(Want::Hybrid(blocked), &infos), [0, 2]);
        let all_high = Box::leak(Box::new(Blockage { tenths: [250; 360] }));
        assert_eq!(
            needed(Want::Hybrid(all_high), &infos),
            [0, 9],
            "above every angle: the highest"
        );
        assert!(needed(Want::ColMax, &[]).is_empty());
    }

    #[test]
    fn a_pseudo_cappi_takes_each_gate_from_the_nearest_beam() {
        // Three scans marking their own code: 0.5° -> 10, 1.5° -> 20, 4° -> 30.
        let tilts = || {
            vec![
                scan(4.0, 360, 400, 250, |_, _| 30),
                scan(0.5, 360, 480, 500, |_, _| 10),
                scan(1.5, 360, 480, 500, |_, _| 20),
            ]
        };
        let out = compose(Want::Cappi(1000.0), tilts()).unwrap();
        assert_eq!(
            (out.rays.len(), out.gates, out.gate_spacing_m),
            (360, 480, 500)
        );
        assert_eq!(out.rays[0].elevation_deg, 0.5);
        assert_eq!(out.end_ms, 2040, "the last scan's end");
        let row = &out.rays[17].codes;
        // Near the radar 4° is nearest 1 km, then 1.5°, then 0.5° far out.
        assert_eq!(row[20], 30, "10 km");
        assert_eq!(row[80], 20, "40 km");
        assert_eq!(row[300], 10, "150 km");
        let order: Vec<u8> = row.iter().copied().fold(Vec::new(), |mut runs, c| {
            if runs.last() != Some(&c) {
                runs.push(c);
            }
            runs
        });
        assert_eq!(order, [30, 20, 10], "one band each, outward");
        // Composing from only the needed scans gives the same answer.
        let infos: Vec<TiltInfo> = tilts().iter().map(Tilt::info).collect();
        let keep = needed(Want::Cappi(1000.0), &infos);
        let subset: Vec<Tilt> = tilts()
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep.contains(i))
            .map(|(_, t)| t)
            .collect();
        let again = compose(Want::Cappi(1000.0), subset).unwrap();
        assert!(
            again
                .rays
                .iter()
                .zip(&out.rays)
                .all(|(a, b)| a.codes == b.codes)
        );
    }

    #[test]
    fn the_column_maximum_prefers_measured_then_below_threshold() {
        // Ray 0: measured only at 1.5°. Ray 1: nothing measured, one scan
        // below threshold. Ray 2: nodata everywhere.
        let code = |values: [u8; 3]| move |i: usize, _| values[i.min(2)];
        let tilts = vec![
            scan(0.5, 3, 10, 500, code([0, 1, 1])),
            scan(1.5, 3, 10, 500, code([90, 0, 1])),
            scan(3.0, 3, 10, 500, code([40, 1, 1])),
        ];
        let out = compose(Want::ColMax, tilts).unwrap();
        assert_eq!(out.rays[0].codes[3], 90);
        assert_eq!(out.rays[1].codes[3], 0);
        assert_eq!(out.rays[2].codes[3], 1);
    }

    #[test]
    fn an_angle_is_the_scan_itself_and_a_gap_is_nodata() {
        let pick = compose(
            Want::Angle(1.4),
            vec![
                scan(0.5, 360, 480, 500, |_, _| 10),
                scan(1.5, 360, 480, 500, |_, _| 20),
            ],
        )
        .unwrap();
        assert_eq!(pick.rays[0].codes[0], 20);
        assert_eq!(pick.rays[0].elevation_deg, 1.5);
        // A higher scan with rays only in the first quadrant: elsewhere the
        // column max has only the lowest scan, and a clear view pointed at
        // the higher scan draws nodata.
        let partial = || {
            let mut high = scan(1.5, 360, 480, 500, |_, _| 50);
            high.sweep.rays.truncate(90);
            vec![scan(0.5, 360, 480, 500, |_, _| 10), high]
        };
        let out = compose(Want::ColMax, partial()).unwrap();
        assert_eq!(out.rays[10].codes[5], 50);
        assert_eq!(out.rays[200].codes[5], 10);
        let up = Box::leak(Box::new(Blockage { tenths: [15; 360] }));
        let clear = compose(Want::Hybrid(up), partial()).unwrap();
        assert_eq!(clear.rays[10].codes[5], 50);
        assert_eq!(clear.rays[200].codes[5], 1);
    }

    fn polar(provider: ProviderId, country: &str) -> Station {
        Station {
            id: "x".into(),
            provider,
            country: country.into(),
            kind: SiteKind::Polar,
            ..Station::default()
        }
    }

    #[test]
    fn what_each_station_offers() {
        let (products, elevations) = for_station(&polar(ProviderId::Smhi, "SE"));
        assert_eq!(
            products,
            ["REF", "CAPPI1", "CAPPI2", "CMAX"],
            "no blockage table: no HYBRID"
        );
        assert_eq!(elevations.len(), 10);
        assert_eq!(
            elevations[0],
            Elevation {
                deg: 0.5,
                beam_km50: 0.6,
                beam_km100: 1.5
            }
        );
        assert_eq!(for_station(&polar(ProviderId::Ord, "NO")).1.len(), 12);
        assert_eq!(for_station(&polar(ProviderId::Ord, "DK")).1[1].deg, 0.7);
        // FMI: one file per angle, so the lowest scan alone.
        assert_eq!(
            for_station(&polar(ProviderId::Ord, "FI")),
            (vec!["REF"], vec![])
        );
        let grid = Station {
            kind: SiteKind::Grid,
            ..polar(ProviderId::Smhi, "SE")
        };
        assert_eq!(for_station(&grid), (vec![], vec![]));
        let choice = |id: &str, index| Choice {
            id: id.into(),
            elevation_index: index,
        };
        let smhi = polar(ProviderId::Smhi, "SE");
        assert_eq!(want_for(&smhi, &choice("CMAX", 0)), Some(Want::ColMax));
        assert_eq!(want_for(&smhi, &choice("REF", 5)), Some(Want::Angle(4.0)));
        assert_eq!(want_for(&smhi, &choice("HYBRID", 0)), None);
        assert_eq!(
            want_for(&polar(ProviderId::Ord, "FI"), &choice("CMAX", 0)),
            None
        );
        assert_eq!(
            want_for(&grid, &choice("REF", 0)),
            None,
            "a composite has no products"
        );
        assert_eq!(angle_deg(&smhi, &choice("REF", 5)), Some(4.0));
        assert_eq!(angle_deg(&smhi, &choice("CMAX", 0)), None);
    }

    #[test]
    fn a_switch_carries_the_product_over() {
        let choice = |id: &str, index| Choice {
            id: id.into(),
            elevation_index: index,
        };
        let (smhi, norway, finland) = (
            polar(ProviderId::Smhi, "SE"),
            polar(ProviderId::Ord, "NO"),
            polar(ProviderId::Ord, "FI"),
        );
        let grid = Station {
            kind: SiteKind::Grid,
            ..smhi.clone()
        };
        // SMHI's 4.0° (index 5) is MET Norway's 4.2° (index 5); 2.5° is
        // nearest 2.4° (index 3).
        assert_eq!(
            carry(&choice("REF", 5), Some(4.0), &norway),
            choice("REF", 5)
        );
        assert_eq!(
            carry(&choice("REF", 4), Some(2.5), &norway),
            choice("REF", 3)
        );
        assert_eq!(
            carry(&choice("CAPPI2", 0), None, &norway),
            choice("CAPPI2", 0)
        );
        // FMI makes neither: the lowest scan.
        assert_eq!(
            carry(&choice("CAPPI2", 0), None, &finland),
            Choice::default()
        );
        assert_eq!(
            carry(&choice("REF", 4), Some(2.5), &finland),
            Choice::default()
        );
        // A composite keeps the choice for the next radar.
        assert_eq!(carry(&choice("REF", 4), Some(2.5), &grid), choice("REF", 4));
        assert_eq!(carry(&Choice::default(), None, &smhi), Choice::default());
    }

    /// The multi-angle fixtures against their answer keys
    /// (`golden/produce-products.py`: h5py, every tilt): each product of each
    /// provider format, decoded by `decode_volume`, which reads only the
    /// tilts `needed` names, byte for byte.
    #[test]
    fn products_match_their_answer_keys() {
        use std::io::Read;
        const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../");
        for (fixture, golden, lowest) in [
            (
                "radar_vara_qcvol_202609131055_tilts.h5",
                "vara-20260913",
                crate::odim::Tilt::First,
            ),
            (
                "ord_nohur_202609140930_tilts.h5",
                "nohur-20260914",
                crate::odim::Tilt::Lowest,
            ),
            (
                "ord_dksin_202609140940_tilts.h5",
                "dksin-20260914",
                crate::odim::Tilt::Lowest,
            ),
        ] {
            let key: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(format!("{ROOT}golden/{golden}/products.json")).unwrap(),
            )
            .unwrap();
            let tenths: Vec<u8> = key["blockageTenths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u8)
                .collect();
            let table: &'static Blockage = Box::leak(Box::new(Blockage {
                tenths: tenths.try_into().unwrap(),
            }));
            let products = key["products"].as_object().unwrap();
            assert_eq!(products.len(), 6, "{golden}");
            for (variant, product) in products {
                let want = match variant.as_str() {
                    "cappi1" => Want::Cappi(1000.0),
                    "cappi2" => Want::Cappi(2000.0),
                    "cmax" => Want::ColMax,
                    "clear" => Want::Hybrid(table),
                    angle => Want::Angle(angle[1..].parse::<f64>().unwrap() / 10.0),
                };
                assert_eq!(want.variant(), *variant);
                let file = std::fs::File::open(format!("{ROOT}data/raw/{fixture}")).unwrap();
                let crate::smhi_live::Scan::Product(sweep, ..) =
                    decode_volume(file, want, lowest).unwrap()
                else {
                    panic!("{golden} {variant}: not a product")
                };
                let mut expected = Vec::new();
                let gz = format!(
                    "{ROOT}golden/{golden}/{}",
                    product["file"].as_str().unwrap()
                );
                flate2::read::GzDecoder::new(std::fs::File::open(gz).unwrap())
                    .read_to_end(&mut expected)
                    .unwrap();
                assert_eq!(
                    (
                        sweep.rays.len() as u64,
                        u64::from(sweep.gates),
                        u64::from(sweep.first_gate_m),
                        u64::from(sweep.gate_spacing_m)
                    ),
                    (
                        product["rays"].as_u64().unwrap(),
                        product["gates"].as_u64().unwrap(),
                        product["firstGateM"].as_u64().unwrap(),
                        product["gateSpacingM"].as_u64().unwrap()
                    ),
                    "{golden} {variant}: geometry"
                );
                if !variant.starts_with('a') {
                    let placement = key["placementDeg"].as_f64().unwrap() as f32;
                    assert!(sweep.rays.iter().all(|r| r.elevation_deg == placement));
                }
                let codes: Vec<u8> = sweep
                    .rays
                    .iter()
                    .flat_map(|r| r.codes.iter().copied())
                    .collect();
                assert_eq!(codes.len(), expected.len(), "{golden} {variant}");
                let differ = codes.iter().zip(&expected).filter(|(a, b)| a != b).count();
                assert_eq!(
                    differ,
                    0,
                    "{golden} {variant}: {differ} of {} codes differ",
                    codes.len()
                );
            }
        }
    }

    /// CAPPI, CMAX and HYBRID carry the lowest scan they read (S26): the
    /// very sweep `decode_lowest` makes of the same volume, for the `e0`
    /// ring. The lowest scan itself and one other angle carry none.
    #[test]
    fn a_product_read_carries_its_lowest_scan() {
        use crate::smhi_live::Scan;
        const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../data/raw/");
        let table: &'static Blockage = Box::leak(Box::new(Blockage { tenths: [8; 360] }));
        let codes = |s: &Sweep| -> Vec<u8> {
            s.rays
                .iter()
                .flat_map(|r| r.codes.iter().copied())
                .collect()
        };
        let rays = |s: &Sweep| -> Vec<(f32, f32, i64)> {
            s.rays
                .iter()
                .map(|r| (r.azimuth_deg, r.elevation_deg, r.time_ms))
                .collect()
        };
        for (fixture, lowest) in [
            (
                "radar_vara_qcvol_202609131055_tilts.h5",
                crate::odim::Tilt::First,
            ),
            ("ord_nohur_202609140930_tilts.h5", crate::odim::Tilt::Lowest),
            ("ord_dksin_202609140940_tilts.h5", crate::odim::Tilt::Lowest),
        ] {
            let open = || std::fs::File::open(format!("{ROOT}{fixture}")).unwrap();
            let expected = crate::odim::decode_lowest(open(), lowest).unwrap();
            for want in [Want::Cappi(1000.0), Want::ColMax, Want::Hybrid(table)] {
                let Scan::Product(_, _, Some(free)) = decode_volume(open(), want, lowest).unwrap()
                else {
                    panic!("{fixture} {}: no lowest scan", want.variant())
                };
                assert_eq!(
                    (
                        free.start_ms,
                        free.end_ms,
                        free.gates,
                        free.first_gate_m,
                        free.gate_spacing_m,
                        free.scale,
                        free.offset
                    ),
                    (
                        expected.start_ms,
                        expected.end_ms,
                        expected.gates,
                        expected.first_gate_m,
                        expected.gate_spacing_m,
                        expected.scale,
                        expected.offset
                    ),
                    "{fixture} {}",
                    want.variant()
                );
                assert!(rays(&free) == rays(&expected), "{fixture}: rays");
                assert!(codes(&free) == codes(&expected), "{fixture}: codes");
            }
            for want in [Want::Lowest, Want::Angle(4.0)] {
                let scan = decode_volume(open(), want, lowest).unwrap();
                assert!(
                    matches!(scan, Scan::Polar(_) | Scan::Product(_, _, None)),
                    "{fixture} {}",
                    want.variant()
                );
            }
        }
    }

    /// The vendored blockage tables (`scripts/blockage-tables.py`): 360
    /// entries per radar, every radar a table station, and those radars
    /// offer "Clear view" with their own table.
    #[test]
    fn blockage_tables_load_and_offer_clear_view() {
        let table = crate::providers::table();
        for id in ["vara", "ostersund", "nohur", "nosta"] {
            let station = table.sites.iter().find(|s| s.id == id).unwrap();
            assert!(blockage(id).is_some(), "{id}");
            assert_eq!(
                for_station(station).0,
                ["REF", "HYBRID", "CAPPI1", "CAPPI2", "CMAX"],
                "{id}"
            );
            let choice = Choice {
                id: HYBRID.into(),
                elevation_index: 0,
            };
            assert_eq!(want_for(station, &choice), blockage(id).map(Want::Hybrid));
        }
        let file: serde_json::Value =
            serde_json::from_str(include_str!("../data/blockage.json")).unwrap();
        for (id, tenths) in file["radars"].as_object().unwrap() {
            assert_eq!(tenths.as_array().unwrap().len(), 360, "{id}");
            assert!(
                table.sites.iter().any(|s| &s.id == id),
                "{id} is not a table station"
            );
        }
    }

    /// SMHI's product plan reads every product from the multi-angle
    /// fixture through the range reader within its budget; DEC-2's plan,
    /// sized for one tilt, is not what a product is read with.
    #[test]
    fn smhi_products_fit_their_range_plan() {
        use crate::smhi_live::{DEC2, PRODUCT_PLAN, RangeReader, RangeSource};
        struct Local(Vec<u8>);
        impl RangeSource for Local {
            fn get(&mut self, offset: u64, len: u64) -> std::io::Result<(Vec<u8>, Option<u64>)> {
                let total = self.0.len() as u64;
                let end = (offset + len).min(total);
                Ok((self.0[offset as usize..end as usize].to_vec(), Some(total)))
            }
        }
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../data/raw/radar_vara_qcvol_202609131055_tilts.h5"
        ))
        .unwrap();
        const {
            assert!(
                PRODUCT_PLAN.max_requests > DEC2.max_requests && PRODUCT_PLAN.block > DEC2.block
            )
        };
        for want in [
            Want::Angle(8.0),
            Want::Cappi(1000.0),
            Want::Cappi(2000.0),
            Want::ColMax,
        ] {
            let reader = RangeReader::open_planned(
                Box::new(Local(bytes.clone())),
                PRODUCT_PLAN,
                std::time::Duration::ZERO,
            )
            .unwrap();
            let traffic = reader.traffic();
            let scan = decode_volume(reader, want, crate::odim::Tilt::First);
            assert!(scan.is_ok(), "{}: {:?}", want.variant(), scan.err());
            assert!(traffic.requests() <= PRODUCT_PLAN.max_requests);
        }
    }

    /// What each product costs over the range reader, offline: requests and
    /// bytes per block size, with no budget, on whole volumes kept outside
    /// the repo (`S20_VOLUMES=vara:/path.h5,nohur:/path.h5`, then
    /// `cargo test range_cost_per_product -- --ignored --nocapture`). The
    /// providers' product budgets are sized from its output.
    #[test]
    #[ignore = "needs whole volumes outside the repo (S20_VOLUMES)"]
    fn range_cost_per_product() {
        use crate::smhi_live::{RangePlan, RangeReader, RangeSource};
        use std::sync::Arc;
        struct Local(Arc<Vec<u8>>);
        impl RangeSource for Local {
            fn get(&mut self, offset: u64, len: u64) -> std::io::Result<(Vec<u8>, Option<u64>)> {
                let total = self.0.len() as u64;
                let end = (offset + len).min(total);
                Ok((self.0[offset as usize..end as usize].to_vec(), Some(total)))
            }
        }
        let Ok(list) = std::env::var("S20_VOLUMES") else {
            return;
        };
        for item in list.split(',') {
            let (name, path) = item.split_once(':').unwrap();
            let bytes = Arc::new(std::fs::read(path).unwrap());
            let lowest = if name == "vara" {
                crate::odim::Tilt::First
            } else {
                crate::odim::Tilt::Lowest
            };
            println!("{name}: {} bytes", bytes.len());
            for block in [4096u64, 8192, 16384, 65536] {
                let plan = RangePlan {
                    prefetch: 64 * 1024,
                    block,
                    max_requests: 100_000,
                    max_bytes: u64::MAX,
                };
                for want in [
                    Want::Lowest,
                    Want::Angle(4.0),
                    Want::Cappi(1000.0),
                    Want::Cappi(2000.0),
                    Want::ColMax,
                ] {
                    let reader = RangeReader::open_planned(
                        Box::new(Local(bytes.clone())),
                        plan,
                        std::time::Duration::ZERO,
                    )
                    .unwrap();
                    let traffic = reader.traffic();
                    let result = decode_volume(reader, want, lowest);
                    println!(
                        "  block {block:>5}: {:<7} {:>4} requests {:>9} bytes {}",
                        want.variant(),
                        traffic.requests(),
                        traffic.bytes(),
                        result.map_or_else(|e| e, |_| "ok".into())
                    );
                }
            }
        }
    }
}
