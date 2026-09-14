//! Composites: SMHI's national composite (`area/sweden/product/comp`;
//! stream S8, DEC-11), and EUMETNET OPERA's European one cut to the Nordic
//! box (`providers/opera.rs`; stream S16, DEC-14). One ODIM `COMP` file
//! every 5 minutes covers every radar of its network. The engine decodes it
//! and reprojects it to a Web Mercator grid texture, so both UIs draw it as
//! one textured rectangle (`docs/protocol.md`, grid texture).
//!
//! - The DBZH layer (the `/dataset1/dataN` whose `what/quantity` is `DBZH`)
//!   is read as numbers (SMHI stores bytes, OPERA float64) and requantized
//!   to the polar convention. Undetect becomes 0 and nodata 1; a measured
//!   value becomes `round(dBZ × 2 + 66)` clamped to 2..255. The palette
//!   bounds and the weak-return floor therefore work as they do for a sweep.
//! - `/where` describes the grid: `projdef`, and `xsize` × `ysize` pixels of
//!   `xscale` × `yscale` metres, row 0 north. `UL_lon`/`UL_lat` is the outer
//!   corner of the upper-left pixel.
//! - The texture covers a lon/lat box with `PIXEL_M` Mercator pixels,
//!   counted from the box's west and north edges: the box of the grid's
//!   outer boundary (SMHI), or a box the provider names (OPERA's Nordic
//!   crop). Each texel holds the source pixel that contains its centre, and
//!   texels off the grid are nodata.
//! - A layer stored in many chunks is read chunk by chunk, and only the
//!   chunks some texel falls in: 11 of OPERA's 30 for the Nordic box. Chunks
//!   stored back to back are read in one go, so a ranged reader fetches each
//!   run in one request (DEC-2). A needed chunk the file does not allocate
//!   reads as nodata.
//!
//! Two projections are understood: the polar `+proj=stere` form SMHI uses
//! (north pole, true scale at `lat_ts`), and the oblique or equatorial
//! ellipsoidal `+proj=laea` OPERA uses, each on an ellipsoid given by name
//! or by `+a` with `+rf` or `+b`. Anything else is an error, which is
//! better than a wrong picture. SMHI's `+towgs84=0,0,0` is read the way
//! PROJ reads it: WGS84 latitudes are carried onto the Bessel ellipsoid
//! through geocentric coordinates before projecting (about 43 m at 70°N).

use crate::protocol::{Frame, FrameKind, FrameStatus, Geometry, GridPlacement, SiteKind, Station};
use crate::smhi_live::{RangeReader, Scan};
use crate::sweep::{BELOW_THRESHOLD, OUTSIDE_COVERAGE, png};
use chrono::{DateTime, NaiveDateTime};
use hdf5_pure::{AttrValue, Chunk, Dataset, Datatype, DatatypeByteOrder, File, ReadSeekSource};
use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// SMHI's area key for the composite, which is also its station id.
pub const AREA: &str = "sweden";
/// SMHI's product name for the composite.
pub const PRODUCT: &str = "comp";
/// Texel size in Web Mercator metres. At 54°N (Sweden's south edge) this
/// is 1.2 km of ground, finer than SMHI's 2 km source, so nearest sampling
/// skips no source pixel; OPERA's 1 km grid is sampled a little coarser
/// south of about 60°N. Sweden's texture stays under 2048 px on each side,
/// the smallest WebGL2 must support; the Nordic one is 1670 × 2297, under
/// the 4096 phones have (plan, budgets).
pub const PIXEL_M: f64 = 2000.0;
/// The Web Mercator sphere (EPSG:3857).
pub const MERCATOR_R: f64 = 6_378_137.0;
/// Measured value = (code - offset) / scale, as for polar frames.
pub const SCALE: f32 = 2.0;
pub const OFFSET: f32 = 66.0;

/// The composite as `hello` lists it: the middle of Sweden, with no antenna.
pub fn station() -> Station {
    Station {
        id: AREA.to_owned(),
        name: "Sweden".to_owned(),
        state: String::new(),
        lat: 62.0,
        lon: 16.0,
        alt_m: 0.0,
        kind: SiteKind::Grid,
        country: "SE".to_owned(),
        provider: crate::providers::ProviderId::Smhi,
        range_km: 0.0,
        attribution: crate::providers::smhi::ATTRIBUTION.to_owned(),
        aliases: Vec::new(),
        source: String::new(),
    }
}

/// Why a composite could not be decoded, with the ODIM path involved.
#[derive(Debug)]
pub struct CompositeError(String);

impl fmt::Display for CompositeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CompositeError {}

fn fail(what: impl fmt::Display) -> CompositeError {
    CompositeError(what.to_string())
}

// ---------------------------------------------------------------------------
// Polar stereographic on an ellipsoid (Snyder 1987, §21; PROJ `stere`)
// ---------------------------------------------------------------------------

/// A north polar stereographic projection. Metres from the pole, y
/// pointing away from `lon0`, the way PROJ writes them. Longitudes and
/// latitudes in and out are WGS84 when the projdef names a `+towgs84`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stereographic {
    /// First eccentricity.
    e: f64,
    /// Central meridian, radians.
    lon0: f64,
    /// `a · m(lat_ts) / t(lat_ts)`: the radius per unit of `t`.
    akm1: f64,
    /// `(a, e²)` of the projection's ellipsoid when WGS84 latitudes are
    /// carried onto it through geocentric coordinates: PROJ's reading of a
    /// zero `+towgs84` on another ellipsoid. SMHI's Bessel grid needs it;
    /// its corners are only consistent with its size under it. The shift
    /// is about 43 m at 70°N. Longitude is unchanged by a zero shift.
    datum: Option<(f64, f64)>,
}

const WGS84_A: f64 = 6_378_137.0;
const WGS84_RF: f64 = 298.257_223_563;

fn wgs84() -> (f64, f64) {
    let f = 1.0 / WGS84_RF;
    (WGS84_A, f * (2.0 - f))
}

/// A latitude at height 0 on ellipsoid `from`, as the geodetic latitude of
/// the same point on ellipsoid `to`, through geocentric coordinates with no
/// shift. Degrees. Each ellipsoid is `(a, e²)`.
fn shift_latitude(lat: f64, (a1, e21): (f64, f64), (a2, e22): (f64, f64)) -> f64 {
    let phi = lat.to_radians();
    let n = a1 / (1.0 - e21 * phi.sin().powi(2)).sqrt();
    let (p, z) = (n * phi.cos(), n * (1.0 - e21) * phi.sin());
    let mut phi = z.atan2(p * (1.0 - e22));
    for _ in 0..30 {
        let n = a2 / (1.0 - e22 * phi.sin().powi(2)).sqrt();
        let h = p / phi.cos() - n;
        let next = z.atan2(p * (1.0 - e22 * n / (n + h)));
        let done = (next - phi).abs() < 1e-15;
        phi = next;
        if done {
            break;
        }
    }
    phi.to_degrees()
}

/// Snyder's `t` (PROJ `pj_tsfn`) for latitude `phi` in radians.
fn tsfn(phi: f64, e: f64) -> f64 {
    let s = e * phi.sin();
    (FRAC_PI_4 - phi / 2.0).tan() / ((1.0 - s) / (1.0 + s)).powf(e / 2.0)
}

/// A projdef's `+key=value` tokens, and the checks both projections share.
struct Params<'a> {
    projdef: &'a str,
    map: HashMap<&'a str, &'a str>,
}

impl<'a> Params<'a> {
    fn new(projdef: &'a str) -> Self {
        let map = projdef
            .split_whitespace()
            .map(|token| {
                let token = token.strip_prefix('+').unwrap_or(token);
                token.split_once('=').unwrap_or((token, ""))
            })
            .collect();
        Params { projdef, map }
    }

    fn bad(&self, why: &str) -> CompositeError {
        fail(format!("projdef {:?}: {why}", self.projdef))
    }

    fn get(&self, key: &str) -> Option<&'a str> {
        self.map.get(key).copied()
    }

    fn number(&self, key: &str) -> Result<Option<f64>, CompositeError> {
        self.get(key)
            .map(|v| {
                v.parse::<f64>()
                    .map_err(|_| self.bad(&format!("+{key} is not a number")))
            })
            .transpose()
    }

    /// Refuses a scale factor other than 1.
    fn unit_scale(&self) -> Result<(), CompositeError> {
        if self
            .number("k_0")?
            .or(self.number("k")?)
            .is_some_and(|k| (k - 1.0).abs() > 1e-12)
        {
            return Err(self.bad("a scale factor other than 1 is not supported"));
        }
        Ok(())
    }

    /// Refuses a `+towgs84` that shifts; a zero one passes.
    fn zero_shift(&self) -> Result<(), CompositeError> {
        if let Some(shift) = self.get("towgs84")
            && shift.split(',').any(|v| v.trim().parse::<f64>() != Ok(0.0))
        {
            return Err(self.bad("a datum shift is not supported"));
        }
        Ok(())
    }

    /// The ellipsoid's semi-major axis and inverse flattening, which is
    /// infinite for a sphere.
    fn ellipsoid(&self) -> Result<(f64, f64), CompositeError> {
        Ok(match (self.get("ellps"), self.number("a")?) {
            (Some("bessel"), _) => (6_377_397.155, 299.152_812_8),
            (Some("WGS84"), _) => (WGS84_A, WGS84_RF),
            (Some("GRS80"), _) => (6_378_137.0, 298.257_222_101),
            (Some(other), _) => return Err(self.bad(&format!("unknown +ellps={other}"))),
            (None, Some(a)) => match (self.number("rf")?, self.number("b")?) {
                (Some(rf), _) => (a, rf),
                (None, Some(b)) if b < a => (a, a / (a - b)),
                (None, _) => (a, f64::INFINITY),
            },
            (None, None) => {
                return Err(self.bad("no ellipsoid (+ellps, or +a with +rf or +b)"));
            }
        })
    }
}

impl Stereographic {
    fn parse(projdef: &str) -> Result<Stereographic, CompositeError> {
        let params = Params::new(projdef);
        let number = |key: &str| params.number(key);
        if params.get("proj") != Some("stere") {
            return Err(params.bad("only +proj=stere is supported"));
        }
        if number("lat_0")?.is_none_or(|lat0| (lat0 - 90.0).abs() > 1e-9) {
            return Err(params.bad("only the north polar aspect (+lat_0=90) is supported"));
        }
        params.unit_scale()?;
        if number("x_0")?.is_some_and(|v| v != 0.0) || number("y_0")?.is_some_and(|v| v != 0.0) {
            return Err(params.bad("false easting or northing is not supported"));
        }
        params.zero_shift()?;
        let (a, inverse_flattening) = params.ellipsoid()?;
        let f = 1.0 / inverse_flattening;
        let e = (f * (2.0 - f)).sqrt();
        let phic = number("lat_ts")?.unwrap_or(90.0).to_radians();
        let akm1 = if (phic - FRAC_PI_2).abs() < 1e-10 {
            2.0 * a / ((1.0 + e).powf(1.0 + e) * (1.0 - e).powf(1.0 - e)).sqrt()
        } else {
            let s = e * phic.sin();
            a * phic.cos() / (1.0 - s * s).sqrt() / tsfn(phic, e)
        };
        let is_wgs84 = a == WGS84_A && (inverse_flattening - WGS84_RF).abs() < 1e-9;
        Ok(Stereographic {
            e,
            lon0: number("lon_0")?.unwrap_or(0.0).to_radians(),
            akm1,
            datum: (params.get("towgs84").is_some() && !is_wgs84).then_some((a, e * e)),
        })
    }

    /// The distance from the pole, in metres, of a latitude in degrees.
    fn rho(&self, lat: f64) -> f64 {
        let lat = match self.datum {
            Some(ellipsoid) => shift_latitude(lat, wgs84(), ellipsoid),
            None => lat,
        };
        self.akm1 * tsfn(lat.to_radians(), self.e)
    }

    /// Degrees to projected metres.
    fn forward(&self, lon: f64, lat: f64) -> (f64, f64) {
        let rho = self.rho(lat);
        let dl = lon.to_radians() - self.lon0;
        (rho * dl.sin(), -rho * dl.cos())
    }

    /// Projected metres to degrees (longitude, latitude).
    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let t = x.hypot(y) / self.akm1;
        let mut phi = FRAC_PI_2 - 2.0 * t.atan();
        for _ in 0..30 {
            let s = self.e * phi.sin();
            let next = FRAC_PI_2 - 2.0 * (t * ((1.0 - s) / (1.0 + s)).powf(self.e / 2.0)).atan();
            let done = (next - phi).abs() < 1e-15;
            phi = next;
            if done {
                break;
            }
        }
        // Height 0 on the projection's ellipsoid, as PROJ assumes.
        let lat = match self.datum {
            Some(ellipsoid) => shift_latitude(phi.to_degrees(), ellipsoid, wgs84()),
            None => phi.to_degrees(),
        };
        ((self.lon0 + x.atan2(-y)).to_degrees(), lat)
    }
}

fn mercator_y(lat: f64) -> f64 {
    MERCATOR_R * (FRAC_PI_4 + lat.to_radians() / 2.0).tan().ln()
}

fn mercator_lat(y: f64) -> f64 {
    (y / MERCATOR_R).sinh().atan().to_degrees()
}

// ---------------------------------------------------------------------------
// Lambert azimuthal equal-area on an ellipsoid (Snyder 1987, §24; PROJ `laea`)
// ---------------------------------------------------------------------------

/// Snyder's `q` (PROJ `pj_qsfn`) of `sin φ`.
fn qsfn(sinphi: f64, e: f64, one_es: f64) -> f64 {
    let con = e * sinphi;
    one_es * (sinphi / (1.0 - con * con) - 0.5 / e * ((1.0 - con) / (1.0 + con)).ln())
}

/// The latitude, in radians, whose `q` is `q`: Snyder's eq. 3-16 by
/// Newton's method. PROJ uses a series here; the two agree far below a
/// millimetre (`laea_matches_pyproj_both_ways`).
fn latitude_of_q(q: f64, e: f64, es: f64) -> f64 {
    let one_es = 1.0 - es;
    let mut phi = (q / 2.0).clamp(-1.0, 1.0).asin();
    for _ in 0..30 {
        let (sin, cos) = phi.sin_cos();
        if cos.abs() < 1e-12 {
            break;
        }
        let w = 1.0 - es * sin * sin;
        let next = phi
            + w * w / (2.0 * cos)
                * (q / one_es - sin / w + 0.5 / e * ((1.0 - e * sin) / (1.0 + e * sin)).ln());
        let done = (next - phi).abs() < 1e-15;
        phi = next;
        if done {
            break;
        }
    }
    phi
}

/// Lambert azimuthal equal-area, oblique or equatorial aspect, on an
/// ellipsoid: OPERA's grid (DEC-14). Metres with the false origin added,
/// the way PROJ writes them. The constants are PROJ's (`laea.cpp`).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Laea {
    a: f64,
    e: f64,
    es: f64,
    /// Central meridian, radians.
    lon0: f64,
    /// Latitude of origin, degrees.
    lat0: f64,
    x0: f64,
    y0: f64,
    /// `q` at the pole.
    qp: f64,
    /// Radius of the authalic sphere, in semi-major axes.
    rq: f64,
    /// The authalic latitude of the origin, as sine and cosine.
    sinb1: f64,
    cosb1: f64,
    /// Snyder's `D`, and the x and y factors built from it.
    dd: f64,
    xmf: f64,
    ymf: f64,
}

impl Laea {
    fn parse(projdef: &str) -> Result<Laea, CompositeError> {
        let params = Params::new(projdef);
        if params.get("proj") != Some("laea") {
            return Err(params.bad("not +proj=laea"));
        }
        if params.get("units").is_some_and(|u| u != "m") {
            return Err(params.bad("only metres (+units=m) are supported"));
        }
        params.unit_scale()?;
        params.zero_shift()?;
        let (a, inverse_flattening) = params.ellipsoid()?;
        let is_wgs84 = a == WGS84_A && (inverse_flattening - WGS84_RF).abs() < 1e-9;
        if params.get("towgs84").is_some() && !is_wgs84 {
            return Err(params.bad("+towgs84 on an ellipsoid other than WGS84 is not supported"));
        }
        if !inverse_flattening.is_finite() {
            return Err(params.bad("a spherical +proj=laea is not supported"));
        }
        let lat0 = params.number("lat_0")?.unwrap_or(0.0);
        if lat0.abs() > 90.0 - 1e-9 {
            return Err(params.bad("the polar aspect of +proj=laea is not supported"));
        }
        let f = 1.0 / inverse_flattening;
        let es = f * (2.0 - f);
        let e = es.sqrt();
        let one_es = 1.0 - es;
        let qp = qsfn(1.0, e, one_es);
        let rq = (0.5 * qp).sqrt();
        let phi0 = lat0.to_radians();
        let sinphi = phi0.sin();
        let sinb1 = qsfn(sinphi, e, one_es) / qp;
        let cosb1 = (1.0 - sinb1 * sinb1).sqrt();
        let dd = phi0.cos() / ((1.0 - es * sinphi * sinphi).sqrt() * rq * cosb1);
        Ok(Laea {
            a,
            e,
            es,
            lon0: params.number("lon_0")?.unwrap_or(0.0).to_radians(),
            lat0,
            x0: params.number("x_0")?.unwrap_or(0.0),
            y0: params.number("y_0")?.unwrap_or(0.0),
            qp,
            rq,
            sinb1,
            cosb1,
            dd,
            xmf: rq * dd,
            ymf: rq / dd,
        })
    }

    /// What a forward projection needs of a latitude in degrees: the sine
    /// and cosine of its authalic latitude.
    fn row(&self, lat: f64) -> (f64, f64) {
        let sinb = qsfn(lat.to_radians().sin(), self.e, 1.0 - self.es) / self.qp;
        let cosb2 = 1.0 - sinb * sinb;
        (sinb, if cosb2 > 0.0 { cosb2.sqrt() } else { 0.0 })
    }

    /// Projected metres from a latitude's `row` and the sine and cosine of
    /// the longitude from the central meridian.
    fn at(&self, (sinb, cosb): (f64, f64), (sinlam, coslam): (f64, f64)) -> (f64, f64) {
        let b = (2.0 / (1.0 + self.sinb1 * sinb + self.cosb1 * cosb * coslam)).sqrt();
        let x = self.xmf * b * cosb * sinlam;
        let y = self.ymf * b * (self.cosb1 * sinb - self.sinb1 * cosb * coslam);
        (self.a * x + self.x0, self.a * y + self.y0)
    }

    fn forward(&self, lon: f64, lat: f64) -> (f64, f64) {
        self.at(self.row(lat), (lon.to_radians() - self.lon0).sin_cos())
    }

    /// Projected metres to degrees (longitude, latitude); NaN beyond the
    /// projection's disc.
    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let x = (x - self.x0) / self.a / self.dd;
        let y = (y - self.y0) / self.a * self.dd;
        let rho = x.hypot(y);
        if rho < 1e-10 {
            return (self.lon0.to_degrees(), self.lat0);
        }
        let s = 0.5 * rho / self.rq;
        if s > 1.0 {
            return (f64::NAN, f64::NAN);
        }
        let (sin_ce, cos_ce) = (2.0 * s.asin()).sin_cos();
        let ab = (cos_ce * self.sinb1 + y * sin_ce * self.cosb1 / rho).clamp(-1.0, 1.0);
        let lam = (x * sin_ce).atan2(rho * self.cosb1 * cos_ce - y * self.sinb1 * sin_ce);
        let phi = if (ab.abs() - 1.0).abs() < 1e-15 {
            FRAC_PI_2.copysign(ab)
        } else {
            latitude_of_q(ab * self.qp, self.e, self.es)
        };
        ((self.lon0 + lam).to_degrees(), phi.to_degrees())
    }
}

// ---------------------------------------------------------------------------
// The projections a composite may use, and the texture's box
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Projection {
    Stereographic(Stereographic),
    Laea(Laea),
}

impl Projection {
    fn parse(projdef: &str) -> Result<Projection, CompositeError> {
        match Params::new(projdef).get("proj") {
            Some("stere") => Stereographic::parse(projdef).map(Projection::Stereographic),
            Some("laea") => Laea::parse(projdef).map(Projection::Laea),
            other => Err(fail(format!(
                "projdef {projdef:?}: +proj={} is not supported (only stere and laea)",
                other.unwrap_or("")
            ))),
        }
    }

    /// Central meridian, radians.
    fn lon0(&self) -> f64 {
        match self {
            Projection::Stereographic(s) => s.lon0,
            Projection::Laea(l) => l.lon0,
        }
    }

    fn forward(&self, lon: f64, lat: f64) -> (f64, f64) {
        match self {
            Projection::Stereographic(s) => s.forward(lon, lat),
            Projection::Laea(l) => l.forward(lon, lat),
        }
    }

    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            Projection::Stereographic(s) => s.inverse(x, y),
            Projection::Laea(l) => l.inverse(x, y),
        }
    }

    /// The part of a forward projection that depends on the latitude
    /// (degrees) alone, so a lattice pays for it once per row.
    fn row(&self, lat: f64) -> (f64, f64) {
        match self {
            Projection::Stereographic(s) => (s.rho(lat), 0.0),
            Projection::Laea(l) => l.row(lat),
        }
    }

    /// Projected metres from a latitude's `row` and the sine and cosine of
    /// the longitude from the central meridian.
    fn at(&self, row: (f64, f64), (sin, cos): (f64, f64)) -> (f64, f64) {
        match self {
            Projection::Stereographic(_) => (row.0 * sin, -row.0 * cos),
            Projection::Laea(l) => l.at(row, (sin, cos)),
        }
    }
}

/// A lon/lat box, degrees: what a texture covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LonLatBox {
    pub west: f64,
    pub east: f64,
    pub south: f64,
    pub north: f64,
}

/// The texture over a box: whole `PIXEL_M` Mercator texels counted from
/// its west and north edges, so its east and south edges move outward to
/// the last texel's edge.
struct Target {
    west: f64,
    north: f64,
    mx_west: f64,
    my_north: f64,
    width: u32,
    height: u32,
}

impl Target {
    fn over(b: LonLatBox) -> Target {
        let (mx_west, my_north) = (MERCATOR_R * b.west.to_radians(), mercator_y(b.north));
        Target {
            west: b.west,
            north: b.north,
            mx_west,
            my_north,
            width: ((MERCATOR_R * b.east.to_radians() - mx_west) / PIXEL_M).ceil() as u32,
            height: ((my_north - mercator_y(b.south)) / PIXEL_M).ceil() as u32,
        }
    }

    fn east(&self) -> f64 {
        (self.mx_west + f64::from(self.width) * PIXEL_M) / MERCATOR_R * 180.0 / std::f64::consts::PI
    }

    fn south(&self) -> f64 {
        mercator_lat(self.my_north - f64::from(self.height) * PIXEL_M)
    }

    /// Each texel's source pixel (column, row), or `None` off the grid, row
    /// by row from the north-west corner. Longitude depends only on the
    /// column and latitude only on the row, so each projection splits into
    /// a per-row and a per-column part.
    fn each_texel(&self, source: &Source, mut each: impl FnMut(Option<(usize, usize)>)) {
        let columns: Vec<(f64, f64)> = (0..self.width)
            .map(|c| {
                let lon = self.mx_west + (f64::from(c) + 0.5) * PIXEL_M;
                (lon / MERCATOR_R - source.proj.lon0()).sin_cos()
            })
            .collect();
        for r in 0..self.height {
            let lat = mercator_lat(self.my_north - (f64::from(r) + 0.5) * PIXEL_M);
            let row = source.proj.row(lat);
            for &column in &columns {
                let (x, y) = source.proj.at(row, column);
                let i = ((x - source.x0) / source.xscale).floor();
                let j = ((source.y0 - y) / source.yscale).floor();
                let inside = i >= 0.0
                    && j >= 0.0
                    && (i as usize) < source.xsize
                    && (j as usize) < source.ysize;
                each(inside.then_some((i as usize, j as usize)));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The decoded composite
// ---------------------------------------------------------------------------

/// A composite reprojected to its Web Mercator texture.
pub struct Grid {
    pub width: u32,
    pub height: u32,
    /// `width × height` codes, row-major, row 0 north: 0 undetect, 1 nodata
    /// or off the source grid, 2..255 measured.
    pub codes: Vec<u8>,
    /// The file's nominal time and its end time, milliseconds since the
    /// epoch. Both are the listing's `valid` for SMHI.
    pub start_ms: i64,
    pub end_ms: i64,
    /// `prodpar` of a PPI composite: the elevation it is built from.
    pub elevation_deg: f64,
    /// Outer edges of the texture, degrees.
    pub west: f64,
    pub east: f64,
    pub north: f64,
    pub south: f64,
    /// The source grid's `/where` projdef, as written.
    pub source_projdef: String,
}

impl Grid {
    /// Where the texture lies (`frame.grid`).
    pub fn placement(&self) -> GridPlacement {
        GridPlacement {
            projection: "EPSG:3857".to_owned(),
            xsize: self.width,
            ysize: self.height,
            xscale: PIXEL_M,
            yscale: PIXEL_M,
            west: self.west,
            east: self.east,
            north: self.north,
            south: self.south,
            source_projdef: self.source_projdef.clone(),
        }
    }

    /// RGBA pixels, `width × height`, with the sweep texture's channels:
    /// R palette class + 1 (0 draws nothing), G status bits, B raw code,
    /// A 255.
    pub fn texture(&self, bounds: &[i32], classes: usize) -> Vec<u8> {
        let class = |code: u8| {
            let value = (f32::from(code) - OFFSET) / SCALE;
            let above = bounds.partition_point(|&b| b as f32 <= value);
            above.saturating_sub(1).min(classes.saturating_sub(1)) as u8
        };
        self.codes
            .iter()
            .flat_map(|&code| {
                let (class, status) = match code {
                    0 => (0, BELOW_THRESHOLD),
                    1 => (0, OUTSIDE_COVERAGE),
                    _ => (class(code) + 1, 0),
                };
                [class, status, code, 255]
            })
            .collect()
    }

    /// A short description for the log.
    pub fn describe(&self) -> String {
        format!("{} × {} grid", self.width, self.height)
    }
}

/// The frame for a composite: the template's product, palette and bounds,
/// the composite's times and placement, and the station table's position.
/// `publish_frame` fills in the texture path; there is no azimuth lookup.
pub fn frame(template: &Frame, station: &Station, grid: &Grid) -> Frame {
    let compact = |ms: i64| {
        DateTime::from_timestamp_millis(ms)
            .map(|t| t.format("%Y%m%dT%H%M%SZ").to_string())
            .unwrap_or_default()
    };
    let iso = |ms: i64| {
        DateTime::from_timestamp_millis(ms)
            .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_default()
    };
    Frame {
        id: format!("{}-{}-e0", station.id, compact(grid.start_ms)),
        kind: FrameKind::Grid,
        product: template.product.clone(),
        product_name: template.product_name.clone(),
        units: template.units.clone(),
        elevation_deg: (grid.elevation_deg * 100.0).round() / 100.0,
        scan_time: iso(grid.start_ms),
        sweep_end: iso(grid.end_ms),
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
            lat: station.lat,
            lon: station.lon,
            alt_m: station.alt_m,
        },
        palette: template.palette.clone(),
        bounds: template.bounds.clone(),
        attribution: station.attribution.clone(),
        grid: Some(grid.placement()),
    }
}

/// The grid texture as a PNG, and an empty azimuth lookup: a grid frame
/// has none.
pub fn encode(grid: &Grid, frame: &Frame) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let pixels = grid.texture(&frame.bounds, frame.palette.len());
    Ok((png(grid.width, grid.height, &pixels)?, Vec::new()))
}

/// The poller's decoder (`smhi_live::Decode`) for the composite.
pub fn decode_scan(reader: RangeReader) -> Result<Scan, String> {
    decode(reader).map(Scan::Grid).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// ODIM
// ---------------------------------------------------------------------------

type Attrs = HashMap<String, AttrValue>;

fn attrs(file: &File, path: &str) -> Result<Attrs, CompositeError> {
    let group = file.group(path).map_err(|e| fail(format!("{path}: {e}")))?;
    group
        .attrs()
        .map_err(|e| fail(format!("{path} attributes: {e}")))
}

fn num(attrs: &Attrs, key: &str) -> Option<f64> {
    Some(match attrs.get(key)? {
        AttrValue::F32(v) => f64::from(*v),
        AttrValue::F64(v) => *v,
        AttrValue::I8(v) => f64::from(*v),
        AttrValue::I16(v) => f64::from(*v),
        AttrValue::I32(v) => f64::from(*v),
        AttrValue::I64(v) => *v as f64,
        AttrValue::U8(v) => f64::from(*v),
        AttrValue::U16(v) => f64::from(*v),
        AttrValue::U32(v) => f64::from(*v),
        AttrValue::U64(v) => *v as f64,
        _ => return None,
    })
}

fn need(attrs: &Attrs, path: &str, key: &str) -> Result<f64, CompositeError> {
    num(attrs, key).ok_or_else(|| fail(format!("{path}: no numeric {key}")))
}

fn text(attrs: &Attrs, key: &str) -> Option<String> {
    let value = match attrs.get(key)? {
        AttrValue::String(s)
        | AttrValue::AsciiString(s)
        | AttrValue::VarLenAsciiString(s)
        | AttrValue::VarLenString(s)
        | AttrValue::AsciiStringSized { value: s, .. }
        | AttrValue::StringSized { value: s, .. } => s,
        _ => return None,
    };
    Some(value.trim_end_matches('\0').trim().to_owned())
}

fn nominal_ms(what: &Attrs, date: &str, time: &str) -> Option<i64> {
    let stamp = format!("{}{}", text(what, date)?, text(what, time)?);
    let parsed = NaiveDateTime::parse_from_str(&stamp, "%Y%m%d%H%M%S").ok()?;
    Some(parsed.and_utc().timestamp_millis())
}

/// One ODIM value as a polar-convention reflectivity byte.
fn requantize(value: f64, gain: f64, offset: f64, nodata: f64, undetect: f64) -> u8 {
    if value == undetect {
        0
    } else if value == nodata {
        1
    } else {
        let dbz = value * gain + offset;
        (dbz * f64::from(SCALE) + f64::from(OFFSET))
            .round()
            .clamp(2.0, 255.0) as u8
    }
}

/// A layer's value coding (`what/gain`, `offset`, `nodata`, `undetect`).
#[derive(Clone, Copy, Debug)]
struct Coding {
    gain: f64,
    offset: f64,
    nodata: f64,
    undetect: f64,
}

impl Coding {
    fn code(&self, value: f64) -> u8 {
        requantize(value, self.gain, self.offset, self.nodata, self.undetect)
    }
}

/// The source grid as `/where` describes it.
struct Source {
    proj: Projection,
    xsize: usize,
    ysize: usize,
    xscale: f64,
    yscale: f64,
    /// The upper-left pixel's outer corner, projected metres.
    x0: f64,
    y0: f64,
}

impl Source {
    fn read(where_: &Attrs) -> Result<(Source, String), CompositeError> {
        let path = "/where";
        let projdef = text(where_, "projdef").ok_or_else(|| fail("/where: no projdef"))?;
        let proj = Projection::parse(&projdef)?;
        let xsize = need(where_, path, "xsize")? as usize;
        let ysize = need(where_, path, "ysize")? as usize;
        let (xscale, yscale) = (need(where_, path, "xscale")?, need(where_, path, "yscale")?);
        if xsize == 0 || ysize == 0 || xscale <= 0.0 || yscale <= 0.0 {
            return Err(fail(format!(
                "/where: {xsize} × {ysize} pixels of {xscale} × {yscale} m"
            )));
        }
        let corner = |name: &str| -> Result<(f64, f64), CompositeError> {
            Ok(proj.forward(
                need(where_, path, &format!("{name}_lon"))?,
                need(where_, path, &format!("{name}_lat"))?,
            ))
        };
        let (x0, y0) = corner("UL")?;
        // The lower-right corner must sit where the size says, or the
        // corners are not what this decoder takes them for (outer corners
        // of the corner pixels, row 0 north).
        let (x1, y1) = corner("LR")?;
        let (dx, dy) = (
            x1 - (x0 + xsize as f64 * xscale),
            y1 - (y0 - ysize as f64 * yscale),
        );
        if dx.abs() > xscale / 2.0 || dy.abs() > yscale / 2.0 {
            return Err(fail(format!(
                "/where: the LR corner is {dx:.0} m, {dy:.0} m from where the grid size puts it"
            )));
        }
        Ok((
            Source {
                proj,
                xsize,
                ysize,
                xscale,
                yscale,
                x0,
                y0,
            },
            projdef,
        ))
    }

    /// The lon/lat box of the grid's outer boundary, sampled at every
    /// pixel edge.
    fn bounds(&self) -> LonLatBox {
        let (x1, y1) = (
            self.x0 + self.xsize as f64 * self.xscale,
            self.y0 - self.ysize as f64 * self.yscale,
        );
        let across = (0..=self.xsize).flat_map(|k| {
            let x = self.x0 + k as f64 * self.xscale;
            [(x, self.y0), (x, y1)]
        });
        let down = (0..=self.ysize).flat_map(|k| {
            let y = self.y0 - k as f64 * self.yscale;
            [(self.x0, y), (x1, y)]
        });
        let (west, east, south, north) = across.chain(down).fold(
            (
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ),
            |(w, e, s, n), (x, y)| {
                let (lon, lat) = self.proj.inverse(x, y);
                (w.min(lon), e.max(lon), s.min(lat), n.max(lat))
            },
        );
        LonLatBox {
            west,
            east,
            south,
            north,
        }
    }
}

/// One reader shared by the HDF5 parser and the chunk reads that go around
/// it. Both seek before every read (`ReadSeekSource` does), so neither
/// disturbs the other's position.
struct Shared<R>(Arc<Mutex<R>>);

impl<R> Clone for Shared<R> {
    fn clone(&self) -> Self {
        Shared(self.0.clone())
    }
}

impl<R> Shared<R> {
    fn new(reader: R) -> Self {
        Shared(Arc::new(Mutex::new(reader)))
    }

    fn lock(&self) -> MutexGuard<'_, R> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<R: Read + Seek> Shared<R> {
    /// `len` bytes from `offset`, in one read, so a ranged reader fetches
    /// whatever of them it lacks in one request.
    fn read_at(&self, offset: u64, len: u64) -> io::Result<Vec<u8>> {
        let mut reader = self.lock();
        reader.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; len as usize];
        reader.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

impl<R: Read> Read for Shared<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.lock().read(buf)
    }
}

impl<R: Seek> Seek for Shared<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.lock().seek(to)
    }
}

/// HDF5's deflate and shuffle filters, the only ones read.
const DEFLATE: u16 = 1;
const SHUFFLE: u16 = 2;

/// How a layer stores one value. Only little-endian numbers are read.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Element {
    U8,
    I16,
    U16,
    F32,
    F64,
}

impl Element {
    fn of(datatype: &Datatype) -> Result<Element, String> {
        use DatatypeByteOrder::LittleEndian;
        Ok(match datatype {
            Datatype::FloatingPoint {
                size: 8,
                byte_order: LittleEndian,
                ..
            } => Element::F64,
            Datatype::FloatingPoint {
                size: 4,
                byte_order: LittleEndian,
                ..
            } => Element::F32,
            Datatype::FixedPoint {
                size: 1,
                signed: false,
                ..
            } => Element::U8,
            Datatype::FixedPoint {
                size: 2,
                byte_order: LittleEndian,
                signed,
                ..
            } => {
                if *signed {
                    Element::I16
                } else {
                    Element::U16
                }
            }
            other => return Err(format!("values stored as {other:?} are not supported")),
        })
    }

    fn size(self) -> usize {
        match self {
            Element::U8 => 1,
            Element::I16 | Element::U16 => 2,
            Element::F32 => 4,
            Element::F64 => 8,
        }
    }

    /// The value in `bytes`, which are exactly `size` long.
    fn value(self, bytes: &[u8]) -> f64 {
        match self {
            Element::U8 => f64::from(bytes[0]),
            Element::I16 => f64::from(i16::from_le_bytes([bytes[0], bytes[1]])),
            Element::U16 => f64::from(u16::from_le_bytes([bytes[0], bytes[1]])),
            Element::F32 => f64::from(f32::from_le_bytes(bytes.try_into().unwrap_or_default())),
            Element::F64 => f64::from_le_bytes(bytes.try_into().unwrap_or_default()),
        }
    }
}

/// A stored chunk with its filters undone, last first, skipping those its
/// mask marks as not applied. It must come to exactly `expected` bytes.
fn unfilter(
    stored: &[u8],
    filters: &[u16],
    mask: u32,
    element: usize,
    expected: usize,
) -> Result<Vec<u8>, String> {
    let mut bytes = stored.to_vec();
    for (i, &id) in filters.iter().enumerate().rev() {
        if i < 32 && (mask >> i) & 1 == 1 {
            continue;
        }
        bytes = match id {
            DEFLATE => {
                let mut out = Vec::with_capacity(expected);
                flate2::read::ZlibDecoder::new(&bytes[..])
                    .take(expected as u64 + 1)
                    .read_to_end(&mut out)
                    .map_err(|e| format!("inflating a chunk: {e}"))?;
                out
            }
            SHUFFLE => unshuffle(&bytes, element),
            other => return Err(format!("HDF5 filter {other} is not supported")),
        };
    }
    if bytes.len() != expected {
        return Err(format!(
            "a chunk decodes to {} bytes, not {expected}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

/// HDF5's shuffle undone: the first bytes of every element, then the
/// second bytes, and so on, back into whole elements.
fn unshuffle(bytes: &[u8], element: usize) -> Vec<u8> {
    let n = bytes.len() / element.max(1);
    let mut out = bytes.to_vec();
    for (k, &b) in bytes[..n * element].iter().enumerate() {
        out[(k % n) * element + k / n] = b;
    }
    out
}

/// The layer as codes, `ysize × xsize`, row 0 north. A layer stored in
/// more than one chunk is read chunk by chunk (`read_chunks`); anything
/// else is read whole.
fn read_codes<R: Read + Seek>(
    dataset: &Dataset,
    reader: &Shared<R>,
    source: &Source,
    target: &Target,
    coding: Coding,
) -> Result<Vec<u8>, String> {
    let shape = dataset.shape().map_err(|e| e.to_string())?;
    if shape != [source.ysize as u64, source.xsize as u64] {
        return Err(format!(
            "shape {shape:?}, not {} × {}",
            source.ysize, source.xsize
        ));
    }
    let chunk = match dataset.chunk_shape().map_err(|e| e.to_string())?.as_deref() {
        Some(&[rows, cols]) if rows > 0 && cols > 0 => Some((rows as usize, cols as usize)),
        _ => None,
    };
    match chunk {
        Some((rows, cols)) if rows < source.ysize || cols < source.xsize => {
            read_chunks(dataset, reader, source, target, coding, (rows, cols))
        }
        _ => {
            let values = dataset.read_f64().map_err(|e| e.to_string())?;
            if values.len() != source.xsize * source.ysize {
                return Err(format!(
                    "holds {} values, not {} × {}",
                    values.len(),
                    source.ysize,
                    source.xsize
                ));
            }
            Ok(values.into_iter().map(|v| coding.code(v)).collect())
        }
    }
}

/// The chunks some texel of `target` falls in, and only those, one read
/// per run of chunks stored back to back. The rest of the grid, and a
/// needed chunk the file does not allocate, is nodata.
fn read_chunks<R: Read + Seek>(
    dataset: &Dataset,
    reader: &Shared<R>,
    source: &Source,
    target: &Target,
    coding: Coding,
    (rows, cols): (usize, usize),
) -> Result<Vec<u8>, String> {
    let across = source.xsize.div_ceil(cols);
    let down = source.ysize.div_ceil(rows);
    let mut needed = vec![false; across * down];
    target.each_texel(source, |pixel| {
        if let Some((i, j)) = pixel {
            needed[j / rows * across + i / cols] = true;
        }
    });
    let element = Element::of(&dataset.datatype().map_err(|e| e.to_string())?)?;
    let filters = dataset.filters();
    if let Some(id) = filters.iter().find(|&&id| id != DEFLATE && id != SHUFFLE) {
        return Err(format!("HDF5 filter {id} is not supported"));
    }
    let place = |c: &Chunk| match c.offset[..] {
        [r, k] => {
            let (r, k) = (r as usize, k as usize);
            (r % rows == 0 && k % cols == 0 && r < source.ysize && k < source.xsize)
                .then_some((r, k))
        }
        _ => None,
    };
    let mut chunks: Vec<Chunk> = dataset
        .chunks()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|c| place(c).is_some_and(|(r, k)| needed[r / rows * across + k / cols]))
        .collect();
    chunks.sort_by_key(|c| c.address);

    let size = element.size();
    let expected = rows * cols * size;
    let mut codes = vec![1u8; source.xsize * source.ysize];
    let mut at = 0;
    while at < chunks.len() {
        let first = chunks[at].address;
        let mut end = first + chunks[at].storage_size;
        let mut next = at + 1;
        while next < chunks.len() && chunks[next].address == end {
            end += chunks[next].storage_size;
            next += 1;
        }
        let run = reader
            .read_at(first, end - first)
            .map_err(|e| format!("reading chunks at {first}: {e}"))?;
        for chunk in &chunks[at..next] {
            let start = (chunk.address - first) as usize;
            let stored = &run[start..start + chunk.storage_size as usize];
            let raw = unfilter(stored, &filters, chunk.filter_mask, size, expected)?;
            let Some((r0, k0)) = place(chunk) else {
                continue;
            };
            let width = cols.min(source.xsize - k0);
            for r in 0..rows.min(source.ysize - r0) {
                let line = &raw[r * cols * size..][..width * size];
                let dest = &mut codes[(r0 + r) * source.xsize + k0..][..width];
                for (code, value) in dest.iter_mut().zip(line.chunks_exact(size)) {
                    *code = coding.code(element.value(value));
                }
            }
        }
        at = next;
    }
    Ok(codes)
}

/// Decode a composite and reproject its whole grid to its Web Mercator
/// texture. `reader` may be a file, a buffer, or a ranged HTTP reader;
/// only the metadata and the DBZH layer are read.
pub fn decode<R: Read + Seek + Send + 'static>(reader: R) -> Result<Grid, CompositeError> {
    decode_box(reader, None)
}

/// `decode`, with the texture over `crop` instead of the whole grid when
/// one is given. Of a layer stored in many chunks, only the chunks under
/// the texture are read.
pub fn decode_box<R: Read + Seek + Send + 'static>(
    reader: R,
    crop: Option<LonLatBox>,
) -> Result<Grid, CompositeError> {
    let reader = Shared::new(reader);
    let hdf5 = ReadSeekSource::new(reader.clone())
        .map_err(|e| fail(format!("reading the composite: {e}")))?;
    let file = File::from_source(hdf5)
        .map_err(|e| fail(format!("opening the composite as HDF5: {e}")))?;

    let what = attrs(&file, "/what")?;
    if let Some(object) = text(&what, "object")
        && object != "COMP"
    {
        return Err(fail(format!("/what: object {object}, not COMP")));
    }
    let where_ = attrs(&file, "/where")?;
    let (source, projdef) = Source::read(&where_)?;

    const DS: &str = "/dataset1";
    let dwhat = attrs(&file, &format!("{DS}/what")).unwrap_or_default();
    let (data, dataw) = (1..)
        .map_while(|m| {
            let path = format!("{DS}/data{m}");
            attrs(&file, &format!("{path}/what"))
                .ok()
                .map(|a| (path, a))
        })
        .find(|(_, a)| text(a, "quantity").as_deref() == Some("DBZH"))
        .ok_or_else(|| fail(format!("{DS} has no DBZH")))?;
    let coding = |key: &str| {
        num(&dataw, key)
            .or_else(|| num(&dwhat, key))
            .ok_or_else(|| fail(format!("{data}/what: no {key}")))
    };
    let coding = Coding {
        gain: coding("gain")?,
        offset: coding("offset")?,
        nodata: coding("nodata")?,
        undetect: coding("undetect")?,
    };
    let start_ms = nominal_ms(&what, "date", "time")
        .or_else(|| nominal_ms(&dwhat, "startdate", "starttime"))
        .ok_or_else(|| fail("/what: no date and time"))?;
    let end_ms = nominal_ms(&dwhat, "enddate", "endtime").unwrap_or(start_ms);
    let elevation_deg = num(&dwhat, "prodpar").unwrap_or(0.0);

    let target = Target::over(crop.unwrap_or_else(|| source.bounds()));
    let dataset = file
        .dataset(&format!("{data}/data"))
        .map_err(|e| fail(format!("{data}/data: {e}")))?;
    let codes = read_codes(&dataset, &reader, &source, &target, coding)
        .map_err(|e| fail(format!("{data}/data: {e}")))?;
    let mut out = Vec::with_capacity(target.width as usize * target.height as usize);
    target.each_texel(&source, |pixel| {
        out.push(pixel.map_or(1, |(i, j)| codes[j * source.xsize + i]));
    });
    Ok(Grid {
        width: target.width,
        height: target.height,
        codes: out,
        start_ms,
        end_ms,
        elevation_deg,
        west: target.west,
        east: target.east(),
        north: target.north,
        south: target.south(),
        source_projdef: projdef,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smhi_live::{self, API, RangeSource};
    use serde::Deserialize;
    use std::io::Cursor;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../data/raw/radar_sweden_comp_202609132220.h5"
    );
    const GOLDEN: &str = include_str!("../../golden/sweden-20260913/grid.json");
    const LISTING: &str = include_str!("../tests/smhi/sweden-comp-20260913T2224Z.json");
    const DAY: &str = include_str!("../tests/smhi/sweden-comp-day-20260913.json");

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Golden {
        sha256: String,
        projdef: String,
        date: String,
        time: String,
        prodpar: f64,
        ul_x: f64,
        ul_y: f64,
        lr_x: f64,
        lr_y: f64,
        source_counts: Counts,
        width: u32,
        height: u32,
        west: f64,
        east: f64,
        north: f64,
        south: f64,
        counts: Counts,
        near_edge: u64,
        projection: Vec<Point>,
        samples: Vec<Sample>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Counts {
        measured: u64,
        undetect: u64,
        #[serde(alias = "nodataOrOutside")]
        nodata: u64,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Point {
        lon: f64,
        lat: f64,
        x: f64,
        y: f64,
        /// pyproj's inverse of `x`, `y`, which assumes height 0 on Bessel.
        inverse_lon: f64,
        inverse_lat: f64,
    }
    #[derive(Deserialize)]
    struct Sample {
        col: u32,
        row: u32,
        lon: f64,
        lat: f64,
        code: u8,
    }

    fn golden() -> Golden {
        serde_json::from_str(GOLDEN).unwrap()
    }

    fn fixture() -> Vec<u8> {
        std::fs::read(FIXTURE).expect("run bash scripts/extract-fixtures.sh first")
    }

    fn counts(codes: &[u8]) -> (u64, u64, u64) {
        codes.iter().fold((0, 0, 0), |(m, u, n), &c| match c {
            0 => (m, u + 1, n),
            1 => (m, u, n + 1),
            _ => (m + 1, u, n),
        })
    }

    #[test]
    fn the_fixture_is_the_one_the_golden_file_describes() {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(fixture());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, golden().sha256);
    }

    #[test]
    fn stereographic_matches_pyproj_both_ways() {
        let g = golden();
        let stereo = Stereographic::parse(&g.projdef).unwrap();
        for p in &g.projection {
            let (x, y) = stereo.forward(p.lon, p.lat);
            assert!(
                (x - p.x).abs() < 1e-3 && (y - p.y).abs() < 1e-3,
                "forward {}, {}: {x}, {y} vs {}, {}",
                p.lon,
                p.lat,
                p.x,
                p.y
            );
            let (lon, lat) = stereo.inverse(p.x, p.y);
            assert!(
                (lon - p.inverse_lon).abs() < 1e-8 && (lat - p.inverse_lat).abs() < 1e-8,
                "inverse {}, {}: {lon}, {lat}",
                p.x,
                p.y
            );
        }
    }

    #[test]
    fn the_composite_reprojects_like_pyproj() {
        let g = golden();
        let grid = decode(Cursor::new(fixture())).unwrap();
        assert_eq!((grid.width, grid.height), (g.width, g.height));
        for (ours, theirs, edge) in [
            (grid.west, g.west, "west"),
            (grid.east, g.east, "east"),
            (grid.north, g.north, "north"),
            (grid.south, g.south, "south"),
        ] {
            assert!((ours - theirs).abs() < 1e-8, "{edge}: {ours} vs {theirs}");
        }
        assert_eq!(grid.source_projdef, g.projdef);
        let when = NaiveDateTime::parse_from_str(&format!("{}{}", g.date, g.time), "%Y%m%d%H%M%S")
            .unwrap()
            .and_utc()
            .timestamp_millis();
        assert_eq!((grid.start_ms, grid.end_ms), (when, when));
        assert_eq!(grid.elevation_deg, g.prodpar);

        // Every sampled texel holds exactly the code pyproj puts there.
        for s in &g.samples {
            let code = grid.codes[(s.row * grid.width + s.col) as usize];
            assert_eq!(
                code, s.code,
                "texel {}, {} ({}, {})",
                s.col, s.row, s.lon, s.lat
            );
        }
        // The whole texture agrees but for centres within a millimetre of
        // a source pixel edge, which may fall either way.
        let (measured, undetect, nodata) = counts(&grid.codes);
        for (ours, theirs, what) in [
            (measured, g.counts.measured, "measured"),
            (undetect, g.counts.undetect, "undetect"),
            (nodata, g.counts.nodata, "nodata or outside"),
        ] {
            assert!(
                ours.abs_diff(theirs) <= g.near_edge,
                "{what}: {ours} vs {theirs}"
            );
        }
    }

    #[test]
    fn the_source_grid_decodes_like_h5py() {
        let g = golden();
        let bytes = fixture();
        let file = File::from_source(ReadSeekSource::new(Cursor::new(bytes)).unwrap()).unwrap();
        let (source, _) = Source::read(&attrs(&file, "/where").unwrap()).unwrap();
        assert!((source.x0 - g.ul_x).abs() < 1e-3 && (source.y0 - g.ul_y).abs() < 1e-3);
        let (x1, y1) = source.proj.forward(
            num(&attrs(&file, "/where").unwrap(), "LR_lon").unwrap(),
            num(&attrs(&file, "/where").unwrap(), "LR_lat").unwrap(),
        );
        assert!((x1 - g.lr_x).abs() < 1e-3 && (y1 - g.lr_y).abs() < 1e-3);
        let values = file
            .dataset("/dataset1/data1/data")
            .unwrap()
            .read_u8()
            .unwrap();
        let codes: Vec<u8> = values
            .iter()
            .map(|&v| requantize(f64::from(v), 0.4, -30.0, 255.0, 0.0))
            .collect();
        assert_eq!(
            counts(&codes),
            (
                g.source_counts.measured,
                g.source_counts.undetect,
                g.source_counts.nodata
            )
        );
    }

    #[test]
    fn texture_channels_follow_the_protocol() {
        let grid = Grid {
            width: 3,
            height: 1,
            codes: vec![0, 1, 2 * 30 + 66],
            start_ms: 0,
            end_ms: 0,
            elevation_deg: 0.5,
            west: 0.0,
            east: 1.0,
            north: 1.0,
            south: 0.0,
            source_projdef: String::new(),
        };
        let bounds = [-32, 0, 10, 20, 30, 40];
        let pixels = grid.texture(&bounds, bounds.len() - 1);
        assert_eq!(
            pixels,
            vec![
                0,
                BELOW_THRESHOLD,
                0,
                255, //
                0,
                OUTSIDE_COVERAGE,
                1,
                255, //
                5,
                0,
                126,
                255, // 30 dBZ is the fifth band, class 4
            ]
        );
    }

    #[test]
    fn requantizes_like_the_polar_convention() {
        assert_eq!(requantize(0.0, 0.4, -30.0, 255.0, 0.0), 0);
        assert_eq!(requantize(255.0, 0.4, -30.0, 255.0, 0.0), 1);
        // 1 → -29.6 dBZ → 6.8 → 7; 75 → 0 dBZ → 66; 254 → 71.6 dBZ → 209.
        assert_eq!(requantize(1.0, 0.4, -30.0, 255.0, 0.0), 7);
        assert_eq!(requantize(75.0, 0.4, -30.0, 255.0, 0.0), 66);
        assert_eq!(requantize(254.0, 0.4, -30.0, 255.0, 0.0), 209);
        // OPERA's floats: dBZ itself, and sentinels far below any echo.
        let opera = |v| requantize(v, 1.0, 0.0, -9_999_000.0, -8_888_000.0);
        assert_eq!(opera(-8_888_000.0), 0);
        assert_eq!(opera(-9_999_000.0), 1);
        assert_eq!(opera(-32.0), 2);
        assert_eq!(opera(-31.5), 3);
        assert_eq!(opera(0.0), 66);
        assert_eq!(opera(69.5), 205);
        assert_eq!(opera(100.0), 255);
    }

    #[test]
    fn only_the_north_polar_stereographic_form_is_accepted() {
        for bad in [
            "+proj=merc +ellps=WGS84",
            "+proj=stere +ellps=bessel +lat_0=-90 +lon_0=14 +lat_ts=-60",
            "+proj=stere +ellps=bessel +lat_0=90 +lon_0=14 +lat_ts=60 +towgs84=414,41,603",
            "+proj=stere +ellps=bessel +lat_0=90 +lon_0=14 +lat_ts=60 +x_0=100",
            "+proj=stere +ellps=clrk66 +lat_0=90",
            "+proj=stere +lat_0=90",
        ] {
            assert!(
                Stereographic::parse(bad).is_err(),
                "{bad} should be rejected"
            );
        }
        let named = Stereographic::parse("+proj=stere +ellps=WGS84 +lat_0=90 +lat_ts=70").unwrap();
        let spelled =
            Stereographic::parse("+proj=stere +a=6378137 +rf=298.257223563 +lat_0=90 +lat_ts=70")
                .unwrap();
        assert_eq!(named, spelled);
    }

    #[test]
    fn other_input_is_an_error_not_a_panic() {
        assert!(decode(Cursor::new(b"not hdf5 at all".to_vec())).is_err());
        // A polar volume is ODIM too, but not a composite.
        let vara = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../data/raw/radar_vara_qcvol_202609131055.h5"
        ));
        if let Ok(vara) = vara {
            assert!(decode(Cursor::new(vara)).is_err());
        }
    }

    /// The fixture served the way SMHI serves it: byte ranges with the
    /// length in `Content-Range`.
    struct Local(Vec<u8>);
    impl RangeSource for Local {
        fn get(&mut self, offset: u64, len: u64) -> io::Result<(Vec<u8>, Option<u64>)> {
            let start = (offset as usize).min(self.0.len());
            let end = (start + len as usize).min(self.0.len());
            Ok((self.0[start..end].to_vec(), Some(self.0.len() as u64)))
        }
    }

    #[test]
    fn a_ranged_read_costs_a_few_requests() {
        let reader = RangeReader::open(Box::new(Local(fixture()))).unwrap();
        let traffic = reader.traffic();
        let Scan::Grid(grid) = decode_scan(reader).unwrap() else {
            panic!("the composite decoder returned a sweep");
        };
        assert_eq!(grid.width, golden().width);
        assert!(
            traffic.requests() <= 10 && traffic.bytes() <= 160 * 1024,
            "{} requests, {} bytes of {}",
            traffic.requests(),
            traffic.bytes(),
            traffic.total()
        );
    }

    #[test]
    fn the_listing_names_composites_by_their_dated_url() {
        let newest = smhi_live::newest_volume(LISTING.as_bytes(), API, AREA)
            .unwrap()
            .unwrap();
        assert_eq!(newest.key, "radar_sweden_comp_202609132220");
        assert_eq!(
            newest.url,
            "https://opendata-download-radar.smhi.se/api/version/latest/area/sweden/product/comp/2026/09/13/radar_sweden_comp_202609132220.h5"
        );
        let next = smhi_live::next_volume(API, AREA, newest.valid_ms + 5 * 60 * 1000);
        assert_eq!(next.key, "radar_sweden_comp_202609132225");
        assert_eq!(
            smhi_live::listing_url(API, AREA),
            format!("{API}/area/sweden/product/comp.json")
        );

        // The day listing mixes png/tif images with the h5 files; only
        // the h5 files are volumes, and every built URL is SMHI's link.
        #[derive(Deserialize)]
        struct Day {
            files: Vec<Listed>,
        }
        #[derive(Deserialize)]
        struct Listed {
            key: String,
            formats: Vec<Format>,
        }
        #[derive(Deserialize)]
        struct Format {
            key: String,
            link: String,
        }
        let day: Day = serde_json::from_str(DAY).unwrap();
        let links: HashMap<String, String> = day
            .files
            .into_iter()
            .filter_map(|f| {
                let link = f.formats.into_iter().find(|x| x.key == "h5")?.link;
                Some((f.key, link))
            })
            .collect();
        let volumes = smhi_live::day_volumes(DAY.as_bytes(), API, AREA).unwrap();
        assert_eq!(volumes.len(), 269);
        assert_eq!(volumes.len(), links.len());
        for v in &volumes {
            assert_eq!(Some(&v.url), links.get(&v.key), "{}", v.key);
        }
    }

    #[test]
    fn the_frame_places_the_texture_and_carries_no_polar_geometry() {
        let grid = decode(Cursor::new(fixture())).unwrap();
        let template: Frame = serde_json::from_str(include_str!("../data/fixture.json")).unwrap();
        let frame = frame(&template, &station(), &grid);
        assert_eq!(frame.id, "sweden-20260913T222000Z-e0");
        assert_eq!(frame.kind, FrameKind::Grid);
        assert_eq!(frame.scan_time, "2026-09-13T22:20:00Z");
        assert_eq!(
            (
                frame.rays,
                frame.gates,
                frame.first_gate_m,
                frame.gate_spacing_m
            ),
            (0, 0, 0, 0)
        );
        assert_eq!((frame.scale, frame.offset), (SCALE, OFFSET));
        let placement = frame.grid.as_ref().unwrap();
        assert_eq!(
            (placement.xsize, placement.ysize),
            (grid.width, grid.height)
        );
        let (texture, lut) = encode(&grid, &frame).unwrap();
        assert!(lut.is_empty());
        let decoder = png::Decoder::new(Cursor::new(&texture));
        let reader = decoder.read_info().unwrap();
        assert_eq!(
            (reader.info().width, reader.info().height),
            (grid.width, grid.height)
        );
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["kind"], "grid");
        assert_eq!(json["grid"]["projection"], "EPSG:3857");
        assert_eq!(json["grid"]["sourceProjdef"], grid.source_projdef);
    }

    // -----------------------------------------------------------------------
    // OPERA's Nordic crop (S16, DEC-14)
    // -----------------------------------------------------------------------

    const NORDIC_FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../data/raw/opera_nordic_202609140810.h5"
    );
    const NORDIC_GOLDEN: &str = include_str!("../../golden/nordic-20260914/grid.json");

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Nordic {
        sha256: String,
        projdef: String,
        date: String,
        time: String,
        end_date: String,
        end_time: String,
        chunk_rows: usize,
        chunk_cols: usize,
        ul_x: f64,
        ul_y: f64,
        lr_x: f64,
        lr_y: f64,
        #[serde(rename = "box")]
        crop: Box4,
        needed_chunks: Vec<ChunkAt>,
        allocated_chunks: Vec<Allocated>,
        width: u32,
        height: u32,
        west: f64,
        east: f64,
        north: f64,
        south: f64,
        counts: Counts,
        near_edge: u64,
        projection: Vec<Point>,
        samples: Vec<Sample>,
    }
    #[derive(Deserialize)]
    struct Box4 {
        west: f64,
        east: f64,
        south: f64,
        north: f64,
    }
    #[derive(Deserialize)]
    struct ChunkAt {
        row: usize,
        col: usize,
    }
    #[derive(Deserialize)]
    struct Allocated {
        row: usize,
        col: usize,
        address: u64,
        size: u64,
        needed: bool,
        counts: Option<Counts>,
    }

    impl Nordic {
        fn crop(&self) -> LonLatBox {
            LonLatBox {
                west: self.crop.west,
                east: self.crop.east,
                south: self.crop.south,
                north: self.crop.north,
            }
        }
    }

    fn nordic() -> Nordic {
        serde_json::from_str(NORDIC_GOLDEN).unwrap()
    }

    fn nordic_fixture() -> Vec<u8> {
        std::fs::read(NORDIC_FIXTURE).expect("run bash scripts/extract-fixtures.sh first")
    }

    fn utc(date: &str, time: &str) -> i64 {
        NaiveDateTime::parse_from_str(&format!("{date}{time}"), "%Y%m%d%H%M%S")
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    #[test]
    fn the_nordic_fixture_is_the_one_its_golden_file_describes() {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(nordic_fixture());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, nordic().sha256);
    }

    #[test]
    fn laea_matches_pyproj_both_ways() {
        let g = nordic();
        let laea = Laea::parse(&g.projdef).unwrap();
        for p in &g.projection {
            let (x, y) = laea.forward(p.lon, p.lat);
            assert!(
                (x - p.x).abs() < 1e-3 && (y - p.y).abs() < 1e-3,
                "forward {}, {}: {x}, {y} vs {}, {}",
                p.lon,
                p.lat,
                p.x,
                p.y
            );
            let (lon, lat) = laea.inverse(p.x, p.y);
            assert!(
                (lon - p.inverse_lon).abs() < 1e-9 && (lat - p.inverse_lat).abs() < 1e-9,
                "inverse {}, {}: {lon}, {lat} vs {}, {}",
                p.x,
                p.y,
                p.inverse_lon,
                p.inverse_lat
            );
        }
    }

    #[test]
    fn only_laea_forms_it_can_draw_are_accepted() {
        for bad in [
            "+proj=laea +lat_0=90 +lon_0=10 +ellps=WGS84",
            "+proj=laea +lat_0=55 +lon_0=10 +a=6371000",
            "+proj=laea +lat_0=55 +lon_0=10 +ellps=WGS84 +units=km",
            "+proj=laea +lat_0=55 +lon_0=10 +ellps=WGS84 +towgs84=1,2,3",
            "+proj=laea +lat_0=55 +lon_0=10 +ellps=bessel +towgs84=0,0,0",
            "+proj=laea +lat_0=55 +lon_0=10",
        ] {
            assert!(Laea::parse(bad).is_err(), "{bad} should be rejected");
        }
        assert!(Projection::parse("+proj=merc +ellps=WGS84").is_err());
        let named = Laea::parse("+proj=laea +lat_0=55 +lon_0=10 +ellps=WGS84").unwrap();
        let spelled =
            Laea::parse("+proj=laea +lat_0=55 +lon_0=10 +a=6378137 +rf=298.257223563").unwrap();
        assert_eq!(named, spelled);
        // The equatorial aspect is the oblique formulas' limit.
        let equator = Laea::parse("+proj=laea +lat_0=0 +lon_0=0 +ellps=WGS84").unwrap();
        let (x, y) = equator.forward(0.0, 0.0);
        assert!(x.abs() < 1e-9 && y.abs() < 1e-9);
        let (x, y) = equator.forward(10.0, 20.0);
        let (lon, lat) = equator.inverse(x, y);
        assert!((lon - 10.0).abs() < 1e-9 && (lat - 20.0).abs() < 1e-9);
    }

    #[test]
    fn the_nordic_box_needs_eleven_chunks() {
        let g = nordic();
        let bytes = nordic_fixture();
        let file = File::from_source(ReadSeekSource::new(Cursor::new(bytes)).unwrap()).unwrap();
        let where_ = attrs(&file, "/where").unwrap();
        let (source, _) = Source::read(&where_).unwrap();
        // UL_* and LR_* are the outer corners of the corner pixels.
        assert!((source.x0 - g.ul_x).abs() < 1e-3 && (source.y0 - g.ul_y).abs() < 1e-3);
        let (x1, y1) = source.proj.forward(
            num(&where_, "LR_lon").unwrap(),
            num(&where_, "LR_lat").unwrap(),
        );
        assert!((x1 - g.lr_x).abs() < 1e-3 && (y1 - g.lr_y).abs() < 1e-3);
        let mut needed = std::collections::BTreeSet::new();
        Target::over(g.crop()).each_texel(&source, |pixel| {
            if let Some((i, j)) = pixel {
                needed.insert((j / g.chunk_rows, i / g.chunk_cols));
            }
        });
        let expected: Vec<(usize, usize)> =
            g.needed_chunks.iter().map(|c| (c.row, c.col)).collect();
        assert_eq!(needed.into_iter().collect::<Vec<_>>(), expected);
        assert_eq!(expected.len(), 11);
    }

    #[test]
    fn the_needed_chunks_decode_like_h5py() {
        let g = nordic();
        let reader = Shared::new(Cursor::new(nordic_fixture()));
        let file = File::from_source(ReadSeekSource::new(reader.clone()).unwrap()).unwrap();
        let (source, _) = Source::read(&attrs(&file, "/where").unwrap()).unwrap();
        let dataset = file.dataset("/dataset1/data1/data").unwrap();
        let coding = Coding {
            gain: 1.0,
            offset: 0.0,
            nodata: -9_999_000.0,
            undetect: -8_888_000.0,
        };
        let codes = read_codes(&dataset, &reader, &source, &Target::over(g.crop()), coding)
            .unwrap();
        let mut checked = 0;
        for c in g.allocated_chunks.iter().filter(|c| c.needed) {
            let theirs = c.counts.as_ref().unwrap();
            let mut ours = (0, 0, 0);
            for r in c.row * g.chunk_rows..((c.row + 1) * g.chunk_rows).min(source.ysize) {
                let line = &codes[r * source.xsize..][..source.xsize];
                let cells = &line[c.col * g.chunk_cols..((c.col + 1) * g.chunk_cols).min(source.xsize)];
                let (m, u, n) = counts(cells);
                ours = (ours.0 + m, ours.1 + u, ours.2 + n);
            }
            assert_eq!(
                ours,
                (theirs.measured, theirs.undetect, theirs.nodata),
                "chunk {}, {}",
                c.row,
                c.col
            );
            checked += 1;
        }
        assert_eq!(checked, 5, "the fixture keeps five of the eleven needed chunks");
    }

    #[test]
    fn the_nordic_crop_reprojects_like_pyproj() {
        let g = nordic();
        let grid = decode_box(Cursor::new(nordic_fixture()), Some(g.crop())).unwrap();
        assert_eq!((grid.width, grid.height), (g.width, g.height));
        assert_eq!((grid.width, grid.height), (1670, 2297));
        for (ours, theirs, edge) in [
            (grid.west, g.west, "west"),
            (grid.east, g.east, "east"),
            (grid.north, g.north, "north"),
            (grid.south, g.south, "south"),
        ] {
            assert!((ours - theirs).abs() < 1e-8, "{edge}: {ours} vs {theirs}");
        }
        assert_eq!(grid.source_projdef, g.projdef);
        assert_eq!(
            (grid.start_ms, grid.end_ms),
            (utc(&g.date, &g.time), utc(&g.end_date, &g.end_time))
        );
        for s in &g.samples {
            let code = grid.codes[(s.row * grid.width + s.col) as usize];
            assert_eq!(code, s.code, "texel {}, {} ({}, {})", s.col, s.row, s.lon, s.lat);
        }
        let (measured, undetect, nodata) = counts(&grid.codes);
        for (ours, theirs, what) in [
            (measured, g.counts.measured, "measured"),
            (undetect, g.counts.undetect, "undetect"),
            (nodata, g.counts.nodata, "nodata or outside"),
        ] {
            assert!(
                ours.abs_diff(theirs) <= g.near_edge,
                "{what}: {ours} vs {theirs}"
            );
        }
    }

    /// The fixture served in ranges, recording every range asked for.
    struct Recording(Vec<u8>, Arc<Mutex<Vec<(u64, u64)>>>);
    impl RangeSource for Recording {
        fn get(&mut self, offset: u64, len: u64) -> io::Result<(Vec<u8>, Option<u64>)> {
            self.1.lock().unwrap().push((offset, len));
            let start = (offset as usize).min(self.0.len());
            let end = (start + len as usize).min(self.0.len());
            Ok((self.0[start..end].to_vec(), Some(self.0.len() as u64)))
        }
    }

    #[test]
    fn only_the_needed_chunks_are_fetched() {
        let g = nordic();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let reader = RangeReader::open(Box::new(Recording(nordic_fixture(), asked.clone()))).unwrap();
        let traffic = reader.traffic();
        let grid = decode_box(reader, Some(g.crop())).unwrap();
        assert_eq!((grid.width, grid.height), (g.width, g.height));
        let asked = asked.lock().unwrap();
        let unneeded: Vec<&Allocated> = g.allocated_chunks.iter().filter(|c| !c.needed).collect();
        assert!(!unneeded.is_empty(), "the fixture keeps an unneeded chunk to test this");
        for c in unneeded {
            // At most the block it shares with a stored neighbour.
            let overlap: u64 = asked
                .iter()
                .map(|&(offset, len)| {
                    (offset + len).min(c.address + c.size).saturating_sub(offset.max(c.address))
                })
                .sum();
            assert!(
                overlap <= smhi_live::BLOCK && c.size > smhi_live::BLOCK,
                "chunk ({}, {}): {overlap} of its {} bytes fetched by {asked:?}",
                c.row,
                c.col,
                c.size
            );
        }
        assert!(
            traffic.requests() <= 12,
            "{} requests, {} bytes of {}: {asked:?}",
            traffic.requests(),
            traffic.bytes(),
            traffic.total()
        );
    }
}
