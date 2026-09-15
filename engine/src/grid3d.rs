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
/// Mercator, about 2 km of ground at 60° N. The Nordic grid is then 836 ×
/// 1149 columns, 77 MB; at 2,000 m its codes, counts and radar bits would be
/// 92 MB each and its column masks 31 MB, ~307 MB (`coord/log/S24c.md`).
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
        Stack { beams, spans, edge }
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
            level_span(centre - w, centre + w)
                .map_or(NO_SPAN, |(lo, hi)| lo as u16 | (hi as u16) << 8)
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
    (code >= 2).then(|| {
        (f64::from(code) - crate::composite::OFFSET as f64) / crate::composite::SCALE as f64
    })
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
pub fn check_line(
    from: Option<Point>,
    to: Option<Point>,
) -> Result<Option<(Point, Point)>, String> {
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
    /// The frame the section was last cut for (`None`: cut it again).
    cut_for: Option<String>,
    /// A build that found nothing for its key, and why.
    failed: Option<(String, String)>,
    waiting: Vec<Waiting>,
    pub wake: Arc<Notify>,
}

/// What `state.section` looks like while a cut is being made.
fn section(
    from: Point,
    to: Point,
    status: Status,
    message: String,
    template: &crate::protocol::Frame,
) -> SectionState {
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
                log(
                    &site,
                    format_args!(
                        "nothing to cut for {}: {} radars, none stored",
                        job.scan_time,
                        stats.missing.len()
                    ),
                );
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
        let key = current
            .as_ref()
            .ok()
            .and_then(|j| j.as_ref())
            .map(|j| j.key.clone());
        if let Some(held) = &self.held {
            let stale = key.as_deref() != Some(held.job.key.as_str());
            if stale || held.used.elapsed() >= KEEP {
                log(
                    &held.job.station.id,
                    format_args!(
                        "grid for {} dropped ({}); {}",
                        held.job.scan_time,
                        if stale {
                            "a newer frame"
                        } else {
                            "unused for 3 minutes"
                        },
                        memory()
                    ),
                );
                self.held = None;
            }
        }
        if self
            .failed
            .as_ref()
            .is_some_and(|(k, _)| key.as_deref() != Some(k.as_str()))
        {
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
                changed |= self.empty_section(
                    state,
                    Status::Building,
                    "Waiting for the first frame.".into(),
                    template,
                );
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
            // Another frame at the same time (a product switch: the same
            // radars and volumes) keeps the grid; what is cut names it.
            if held.job.frame_id != job.frame_id {
                held.job.frame_id = job.frame_id;
                held.job.scan_time = job.scan_time;
            }
            for w in self.waiting.drain(..) {
                let profile = Profile::of(&held.grid, &held.job, w.lat, w.lon);
                let _ = w.reply.try_send(crate::line(&Message::Profile(&profile)));
            }
            if let Some(line) = &self.line
                && self.cut_for.as_deref() != Some(held.job.frame_id.as_str())
            {
                self.cut_for = Some(held.job.frame_id.clone());
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
            *state = Some(section(
                line.from,
                line.to,
                Status::Building,
                String::new(),
                template,
            ));
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
fn cut_section(
    held: &Held,
    line: &Line,
    dir: &Path,
    template: &crate::protocol::Frame,
) -> SectionState {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sweep::{Ray, Sweep};
    use serde_json::Value;
    use std::io::Read;

    const KEY: &str = include_str!("../tests/sections/tower-key.json");
    const VOLUMES: &[u8] = include_bytes!("../tests/sections/tower-volumes.zz");
    const T: i64 = 1_789_992_000_000;

    fn key() -> Value {
        serde_json::from_str(KEY).unwrap()
    }

    /// The key's two radars as stations.
    fn stations(key: &Value) -> Vec<Station> {
        key["radars"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| Station {
                id: r["id"].as_str().unwrap().to_owned(),
                name: r["id"].as_str().unwrap().to_owned(),
                lat: r["lat"].as_f64().unwrap(),
                lon: r["lon"].as_f64().unwrap(),
                alt_m: r["altM"].as_f64().unwrap(),
                kind: SiteKind::Polar,
                range_km: r["rangeKm"].as_f64().unwrap(),
                ..Station::default()
            })
            .collect()
    }

    /// The key's volumes, as the engine decodes a volume: per radar its
    /// scans, 360 rays at azimuth k + 0.5.
    fn volumes(key: &Value) -> Vec<Vec<Tilt>> {
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(VOLUMES)
            .read_to_end(&mut raw)
            .unwrap();
        let angles: Vec<f64> = key["angles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_f64().unwrap())
            .collect();
        let gates = key["gates"].as_u64().unwrap() as usize;
        let per_radar = angles.len() * 360 * gates;
        assert_eq!(raw.len(), per_radar * 2);
        (0..2)
            .map(|r| {
                angles
                    .iter()
                    .enumerate()
                    .map(|(t, &deg)| {
                        let base = r * per_radar + t * 360 * gates;
                        Tilt {
                            elangle: deg,
                            sweep: Sweep {
                                rays: (0..360)
                                    .map(|a| Ray {
                                        azimuth_deg: a as f32 + 0.5,
                                        elevation_deg: deg as f32,
                                        time_ms: T,
                                        codes: raw[base + a * gates..base + (a + 1) * gates]
                                            .to_vec(),
                                    })
                                    .collect(),
                                start_ms: T,
                                end_ms: T + 20_000,
                                gates: gates as u16,
                                first_gate_m: key["firstGateM"].as_u64().unwrap() as u32,
                                gate_spacing_m: key["gateSpacingM"].as_u64().unwrap() as u32,
                                scale: 2.0,
                                offset: 66.0,
                                code1_status: crate::sweep::OUTSIDE_COVERAGE,
                            },
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// The tower's grid, filled by the engine from the key's volumes.
    fn tower() -> (Grid, Stats, Vec<Station>) {
        let key = key();
        let sites = stations(&key);
        let set = Set {
            sites: sites
                .iter()
                .map(|s| SiteReach {
                    id: s.id.clone(),
                    reach_km: s.range_km,
                })
                .collect(),
            rule: Rule::Lowest,
            height_m: None,
            above: None,
        };
        let layout = Layout::new(&set, &sites).unwrap();
        let mut volumes: Vec<Option<Vec<Tilt>>> = volumes(&key).into_iter().map(Some).collect();
        let (grid, stats) = fill(&layout, |i| Some((volumes[i].take()?, None)));
        (grid, stats, sites)
    }

    fn point(v: &Value) -> Point {
        Point {
            lat: v["lat"].as_f64().unwrap(),
            lon: v["lon"].as_f64().unwrap(),
        }
    }

    fn names(v: &Value) -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_owned())
            .collect()
    }

    /// The answer key (plan §S24c tests): a 10 km tower seen by two
    /// synthetic radars; its cuts and profiles computed independently in
    /// numpy (`scripts/section-key.py`) from the rule in `docs/protocol.md`,
    /// the engine's own placement and fill compared cell by cell.
    #[test]
    fn a_tower_seen_by_two_radars_matches_the_numpy_key() {
        let key = key();
        let (grid, stats, _) = tower();
        assert_eq!(stats.used, ["twra", "twrb"]);
        assert!(stats.missing.is_empty());
        assert_eq!(stats.crowded, 0);
        let mut cells = 0;
        for cut in key["cuts"].as_array().unwrap() {
            let name = cut["name"].as_str().unwrap();
            let ours = grid.cut(point(&cut["from"]), point(&cut["to"]));
            assert_eq!(
                ours.columns as u64,
                cut["columns"].as_u64().unwrap(),
                "{name}"
            );
            assert!(
                (ours.length_m - cut["lengthM"].as_f64().unwrap()).abs() < 1e-3,
                "{name}"
            );
            let expected: Vec<u8> = cut["codes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_u64().unwrap() as u8)
                .collect();
            let off: Vec<usize> = (0..expected.len())
                .filter(|&i| ours.codes[i] != expected[i])
                .collect();
            assert!(
                off.is_empty(),
                "{name}: {} of {} cells differ, first at {:?}",
                off.len(),
                expected.len(),
                off.first()
            );
            assert_eq!(ours.radars, names(&cut["radars"]), "{name}");
            cells += expected.len();
        }
        for p in key["profiles"].as_array().unwrap() {
            let name = p["name"].as_str().unwrap();
            let column = grid.column(p["lat"].as_f64().unwrap(), p["lon"].as_f64().unwrap());
            if p["status"] == "outside" {
                assert!(column.is_none(), "{name}");
                continue;
            }
            let column = column.unwrap();
            assert_eq!(column.radars, names(&p["radars"]), "{name}");
            for (k, (level, want)) in column
                .levels
                .iter()
                .zip(p["levels"].as_array().unwrap())
                .enumerate()
            {
                assert_eq!(
                    u64::from(level.0),
                    want["code"].as_u64().unwrap(),
                    "{name} level {k} code"
                );
                assert_eq!(
                    u64::from(level.1),
                    want["samples"].as_u64().unwrap(),
                    "{name} level {k} samples"
                );
                assert_eq!(level.2, names(&want["radars"]), "{name} level {k} radars");
            }
        }
        assert!(cells > 9_000, "{cells} section cells compared");
    }

    /// What the tower looks like, whatever the key says: the storm up to
    /// the highest beam inside it, seen by both radars; clear air around
    /// it; a no-data sector counts nothing; a long line keeps 300 columns.
    #[test]
    fn a_towers_profile_and_cut_look_like_a_tower() {
        let (grid, _, sites) = tower();
        let job = Job::new(
            &sites[0],
            &Set::default(),
            &sites,
            "twra-x",
            "2026-09-15T12:00:00Z",
            T,
        )
        .unwrap();
        let centre = Profile::of(&grid, &job, 59.25, 14.2);
        assert_eq!(centre.status, Status::Ready);
        assert_eq!(centre.radars, ["twra", "twrb"]);
        assert_eq!(centre.levels[1].dbz, Some(50.0));
        assert!(centre.levels[1].samples >= 2);
        assert_eq!(centre.levels[1].radars, ["twra", "twrb"]);
        let top = centre.echo_top_m.unwrap();
        assert!((8_000..=10_500).contains(&top), "echo top {top} m");
        // Above the tower the beams see clear air (0), or nothing at all.
        assert!(centre.levels[23].dbz.is_none());
        let clear = Profile::of(&grid, &job, 59.0, 14.0);
        assert!(clear.levels[0].samples > 0 && clear.levels[0].dbz.is_none());
        assert_eq!(clear.echo_top_m, None);
        let outside = Profile::of(&grid, &job, 65.0, 20.0);
        assert_eq!(outside.status, Status::Outside);
        // Under B's no-data sector (bearing 205°, 50 km from B) only A counts.
        let blocked = Profile::of(&grid, &job, 58.59, 14.63);
        assert_eq!(blocked.radars, ["twra"]);
        assert!(
            blocked
                .levels
                .iter()
                .all(|l| !l.radars.contains(&"twrb".to_owned()))
        );
        assert!(blocked.levels[2].samples > 0);
        let long = grid.cut(
            Point {
                lat: 57.0,
                lon: 10.0,
            },
            Point {
                lat: 62.5,
                lon: 18.0,
            },
        );
        assert_eq!(long.columns, MAX_COLUMNS);
        assert!(long.length_m > 600_000.0);
        // Row 0 is the top level: the tower's cut has its echo at the bottom.
        let cut = grid.cut(
            Point {
                lat: 59.25,
                lon: 13.6,
            },
            Point {
                lat: 59.25,
                lon: 14.8,
            },
        );
        let bottom = &cut.codes[(LEVELS - 2) * cut.columns..(LEVELS - 1) * cut.columns];
        assert!(bottom.contains(&166));
        assert!(!cut.codes[..cut.columns].contains(&166));
    }

    #[test]
    fn levels_are_touched_where_the_beam_reaches_inside_them() {
        assert_eq!(level_span(100.0, 400.0), Some((0, 0)));
        assert_eq!(level_span(400.0, 600.0), Some((0, 1)));
        assert_eq!(level_span(500.0, 1000.0), Some((1, 1)));
        assert_eq!(level_span(-300.0, 20.0), Some((0, 0)));
        assert_eq!(level_span(-300.0, 0.0), None);
        assert_eq!(level_span(11_900.0, 13_000.0), Some((23, 23)));
        assert_eq!(level_span(12_000.0, 13_000.0), None);
        assert_eq!(columns_for(1.0), 1);
        assert_eq!(columns_for(219_600.0), 110);
        assert_eq!(columns_for(600_000.0), 300);
        assert_eq!(columns_for(900_000.0), 300);
    }

    #[test]
    fn a_section_needs_two_points_in_range_two_kilometres_apart() {
        let a = Point {
            lat: 58.0,
            lon: 12.0,
        };
        let b = Point {
            lat: 58.1,
            lon: 12.0,
        };
        assert_eq!(check_line(None, None), Ok(None));
        assert_eq!(check_line(Some(a), Some(b)), Ok(Some((a, b))));
        assert!(check_line(Some(a), None).unwrap_err().contains("both"));
        assert!(
            check_line(
                Some(a),
                Some(Point {
                    lat: 91.0,
                    lon: 0.0
                })
            )
            .unwrap_err()
            .contains("lat")
        );
        let near = Point {
            lat: 58.01,
            lon: 12.0,
        };
        assert!(
            check_line(Some(a), Some(near))
                .unwrap_err()
                .contains("2 km")
        );
    }

    fn built(job: &Job) -> Result<Built, String> {
        let (grid, stats, _) = tower();
        Ok(Built {
            key: job.key.clone(),
            grid: Some(grid),
            stats,
            seconds: 0.0,
        })
    }

    /// A section's life (S24b's M1 applied): built on demand, cut, re-cut
    /// for a newer frame from a new grid (the old one dropped first), gone
    /// with the client that set it; a profile answered to its sender; a
    /// frame with nothing stored is said once, not rebuilt every second.
    #[test]
    fn a_section_lives_with_its_client_and_follows_the_newest_frame() {
        let dir = std::env::temp_dir().join(format!("omastorm-sections-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let template: crate::protocol::Frame =
            serde_json::from_str(include_str!("../data/fixture.json")).unwrap();
        let (_, _, sites) = tower();
        let job = |start: i64| {
            Job::new(
                &sites[1],
                &Set::default(),
                &sites,
                "twrb-x",
                "2026-09-15T12:00:00Z",
                start,
            )
            .unwrap()
        };
        let mut sections = Sections::default();
        let mut state = None;
        // Nothing asked: nothing built.
        assert!(
            sections
                .pass(Ok(Some(job(T))), &dir, &template, &mut state)
                .0
                .is_none()
        );
        let line = (
            Point {
                lat: 59.25,
                lon: 13.6,
            },
            Point {
                lat: 59.25,
                lon: 14.8,
            },
        );
        assert!(sections.set_line(Some(line), 7, &mut state, &template));
        assert_eq!(state.as_ref().unwrap().status, Status::Building);
        let (asked, _) = sections.pass(Ok(Some(job(T))), &dir, &template, &mut state);
        let asked = asked.expect("a grid to build");
        assert!(
            sections
                .pass(Ok(Some(job(T))), &dir, &template, &mut state)
                .0
                .is_none(),
            "one build at a time"
        );
        sections.built(asked.clone(), built(&asked));
        let (_, changed) = sections.pass(Ok(Some(job(T))), &dir, &template, &mut state);
        assert!(changed);
        let ready = state.clone().unwrap();
        assert_eq!(ready.status, Status::Ready);
        assert_eq!(ready.columns, 35);
        assert_eq!(ready.radars, ["twra", "twrb"]);
        assert!(crate::protocol::is_texture_path(&ready.texture));
        assert!(dir.join(&ready.texture).exists());
        // A profile, answered on its sender's queue at once.
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        sections.ask(9, tx, 59.25, 14.2);
        sections.pass(Ok(Some(job(T))), &dir, &template, &mut state);
        let reply: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(reply["type"], "profile");
        assert_eq!(reply["status"], "ready");
        assert_eq!(reply["levels"].as_array().unwrap().len(), LEVELS);
        assert_eq!(reply["levels"][1]["dbz"], 50.0);
        // A newer frame: the grid is dropped before the next is built.
        let newer = job(T + 300_000);
        let (next, _) = sections.pass(Ok(Some(newer.clone())), &dir, &template, &mut state);
        assert!(!sections.holds());
        assert_eq!(next.unwrap().key, newer.key);
        assert_eq!(state.as_ref().unwrap().status, Status::Building);
        // Nothing stored for it: said once, not asked for again.
        sections.built(
            newer.clone(),
            Ok(Built {
                key: newer.key.clone(),
                grid: None,
                stats: Stats::default(),
                seconds: 0.0,
            }),
        );
        let (again, _) = sections.pass(Ok(Some(newer.clone())), &dir, &template, &mut state);
        assert!(again.is_none());
        assert_eq!(state.as_ref().unwrap().status, Status::Empty);
        assert!(state.as_ref().unwrap().message.contains("tilt store"));
        // Another client leaving changes nothing; the owner leaving drops it.
        assert!(!sections.client_left(9, &mut state));
        assert!(sections.client_left(7, &mut state));
        assert!(state.is_none());
        assert!(
            sections
                .pass(Ok(Some(newer)), &dir, &template, &mut state)
                .0
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn memory_kb(field: &str) -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .find(|l| l.starts_with(field))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|kb| kb.parse().ok())
            .unwrap_or(0)
    }

    /// Measurement, not a check (ignored): the Nordic grid from a copy of a
    /// tilt store at a fixed frame time, in release: fill seconds and memory,
    /// then a cut across the column with the highest echo top.
    /// `OMASTORM_BENCH_STORE=<copy> OMASTORM_BENCH_TIME=2026-09-15T12:00:00Z
    /// [OMASTORM_BENCH_OUT=<dir>] cargo test --release grid3d::tests::a_real
    /// -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn a_real_nordic_grid_from_a_tilt_store() {
        let dir = std::env::var("OMASTORM_BENCH_STORE").expect("OMASTORM_BENCH_STORE");
        let when = std::env::var("OMASTORM_BENCH_TIME")
            .unwrap_or_else(|_| "2026-09-15T12:00:00Z".to_owned());
        let t = chrono::DateTime::parse_from_rfc3339(&when)
            .unwrap()
            .timestamp_millis();
        let store = crate::tilts::Store::open(dir.into(), u64::MAX).unwrap();
        let sites = crate::providers::table().sites;
        let nordic = sites.iter().find(|s| s.id == "nordic").unwrap();
        let rss_before = memory_kb("VmRSS:");
        let job = Job::new(nordic, &Set::default(), &sites, "nordic-bench", &when, t).unwrap();
        let built = job.build_from(&store).unwrap();
        let (peak, held) = (memory_kb("VmHWM:"), memory_kb("VmRSS:"));
        let grid = built.grid.expect("a grid");
        let stats = &built.stats;
        // The column with the highest echo (≥ 18 dBZ) top.
        let mut best = (0usize, 0usize);
        for column in 0..grid.cols * grid.rows {
            let cells = column * LEVELS..(column + 1) * LEVELS;
            let top = cells
                .clone()
                .rev()
                .find(|&c| grid.counts[c] > 0 && dbz(grid.codes[c]).is_some_and(|v| v >= ETOP_DBZ))
                .map_or(0, |c| c - cells.start + 1);
            if top > best.1 {
                best = (column, top);
            }
        }
        let (r, c) = (best.0 / grid.cols, best.0 % grid.cols);
        let x = ((grid.west + c as i64) * FACTOR + 1) as f64 * PIXEL_M;
        let y = ((grid.top - r as i64) * FACTOR + 1) as f64 * PIXEL_M;
        let lon = (x / crate::composite::MERCATOR_R).to_degrees();
        let lat = crate::composite::mercator_lat(y);
        let (wlat, wlon) = crate::terrain::destination(lat, lon, 270.0, 150_000.0);
        let (elat, elon) = crate::terrain::destination(lat, lon, 90.0, 150_000.0);
        let started = Instant::now();
        let cut = grid.cut(
            Point {
                lat: wlat,
                lon: wlon,
            },
            Point {
                lat: elat,
                lon: elon,
            },
        );
        let cut_ms = started.elapsed().as_secs_f64() * 1000.0;
        let profile = Profile::of(&grid, &job, lat, lon);
        let summary = serde_json::json!({
            "time": when,
            "radarsUsed": stats.used.len(),
            "missing": stats.missing,
            "fillSeconds": built.seconds,
            "grid": [grid.cols, grid.rows, LEVELS],
            "gridMB": grid.bytes() as f64 / 1e6,
            "samples": stats.samples,
            "crowdedColumns": stats.crowded,
            "rssBeforeMB": rss_before / 1024,
            "peakMB": peak / 1024,
            "rssHoldingMB": held / 1024,
            "tallest": {"lat": lat, "lon": lon, "echoTopM": profile.echo_top_m, "radars": profile.radars},
            "cut": {"from": [wlat, wlon], "to": [elat, elon], "columns": cut.columns,
                    "ms": cut_ms, "radars": cut.radars},
        });
        eprintln!("{summary:#}");
        drop(grid);
        eprintln!("after drop: RSS {} MB", memory_kb("VmRSS:") / 1024);
        if let Ok(out) = std::env::var("OMASTORM_BENCH_OUT") {
            let out = std::path::PathBuf::from(out);
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.join("bench.json"), format!("{summary:#}\n")).unwrap();
            let png = crate::gray_png(cut.columns as u32, LEVELS as u32, &cut.codes).unwrap();
            std::fs::write(out.join("bench-cut-codes.png"), png).unwrap();
            let reply = crate::line(&Message::Profile(&profile));
            std::fs::write(out.join("bench-profile.json"), reply).unwrap();
        }
        assert!(built.seconds < 3.0, "fill {:.2} s", built.seconds);
    }

    /// Another frame at the same time (a product switch on the same station:
    /// same radars, same volumes) keeps the grid and cuts the section again,
    /// naming the new frame; a profile names it too.
    #[test]
    fn a_new_frame_at_the_same_time_is_cut_again_from_the_same_grid() {
        let dir =
            std::env::temp_dir().join(format!("omastorm-sections-same-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let template: crate::protocol::Frame =
            serde_json::from_str(include_str!("../data/fixture.json")).unwrap();
        let (_, _, sites) = tower();
        let job = |frame: &str| {
            Job::new(
                &sites[1],
                &Set::default(),
                &sites,
                frame,
                "2026-09-15T12:00:00Z",
                T,
            )
            .unwrap()
        };
        let mut sections = Sections::default();
        let mut state = None;
        let line = (
            Point {
                lat: 59.25,
                lon: 13.6,
            },
            Point {
                lat: 59.25,
                lon: 14.8,
            },
        );
        sections.set_line(Some(line), 7, &mut state, &template);
        let asked = sections
            .pass(Ok(Some(job("twrb-cmax"))), &dir, &template, &mut state)
            .0
            .unwrap();
        sections.built(asked.clone(), built(&asked));
        sections.pass(Ok(Some(job("twrb-cmax"))), &dir, &template, &mut state);
        let first = state.clone().unwrap();
        assert_eq!(
            (first.status, first.frame_id.as_str()),
            (Status::Ready, "twrb-cmax")
        );
        let (again, changed) = sections.pass(Ok(Some(job("twrb-e0"))), &dir, &template, &mut state);
        assert!(again.is_none(), "the same grid, no build");
        assert!(sections.holds());
        assert!(changed);
        let second = state.clone().unwrap();
        assert_eq!(
            (second.status, second.frame_id.as_str()),
            (Status::Ready, "twrb-e0")
        );
        assert_ne!(second.texture, first.texture, "a new cut, a new name");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        sections.ask(9, tx, 59.25, 14.2);
        sections.pass(Ok(Some(job("twrb-e0"))), &dir, &template, &mut state);
        let reply: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(reply["frameId"], "twrb-e0");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
