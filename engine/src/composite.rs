//! SMHI's national composite (`area/sweden/product/comp`; stream S8,
//! DEC-11). One ODIM `COMP` file every 5 minutes covers every Swedish
//! radar. The engine decodes it and reprojects it to a Web Mercator grid
//! texture, so both UIs draw it as one textured rectangle
//! (`docs/protocol.md`, grid texture).
//!
//! - The DBZH layer (the `/dataset1/dataN` whose `what/quantity` is `DBZH`)
//!   is read as bytes and requantized to the polar convention. Undetect
//!   becomes 0 and nodata 1; a measured value becomes `round(dBZ × 2 + 66)`
//!   clamped to 2..255. The palette bounds and the weak-return floor
//!   therefore work as they do for a sweep.
//! - `/where` describes a polar stereographic grid: `projdef`, and `xsize` ×
//!   `ysize` pixels of `xscale` × `yscale` metres, row 0 north.
//!   `UL_lon`/`UL_lat` is the outer corner of the upper-left pixel.
//! - The texture covers the lon/lat box of the grid's outer boundary with
//!   `PIXEL_M` Mercator pixels, counted from the box's west and north edges.
//!   Each texel holds the source pixel that contains its centre, and texels
//!   off the grid are nodata.
//!
//! Only the polar `+proj=stere` form SMHI uses is understood: north pole,
//! true scale at `lat_ts`, and an ellipsoid given by name or by `+a` with
//! `+rf` or `+b`. Anything else is an error, which is better than a wrong
//! picture. SMHI's `+towgs84=0,0,0` is read the way PROJ reads it: WGS84
//! latitudes are carried onto the Bessel ellipsoid through geocentric
//! coordinates before projecting (about 43 m at 70°N).

use crate::protocol::{Frame, FrameKind, FrameStatus, Geometry, GridPlacement, SiteKind, Station};
use crate::smhi_live::{RangeReader, Scan};
use crate::sweep::{BELOW_THRESHOLD, OUTSIDE_COVERAGE, png};
use chrono::{DateTime, NaiveDateTime};
use hdf5_pure::{AttrValue, File, ReadSeekSource};
use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};
use std::fmt;
use std::io::{self, Read, Seek};

/// SMHI's area key for the composite, which is also its station id.
pub const AREA: &str = "sweden";
/// SMHI's product name for the composite.
pub const PRODUCT: &str = "comp";
/// Texel size in Web Mercator metres. At 54°N (the south edge) this is
/// 1.2 km of ground, finer than the 2 km source, so nearest sampling
/// skips no source pixel. The texture stays under 2048 px on each side,
/// the smallest WebGL2 must support.
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

impl Stereographic {
    fn parse(projdef: &str) -> Result<Stereographic, CompositeError> {
        let params: HashMap<&str, &str> = projdef
            .split_whitespace()
            .map(|token| {
                let token = token.strip_prefix('+').unwrap_or(token);
                token.split_once('=').unwrap_or((token, ""))
            })
            .collect();
        let bad = |why: &str| fail(format!("projdef {projdef:?}: {why}"));
        let number = |key: &str| -> Result<Option<f64>, CompositeError> {
            params
                .get(key)
                .map(|v| {
                    v.parse::<f64>()
                        .map_err(|_| bad(&format!("+{key} is not a number")))
                })
                .transpose()
        };
        if params.get("proj") != Some(&"stere") {
            return Err(bad("only +proj=stere is supported"));
        }
        if number("lat_0")?.is_none_or(|lat0| (lat0 - 90.0).abs() > 1e-9) {
            return Err(bad("only the north polar aspect (+lat_0=90) is supported"));
        }
        if number("k_0")?
            .or(number("k")?)
            .is_some_and(|k| (k - 1.0).abs() > 1e-12)
        {
            return Err(bad("a scale factor other than 1 is not supported"));
        }
        if number("x_0")?.is_some_and(|v| v != 0.0) || number("y_0")?.is_some_and(|v| v != 0.0) {
            return Err(bad("false easting or northing is not supported"));
        }
        if let Some(shift) = params.get("towgs84")
            && shift.split(',').any(|v| v.trim().parse::<f64>() != Ok(0.0))
        {
            return Err(bad("a datum shift is not supported"));
        }
        let (a, inverse_flattening) = match (params.get("ellps"), number("a")?) {
            (Some(&"bessel"), _) => (6_377_397.155, 299.152_812_8),
            (Some(&"WGS84"), _) => (6_378_137.0, 298.257_223_563),
            (Some(&"GRS80"), _) => (6_378_137.0, 298.257_222_101),
            (Some(other), _) => return Err(bad(&format!("unknown +ellps={other}"))),
            (None, Some(a)) => match (number("rf")?, number("b")?) {
                (Some(rf), _) => (a, rf),
                (None, Some(b)) if b < a => (a, a / (a - b)),
                (None, _) => (a, f64::INFINITY),
            },
            (None, None) => return Err(bad("no ellipsoid (+ellps, or +a with +rf or +b)")),
        };
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
            datum: (params.contains_key("towgs84") && !is_wgs84).then_some((a, e * e)),
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
fn requantize(raw: u8, gain: f64, offset: f64, nodata: f64, undetect: f64) -> u8 {
    let value = f64::from(raw);
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

/// The source grid as `/where` describes it.
struct Source {
    stereo: Stereographic,
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
        let stereo = Stereographic::parse(&projdef)?;
        let xsize = need(where_, path, "xsize")? as usize;
        let ysize = need(where_, path, "ysize")? as usize;
        let (xscale, yscale) = (need(where_, path, "xscale")?, need(where_, path, "yscale")?);
        if xsize == 0 || ysize == 0 || xscale <= 0.0 || yscale <= 0.0 {
            return Err(fail(format!(
                "/where: {xsize} × {ysize} pixels of {xscale} × {yscale} m"
            )));
        }
        let corner = |name: &str| -> Result<(f64, f64), CompositeError> {
            Ok(stereo.forward(
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
                stereo,
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
    /// pixel edge: (west, east, south, north).
    fn bounds(&self) -> (f64, f64, f64, f64) {
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
        across.chain(down).fold(
            (
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ),
            |(w, e, s, n), (x, y)| {
                let (lon, lat) = self.stereo.inverse(x, y);
                (w.min(lon), e.max(lon), s.min(lat), n.max(lat))
            },
        )
    }
}

/// Decode a composite and reproject it to its Web Mercator texture.
/// `reader` may be a file, a buffer, or a ranged HTTP reader; only the
/// metadata and the DBZH layer are read.
pub fn decode<R: Read + Seek + Send + 'static>(reader: R) -> Result<Grid, CompositeError> {
    let source =
        ReadSeekSource::new(reader).map_err(|e| fail(format!("reading the composite: {e}")))?;
    let file = File::from_source(source)
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
    let (gain, offset) = (coding("gain")?, coding("offset")?);
    let (nodata, undetect) = (coding("nodata")?, coding("undetect")?);
    let start_ms = nominal_ms(&what, "date", "time")
        .or_else(|| nominal_ms(&dwhat, "startdate", "starttime"))
        .ok_or_else(|| fail("/what: no date and time"))?;
    let end_ms = nominal_ms(&dwhat, "enddate", "endtime").unwrap_or(start_ms);
    let elevation_deg = num(&dwhat, "prodpar").unwrap_or(0.0);

    let values = file
        .dataset(&format!("{data}/data"))
        .and_then(|d| d.read_u8())
        .map_err(|e| fail(format!("{data}/data: {e}")))?;
    if values.len() != source.xsize * source.ysize {
        return Err(fail(format!(
            "{data}/data holds {} values, not {} × {}",
            values.len(),
            source.ysize,
            source.xsize
        )));
    }
    let source_codes: Vec<u8> = values
        .iter()
        .map(|&v| requantize(v, gain, offset, nodata, undetect))
        .collect();

    Ok(reproject(
        &source,
        &source_codes,
        projdef,
        start_ms,
        end_ms,
        elevation_deg,
    ))
}

/// The Web Mercator texture over `source`: whole `PIXEL_M` texels from the
/// box's west and north edges, each holding the source pixel under its
/// centre. Longitude depends only on the column and latitude only on the
/// row, so the projection splits into a per-row radius and a per-column
/// angle.
fn reproject(
    source: &Source,
    codes: &[u8],
    source_projdef: String,
    start_ms: i64,
    end_ms: i64,
    elevation_deg: f64,
) -> Grid {
    let (west, east, south, north) = source.bounds();
    let (mx_west, my_north) = (MERCATOR_R * west.to_radians(), mercator_y(north));
    let width = ((MERCATOR_R * east.to_radians() - mx_west) / PIXEL_M).ceil() as u32;
    let height = ((my_north - mercator_y(south)) / PIXEL_M).ceil() as u32;
    let angles: Vec<(f64, f64)> = (0..width)
        .map(|c| {
            let lon = mx_west + (f64::from(c) + 0.5) * PIXEL_M;
            (lon / MERCATOR_R - source.stereo.lon0).sin_cos()
        })
        .collect();
    let mut out = Vec::with_capacity(width as usize * height as usize);
    for r in 0..height {
        let lat = mercator_lat(my_north - (f64::from(r) + 0.5) * PIXEL_M);
        let rho = source.stereo.rho(lat);
        out.extend(angles.iter().map(|&(sin, cos)| {
            let i = ((rho * sin - source.x0) / source.xscale).floor();
            let j = ((source.y0 + rho * cos) / source.yscale).floor();
            if i >= 0.0 && j >= 0.0 && (i as usize) < source.xsize && (j as usize) < source.ysize {
                codes[j as usize * source.xsize + i as usize]
            } else {
                1
            }
        }));
    }
    Grid {
        width,
        height,
        codes: out,
        start_ms,
        end_ms,
        elevation_deg,
        west,
        east: (mx_west + f64::from(width) * PIXEL_M) / MERCATOR_R * 180.0 / std::f64::consts::PI,
        north,
        south: mercator_lat(my_north - f64::from(height) * PIXEL_M),
        source_projdef,
    }
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
        let (x1, y1) = source.stereo.forward(
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
            .map(|&v| requantize(v, 0.4, -30.0, 255.0, 0.0))
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
        assert_eq!(requantize(0, 0.4, -30.0, 255.0, 0.0), 0);
        assert_eq!(requantize(255, 0.4, -30.0, 255.0, 0.0), 1);
        // 1 → -29.6 dBZ → 6.8 → 7; 75 → 0 dBZ → 66; 254 → 71.6 dBZ → 209.
        assert_eq!(requantize(1, 0.4, -30.0, 255.0, 0.0), 7);
        assert_eq!(requantize(75, 0.4, -30.0, 255.0, 0.0), 66);
        assert_eq!(requantize(254, 0.4, -30.0, 255.0, 0.0), 209);
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
}
