//! The MET Nordic grid under the stations (S43, docs/protocol.md "The MET
//! Nordic grid"): MET Norway's hourly 1 km analysis of 2 m temperature and
//! 10 m wind, read over OPeNDAP as a strided subset (never the whole file),
//! the temperature reprojected to a Web Mercator PNG under `tex/` on the
//! stations' colour scale, the wind as a point grid.
//!
//! The fetch rule (M1, as for the stations): nothing while no client has a
//! grid layer on; then a probe of `time` alone (about 100 bytes) when the
//! next hour is due, and one subset download per new hour. The subset is
//! cached beside the stations' bodies, so a restart re-renders it without a
//! request.

use super::{Layers, cache_dir, iso, now_ms, stamp, write_atomic};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, mpsc::Sender};

/// The newest analysis; `metpplatest` keeps about three days of hourly
/// files beside it.
const URL: &str =
    "https://thredds.met.no/thredds/dodsC/metpplatest/met_analysis_1_0km_nordic_latest.nc.dods";
/// Every third point of the 1 km grid for temperature, every 24th for wind
/// (a multiple of the temperature stride, so both share `x` and `y`).
pub const TEMP_STRIDE: usize = 3;
pub const WIND_STRIDE: usize = 24;
/// The source grid's size, points (x, y).
const NX: usize = 1796;
const NY: usize = 2321;
/// The Mercator image's width, pixels; its height follows the extent.
pub const IMAGE_WIDTH: u32 = 1200;
/// The analysis for HH:00 appears about HH:15: an hour in hand, the next is
/// probed from 20 minutes past the following hour, then every 10 minutes.
const NEXT_AFTER_MS: i64 = 80 * 60 * 1000;
const RETRY_MS: i64 = 10 * 60 * 1000;
/// A grid older than this is dropped rather than shown.
pub const STALE_MS: i64 = 3 * 60 * 60 * 1000;
const CACHE_NAME: &str = "metnordic.dods";
const TICK: Duration = Duration::from_secs(60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_BODY: usize = 8 << 20;

/// The subset's query: `x`, `y` and temperature every `TEMP_STRIDE`, wind
/// every `WIND_STRIDE` (brackets escaped; THREDDS refuses them bare).
pub fn subset_url() -> String {
    let t = TEMP_STRIDE;
    let w = WIND_STRIDE;
    let (xl, yl) = (NX - 1, NY - 1);
    let part =
        |name: &str, s: usize| format!("{name}.{name}%5B0%5D%5B0:{s}:{yl}%5D%5B0:{s}:{xl}%5D");
    format!(
        "{URL}?time,x%5B0:{t}:{xl}%5D,y%5B0:{t}:{yl}%5D,{},{},{}",
        part("air_temperature_2m", t),
        part("wind_speed_10m", w),
        part("wind_direction_10m", w)
    )
}
pub fn probe_url() -> String {
    format!("{URL}?time")
}

// ---- OPeNDAP (DAP2) binary answers -----------------------------------------

/// One variable of a `.dods` answer: its dimensions and values.
#[derive(Debug, Clone)]
pub struct Var {
    pub dims: Vec<usize>,
    pub values: Vec<f64>,
}

/// The variables of a DAP2 `.dods` answer by name (a structure's member by
/// its own name; the first of two with one name wins). The text DDS says
/// what follows `Data:`, in order: arrays as two big-endian counts and the
/// values, scalars as the value alone. 16-bit integers travel as 32.
pub fn parse_dods(bytes: &[u8]) -> Result<HashMap<String, Var>, String> {
    const MARK: &[u8] = b"\nData:\n";
    let at = bytes
        .windows(MARK.len())
        .position(|w| w == MARK)
        .ok_or("not a DAP2 answer (no Data: mark)")?;
    let dds = std::str::from_utf8(&bytes[..at]).map_err(|_| "the DDS is not UTF-8")?;
    let mut data = &bytes[at + MARK.len()..];
    let mut out = HashMap::new();
    for decl in dds.lines().map(str::trim) {
        let Some((kind, rest)) = decl.split_once(' ') else {
            continue;
        };
        let size = match kind {
            "Float64" => 8,
            "Float32" | "Int32" | "UInt32" | "Int16" | "UInt16" => 4,
            "Dataset" | "Structure" | "Grid" | "}" | "ARRAY:" | "MAPS:" => continue,
            other if other.starts_with('}') => continue,
            other => return Err(format!("unsupported DAP2 type {other}")),
        };
        let rest = rest.trim_end_matches(';');
        let (name, shape) = rest.split_once('[').map_or((rest, ""), |(n, s)| (n, s));
        let mut dims = Vec::new();
        for dim in shape.split('[') {
            let n = dim
                .trim_end_matches(']')
                .rsplit_once('=')
                .and_then(|(_, n)| n.trim().parse::<usize>().ok())
                .ok_or_else(|| format!("a bad dimension in {decl:?}"))?;
            dims.push(n);
        }
        let dims = if shape.is_empty() { Vec::new() } else { dims };
        let count: usize = dims.iter().product();
        if !dims.is_empty() {
            if data.len() < 8 {
                return Err(format!("{name}: truncated"));
            }
            let n = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
            if n != count {
                return Err(format!("{name}: {n} values, the DDS says {count}"));
            }
            data = &data[8..];
        }
        if data.len() < count * size {
            return Err(format!("{name}: truncated"));
        }
        let values = data[..count * size]
            .chunks_exact(size)
            .map(|c| match kind {
                "Float64" => f64::from_be_bytes(c.try_into().unwrap()),
                "Float32" => f32::from_be_bytes(c.try_into().unwrap()) as f64,
                "UInt32" | "UInt16" => u32::from_be_bytes(c.try_into().unwrap()) as f64,
                _ => i32::from_be_bytes(c.try_into().unwrap()) as f64,
            })
            .collect();
        data = &data[count * size..];
        out.entry(name.to_owned()).or_insert(Var { dims, values });
    }
    Ok(out)
}

/// The valid time of a `.dods` answer holding `time`, ms since the epoch.
pub fn time_ms(vars: &HashMap<String, Var>) -> Result<i64, String> {
    let seconds = vars
        .get("time")
        .and_then(|v| v.values.first().copied())
        .filter(|s| s.is_finite() && *s > 0.0)
        .ok_or("no time")?;
    Ok((seconds * 1000.0).round() as i64)
}

// ---- The projection ---------------------------------------------------------

/// MET Nordic's Lambert conformal conic on a sphere: `+proj=lcc +lat_0=63
/// +lon_0=15 +lat_1=63 +lat_2=63 +R=6371000`, metres.
pub struct Lcc {
    n: f64,
    rf: f64,
    rho0: f64,
    lon0: f64,
}

impl Default for Lcc {
    fn default() -> Self {
        let r = 6_371_000.0;
        let lat1 = 63f64.to_radians();
        let n = lat1.sin();
        let t = |lat: f64| (std::f64::consts::FRAC_PI_4 + lat / 2.0).tan();
        let f = lat1.cos() * t(lat1).powf(n) / n;
        Lcc {
            n,
            rf: r * f,
            rho0: r * f / t(lat1).powf(n),
            lon0: 15f64.to_radians(),
        }
    }
}

impl Lcc {
    pub fn forward(&self, lat: f64, lon: f64) -> (f64, f64) {
        let rho = self.rf
            / (std::f64::consts::FRAC_PI_4 + lat.to_radians() / 2.0)
                .tan()
                .powf(self.n);
        let theta = self.n * (lon.to_radians() - self.lon0);
        (rho * theta.sin(), self.rho0 - rho * theta.cos())
    }
    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let dy = self.rho0 - y;
        let rho = (x * x + dy * dy).sqrt();
        let theta = x.atan2(dy);
        let lat = 2.0 * (self.rf / rho).powf(1.0 / self.n).atan() - std::f64::consts::FRAC_PI_2;
        (lat.to_degrees(), (self.lon0 + theta / self.n).to_degrees())
    }
}

// ---- The field --------------------------------------------------------------

/// A regular grid of one variable in the projection: `values[row * nx +
/// col]`, row 0 the southernmost, at `x0 + col * step`, `y0 + row * step`.
#[derive(Debug, Clone)]
pub struct Plane {
    pub nx: usize,
    pub ny: usize,
    pub x0: f64,
    pub y0: f64,
    pub step: f64,
    pub values: Vec<f64>,
}

impl Plane {
    /// Bilinear value at projection point (x, y), or `None` outside.
    pub fn at(&self, x: f64, y: f64) -> Option<f64> {
        let fx = (x - self.x0) / self.step;
        let fy = (y - self.y0) / self.step;
        if !(fx >= 0.0 && fy >= 0.0) {
            return None;
        }
        let (c, r) = (fx.floor() as usize, fy.floor() as usize);
        if c >= self.nx || r >= self.ny {
            return None;
        }
        let (c1, r1) = ((c + 1).min(self.nx - 1), (r + 1).min(self.ny - 1));
        if (c == self.nx - 1 && fx > c as f64) || (r == self.ny - 1 && fy > r as f64) {
            return None;
        }
        let (tx, ty) = (fx - c as f64, fy - r as f64);
        let v = |r: usize, c: usize| self.values[r * self.nx + c];
        let (a, b, cc, d) = (v(r, c), v(r, c1), v(r1, c), v(r1, c1));
        if ![a, b, cc, d].iter().all(|v| v.is_finite()) {
            return None;
        }
        Some((a * (1.0 - tx) + b * tx) * (1.0 - ty) + (cc * (1.0 - tx) + d * tx) * ty)
    }
}

/// One analysis hour: temperature (°C) and wind (speed, from-direction).
#[derive(Debug, Clone)]
pub struct Field {
    pub time_ms: i64,
    pub temp_c: Plane,
    pub wind_ms: Plane,
    pub wind_dir: Plane,
}

/// A value MET Nordic would never write (fill values, NaN).
fn sane(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_finite() && (lo..=hi).contains(&v) {
        v
    } else {
        f64::NAN
    }
}

impl Field {
    /// The field of a subset answer fetched at strides `temp` and `wind`.
    pub fn from_dods(bytes: &[u8], temp: usize, wind: usize) -> Result<Field, String> {
        let vars = parse_dods(bytes)?;
        let time_ms = time_ms(&vars)?;
        let get = |name: &str| vars.get(name).ok_or_else(|| format!("no {name}"));
        let (x, y) = (get("x")?, get("y")?);
        if x.values.len() < 2 || y.values.len() < 2 || !wind.is_multiple_of(temp) {
            return Err("x and y need two points each".into());
        }
        let step = (x.values[1] - x.values[0]) / temp as f64;
        if step.is_nan()
            || step <= 0.0
            || ((y.values[1] - y.values[0]) / temp as f64 - step).abs() > 1.0
        {
            return Err("x and y are not one regular grid".into());
        }
        let (x0, y0) = (x.values[0], y.values[0]);
        let plane = |name: &str, stride: usize, f: &dyn Fn(f64) -> f64| -> Result<Plane, String> {
            let var = get(name)?;
            let (ny, nx) = match var.dims.as_slice() {
                [1, ny, nx] | [ny, nx] => (*ny, *nx),
                _ => return Err(format!("{name}: unexpected shape {:?}", var.dims)),
            };
            Ok(Plane {
                nx,
                ny,
                x0,
                y0,
                step: step * stride as f64,
                values: var.values.iter().map(|&v| f(v)).collect(),
            })
        };
        let temp_c = plane("air_temperature_2m", temp, &|k| {
            sane(k, 180.0, 340.0) - 273.15
        })?;
        if temp_c.nx != x.values.len() || temp_c.ny != y.values.len() {
            return Err("temperature and x/y differ in size".into());
        }
        // Review S1: a field with no usable value (fill values, or a packed
        // variable read raw) is a failure, not an empty map.
        if !temp_c.values.iter().any(|v| v.is_finite()) {
            return Err("no values (packed?)".into());
        }
        Ok(Field {
            time_ms,
            temp_c,
            wind_ms: plane("wind_speed_10m", wind, &|v| sane(v, 0.0, 150.0))?,
            wind_dir: plane("wind_direction_10m", wind, &|v| sane(v, 0.0, 360.0))?,
        })
    }

    /// `[west, south, east, north]` of the temperature plane, degrees.
    pub fn bounds(&self, lcc: &Lcc) -> [f64; 4] {
        let p = &self.temp_c;
        let (x1, y1) = (
            p.x0 + (p.nx - 1) as f64 * p.step,
            p.y0 + (p.ny - 1) as f64 * p.step,
        );
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        let steps = 200;
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            let (xs, ys) = (p.x0 + (x1 - p.x0) * t, p.y0 + (y1 - p.y0) * t);
            for (x, y) in [(xs, p.y0), (xs, y1), (p.x0, ys), (x1, ys)] {
                let (lat, lon) = lcc.inverse(x, y);
                b = [b[0].min(lon), b[1].min(lat), b[2].max(lon), b[3].max(lat)];
            }
        }
        b.map(|v| (v * 100.0).round() / 100.0)
    }
}

// ---- Drawing ----------------------------------------------------------------

/// The stations' scale (ObsLayer.qml, app.js OBS_SCALE), °C → sRGB.
const SCALE: [(f64, [u8; 3]); 10] = [
    (-30.0, [0x6a, 0x3d, 0x9a]),
    (-20.0, [0x3f, 0x5f, 0xbf]),
    (-10.0, [0x3f, 0x94, 0xd6]),
    (-2.0, [0x8f, 0xd0, 0xee]),
    (2.0, [0xb8, 0xe3, 0xa8]),
    (8.0, [0xe9, 0xe9, 0x8a]),
    (14.0, [0xf7, 0xc9, 0x5c]),
    (20.0, [0xf3, 0x9a, 0x45]),
    (26.0, [0xe5, 0x60, 0x3a]),
    (32.0, [0xc2, 0x23, 0x3a]),
];

pub fn color(c: f64) -> [u8; 3] {
    if c <= SCALE[0].0 {
        return SCALE[0].1;
    }
    for pair in SCALE.windows(2) {
        let ((t0, a), (t1, b)) = (pair[0], pair[1]);
        if c <= t1 {
            let t = (c - t0) / (t1 - t0);
            return [0, 1, 2]
                .map(|i| (a[i] as f64 + (b[i] as f64 - a[i] as f64) * t).round() as u8);
        }
    }
    SCALE[SCALE.len() - 1].1
}

fn mercator_y(lat: f64) -> f64 {
    (std::f64::consts::FRAC_PI_4 + lat.to_radians() / 2.0)
        .tan()
        .ln()
}

/// The temperature as a Web Mercator RGBA image over `bounds`: columns
/// linear in longitude, rows linear in Mercator y, north at the top.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub min_c: f64,
    pub max_c: f64,
}

pub fn render(field: &Field, lcc: &Lcc, bounds: [f64; 4], width: u32) -> Image {
    let [west, south, east, north] = bounds;
    let (top, bottom) = (mercator_y(north), mercator_y(south));
    let span = (east - west).to_radians();
    let height = ((width as f64) * (top - bottom) / span).round().max(1.0) as u32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let (mut min_c, mut max_c) = (f64::MAX, f64::MIN);
    for row in 0..height {
        let my = top - (row as f64 + 0.5) / height as f64 * (top - bottom);
        let lat = (2.0 * my.exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees();
        for col in 0..width {
            let lon = west + (col as f64 + 0.5) / width as f64 * (east - west);
            let (x, y) = lcc.forward(lat, lon);
            let Some(c) = field.temp_c.at(x, y) else {
                continue;
            };
            min_c = min_c.min(c);
            max_c = max_c.max(c);
            let at = ((row * width + col) * 4) as usize;
            let [r, g, b] = color(c);
            rgba[at..at + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
    if min_c > max_c {
        (min_c, max_c) = (f64::NAN, f64::NAN);
    }
    Image {
        width,
        height,
        rgba,
        min_c,
        max_c,
    }
}

pub fn png(image: &Image) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    let mut writer = encoder.write_header().map_err(io::Error::other)?;
    writer
        .write_image_data(&image.rgba)
        .map_err(io::Error::other)?;
    writer.finish().map_err(io::Error::other)?;
    Ok(out)
}

/// The wind points as JSON rows `[lat, lon, m/s, from °]`, row by row
/// from the south-west corner (`cols` to a row), every point of the wind
/// plane; a point without both values has `null` for them, so a point's
/// place in the list is its place in the grid.
pub fn wind_points(field: &Field, lcc: &Lcc) -> String {
    let (s, d) = (&field.wind_ms, &field.wind_dir);
    let mut out = String::from("[");
    for row in 0..s.ny {
        for col in 0..s.nx {
            let at = row * s.nx + col;
            let (ms, deg) = (s.values[at], d.values.get(at).copied().unwrap_or(f64::NAN));
            let (lat, lon) = lcc.inverse(s.x0 + col as f64 * s.step, s.y0 + row as f64 * s.step);
            if out.len() > 1 {
                out.push(',');
            }
            if ms.is_finite() && deg.is_finite() {
                let _ = write!(
                    out,
                    "[{lat:.2},{lon:.2},{ms:.1},{}]",
                    (deg.round() as i64).rem_euclid(360)
                );
            } else {
                let _ = write!(out, "[{lat:.2},{lon:.2},null,null]");
            }
        }
    }
    out.push(']');
    out
}

/// FNV-1a, for a texture name that changes with its content.
fn tag(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{:08x}", (h ^ (h >> 32)) as u32)
}

/// A drawn hour: what goes on the wire, and the texture it names.
#[derive(Debug, Clone)]
pub struct Drawn {
    pub time_ms: i64,
    /// `tex/grid-temp-<hour>-<tag>.png`.
    pub texture: String,
    pub temperature: String,
    pub wind: String,
    pub png: Vec<u8>,
}

/// Review N8: one hour drawn is the same when its time and texture name
/// (which follows the PNG's content) are, without comparing the PNG.
impl PartialEq for Drawn {
    fn eq(&self, other: &Self) -> bool {
        self.time_ms == other.time_ms && self.texture == other.texture
    }
}

/// Draw `field`: the PNG's bytes and name, the `temperature` and `wind`
/// objects of the `obs` line.
pub fn draw(field: &Field) -> io::Result<Drawn> {
    let lcc = Lcc::default();
    let bounds = field.bounds(&lcc);
    let image = render(field, &lcc, bounds, IMAGE_WIDTH);
    let png = png(&image)?;
    let hour = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(field.time_ms)
        .unwrap_or_default()
        .format("%Y%m%dT%HZ");
    let texture = format!("tex/grid-temp-{hour}-{}.png", tag(&png));
    let round = |v: f64| {
        if v.is_finite() {
            (v * 10.0).round() / 10.0
        } else {
            0.0
        }
    };
    let temperature = serde_json::json!({
        "texture": texture,
        "bounds": bounds,
        "width": image.width,
        "height": image.height,
        "minC": round(image.min_c),
        "maxC": round(image.max_c),
    })
    .to_string();
    let wind = format!(
        "{{\"spacingKm\":{},\"cols\":{},\"points\":{}}}",
        (field.wind_ms.step / 1000.0).round(),
        field.wind_ms.nx,
        wind_points(field, &lcc)
    );
    Ok(Drawn {
        time_ms: field.time_ms,
        texture,
        temperature,
        wind,
        png,
    })
}

// ---- The hub ----------------------------------------------------------------

/// What the grid layer holds: the newest drawn hour, and how the last
/// fetch went.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Held {
    pub drawn: Option<Arc<Drawn>>,
    /// `ok` or `failed`.
    pub status: &'static str,
    pub note: String,
}

/// The `obs` line with `source` `grid` for a client showing `layers`.
pub fn line(held: &Held, layers: Layers) -> String {
    let mut out = format!(
        "{{\"type\":\"obs\",\"v\":{},\"source\":\"grid\",\"time\":\"{}\"",
        crate::protocol::VERSION,
        held.drawn
            .as_ref()
            .map_or(String::new(), |d| iso(d.time_ms))
    );
    if let Some(drawn) = &held.drawn {
        if layers.temp {
            let _ = write!(out, ",\"temperature\":{}", drawn.temperature);
        }
        if layers.wind {
            let _ = write!(out, ",\"wind\":{}", drawn.wind);
        }
    }
    let mut provider = serde_json::json!({
        "id": "metnordic",
        "name": "MET Norway",
        "status": if held.status.is_empty() { "ok" } else { held.status },
    });
    if !held.note.is_empty() {
        provider["note"] = held.note.clone().into();
    }
    let _ = write!(
        out,
        ",\"provider\":{provider},\"attribution\":\"{}\"}}",
        if held.drawn.is_some() {
            "MET Norway (CC BY 4.0)"
        } else {
            ""
        }
    );
    out.push('\n');
    out
}

pub static HUB: LazyLock<Hub> = LazyLock::new(Hub::default);

/// The clients showing a grid layer and what the grid holds; separate from
/// the stations' hub so a client may show either, or both.
#[derive(Default)]
pub struct Hub {
    inner: Mutex<HubInner>,
    wake: Notify,
}

#[derive(Default)]
struct HubInner {
    clients: HashMap<u64, (Layers, Sender<String>)>,
    held: Option<Held>,
}

impl Hub {
    /// A client's grid layers (both off: it shows no grid). A change is
    /// answered at once with what the grid holds; the first client wakes
    /// the fetcher.
    pub fn set(&self, client: u64, layers: Layers, reply: &Sender<String>) {
        let mut inner = self.inner.lock().unwrap();
        let before = inner
            .clients
            .get(&client)
            .map(|(l, _)| *l)
            .unwrap_or_default();
        if !layers.any() {
            inner.clients.remove(&client);
            return;
        }
        inner.clients.insert(client, (layers, reply.clone()));
        if before != layers
            && let Some(held) = &inner.held
        {
            let _ = reply.try_send(line(held, layers));
        }
        if !before.any() {
            self.wake.notify_one();
        }
    }
    pub fn remove(&self, client: u64) {
        self.inner.lock().unwrap().clients.remove(&client);
    }
    pub fn wanted(&self) -> bool {
        !self.inner.lock().unwrap().clients.is_empty()
    }
    /// The texture the grid holds, for the texture cleanup.
    pub fn texture(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        inner
            .held
            .as_ref()?
            .drawn
            .as_ref()
            .map(|d| d.texture.clone())
    }
    /// Send `held` to every client with a grid layer on when it changed.
    fn publish(&self, held: Held) {
        let mut inner = self.inner.lock().unwrap();
        if inner.held.as_ref() == Some(&held) {
            return;
        }
        // Review S2: the same hour with only its status or note changed is
        // kept, not sent again (the line is about 190 kB).
        if inner
            .held
            .as_ref()
            .is_some_and(|h| h.drawn.is_some() && h.drawn == held.drawn)
        {
            inner.held = Some(held);
            return;
        }
        for (layers, tx) in inner.clients.values() {
            let _ = tx.try_send(line(&held, *layers));
        }
        inner.held = Some(held);
    }
}

// ---- Fetching ---------------------------------------------------------------

/// When the next probe is due, having `held` the hour at `time_ms` (or
/// none), at `now`.
pub fn next_probe(held_time: Option<i64>, now: i64) -> i64 {
    match held_time {
        Some(t) => (t + NEXT_AFTER_MS).max(now),
        None => now,
    }
}

struct Fetcher {
    client: reqwest::Client,
    cache: PathBuf,
    runtime: PathBuf,
    field_time: Option<i64>,
    /// The newest hour the probe named whose subset was asked for (or that
    /// was too old to ask for): never asked again, whatever came of it, and
    /// never cleared by the stale drop (review M1).
    probed: Option<i64>,
    held: Held,
    due: i64,
}

/// Whether the probe's hour `time` is worth the subset at `now`: `Ok(false)`
/// when that hour was already asked for, an error when it is too old to show.
pub fn worth_fetching(probed: Option<i64>, time: i64, now: i64) -> Result<bool, String> {
    if probed.is_some_and(|p| p >= time) {
        return Ok(false);
    }
    if now - time >= STALE_MS {
        return Err(format!(
            "the newest analysis ({}) is over 3 hours old",
            iso(time)
        ));
    }
    Ok(true)
}

impl Fetcher {
    async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let url = match std::env::var(super::BASE_ENV) {
            Ok(base) if !base.is_empty() => super::rebase(url, &base),
            _ => url.to_owned(),
        };
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("{e}"))?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BODY as u64)
        {
            return Err("body over the size limit".into());
        }
        let body = response.bytes().await.map_err(|e| format!("{e}"))?;
        if body.len() > MAX_BODY {
            return Err("body over the size limit".into());
        }
        Ok(body.to_vec())
    }

    /// Draw a subset answer and publish its texture; `Err` says why not.
    async fn take(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        let drawn = tokio::task::spawn_blocking(move || {
            let field = Field::from_dods(&bytes, TEMP_STRIDE, WIND_STRIDE)?;
            draw(&field).map_err(|e| format!("drawing: {e}"))
        })
        .await
        .map_err(|e| format!("{e}"))??;
        publish_texture(&self.runtime, &drawn.texture, &drawn.png)
            .map_err(|e| format!("writing the texture: {e}"))?;
        self.field_time = Some(drawn.time_ms);
        self.held.drawn = Some(Arc::new(drawn));
        Ok(())
    }

    /// The cached subset, when it is still fresh.
    async fn load_cached(&mut self, now: i64) {
        let Ok(bytes) = fs::read(self.cache.join(CACHE_NAME)) else {
            return;
        };
        let fresh = parse_dods(&bytes)
            .and_then(|v| time_ms(&v))
            .is_ok_and(|t| now - t < STALE_MS);
        if fresh && self.take(bytes).await.is_ok() {
            self.held.status = "ok";
            self.due = next_probe(self.field_time, now);
        }
    }

    /// Probe, and fetch the subset when the probe names a newer hour.
    async fn update(&mut self, now: i64) {
        let mut requests = 1;
        let mut bytes = 0;
        let result: Result<bool, String> = async {
            let probe = self.get(&probe_url()).await?;
            bytes += probe.len();
            let time = time_ms(&parse_dods(&probe)?)?;
            if self.field_time.is_some_and(|t| t >= time) {
                return Ok(false);
            }
            let fetch = worth_fetching(self.probed, time, now);
            if fetch.is_err() {
                self.probed = Some(time);
            }
            if !fetch? {
                return Ok(false);
            }
            requests += 1;
            let body = self.get(&subset_url()).await;
            // Asked once: an hour that does not decode is not asked again.
            self.probed = Some(time);
            let body = body?;
            bytes += body.len();
            self.take(body.clone()).await?;
            if let Err(e) = write_atomic(&self.cache.join(CACHE_NAME), &body) {
                eprintln!("{} Grid metnordic: caching: {e}", stamp());
            }
            Ok(true)
        }
        .await;
        match result {
            Ok(new) => {
                // Unchanged after a failure (an hour already asked for):
                // the failure stands until a new hour reads.
                if new || self.held.status != "failed" {
                    self.held.status = "ok";
                    self.held.note.clear();
                }
                self.due = if new {
                    next_probe(self.field_time, now)
                } else {
                    now + RETRY_MS
                };
                eprintln!(
                    "{} Grid metnordic: requests={requests} bytes={bytes} hour={} {}",
                    stamp(),
                    self.field_time.map_or("none".into(), iso),
                    if new { "new" } else { "unchanged" }
                );
            }
            Err(e) => {
                self.held.status = "failed";
                self.held.note = e.clone();
                self.due = now + RETRY_MS;
                eprintln!(
                    "{} Grid metnordic: requests={requests} failed: {e}",
                    stamp()
                );
            }
        }
    }
}

/// Write the texture under `runtime/tex/` (temporary name, then rename);
/// an existing file of that name is the same bytes.
fn publish_texture(runtime: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    if !crate::protocol::is_texture_path(name) {
        return Err(io::Error::other(format!("refusing texture path {name:?}")));
    }
    let path = runtime.join(name);
    if path.exists() {
        return Ok(());
    }
    fs::create_dir_all(runtime.join("tex"))?;
    let tmp = runtime.join(format!("{name}.tmp"));
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

/// The grid's task: while a client shows a grid layer, keep the newest
/// hour; otherwise sleep until one turns it on. `runtime` is the engine's
/// runtime directory (textures go under its `tex/`).
pub async fn run(runtime: PathBuf) {
    let hub = &*HUB;
    let setup = reqwest::Client::builder()
        .user_agent(super::USER_AGENT)
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| format!("building the HTTP client: {e}"))
        .and_then(|client| {
            cache_dir()
                .map(|cache| (client, cache))
                .map_err(|e| format!("the obs cache: {e}"))
        });
    let (client, cache) = match setup {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("{} Grid metnordic: disabled: {e}", stamp());
            let held = Held {
                drawn: None,
                status: "failed",
                note: e,
            };
            loop {
                if hub.wanted() {
                    hub.publish(held.clone());
                }
                hub.wake.notified().await;
            }
        }
    };
    let mut fetcher = Fetcher {
        client,
        cache,
        runtime,
        field_time: None,
        probed: None,
        held: Held::default(),
        due: 0,
    };
    let mut first = true;
    loop {
        if !hub.wanted() {
            hub.wake.notified().await;
            continue;
        }
        let now = now_ms();
        if first {
            first = false;
            fetcher.load_cached(now).await;
        }
        if now >= fetcher.due {
            fetcher.update(now).await;
        }
        // An hour past its use is dropped, not shown.
        if fetcher.field_time.is_some_and(|t| now_ms() - t >= STALE_MS) {
            fetcher.field_time = None;
            fetcher.held.drawn = None;
            if fetcher.held.note.is_empty() {
                fetcher.held.note = "the newest analysis is over 3 hours old".into();
                fetcher.held.status = "failed";
            }
        }
        hub.publish(fetcher.held.clone());
        let _ = tokio::time::timeout(TICK, hub.wake.notified()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::tests::fixture;

    const FIXTURE: &str = "metnordic_20260923T04Z.dods.gz";

    #[test]
    fn the_projection_matches_the_files_corners() {
        let lcc = Lcc::default();
        // latitude/longitude[0][0] and [2320][1795] of the 04Z file.
        let (lat, lon) = lcc.inverse(-897442.2, -1104322.0);
        assert!(
            (lat - 52.30272).abs() < 1e-3 && (lon - 1.9184653).abs() < 1e-3,
            "{lat} {lon}"
        );
        let (lat, lon) = lcc.inverse(897557.8, 1215678.0);
        assert!(
            (lat - 72.18485).abs() < 1e-3 && (lon - 41.764282).abs() < 1e-3,
            "{lat} {lon}"
        );
        let (x, y) = lcc.forward(59.33, 18.06);
        let (lat, lon) = lcc.inverse(x, y);
        assert!((lat - 59.33).abs() < 1e-9 && (lon - 18.06).abs() < 1e-9);
    }

    #[test]
    fn a_time_probe_reads() {
        let vars = parse_dods(&fixture("metnordic_time_20260923T04Z.dods.gz")).unwrap();
        assert_eq!(iso(time_ms(&vars).unwrap()), "2026-09-23T04:00:00Z");
    }

    #[test]
    fn the_fixture_subset_decodes() {
        // Fetched at stride 60 (temperature, x, y) and 240 (wind).
        let field = Field::from_dods(&fixture(FIXTURE), 60, 240).unwrap();
        assert_eq!(iso(field.time_ms), "2026-09-23T04:00:00Z");
        let t = &field.temp_c;
        assert_eq!((t.nx, t.ny), (30, 39));
        assert!((t.step - 60_000.0).abs() < 1.0 && (t.x0 + 897442.2).abs() < 1.0);
        assert_eq!((field.wind_ms.nx, field.wind_ms.ny), (8, 10));
        assert!((field.wind_ms.step - 240_000.0).abs() < 1.0);
        // A September night: every value between −10 and 25 °C.
        assert!(t.values.iter().all(|c| (-10.0..25.0).contains(c)));
        assert!(field.wind_ms.values.iter().all(|v| (0.0..40.0).contains(v)));
        assert!(
            field
                .wind_dir
                .values
                .iter()
                .all(|v| (0.0..=360.0).contains(v))
        );
        // A truncated answer is an error, not a short field.
        let bytes = fixture(FIXTURE);
        assert!(Field::from_dods(&bytes[..bytes.len() - 10], 60, 240).is_err());
        assert!(parse_dods(b"<html>busy</html>").is_err());
    }

    #[test]
    fn the_field_draws_in_mercator_on_the_station_scale() {
        let field = Field::from_dods(&fixture(FIXTURE), 60, 240).unwrap();
        let lcc = Lcc::default();
        let bounds = field.bounds(&lcc);
        // Near the file's corners (the stride-60 fixture stops 55 km short
        // of the east and north edges); the top edge bulges north of them.
        assert!((-11.8..-11.0).contains(&bounds[0]), "{bounds:?}");
        assert!(bounds[3] > 72.2 && bounds[1] < 52.4, "{bounds:?}");
        let image = render(&field, &lcc, bounds, 300);
        // A pixel at Stockholm carries the field's value there, opaque.
        let [west, south, east, north] = bounds;
        let (lat, lon) = (59.33, 18.06);
        let col = ((lon - west) / (east - west) * 300.0) as u32;
        let row = ((mercator_y(north) - mercator_y(lat)) / (mercator_y(north) - mercator_y(south))
            * image.height as f64) as u32;
        let at = ((row * 300 + col) * 4) as usize;
        let (x, y) = lcc.forward(lat, lon);
        let c = field.temp_c.at(x, y).unwrap();
        let want = color(c);
        let got = &image.rgba[at..at + 4];
        assert_eq!(got[3], 255);
        for i in 0..3 {
            assert!(
                (got[i] as i32 - want[i] as i32).abs() <= 12,
                "{got:?} {want:?}"
            );
        }
        // The top-left corner is outside the conic grid: transparent.
        assert_eq!(image.rgba[3], 0);
        assert!(image.min_c <= c && c <= image.max_c);
        // Scale ends and a middle stop.
        assert_eq!(color(-40.0), [0x6a, 0x3d, 0x9a]);
        assert_eq!(color(8.0), [0xe9, 0xe9, 0x8a]);
        assert_eq!(color(40.0), [0xc2, 0x23, 0x3a]);
        // The PNG reads back at its size.
        let bytes = png(&image).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().width, 300);
        assert_eq!(reader.info().height, image.height);
    }

    #[test]
    fn the_wind_is_a_point_list_with_the_stations_convention() {
        let field = Field::from_dods(&fixture(FIXTURE), 60, 240).unwrap();
        let points: Vec<[f64; 4]> =
            serde_json::from_str(&wind_points(&field, &Lcc::default())).unwrap();
        assert_eq!(points.len(), 80);
        // Row 0, column 0 is the south-west corner.
        assert!((points[0][0] - 52.30).abs() < 0.01 && (points[0][1] - 1.92).abs() < 0.01);
        assert_eq!(
            points[0][2],
            (field.wind_ms.values[0] * 10.0).round() / 10.0
        );
        assert!(points.iter().all(|p| (0.0..360.0).contains(&p[3])));
    }

    #[test]
    fn the_line_holds_only_the_layers_asked_for() {
        let field = Field::from_dods(&fixture(FIXTURE), 60, 240).unwrap();
        let drawn = Arc::new(draw(&field).unwrap());
        assert!(drawn.texture.starts_with("tex/grid-temp-20260923T04Z-"));
        assert!(crate::protocol::is_texture_path(&drawn.texture));
        let held = Held {
            drawn: Some(drawn.clone()),
            status: "ok",
            note: String::new(),
        };
        let parse = |layers| -> serde_json::Value {
            let text = line(&held, layers);
            assert!(text.ends_with('\n'));
            serde_json::from_str(&text).unwrap()
        };
        let both = parse(Layers {
            temp: true,
            wind: true,
        });
        assert_eq!(both["type"], "obs");
        assert_eq!(both["source"], "grid");
        assert_eq!(both["time"], "2026-09-23T04:00:00Z");
        assert_eq!(both["temperature"]["texture"], drawn.texture.as_str());
        assert_eq!(both["temperature"]["width"], IMAGE_WIDTH);
        assert_eq!(both["temperature"]["bounds"].as_array().unwrap().len(), 4);
        assert_eq!(both["wind"]["spacingKm"], 240.0);
        assert_eq!(both["wind"]["cols"], 8);
        assert_eq!(both["attribution"], "MET Norway (CC BY 4.0)");
        assert_eq!(both["provider"]["status"], "ok");
        let temp = parse(Layers {
            temp: true,
            wind: false,
        });
        assert!(temp.get("wind").is_none() && temp.get("temperature").is_some());
        let wind = parse(Layers {
            temp: false,
            wind: true,
        });
        assert!(wind.get("temperature").is_none() && wind.get("wind").is_some());
        // Nothing held: the note says why, no layers, no credit.
        let failed = Held {
            drawn: None,
            status: "failed",
            note: "HTTP 503".into(),
        };
        let v: serde_json::Value = serde_json::from_str(&line(
            &failed,
            Layers {
                temp: true,
                wind: true,
            },
        ))
        .unwrap();
        assert_eq!(v["provider"]["note"], "HTTP 503");
        assert!(v.get("temperature").is_none() && v["attribution"] == "");
    }

    #[test]
    fn the_hub_answers_a_change_and_wakes_on_the_first_client() {
        let hub = Hub::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let on = Layers {
            temp: true,
            wind: false,
        };
        hub.set(1, on, &tx);
        assert!(hub.wanted());
        assert!(rx.try_recv().is_err(), "nothing held yet");
        let held = Held {
            drawn: None,
            status: "failed",
            note: "x".into(),
        };
        hub.publish(held.clone());
        hub.publish(held);
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err(), "an unchanged grid is not re-sent");
        hub.set(1, on, &tx);
        assert!(rx.try_recv().is_err(), "the same layers again: nothing");
        hub.set(
            1,
            Layers {
                temp: true,
                wind: true,
            },
            &tx,
        );
        assert!(
            rx.try_recv().is_ok(),
            "wind added: the line again, with wind"
        );
        hub.set(1, Layers::default(), &tx);
        assert!(!hub.wanted());
    }

    /// Development: draw a real subset (`OMASTORM_GRID_SAMPLE`, a `.dods`
    /// fetched at the engine's strides) into `OMASTORM_GRID_OUT` and say
    /// how big and how slow it was. `cargo test -- --ignored draw_a_sample`.
    #[test]
    #[ignore]
    fn draw_a_sample() {
        let input = std::env::var("OMASTORM_GRID_SAMPLE").unwrap();
        let out = std::env::var("OMASTORM_GRID_OUT").unwrap();
        let bytes = fs::read(input).unwrap();
        let start = std::time::Instant::now();
        let field = Field::from_dods(&bytes, TEMP_STRIDE, WIND_STRIDE).unwrap();
        let drawn = draw(&field).unwrap();
        let took = start.elapsed();
        fs::write(&out, &drawn.png).unwrap();
        eprintln!(
            "{} png={} bytes wind={} bytes in {took:?}; {}",
            drawn.texture,
            drawn.png.len(),
            drawn.wind.len(),
            drawn.temperature
        );
    }

    #[test]
    fn a_status_change_alone_does_not_resend_the_hour() {
        let field = Field::from_dods(&fixture(FIXTURE), 60, 240).unwrap();
        let drawn = Arc::new(draw(&field).unwrap());
        let hub = Hub::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        hub.set(
            1,
            Layers {
                temp: true,
                wind: true,
            },
            &tx,
        );
        let ok = Held {
            drawn: Some(drawn.clone()),
            status: "ok",
            note: String::new(),
        };
        hub.publish(ok);
        assert!(rx.try_recv().is_ok());
        hub.publish(Held {
            drawn: Some(drawn),
            status: "failed",
            note: "HTTP 503".into(),
        });
        assert!(rx.try_recv().is_err(), "same hour, new status: not resent");
        // A field with no value is a failure (review S1): every temperature
        // of the fixture (after x, y and time) made NaN.
        let mut bytes = fixture(FIXTURE);
        let data = bytes.windows(7).position(|w| w == b"\nData:\n").unwrap() + 7;
        let temp = data + (8 + 30 * 4) + (8 + 39 * 4) + (8 + 8) + 8;
        for i in 0..30 * 39 {
            bytes[temp + 4 * i..temp + 4 * i + 4].copy_from_slice(&f32::NAN.to_be_bytes());
        }
        let err = Field::from_dods(&bytes, 60, 240).unwrap_err();
        assert!(err.contains("no values"), "{err}");
    }

    #[test]
    fn an_hour_is_asked_for_once_and_a_stale_one_never() {
        let t = crate::obs::parse_iso("2026-09-23T04:00:00Z").unwrap();
        let hour = 60 * 60 * 1000;
        assert_eq!(worth_fetching(None, t, t + hour / 4), Ok(true));
        // Asked for already (read or not): not again, not even when stale.
        assert_eq!(worth_fetching(Some(t), t, t + hour / 4), Ok(false));
        assert_eq!(worth_fetching(Some(t), t, t + 5 * hour), Ok(false));
        // A `_latest` over 3 hours old: an error, and no subset.
        assert!(worth_fetching(None, t, t + 3 * hour).is_err());
        assert_eq!(worth_fetching(Some(t), t + hour, t + 2 * hour), Ok(true));
    }

    #[test]
    fn the_next_hour_is_probed_twenty_minutes_past_it() {
        let t = crate::obs::parse_iso("2026-09-23T04:00:00Z").unwrap();
        let now = crate::obs::parse_iso("2026-09-23T04:16:00Z").unwrap();
        assert_eq!(iso(next_probe(Some(t), now)), "2026-09-23T05:20:00Z");
        assert_eq!(next_probe(None, now), now);
        // Late: due at once.
        let late = crate::obs::parse_iso("2026-09-23T06:00:00Z").unwrap();
        assert_eq!(next_probe(Some(t), late), late);
    }

    #[test]
    fn the_subset_asks_for_strided_parts_only() {
        let url = subset_url();
        assert!(
            url.contains(
                "air_temperature_2m.air_temperature_2m%5B0%5D%5B0:3:2320%5D%5B0:3:1795%5D"
            )
        );
        assert!(url.contains("wind_speed_10m%5B0%5D%5B0:24:2320%5D%5B0:24:1795%5D"));
        assert!(url.contains("?time,x%5B0:3:1795%5D,y%5B0:3:2320%5D,"));
        assert!(probe_url().ends_with(".dods?time"));
    }
}
