//! A 3D grid of the newest frame (S24c, plan §S24c, `docs/protocol.md`
//! "Sections and profiles"): the selected station's radars' volumes, as the
//! tilt store holds them for the newest frame's time, poured into columns of
//! 24 levels of 500 m, so a client can ask for a vertical cut along a line
//! (`set_section`) or one column's values (`profile`).
//!
//! - **Placement** is My mosaic's (`mosaic::Layout`: a radar alone, My
//!   mosaic's set, a composite's every radar), on its 2,000 m Web Mercator
//!   texels; each column here is `FACTOR` × `FACTOR` of them, aligned to the
//!   global lattice, so the same place is the same column for every station.
//! - **Fill** (`fill`): per radar, per scan (`mosaic::Beam`, the lookup rule
//!   the frames use), per texel, the gate's code goes to every level its 1°
//!   beam covers there; each cell keeps the maximum code and a sample count,
//!   and which radars fed it. No averaging.
//! - **Cut** (`Grid::cut`) and **profile** (`Grid::column`): read back by
//!   latitude and longitude.
//! - **Life** (`Sections`): built on demand from the tilt store only (never
//!   a request), for the newest frame only, held until that frame is
//!   replaced or `KEEP` passes unused, then dropped; a section is dropped
//!   when the client that set it leaves.

use crate::composite::{PIXEL_M, mercator_y};
use crate::mosaic::{Beam, Layout, NONE, OUTER_MS, Rule, Set, SiteReach, UNIT_M, mercator_x};
use crate::products::{BEAM_HALF_DEG, EARTH_M, ETOP_DBZ, Tilt, Want};
use crate::protocol::{Message, SiteKind, Station};
use crate::providers::ProviderId;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::sync::mpsc::Sender;

/// Levels of `LEVEL_M` from sea level: 0 to 12 km.
pub const LEVELS: usize = 24;
pub const LEVEL_M: u32 = 500;
/// A column is `FACTOR` × `FACTOR` of the 2,000 m texels: 4,000 m of Web
/// Mercator, about 2 km of ground at 60° N. At 2,000 m the Nordic grid's
/// codes, counts and radar bits would be ~290 MB (`coord/log/S24c.md`).
pub const FACTOR: i64 = 2;
/// A section's column length on the ground, and the most columns.
pub const COLUMN_M: f64 = 2000.0;
pub const MAX_COLUMNS: usize = 300;
/// Radars a cell can name (`fed`'s bits, per column in the station's order).
const MAX_FED: u32 = 8;
/// Radars a column can name (`masks`' bits).
const MAX_RADARS: usize = 64;
/// How long a grid is held unused.
pub const KEEP: Duration = Duration::from_secs(3 * 60);
/// The great-circle sphere of the lookup rule.
const SPHERE_M: f64 = 6_371_000.0;
/// No level at this distance (`spans`).
const NO_SPAN: u16 = u16::MAX;

/// `hello.sections`.
#[derive(Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub levels: u32,
    pub level_m: u32,
    pub column_m: u32,
    pub max_columns: u32,
}

pub fn info() -> Info {
    Info {
        levels: LEVELS as u32,
        level_m: LEVEL_M,
        column_m: COLUMN_M as u32,
        max_columns: MAX_COLUMNS as u32,
    }
}

// ---------------------------------------------------------------------------
// The grid
// ---------------------------------------------------------------------------

/// Columns of `LEVELS` cells over a station's box.
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    /// The global column index (2,000 m lattice index ÷ `FACTOR`) of column
    /// 0, and of row 0 counted from the south: row `r` is global `top − r`.
    west: i64,
    top: i64,
    /// Per cell, `(row × cols + col) × LEVELS + level`: the maximum code
    /// (0 below threshold, 2–255 measured), the samples (0: no beam) and
    /// which of the column's radars fed it (bit `i`: its `i`-th radar).
    codes: Vec<u8>,
    counts: Vec<u8>,
    fed: Vec<u8>,
    /// Per column, which of `radars` reached it (bit = index).
    masks: Vec<u64>,
    /// The layout's radars' ids, in its order.
    pub radars: Vec<String>,
}

/// What a fill found.
#[derive(Default, Debug)]
pub struct Stats {
    /// Radars with a volume, and without.
    pub used: Vec<String>,
    pub missing: Vec<String>,
    /// Columns more than `MAX_FED` radars reached.
    pub crowded: usize,
    /// Gate samples placed (one per texel, scan and gate, over its levels).
    pub samples: u64,
}

/// One radar's volume turned into lookups: its scans (`Beam`), and per scan
/// and ground distance unit the levels its beam covers, packed low | high
/// << 8 (`NO_SPAN`: none, or no gate).
struct Stack<'a> {
    beams: Vec<Beam<'a>>,
    spans: Vec<Vec<u16>>,
    /// The first distance unit past every scan's far edge.
    edge: usize,
}

impl<'a> Stack<'a> {
    fn new(tilts: &'a [Tilt], alt_m: f64, reach_m: f64) -> Stack<'a> {
        let beams: Vec<Beam> = tilts.iter().map(|t| Beam::new(t, reach_m)).collect();
        let spans = beams.iter().map(|b| spans(b, alt_m)).collect();
        let edge = beams.iter().map(|b| b.edge).max().unwrap_or(0);
        Stack {
            beams,
            spans,
            edge,
        }
    }
}

/// The levels `lo_m`..`hi_m` metres above sea level touch: level `k` covers
/// `k × LEVEL_M` up to `(k + 1) × LEVEL_M`, touched when the span reaches
/// inside it; `None` wholly below 0 or above the top.
pub fn level_span(lo_m: f64, hi_m: f64) -> Option<(usize, usize)> {
    let step = f64::from(LEVEL_M);
    let top = LEVELS as f64 * step;
    if hi_m <= 0.0 || lo_m >= top || hi_m < lo_m {
        return None;
    }
    let lo = (lo_m / step).floor().max(0.0) as usize;
    let hi = (((hi_m / step).ceil() as usize).saturating_sub(1)).min(LEVELS - 1);
    (lo <= hi).then_some((lo, hi))
}

/// Per ground distance unit, the levels one scan's beam covers: its centre
/// above sea level (`alt_m` + `Beam::height`) ± `r · tan(BEAM_HALF_DEG)` at
/// slant range `r`, where the scan has a gate.
fn spans(beam: &Beam, alt_m: f64) -> Vec<u16> {
    let e = beam.deg.to_radians();
    let half = BEAM_HALF_DEG.to_radians().tan();
    beam.gate
        .iter()
        .zip(&beam.height)
        .enumerate()
        .map(|(d, (&gate, &height))| {
            if gate == NONE {
                return NO_SPAN;
            }
            let theta = d as f64 * UNIT_M / EARTH_M;
            let r = EARTH_M * theta.sin() / (e + theta).cos();
            let centre = alt_m + height;
            let w = r * half;
            level_span(centre - w, centre + w).map_or(NO_SPAN, |(lo, hi)| lo as u16 | (hi as u16) << 8)
        })
        .collect()
}

/// Fill the grid over `layout`'s box from each radar's volumes, radar by
/// radar in its order: `load(i)` gives radar `i`'s volume at T (scans in
/// any order) and, when its scans reach less than its reach, a longer one
/// for the distances past them (`None`: missing). A volume is held only
/// while its radar is poured in.
pub fn fill(
    layout: &Layout,
    mut load: impl FnMut(usize) -> Option<(Vec<Tilt>, Option<Vec<Tilt>>)>,
) -> (Grid, Stats) {
    let (l0, l1) = layout.lattice;
    let west = l0.div_euclid(FACTOR);
    let east = (l0 + i64::from(layout.width) - 1).div_euclid(FACTOR);
    // Row 0 of the layout is lattice row l1 − 1 counted from the south.
    let top = (l1 - 1).div_euclid(FACTOR);
    let bottom = (l1 - i64::from(layout.height)).div_euclid(FACTOR);
    let (cols, rows) = ((east - west + 1) as usize, (top - bottom + 1) as usize);
    let n = cols * rows;
    let mut codes = vec![0u8; n * LEVELS];
    let mut counts = vec![0u8; n * LEVELS];
    let mut fed = vec![0u8; n * LEVELS];
    let mut masks = vec![0u64; n];
    let mut stats = Stats::default();
    for (i, radar) in layout.radars.iter().enumerate() {
        let id = radar.station.id.clone();
        let volumes = if i < MAX_RADARS { load(i) } else { None };
        let Some((primary, outer)) = volumes.filter(|(p, _)| !p.is_empty()) else {
            stats.missing.push(id);
            continue;
        };
        stats.used.push(id);
        let alt = radar.station.alt_m;
        let near = Stack::new(&primary, alt, radar.reach_m);
        let far = outer
            .as_deref()
            .filter(|o| !o.is_empty())
            .map(|o| Stack::new(o, alt, radar.reach_m))
            .filter(|f| f.edge > near.edge);
        let own = 1u64 << i;
        let before = own - 1;
        for (col, row, d, az) in radar.texels() {
            let stack = match &far {
                Some(f) if d >= near.edge => f,
                _ => &near,
            };
            let gc = (l0 + col as i64).div_euclid(FACTOR) - west;
            let gr = top - (l1 - 1 - row as i64).div_euclid(FACTOR);
            let column = gr as usize * cols + gc as usize;
            let mut bit: Option<u8> = None;
            for (beam, span) in stack.beams.iter().zip(&stack.spans) {
                let span = span[d];
                if span == NO_SPAN {
                    continue;
                }
                let ray = beam.rows[usize::from(az)];
                if ray == NONE {
                    continue;
                }
                let code = beam.sweep.rays[usize::from(ray)].codes[usize::from(beam.gate[d])];
                if code == 1 {
                    continue;
                }
                let bit = *bit.get_or_insert_with(|| {
                    let mask = masks[column];
                    // Radars pour in in order: every bit set is below ours.
                    let local = (mask & before).count_ones();
                    if mask & own == 0 {
                        masks[column] = mask | own;
                        if local == MAX_FED {
                            stats.crowded += 1;
                        }
                    }
                    if local < MAX_FED { 1 << local } else { 0 }
                });
                let (lo, hi) = (usize::from(span & 0xff), usize::from(span >> 8));
                let base = column * LEVELS;
                for cell in base + lo..=base + hi {
                    codes[cell] = codes[cell].max(code);
                    counts[cell] = counts[cell].saturating_add(1);
                    fed[cell] |= bit;
                }
                stats.samples += 1;
            }
        }
    }
    let grid = Grid {
        cols,
        rows,
        west,
        top,
        codes,
        counts,
        fed,
        masks,
        radars: layout.radars.iter().map(|r| r.station.id.clone()).collect(),
    };
    (grid, stats)
}

/// One column's cells, bottom to top: code, samples, the radars that fed it.
pub struct Column {
    pub levels: Vec<(u8, u8, Vec<String>)>,
    /// Every radar that reached the column, in the station's order.
    pub radars: Vec<String>,
}

/// A cut along a line: `columns` × `LEVELS` codes, row 0 the top level.
pub struct Cut {
    pub columns: usize,
    pub length_m: f64,
    pub codes: Vec<u8>,
    /// The radars that reached any of its columns, in the station's order.
    pub radars: Vec<String>,
}

impl Grid {
    /// Bytes held.
    pub fn bytes(&self) -> usize {
        self.codes.len() * 3 + self.masks.len() * 8
    }

    /// The column holding (`lat`, `lon`), `None` outside the grid.
    fn column_at(&self, lat: f64, lon: f64) -> Option<usize> {
        let i = (mercator_x(lon) / PIXEL_M).floor() as i64;
        let j = (mercator_y(lat) / PIXEL_M).floor() as i64;
        let c = i.div_euclid(FACTOR) - self.west;
        let r = self.top - j.div_euclid(FACTOR);
        (c >= 0 && r >= 0 && (c as usize) < self.cols && (r as usize) < self.rows)
            .then(|| r as usize * self.cols + c as usize)
    }

    fn ids(&self, mask: u64) -> Vec<String> {
        (0..self.radars.len().min(MAX_RADARS))
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| self.radars[i].clone())
            .collect()
    }

    /// The column at (`lat`, `lon`), `None` outside the grid.
    pub fn column(&self, lat: f64, lon: f64) -> Option<Column> {
        let column = self.column_at(lat, lon)?;
        let radars = self.ids(self.masks[column]);
        let levels = (0..LEVELS)
            .map(|k| {
                let cell = column * LEVELS + k;
                let fed = self.fed[cell];
                let names = radars
                    .iter()
                    .take(MAX_FED as usize)
                    .enumerate()
                    .filter(|(b, _)| fed & (1 << b) != 0)
                    .map(|(_, id)| id.clone())
                    .collect();
                (self.codes[cell], self.counts[cell], names)
            })
            .collect();
        Some(Column { levels, radars })
    }

    /// The cut along the great circle from `from` to `to`: columns of
    /// `COLUMN_M` of ground (at most `MAX_COLUMNS`), each the grid column
    /// holding its middle; code 1 where no beam reached a cell.
    pub fn cut(&self, from: Point, to: Point) -> Cut {
        let length_m = great_circle_m(from, to);
        let columns = columns_for(length_m);
        let step = length_m / columns as f64;
        let bearing = bearing_deg(from, to);
        let mut codes = vec![1u8; columns * LEVELS];
        let mut mask = 0u64;
        for i in 0..columns {
            let (lat, lon) =
                crate::terrain::destination(from.lat, from.lon, bearing, (i as f64 + 0.5) * step);
            let Some(column) = self.column_at(lat, lon) else {
                continue;
            };
            mask |= self.masks[column];
            for k in 0..LEVELS {
                let cell = column * LEVELS + k;
                if self.counts[cell] > 0 {
                    codes[(LEVELS - 1 - k) * columns + i] = self.codes[cell];
                }
            }
        }
        Cut {
            columns,
            length_m,
            codes,
            radars: self.ids(mask),
        }
    }
}

/// A section's column count for a line `length_m` long.
pub fn columns_for(length_m: f64) -> usize {
    ((length_m / COLUMN_M).ceil() as usize).clamp(1, MAX_COLUMNS)
}

/// Great-circle distance on the lookup rule's sphere, metres.
pub fn great_circle_m(a: Point, b: Point) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dp = (p2 - p1) / 2.0;
    let dl = (b.lon - a.lon).to_radians() / 2.0;
    let h = dp.sin().powi(2) + p1.cos() * p2.cos() * dl.sin().powi(2);
    2.0 * SPHERE_M * h.clamp(0.0, 1.0).sqrt().asin()
}

/// The initial bearing from `a` to `b`, degrees.
fn bearing_deg(a: Point, b: Point) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dl = (b.lon - a.lon).to_radians();
    (dl.sin() * p2.cos())
        .atan2(p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos())
        .to_degrees()
}

/// A code as dBZ; `None` for below threshold (0) and no data (1).
fn dbz(code: u8) -> Option<f64> {
    (code >= 2).then(|| (f64::from(code) - crate::composite::OFFSET as f64) / crate::composite::SCALE as f64)
}

// ---------------------------------------------------------------------------
// What a grid is built from
// ---------------------------------------------------------------------------

/// Where a grid's radars come from.
#[derive(Clone, Debug)]
enum Place {
    Radar,
    Mosaic(Set),
    Composite,
}

/// A grid to build: the selected station's, for its newest frame.
#[derive(Clone, Debug)]
pub struct Job {
    /// Station, radars and time: a grid is reused while the key holds.
    pub key: String,
    pub station: Station,
    place: Place,
    sites: Vec<Station>,
    /// The frame's nominal time, its id and `scanTime`.
    pub t_ms: i64,
    pub frame_id: String,
    pub scan_time: String,
}

/// A built grid, or why there is none.
pub struct Built {
    pub key: String,
    pub grid: Option<Grid>,
    pub stats: Stats,
    pub seconds: f64,
}

impl Job {
    /// The grid for `station` showing the frame `frame_id` that started at
    /// `start_ms`; `set` is My mosaic's. `Err`: the station has no radars to
    /// cut.
    pub fn new(
        station: &Station,
        set: &Set,
        sites: &[Station],
        frame_id: &str,
        scan_time: &str,
        start_ms: i64,
    ) -> Result<Job, String> {
        let (place, variant) = if station.provider == ProviderId::Mosaic {
            if set.is_empty() {
                return Err("My mosaic has no radars yet.".into());
            }
            (Place::Mosaic(set.clone()), set.variant())
        } else if station.kind == SiteKind::Grid {
            if crate::mosaic::grid_box(station).is_none() {
                return Err(format!("{} has no radars to cut.", station.name));
            }
            (Place::Composite, String::new())
        } else {
            (Place::Radar, String::new())
        };
        let t_ms = crate::mosaic::nominal_ms(start_ms);
        Ok(Job {
            key: format!("{}|{variant}|{t_ms}", station.id),
            station: station.clone(),
            place,
            sites: sites.to_vec(),
            t_ms,
            frame_id: frame_id.to_owned(),
            scan_time: scan_time.to_owned(),
        })
    }

    /// The station's radars placed as its frames place them.
    pub fn layout(&self) -> Result<Layout, String> {
        match &self.place {
            Place::Radar => {
                let one = Set {
                    sites: vec![SiteReach {
                        id: self.station.id.clone(),
                        reach_km: self.station.range_km,
                    }],
                    rule: Rule::Lowest,
                    height_m: None,
                    above: None,
                };
                Layout::new(&one, &self.sites)
            }
            Place::Mosaic(set) => Layout::new(set, &self.sites),
            Place::Composite => Layout::grid(&self.station, Want::ColMax, &self.sites),
        }
    }

    /// Build from the engine's tilt store.
    pub fn build(&self) -> Result<Built, String> {
        let store = crate::tilts::shared()
            .ok_or("the tilt store is off (OMASTORM_TILTS_MB=0), and sections are made from it")?;
        self.build_from(&store)
    }

    /// Build from `store`: each radar's volume at T and, where its scans
    /// reach less than its reach, its latest longer volume up to
    /// `OUTER_MS` older. Nothing else is read, and nothing is fetched.
    pub fn build_from(&self, store: &crate::tilts::Store) -> Result<Built, String> {
        let started = Instant::now();
        let layout = self.layout()?;
        let t = self.t_ms;
        let (grid, stats) = fill(&layout, |i| {
            let radar = &layout.radars[i];
            let id = &radar.station.id;
            let primary = volume_of(store, id, t)?;
            let outer = if reach_m(&primary) + 1000.0 < radar.reach_m {
                longer(store, id, t, reach_m(&primary))
            } else {
                None
            };
            Some((primary, outer))
        });
        drop(layout);
        let grid = (!stats.used.is_empty()).then_some(grid);
        Ok(Built {
            key: self.key.clone(),
            grid,
            stats,
            seconds: started.elapsed().as_secs_f64(),
        })
    }
}

/// `station`'s volume at `t` from the store, `None` when it holds none.
fn volume_of(store: &crate::tilts::Store, station: &str, t: i64) -> Option<Vec<Tilt>> {
    let tilts: Vec<Tilt> = store
        .tilts(station, t)
        .ok()?
        .into_iter()
        .map(|(_, tilt)| tilt)
        .collect();
    (!tilts.is_empty()).then_some(tilts)
}

/// How far a volume's farthest gate is, in slant metres (as far as ground).
fn reach_m(tilts: &[Tilt]) -> f64 {
    tilts
        .iter()
        .map(|t| {
            let s = &t.sweep;
            f64::from(s.first_gate_m) + f64::from(s.gates) * f64::from(s.gate_spacing_m)
        })
        .fold(0.0, f64::max)
}

/// The newest stored volume of `station` before `t`, up to `OUTER_MS`
/// older, that reaches farther than `than` metres.
fn longer(store: &crate::tilts::Store, station: &str, t: i64, than: f64) -> Option<Vec<Tilt>> {
    let volumes = store.volumes(station).ok()?;
    volumes
        .into_iter()
        .map(|(when, _)| when)
        .filter(|&when| when < t && when >= t - OUTER_MS)
        .find_map(|when| volume_of(store, station, when).filter(|v| reach_m(v) > than + 1000.0))
}

// ---------------------------------------------------------------------------
// The wire
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Point {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Building,
    Ready,
    Empty,
    Outside,
}

/// `state.section`.
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SectionState {
    pub from: Point,
    pub to: Point,
    pub status: Status,
    pub message: String,
    pub frame_id: String,
    pub scan_time: String,
    pub texture: String,
    pub columns: u32,
    pub length_km: f64,
    pub levels: u32,
    pub level_m: u32,
    pub units: &'static str,
    pub scale: f32,
    pub offset: f32,
    pub palette: Vec<String>,
    pub bounds: Vec<i32>,
    pub radars: Vec<String>,
}

/// One level of a `profile`.
#[derive(Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Level {
    pub bottom_m: u32,
    pub top_m: u32,
    pub dbz: Option<f64>,
    pub samples: u32,
    pub radars: Vec<String>,
}

/// The `profile` reply.
#[derive(Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub v: u32,
    pub lat: f64,
    pub lon: f64,
    pub status: Status,
    pub message: String,
    pub frame_id: String,
    pub scan_time: String,
    pub units: &'static str,
    pub levels: Vec<Level>,
    pub radars: Vec<String>,
    pub echo_top_m: Option<u32>,
}

impl Profile {
    fn new(lat: f64, lon: f64, status: Status, message: String) -> Profile {
        Profile {
            v: crate::protocol::VERSION,
            lat,
            lon,
            status,
            message,
            frame_id: String::new(),
            scan_time: String::new(),
            units: "dBZ",
            levels: (0..LEVELS as u32)
                .map(|k| Level {
                    bottom_m: k * LEVEL_M,
                    top_m: (k + 1) * LEVEL_M,
                    dbz: None,
                    samples: 0,
                    radars: Vec::new(),
                })
                .collect(),
            radars: Vec::new(),
            echo_top_m: None,
        }
    }

    /// The column at (`lat`, `lon`) of `grid`, made for `job`'s frame.
    fn of(grid: &Grid, job: &Job, lat: f64, lon: f64) -> Profile {
        let Some(column) = grid.column(lat, lon) else {
            let mut outside = Profile::new(lat, lon, Status::Outside, "Outside the grid.".into());
            outside.frame_id.clone_from(&job.frame_id);
            outside.scan_time.clone_from(&job.scan_time);
            return outside;
        };
        let mut out = Profile::new(lat, lon, Status::Ready, String::new());
        out.frame_id.clone_from(&job.frame_id);
        out.scan_time.clone_from(&job.scan_time);
        for (level, (code, count, radars)) in out.levels.iter_mut().zip(column.levels) {
            level.dbz = if count > 0 { dbz(code) } else { None };
            level.samples = u32::from(count);
            level.radars = radars;
        }
        out.echo_top_m = out
            .levels
            .iter()
            .rev()
            .find(|l| l.dbz.is_some_and(|v| v >= ETOP_DBZ))
            .map(|l| l.top_m);
        out.radars = column.radars;
        out
    }
}

/// Check a `set_section`'s points: `Ok(None)` clears, `Ok(Some)` sets.
pub fn check_line(from: Option<Point>, to: Option<Point>) -> Result<Option<(Point, Point)>, String> {
    let (from, to) = match (from, to) {
        (None, None) => return Ok(None),
        (Some(from), Some(to)) => (from, to),
        _ => return Err("set_section needs both from and to, or neither to clear.".into()),
    };
    if [from, to].iter().any(|p| !in_range(p.lat, p.lon)) {
        return Err("set_section needs lat in [-90, 90] and lon in [-180, 180].".into());
    }
    if great_circle_m(from, to) < COLUMN_M {
        return Err("A section's two points must be at least 2 km apart.".into());
    }
    Ok(Some((from, to)))
}

pub fn in_range(lat: f64, lon: f64) -> bool {
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

// ---------------------------------------------------------------------------
// Life: the grid held, the section set, profiles waiting
// ---------------------------------------------------------------------------

struct Held {
    job: Job,
    grid: Grid,
    used: Instant,
}

struct Line {
    from: Point,
    to: Point,
    owner: u64,
}

struct Waiting {
    client: u64,
    reply: Sender<String>,
    lat: f64,
    lon: f64,
}

/// The engine's sections: at most one grid, held or being built, the
/// section line and who set it, and the profiles waiting for a grid.
#[derive(Default)]
pub struct Sections {
    held: Option<Held>,
    building: Option<String>,
    line: Option<Line>,
    /// The key and line the section was last cut for.
    cut_for: Option<String>,
    /// A build that found nothing for its key, and why.
    failed: Option<(String, String)>,
    waiting: Vec<Waiting>,
    pub wake: Arc<Notify>,
}

/// What `state.section` looks like while a cut is being made.
fn section(from: Point, to: Point, status: Status, message: String, template: &crate::protocol::Frame) -> SectionState {
    SectionState {
        from,
        to,
        status,
        message,
        frame_id: String::new(),
        scan_time: String::new(),
        texture: String::new(),
        columns: 0,
        length_km: (great_circle_m(from, to) / 100.0).round() / 10.0,
        levels: LEVELS as u32,
        level_m: LEVEL_M,
        units: "dBZ",
        scale: crate::composite::SCALE,
        offset: crate::composite::OFFSET,
        palette: template.palette.clone(),
        bounds: template.bounds.clone(),
        radars: Vec::new(),
    }
}

fn log(site: &str, message: impl std::fmt::Display) {
    eprintln!("{} Section {site}: {message}", crate::iso(crate::now_ms()));
}

/// The engine's resident and peak memory, from `/proc/self/status`.
pub fn memory() -> String {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|kb| kb.parse::<u64>().ok())
            .map_or_else(|| "?".to_owned(), |kb| format!("{} MB", kb / 1024))
    };
    format!("RSS {}, peak {}", field("VmRSS:"), field("VmHWM:"))
}

impl Sections {
    /// Set the section's line for `owner`, or clear it; `state.section`
    /// follows at once. True when it changed.
    pub fn set_line(
        &mut self,
        line: Option<(Point, Point)>,
        owner: u64,
        state: &mut Option<SectionState>,
        template: &crate::protocol::Frame,
    ) -> bool {
        self.cut_for = None;
        match line {
            None => {
                self.line = None;
                state.take().is_some()
            }
            Some((from, to)) => {
                self.line = Some(Line { from, to, owner });
                *state = Some(section(from, to, Status::Building, String::new(), template));
                self.wake.notify_one();
                true
            }
        }
    }

    /// A `profile` from `client`, answered on `reply` by the next pass.
    pub fn ask(&mut self, client: u64, reply: Sender<String>, lat: f64, lon: f64) {
        self.waiting.push(Waiting {
            client,
            reply,
            lat,
            lon,
        });
        self.wake.notify_one();
    }

    /// `client` disconnected: its profiles are forgotten, and a section it
    /// set is dropped. True when `state.section` changed.
    pub fn client_left(&mut self, client: u64, state: &mut Option<SectionState>) -> bool {
        self.waiting.retain(|w| w.client != client);
        if self.line.as_ref().is_some_and(|l| l.owner == client) {
            self.line = None;
            self.cut_for = None;
            return state.take().is_some();
        }
        false
    }

    /// A build finished (or failed): hold its grid, or remember why there
    /// is none so it is not tried again for the same frame.
    pub fn built(&mut self, job: Job, result: Result<Built, String>) {
        self.building = None;
        let site = job.station.id.clone();
        match result {
            Ok(Built {
                key,
                grid: Some(grid),
                stats,
                seconds,
            }) => {
                log(
                    &site,
                    format_args!(
                        "grid for {} built in {seconds:.2} s from {} of {} radars{}: {} × {} × {LEVELS}, {} MB, {} samples, {} columns over {MAX_FED} radars; {}",
                        job.scan_time,
                        stats.used.len(),
                        stats.used.len() + stats.missing.len(),
                        if stats.missing.is_empty() {
                            String::new()
                        } else {
                            format!(" (no volume: {})", stats.missing.join(", "))
                        },
                        grid.cols,
                        grid.rows,
                        grid.bytes() / 1_000_000,
                        stats.samples,
                        stats.crowded,
                        memory()
                    ),
                );
                debug_assert_eq!(key, job.key);
                self.held = Some(Held {
                    job,
                    grid,
                    used: Instant::now(),
                });
            }
            Ok(Built { key, stats, .. }) => {
                let message = format!(
                    "The tilt store holds no volume of {}'s radars for {}: a section is made from the volumes what is shown reads (Column max, Storm height, Rain mass or Height read whole volumes).",
                    job.station.name, job.scan_time
                );
                log(&site, format_args!("nothing to cut for {}: {} radars, none stored", job.scan_time, stats.missing.len()));
                self.failed = Some((key, message));
            }
            Err(e) => {
                log(&site, format_args!("no grid for {}: {e}", job.scan_time));
                self.failed = Some((job.key, e));
            }
        }
    }

    /// One pass, on a wake or once a second: drop a grid that is no longer
    /// the newest frame's or has not been used for `KEEP`; answer waiting
    /// profiles and (re)cut the section from the grid when it is held; or
    /// say which grid to build. `current` is the selected station's grid
    /// for its newest frame (`Ok(None)`: no frame yet; `Err`: nothing to
    /// cut there). Returns the job to build, and whether `state` changed.
    pub fn pass(
        &mut self,
        current: Result<Option<Job>, String>,
        dir: &Path,
        template: &crate::protocol::Frame,
        state: &mut Option<SectionState>,
    ) -> (Option<Job>, bool) {
        let key = current.as_ref().ok().and_then(|j| j.as_ref()).map(|j| j.key.clone());
        if let Some(held) = &self.held {
            let stale = key.as_deref() != Some(held.job.key.as_str());
            if stale || held.used.elapsed() >= KEEP {
                log(
                    &held.job.station.id,
                    format_args!(
                        "grid for {} dropped ({}); {}",
                        held.job.scan_time,
                        if stale { "a newer frame" } else { "unused for 3 minutes" },
                        memory()
                    ),
                );
                self.held = None;
            }
        }
        if self.failed.as_ref().is_some_and(|(k, _)| key.as_deref() != Some(k.as_str())) {
            self.failed = None;
        }
        let mut changed = false;
        if self.line.is_none() && self.waiting.is_empty() {
            return (None, false);
        }
        let job = match current {
            Err(message) => {
                self.answer_empty(Status::Empty, &message);
                changed |= self.empty_section(state, Status::Empty, message, template);
                return (None, changed);
            }
            Ok(None) => {
                self.answer_empty(Status::Empty, "No frame yet.");
                changed |= self.empty_section(state, Status::Building, "Waiting for the first frame.".into(), template);
                return (None, changed);
            }
            Ok(Some(job)) => job,
        };
        if let Some((_, message)) = self.failed.clone() {
            self.answer_empty(Status::Empty, &message);
            changed |= self.empty_section(state, Status::Empty, message, template);
            return (None, changed);
        }
        if let Some(held) = &mut self.held {
            held.used = Instant::now();
            for w in self.waiting.drain(..) {
                let profile = Profile::of(&held.grid, &held.job, w.lat, w.lon);
                let _ = w.reply.try_send(crate::line(&Message::Profile(&profile)));
            }
            if let Some(line) = &self.line
                && self.cut_for.as_deref() != Some(held.job.key.as_str())
            {
                self.cut_for = Some(held.job.key.clone());
                *state = Some(cut_section(held, line, dir, template));
                changed = true;
            }
            return (None, changed);
        }
        // No grid for this frame yet: say so, and build one unless one is
        // on its way (a build for another frame finishes first).
        if let Some(line) = &self.line
            && state.as_ref().is_none_or(|s| s.status != Status::Building)
        {
            *state = Some(section(line.from, line.to, Status::Building, String::new(), template));
            changed = true;
        }
        if self.building.is_some() {
            return (None, changed);
        }
        self.building = Some(job.key.clone());
        (Some(job), changed)
    }

    fn answer_empty(&mut self, status: Status, message: &str) {
        for w in self.waiting.drain(..) {
            let profile = Profile::new(w.lat, w.lon, status, message.to_owned());
            let _ = w.reply.try_send(crate::line(&Message::Profile(&profile)));
        }
    }

    fn empty_section(
        &mut self,
        state: &mut Option<SectionState>,
        status: Status,
        message: String,
        template: &crate::protocol::Frame,
    ) -> bool {
        let Some(line) = &self.line else { return false };
        let next = section(line.from, line.to, status, message, template);
        if state.as_ref() == Some(&next) {
            return false;
        }
        self.cut_for = None;
        *state = Some(next);
        true
    }

    /// Whether a grid is held (tests, logs).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn holds(&self) -> bool {
        self.held.is_some()
    }
}

/// Cut `line` from the held grid and publish its texture.
fn cut_section(held: &Held, line: &Line, dir: &Path, template: &crate::protocol::Frame) -> SectionState {
    let cut = held.grid.cut(line.from, line.to);
    let mut out = section(line.from, line.to, Status::Ready, String::new(), template);
    out.frame_id.clone_from(&held.job.frame_id);
    out.scan_time.clone_from(&held.job.scan_time);
    out.columns = cut.columns as u32;
    out.length_km = (cut.length_m / 100.0).round() / 10.0;
    out.radars = cut.radars;
    let published = crate::gray_png(cut.columns as u32, LEVELS as u32, &cut.codes)
        .and_then(|png| crate::publish(dir, "section", &held.job.station.id, &png));
    match published {
        Ok(path) => out.texture = path,
        Err(e) => {
            out.status = Status::Empty;
            out.message = format!("The section could not be published: {e}");
        }
    }
    log(
        &held.job.station.id,
        format_args!(
            "cut {} columns, {:.1} km, for {} from {} radars",
            out.columns,
            out.length_km,
            held.job.scan_time,
            out.radars.len()
        ),
    );
    out
}
