//! Products (S20, `docs/protocol.md`, products): what a radar's frame shows,
//! made from the scan angles of one volume.
//!
//! - `REF` at elevation index 0 is the lowest scan, decoded as before S20
//!   (`Want::Lowest`); `REF` at another index is that one angle as it is
//!   (`Want::Angle`).
//! - `CAPPI` ("Height", S29) is a horizontal slice at a chosen height above
//!   sea level (500 m to 12 km), with no data where no beam holds that
//!   height; `CAPPI1` and `CAPPI2` are its old names for 1 and 2 km (`choose`).
//!   Since S30 a height may be above the ground instead (`Above::Ground`,
//!   `Want::CappiGround`): the terrain under each gate (`terrain.rs`) plus
//!   the height. Its rule, `nearest_beam`, is also My mosaic's `height`
//!   rule per radar (`mosaic.rs`): one rule, not two copies.
//!   `CMAX` is the column maximum, `HYBRID` ("Clear view") the lowest
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
/// "Height" (S29): a horizontal slice at `heightM` above sea level.
pub const CAPPI: &str = "CAPPI";
/// The fixed heights before S29, which `set_product` still accepts as
/// `CAPPI` at 1,000 and 2,000 m (`choose`); no longer in the vocabulary.
pub const CAPPI1: &str = "CAPPI1";
pub const CAPPI2: &str = "CAPPI2";
pub const CMAX: &str = "CMAX";
/// "Storm height" (S24a): the echo top at 18 dBZ, km above sea level.
pub const ETOP: &str = "ETOP";
/// "Rain mass" (S24a): vertically integrated liquid, kg/m².
pub const VIL: &str = "VIL";
/// The lowest beam (S24b): on the composites only, per texel the radar
/// whose lowest clear beam is lowest there (`mosaic.rs`).
pub const LOWB: &str = "LOWB";
/// What `sweden` and `nordic` offer (S24b, `docs/protocol.md`, the
/// composites' products), in the vocabulary's order: `REF` is the
/// provider's composite, the rest the engine makes from the radars.
pub const GRID_PRODUCTS: [&str; 6] = [REF, LOWB, CAPPI, CMAX, ETOP, VIL];

/// `CAPPI`'s heights above sea level, in metres (S29).
pub const HEIGHT_MIN_M: u32 = 500;
pub const HEIGHT_MAX_M: u32 = 12_000;
pub const HEIGHT_STEP_M: u32 = 500;
/// `CAPPI`'s height when `set_product` names none.
pub const HEIGHT_DEFAULT_M: u32 = 2_000;
/// Half of a nominal 1° beam: a scan's beam holds a height where the height
/// is seen within this of the scan's angle (`cappi_pick`).
pub const BEAM_HALF_DEG: f64 = 0.5;

/// One entry of `hello.products`.
#[derive(Serialize, PartialEq, Clone, Debug)]
pub struct Info {
    pub id: &'static str,
    pub name: &'static str,
    /// What a height can be measured from (S30, `CAPPI` only); not sent
    /// when empty.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub above: &'static [&'static str],
    /// The frames' units when they are not dBZ (S24a: `ETOP`, `VIL`); not
    /// sent when empty.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub units: &'static str,
}

/// `hello.products`: the vocabulary, in the order a chooser lists it.
pub const VOCABULARY: [Info; 7] = [
    Info {
        id: REF,
        name: "Lowest scan",
        above: &[],
        units: "",
    },
    Info {
        id: HYBRID,
        name: "Clear view",
        above: &[],
        units: "",
    },
    // S24b: the composites' only (`GRID_PRODUCTS`); no radar offers it.
    Info {
        id: LOWB,
        name: "Lowest beam",
        above: &[],
        units: "",
    },
    Info {
        id: CAPPI,
        name: "Height",
        above: &["sea", "ground"],
        units: "",
    },
    Info {
        id: CMAX,
        name: "Column max",
        above: &[],
        units: "",
    },
    Info {
        id: ETOP,
        name: "Storm height",
        above: &[],
        units: ETOP_LEGEND.units,
    },
    Info {
        id: VIL,
        name: "Rain mass",
        above: &[],
        units: VIL_LEGEND.units,
    },
];

// ---------------------------------------------------------------------------
// Storm height and rain mass (S24a)
// ---------------------------------------------------------------------------

/// A product's own units, palette and bounds (`frame.units`, `palette`,
/// `bounds`), for a product not in dBZ.
#[derive(Debug, PartialEq)]
pub struct Legend {
    pub units: &'static str,
    pub palette: &'static [&'static str],
    pub bounds: &'static [i32],
}

/// The echo that makes a storm's height: 18 dBZ, as a code
/// (`round(18 × 2 + 66)`).
pub const ETOP_DBZ: f64 = 18.0;
const ETOP_CODE: u8 = (ETOP_DBZ * 2.0 + 66.0) as u8;
/// `ETOP`'s encoding: value = (code − offset) / scale km, so 0.1 km a code;
/// tops are rounded to 0.2 km (even codes), and an "at least" top is one
/// code more (odd), within the same 0.2 km (`docs/protocol.md`).
pub const ETOP_SCALE: f32 = 10.0;
pub const ETOP_OFFSET: f32 = 2.0;
/// `VIL`'s encoding: value = (code − offset) / scale kg/m², 0.5 a code.
pub const VIL_SCALE: f32 = 2.0;
pub const VIL_OFFSET: f32 = 2.0;
/// Marshall–Palmer liquid water: M = 3.44e-6 · Z^(4/7) kg/m³.
const VIL_COEFFICIENT: f64 = 3.44e-6;
/// Reflectivity is capped here before VIL (hail would count as water).
pub const VIL_CAP_DBZ: f64 = 56.0;
/// The texture's G bit for an "at least" storm height (`docs/protocol.md`,
/// sweep texture).
pub const AT_LEAST: u8 = 8;

/// `ETOP`: km, a band a kilometre from 2 to 10 km, then 12, 15 and above.
pub const ETOP_LEGEND: Legend = Legend {
    units: "km",
    palette: &[
        "#2c3e73", "#2f5f98", "#2d80b0", "#3aa0b8", "#5bbcad", "#8fd19b", "#c7dd7f", "#f1d36b",
        "#f3a95a", "#eb7a52", "#d9505c", "#b73a7a",
    ],
    bounds: &[0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 15, 26],
};

/// `VIL`: kg/m², bands to 70 and above.
pub const VIL_LEGEND: Legend = Legend {
    units: "kg/m²",
    palette: &[
        "#3b4a5a", "#2f6f5e", "#3a8f55", "#5aa846", "#8cbc3c", "#c3cc3b", "#ecc943", "#f0a23d",
        "#ea7a3a", "#dc4f3c", "#c23a58", "#a33487", "#d6a4e6",
    ],
    bounds: &[0, 1, 2, 4, 7, 10, 15, 20, 25, 30, 40, 50, 70, 127],
};

/// What a height is measured from (S30): sea level, or the ground under
/// each point (`terrain.rs`).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Above {
    #[default]
    Sea,
    Ground,
}

impl Above {
    pub fn id(self) -> &'static str {
        match self {
            Above::Sea => "sea",
            Above::Ground => "ground",
        }
    }

    pub fn parse(id: &str) -> Option<Above> {
        [Above::Sea, Above::Ground]
            .into_iter()
            .find(|a| a.id() == id)
    }
}

/// `state.product`, and what a `set_product` asks for.
#[derive(Serialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub id: String,
    pub elevation_index: u32,
    /// `CAPPI`'s height above sea level, in metres (S29); `None`, and not
    /// sent, for any other product.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_m: Option<u32>,
    /// What `CAPPI`'s height is above (S30); `None`, and not sent, for any
    /// other product.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub above: Option<Above>,
}

impl Default for Choice {
    /// The lowest scan: what every frame was before S20.
    fn default() -> Self {
        Choice {
            id: REF.to_owned(),
            elevation_index: 0,
            height_m: None,
            above: None,
        }
    }
}

/// What a `set_product` asks for, as `state.product` will hold it: the
/// aliases `CAPPI1` and `CAPPI2` become `CAPPI` at 1,000 and 2,000 m (a
/// `heightM` sent with them is ignored), and `CAPPI` without a height takes
/// `HEIGHT_DEFAULT_M`; `above` (S30) is sea level unless `ground` is sent
/// with `CAPPI` (the aliases are above sea level, whatever is sent). An id
/// neither in the vocabulary nor an alias, a height out of range, off the
/// 500 m steps or sent with another product, or an `above` that is neither
/// word or is sent with another product, is the error its sender hears.
/// Whether the station can make the choice is `want_for`'s question.
pub fn choose(
    product: &str,
    elevation_index: u32,
    height_m: Option<u32>,
    above: Option<&str>,
) -> Result<Choice, String> {
    let from = match (product, above) {
        (CAPPI, Some(word)) => Some(Above::parse(word).ok_or_else(|| {
            format!("above {word} is not what a height is measured from: sea or ground.")
        })?),
        (CAPPI | CAPPI1 | CAPPI2, _) => Some(Above::Sea),
        (_, Some(word)) => {
            return Err(format!("above {word} goes with CAPPI only, not {product}."));
        }
        _ => None,
    };
    let height = match product {
        CAPPI1 => Some(1_000),
        CAPPI2 => Some(2_000),
        CAPPI => {
            let h = height_m.unwrap_or(HEIGHT_DEFAULT_M);
            if !(HEIGHT_MIN_M..=HEIGHT_MAX_M).contains(&h) || !h.is_multiple_of(HEIGHT_STEP_M) {
                return Err(format!(
                    "heightM {h} is not a height CAPPI shows: {HEIGHT_MIN_M} to {HEIGHT_MAX_M} m \
                     above sea level, in steps of {HEIGHT_STEP_M}."
                ));
            }
            Some(h)
        }
        _ if !VOCABULARY.iter().any(|p| p.id == product) => {
            return Err(format!(
                "Unknown product {product}; products are listed in hello."
            ));
        }
        _ => {
            if let Some(h) = height_m {
                return Err(format!("heightM {h} goes with CAPPI only, not {product}."));
            }
            None
        }
    };
    Ok(Choice {
        id: if height.is_some() { CAPPI } else { product }.to_owned(),
        elevation_index,
        height_m: height,
        above: from,
    })
}

/// A height in metres as a label: `3 km`, `3.5 km`.
fn km_label(m: u32) -> String {
    if m.is_multiple_of(1000) {
        format!("{} km", m / 1000)
    } else {
        format!("{:.1} km", f64::from(m) / 1000.0)
    }
}

/// A height's `productName` (S29, S30): `Height 3.5 km`, or `Height 1 km
/// above ground`; My mosaic's `height` frames are named the same way.
pub fn height_name(m: u32, above: Above) -> String {
    match above {
        Above::Sea => format!("Height {}", km_label(m)),
        Above::Ground => format!("Height {} above ground", km_label(m)),
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
    /// A horizontal slice (S29): `.0` metres above the antenna, what the
    /// beams are matched against; `.1` the chosen height above sea level in
    /// metres, which names the product.
    Cappi(f64, u32),
    /// A horizontal slice above the ground (S30): at each gate the target
    /// is `.0` plus the terrain there, metres above the antenna (`.0` is the
    /// height less the antenna's altitude); `.1` the chosen height above
    /// the ground, which names the product; `.2` where the antenna stands.
    CappiGround(f64, u32, At),
    /// The column maximum.
    ColMax,
    /// Per azimuth, the lowest angle clear of the terrain.
    Hybrid(&'static Blockage),
    /// The storm height (S24a): the echo top at 18 dBZ; `.0` the antenna's
    /// height above sea level in metres, which the beam heights are added to.
    EchoTop(f64),
    /// The rain mass (S24a): vertically integrated liquid.
    Vil,
    /// The lowest beam (S24b): a composite's product only, made by
    /// `mosaic.rs` across its radars. A radar never makes it; if asked, it
    /// is the lowest scan.
    LowestBeam,
}

/// Where a radar's antenna stands, for the terrain under its gates.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct At {
    pub lat: f64,
    pub lon: f64,
}

impl Want {
    /// The product a choice names on a station with these nominal angles,
    /// blockage table and antenna height above sea level (`alt_m`); `None`
    /// when the station cannot make it.
    pub fn of(
        choice: &Choice,
        elevations: &[f64],
        blockage: Option<&'static Blockage>,
        alt_m: f64,
    ) -> Option<Want> {
        let index = choice.elevation_index as usize;
        match choice.id.as_str() {
            REF if index == 0 => Some(Want::Lowest),
            REF => elevations.get(index).map(|&deg| Want::Angle(deg)),
            _ if index != 0 => None,
            CAPPI => {
                let h = choice.height_m.unwrap_or(HEIGHT_DEFAULT_M);
                Some(Want::Cappi(f64::from(h) - alt_m, h))
            }
            CMAX => Some(Want::ColMax),
            HYBRID => blockage.map(Want::Hybrid),
            ETOP => Some(Want::EchoTop(alt_m)),
            VIL => Some(Want::Vil),
            LOWB => Some(Want::LowestBeam),
            _ => None,
        }
    }

    /// The product's own units, palette and bounds when they are not the
    /// reflectivity's (S24a); `None` for every product in dBZ.
    pub fn legend(&self) -> Option<&'static Legend> {
        match self {
            Want::EchoTop(_) => Some(&ETOP_LEGEND),
            Want::Vil => Some(&VIL_LEGEND),
            _ => None,
        }
    }

    pub fn is_lowest(&self) -> bool {
        *self == Want::Lowest
    }

    /// The product's id and display name (`frame.product`, `productName`).
    pub fn product(&self) -> (&'static str, String) {
        match self {
            Want::Lowest | Want::Angle(_) => (REF, "Reflectivity".to_owned()),
            Want::Cappi(_, asl) => (CAPPI, height_name(*asl, Above::Sea)),
            Want::CappiGround(_, agl, _) => (CAPPI, height_name(*agl, Above::Ground)),
            Want::ColMax => (CMAX, "Column max".to_owned()),
            Want::Hybrid(_) => (HYBRID, "Clear view".to_owned()),
            Want::EchoTop(_) => (ETOP, "Storm height".to_owned()),
            Want::Vil => (VIL, "Rain mass".to_owned()),
            Want::LowestBeam => (LOWB, "Lowest beam".to_owned()),
        }
    }

    /// The last part of a frame id, and the catalog's key for the product:
    /// `e0` for the lowest scan (the id every frame had before S20), `a40`
    /// for the angle 4.0°, `cappi3500` for a height of 3,500 m above sea level
    /// (every height in metres, `cappi500` to `cappi12000`: S20's `cappi1`
    /// and `cappi2` rings, made by another rule, never meet a height's and are
    /// pruned as products no longer chosen), `cappi1000g` for 1,000 m above
    /// the ground (S30), and so on.
    pub fn variant(&self) -> String {
        match self {
            Want::Lowest => "e0".to_owned(),
            Want::Angle(deg) => format!("a{}", (deg * 10.0).round() as i64),
            Want::Cappi(_, asl) => format!("cappi{asl}"),
            Want::CappiGround(_, agl, _) => format!("cappi{agl}g"),
            Want::ColMax => "cmax".to_owned(),
            Want::Hybrid(_) => "clear".to_owned(),
            Want::EchoTop(_) => "etop".to_owned(),
            Want::Vil => "vil".to_owned(),
            Want::LowestBeam => "lowb".to_owned(),
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
///
/// Any other all-letter last part (`[a-z]+`) but the placeholder's
/// `loading` is a product too (S24a review #3): one a later engine makes,
/// so an engine rolled back over that engine's cache keeps those frames in
/// their own ring instead of showing them as its lowest scan.
pub fn variant_of(frame_id: &str) -> &str {
    let last = frame_id.rsplit('-').next().unwrap_or_default();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let hex8 = |s: &str| s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit());
    let letters = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase());
    let product = matches!(last, "cmax" | "clear" | "etop" | "vil" | "lowb")
        || (frame_id.contains('-') && last != "loading" && letters(last))
        || last.strip_prefix("cappi").is_some_and(|h| {
            // S30: above the ground ends in `g`.
            digits(h) || h.strip_suffix('g').is_some_and(digits)
        })
        || last.strip_prefix('a').is_some_and(digits)
        // S25: a My mosaic set (`mosaic::Set::variant`).
        || last.strip_prefix('m').is_some_and(hex8);
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
/// FMI's five (S24a): one `SCAN` file per angle in ORD's cache, the same
/// set at every radar but the lowest (ORD's listings of 2026-09-15, 91
/// times each): 0.3° at ten radars, 0.5° at Korppoo, 0.1° at Luosto.
const FI_ANGLES: [f64; 5] = [0.3, 0.7, 1.5, 3.0, 5.0];
const FI_ANGLES_KORPPOO: [f64; 5] = [0.5, 0.7, 1.5, 3.0, 5.0];
const FI_ANGLES_LUOSTO: [f64; 5] = [0.1, 0.7, 1.5, 3.0, 5.0];

/// A station's nominal angles, ascending: empty for a composite. FMI's
/// radars publish a file per angle, which the ORD poller reads as one
/// volume for any product but the lowest scan (S24a).
pub fn nominal_angles(station: &Station) -> &'static [f64] {
    match (station.kind, station.provider, station.country.as_str()) {
        (SiteKind::Grid, ..) => &[],
        (_, ProviderId::Smhi, _) => &SMHI_ANGLES,
        (_, ProviderId::Ord, "NO") => &NO_ANGLES,
        (_, ProviderId::Ord, "DK") => &DK_ANGLES,
        (_, ProviderId::Ord, "FI") => match station.id.as_str() {
            "fikor" => &FI_ANGLES_KORPPOO,
            "filuo" => &FI_ANGLES_LUOSTO,
            _ => &FI_ANGLES,
        },
        _ => &[],
    }
}

/// `hello.sites[].products` and `elevations`: nothing for a grid station;
/// the lowest scan for any radar; every product built from several angles
/// where the engine knows the radar's angles, `HYBRID` only where it also
/// has a blockage table.
pub fn for_station(station: &Station) -> (Vec<&'static str>, Vec<Elevation>) {
    if station.kind == SiteKind::Grid {
        // S24b: a provider's composite offers the products the engine makes
        // from its radars; My mosaic has its set's rule instead.
        let products = if station.provider == ProviderId::Mosaic {
            Vec::new()
        } else {
            GRID_PRODUCTS.to_vec()
        };
        return (products, Vec::new());
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
            // S24b: the composites' only.
            LOWB => false,
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
/// (a grid station makes nothing to choose). A height above the ground
/// (S30) is measured from the terrain under each gate of this station.
pub fn want_for(station: &Station, choice: &Choice) -> Option<Want> {
    let (products, _) = for_station(station);
    if !products.contains(&choice.id.as_str()) {
        return None;
    }
    let want = Want::of(
        choice,
        nominal_angles(station),
        blockage(&station.id),
        station.alt_m,
    )?;
    Some(match (want, choice.above) {
        (Want::Cappi(base, agl), Some(Above::Ground)) => Want::CappiGround(
            base,
            agl,
            At {
                lat: station.lat,
                lon: station.lon,
            },
        ),
        (want, _) => want,
    })
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
            height_m: None,
            above: None,
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

/// The ground distance of each output gate: the gates of the scan `lowest`
/// indexes, placed at its nominal angle.
fn grounds(infos: &[TiltInfo], lowest: usize) -> Vec<f64> {
    let base = infos[lowest];
    let e0 = placement_deg(base.elangle).to_radians();
    (0..usize::from(base.gates))
        .map(|g| {
            let r = f64::from(base.first_gate_m) + g as f64 * f64::from(base.gate_spacing_m);
            ground_of(r, e0)
        })
        .collect()
}

/// The elevation, in radians, at which the antenna sees the point at ground
/// distance `ground_m` and `height_m` above the antenna, on the 4/3 earth.
pub fn elevation_to(ground_m: f64, height_m: f64) -> f64 {
    let (theta, r) = (ground_m / EARTH_M, EARTH_M + height_m);
    (r * theta.cos() - EARTH_M).atan2(r * theta.sin())
}

/// The height rule (S29; S30 uses it per radar in My mosaic too): of the
/// scans covering a point, given as (index, angle in degrees, beam centre
/// above the antenna there in metres), those whose beam holds `target`
/// metres above the antenna, which the antenna sees at `seen` radians
/// (`elevation_to`), within `BEAM_HALF_DEG` of the scan's angle; of those,
/// the one whose beam centre is nearest `target`, the lower angle on a tie.
/// With how far that centre is from `target`, metres. `None`: no beam
/// holds it, "no radar at this height".
pub fn nearest_beam(
    seen: f64,
    target: f64,
    beams: impl IntoIterator<Item = (usize, f64, f64)>,
) -> Option<(usize, f64)> {
    let half = BEAM_HALF_DEG.to_radians();
    let mut best: Option<(usize, f64, f64)> = None;
    for (k, deg, height) in beams {
        if (seen - deg.to_radians()).abs() > half {
            continue;
        }
        let d = (height - target).abs();
        let better = match best {
            None => true,
            Some((_, held_deg, held_d)) => d < held_d || (d == held_d && deg < held_deg),
        };
        if better {
            best = Some((k, deg, d));
        }
    }
    best.map(|(k, _, d)| (k, d))
}

/// Per scan, per output gate, where it covers the gate (`docs/protocol.md`,
/// how each product is made). `lowest` indexes the scan whose grid is the
/// output's.
fn hits(infos: &[TiltInfo], lowest: usize) -> Vec<Vec<Option<Hit>>> {
    let grounds = grounds(infos, lowest);
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

/// The height slice's scan at output gate `g`, `ground_m` from the radar:
/// of the scans covering it whose beam holds `height` metres above the
/// antenna (seen within `BEAM_HALF_DEG` of the scan's angle), the one whose
/// beam centre is nearest `height`; the lower angle on a tie. `None` where
/// no beam holds it: no radar at this height.
fn cappi_pick(
    hits: &[Vec<Option<Hit>>],
    infos: &[TiltInfo],
    ground_m: f64,
    g: usize,
    height: f64,
) -> Option<usize> {
    let covering = hits
        .iter()
        .enumerate()
        .filter_map(|(k, per_gate)| per_gate[g].map(|hit| (k, infos[k].elangle, hit.height)));
    nearest_beam(elevation_to(ground_m, height), height, covering).map(|(k, _)| k)
}

/// The clear view's scan for one azimuth degree: the lowest at or above
/// the table's angle, else the highest.
pub(crate) fn hybrid_pick(infos: &[TiltInfo], tenths: u8) -> usize {
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
        Want::Lowest | Want::LowestBeam => {}
        Want::Angle(deg) => {
            picked[lowest] = false;
            picked[nearest_of(infos, deg)] = true;
        }
        Want::Cappi(height, _) => {
            let hits = hits(infos, lowest);
            for (g, &s) in grounds(infos, lowest).iter().enumerate() {
                if let Some(k) = cappi_pick(&hits, infos, s, g, height) {
                    picked[k] = true;
                }
            }
        }
        Want::CappiGround(base, _, _) => {
            // The ground under a gate is 0 to the grid's highest, and the
            // angle a height is seen at grows with it: every scan covering
            // the gate whose beam holds some height in that span, a
            // superset of what `compose` picks at any azimuth.
            let top = base + crate::terrain::max_m();
            let half = BEAM_HALF_DEG.to_radians();
            let hits = hits(infos, lowest);
            for (g, &s) in grounds(infos, lowest).iter().enumerate() {
                let (low, high) = (elevation_to(s, base) - half, elevation_to(s, top) + half);
                for (k, per_gate) in hits.iter().enumerate() {
                    let e = infos[k].elangle.to_radians();
                    if per_gate[g].is_some() && e >= low && e <= high {
                        picked[k] = true;
                    }
                }
            }
        }
        // S24a: the storm height and the rain mass read the whole column.
        Want::ColMax | Want::EchoTop(_) | Want::Vil => {
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
        Want::Lowest | Want::LowestBeam => return Ok(tilts.swap_remove(0).sweep),
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
    let ground = grounds(&infos, 0);
    let per_gate_pick: Vec<Option<usize>> = match want {
        Want::Cappi(height, _) => ground
            .iter()
            .enumerate()
            .map(|(g, &s)| cappi_pick(&hits, &infos, s, g, height))
            .collect(),
        _ => Vec::new(),
    };
    let elevation = placement_deg(tilts[0].elangle) as f32;
    let mut rays = Vec::with_capacity(base.rays.len());
    // One gate's column (S24a): the covering scans ascending by angle, each
    // as (code, beam centre above the antenna).
    let mut column: Vec<(u8, f64)> = Vec::with_capacity(tilts.len());
    for (i, out) in base.rays.iter().enumerate() {
        if let Want::EchoTop(_) | Want::Vil = want {
            let codes: Vec<u8> = (0..gates)
                .map(|g| {
                    column.clear();
                    for (k, per_gate) in hits.iter().enumerate() {
                        if let (Some(hit), Some(code)) = (per_gate[g], value(k, i, g)) {
                            column.push((code, hit.height));
                        }
                    }
                    match want {
                        Want::EchoTop(alt_m) => echo_top_code(&column, alt_m),
                        _ => vil_code(&column),
                    }
                })
                .collect();
            rays.push(Ray {
                azimuth_deg: out.azimuth_deg,
                elevation_deg: elevation,
                time_ms: out.time_ms,
                codes,
            });
            continue;
        }
        let hybrid = match want {
            Want::Hybrid(table) => {
                let degree = (f64::from(out.azimuth_deg).floor() as i64).rem_euclid(360) as usize;
                Some(hybrid_pick(&infos, table.tenths[degree]))
            }
            _ => None,
        };
        // Above the ground (S30): the target at each gate of this ray is the
        // terrain under it plus the height.
        let ground_pick: Vec<Option<usize>> = match want {
            Want::CappiGround(base, _, at) => ground
                .iter()
                .enumerate()
                .map(|(g, &s)| {
                    let (lat, lon) =
                        crate::terrain::destination(at.lat, at.lon, f64::from(out.azimuth_deg), s);
                    cappi_pick(&hits, &infos, s, g, base + crate::terrain::at(lat, lon))
                })
                .collect(),
            _ => Vec::new(),
        };
        let codes: Vec<u8> = (0..gates)
            .map(|g| match want {
                Want::Cappi(..) => per_gate_pick[g].and_then(|k| value(k, i, g)).unwrap_or(1),
                Want::CappiGround(..) => ground_pick[g].and_then(|k| value(k, i, g)).unwrap_or(1),
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
                Want::Lowest | Want::LowestBeam | Want::Angle(_) => {
                    unreachable!("returned above")
                }
                Want::EchoTop(_) | Want::Vil => unreachable!("composed above"),
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
        // S24a: the storm height and the rain mass have their own coding.
        scale: match want {
            Want::EchoTop(_) => ETOP_SCALE,
            Want::Vil => VIL_SCALE,
            _ => base.scale,
        },
        offset: match want {
            Want::EchoTop(_) => ETOP_OFFSET,
            Want::Vil => VIL_OFFSET,
            _ => base.offset,
        },
        code1_status: OUTSIDE_COVERAGE,
    })
}

/// `ETOP`'s code at one output gate (`docs/protocol.md`, storm height):
/// `column` holds the scans covering it, ascending by angle, as (code, beam
/// centre above the antenna in metres); `alt_m` is the antenna's height
/// above sea level. The highest scan with 18 dBZ or more gives the top,
/// `2 + 2 × round(5 T)` for `T` km above sea level, one more when no scan
/// above it holds anything but no data ("at least"). 0: readings, none of
/// 18 dBZ; 1: no reading.
pub fn echo_top_code(column: &[(u8, f64)], alt_m: f64) -> u8 {
    let Some(top) = column.iter().rposition(|&(code, _)| code >= ETOP_CODE) else {
        return if column.iter().any(|&(code, _)| code != 1) {
            0
        } else {
            1
        };
    };
    let km = (column[top].1 + alt_m) / 1000.0;
    let code = (2 + 2 * (5.0 * km).round() as i64).clamp(2, 254) as u8;
    let at_least = column[top + 1..].iter().all(|&(code, _)| code == 1);
    code + u8::from(at_least)
}

/// `VIL`'s code at one output gate (`docs/protocol.md`, rain mass), the same
/// `column`: Marshall–Palmer water over each gap between consecutive beam
/// centres with a reading, reflectivity capped at `VIL_CAP_DBZ`, in kg/m²,
/// as `2 + round(2 × VIL)`. 0: every reading below threshold, or a rain
/// mass under `VIL_FLOOR`; 1: none.
pub fn vil_code(column: &[(u8, f64)]) -> u8 {
    let readings = || column.iter().filter(|&&(code, _)| code != 1);
    if readings().next().is_none() {
        return 1;
    }
    if readings().all(|&(code, _)| code == 0) {
        return 0;
    }
    // Z per code, computed once (S24b: a Nordic mosaic makes 41 radars'
    // VIL a frame); the same values as computing each one here.
    static Z: LazyLock<[f64; 256]> = LazyLock::new(|| {
        std::array::from_fn(|code| {
            if code == 0 {
                0.0
            } else {
                let dbz = ((code as f64 - 66.0) / 2.0).min(VIL_CAP_DBZ);
                10f64.powf(dbz / 10.0)
            }
        })
    });
    let z = |code: u8| Z[usize::from(code)];
    let mut total = 0.0;
    let mut below: Option<(f64, f64)> = None;
    for &(code, height) in readings() {
        let here = z(code);
        if let Some((z0, h0)) = below {
            total += VIL_COEFFICIENT * ((z0 + here) / 2.0).powf(4.0 / 7.0) * (height - h0);
        }
        below = Some((here, height));
    }
    if total < VIL_FLOOR {
        return 0;
    }
    (2 + (2.0 * total).round() as i64).min(255) as u8
}

/// A rain mass under this, kg/m², is code 0 like a column below threshold,
/// so any weak echo is not drawn as a "< 1" blob (S24a review #4).
pub const VIL_FLOOR: f64 = 0.25;

/// Mark the "at least" storm heights in a sweep texture (`Sweep::texture`'s
/// RGBA pixels): G bit 8 on every odd measured code of an `ETOP` frame
/// (`docs/protocol.md`, sweep texture). Any other product: unchanged.
pub fn mark_texture(product: &str, pixels: &mut [u8]) {
    if product != ETOP {
        return;
    }
    for texel in pixels.as_chunks_mut::<4>().0 {
        if texel[0] > 0 && texel[2] >= 2 && texel[2] % 2 == 1 {
            texel[1] |= AT_LEAST;
        }
    }
}

/// Every scan of one file, each with its angle (S24a: an FMI `SCAN` file
/// holds one): FMI's volumes are several files (`assemble`).
pub fn scans_of<R: std::io::Read + std::io::Seek + Send + 'static>(
    reader: R,
) -> Result<Vec<Tilt>, String> {
    crate::odim::decode_tilts(reader, |infos| (0..infos.len()).collect()).map_err(|e| e.to_string())
}

/// What `want` makes of one volume assembled from several files' scans
/// (S24a, FMI), as `decode_volume` makes it of one file: the product, and
/// the lowest scan it read riding along for the `-e0` ring (S26). `tilts`
/// in any order; `compose` sorts them by angle.
pub fn assemble(want: Want, tilts: Vec<Tilt>) -> Result<crate::smhi_live::Scan, String> {
    use crate::smhi_live::Scan;
    if want.is_lowest() {
        return tilts
            .into_iter()
            .min_by(|a, b| a.elangle.total_cmp(&b.elangle))
            .map(|t| Scan::Polar(t.sweep))
            .ok_or_else(|| "the volume holds no reflectivity scans".to_owned());
    }
    let free = match want {
        Want::Angle(_) => None,
        _ => tilts
            .iter()
            .min_by(|a, b| a.elangle.total_cmp(&b.elangle))
            .map(|t| Box::new(copy_sweep(&t.sweep))),
    };
    compose(want, tilts).map(|sweep| Scan::Product(sweep, want, free))
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

    /// The choice of `id` at index 0 with no height.
    fn choice_of(id: &str) -> Choice {
        Choice {
            id: id.into(),
            elevation_index: 0,
            height_m: None,
            above: None,
        }
    }

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
        assert_eq!(
            ids,
            ["REF", "HYBRID", "LOWB", "CAPPI", "CMAX", "ETOP", "VIL"]
        );
        assert_eq!(
            Want::of(&choice_of(LOWB), &[], None, 0.0),
            Some(Want::LowestBeam)
        );
        assert_eq!(Want::LowestBeam.variant(), "lowb");
        assert_eq!(variant_of("nordic-20260915T1200Z-lowb"), "lowb");
        let elevations = [0.5, 1.0, 1.5];
        let choice = |id: &str, index| Choice {
            id: id.into(),
            elevation_index: index,
            height_m: None,
            above: None,
        };
        let height = |m| Choice {
            id: CAPPI.into(),
            elevation_index: 0,
            height_m: Some(m),
            above: Some(Above::Sea),
        };
        assert_eq!(
            Want::of(&Choice::default(), &elevations, None, 0.0),
            Some(Want::Lowest)
        );
        assert_eq!(
            Want::of(&choice("REF", 2), &elevations, None, 0.0),
            Some(Want::Angle(1.5))
        );
        assert_eq!(Want::of(&choice("REF", 3), &elevations, None, 0.0), None);
        assert_eq!(
            Want::of(&height(1000), &[], None, 0.0),
            Some(Want::Cappi(1000.0, 1000))
        );
        assert_eq!(
            Want::of(&height(3500), &[], None, 220.0),
            Some(Want::Cappi(3280.0, 3500)),
            "above sea level: the antenna's height comes off"
        );
        assert_eq!(
            Want::of(
                &Choice {
                    elevation_index: 1,
                    ..height(2000)
                },
                &elevations,
                None,
                0.0
            ),
            None,
            "an index with another product"
        );
        assert_eq!(
            Want::of(&choice("HYBRID", 0), &elevations, None, 0.0),
            None,
            "no blockage table"
        );
        assert_eq!(Want::of(&choice("SNOW", 0), &elevations, None, 0.0), None);
        // S24a: the storm height carries the antenna's height; the rain
        // mass needs none. Both bring their own legend.
        assert_eq!(
            Want::of(&choice("ETOP", 0), &elevations, None, 164.0),
            Some(Want::EchoTop(164.0))
        );
        assert_eq!(
            Want::of(&choice("VIL", 0), &elevations, None, 164.0),
            Some(Want::Vil)
        );
        assert_eq!(Want::EchoTop(164.0).legend().unwrap().units, "km");
        assert_eq!(Want::Vil.legend().unwrap().units, "kg/m²");
        assert_eq!(Want::ColMax.legend(), None);
        for legend in [&ETOP_LEGEND, &VIL_LEGEND] {
            assert_eq!(legend.bounds.len(), legend.palette.len() + 1);
            assert!(legend.bounds.windows(2).all(|w| w[0] < w[1]));
        }
        assert_eq!(ETOP_CODE, 102, "18 dBZ");
        assert_eq!(
            Want::of(&choice("CAPPI1", 0), &elevations, None, 0.0),
            None,
            "an alias is choose's to resolve; state never holds one"
        );
        for (want, variant, id, name) in [
            (Want::Lowest, "e0", "REF", "Reflectivity"),
            (Want::Angle(4.0), "a40", "REF", "Reflectivity"),
            (Want::Angle(0.7), "a7", "REF", "Reflectivity"),
            (
                Want::Cappi(1000.0, 1000),
                "cappi1000",
                "CAPPI",
                "Height 1 km",
            ),
            (
                Want::Cappi(1780.0, 2000),
                "cappi2000",
                "CAPPI",
                "Height 2 km",
            ),
            (
                Want::Cappi(3280.0, 3500),
                "cappi3500",
                "CAPPI",
                "Height 3.5 km",
            ),
            (Want::Cappi(0.0, 500), "cappi500", "CAPPI", "Height 0.5 km"),
            (
                Want::Cappi(0.0, 12000),
                "cappi12000",
                "CAPPI",
                "Height 12 km",
            ),
            (Want::ColMax, "cmax", "CMAX", "Column max"),
        ] {
            let (product, product_name) = want.product();
            assert_eq!(
                (want.variant().as_str(), product, product_name.as_str()),
                (variant, id, name)
            );
        }
        assert_eq!(variant_of("vara-20260913T105503Z-cappi3500"), "cappi3500");
        assert_eq!(variant_of("vara-20260913T105503Z-cappi1"), "cappi1");
        assert_eq!(variant_of("vara-20260913T105503Z-e0"), "e0");
        assert_eq!(variant_of("vara-20260913T105503Z-a240"), "a240");
        assert_eq!(variant_of("vara-20260913T105503Z-clear"), "clear");
        // An id that names no product is the lowest scan (a pre-S20 or
        // synthetic frame, scripts/check-popover.sh's `popover-test-0`).
        assert_eq!(variant_of("popover-test-0"), "e0");
        assert_eq!(variant_of("vara-loading"), "e0");
        assert_eq!(variant_of("-loading"), "e0");
        // S24a review #3: an all-letter suffix this engine does not know is
        // a later engine's product, its own ring, never the lowest scan.
        assert_eq!(variant_of("x-abc"), "abc");
        assert_eq!(variant_of("vara-20260915T0000Z-etop"), "etop");
        assert_eq!(variant_of("vara-20260915T0000Z-vil"), "vil");
        assert_eq!(variant_of("vara-20260915T0000Z"), "e0");
        assert_eq!(variant_of("x-abc1"), "e0");
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

    #[test]
    fn choosing_a_height() {
        let ok = |p: &str, i, h| choose(p, i, h, None).unwrap();
        assert_eq!(
            ok("CAPPI", 0, Some(3500)),
            Choice {
                id: "CAPPI".into(),
                elevation_index: 0,
                height_m: Some(3500),
                above: Some(Above::Sea),
            }
        );
        assert_eq!(ok("CAPPI", 0, None).height_m, Some(2000), "the default");
        assert_eq!(
            ok("CAPPI1", 0, None),
            ok("CAPPI", 0, Some(1000)),
            "the old names"
        );
        assert_eq!(
            ok("CAPPI2", 0, Some(5000)),
            ok("CAPPI", 0, Some(2000)),
            "an alias keeps its own height"
        );
        assert_eq!(
            ok("CMAX", 0, None),
            Choice {
                id: "CMAX".into(),
                ..Choice::default()
            }
        );
        assert_eq!(ok("REF", 3, None).elevation_index, 3);
        for (p, h, says) in [
            ("CAPPI", Some(750), "heightM 750"),
            ("CAPPI", Some(0), "heightM 0"),
            ("CAPPI", Some(12_500), "heightM 12500"),
            ("CMAX", Some(2000), "CAPPI only"),
            ("SNOW", None, "Unknown product SNOW"),
            ("VIL", Some(2000), "CAPPI only"),
        ] {
            let e = choose(p, 0, h, None).unwrap_err();
            assert!(e.contains(says), "{e}");
        }
        let every: Vec<u32> = (0..=30_000)
            .filter(|&m| choose("CAPPI", 0, Some(m), None).is_ok())
            .collect();
        assert_eq!(every, (1..=24).map(|n| n * 500).collect::<Vec<u32>>());
        // `state.product` sends heightM with CAPPI only.
        assert_eq!(
            serde_json::to_value(ok("CAPPI", 0, Some(3000))).unwrap(),
            serde_json::json!({"id":"CAPPI","elevationIndex":0,"heightM":3000,"above":"sea"})
        );
        assert_eq!(
            serde_json::to_value(Choice::default()).unwrap(),
            serde_json::json!({"id":"REF","elevationIndex":0})
        );
        // Above sea level: a radar 700 m up takes 1 km at 300 m above it.
        let high = Station {
            alt_m: 700.0,
            ..polar(ProviderId::Smhi, "SE")
        };
        assert_eq!(
            want_for(&high, &ok("CAPPI", 0, Some(1000))),
            Some(Want::Cappi(300.0, 1000))
        );
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
        let cappi1 = needed(Want::Cappi(1000.0, 1000), &infos);
        let cappi2 = needed(Want::Cappi(2000.0, 2000), &infos);
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
    fn a_height_takes_each_gate_from_the_nearest_beam_that_holds_it() {
        // Three scans marking their own code: 0.5° -> 10, 1.5° -> 20, 4° -> 30.
        let tilts = || {
            vec![
                scan(4.0, 360, 400, 250, |_, _| 30),
                scan(0.5, 360, 480, 500, |_, _| 10),
                scan(1.5, 360, 480, 500, |_, _| 20),
            ]
        };
        let out = compose(Want::Cappi(1000.0, 1000), tilts()).unwrap();
        assert_eq!(
            (out.rays.len(), out.gates, out.gate_spacing_m),
            (360, 480, 500)
        );
        assert_eq!(out.rays[0].elevation_deg, 0.5);
        assert_eq!(out.end_ms, 2040, "the last scan's end");
        let row = &out.rays[17].codes;
        // 1 km up is seen between 4.5° and 3.5° from about 12.7 to 16.3 km
        // (the 4° beam holds it), between 2° and 1° from about 28 to 49 km
        // (1.5°), and below 1° out to where it meets the horizon, ~130 km
        // (0.5°). Where no beam holds it: no data (1), never no rain (0).
        assert_eq!(row[5], 1, "2.8 km: above every beam");
        assert_eq!(row[28], 30, "14 km");
        assert_eq!(row[40], 1, "20 km: between the 4° and the 1.5° beams");
        assert_eq!(row[80], 20, "40 km");
        assert_eq!(row[200], 10, "100 km");
        assert_eq!(row[300], 1, "150 km: under the lowest beam");
        let order: Vec<u8> = row.iter().copied().fold(Vec::new(), |mut runs, c| {
            if runs.last() != Some(&c) {
                runs.push(c);
            }
            runs
        });
        assert_eq!(order, [1, 30, 1, 20, 10, 1], "one band each, outward");
        assert!(row.iter().all(|&c| c != 0), "no gate says no rain");
        // The slice ends where the lowest beam's lower edge (0°) rises past
        // 1 km: cos(s/R) = R / (R + 1000).
        let last = row.iter().rposition(|&c| c == 10).unwrap();
        let edge = EARTH_M * (EARTH_M / (EARTH_M + 1000.0)).acos();
        let at = |g: usize| ground_of(250.0 + 500.0 * g as f64, 0.5f64.to_radians());
        assert!(
            at(last) <= edge && at(last + 1) > edge,
            "{} {} {edge}",
            at(last),
            at(last + 1)
        );
        // Composing from only the needed scans gives the same answer.
        let infos: Vec<TiltInfo> = tilts().iter().map(Tilt::info).collect();
        let keep = needed(Want::Cappi(1000.0, 1000), &infos);
        let subset: Vec<Tilt> = tilts()
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep.contains(i))
            .map(|(_, t)| t)
            .collect();
        let again = compose(Want::Cappi(1000.0, 1000), subset).unwrap();
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
            ["REF", "CAPPI", "CMAX", "ETOP", "VIL"],
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
        // FMI (S24a): one file per angle, read as one volume, so every
        // product but the clear view (no blockage table); the lowest angle
        // is the radar's own.
        let (fi, fi_angles) = for_station(&polar(ProviderId::Ord, "FI"));
        assert_eq!(fi, ["REF", "CAPPI", "CMAX", "ETOP", "VIL"]);
        let degrees = |e: &[Elevation]| e.iter().map(|e| e.deg).collect::<Vec<f64>>();
        assert_eq!(degrees(&fi_angles), [0.3, 0.7, 1.5, 3.0, 5.0]);
        for (id, lowest) in [("fikor", 0.5), ("filuo", 0.1), ("fivih", 0.3)] {
            let station = Station {
                id: id.into(),
                ..polar(ProviderId::Ord, "FI")
            };
            assert_eq!(degrees(&for_station(&station).1)[0], lowest, "{id}");
        }
        let grid = Station {
            kind: SiteKind::Grid,
            ..polar(ProviderId::Smhi, "SE")
        };
        // S24b: a provider's composite offers the products the engine makes
        // from its radars, no angles; My mosaic offers none.
        assert_eq!(for_station(&grid), (GRID_PRODUCTS.to_vec(), vec![]));
        let mine = Station {
            provider: ProviderId::Mosaic,
            ..grid.clone()
        };
        assert_eq!(for_station(&mine), (vec![], vec![]));
        assert_eq!(want_for(&grid, &choice_of(LOWB)), Some(Want::LowestBeam));
        assert_eq!(want_for(&grid, &choice_of(CMAX)), Some(Want::ColMax));
        assert_eq!(want_for(&grid, &choice_of(HYBRID)), None);
        assert_eq!(want_for(&grid, &Choice::default()), Some(Want::Lowest));
        let radar = polar(ProviderId::Smhi, "SE");
        assert_eq!(
            want_for(&radar, &choice_of(LOWB)),
            None,
            "no radar makes it"
        );
        assert_eq!(carry(&choice_of(LOWB), None, &radar), Choice::default());
        let choice = |id: &str, index| Choice {
            id: id.into(),
            elevation_index: index,
            height_m: None,
            above: None,
        };
        let smhi = polar(ProviderId::Smhi, "SE");
        assert_eq!(want_for(&smhi, &choice("CMAX", 0)), Some(Want::ColMax));
        assert_eq!(want_for(&smhi, &choice("REF", 5)), Some(Want::Angle(4.0)));
        assert_eq!(want_for(&smhi, &choice("HYBRID", 0)), None);
        assert_eq!(
            want_for(&polar(ProviderId::Ord, "FI"), &choice("CMAX", 0)),
            Some(Want::ColMax)
        );
        assert_eq!(
            want_for(&polar(ProviderId::Ord, "FI"), &choice("HYBRID", 0)),
            None,
            "no blockage table for FMI's radars"
        );
        assert_eq!(
            want_for(&grid, &choice("REF", 0)),
            Some(Want::Lowest),
            "S24b: REF on a composite is the composite itself"
        );
        assert_eq!(
            want_for(&grid, &choice("REF", 3)),
            None,
            "a composite has no angles"
        );
        assert_eq!(angle_deg(&smhi, &choice("REF", 5)), Some(4.0));
        assert_eq!(angle_deg(&smhi, &choice("CMAX", 0)), None);
    }

    #[test]
    fn a_switch_carries_the_product_over() {
        let choice = |id: &str, index| Choice {
            id: id.into(),
            elevation_index: index,
            height_m: None,
            above: None,
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
        // A height stays the same height above sea level.
        let tall = Choice {
            id: CAPPI.into(),
            elevation_index: 0,
            height_m: Some(3500),
            above: Some(Above::Sea),
        };
        // And a height above the ground stays above the ground (S30).
        let over = Choice {
            above: Some(Above::Ground),
            ..tall.clone()
        };
        assert_eq!(carry(&over, None, &norway), over);
        assert_eq!(carry(&tall, None, &norway), tall);
        // FMI makes both since S24a (its five angles as one volume); 2.5°
        // is nearest its 3.0° (index 3).
        assert_eq!(carry(&tall, None, &finland), tall);
        assert_eq!(
            carry(&choice("REF", 4), Some(2.5), &finland),
            choice("REF", 3)
        );
        assert_eq!(carry(&choice("ETOP", 0), None, &finland), choice("ETOP", 0));
        // A product a radar cannot make falls back to the lowest scan.
        assert_eq!(
            carry(&choice("HYBRID", 0), None, &finland),
            Choice::default()
        );
        // A composite keeps the choice for the next radar.
        assert_eq!(carry(&choice("REF", 4), Some(2.5), &grid), choice("REF", 4));
        assert_eq!(carry(&Choice::default(), None, &smhi), Choice::default());
    }

    #[test]
    fn choosing_what_a_height_is_above() {
        let ok = |h, above| choose(CAPPI, 0, h, above).unwrap();
        assert_eq!(ok(Some(1000), Some("ground")).above, Some(Above::Ground));
        assert_eq!(ok(Some(1000), None).above, Some(Above::Sea), "the default");
        assert_eq!(
            choose(CAPPI1, 0, None, Some("ground")).unwrap(),
            ok(Some(1000), Some("sea")),
            "an alias is above sea level"
        );
        assert_eq!(choose(CMAX, 0, None, None).unwrap().above, None);
        for (p, above, says) in [
            (CAPPI, "space", "sea or ground"),
            (CMAX, "sea", "CAPPI only"),
            (REF, "ground", "CAPPI only"),
        ] {
            let e = choose(p, 0, None, Some(above)).unwrap_err();
            assert!(e.contains(says), "{e}");
        }
        assert_eq!(
            serde_json::to_value(ok(Some(3000), Some("ground"))).unwrap(),
            serde_json::json!({"id":"CAPPI","elevationIndex":0,"heightM":3000,"above":"ground"})
        );
        // hello.products: CAPPI says what a height can be above; the rest
        // are as they were.
        let hello = serde_json::to_value(VOCABULARY).unwrap();
        assert_eq!(
            hello[2],
            serde_json::json!({"id":"LOWB","name":"Lowest beam"})
        );
        assert_eq!(
            hello[3],
            serde_json::json!({"id":"CAPPI","name":"Height","above":["sea","ground"]})
        );
        assert_eq!(
            hello[0],
            serde_json::json!({"id":"REF","name":"Lowest scan"})
        );
        // A station's ground under its gates: its own place.
        let station = Station {
            alt_m: 300.0,
            lat: 60.0,
            lon: 9.0,
            ..polar(ProviderId::Ord, "NO")
        };
        let ground = want_for(&station, &ok(Some(1000), Some("ground"))).unwrap();
        assert_eq!(
            ground,
            Want::CappiGround(
                700.0,
                1000,
                At {
                    lat: 60.0,
                    lon: 9.0
                }
            )
        );
        assert_eq!(ground.variant(), "cappi1000g");
        assert_eq!(ground.product(), (CAPPI, "Height 1 km above ground".into()));
        let sea = want_for(&station, &ok(Some(1000), Some("sea"))).unwrap();
        assert_eq!(sea, Want::Cappi(700.0, 1000));
        assert_ne!(sea.variant(), ground.variant());
        assert_eq!(
            variant_of("nohur-20260914T093000Z-cappi1000g"),
            "cappi1000g"
        );
        assert_eq!(variant_of("x-cappi1000q"), "e0");
        // All letters: a product of its own since S24a review #3.
        assert_eq!(variant_of("x-cappig"), "cappig");
    }

    #[test]
    fn the_nearest_beam_rule() {
        let seen = 1.0f64.to_radians();
        // 0.6° holds (0.4° off), 1.6° does not (0.6° off); of those that
        // hold, the centre nearest 1,000 m.
        assert_eq!(
            nearest_beam(
                seen,
                1000.0,
                [
                    (0, 0.6, 900.0),
                    (1, 1.0, 1030.0),
                    (2, 1.4, 1010.0),
                    (3, 1.6, 1000.0)
                ]
            ),
            Some((2, 10.0))
        );
        assert_eq!(
            nearest_beam(seen, 1000.0, [(5, 1.2, 1010.0), (4, 0.8, 990.0)]),
            Some((4, 10.0)),
            "a tie: the lower angle"
        );
        assert_eq!(nearest_beam(seen, 1000.0, [(0, 2.0, 1000.0)]), None);
        assert_eq!(nearest_beam(seen, 1000.0, []), None);
    }

    /// A height above the ground (S30) against the protocol's rule written
    /// out here, point by point: the terrain under each gate plus the
    /// height, then the nearest beam that holds it. Near Jotunheimen, where
    /// the ground is 0.5 to 2 km up, the slice differs from the one above
    /// sea level; over the sea it is the same.
    #[test]
    fn a_height_above_ground_follows_the_terrain() {
        let at = At {
            lat: 61.3,
            lon: 8.2,
        };
        let (alt, agl) = (600.0, 1000u32);
        let base = f64::from(agl) - alt;
        let markers = [
            (0.5, 10u8),
            (1.0, 20),
            (1.5, 30),
            (2.5, 40),
            (4.0, 50),
            (8.0, 60),
            (14.0, 70),
        ];
        let tilts = || -> Vec<Tilt> {
            markers
                .iter()
                .map(|&(e, c)| scan(e, 360, 480, 500, move |_, _| c))
                .collect()
        };
        let want = Want::CappiGround(base, agl, at);
        let out = compose(want, tilts()).unwrap();
        let sea = compose(Want::Cappi(base, agl), tilts()).unwrap();
        let e0 = 0.5f64.to_radians();
        let half = 0.5f64.to_radians();
        let (mut checked, mut differ, mut level) = (0, 0, 0);
        for i in (0..360).step_by(5) {
            for g in (0..480).step_by(3) {
                let r = 250.0 + 500.0 * g as f64;
                let s = EARTH_M * (r * e0.cos()).atan2(EARTH_M + r * e0.sin());
                let az = f64::from(out.rays[i].azimuth_deg);
                let (lat, lon) = crate::terrain::destination(at.lat, at.lon, az, s);
                let terrain = crate::terrain::at(lat, lon);
                let target = base + terrain;
                let th = s / EARTH_M;
                let seen =
                    ((EARTH_M + target) * th.cos() - EARTH_M).atan2((EARTH_M + target) * th.sin());
                let mut best: Option<(u8, f64)> = None;
                for &(deg, code) in &markers {
                    let e = f64::to_radians(deg);
                    let c = (e + th).cos();
                    let slant = EARTH_M * th.sin() / c;
                    let gate = ((slant - 250.0) / 500.0).round();
                    if c <= 1e-9 || !(0.0..480.0).contains(&gate) || (seen - e).abs() > half {
                        continue;
                    }
                    let miss = (EARTH_M * e.cos() / c - EARTH_M - target).abs();
                    if best.is_none_or(|(_, m)| miss < m) {
                        best = Some((code, miss));
                    }
                }
                let expect = best.map_or(1, |(c, _)| c);
                assert_eq!(
                    out.rays[i].codes[g], expect,
                    "ray {i} gate {g}: {terrain} m of ground"
                );
                checked += 1;
                differ += usize::from(out.rays[i].codes[g] != sea.rays[i].codes[g]);
                if terrain == 0.0 {
                    level += 1;
                    assert_eq!(out.rays[i].codes[g], sea.rays[i].codes[g], "sea level");
                }
            }
        }
        assert!(
            differ * 5 > checked && level > 100,
            "{differ} of {checked} differ from above sea level; {level} over the sea"
        );
        // The scans `needed` names are all `compose` uses: the same slice.
        let infos: Vec<TiltInfo> = tilts().iter().map(Tilt::info).collect();
        let keep = needed(want, &infos);
        let subset: Vec<Tilt> = tilts()
            .into_iter()
            .enumerate()
            .filter(|(k, _)| keep.contains(k))
            .map(|(_, t)| t)
            .collect();
        let again = compose(want, subset).unwrap();
        assert!(
            again
                .rays
                .iter()
                .zip(&out.rays)
                .all(|(a, b)| a.codes == b.codes)
        );
        assert!(
            keep.len() >= needed(Want::Cappi(base, agl), &infos).len(),
            "at least what the same height above sea level reads"
        );
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
            assert_eq!(products.len(), 8, "{golden}");
            // S24a: the key's antenna height is the file's own `/where`.
            let alt_m = key["altM"].as_f64().unwrap();
            for (variant, product) in products {
                let want = match variant.as_str() {
                    "cappi1" => Want::Cappi(1000.0, 1000),
                    "cappi2" => Want::Cappi(2000.0, 2000),
                    "cmax" => Want::ColMax,
                    "clear" => Want::Hybrid(table),
                    "etop" => Want::EchoTop(alt_m),
                    "vil" => Want::Vil,
                    angle => Want::Angle(angle[1..].parse::<f64>().unwrap() / 10.0),
                };
                // The S20 answer keys' names; a height's variant is in metres (S29).
                let named = match variant.as_str() {
                    "cappi1" => "cappi1000",
                    "cappi2" => "cappi2000",
                    other => other,
                };
                assert_eq!(want.variant(), named);
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

    /// FMI's five one-angle files of one nominal time (S24a), each decoded
    /// by `scans_of` and assembled into one volume, against their answer key
    /// (`golden/produce-products.py --parts`): the column maximum, storm
    /// height and rain mass, byte for byte, in any file order; the lowest
    /// scan rides along as the lowest file's own sweep.
    #[test]
    fn an_assembled_volume_matches_its_answer_key() {
        use crate::smhi_live::Scan;
        use std::io::Read;
        const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../");
        let key: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{ROOT}golden/fikor-20260915/products.json")).unwrap(),
        )
        .unwrap();
        let files: Vec<String> = key["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| format!("{ROOT}data/raw/{}", p["fixture"].as_str().unwrap()))
            .collect();
        assert_eq!(files.len(), 5);
        let open = |path: &String| std::fs::File::open(path).unwrap();
        let scans = |order: &[usize]| -> Vec<Tilt> {
            order
                .iter()
                .flat_map(|&i| scans_of(open(&files[i])).unwrap())
                .collect()
        };
        let alt_m = key["altM"].as_f64().unwrap();
        let products = key["products"].as_object().unwrap();
        assert_eq!(products.len(), 3);
        for (variant, product) in products {
            let want = match variant.as_str() {
                "cmax" => Want::ColMax,
                "etop" => Want::EchoTop(alt_m),
                "vil" => Want::Vil,
                other => panic!("{other}"),
            };
            let mut expected = Vec::new();
            let gz = format!(
                "{ROOT}golden/fikor-20260915/{}",
                product["file"].as_str().unwrap()
            );
            flate2::read::GzDecoder::new(std::fs::File::open(gz).unwrap())
                .read_to_end(&mut expected)
                .unwrap();
            for order in [[0, 1, 2, 3, 4], [4, 2, 0, 3, 1]] {
                let Scan::Product(sweep, _, Some(free)) = assemble(want, scans(&order)).unwrap()
                else {
                    panic!("{variant}: not a product with its lowest scan")
                };
                assert_eq!(
                    (sweep.rays.len(), sweep.gates),
                    (360, 500),
                    "{variant}: the lowest scan's grid"
                );
                let codes: Vec<u8> = sweep.rays.iter().flat_map(|r| r.codes.clone()).collect();
                let differ = codes.iter().zip(&expected).filter(|(a, b)| a != b).count();
                assert_eq!(
                    (codes.len(), differ),
                    (expected.len(), 0),
                    "{variant} {order:?}"
                );
                let lowest =
                    crate::odim::decode_lowest(open(&files[0]), crate::odim::Tilt::Lowest).unwrap();
                assert!(
                    free.rays
                        .iter()
                        .zip(&lowest.rays)
                        .all(|(a, b)| a.codes == b.codes)
                );
            }
        }
        let at_least = key["products"]["etop"]["atLeast"].as_u64().unwrap();
        assert!(at_least > 0, "the fixture has 'at least' tops");
        // Their texels carry G bit 8; exact tops and other products none.
        let Scan::Product(sweep, ..) =
            assemble(Want::EchoTop(alt_m), scans(&[0, 1, 2, 3, 4])).unwrap()
        else {
            panic!()
        };
        let mut pixels = sweep.texture(ETOP_LEGEND.bounds, ETOP_LEGEND.palette.len());
        mark_texture(ETOP, &mut pixels);
        let flagged = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|t| t[1] & AT_LEAST != 0)
            .count();
        assert_eq!(flagged as u64, at_least);
        let mut other = sweep.texture(ETOP_LEGEND.bounds, ETOP_LEGEND.palette.len());
        mark_texture(CMAX, &mut other);
        assert!(
            other
                .as_chunks::<4>()
                .0
                .iter()
                .all(|t| t[1] & AT_LEAST == 0)
        );
    }

    /// The storm height and the rain mass of one column, by hand
    /// (`docs/protocol.md`, storm height and rain mass).
    #[test]
    fn a_columns_storm_height_and_rain_mass() {
        let dbz = |d: f64| (d * 2.0 + 66.0) as u8;
        // 30 dBZ at 1 and 4 km, 10 dBZ at 7 km: the top is the 4 km beam,
        // 4.164 km above sea level -> 4.2 km, code 44; not "at least".
        let column = [
            (dbz(30.0), 1000.0),
            (dbz(30.0), 4000.0),
            (dbz(10.0), 7000.0),
        ];
        assert_eq!(echo_top_code(&column, 164.0), 44);
        // The same with no data above: the top beam still holds 18 dBZ.
        let open_top = [(dbz(30.0), 1000.0), (dbz(30.0), 4000.0), (1, 7000.0)];
        assert_eq!(echo_top_code(&open_top, 164.0), 45, "at least 4.2 km");
        assert_eq!(
            echo_top_code(&[(dbz(30.0), 4000.0)], 0.0),
            43,
            "one scan: at least"
        );
        // 17.5 dBZ nowhere reaches 18; no readings at all is no data.
        assert_eq!(echo_top_code(&[(dbz(17.5), 1000.0), (0, 2000.0)], 0.0), 0);
        assert_eq!(echo_top_code(&[(1, 1000.0)], 0.0), 1);
        assert_eq!(echo_top_code(&[], 0.0), 1);
        assert_eq!(
            echo_top_code(&[(dbz(60.0), 90_000.0)], 0.0),
            255,
            "capped, at least"
        );
        // VIL: 40 dBZ from 1 to 3 km (Z = 10^4): 3.44e-6 · 10^(16/7) · 2000
        // = 1.328 kg/m², code 2 + round(2.656) = 5.
        let steady = [(dbz(40.0), 1000.0), (dbz(40.0), 3000.0)];
        let expect = 3.44e-6 * 10f64.powf(16.0 / 7.0) * 2000.0;
        assert!((expect - 1.328).abs() < 0.001, "{expect}");
        assert_eq!(vil_code(&steady), 5);
        // Nodata is skipped (the gap spans it); below threshold is Z = 0.
        let gap = [(dbz(40.0), 1000.0), (1, 2000.0), (dbz(40.0), 3000.0)];
        assert_eq!(vil_code(&gap), 5);
        let half = [(dbz(40.0), 1000.0), (0, 3000.0)];
        let expect = 3.44e-6 * (1e4f64 / 2.0).powf(4.0 / 7.0) * 2000.0;
        assert_eq!(vil_code(&half), 2 + (2.0 * expect).round() as u8);
        // Hail is capped at 56 dBZ: 70 dBZ counts as 56.
        assert_eq!(
            vil_code(&[(dbz(70.0), 0.0), (dbz(70.0), 10_000.0)]),
            vil_code(&[(dbz(56.0), 0.0), (dbz(56.0), 10_000.0)])
        );
        // Review #4: under 0.25 kg/m² is code 0 (not drawn); from 0.25 the
        // code is at least 3, so code 2 never appears.
        assert_eq!(vil_code(&[(dbz(40.0), 1000.0)]), 0, "one reading: no gap");
        let thin = [(dbz(20.0), 1000.0), (dbz(20.0), 2000.0)];
        let mass = 3.44e-6 * 100f64.powf(4.0 / 7.0) * 1000.0;
        assert!(mass < VIL_FLOOR, "{mass}");
        assert_eq!(vil_code(&thin), 0, "{mass} kg/m² is not drawn");
        let enough = [(dbz(30.0), 1000.0), (dbz(30.0), 3000.0)];
        let mass = 3.44e-6 * 1000f64.powf(4.0 / 7.0) * 2000.0;
        assert!((VIL_FLOOR..0.75).contains(&mass), "{mass}");
        assert_eq!(vil_code(&enough), 3);
        assert_eq!(vil_code(&[(0, 1000.0), (0, 2000.0), (1, 3000.0)]), 0);
        assert_eq!(vil_code(&[(1, 1000.0)]), 1);
        assert_eq!(
            vil_code(&[(dbz(56.0), 0.0), (dbz(56.0), 40_000.0)]),
            255,
            "capped"
        );
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
            for want in [Want::Cappi(1000.0, 1000), Want::ColMax, Want::Hybrid(table)] {
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
    /// entries per radar, every radar a table station, and every radar
    /// whose angles the engine knows (SMHI's, MET Norway's and DMI's, S26)
    /// offers "Clear view" with its own table. FMI's offer the lowest scan
    /// only.
    #[test]
    fn blockage_tables_load_and_offer_clear_view() {
        let table = crate::providers::table();
        let radars: Vec<&Station> = table
            .sites
            .iter()
            .filter(|s| s.kind == SiteKind::Polar && !nominal_angles(s).is_empty())
            .collect();
        assert_eq!(
            radars.len(),
            41,
            "12 SE, 12 NO, 12 FI (S24a) and 5 DK radars"
        );
        let (finnish, radars): (Vec<&Station>, Vec<&Station>) =
            radars.into_iter().partition(|s| s.country == "FI");
        assert_eq!((finnish.len(), radars.len()), (12, 29));
        for station in finnish {
            assert_eq!(blockage(&station.id), None, "{}", station.id);
            assert_eq!(
                for_station(station).0,
                ["REF", "CAPPI", "CMAX", "ETOP", "VIL"],
                "{}",
                station.id
            );
        }
        for station in radars {
            let id = station.id.as_str();
            assert!(blockage(id).is_some(), "{id}");
            assert_eq!(
                for_station(station).0,
                ["REF", "HYBRID", "CAPPI", "CMAX", "ETOP", "VIL"],
                "{id}"
            );
            let choice = Choice {
                id: HYBRID.into(),
                elevation_index: 0,
                height_m: None,
                above: None,
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
            Want::Cappi(1000.0, 1000),
            Want::Cappi(2000.0, 2000),
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
                    Want::Cappi(1000.0, 1000),
                    Want::Cappi(2000.0, 2000),
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
