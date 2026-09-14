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

use crate::sweep::{OUTSIDE_COVERAGE, Ray, Sweep};
use serde::Serialize;

/// The effective earth radius of the lookup rule (`ui/shaders/radar.frag`).
pub const EARTH_M: f64 = 6_371_000.0 * 4.0 / 3.0;
/// A scan's ray farther than this from an output ray's azimuth does not
/// reach it (the texture's own gap rule, `sweep::GAP_DEG`).
pub const GAP_DEG: f64 = 0.75;
/// Frames any product but the lowest scan backfills after a switch: two
/// hours, what both clients buffer. Its ring still fills to
/// `catalog::RING` as live frames arrive.
pub const BACKFILL: usize = 24;
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

impl Choice {
    pub fn is_lowest(&self) -> bool {
        self.id == REF && self.elevation_index == 0
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
    pub fn of(choice: &Choice, elevations: &[f64], blockage: Option<&'static Blockage>) -> Option<Want> {
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

    /// Frames a switch backfills: the provider's own depth for the lowest
    /// scan, `BACKFILL` for any other product.
    pub fn backfill(&self, provider: usize) -> usize {
        if self.is_lowest() {
            provider
        } else {
            provider.min(BACKFILL)
        }
    }
}

/// The frame id variant of a frame id (its last `-` part).
pub fn variant_of(frame_id: &str) -> &str {
    frame_id.rsplit('-').next().unwrap_or_default()
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
                    let gate =
                        ((r - f64::from(info.first_gate_m)) / f64::from(info.gate_spacing_m)).round();
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
        let (d, held) = ((info.elangle - deg).abs(), (infos[best].elangle - deg).abs());
        if d < held || (d == held && info.elangle < infos[best].elangle) {
            best = i;
        }
    }
    best
}

/// The pseudo-CAPPI's scan at one output gate: of those covering it, the
/// one whose beam centre is nearest `height`; the lower angle on a tie.
fn cappi_pick(hits: &[Vec<Option<Hit>>], infos: &[TiltInfo], g: usize, height: f64) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (k, per_gate) in hits.iter().enumerate() {
        let Some(hit) = per_gate[g] else { continue };
        let d = (hit.height - height).abs();
        let better = match best {
            None => true,
            Some((held, held_d)) => d < held_d || (d == held_d && infos[k].elangle < infos[held].elangle),
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
    let maps: Vec<Vec<Option<usize>>> = tilts.iter().map(|t| ray_map(&base.rays, &t.sweep.rays)).collect();
    // The value scan `k` gives output ray `i` at output gate `g`.
    let value = |k: usize, i: usize, g: usize| -> Option<u8> {
        let hit = hits[k][g]?;
        Some(match maps[k][i] {
            Some(j) => tilts[k].sweep.rays[j].codes[hit.gate],
            None => 1,
        })
    };
    let per_gate_pick: Vec<Option<usize>> = match want {
        Want::Cappi(height) => (0..gates).map(|g| cappi_pick(&hits, &infos, g, height)).collect(),
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
                            Some(code) if code >= 2 => top = Some(top.map_or(code, |t| t.max(code))),
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
        end_ms: tilts.iter().map(|t| t.sweep.end_ms).max().unwrap_or(base.end_ms),
        gates: base.gates,
        first_gate_m: base.first_gate_m,
        gate_spacing_m: base.gate_spacing_m,
        scale: base.scale,
        offset: base.offset,
        code1_status: OUTSIDE_COVERAGE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scan of `rays` rays and `gates` gates at `elangle`, every code
    /// `code` (or `f(ray, gate)`).
    fn scan(elangle: f64, rays: usize, gates: u16, spacing: u32, f: impl Fn(usize, usize) -> u8) -> Tilt {
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
        assert_eq!(Want::of(&Choice::default(), &elevations, None), Some(Want::Lowest));
        assert_eq!(Want::of(&choice("REF", 2), &elevations, None), Some(Want::Angle(1.5)));
        assert_eq!(Want::of(&choice("REF", 3), &elevations, None), None);
        assert_eq!(Want::of(&choice("CAPPI1", 0), &[], None), Some(Want::Cappi(1000.0)));
        assert_eq!(Want::of(&choice("CAPPI2", 1), &elevations, None), None, "an index with another product");
        assert_eq!(Want::of(&choice("HYBRID", 0), &elevations, None), None, "no blockage table");
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
        assert_eq!(Want::Lowest.backfill(60), 60);
        assert_eq!(Want::ColMax.backfill(60), 24);
        assert_eq!(nearest_index(&[0.5, 1.0, 2.4, 3.2], 2.6), Some(2));
        assert_eq!(nearest_index(&[0.5, 1.0], 0.75), Some(0), "a tie takes the lower");
        assert_eq!(nearest_index(&[], 1.0), None);
    }

    /// The plan's table (§S20): beam centre at 50 and 100 km.
    #[test]
    fn beam_heights_match_the_plans_table() {
        for (deg, at50, at100) in [(0.5, 0.6, 1.5), (1.0, 1.0, 2.3), (2.0, 1.9, 4.1), (4.0, 3.6, 7.6), (8.0, 7.1, 14.5)] {
            let km = |m: f64| (m / 100.0).round() / 10.0;
            assert_eq!(km(beam_height_m(deg, 50_000.0)), at50, "{deg}° at 50 km");
            assert_eq!(km(beam_height_m(deg, 100_000.0)), at100, "{deg}° at 100 km");
        }
    }

    #[test]
    fn the_output_grid_maps_back_onto_the_lowest_scan() {
        let infos = smhi();
        let hits = hits(&infos, 0);
        for (g, hit) in hits[0].iter().enumerate() {
            assert_eq!(hit.map(|h| h.gate), Some(g), "the lowest scan covers its own gates");
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
        assert_eq!(needed(Want::Angle(3.0), &infos), [4], "2.5° is nearer 3.0° than 4.0°");
        assert_eq!(needed(Want::ColMax, &infos), (0..10).collect::<Vec<_>>());
        let cappi1 = needed(Want::Cappi(1000.0), &infos);
        let cappi2 = needed(Want::Cappi(2000.0), &infos);
        assert_eq!(cappi1[0], 0);
        assert!(cappi1.contains(&3) && cappi2.contains(&6), "{cappi1:?} {cappi2:?}");
        // Near the radar only the high angles reach 1-2 km.
        assert!(cappi1.contains(&9) || cappi1.contains(&8), "{cappi1:?}");
        let blocked = Box::leak(Box::new(Blockage {
            tenths: std::array::from_fn(|d| if d < 90 { 15 } else { 0 }),
        }));
        assert_eq!(needed(Want::Hybrid(blocked), &infos), [0, 2]);
        let all_high = Box::leak(Box::new(Blockage { tenths: [250; 360] }));
        assert_eq!(needed(Want::Hybrid(all_high), &infos), [0, 9], "above every angle: the highest");
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
        assert_eq!((out.rays.len(), out.gates, out.gate_spacing_m), (360, 480, 500));
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
        let subset: Vec<Tilt> = tilts().into_iter().enumerate().filter(|(i, _)| keep.contains(i)).map(|(_, t)| t).collect();
        let again = compose(Want::Cappi(1000.0), subset).unwrap();
        assert!(again.rays.iter().zip(&out.rays).all(|(a, b)| a.codes == b.codes));
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
            vec![scan(0.5, 360, 480, 500, |_, _| 10), scan(1.5, 360, 480, 500, |_, _| 20)],
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
}
