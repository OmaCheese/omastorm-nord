mod catalog;
mod composite;
mod grid3d;
mod loading;
mod mosaic;
mod netstats;
mod odim;
mod osm;
mod products;
use products::Want;
mod protocol;
mod providers;
mod reference;
mod smhi_live;
mod sweep;
mod terrain;
mod tiles;
mod tilts;

use catalog::Entry;
use chrono::{DateTime, Utc};
use protocol::{
    Basemap, Command, Connection, ConnectionStatus, Frame, FrameKind, FrameStatus, Geometry,
    Handshake, Hello, Message, NaturalEarth, Places, Rejection, SiteKind, SiteSelection, Source,
    State, Station, TileReady, TimelineEntry, VERSION, is_texture_path,
};
use providers::{Scan, Staleness};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, OpenOptions},
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tiles::{Ready, Set, TileKey};
// The daemon runs on tokio (DESIGN.md, engine runtime); the launcher paths
// (`ensure`, `stop`) stay on std sockets, since they are short and sequential.
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader as AsyncBufReader},
    net::UnixListener,
    sync::{
        Notify,
        mpsc::{self, Receiver, Sender},
    },
    task::{JoinHandle, spawn_blocking},
    time::{interval, sleep, timeout, timeout_at},
};

/// Development only: the path of an archived Level II volume to decode and
/// show at startup as `archived`, the way the checks and captures expect.
/// Unset, the daemon starts lean, with no frame, and waits for
/// `select_site`; nothing archived travels inside the binary.
const ARCHIVE_ENV: &str = "OMASTORM_ARCHIVE";
/// Keep-warm stations (`docs/protocol.md`, keep-warm stations): ids polled
/// while not selected, so their history is full when a client opens them.
const WARM_ENV: &str = "OMASTORM_WARM";
/// A warm poller that ended is started again at most this often.
const WARM_RETRY: Duration = Duration::from_secs(10 * 60);
/// `OMASTORM_WARM`: comma-separated station ids or aliases, each once, in
/// canonical form; unknown ids are reported and skipped.
fn parse_warm(value: &str, sites: &[Station]) -> Vec<String> {
    let mut warm: Vec<String> = Vec::new();
    for id in value.split(',').map(str::trim).filter(|id| !id.is_empty()) {
        match providers::resolve(sites, id) {
            // My mosaic is polled only while shown (S25).
            Some(s) if s.provider == providers::ProviderId::Mosaic => {
                eprintln!("{WARM_ENV}: {} is never kept warm; ignored", s.id);
            }
            Some(s) if !warm.contains(&s.id) => warm.push(s.id.clone()),
            Some(_) => {}
            None => eprintln!("{WARM_ENV}: no station {id:?}; ignored"),
        }
    }
    warm
}
const MAX_LINE: u64 = 16 * 1024;
/// A client that cannot take one message in this long has stalled; its
/// connection is closed rather than letting it hold anything up.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a texture outlives its last reference from `state` (`docs/protocol.md`).
const RETIRE_AFTER: Duration = Duration::from_secs(30);
/// Messages queued for one client. A `tiles_needed` answer is up to 64
/// `tile_ready` lines in a burst, so the queue holds a couple of those; a
/// client that still falls behind is dropped by the next broadcast.
const QUEUE: usize = 128;
/// The credit on an archived Level II frame (the checks' KTLX, DEC-10).
const LEVEL_II_CREDIT: &str = "NOAA NEXRAD Level II";
/// Playback advances one frame per tick and loops (DESIGN.md, timeline).
const PLAY_LOOP: Duration = Duration::from_secs(10);
const PLAY_STEP_MIN: Duration = Duration::from_millis(250);
const PLAY_STEP_MAX: Duration = Duration::from_millis(1000);
/// Following hands off when a pan settles with another station closer than
/// this fraction of the current one's distance to the view centre, and
/// closer by at least `HANDOFF_MARGIN_KM` (DESIGN.md, site navigation).
/// Relative, so the dead band scales with the spacing: about a twentieth
/// of the distance between two stations on either side of their midpoint.
const HANDOFF_RATIO: f64 = 0.8;
const HANDOFF_MARGIN_KM: f64 = 1.0;

/// Fingerprint of the running executable, set once in `main`.
static BUILD: OnceLock<String> = OnceLock::new();

/// Hash the executable image this process is running. That covers every
/// source file, embedded asset, dependency, and compiler version without
/// enumerating them, so adding a module can never let a stale daemon pass as
/// current. Read through `/proc/self/exe`, which still opens the original
/// image after Cargo replaces the file on disk; the daemon hashes itself at
/// startup, before any rebuild.
fn fingerprint() -> io::Result<String> {
    let image = fs::read("/proc/self/exe").or_else(|_| fs::read(env::current_exe()?))?;
    let mut hash = DefaultHasher::new();
    image.hash(&mut hash);
    Ok(format!("{:016x}", hash.finish()))
}
fn build_id() -> &'static str {
    BUILD.get().expect("fingerprint is computed before use")
}
/// The embedded station snapshot (`engine/data/sites.json`: the 12 radars
/// `scripts/fetch-smhi-sites.sh` writes and the 29 of
/// `scripts/fetch-ord-sites.sh`) plus the national composite, which neither
/// script knows about (`composite.rs`).
fn site_table() -> providers::Table {
    providers::table()
}
fn hello() -> Hello {
    let table = site_table();
    Hello {
        v: VERSION,
        engine: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        build: build_id().to_owned(),
        products: &products::VOCABULARY,
        sites: table.sites.into_iter().map(products::site_entry).collect(),
        sites_source: table.source,
        sites_retrieved: table.retrieved,
        sites_notes: table.notes,
        reference_sites: reference::sites(),
        mosaic: mosaic::info(),
        sections: grid3d::info(),
    }
}
fn fixture_frame() -> Frame {
    serde_json::from_str(include_str!("../data/fixture.json")).unwrap()
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
/// Milliseconds since the epoch as the protocol writes times.
fn iso(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
fn compact(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|t| t.format("%Y%m%dT%H%M%SZ").to_string())
        .unwrap_or_default()
}
/// What a client can step through (`state.timeline`, `docs/protocol.md`):
/// the station's complete frames from the catalog, oldest first, and the
/// sweep in progress after them while one is painting. `shown` is the frame
/// the user stepped or sought to; `None` follows the newest, so each new
/// sweep replaces the picture (DESIGN.md, live sweeps as built).
#[derive(Default)]
struct Timeline {
    stored: Vec<Entry>,
    partial: Option<Entry>,
    shown: Option<String>,
}
impl Timeline {
    fn new(stored: Vec<Entry>) -> Self {
        Timeline {
            stored,
            partial: None,
            shown: None,
        }
    }
    fn len(&self) -> usize {
        self.stored.len() + usize::from(self.partial.is_some())
    }
    fn following(&self) -> bool {
        self.shown.is_none()
    }
    fn index_of(&self, id: &str) -> Option<usize> {
        self.stored.iter().position(|e| e.id == id).or_else(|| {
            self.partial
                .as_ref()
                .filter(|p| p.id == id)
                .map(|_| self.stored.len())
        })
    }
    fn id_at(&self, index: usize) -> Option<&str> {
        self.stored
            .get(index)
            .or(self.partial.as_ref().filter(|_| index == self.stored.len()))
            .map(|e| e.id.as_str())
    }
    /// Whether `index` names the sweep in progress rather than a stored frame.
    fn is_partial(&self, index: usize) -> bool {
        self.partial.is_some() && index == self.stored.len()
    }
    /// The frame on screen, `None` only while the timeline is empty.
    fn position(&self) -> Option<usize> {
        match &self.shown {
            None => self.len().checked_sub(1),
            Some(id) => self.index_of(id),
        }
    }
    /// Show the frame at `index`; landing on the newest follows again. The
    /// index to show when the position changed.
    fn pin(&mut self, index: usize) -> Option<usize> {
        let from = self.position();
        self.shown = if index + 1 == self.len() {
            None
        } else {
            self.id_at(index).map(str::to_owned)
        };
        (from != Some(index)).then_some(index)
    }
    /// `[` and `]`: `delta` frames from the one on screen, stopping at the ends.
    fn step(&mut self, delta: i64) -> Option<usize> {
        let from = self.position()? as i64;
        let to = (from + delta).clamp(0, self.len() as i64 - 1) as usize;
        self.pin(to)
    }
    /// Show frame `id`, or say it is not here.
    fn seek(&mut self, id: &str) -> Result<Option<usize>, ()> {
        let index = self.index_of(id).ok_or(())?;
        Ok(self.pin(index))
    }
    /// Playback: the next complete frame, wrapping to the oldest after the
    /// newest; nothing to loop over with fewer than two.
    fn advance(&mut self) -> Option<usize> {
        if self.stored.len() < 2 {
            return None;
        }
        let from = self.position().unwrap_or(0);
        let to = if from + 1 >= self.stored.len() {
            0
        } else {
            from + 1
        };
        self.pin(to)
    }
    /// A sweep began or grew: it is the newest entry until it completes or
    /// the next volume replaces it.
    fn begin(&mut self, entry: Entry) {
        self.partial = Some(entry);
    }
    /// A complete frame joined the catalog: it takes its place in time
    /// order and the ring drops the oldest past `RING`. True when the frame
    /// the user was pinned to fell off the ring; the pin moves to the
    /// oldest, and the caller shows it.
    fn complete(&mut self, entry: Entry) -> bool {
        self.partial = None;
        self.insert(entry)
    }
    /// How many complete frames the loop has.
    fn complete_count(&self) -> usize {
        self.stored.len()
    }
    /// A complete frame from an earlier volume (a backfill) or the live
    /// one, in time order; the sweep in progress is untouched. True when the
    /// pinned frame fell off the ring.
    fn insert(&mut self, entry: Entry) -> bool {
        self.stored.retain(|e| e.id != entry.id);
        let at = self
            .stored
            .partition_point(|e| e.start_ms <= entry.start_ms);
        self.stored.insert(at, entry);
        let excess = self.stored.len().saturating_sub(catalog::RING);
        self.stored.drain(..excess);
        match &self.shown {
            Some(id) if self.index_of(id).is_none() => {
                self.shown = self.stored.first().map(|e| e.id.clone());
                true
            }
            _ => false,
        }
    }
    /// `state.timeline`: each entry with its stable textures (a catalogued
    /// frame's; none for the sweep in progress or an archived frame), and
    /// its placement where it differs from `shown`'s, the frame on screen
    /// (`docs/protocol.md`, timeline textures).
    fn entries(&self, shown: &Frame) -> Vec<TimelineEntry> {
        let reference = shown.placement();
        let entry = |e: &Entry, status| {
            let record = e
                .record
                .as_ref()
                .filter(|_| status == FrameStatus::Complete);
            let (texture, azimuth_lut) = record.map(stable_paths).unwrap_or_default();
            TimelineEntry {
                id: e.id.clone(),
                scan_time: e.scan_time.clone(),
                status,
                texture,
                azimuth_lut,
                codes: record.map(codes_path).unwrap_or_default(),
                placement: record
                    .map(|r| r.frame.placement())
                    .filter(|p| *p != reference),
            }
        };
        self.stored
            .iter()
            .map(|e| entry(e, FrameStatus::Complete))
            .chain(self.partial.iter().map(|e| entry(e, FrameStatus::Partial)))
            .collect()
    }
}
/// The sweep in progress, held in memory so a client that stepped back can
/// return to it (End) between chunks without a texture on disk.
struct Pending {
    frame: Frame,
    texture: Vec<u8>,
    lut: Vec<u8>,
    /// Collection time of its newest radial, milliseconds since the epoch:
    /// the freshest evidence that the feed is up while it paints.
    end_ms: i64,
}
/// A live sweep encoded and, if complete, catalogued, on its way to the
/// timeline: the frame, its two textures, and the collection times of its
/// first and newest radial.
struct Arrival {
    frame: Frame,
    texture: Vec<u8>,
    lut: Vec<u8>,
    start_ms: i64,
    end_ms: i64,
    /// The frame as the catalog lists it, once complete and stored.
    stored: Option<Entry>,
}
/// The condition of a reachable feed from the age of the newest radial the
/// station has published: `Ok`, then `Stale`, then `Unavailable`. `Loading`
/// and `Offline` are not judged by age: a switch or the poller set them and
/// the next sweep clears them. `None` (no radial yet) keeps the current
/// condition.
/// The thresholds are the station's provider's (`providers::Staleness`):
/// SMHI's 15 and 30 minutes (DEC-9).
fn feed_condition(
    current: ConnectionStatus,
    evidence_age: Option<u64>,
    staleness: Staleness,
) -> ConnectionStatus {
    match (current, evidence_age) {
        (ConnectionStatus::Loading | ConnectionStatus::Offline, _) | (_, None) => current,
        (_, Some(age)) if age >= staleness.unavailable.as_secs() => ConnectionStatus::Unavailable,
        (_, Some(age)) if age >= staleness.stale.as_secs() => ConnectionStatus::Stale,
        _ => ConnectionStatus::Ok,
    }
}
/// Restart the live poller when it has exited, or when the newest radial
/// is old enough that the UI already says UNAVAILABLE and we have not
/// tried discovery since. A cooldown equal to that age keeps a silent
/// station from being rediscovered every cleanup tick.
fn should_restart_live(
    poller_dead: bool,
    evidence_age: Option<u64>,
    since_restart: Duration,
    staleness: Staleness,
) -> bool {
    if poller_dead {
        return true;
    }
    evidence_age.is_some_and(|age| age >= staleness.unavailable.as_secs())
        && since_restart >= staleness.unavailable
}
/// A sweep already in the catalog may clear `loading` after `select_site`.
/// Any other status stays put: rediscovery must not flash `ok`.
fn known_sweep_clears_loading(status: ConnectionStatus) -> bool {
    status == ConnectionStatus::Loading
}
/// A frame's units, palette and bounds for `want` (S24a): the storm
/// height's and the rain mass's own, else the engine's reflectivity
/// vocabulary (the fixture frame's).
fn legend_of(template: &Frame, want: Want) -> (String, Vec<String>, Vec<i32>) {
    match want.legend() {
        Some(legend) => (
            legend.units.to_owned(),
            legend.palette.iter().map(|&c| c.to_owned()).collect(),
            legend.bounds.to_vec(),
        ),
        None => (
            template.units.clone(),
            template.palette.clone(),
            template.bounds.clone(),
        ),
    }
}
/// A live station's frame from an assembled sweep: the fixture frame's
/// product, palette, and bounds (the engine's reflectivity vocabulary), the
/// sweep's geometry and times, and the station table's coordinates. Texture
/// paths are filled in by `publish_frame`. A product other than the lowest
/// scan names itself and ends the id in its variant (S20).
fn live_frame(
    template: &Frame,
    station: &Station,
    sweep: &sweep::Sweep,
    complete: bool,
    want: Want,
) -> Frame {
    let (product, product_name) = if want.is_lowest() {
        (template.product.clone(), template.product_name.clone())
    } else {
        let (id, name) = want.product();
        (id.to_owned(), name)
    };
    // S24a: the storm height and the rain mass bring their own units,
    // palette and bounds; every other product is the reflectivity's.
    let (units, palette, bounds) = legend_of(template, want);
    Frame {
        id: format!(
            "{}-{}-{}",
            station.id,
            compact(sweep.start_ms),
            want.variant()
        ),
        kind: FrameKind::Polar,
        product,
        product_name,
        units,
        elevation_deg: (sweep.elevation_deg() * 100.0).round() / 100.0,
        scan_time: iso(sweep.start_ms),
        sweep_end: iso(sweep.end_ms),
        status: if complete {
            FrameStatus::Complete
        } else {
            FrameStatus::Partial
        },
        texture: String::new(),
        azimuth_lut: String::new(),
        rays: sweep.rows(),
        gates: u32::from(sweep.gates),
        first_gate_m: sweep.first_gate_m,
        gate_spacing_m: sweep.gate_spacing_m,
        scale: sweep.scale,
        offset: sweep.offset,
        site: Geometry {
            lat: station.lat,
            lon: station.lon,
            alt_m: station.alt_m,
        },
        palette,
        bounds,
        // A height above the ground credits the terrain too (S30).
        attribution: if matches!(want, Want::CappiGround(..)) {
            format!("{}; {}", station.attribution, terrain::CREDIT)
        } else {
            station.attribution.clone()
        },
        grid: None,
    }
}
/// The frame shown while a station's first live sweep loads and nothing is
/// cached: one blank row, so the shader draws nothing, at the station's
/// coordinates, with no scan time to show.
fn empty_frame(template: &Frame, station: &Station) -> Frame {
    Frame {
        id: format!("{}-loading", station.id),
        // Always polar, the composite's too: one blank gate draws nothing.
        kind: FrameKind::Polar,
        product: template.product.clone(),
        product_name: template.product_name.clone(),
        units: template.units.clone(),
        elevation_deg: 0.0,
        scan_time: String::new(),
        sweep_end: String::new(),
        status: FrameStatus::Partial,
        texture: String::new(),
        azimuth_lut: String::new(),
        rays: 1,
        gates: 1,
        first_gate_m: template.first_gate_m,
        gate_spacing_m: template.gate_spacing_m.max(1),
        scale: 0.0,
        offset: 0.0,
        site: Geometry {
            lat: station.lat,
            lon: station.lon,
            alt_m: station.alt_m,
        },
        palette: template.palette.clone(),
        bounds: template.bounds.clone(),
        attribution: station.attribution.clone(),
        grid: None,
    }
}
/// The frame a lean daemon starts on before any `select_site`: the loading
/// placeholder for no station at all, sited at the middle of Sweden so the
/// map shows the whole set of markers until a station is chosen. `site.id`
/// is empty, so any home the UI names differs from it.
fn startup_frame(template: &Frame) -> Frame {
    let nowhere = Station {
        lat: 62.0,
        lon: 16.0,
        kind: SiteKind::Polar,
        ..Station::default()
    };
    empty_frame(template, &nowhere)
}
/// Development only: the daemon's own station table when it starts on an
/// archived volume whose station the table lacks (the NEXRAD KTLX scan the
/// checks use on purpose, DEC-10: an archived Vara volume is a table
/// station, and selecting it would go live on SMHI). The archive's station joins
/// it from the frame's geometry, so following keeps the archive's home view
/// instead of handing off to the nearest SMHI radar, and `select_site` still
/// reaches it. `hello` never lists it: the picker shows the SMHI sites alone.
fn with_archived_station(mut sites: Vec<Station>, frame: &Frame) -> Vec<Station> {
    let id = frame.id.split('-').next().unwrap_or_default();
    if !id.is_empty() && !sites.iter().any(|s| s.id == id) {
        sites.push(Station {
            id: id.to_owned(),
            name: id.to_owned(),
            state: String::new(),
            lat: frame.site.lat,
            lon: frame.site.lon,
            alt_m: frame.site.alt_m,
            kind: SiteKind::Polar,
            attribution: frame.attribution.clone(),
            ..Station::default()
        });
    }
    sites
}
/// The textures behind a loading placeholder: one blank gate, so the
/// shader draws nothing, and an azimuth lookup for it.
fn blank_textures(frame: &Frame) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let sweep = sweep::Sweep {
        rays: Vec::new(),
        start_ms: 0,
        end_ms: 0,
        gates: 1,
        first_gate_m: frame.first_gate_m,
        gate_spacing_m: frame.gate_spacing_m,
        scale: 1.0,
        offset: 0.0,
        code1_status: sweep::FOLDED,
    };
    let texture = sweep::png(1, 1, &sweep.texture(&frame.bounds, frame.palette.len()))?;
    let lut = sweep::png(3600, 1, &sweep.azimuth_lut())?;
    Ok((texture, lut))
}
/// Great-circle distance in kilometres on the radar's 6371 km sphere.
fn great_circle_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.clamp(0.0, 1.0).sqrt().asin()
}
/// Beyond this many times the nearest radar's `rangeKm`, a centre in an
/// OPERA box that takes the hand-off goes to its composite (S33, review
/// SF4: Galicia's coast, the sea off Portugal).
const HANDOFF_BEYOND_RANGE: f64 = 1.3;
/// S23's reference radars: the ones in an OPERA box are seen only through
/// its composite (S33: Portugal's), which then takes the hand-off near them.
static REFERENCE_RADARS: std::sync::LazyLock<Vec<reference::ReferenceSite>> =
    std::sync::LazyLock::new(reference::sites);
/// The station following should hand off to when the view centre settles
/// at `lat`, `lon`: the nearest table station, when it is not `current` and
/// beats it by the hysteresis rule. `None` keeps the current station, so a
/// centre between two stations does not flap. Nothing about the camera is
/// decided here: the centre is the user's.
///
/// The composite covers the whole view already, so it is never a target,
/// and while it is selected nothing is handed off to (`docs/protocol.md`,
/// `view_center`). S33: except an OPERA box that takes the hand-off
/// (`iberia`, where Portugal's radars show only through the composite):
/// with the centre inside it, it is the target where the composite alone
/// shows the radars there (review SF4): nearer one of the box's reference
/// radars than to any table radar, or beyond `HANDOFF_BEYOND_RANGE` × the
/// nearest radar's range; a place in Spain still follows a Spanish radar
/// (S32). Selected, it holds while the centre stays in its box and lets go
/// once it leaves (review SF3).
fn handoff<'a>(sites: &'a [Station], current: &str, lat: f64, lon: f64) -> Option<&'a Station> {
    let holds = |c: &providers::opera::Cut, la: f64, lo: f64| {
        (c.area.south..=c.area.north).contains(&la) && (c.area.west..=c.area.east).contains(&lo)
    };
    if let Some(held) = sites
        .iter()
        .find(|s| s.id == current && s.kind == SiteKind::Grid)
    {
        match providers::opera::cut(&held.id) {
            Some(c) if c.handoff && !holds(c, lat, lon) => {}
            _ => return None,
        }
    }
    let distance = |s: &Station| great_circle_km(lat, lon, s.lat, s.lon);
    let nearest = sites
        .iter()
        .filter(|s| s.kind == SiteKind::Polar)
        .min_by(|a, b| distance(a).total_cmp(&distance(b)));
    let composite = sites
        .iter()
        .filter(|s| s.kind == SiteKind::Grid)
        .find_map(|s| {
            providers::opera::cut(&s.id)
                .filter(|c| c.handoff && holds(c, lat, lon))
                .map(|c| (s, c))
        });
    if let Some((composite, c)) = composite {
        let radar_km = nearest.map_or(f64::INFINITY, distance);
        let beyond = nearest.is_none_or(|n| radar_km > HANDOFF_BEYOND_RANGE * n.range_km);
        let only_there = REFERENCE_RADARS
            .iter()
            .any(|r| holds(c, r.lat, r.lon) && great_circle_km(lat, lon, r.lat, r.lon) < radar_km);
        if beyond || only_there {
            return Some(composite);
        }
    }
    let nearest = nearest?;
    if nearest.id == current {
        return None;
    }
    match sites
        .iter()
        .find(|s| s.id == current && s.kind == SiteKind::Polar)
    {
        Some(held)
            if distance(nearest) >= HANDOFF_RATIO * distance(held)
                || distance(held) - distance(nearest) < HANDOFF_MARGIN_KM =>
        {
            None
        }
        _ => Some(nearest),
    }
}
fn initial_state(
    frame: Frame,
    osm: protocol::Osm,
    source: Source,
    status: ConnectionStatus,
) -> State {
    // Archived, the station is the one the frame names; lean, none yet.
    let site = match source {
        Source::Archived => frame.id.split('-').next().unwrap_or_default().to_owned(),
        Source::Live => String::new(),
    };
    State {
        v: VERSION,
        source,
        connection: Connection {
            status,
            age_seconds: 0,
        },
        site: SiteSelection {
            id: site,
            follow: true,
            locked: false,
        },
        // Filled from the `Timeline` at every snapshot.
        timeline: Vec::new(),
        frame,
        basemap: Basemap {
            ne: NaturalEarth {
                version: tiles::NE_VERSION,
            },
            osm,
        },
        playing: false,
        product: products::Choice::default(),
        mosaic: mosaic::Set::default(),
        section: None,
        loading: None,
    }
}
fn line(message: &Message) -> String {
    let mut text = serde_json::to_string(message).unwrap();
    text.push('\n');
    text
}
/// Store `value`, reporting whether anything changed.
fn set<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}
struct Shared {
    state: State,
    /// The tile masks rendered this session (`tiles.rs`).
    tiles: tiles::Store,
    /// Each client's bounded outgoing queue; `try_send` never waits, so a
    /// client that stops reading is dropped instead of blocking the others.
    clients: Vec<(u64, Sender<String>)>,
    next_client: u64,
    /// The fixture frame: the engine's reflectivity vocabulary (product,
    /// palette, bounds) that live frames share.
    template: Frame,
    /// The station table from `hello`.
    sites: Vec<Station>,
    /// The runtime directory textures are published to.
    dir: PathBuf,
    catalog: Arc<catalog::Catalog>,
    /// The poller for the selected station in live mode (`smhi_live.rs`);
    /// aborted and replaced by a site switch or a quiet-feed restart.
    live: Option<JoinHandle<()>>,
    /// Keep-warm stations (`OMASTORM_WARM`) and the pollers of those not
    /// selected, with when each was spawned.
    warm: Vec<String>,
    warm_pollers: HashMap<String, (JoinHandle<()>, Instant)>,
    /// When the poller was last spawned, so an UNAVAILABLE feed is
    /// rediscovered at most once per its provider's `unavailable` age rather
    /// than every cleanup tick.
    last_live_restart: Instant,
    events: Sender<providers::Event>,
    /// `scanTime` of the newest complete frame, milliseconds since the
    /// epoch, for `connection.ageSeconds`; `None` while there is none.
    frame_ms: Option<i64>,
    /// The frames a client can step through, and which one is on screen.
    timeline: Timeline,
    /// The sweep in progress, when the timeline lists one.
    pending: Option<Pending>,
    /// Wakes the player task when `playing` becomes true.
    wake: Arc<Notify>,
    /// The last `state` line sent, so a tick that changed nothing is not
    /// re-sent.
    last_broadcast: String,
    /// The selected station and its condition as engine.log last said.
    logged_condition: Option<(String, ConnectionStatus)>,
    /// The angle `state.product` names in degrees, when it is one angle,
    /// so a switch (through a composite, too) keeps the nearest (S20).
    product_deg: Option<f64>,
    /// While a grid station is selected (S24b), the choice the next radar
    /// gets, with its angle: the one before the grid station, or one chosen
    /// on it. `state.product` then says what the grid station shows.
    aside: Option<(products::Choice, Option<f64>)>,
    /// Sections and profiles (S24c): the grid of the newest frame, the
    /// section's line and who set it, profiles waiting.
    sections: grid3d::Sections,
    /// The load a client waits for and what `state.loading` says (S31).
    loading: loading::Tracker,
}
impl Shared {
    fn snapshot(&mut self) -> String {
        let age = self
            .frame_ms
            .map_or(0, |ms| now_ms().saturating_sub(ms).max(0) as u64 / 1000);
        self.state.connection.age_seconds = age;
        self.state.timeline = self.timeline.entries(&self.state.frame);
        // Live, the condition follows the newest radial received: the sweep
        // in progress while one paints, else the newest complete frame. A
        // half-finished cut from a station that then fell silent ages like
        // any other evidence.
        if self.state.source == Source::Live {
            self.state.connection.status = feed_condition(
                self.state.connection.status,
                self.evidence_age_secs(),
                self.staleness(),
            );
        }
        // S31: the load's progress, throttled by the tracker.
        self.state.loading = self.loading.wire(Instant::now());
        line(&Message::State(&self.state))
    }
    /// A frame catalogued before S14 has no credit: give it its station's.
    fn credit(&self, frame: &mut Frame) {
        if frame.attribution.is_empty() {
            let id = frame.id.split('-').next().unwrap_or_default();
            if let Some(station) = self.sites.iter().find(|s| s.id == id) {
                frame.attribution = station.attribution.clone();
            }
        }
    }
    /// Show `frame` with its textures published under `tex/` as a new
    /// revision: the sweep in progress, the loading placeholder, or a
    /// complete frame the catalog could not take.
    fn show(&mut self, mut frame: Frame, texture: &[u8], lut: &[u8]) -> io::Result<()> {
        self.credit(&mut frame);
        publish_frame(&self.dir, &mut frame, texture, lut)?;
        self.state.frame = frame;
        Ok(())
    }
    /// Show a catalogued frame under its stable names (`stable_paths`), the
    /// ones its timeline entry lists; nothing is read from the catalog.
    fn show_entry(&mut self, entry: &Entry) -> io::Result<()> {
        let record = entry
            .record
            .as_ref()
            .ok_or_else(|| io::Error::other(format!("{} has no catalogued textures", entry.id)))?;
        link_record(&self.dir, record)?;
        let mut frame = record.frame.clone();
        (frame.texture, frame.azimuth_lut) = stable_paths(record);
        self.credit(&mut frame);
        self.state.frame = frame;
        Ok(())
    }
    /// A catalogued frame joined the timeline: publish its files under their
    /// stable names. A failure is only logged; a client that cannot fetch
    /// the entry's textures falls back to `seek`.
    fn link(&self, entry: &Entry) {
        if let Some(record) = &entry.record
            && let Err(e) = link_record(&self.dir, record)
        {
            eprintln!("Publishing {}: {e}", entry.id);
        }
    }
    /// Show the timeline's frame at `index`: the sweep in progress from
    /// memory under a new revision, a catalogued frame under its stable
    /// names.
    fn show_position(&mut self, index: usize) -> io::Result<()> {
        if self.timeline.is_partial(index) {
            let pending = self
                .pending
                .as_ref()
                .ok_or_else(|| io::Error::other("the sweep in progress has no texture"))?;
            let mut frame = pending.frame.clone();
            publish_frame(&self.dir, &mut frame, &pending.texture, &pending.lut)?;
            self.state.frame = frame;
            return Ok(());
        }
        let entry = self
            .timeline
            .stored
            .get(index)
            .cloned()
            .ok_or_else(|| io::Error::other(format!("no frame at {index}")))?;
        self.show_entry(&entry)
    }
    /// The selected station's staleness thresholds, from its provider's
    /// cadence (an ORD station's from its country's, S32); SMHI's before any
    /// station is selected.
    fn staleness(&self) -> Staleness {
        let station = self.sites.iter().find(|s| s.id == self.state.site.id);
        match station {
            Some(s) if s.provider == providers::ProviderId::Ord => providers::ord::staleness_for(s),
            _ => {
                let provider = station.map_or(providers::ProviderId::Smhi, |s| s.provider);
                providers::spec(provider).staleness
            }
        }
    }
    /// Age of the newest radial received for the live station: the sweep
    /// in progress while one paints, else the newest complete frame.
    fn evidence_age_secs(&self) -> Option<u64> {
        let newest_ms = self
            .pending
            .as_ref()
            .map(|p| p.end_ms)
            .into_iter()
            .chain(self.frame_ms)
            .max()?;
        Some(now_ms().saturating_sub(newest_ms).max(0) as u64 / 1000)
    }
    /// S31: a load in words for `state.loading`: a radar's or a
    /// composite's (`loading::radar_name`), My mosaic's set, a composite's
    /// product (as `mosaic::run` names it).
    fn load_name(&self, station: &Station, want: Want) -> String {
        match loading::kind_of(station, want) {
            loading::Kind::Radar => loading::radar_name(station, want),
            loading::Kind::Made if station.provider == providers::ProviderId::Mosaic => {
                format!("My mosaic, {}", self.state.mosaic.product().1)
            }
            loading::Kind::Made => format!("{} {}", station.name, want.product().1),
        }
    }
    /// S31: the composite's own newest frame (`REF`), drawn under its
    /// product's first stage, its stable files published; `None` when its
    /// ring holds none.
    fn composite_under(&self, station: &Station) -> Option<Frame> {
        let listed = self
            .catalog
            .list_variant(&station.id, &Want::Lowest.variant())
            .ok()?;
        let record = listed.iter().rev().find_map(|e| e.record.as_ref())?;
        if let Err(e) = link_record(&self.dir, record) {
            eprintln!("Publishing {} under its product: {e}", record.frame.id);
            return None;
        }
        let mut frame = record.frame.clone();
        (frame.texture, frame.azimuth_lut) = stable_paths(record);
        self.credit(&mut frame);
        Some(frame)
    }
    /// Abort the current poller, if any, and start another on the selected
    /// station. Timeline and the frame on screen stay; the next *new* sweep
    /// clears `unavailable` / `offline`. `skip_known` is true on a respawn
    /// so a catalogued replay is not published again.
    fn restart_live(&mut self, why: &str, skip_known: bool) {
        let site = self.state.site.id.clone();
        if site.is_empty() || self.state.source != Source::Live {
            return;
        }
        eprintln!("{} Live {site}: {why}", iso(now_ms()));
        if let Some(task) = self.live.take() {
            task.abort();
        }
        let cached: Vec<i64> = self.timeline.stored.iter().map(|e| e.start_ms).collect();
        let station = self
            .sites
            .iter()
            .find(|s| s.id == site)
            .cloned()
            .unwrap_or_else(|| Station {
                id: site.clone(),
                ..Station::default()
            });
        let want = self.want();
        // S31: a new load, or none for the engine's own restart (its
        // backfill may still bring a history stage).
        if skip_known {
            self.loading.reset();
        } else {
            let has_frame = !self.state.frame.scan_time.is_empty();
            let kind = loading::kind_of(&station, want);
            let under =
                (kind == loading::Kind::Made && station.kind == SiteKind::Grid && !has_frame)
                    .then(|| self.composite_under(&station))
                    .flatten();
            let name = self.load_name(&station, want);
            self.loading.begin(kind, &name, has_frame, under);
        }
        self.live = Some(if station.provider == providers::ProviderId::Mosaic {
            // My mosaic polls its set's radars itself (S25).
            tokio::spawn(mosaic::poll(
                self.state.mosaic.clone(),
                self.sites.clone(),
                self.events.clone(),
                cached,
            ))
        } else if station.kind == SiteKind::Grid && !want.is_lowest() {
            // A composite's product (S24b): made from its radars, only
            // while it is shown.
            tokio::spawn(mosaic::poll_grid(
                station,
                want,
                self.sites.clone(),
                self.events.clone(),
                cached,
            ))
        } else {
            tokio::spawn(providers::poll(
                station,
                self.events.clone(),
                cached,
                skip_known,
                want,
            ))
        });
        self.last_live_restart = Instant::now();
    }
    /// The catalog ring `station` shows under `want`: the product's
    /// (`Want::variant`), or for My mosaic the set's (S25).
    fn variant_for(&self, station: &Station, want: Want) -> String {
        if station.provider == providers::ProviderId::Mosaic {
            self.state.mosaic.variant()
        } else {
            want.variant()
        }
    }
    /// The ring the selected station's timeline shows.
    fn variant(&self) -> String {
        match self.sites.iter().find(|s| s.id == self.state.site.id) {
            Some(station) => self.variant_for(station, self.want()),
            None => self.want().variant(),
        }
    }
    /// The product the selected station's poller follows and its timeline
    /// shows: `state.product` where the station makes it, else (a
    /// composite) the lowest scan.
    fn want(&self) -> Want {
        self.sites
            .iter()
            .find(|s| s.id == self.state.site.id)
            .and_then(|s| products::want_for(s, &self.state.product))
            .unwrap_or(Want::Lowest)
    }
    /// Open `station`'s catalogued `want` (S20: one ring per product): the
    /// ring is published and becomes the timeline, and its newest frame, or
    /// the loading placeholder, shows. Pruning first keeps the station's
    /// lowest scan and at most two other products.
    fn open_catalog(&mut self, station: &Station, want: Want) -> Result<(), String> {
        let variant = self.variant_for(station, want);
        if let Err(e) = self.catalog.prune(&station.id, &variant) {
            eprintln!("Frame catalog: {e}");
        }
        let listed = self
            .catalog
            .list_variant(&station.id, &variant)
            .unwrap_or_else(|e| {
                eprintln!("Frame catalog: {e}");
                Vec::new()
            });
        // The whole ring is published under stable names, so a client can
        // fetch any frame of the loop without a seek.
        for entry in &listed {
            self.link(entry);
        }
        let shown = match listed.iter().rev().find(|e| e.record.is_some()) {
            Some(newest) => self.show_entry(newest).map(|()| Some(newest.start_ms)),
            None => {
                let mut frame = empty_frame(&self.template, station);
                // A radar's product, or a composite's (S24b).
                if !want.is_lowest() {
                    let (id, name) = want.product();
                    (frame.product, frame.product_name) = (id.to_owned(), name);
                    // S24a: and the product's own legend, before its frame.
                    (frame.units, frame.palette, frame.bounds) = legend_of(&self.template, want);
                }
                // A chosen angle shows its own until its first frame (S26),
                // not "Reflectivity 0.0°".
                if let Want::Angle(deg) = want {
                    frame.elevation_deg = deg;
                }
                blank_textures(&frame)
                    .and_then(|(texture, lut)| self.show(frame, &texture, &lut))
                    .map(|()| None)
            }
        };
        self.frame_ms = match shown {
            Ok(frame_ms) => frame_ms,
            Err(e) => {
                eprintln!("Publishing the frame for {}: {e}", station.id);
                return Err(format!("Could not publish a frame for {}.", station.id));
            }
        };
        self.timeline = Timeline::new(listed);
        self.pending = None;
        self.state.playing = false;
        Ok(())
    }
    /// `set_product` (S20, `docs/protocol.md`, products): refused, changing
    /// nothing, for an unknown id, no radar selected, or one that cannot make
    /// it; the same choice again changes nothing; otherwise the timeline
    /// becomes the radar's history of that product and the poller follows it.
    /// `CAPPI1`/`CAPPI2` are `CAPPI` at 1 and 2 km, and a height is checked,
    /// before anything else (S29, `products::choose`).
    fn set_product(
        &mut self,
        product: &str,
        elevation_index: u32,
        height_m: Option<u32>,
        above: Option<&str>,
    ) -> (bool, Option<String>) {
        let choice = match products::choose(product, elevation_index, height_m, above) {
            Ok(choice) => choice,
            Err(message) => return (false, Some(message)),
        };
        let station = self
            .sites
            .iter()
            .find(|s| s.id == self.state.site.id)
            .cloned();
        // S24b: a provider's composite offers the products the engine makes
        // from its radars; My mosaic has none (its set's rule).
        let offers =
            |s: &Station| s.kind == SiteKind::Polar || !products::for_station(s).0.is_empty();
        let station = match station {
            Some(s) if self.state.source == Source::Live && offers(&s) => s,
            Some(s) if s.kind == SiteKind::Grid => {
                // S33: an OPERA box without products (`iberia`) shows its
                // composite only.
                let why = if s.provider == providers::ProviderId::Mosaic {
                    "its set's rule says what it shows"
                } else {
                    "it shows the composite only"
                };
                return (
                    false,
                    Some(format!("{} has no products to choose; {why}.", s.name)),
                );
            }
            Some(_) if self.state.source == Source::Archived => {
                return (
                    false,
                    Some("An archived volume shows its lowest scan only.".into()),
                );
            }
            _ => {
                return (
                    false,
                    Some("Select a radar before choosing a product.".into()),
                );
            }
        };
        // S32: no terrain under this radar (a Spanish one): a height above
        // the ground is one above sea level, and `state.product` says so.
        let choice = match choice.above {
            Some(products::Above::Ground) if !products::has_terrain(&station) => products::Choice {
                above: Some(products::Above::Sea),
                ..choice
            },
            _ => choice,
        };
        let Some(want) = products::want_for(&station, &choice) else {
            let at = if elevation_index > 0 {
                format!(" at elevation index {elevation_index}")
            } else {
                String::new()
            };
            return (
                false,
                Some(format!("{} cannot show {product}{at}.", station.name)),
            );
        };
        if choice == self.state.product {
            return (false, None);
        }
        if let Err(message) = self.open_catalog(&station, want) {
            return (false, Some(message));
        }
        eprintln!(
            "{} Product {}: {}",
            iso(now_ms()),
            station.id,
            want.variant()
        );
        self.product_deg = products::angle_deg(&station, &choice);
        // On a grid station (S24b) the choice is also the next radar's.
        if station.kind == SiteKind::Grid {
            self.aside = Some((choice.clone(), None));
        }
        self.state.product = choice;
        self.state.connection.status = ConnectionStatus::Loading;
        self.restart_live("polling the new product", false);
        (true, None)
    }
    /// `set_mosaic` (S25, `docs/protocol.md`, My mosaic): refused, changing
    /// nothing, when `mosaic::choose` refuses it; the same set again changes
    /// nothing; otherwise it becomes `state.mosaic`, and while `mymosaic` is
    /// selected its timeline becomes the new set's and its radars are
    /// polled, like a `select_site`.
    fn set_mosaic(
        &mut self,
        sites: &[mosaic::SiteArg],
        rule: Option<&str>,
        height_m: Option<u32>,
        above: Option<&str>,
    ) -> (bool, Option<String>) {
        let set = match mosaic::choose_with(&self.sites, sites, rule, height_m, above) {
            Ok(set) => set,
            Err(message) => return (false, Some(message)),
        };
        if set.same(&self.state.mosaic) {
            return (false, None);
        }
        eprintln!(
            "{} Mosaic set {}: {} {}",
            iso(now_ms()),
            set.variant(),
            set.rule.id(),
            set.sites
                .iter()
                .map(|s| format!("{}:{}", s.id, s.reach_km))
                .collect::<Vec<_>>()
                .join(",")
        );
        self.state.mosaic = set;
        // Kept across restarts (review #11); an archived start keeps none.
        if self.state.source == Source::Live {
            mosaic::save(&self.state.mosaic);
        }
        let shown = self
            .sites
            .iter()
            .find(|s| s.id == self.state.site.id)
            .filter(|s| s.provider == providers::ProviderId::Mosaic)
            .cloned();
        if let Some(station) = shown
            && self.state.source == Source::Live
        {
            if let Err(message) = self.open_catalog(&station, Want::Lowest) {
                return (true, Some(message));
            }
            self.state.connection.status = ConnectionStatus::Loading;
            self.restart_live("polling the new set", false);
        }
        (true, None)
    }
    /// A composite's product (S24b) is polled only while some client shows
    /// it (review M1): with no client left, the selected grid station goes
    /// back to its composite (`REF`), its product frames stay catalogued and
    /// the choice stays aside for the next radar, so a closed tab does not
    /// fetch 41 radars' volumes all night. True when anything changed.
    fn release_unwatched_product(&mut self) -> bool {
        if !self.clients.is_empty() || self.state.source != Source::Live {
            return false;
        }
        let Some(station) = self
            .sites
            .iter()
            .find(|s| s.id == self.state.site.id)
            .cloned()
        else {
            return false;
        };
        if station.kind != SiteKind::Grid || self.want().is_lowest() {
            return false;
        }
        if self.aside.is_none() {
            self.aside = Some((self.state.product.clone(), None));
        }
        if let Err(message) = self.open_catalog(&station, Want::Lowest) {
            eprintln!("{} Live {}: {message}", iso(now_ms()), station.id);
        }
        self.state.product = products::Choice::default();
        self.state.connection.status = ConnectionStatus::Loading;
        self.restart_live("no client shows the product; back to the composite", false);
        true
    }
    /// S24c: the grid sections and profiles need now: the selected station's
    /// for its newest complete frame; `Ok(None)` before it has one.
    fn section_job(&self) -> Result<Option<grid3d::Job>, String> {
        if self.state.source != Source::Live {
            return Err("Sections are made in live mode only.".into());
        }
        let Some(station) = self.sites.iter().find(|s| s.id == self.state.site.id) else {
            return Ok(None);
        };
        let Some(newest) = self.timeline.stored.last() else {
            return Ok(None);
        };
        grid3d::Job::new(
            station,
            &self.state.mosaic,
            &self.sites,
            &newest.id,
            &newest.scan_time,
            newest.start_ms,
        )
        .map(Some)
    }
    /// S24c: one pass of the sections (`grid3d::Sections::pass`); the grid
    /// to build, if any. Broadcasts when `state.section` changed.
    fn sections_pass(&mut self) -> Option<grid3d::Job> {
        let current = self.section_job();
        let (job, changed) =
            self.sections
                .pass(current, &self.dir, &self.template, &mut self.state.section);
        if changed {
            self.broadcast();
        }
        job
    }
    /// S24c: `set_section` from `client` (checked by `grid3d::check_line`).
    fn set_section(&mut self, line: Option<(grid3d::Point, grid3d::Point)>, client: u64) {
        if self
            .sections
            .set_line(line, client, &mut self.state.section, &self.template)
        {
            self.broadcast();
        }
    }
    /// Keep-warm stations (`OMASTORM_WARM`): each one not selected has a
    /// poller of its own that fills its catalog, so opening it finds its
    /// history; the selected station's own poller covers it while selected.
    /// A warm poller that ended is started again after `WARM_RETRY`.
    fn keep_warm(&mut self) {
        if self.state.source != Source::Live || self.warm.is_empty() {
            return;
        }
        let selected = self.state.site.id.clone();
        if let Some((task, _)) = self.warm_pollers.remove(&selected) {
            task.abort();
        }
        for id in self.warm.clone() {
            if id == selected {
                continue;
            }
            if let Some((task, since)) = self.warm_pollers.get(&id)
                && (!task.is_finished() || since.elapsed() < WARM_RETRY)
            {
                continue;
            }
            let Some(station) = self.sites.iter().find(|s| s.id == id).cloned() else {
                continue;
            };
            let cached: Vec<i64> = self
                .catalog
                .list(&id)
                .map(|listed| listed.iter().map(|e| e.start_ms).collect())
                .unwrap_or_default();
            eprintln!("{} Warm {id}: polling", iso(now_ms()));
            // Keep-warm stays on the lowest scan, whatever product is chosen.
            let task = tokio::spawn(providers::poll(
                station,
                self.events.clone(),
                cached,
                true,
                Want::Lowest,
            ));
            self.warm_pollers.insert(id, (task, Instant::now()));
        }
    }
    /// Go live on a station: the newest cached frame (or an empty one)
    /// shows at once under `loading`, the timeline is the station's
    /// catalog, and a poller replaces the previous station's. Reselecting
    /// the live station changes nothing while its poller is still running;
    /// a finished poller is started again so opening the popover recovers
    /// a wedged feed.
    fn select_site(&mut self, id: &str) -> (bool, Option<String>) {
        // An alias, or the id in another case, names the same station, and
        // `state.site.id` is always its canonical id (DEC-12).
        let station = providers::resolve(&self.sites, id).cloned();
        let target = station.as_ref().map_or(id, |s| s.id.as_str());
        if target == self.state.site.id && self.state.source == Source::Live {
            if self.live.as_ref().is_none_or(JoinHandle::is_finished) {
                self.restart_live("poller ended; restarting on reselect", true);
            }
            return (false, None);
        }
        let Some(station) = station else {
            return (
                false,
                Some(format!("Unknown site {id}; stations are listed in hello.")),
            );
        };
        // The product carries over (S20): an angle to the nearest, a product
        // the radar cannot make to the lowest scan. A grid station (S24b)
        // opens on its composite, and keeps the choice before it aside for
        // the next radar (one chosen on the grid station replaces it).
        let (held, held_deg) = self
            .aside
            .clone()
            .unwrap_or_else(|| (self.state.product.clone(), self.product_deg));
        let (choice, want) = if station.kind == SiteKind::Grid {
            (products::Choice::default(), Want::Lowest)
        } else {
            let choice = products::carry(&held, held_deg, &station);
            let want = products::want_for(&station, &choice).unwrap_or(Want::Lowest);
            (choice, want)
        };
        if let Err(message) = self.open_catalog(&station, want) {
            return (false, Some(message));
        }
        if station.kind == SiteKind::Grid {
            self.aside = Some((held, held_deg));
            self.product_deg = None;
        } else {
            self.aside = None;
            self.product_deg = products::angle_deg(&station, &choice);
        }
        self.state.product = choice;
        self.state.site.id = station.id;
        self.state.source = Source::Live;
        self.state.connection.status = ConnectionStatus::Loading;
        self.restart_live("polling", false);
        // A warm station just selected gives up its warm poller; the one
        // left keeps warm with a poller of its own.
        self.keep_warm();
        (true, None)
    }
    /// A pan settled with the map centred at `lat`, `lon`. While following and
    /// not locked, the nearest station takes over when it beats the current
    /// one by the hysteresis rule (`handoff`); the switch is a `select_site`,
    /// so an uncached station opens on the loading view. Locked, or with
    /// following off, the centre is noted for nothing.
    fn view_center(&mut self, lat: f64, lon: f64) -> (bool, Option<String>) {
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return (
                false,
                Some("view_center needs lat in [-90, 90] and lon in [-180, 180].".into()),
            );
        }
        if !self.state.site.follow || self.state.site.locked {
            return (false, None);
        }
        match handoff(&self.sites, &self.state.site.id, lat, lon) {
            Some(station) => {
                let id = station.id.clone();
                self.select_site(&id)
            }
            None => (false, None),
        }
    }
    /// A live sweep for the selected station grew or completed: it joins the
    /// timeline (a complete frame in time order, a growing one as the newest
    /// entry) and takes the screen while the user follows the newest frame.
    /// Broadcasts either way, since the timeline changed.
    fn arrived(&mut self, arrival: Arrival, complete: bool) -> io::Result<()> {
        let Arrival {
            frame,
            texture,
            lut,
            start_ms,
            end_ms,
            stored,
        } = arrival;
        // A complete frame comes back from the catalog with its record; a
        // sweep in progress has none.
        let entry = stored.unwrap_or_else(|| Entry {
            id: frame.id.clone(),
            scan_time: frame.scan_time.clone(),
            start_ms,
            record: None,
        });
        if let Some(known) = self.timeline.stored.iter().find(|e| e.start_ms == start_ms) {
            // A catalogued replay. Stored again with other bytes, it names
            // other files now, and the entry follows them.
            let tag = |e: &Entry| e.record.as_ref().map(|r| r.tag.clone());
            let refreshed = entry.record.is_some() && tag(known) != tag(&entry);
            if refreshed {
                self.link(&entry);
                let on_screen = self.state.frame.id == entry.id;
                self.timeline.insert(entry.clone());
                if on_screen {
                    self.show_entry(&entry)?;
                }
            }
            let cleared = known_sweep_clears_loading(self.state.connection.status);
            if cleared {
                self.state.connection.status = ConnectionStatus::Ok;
            }
            if refreshed || cleared {
                self.broadcast();
            }
            return Ok(());
        }
        let following = self.timeline.following();
        self.state.connection.status = ConnectionStatus::Ok;
        // S31: the load's first frame is on screen. S35: whether or not the
        // timeline is following it. The frame is made and in the timeline,
        // which is what the load was waiting for; a viewer scrubbed back
        // into the past is still told the load has moved on, instead of
        // watching the bar freeze until the whole fill ends.
        self.loading.shown(Instant::now());
        let shown = if complete {
            self.frame_ms = Some(start_ms);
            self.pending = None;
            self.link(&entry);
            let dropped = self.timeline.complete(entry.clone());
            if following && entry.record.is_some() {
                self.show_entry(&entry)
            } else if following {
                self.show(frame, &texture, &lut)
            } else if dropped {
                self.show_position(0)
            } else {
                Ok(())
            }
        } else {
            self.timeline.begin(entry);
            self.pending = Some(Pending {
                frame,
                texture,
                lut,
                end_ms,
            });
            if following {
                self.show_position(self.timeline.len() - 1)
            } else {
                Ok(())
            }
        };
        self.broadcast();
        shown
    }
    /// An earlier volume's frame joined the catalog: it takes its place in
    /// the timeline without touching the frame on screen, unless the pin
    /// fell off the ring. The age keeps following the newest frame.
    fn backfilled(&mut self, entry: Entry) -> io::Result<()> {
        if self.frame_ms.is_none_or(|ms| entry.start_ms > ms) {
            self.frame_ms = Some(entry.start_ms);
        }
        self.link(&entry);
        // S31: one more frame of a radar's history.
        self.loading.backfilled(Instant::now());
        // A frame built again (My mosaic's late rebuild, S25) that is on
        // screen shows its new files.
        let on_screen = self.state.frame.id == entry.id;
        let shown = if self.timeline.insert(entry.clone()) {
            self.show_position(0)
        } else if on_screen {
            self.show_entry(&entry)
        } else {
            Ok(())
        };
        self.broadcast();
        shown
    }
    /// `step` or `seek`: stop playback and show the frame the move lands on.
    /// A move that cannot be shown leaves the position where it was.
    fn navigate(
        &mut self,
        moved: impl FnOnce(&mut Timeline) -> Result<Option<usize>, &'static str>,
    ) -> (bool, Option<String>) {
        let before = self.timeline.shown.clone();
        let target = match moved(&mut self.timeline) {
            Ok(target) => target,
            Err(message) => return (false, Some(message.into())),
        };
        let paused = set(&mut self.state.playing, false);
        let Some(index) = target else {
            return (paused, None);
        };
        match self.show_position(index) {
            Ok(()) => (true, None),
            Err(e) => {
                eprintln!("Showing timeline frame {index}: {e}");
                self.timeline.shown = before;
                (
                    paused,
                    Some("Could not load the requested frame; keeping the current frame.".into()),
                )
            }
        }
    }
    /// Start playback when there is something to loop over.
    fn play(&mut self) -> bool {
        if self.state.playing || self.timeline.stored.len() < 2 {
            return false;
        }
        self.state.playing = true;
        self.wake.notify_one();
        true
    }
    /// One playback tick: the next frame, or the end of playback when it
    /// cannot be shown.
    fn tick(&mut self) {
        if !self.state.playing {
            return;
        }
        if let Some(index) = self.timeline.advance() {
            if let Err(e) = self.show_position(index) {
                eprintln!("Playback stopped at frame {index}: {e}");
                self.state.playing = false;
            }
            self.broadcast();
        }
    }
    fn broadcast(&mut self) {
        let message = self.snapshot();
        self.log_condition();
        if message == self.last_broadcast {
            return;
        }
        self.last_broadcast = message.clone();
        // Never let a stalled UI hold up other clients. Its writer closes on EOF.
        self.clients
            .retain(|(_, client)| client.try_send(message.clone()).is_ok());
    }
    /// One engine.log line whenever the selected station's condition
    /// changes (`scripts/soak-report.sh` reads them as episodes).
    fn log_condition(&mut self) {
        let now = (self.state.site.id.clone(), self.state.connection.status);
        if self.state.source != Source::Live
            || now.0.is_empty()
            || self.logged_condition.as_ref() == Some(&now)
        {
            return;
        }
        eprintln!(
            "{} Status {}: {} (age {}s)",
            iso(now_ms()),
            now.0,
            format!("{:?}", now.1).to_lowercase(),
            self.state.connection.age_seconds
        );
        self.logged_condition = Some(now);
    }
    /// Carry the fetch path's condition into `state.basemap.osm`; a change
    /// is broadcast like any other.
    fn update_osm(&mut self, info: protocol::Osm) {
        if set(&mut self.state.basemap.osm, info) {
            self.broadcast();
        }
    }
    /// A well-formed command from a client; the reader has already logged
    /// `Unsupported` ones by name. Broadcasts if anything changed and returns
    /// the message for the sender when the command could not be carried out;
    /// a rejection never changes shared state.
    fn apply(&mut self, command: Command) -> Option<String> {
        let (changed, rejection) = match command {
            Command::SelectSite { id } => self.select_site(&id),
            Command::Follow { enabled } => (set(&mut self.state.site.follow, enabled), None),
            Command::Lock { enabled } => (set(&mut self.state.site.locked, enabled), None),
            Command::ViewCenter { lat, lon } => self.view_center(lat, lon),
            Command::Seek { id } => self.navigate(|timeline| {
                timeline.seek(&id).map_err(
                    |()| "Requested frame is not in the timeline; keeping the current frame.",
                )
            }),
            Command::Step { delta } => self.navigate(|timeline| Ok(timeline.step(delta))),
            Command::Play => (self.play(), None),
            Command::Pause => (set(&mut self.state.playing, false), None),
            Command::SetProduct {
                product,
                elevation_index,
                height_m,
                above,
            } => self.set_product(&product, elevation_index, height_m, above.as_deref()),
            Command::SetMosaic {
                sites,
                rule,
                height_m,
                above,
            } => self.set_mosaic(&sites, rule.as_deref(), height_m, above.as_deref()),
            // Tile requests and place search are answered to the sender, not
            // state; sections and profiles (S24c) need the sender, `receive`.
            Command::TilesNeeded { .. }
            | Command::SearchPlaces { .. }
            | Command::SetSection { .. }
            | Command::Profile { .. }
            | Command::Unsupported => {
                return None;
            }
        };
        if changed {
            self.broadcast();
        }
        rejection
    }
}

/// `$XDG_RUNTIME_DIR/omastorm-se`, without creating it.
fn runtime_path() -> io::Result<PathBuf> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| io::Error::other("XDG_RUNTIME_DIR is required"))?;
    if !Path::new(&base).is_absolute() {
        return Err(io::Error::other("XDG_RUNTIME_DIR must be absolute"));
    }
    Ok(PathBuf::from(base).join("omastorm-se"))
}
fn runtime() -> io::Result<PathBuf> {
    let dir = runtime_path()?;
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}
/// Write `bytes` under `tex/` as an immutable revision and return its
/// protocol path.
fn publish(dir: &Path, stem: &str, frame: &str, bytes: &[u8]) -> io::Result<String> {
    fs::create_dir_all(dir.join("tex"))?;
    // Nanosecond revision plus PID avoids Qt cache collisions across daemon restarts.
    let revision = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("tex/{stem}-{frame}-{}-r{revision}.png", std::process::id());
    if !is_texture_path(&name) {
        return Err(io::Error::other(format!(
            "Refusing to publish texture path {name:?}; see docs/protocol.md"
        )));
    }
    let temporary = dir.join(format!("{name}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temporary, dir.join(&name))?;
    Ok(name)
}
/// Publish a frame's texture under `tex/` and set its path, and for a polar
/// frame its azimuth lookup's too. A grid frame has no lookup: its
/// `azimuthLut` is empty (`docs/protocol.md`, `frame.kind`).
fn publish_frame(dir: &Path, frame: &mut Frame, texture: &[u8], lut: &[u8]) -> io::Result<()> {
    frame.texture = publish(dir, "sweep", &frame.id, texture)?;
    frame.azimuth_lut = match frame.kind {
        FrameKind::Polar => publish(dir, "azlut", &frame.id, lut)?,
        FrameKind::Grid => String::new(),
    };
    Ok(())
}
/// A catalogued frame's stable texture paths (`docs/protocol.md`, texture
/// files): one name per file version for as long as the catalog holds it.
/// A grid frame has no lookup.
fn stable_paths(record: &catalog::Record) -> (String, String) {
    let id = &record.frame.id;
    let texture = format!("tex/sweep-{id}-{}.png", record.tag);
    let lut = match record.frame.kind {
        FrameKind::Polar => format!("tex/azlut-{id}-{}.png", record.tag),
        FrameKind::Grid => String::new(),
    };
    (texture, lut)
}
/// A catalogued grid frame's code texture's stable path (`docs/protocol.md`,
/// code texture); empty when it has none.
fn codes_path(record: &catalog::Record) -> String {
    record.codes.as_ref().map_or_else(String::new, |_| {
        format!("tex/codes-{}-{}.png", record.frame.id, record.tag)
    })
}
/// Publish a catalogued frame under its stable names: symbolic links into
/// the catalog, whose files never change under a tag, made once and left
/// alone while they point where they should.
fn link_record(dir: &Path, record: &catalog::Record) -> io::Result<()> {
    let (texture, lut) = stable_paths(record);
    fs::create_dir_all(dir.join("tex"))?;
    let mut links = vec![(texture, &record.texture), (lut, &record.azimuth_lut)];
    if let Some(codes) = &record.codes {
        links.push((codes_path(record), codes));
    }
    for (name, target) in links {
        if name.is_empty() {
            continue;
        }
        if !is_texture_path(&name) {
            return Err(io::Error::other(format!(
                "Refusing to publish texture path {name:?}; see docs/protocol.md"
            )));
        }
        let path = dir.join(&name);
        if fs::read_link(&path).is_ok_and(|to| to == *target) {
            continue;
        }
        let temporary = dir.join(format!("{name}.{}.tmp", std::process::id()));
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        std::os::unix::fs::symlink(target, &temporary)?;
        fs::rename(&temporary, &path)?;
    }
    Ok(())
}
/// Encode a sweep's texture and lookup as PNGs.
fn encode(sweep: &sweep::Sweep, frame: &Frame) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut pixels = sweep.texture(&frame.bounds, frame.palette.len());
    // S24a: an "at least" storm height's texel gets G bit 8.
    products::mark_texture(&frame.product, &mut pixels);
    let texture = sweep::png(u32::from(sweep.gates), sweep.rows(), &pixels)?;
    let lut = sweep::png(3600, 1, &sweep.azimuth_lut())?;
    Ok((texture, lut))
}
/// A poller's scan as a frame: a sweep like any live frame, the composite
/// as a grid frame (`composite.rs`).
fn scan_frame(template: &Frame, station: &Station, scan: &Scan, complete: bool) -> Frame {
    match scan {
        Scan::Polar(sweep) => live_frame(template, station, sweep, complete, Want::Lowest),
        Scan::Product(sweep, want, _) => live_frame(template, station, sweep, complete, *want),
        Scan::Grid(grid) => composite::frame(template, station, grid),
        Scan::Mosaic(built) => mosaic::frame(template, station, built),
    }
}
/// A scan's texture, azimuth lookup, and code texture as PNGs: a grid's
/// lookup is empty, a polar sweep has no code texture.
fn encode_scan(scan: &Scan, frame: &Frame) -> io::Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    match scan {
        Scan::Polar(sweep) | Scan::Product(sweep, ..) => {
            encode(sweep, frame).map(|(t, l)| (t, l, Vec::new()))
        }
        Scan::Grid(grid) => encode_grid(grid, frame),
        Scan::Mosaic(built) => {
            let started = Instant::now();
            let encoded = encode_grid(&built.grid, frame)?;
            // My mosaic's, or a composite's product (S24b): the id names it.
            eprintln!(
                "{} Mosaic {}: {} encoded in {:.0?}, texture {} KB, codes {} KB",
                iso(now_ms()),
                frame.id.split('-').next().unwrap_or(mosaic::STATION),
                frame.id,
                started.elapsed(),
                encoded.0.len() / 1000,
                encoded.2.len() / 1000
            );
            Ok(encoded)
        }
    }
}
/// A grid's texture, its empty lookup and its code texture as PNGs.
fn encode_grid(grid: &composite::Grid, frame: &Frame) -> io::Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let (texture, lut) = composite::encode(grid, frame)?;
    Ok((
        texture,
        lut,
        gray_png(grid.width, grid.height, &grid.codes)?,
    ))
}
/// A grid's raw codes as an 8-bit grayscale PNG: its one-channel code
/// texture (`docs/protocol.md`, code texture), a quarter of the GPU memory.
fn gray_png(width: u32, height: u32, codes: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(io::Error::other)?;
    writer.write_image_data(codes).map_err(io::Error::other)?;
    writer.finish().map_err(io::Error::other)?;
    Ok(out)
}
/// The station an archived ODIM volume came from: the nearest table site
/// within 5 km of the volume's position, else one named by its `PLC`.
fn odim_station(site: &odim::OdimSite) -> Station {
    let km = |s: &Station| great_circle_km(s.lat, s.lon, site.lat, site.lon);
    site_table()
        .sites
        .into_iter()
        .filter(|s| km(s) < 5.0)
        .min_by(|a, b| km(a).total_cmp(&km(b)))
        .unwrap_or_else(|| {
            let name = site.source_item("PLC").unwrap_or("odim");
            Station {
                id: name.to_lowercase(),
                name: name.to_owned(),
                state: String::new(),
                lat: site.lat,
                lon: site.lon,
                alt_m: site.alt_m,
                kind: SiteKind::Polar,
                ..Station::default()
            }
        })
}
/// Decode the fixture's lowest sweep and publish the sweep texture and the
/// azimuth lookup for the initial frame; also its `scanTime` in milliseconds.
/// An ODIM volume is framed like a live frame of its station (DEC-10); any
/// other file is read as Level II over the fixture frame.
fn decode_and_publish(dir: &Path, template: &Frame, archive: &[u8]) -> io::Result<(Frame, i64)> {
    let started = Instant::now();
    let mut frame = template.clone();
    let failed = |e: &dyn std::fmt::Display| io::Error::other(format!("Decoding the archive: {e}"));
    let sweep = if odim::is_odim(archive) {
        let (sweep, site) = odim::decode_lowest_dbzh_with_site(io::Cursor::new(archive.to_vec()))
            .map_err(|e| failed(&e))?;
        frame = live_frame(template, &odim_station(&site), &sweep, true, Want::Lowest);
        sweep
    } else {
        frame.attribution = LEVEL_II_CREDIT.to_owned();
        sweep::lowest_reflectivity(archive).map_err(|e| failed(&e))?
    };
    let decoded = started.elapsed();
    frame.rays = sweep.rows();
    frame.gates = u32::from(sweep.gates);
    frame.first_gate_m = sweep.first_gate_m;
    frame.gate_spacing_m = sweep.gate_spacing_m;
    frame.scale = sweep.scale;
    frame.offset = sweep.offset;
    let (sweep_png, lut_png) = encode(&sweep, &frame)?;
    let encoded = started.elapsed();
    publish_frame(dir, &mut frame, &sweep_png, &lut_png)?;
    // The launcher waits a bounded time for the socket; keep these visible.
    eprintln!(
        "Archive ready: decoded in {decoded:.2?}, textures encoded by {encoded:.2?}, published by {:.2?}",
        started.elapsed()
    );
    Ok((frame, sweep.start_ms))
}
/// Turn the pollers' events into frames: encode on the blocking pool and
/// record complete frames in the catalog, then hand the frame to the
/// timeline, which publishes it if it is to be shown, and broadcast. An
/// event for a station that is no longer selected is dropped, except a
/// keep-warm station's complete frames, which only go into the catalog.
async fn live_events(shared: Arc<Mutex<Shared>>, mut events: Receiver<providers::Event>) {
    while let Some(event) = events.recv().await {
        match event {
            providers::Event::Sweep {
                site,
                sweep,
                complete,
                provenance,
            } => {
                let (frame, catalog) = {
                    let shared = shared.lock().unwrap();
                    let selected = shared.state.site.id == site;
                    let warm = complete && shared.warm.contains(&site);
                    if shared.state.source != Source::Live || !(selected || warm) {
                        continue;
                    }
                    let Some(station) = shared.sites.iter().find(|s| s.id == site) else {
                        continue;
                    };
                    (
                        scan_frame(&shared.template, station, &sweep, complete),
                        shared.catalog.clone(),
                    )
                };
                let encoded = spawn_blocking(move || -> io::Result<Arrival> {
                    let started = Instant::now();
                    let (texture, lut, codes) = encode_scan(&sweep, &frame)?;
                    let stored = if complete {
                        let pngs = catalog::Pngs {
                            texture: &texture,
                            azimuth_lut: &lut,
                            codes: &codes,
                        };
                        Some(catalog.store_with_codes(
                            &site,
                            &frame,
                            sweep.start_ms(),
                            pngs,
                            &provenance,
                        )?)
                    } else {
                        None
                    };
                    eprintln!(
                        "{} Live {site}: {} {} in {:.0?}",
                        iso(now_ms()),
                        sweep.describe(),
                        if complete { "complete" } else { "partial" },
                        started.elapsed()
                    );
                    Ok(Arrival {
                        frame,
                        texture,
                        lut,
                        start_ms: sweep.start_ms(),
                        end_ms: sweep.end_ms(),
                        stored,
                    })
                })
                .await
                .map_err(io::Error::other)
                .and_then(|r| r);
                match encoded {
                    Ok(arrival) => {
                        let mut shared = shared.lock().unwrap();
                        // A switch while encoding: this frame belongs to the
                        // previous station's timeline, or product's (S20),
                        // which is gone; it stays catalogued.
                        if !arrival.frame.id.starts_with(&shared.state.site.id)
                            || shared.state.source != Source::Live
                            || products::variant_of(&arrival.frame.id) != shared.variant()
                        {
                            continue;
                        }
                        if let Err(e) = shared.arrived(arrival, complete) {
                            eprintln!("Live frame: {e}");
                        }
                    }
                    Err(e) => eprintln!("Live frame: {e}"),
                }
            }
            providers::Event::Backfill {
                site,
                sweep,
                provenance,
            } => {
                let (frame, catalog) = {
                    let shared = shared.lock().unwrap();
                    let wanted = shared.state.site.id == site || shared.warm.contains(&site);
                    if shared.state.source != Source::Live || !wanted {
                        continue;
                    }
                    let Some(station) = shared.sites.iter().find(|s| s.id == site) else {
                        continue;
                    };
                    (
                        scan_frame(&shared.template, station, &sweep, true),
                        shared.catalog.clone(),
                    )
                };
                let stored = spawn_blocking(move || -> io::Result<Entry> {
                    let (texture, lut, codes) = encode_scan(&sweep, &frame)?;
                    let pngs = catalog::Pngs {
                        texture: &texture,
                        azimuth_lut: &lut,
                        codes: &codes,
                    };
                    let entry = catalog.store_with_codes(
                        &site,
                        &frame,
                        sweep.start_ms(),
                        pngs,
                        &provenance,
                    )?;
                    eprintln!(
                        "{} Live {site}: backfilled {} from {}",
                        iso(now_ms()),
                        frame.scan_time,
                        provenance
                    );
                    Ok(entry)
                })
                .await
                .map_err(io::Error::other)
                .and_then(|r| r);
                match stored {
                    Ok(entry) => {
                        let mut shared = shared.lock().unwrap();
                        if !entry.id.starts_with(&shared.state.site.id)
                            || shared.state.source != Source::Live
                            || products::variant_of(&entry.id) != shared.variant()
                        {
                            continue;
                        }
                        if let Err(e) = shared.backfilled(entry) {
                            eprintln!("Backfill frame: {e}");
                        }
                    }
                    Err(e) => eprintln!("Backfill frame: {e}"),
                }
            }
            // The newest volume is already catalogued: the feed is up, so a
            // station opened on its cached frame leaves `loading`. Any other
            // condition stays, as for a catalogued sweep in `arrived`.
            providers::Event::Current { site } => {
                let mut shared = shared.lock().unwrap();
                if shared.state.site.id == site && shared.state.source == Source::Live {
                    // S31: its newest frame was on screen already.
                    shared.loading.shown(Instant::now());
                    if known_sweep_clears_loading(shared.state.connection.status) {
                        shared.state.connection.status = ConnectionStatus::Ok;
                    }
                    shared.broadcast();
                }
            }
            // S31: a made fill's progress, a radar backfill's plan and end,
            // for the selected station's ring only (a switch may leave an
            // aborted poller's last events in the channel).
            providers::Event::Progress {
                site,
                variant,
                progress,
            } => {
                let mut shared = shared.lock().unwrap();
                if shared.state.site.id == site
                    && shared.state.source == Source::Live
                    && shared.variant() == variant
                {
                    shared.loading.progress(progress, Instant::now());
                    shared.broadcast();
                }
            }
            // S35: a made frame's build started or its frame was sent. The
            // build is the load's own stage, so the wait that used to hide
            // behind the first stage's 99 % is drawn.
            providers::Event::Building {
                site,
                variant,
                time,
                building,
            } => {
                let mut shared = shared.lock().unwrap();
                if shared.state.site.id == site
                    && shared.state.source == Source::Live
                    && shared.variant() == variant
                {
                    shared.loading.building(&time, building, Instant::now());
                    shared.broadcast();
                }
            }
            providers::Event::HistoryPlan { site, want, frames } => {
                let mut shared = shared.lock().unwrap();
                if shared.state.site.id == site
                    && shared.state.source == Source::Live
                    && shared.variant() == want.variant()
                    && let Some(station) = shared.sites.iter().find(|s| s.id == site).cloned()
                {
                    let name = shared.load_name(&station, want);
                    shared.loading.plan(&name, frames, Instant::now());
                    shared.broadcast();
                }
            }
            providers::Event::HistoryEnd { site, want } => {
                let mut shared = shared.lock().unwrap();
                if shared.state.site.id == site
                    && shared.state.source == Source::Live
                    && shared.variant() == want.variant()
                {
                    shared.loading.history_end(Instant::now());
                    shared.broadcast();
                }
            }
            providers::Event::Offline { site, reason } => {
                report(&shared, &site, &reason, ConnectionStatus::Offline);
            }
            providers::Event::Silent { site, reason } => {
                report(&shared, &site, &reason, ConnectionStatus::Unavailable);
            }
        }
    }
}
/// The poller's word on the feed for `site`: `Offline` when SMHI could not
/// be reached, `Unavailable` when it answered and the station has published
/// nothing recent. Broadcast if the condition changed; ignored for a station
/// no longer selected.
fn report(shared: &Mutex<Shared>, site: &str, reason: &str, condition: ConnectionStatus) {
    let mut shared = shared.lock().unwrap();
    if shared.state.site.id != site || shared.state.source != Source::Live {
        return;
    }
    eprintln!("{} Live {site}: {reason}", iso(now_ms()));
    set(&mut shared.state.connection.status, condition);
    // S31: no first frame is coming; offline, no stage stays (review NIT5).
    shared
        .loading
        .quiet(Instant::now(), condition == ConnectionStatus::Offline);
    shared.broadcast();
}
/// Sections and profiles (S24c): on a wake (a `set_section`, a `profile`)
/// or once a second, a pass over `grid3d::Sections`; a grid it asks for is
/// built on the blocking pool from the tilt store (no request), then the
/// pass runs again to cut and answer. Idle, it holds nothing.
async fn sections(shared: Arc<Mutex<Shared>>) {
    let wake = shared.lock().unwrap().sections.wake.clone();
    loop {
        let _ = timeout(Duration::from_secs(1), wake.notified()).await;
        let mut job = shared.lock().unwrap().sections_pass();
        while let Some(next) = job {
            let building = next.clone();
            let built = spawn_blocking(move || building.build())
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            let mut held = shared.lock().unwrap();
            held.sections.built(next, built);
            job = held.sections_pass();
        }
    }
}
/// Playback: woken by `play`, it advances the timeline one frame per
/// `PLAY_INTERVAL` until `playing` is cleared, then waits for the next wake.
async fn player(shared: Arc<Mutex<Shared>>, wake: Arc<Notify>) {
    loop {
        wake.notified().await;
        loop {
            // The step is re-judged every frame, so a backfill landing
            // mid-loop slows the loop rather than shortening it.
            let step = {
                let shared = shared.lock().unwrap();
                if !shared.state.playing {
                    break;
                }
                play_step(shared.timeline.complete_count())
            };
            sleep(step).await;
            let mut shared = shared.lock().unwrap();
            if !shared.state.playing {
                break;
            }
            shared.tick();
        }
    }
}
/// Playback loops the complete frames over about `PLAY_LOOP` however many
/// there are, within `PLAY_STEP_MIN` to `PLAY_STEP_MAX` per frame: a fresh
/// station's dozen backfilled frames turn slowly enough to read, a full
/// ring turns at four frames a second.
fn play_step(frames: usize) -> Duration {
    (PLAY_LOOP / frames.max(1) as u32).clamp(PLAY_STEP_MIN, PLAY_STEP_MAX)
}
/// Files under `tex/` that no `state` references, keyed by when each was first
/// seen unreferenced. Time is measured from observation rather than from file
/// timestamps, so a file left by an earlier daemon counts from this daemon's
/// start: a texture served immediately before a crash survives the grace period.
#[derive(Default)]
struct Retirement {
    unreferenced: HashMap<PathBuf, Instant>,
}
impl Retirement {
    /// Given the files present and the files the state references, return
    /// those whose grace period has run out.
    fn sweep(
        &mut self,
        present: impl IntoIterator<Item = PathBuf>,
        referenced: &HashSet<PathBuf>,
        now: Instant,
    ) -> Vec<PathBuf> {
        let mut seen = HashMap::new();
        for path in present {
            if !referenced.contains(&path) {
                let since = self.unreferenced.get(&path).copied().unwrap_or(now);
                seen.insert(path, since);
            }
        }
        // Referenced again, or already gone: forget it. Deleted below: forget it too.
        self.unreferenced = seen;
        let expired: Vec<PathBuf> = self
            .unreferenced
            .iter()
            .filter(|(_, since)| now.duration_since(**since) >= RETIRE_AFTER)
            .map(|(path, _)| path.clone())
            .collect();
        for path in &expired {
            self.unreferenced.remove(path);
        }
        expired
    }
}
fn cleanup(dir: &Path, shared: &Mutex<Shared>, retirement: &mut Retirement) -> io::Result<()> {
    let mut present = Vec::new();
    for entry in fs::read_dir(dir.join("tex"))? {
        let entry = entry?;
        let kind = entry.file_type()?;
        // A catalogued frame's stable names are symbolic links (S19).
        if kind.is_file() || kind.is_symlink() {
            present.push(entry.path());
        }
    }
    // Judged and deleted under the lock: a stable name can be referenced
    // again at any moment (the station reselected), and must not be
    // deleted after that.
    let shared = shared.lock().unwrap();
    let referenced: HashSet<PathBuf> = shared
        .state
        .referenced_files()
        .map(|path| dir.join(path))
        .collect();
    for path in retirement.sweep(present, &referenced, Instant::now()) {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
/// One line from a client. Rejections go back on `reply`, that client's own
/// queue, so no other client hears about a command it did not send; a tile
/// request goes to that client's tile task on `tiles`.
fn receive(
    shared: &Mutex<Shared>,
    client: u64,
    reply: &Sender<String>,
    tiles: &Sender<tiles::Request>,
    bytes: &[u8],
) {
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) if value.is_object() => value,
        _ => return eprintln!("Ignoring malformed command"),
    };
    let kind = value["type"].as_str().unwrap_or_default();
    let message = match Command::deserialize(&value) {
        // A command newer than this build; the sender is not told, since
        // ignoring it is the documented answer (docs/protocol.md).
        Ok(Command::Unsupported) => return eprintln!("Ignoring unsupported command: {kind}"),
        Ok(Command::TilesNeeded { z, x0, y0, x1, y1 }) => {
            match (tiles::Request { z, x0, y0, x1, y1 }).validate() {
                // A full request queue means the client is flooding; the
                // newest request supersedes anyway, so dropping is harmless.
                Ok(request) => {
                    let _ = tiles.try_send(request);
                    return;
                }
                Err(reason) => format!("Invalid tiles_needed command: {reason}."),
            }
        }
        Ok(Command::SearchPlaces { query, lat, lon }) => {
            if query.chars().count() > 200 {
                "search_places query is too long.".into()
            } else {
                let origin = match (lat, lon) {
                    (None, None) => None,
                    (Some(lat), Some(lon))
                        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) =>
                    {
                        Some((lat, lon))
                    }
                    _ => {
                        let message =
                            "search_places needs lat in [-90, 90] and lon in [-180, 180].";
                        let rejection = Rejection {
                            v: VERSION,
                            command: kind,
                            message,
                        };
                        let _ = reply.try_send(line(&Message::Error(&rejection)));
                        return;
                    }
                };
                let results = tiles::search_places(&query, origin, 8);
                let message = Places {
                    v: VERSION,
                    query: &query,
                    results: &results,
                };
                let _ = reply.try_send(line(&Message::Places(&message)));
                return;
            }
        }
        // S24c: a section is owned by the client that set it; a profile is
        // answered to its sender once a grid is held.
        Ok(Command::SetSection { from, to }) => match grid3d::check_line(from, to) {
            Ok(line) => return shared.lock().unwrap().set_section(line, client),
            Err(message) => message,
        },
        Ok(Command::Profile { lat, lon }) if grid3d::in_range(lat, lon) => {
            let mut shared = shared.lock().unwrap();
            return shared.sections.ask(client, reply.clone(), lat, lon);
        }
        Ok(Command::Profile { .. }) => {
            "profile needs lat in [-90, 90] and lon in [-180, 180].".into()
        }
        Ok(command) => match shared.lock().unwrap().apply(command) {
            Some(message) => message,
            None => return,
        },
        Err(e) => format!("Invalid {kind} command: {e}."),
    };
    let rejection = Rejection {
        v: VERSION,
        command: kind,
        message: &message,
    };
    // A full queue means this client has stalled; the next broadcast drops it.
    let _ = reply.try_send(line(&Message::Error(&rejection)));
}
/// Register a connection and start its two tasks: a writer draining the
/// client's queue and a reader turning its lines into commands. Must run
/// inside the runtime.
fn client(
    stream: tokio::net::UnixStream,
    shared: Arc<Mutex<Shared>>,
    osm: Arc<osm::Osm>,
) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<String>(QUEUE);
    let (tiles_tx, tiles_rx) = mpsc::channel::<tiles::Request>(8);
    let id;
    {
        let mut shared = shared.lock().unwrap();
        let snapshot = shared.snapshot();
        if shared.clients.len() >= 64 {
            return Err(io::Error::other("Too many clients"));
        }
        tx.try_send(line(&Message::Hello(&hello()))).unwrap();
        tx.try_send(snapshot).unwrap();
        id = shared.next_client;
        shared.next_client += 1;
        shared.clients.push((id, tx.clone()));
    }
    tokio::spawn(serve_tiles(shared.clone(), osm, tx.clone(), tiles_rx));
    tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if !matches!(
                timeout(WRITE_TIMEOUT, writer.write_all(message.as_bytes())).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
        // Every sender is gone (the reader ended and the client was removed)
        // or the client stalled: either way the connection is finished.
        let _ = writer.shutdown().await;
    });
    tokio::spawn(async move {
        // Bound allocation even when a client never sends a newline.
        let mut reader = AsyncBufReader::new(reader).take(MAX_LINE + 1);
        loop {
            let mut bytes = Vec::new();
            reader.set_limit(MAX_LINE + 1);
            let read = reader.read_until(b'\n', &mut bytes).await;
            if !matches!(read, Ok(n) if n > 0)
                || bytes.len() as u64 > MAX_LINE
                || !bytes.ends_with(b"\n")
            {
                break;
            }
            receive(&shared, id, &tx, &tiles_tx, &bytes);
        }
        {
            let mut shared = shared.lock().unwrap();
            shared.clients.retain(|(client_id, _)| *client_id != id);
            // S24c: a section goes with the client that set it.
            let Shared {
                sections, state, ..
            } = &mut *shared;
            if sections.client_left(id, &mut state.section) {
                shared.broadcast();
            }
            // Review M1: the last client gone, a composite's product stops.
            if shared.release_unwatched_product() {
                shared.broadcast();
            }
        }
        // `tx` drops here; with the clone in `clients` gone, the writer's
        // queue closes and it shuts the socket down.
    });
    Ok(())
}
/// Answer one client's `tiles_needed` requests, tile by tile, centre-out.
/// A tile rendered this session is answered at once; another is drawn on
/// the blocking pool and published, then announced. From z7 the vector tile
/// is fetched (or read from the cache) and the tile answered as `osm`; when
/// it cannot be, the tile is answered as `ne` and asked for again once the
/// back-off passes, so the client hears it a second time under a new path.
/// A newer request from the same client supersedes the pending tiles
/// outside its rectangle, and its own tiles follow. Ends when the client's
/// queue closes.
async fn serve_tiles(
    shared: Arc<Mutex<Shared>>,
    osm: Arc<osm::Osm>,
    reply: Sender<String>,
    mut requests: Receiver<tiles::Request>,
) {
    let dir = match runtime_path() {
        Ok(dir) => dir,
        Err(e) => return eprintln!("Tiles: {e}"),
    };
    let geography = tiles::Geography::embedded();
    let Some(mut request) = requests.recv().await else {
        return;
    };
    let mut queue = request.centre_out();
    loop {
        let mut newer = None;
        let mut announced = HashSet::new();
        let mut stand_ins = Vec::new();
        for key in queue {
            // The newest request decides which pending tiles are still wanted.
            while let Ok(latest) = requests.try_recv() {
                newer = Some(latest);
            }
            if newer.is_some_and(|latest| !latest.contains(key)) || !announced.insert(key) {
                continue;
            }
            let mut served = None;
            if (osm::FROM_ZOOM..=osm::MAX_ZOOM).contains(&key.z) {
                match serve_osm(&shared, &osm, &dir, key).await {
                    Ok(Some(ready)) => served = Some((Set::Osm, ready)),
                    Ok(None) => stand_ins.push(key),
                    Err(e) => eprintln!("Tile {key:?} (osm): {e}"),
                }
                shared.lock().unwrap().update_osm(osm.info());
            }
            let (set, ready) = match served {
                Some(served) => served,
                None => match serve_ne(&shared, geography, &dir, key).await {
                    Ok(ready) => (Set::Ne, ready),
                    Err(e) => {
                        eprintln!("Tile {key:?}: {e}");
                        continue;
                    }
                },
            };
            let message = TileReady {
                v: VERSION,
                set: set.name(),
                z: key.z,
                x: key.x,
                y: key.y,
                path: &ready.path,
                labels: ready.labels,
            };
            if reply
                .send(line(&Message::TileReady(&message)))
                .await
                .is_err()
            {
                return;
            }
        }
        osm.trim().await;
        // The next pass: a newer request, or the `ne` stand-ins once the
        // back-off has passed, unless a request arrives first.
        queue = match newer {
            Some(latest) => {
                request = latest;
                request.centre_out()
            }
            None if stand_ins.is_empty() => match requests.recv().await {
                Some(next) => {
                    request = next;
                    request.centre_out()
                }
                None => return,
            },
            None => match timeout_at(osm.retry_at().into(), requests.recv()).await {
                Ok(Some(next)) => {
                    request = next;
                    request.centre_out()
                }
                Ok(None) => return,
                Err(_) => stand_ins,
            },
        };
    }
}
/// Publish `png` under `path` on the blocking pool and record it, deleting
/// what fell off the store's cap.
async fn publish_tile(
    shared: &Mutex<Shared>,
    dir: &Path,
    set: Set,
    key: TileKey,
    render: impl FnOnce() -> io::Result<(Vec<u8>, Vec<protocol::Label>)> + Send + 'static,
) -> io::Result<Ready> {
    let path = shared.lock().unwrap().tiles.path(set, key);
    let (root, name) = (dir.to_path_buf(), path.clone());
    let labels = spawn_blocking(move || {
        let (png, labels) = render()?;
        tiles::write(&root, &name, &png)?;
        Ok::<_, io::Error>(labels)
    })
    .await
    .map_err(io::Error::other)??;
    let ready = Ready { path, labels };
    let evicted = shared
        .lock()
        .unwrap()
        .tiles
        .announce(set, key, ready.clone());
    for old in evicted {
        let _ = fs::remove_file(dir.join(old));
    }
    Ok(ready)
}
/// The `ne` tile, rendered unless this session already has it.
async fn serve_ne(
    shared: &Mutex<Shared>,
    geography: &'static tiles::Geography,
    dir: &Path,
    key: TileKey,
) -> io::Result<Ready> {
    if let Some(ready) = shared.lock().unwrap().tiles.ready(Set::Ne, key).cloned() {
        return Ok(ready);
    }
    publish_tile(shared, dir, Set::Ne, key, move || {
        Ok((
            tiles::render(geography, key)?,
            tiles::labels(geography, key),
        ))
    })
    .await
}
/// The `osm` tile, rendered from the cached or fetched vector tile unless
/// this session already has it; `None` when the vector tile cannot be had
/// right now.
async fn serve_osm(
    shared: &Mutex<Shared>,
    osm: &osm::Osm,
    dir: &Path,
    key: TileKey,
) -> io::Result<Option<Ready>> {
    if let Some(ready) = shared.lock().unwrap().tiles.ready(Set::Osm, key).cloned() {
        return Ok(Some(ready));
    }
    let Some(bytes) = osm.tile(key).await else {
        return Ok(None);
    };
    // The version is known once a tile is: name this generation after it.
    shared
        .lock()
        .unwrap()
        .tiles
        .osm_version(&osm.info().version);
    publish_tile(shared, dir, Set::Osm, key, move || osm::render(&bytes, key))
        .await
        .map(Some)
}
/// Run the daemon: decode and publish synchronously, then serve on a
/// current-thread tokio runtime with the texture cleanup on an interval and
/// a pair of tasks per client.
fn serve(dir: PathBuf) -> io::Result<()> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("engine.lock"))?;
    lock.try_lock()
        .map_err(|e| io::Error::other(format!("Engine already running: {e}")))?;
    let socket = dir.join("engine.sock");
    // Only the lock owner can recover a stale socket or publish textures.
    match fs::remove_file(&socket) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let template = fixture_frame();
    // Development runs may start on an archived volume; the shipped daemon
    // starts with no frame and goes live on the first `select_site`.
    let archive = env::var_os(ARCHIVE_ENV).filter(|path| !path.is_empty());
    let (frame, frame_ms, entries, source, status) = match archive {
        Some(path) => {
            let bytes = fs::read(&path).map_err(|e| {
                io::Error::other(format!(
                    "Reading {ARCHIVE_ENV} {}: {e}",
                    Path::new(&path).display()
                ))
            })?;
            let (frame, ms) = decode_and_publish(&dir, &template, &bytes)?;
            let entry = Entry {
                id: frame.id.clone(),
                scan_time: frame.scan_time.clone(),
                start_ms: ms,
                record: None,
            };
            (
                frame,
                Some(ms),
                vec![entry],
                Source::Archived,
                ConnectionStatus::Ok,
            )
        }
        None => {
            let mut frame = startup_frame(&template);
            let (texture, lut) = blank_textures(&frame)?;
            publish_frame(&dir, &mut frame, &texture, &lut)?;
            eprintln!("Ready with no frame; waiting for select_site");
            (
                frame,
                None,
                Vec::new(),
                Source::Live,
                ConnectionStatus::Loading,
            )
        }
    };
    // Tile names end in a generation tag so pixels never change under a name
    // the UI has cached: eight hex digits of the build fingerprint (`ne`
    // geography travels inside the binary, so the build is its version).
    let tile_store = tiles::Store::open(&dir, &build_id()[..8])?;
    // Opens the vector tile cache and builds the HTTP client; fetches nothing.
    let osm = Arc::new(osm::Osm::open()?);
    // The frame ring buffer; live frames are written here as they complete.
    let catalog = Arc::new(catalog::Catalog::open(osm::cache_root()?.join("frames"))?);
    let sites = match source {
        Source::Archived => with_archived_station(site_table().sites, &frame),
        Source::Live => site_table().sites,
    };
    let warm = match (source, env::var(WARM_ENV)) {
        (Source::Live, Ok(value)) => parse_warm(&value, &sites),
        _ => Vec::new(),
    };
    if !warm.is_empty() {
        eprintln!("Keeping warm: {}", warm.join(", "));
    }
    let (events, event_rx) = mpsc::channel(16);
    let wake = Arc::new(Notify::new());
    let shared = Arc::new(Mutex::new(Shared {
        state: initial_state(frame, osm.info(), source, status),
        tiles: tile_store,
        clients: Vec::new(),
        next_client: 0,
        template,
        sites,
        dir: dir.clone(),
        catalog,
        live: None,
        warm,
        warm_pollers: HashMap::new(),
        product_deg: None,
        aside: None,
        last_live_restart: Instant::now(),
        events,
        frame_ms,
        timeline: Timeline::new(entries),
        pending: None,
        wake: wake.clone(),
        last_broadcast: String::new(),
        logged_condition: None,
        sections: grid3d::Sections::default(),
        loading: loading::Tracker::default(),
    }));
    // My mosaic's set as the last live run left it (S25, review #11).
    if source == Source::Live {
        let mut held = shared.lock().unwrap();
        let kept = mosaic::load(&held.sites);
        held.state.mosaic = kept;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    // Bind needs the reactor, so it happens inside the runtime; a bind
    // failure ends `serve` with the lock still held until we return.
    let _guard = runtime.enter();
    let listener = UnixListener::bind(&socket)?;
    runtime.spawn(live_events(shared.clone(), event_rx));
    runtime.spawn(player(shared.clone(), wake));
    runtime.spawn(sections(shared.clone()));
    runtime.spawn(netstats::report(netstats::period()));
    shared.lock().unwrap().keep_warm();
    let cleanup_shared = shared.clone();
    runtime.spawn(async move {
        let mut retirement = Retirement::default();
        // The first tick completes at once, so cleanup runs at startup too.
        let mut tick = interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            if let Err(e) = cleanup(&dir, &cleanup_shared, &mut retirement) {
                eprintln!("Texture cleanup: {e}");
            }
            // Once a second while live, `ageSeconds` and `stale` move on a
            // quiet feed; `broadcast` sends nothing when nothing changed.
            let mut shared = cleanup_shared.lock().unwrap();
            if shared.state.source == Source::Live {
                shared.broadcast();
                // Review M1: a broadcast may have dropped the last client.
                if shared.release_unwatched_product() {
                    shared.broadcast();
                }
                shared.keep_warm();
                if !shared.state.site.id.is_empty()
                    && should_restart_live(
                        shared.live.as_ref().is_none_or(JoinHandle::is_finished),
                        shared.evidence_age_secs(),
                        shared.last_live_restart.elapsed(),
                        shared.staleness(),
                    )
                {
                    let why = if shared.live.as_ref().is_none_or(JoinHandle::is_finished) {
                        "poller ended; restarting".to_owned()
                    } else {
                        format!(
                            "no new radial for {}s; rediscovering",
                            shared.evidence_age_secs().unwrap_or(0)
                        )
                    };
                    shared.restart_live(&why, true);
                }
            }
        }
    });
    runtime.block_on(async {
        loop {
            match listener
                .accept()
                .await
                .and_then(|(stream, _)| client(stream, shared.clone(), osm.clone()))
            {
                Ok(()) => {}
                Err(e) => eprintln!("Client: {e}"),
            }
        }
    })
}
/// The hello of whatever daemon answers on the socket, of any build, or
/// `None` when nothing accepts the connection.
fn handshake(dir: &Path) -> io::Result<Option<Handshake>> {
    let Ok(stream) = UnixStream::connect(dir.join("engine.sock")) else {
        return Ok(None);
    };
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut reader = BufReader::new(stream).take(64 * 1024);
    let mut text = String::new();
    // A daemon that has bound the socket but not yet published its hello is
    // starting, not broken: the read timeout means "not ready", the same
    // answer as no socket at all, so `ensure` keeps waiting out its deadline.
    match reader.read_line(&mut text) {
        Ok(_) => {}
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e),
    }
    let handshake: Handshake = serde_json::from_str(&text).map_err(io::Error::other)?;
    if handshake.kind != "hello" {
        return Err(io::Error::other(
            "Existing engine did not answer with a hello",
        ));
    }
    Ok(Some(handshake))
}
/// Whether a daemon of this build answers on the socket. A daemon of another
/// build or protocol is ended here (decided 2026-09-06, DESIGN.md): windows
/// reconnect within a second, while refusing it left the launch key dead
/// after every rebuild until a manual `stop`.
fn ready(dir: &Path) -> io::Result<bool> {
    Ok(answering(dir)?.is_some())
}
/// `ready`, naming the PID of the daemon of this build that answered.
fn answering(dir: &Path) -> io::Result<Option<u32>> {
    let Some(handshake) = handshake(dir)? else {
        return Ok(None);
    };
    if handshake.v != VERSION || handshake.build != build_id() {
        terminate(dir, handshake.pid)?;
        eprintln!(
            "Stopped engine of another build (PID {}); starting this build.",
            handshake.pid
        );
        return Ok(None);
    }
    Ok(Some(handshake.pid))
}
/// Whether no daemon holds `engine.lock`. Taking the lock briefly here is
/// harmless: only `serve` keeps it, and a `serve` racing us simply waits.
fn lock_is_free(dir: &Path) -> io::Result<bool> {
    match OpenOptions::new().write(true).open(dir.join("engine.lock")) {
        Ok(lock) => Ok(lock.try_lock().is_ok()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}
/// End the daemon answering on the socket, whatever its build, and wait until
/// it has released the socket and the lock. No daemon means nothing to do.
/// The daemon needs no signal handler: the OS releases its lock and `serve`
/// already recovers a leftover socket file.
fn stop(dir: PathBuf) -> io::Result<()> {
    let Some(handshake) = handshake(&dir)? else {
        return Ok(());
    };
    terminate(&dir, handshake.pid)?;
    println!("Stopped engine (PID {})", handshake.pid);
    Ok(())
}
/// Signal the daemon with `pid` and wait up to 2 s until its socket refuses
/// connections and `engine.lock` is free.
fn terminate(dir: &Path, pid: u32) -> io::Result<()> {
    // 0 would signal our own process group and 1 is init; neither is a daemon.
    let target = libc::pid_t::try_from(pid)
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| io::Error::other(format!("Existing engine reports PID {pid}")))?;
    // SAFETY: kill(2) only sends a signal; it touches no memory of ours.
    if unsafe { libc::kill(target, libc::SIGTERM) } != 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(io::Error::other(format!("Cannot signal PID {pid}: {e}")));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let refused = UnixStream::connect(dir.join("engine.sock")).is_err();
        if refused && lock_is_free(dir)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "Engine (PID {pid}) did not stop within 2 s"
            )));
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn ensure(dir: PathBuf) -> io::Result<()> {
    if ready(&dir)? {
        return Ok(());
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("engine.log"))?;
    let spawn = || -> io::Result<std::process::Child> {
        Process::new(env::current_exe()?)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log.try_clone()?)
            .spawn()
    };
    let mut child = spawn()?;
    // Startup opens the caches and publishes a blank frame (an archived
    // development volume adds about 0.3 s; timings go to engine.log). Allow
    // far more than that, so a slow disk or a busy machine gets a slow
    // launch rather than a killed daemon.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(pid) = answering(&dir)? {
            // Another daemon answered while ours was still starting: a
            // concurrent launcher, or one already running whose first hello
            // was slow. Ours would take the lock once that one exits and
            // then serve forever on a socket nobody finds, so end it now.
            if pid != child.id() && child.try_wait()?.is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Ok(());
        }
        // A concurrent launcher may have won the lock. If a later probe
        // retired a stale daemon, start again after its lock is released.
        if child.try_wait()?.is_some() && lock_is_free(&dir)? {
            child = spawn()?;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(io::Error::other(format!(
        "Engine did not become ready; see {}",
        dir.join("engine.log").display()
    )))
}
fn main() -> io::Result<()> {
    BUILD.set(fingerprint()?).expect("set once");
    let mode = env::args().nth(1).unwrap_or_else(|| "serve".into());
    match mode.as_str() {
        "serve" => serve(runtime()?),
        "ensure" => ensure(runtime()?),
        "stop" => stop(runtime_path()?),
        _ => Err(io::Error::other(
            "Usage: omastorm-engine [serve|ensure|stop]",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> HashSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }
    fn files(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }
    fn entry(minute: i64) -> Entry {
        Entry {
            id: format!("KJAX-20260907T00{minute:02}00Z-e0"),
            scan_time: format!("2026-09-07T00:{minute:02}:00Z"),
            start_ms: 1_788_998_400_000 + minute * 60_000,
            record: None,
        }
    }
    /// A live engine's shared state with no client, under a scratch dir;
    /// the runtime is for `restart_live`'s spawn (entered by the caller,
    /// never driven).
    fn live_shared(name: &str) -> (Shared, tokio::runtime::Runtime) {
        let root = std::env::temp_dir().join(format!("omastorm-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("runtime");
        fs::create_dir_all(&dir).unwrap();
        let (events, _) = mpsc::channel(16);
        let osm = protocol::Osm {
            status: protocol::OsmStatus::Unavailable,
            source: String::new(),
            version: String::new(),
            attribution: String::new(),
        };
        let frame = fixture_frame();
        let shared = Shared {
            state: initial_state(frame.clone(), osm, Source::Live, ConnectionStatus::Loading),
            tiles: tiles::Store::open(&dir, "test").unwrap(),
            clients: Vec::new(),
            next_client: 0,
            template: frame,
            sites: site_table().sites,
            dir,
            catalog: Arc::new(catalog::Catalog::open(root.join("frames")).unwrap()),
            live: None,
            warm: Vec::new(),
            warm_pollers: HashMap::new(),
            product_deg: None,
            aside: None,
            last_live_restart: Instant::now(),
            events,
            frame_ms: None,
            timeline: Timeline::new(Vec::new()),
            pending: None,
            wake: Arc::new(Notify::new()),
            last_broadcast: String::new(),
            logged_condition: None,
            sections: grid3d::Sections::default(),
            loading: loading::Tracker::default(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        (shared, runtime)
    }
    /// Review M1: a composite's product stops, back to the composite, when
    /// the engine's last client has gone; a radar's product and My mosaic
    /// are left alone.
    #[test]
    fn a_composites_product_stops_when_no_client_is_left() {
        let (mut shared, runtime) = live_shared("unwatched");
        let _guard = runtime.enter();
        shared.state.site.id = "nordic".into();
        let cmax = products::choose(products::CMAX, 0, None, None).unwrap();
        shared.state.product = cmax.clone();
        assert_eq!(shared.variant(), "cmax");
        let (tx, _client) = mpsc::channel::<String>(4);
        shared.clients.push((1, tx));
        assert!(
            !shared.release_unwatched_product(),
            "a client still shows it"
        );
        assert_eq!(shared.state.product, cmax);
        shared.clients.clear();
        assert!(shared.release_unwatched_product());
        assert_eq!(shared.state.product, products::Choice::default());
        assert_eq!(
            shared.aside,
            Some((cmax.clone(), None)),
            "kept for the next radar"
        );
        assert_eq!(shared.variant(), "e0");
        assert!(shared.live.is_some(), "the composite's own poller runs");
        assert_eq!(shared.state.frame.id, "nordic-loading");
        assert!(!shared.release_unwatched_product(), "once");
        shared.state.site.id = "vara".into();
        shared.state.product = cmax.clone();
        assert!(!shared.release_unwatched_product(), "a radar's product");
        shared.state.site.id = "mymosaic".into();
        assert!(!shared.release_unwatched_product(), "My mosaic");
        if let Some(task) = shared.live.take() {
            task.abort();
        }
    }
    /// S31: a radar opened on the placeholder loads its first frame; a
    /// composite's product loads over the composite's newest frame, whose
    /// files stay published and referenced; a station opened on a
    /// catalogued frame shows no first stage.
    #[test]
    fn a_load_names_its_first_frame_and_a_composites_product_loads_over_it() {
        let (mut shared, runtime) = live_shared("loading");
        let _guard = runtime.enter();
        let later = || Instant::now() + Duration::from_secs(5);
        assert!(shared.select_site("vara").0);
        shared.snapshot();
        let first = shared.state.loading.clone().unwrap();
        assert_eq!(
            (first.stage, first.percent, first.total, first.unit),
            (loading::Stage::First, 0, 1, loading::Unit::Frames)
        );
        assert_eq!(first.label, "Vara Reflectivity 0.5°: first frame");
        // The composite's ring holds a frame: it opens on it, no first stage.
        let mut frame = fixture_frame();
        frame.id = "nordic-20260914T100000Z-e0".into();
        shared
            .catalog
            .store("nordic", &frame, 1, b"sweep", b"lut", "p")
            .unwrap();
        assert!(shared.select_site("nordic").0);
        assert_eq!(shared.loading.wire(later()), None);
        // Its product has no frame: the placeholder, the fill's first
        // stage, and the composite under it.
        let (changed, refused) = shared.set_product(products::CMAX, 0, None, None);
        assert!(changed && refused.is_none());
        assert!(shared.state.frame.scan_time.is_empty());
        shared.snapshot();
        let product = shared.state.loading.clone().unwrap();
        assert_eq!(product.stage, loading::Stage::First);
        assert_eq!(product.label, "Nordic Column max: placing the radars");
        let under = product.under.expect("the composite under its product");
        assert_eq!(under.id, frame.id);
        assert!(
            under
                .texture
                .starts_with("tex/sweep-nordic-20260914T100000Z-e0-")
        );
        assert_eq!(fs::read(shared.dir.join(&under.texture)).unwrap(), b"sweep");
        assert!(
            shared.state.referenced_files().any(|p| p == under.texture),
            "kept from the texture cleanup"
        );
        // Review M1: the composite's build starts. `state.frame` is still
        // the placeholder, so the composite must go on being drawn under
        // it — a Nordic grid takes about ten seconds to build, and
        // dropping `under` at the build's start blanked the map for all
        // of it. This is the composite product's own load, end to end, at
        // no request: the one load type the live runs did not reach.
        shared.loading.building("10:10Z", true, Instant::now());
        shared.snapshot();
        let build = shared.state.loading.clone().unwrap();
        assert_eq!(build.stage, loading::Stage::Build);
        assert_eq!(build.label, "Nordic Column max: building 10:10Z");
        let held = build.under.expect("the composite stays under the build");
        assert_eq!(held.id, frame.id);
        assert!(
            shared.state.referenced_files().any(|p| p == held.texture),
            "and its files are still kept from the texture cleanup"
        );
        assert!(
            shared.state.frame.scan_time.is_empty(),
            "the placeholder is what `under` is drawn in place of"
        );
        // Built and sent. A real Nordic build takes about ten seconds, so
        // the counts reach the wire on the next throttle tick, not this
        // millisecond.
        shared.loading.building("10:10Z", false, Instant::now());
        let sent = shared.loading.wire(later()).unwrap();
        assert_eq!((sent.percent, sent.done, sent.total), (50, 1, 2));
        assert_eq!(sent.label, "Nordic Column max: drawing 10:10Z");
        assert!(sent.under.is_some(), "and through the send");
        // S35: the load's frame arrives while the viewer is scrubbed back
        // into the past. Before S35 `shown` waited on the timeline
        // following the newest frame, so the bar froze at 99 % until the
        // whole fill ended; the frame is made either way, so the stage
        // ends either way.
        shared.timeline.shown = Some("some-older-frame".into());
        assert!(!shared.timeline.following(), "scrubbed back");
        let mut arrived = fixture_frame();
        arrived.id = "nordic-20260914T101000Z-cmax".into();
        let arrival = Arrival {
            frame: arrived,
            texture: b"sweep".to_vec(),
            lut: b"lut".to_vec(),
            start_ms: 1_789_380_600_000,
            end_ms: 1_789_380_660_000,
            stored: None,
        };
        shared.arrived(arrival, false).unwrap();
        shared.snapshot();
        let scrubbed = shared.state.loading.clone().unwrap();
        assert_eq!(
            scrubbed.percent, 100,
            "the stage ends though the viewer is not looking at the frame"
        );
        assert_eq!(
            scrubbed
                .stages
                .iter()
                .find(|s| s.stage == loading::Stage::First)
                .map(|s| s.state),
            Some(loading::SegmentState::Done),
            "and its segment stays full"
        );
        // Its first frame on screen: 100, and no composite under it.
        shared.loading.shown(Instant::now());
        shared.snapshot();
        let shown = shared.state.loading.clone().unwrap();
        assert_eq!((shown.percent, shown.under.is_none()), (100, true));
        if let Some(task) = shared.live.take() {
            task.abort();
        }
    }
    #[test]
    fn catalogued_frames_keep_stable_names_and_name_them_in_the_timeline() {
        let root = std::env::temp_dir().join(format!("omastorm-stable-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let catalog = catalog::Catalog::open(root.join("frames")).unwrap();
        let dir = root.join("runtime");
        let mut frame = fixture_frame();
        frame.id = "vara-20260914T100000Z-e0".into();
        let first = catalog
            .store("vara", &frame, 1, b"sweep-a", b"lut-a", "p")
            .unwrap();
        let mut other = frame.clone();
        other.id = "vara-20260914T100500Z-e0".into();
        other.rays += 1; // a blank row: another placement
        let second = catalog
            .store("vara", &other, 2, b"sweep-b", b"lut-b", "p")
            .unwrap();
        let timeline = Timeline::new(catalog.list("vara").unwrap());
        assert_eq!(timeline.stored, vec![first.clone(), second.clone()]);
        let entries = timeline.entries(&frame);
        let tag = &first.record.as_ref().unwrap().tag;
        assert_eq!(
            entries[0].texture,
            format!("tex/sweep-vara-20260914T100000Z-e0-{tag}.png")
        );
        assert_eq!(
            entries[0].azimuth_lut,
            format!("tex/azlut-vara-20260914T100000Z-e0-{tag}.png")
        );
        assert_eq!(entries[0].placement, None, "drawn like the frame on screen");
        assert_eq!(
            entries[1].placement.as_ref().map(|p| p.rays),
            Some(other.rays)
        );
        for e in [&first, &second] {
            link_record(&dir, e.record.as_ref().unwrap()).unwrap();
        }
        assert_eq!(fs::read(dir.join(&entries[0].texture)).unwrap(), b"sweep-a");
        assert_eq!(
            fs::read(dir.join(&entries[1].azimuth_lut)).unwrap(),
            b"lut-b"
        );
        // Linking again changes nothing, and a later listing (a reselect, an
        // engine restart) names the same files.
        link_record(&dir, first.record.as_ref().unwrap()).unwrap();
        assert_eq!(
            Timeline::new(catalog.list("vara").unwrap()).entries(&frame),
            entries
        );
        // A grid frame names its texture and no lookup, and its code texture.
        let mut grid = frame.clone();
        grid.id = "sweden-20260914T100000Z-e0".into();
        grid.kind = FrameKind::Grid;
        grid.grid = None;
        let pngs = catalog::Pngs {
            texture: b"grid",
            azimuth_lut: b"",
            codes: b"codes",
        };
        let stored = catalog
            .store_with_codes("sweden", &grid, 1, pngs, "p")
            .unwrap();
        let record = stored.record.as_ref().unwrap();
        let (texture, lut) = stable_paths(record);
        assert!(texture.starts_with("tex/sweep-sweden-20260914T100000Z-e0-") && lut.is_empty());
        let codes = codes_path(record);
        assert_eq!(
            codes,
            format!("tex/codes-sweden-20260914T100000Z-e0-{}.png", record.tag)
        );
        link_record(&dir, record).unwrap();
        assert_eq!(fs::read(dir.join(&codes)).unwrap(), b"codes");
        let listed = Timeline::new(catalog.list("sweden").unwrap()).entries(&grid);
        assert_eq!(listed[0].codes, codes);
        assert!(
            entries.iter().all(|e| e.codes.is_empty()),
            "polar entries name none"
        );
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn keep_warm_names_stations_by_id_or_alias_once() {
        let sites = site_table().sites;
        assert_eq!(
            parse_warm("sevax, SWEDEN,,nowhere,vara", &sites),
            ["vara", "sweden"]
        );
        assert!(parse_warm("", &sites).is_empty());
    }
    fn ids(timeline: &Timeline) -> Vec<(String, FrameStatus)> {
        timeline
            .entries(&fixture_frame())
            .into_iter()
            .map(|e| (e.id, e.status))
            .collect()
    }

    #[test]
    fn stepping_pins_a_frame_and_the_newest_follows_again() {
        let mut timeline = Timeline::new(vec![entry(0), entry(5), entry(10)]);
        assert!(timeline.following());
        assert_eq!(timeline.position(), Some(2));
        // Steps stop at the ends; a step that goes nowhere is not a change.
        assert_eq!(timeline.step(1), None);
        assert_eq!(timeline.step(-1), Some(1));
        assert!(!timeline.following());
        assert_eq!(timeline.step(-5), Some(0));
        assert_eq!(timeline.step(-1), None);
        assert_eq!(timeline.position(), Some(0));
        // Landing on the newest frame follows again.
        assert_eq!(timeline.step(2), Some(2));
        assert!(timeline.following());
        // Seeking: a listed id, the same id, an unknown id.
        assert_eq!(timeline.seek(&entry(5).id), Ok(Some(1)));
        assert_eq!(timeline.seek(&entry(5).id), Ok(None));
        assert_eq!(timeline.seek("KJAX-nowhere"), Err(()));
        assert_eq!(timeline.position(), Some(1));
    }

    #[test]
    fn a_sweep_in_progress_is_the_newest_entry_until_it_completes() {
        let mut timeline = Timeline::new(vec![entry(0), entry(5)]);
        timeline.begin(entry(10));
        assert_eq!(
            ids(&timeline),
            vec![
                (entry(0).id, FrameStatus::Complete),
                (entry(5).id, FrameStatus::Complete),
                (entry(10).id, FrameStatus::Partial),
            ]
        );
        assert!(timeline.is_partial(2) && !timeline.is_partial(1));
        // Following: the partial sweep is on screen; stepping back leaves it
        // and End (seek to the newest) returns to it.
        assert_eq!(timeline.position(), Some(2));
        assert_eq!(timeline.step(-1), Some(1));
        assert!(!timeline.following());
        assert_eq!(timeline.seek(&entry(10).id), Ok(Some(2)));
        assert!(timeline.following());
        // The cut completes: the entry becomes complete, still followed.
        assert!(!timeline.complete(entry(10)));
        assert_eq!(timeline.len(), 3);
        assert!(
            timeline
                .entries(&fixture_frame())
                .iter()
                .all(|e| e.status == FrameStatus::Complete)
        );
        assert!(timeline.following());
        // A pinned frame stays pinned while sweeps arrive.
        assert_eq!(timeline.step(-2), Some(0));
        timeline.begin(entry(15));
        assert_eq!(timeline.position(), Some(0));
        assert_eq!(timeline.len(), 4);
        assert!(!timeline.complete(entry(15)));
        assert_eq!(timeline.position(), Some(0));
        // A complete frame lands in time order even when it arrives late.
        assert!(!timeline.complete(entry(12)));
        assert_eq!(timeline.id_at(3), Some(entry(12).id.as_str()));
        assert_eq!(timeline.id_at(4), Some(entry(15).id.as_str()));
    }

    #[test]
    fn the_ring_drops_the_oldest_and_moves_a_pin_that_fell_off() {
        let mut timeline = Timeline::new((0..catalog::RING as i64).map(entry).collect());
        assert_eq!(timeline.step(-(catalog::RING as i64)), Some(0));
        assert!(timeline.complete(entry(catalog::RING as i64)));
        assert_eq!(timeline.len(), catalog::RING);
        assert_eq!(timeline.position(), Some(0));
        assert_eq!(timeline.id_at(0), Some(entry(1).id.as_str()));
        // Pinned elsewhere, the pin keeps its frame while the index shifts.
        assert_eq!(timeline.step(5), Some(5));
        assert!(!timeline.complete(entry(catalog::RING as i64 + 1)));
        assert_eq!(timeline.position(), Some(4));
        assert_eq!(timeline.id_at(4), Some(entry(6).id.as_str()));
    }

    #[test]
    fn playback_paces_the_loop_to_ten_seconds() {
        assert_eq!(play_step(1).as_millis(), 1000);
        assert_eq!(play_step(12).as_millis(), 833);
        assert_eq!(play_step(36).as_millis(), 277);
        assert_eq!(play_step(60).as_millis(), 250);
    }
    #[test]
    fn a_backfilled_frame_keeps_the_sweep_in_progress() {
        let mut timeline = Timeline::new(vec![entry(10)]);
        timeline.begin(entry(20));
        assert!(!timeline.insert(entry(5)));
        assert_eq!(
            ids(&timeline),
            vec![
                (entry(5).id, FrameStatus::Complete),
                (entry(10).id, FrameStatus::Complete),
                (entry(20).id, FrameStatus::Partial),
            ]
        );
        assert!(timeline.following());
    }
    #[test]
    fn playback_loops_over_complete_frames() {
        let mut timeline = Timeline::new(vec![entry(0)]);
        assert_eq!(timeline.advance(), None, "one frame is nothing to loop");
        assert!(!timeline.complete(entry(5)));
        assert!(!timeline.complete(entry(10)));
        timeline.begin(entry(15));
        // From the sweep in progress the loop starts at the oldest frame and
        // skips the partial sweep at the end.
        assert_eq!(timeline.advance(), Some(0));
        assert_eq!(timeline.advance(), Some(1));
        assert_eq!(timeline.advance(), Some(2));
        assert_eq!(timeline.advance(), Some(0));
        // Without a sweep in progress the newest frame is followed as the
        // loop passes it.
        assert!(!timeline.complete(entry(15)));
        assert_eq!(timeline.advance(), Some(1));
        assert_eq!(timeline.advance(), Some(2));
        assert_eq!(timeline.advance(), Some(3));
        assert!(timeline.following());
        assert_eq!(timeline.advance(), Some(0));
        assert!(!timeline.following());
    }

    #[test]
    fn a_reachable_feed_is_judged_by_the_newest_radial_age() {
        use ConnectionStatus::*;
        // SMHI keeps DEC-9's 15 and 30 minutes; a provider publishing every
        // 15 minutes is stale at 25 and unavailable at 40.
        let smhi = providers::smhi::SPEC.staleness;
        assert_eq!(
            (smhi.stale.as_secs(), smhi.unavailable.as_secs()),
            (900, 1800)
        );
        let slow = Staleness::from_cadence(Duration::from_secs(900));
        assert_eq!(
            (slow.stale.as_secs(), slow.unavailable.as_secs()),
            (1500, 2400)
        );
        for staleness in [smhi, slow] {
            let judge = |from, age| feed_condition(from, age, staleness);
            let stale = staleness.stale.as_secs();
            let unavailable = staleness.unavailable.as_secs();
            for from in [Ok, Stale, Unavailable] {
                assert_eq!(judge(from, Some(0)), Ok);
                assert_eq!(judge(from, Some(stale - 1)), Ok);
                assert_eq!(judge(from, Some(stale)), Stale);
                assert_eq!(judge(from, Some(unavailable - 1)), Stale);
                assert_eq!(judge(from, Some(unavailable)), Unavailable);
                // Nothing received yet: nothing to judge.
                assert_eq!(judge(from, None), from);
            }
            // A switch or the poller set these; only a sweep clears them.
            for held in [Loading, Offline] {
                for age in [None, Some(0), Some(stale), Some(unavailable)] {
                    assert_eq!(judge(held, age), held);
                }
            }
        }
        // The same 20-minute-old frame: stale from SMHI, fine from the
        // 15-minute provider.
        assert_eq!(feed_condition(Ok, Some(1200), smhi), Stale);
        assert_eq!(feed_condition(Ok, Some(1200), slow), Ok);
        assert_eq!(
            serde_json::to_string(&Unavailable).unwrap(),
            "\"unavailable\""
        );
    }

    #[test]
    fn a_wedged_poller_is_restarted_once_the_feed_is_unavailable() {
        let s = providers::smhi::SPEC.staleness;
        let cooldown = s.unavailable;
        assert!(
            should_restart_live(true, None, Duration::from_secs(0), s),
            "a finished poller restarts at once"
        );
        assert!(
            !should_restart_live(
                false,
                Some(s.unavailable.as_secs()),
                Duration::from_secs(0),
                s
            ),
            "do not restart every tick after going unavailable"
        );
        assert!(
            !should_restart_live(false, Some(s.stale.as_secs()), cooldown, s),
            "stale is not old enough; the inner iterator watchdog fires first"
        );
        assert!(should_restart_live(
            false,
            Some(s.unavailable.as_secs()),
            cooldown,
            s
        ));
        // A slower provider waits for its own unavailable age.
        let slow = Staleness::from_cadence(Duration::from_secs(900));
        assert!(!should_restart_live(
            false,
            Some(s.unavailable.as_secs()),
            cooldown,
            slow
        ));
    }

    #[test]
    fn a_known_sweep_does_not_clear_unavailable() {
        use ConnectionStatus::*;
        assert!(
            known_sweep_clears_loading(Loading),
            "select_site is still loading until the first join reports"
        );
        for held in [Ok, Stale, Unavailable, Offline] {
            assert!(
                !known_sweep_clears_loading(held),
                "{held:?} must not flash ok when rediscovery repeats a catalogued sweep"
            );
        }
    }

    #[test]
    fn retires_thirty_seconds_after_the_last_reference() {
        let mut retirement = Retirement::default();
        let t0 = Instant::now();
        let both = files(&["tex/a.png", "tex/b.png"]);
        // Both referenced: nothing is tracked.
        assert!(
            retirement
                .sweep(both.clone(), &set(&["tex/a.png", "tex/b.png"]), t0)
                .is_empty()
        );
        assert!(retirement.unreferenced.is_empty());
        // b stops being referenced at t0 + 5 s; it survives until t0 + 35 s.
        let only_a = set(&["tex/a.png"]);
        let t5 = t0 + Duration::from_secs(5);
        assert!(retirement.sweep(both.clone(), &only_a, t5).is_empty());
        assert!(
            retirement
                .sweep(both.clone(), &only_a, t5 + Duration::from_secs(29))
                .is_empty()
        );
        assert_eq!(
            retirement.sweep(both.clone(), &only_a, t5 + RETIRE_AFTER),
            files(&["tex/b.png"])
        );
        assert!(retirement.unreferenced.is_empty());
        // Deleted files stop being present; the current texture is never retired.
        assert!(
            retirement
                .sweep(files(&["tex/a.png"]), &only_a, t5 + Duration::from_secs(90))
                .is_empty()
        );
    }

    #[test]
    fn a_reference_that_returns_resets_the_clock() {
        let mut retirement = Retirement::default();
        let t0 = Instant::now();
        let present = files(&["tex/a.png", "tex/b.png"]);
        assert!(
            retirement
                .sweep(present.clone(), &set(&["tex/a.png"]), t0)
                .is_empty()
        );
        // b is referenced again at 20 s, then dropped again at 25 s.
        let t20 = t0 + Duration::from_secs(20);
        assert!(
            retirement
                .sweep(present.clone(), &set(&["tex/a.png", "tex/b.png"]), t20)
                .is_empty()
        );
        let t25 = t0 + Duration::from_secs(25);
        assert!(
            retirement
                .sweep(present.clone(), &set(&["tex/a.png"]), t25)
                .is_empty()
        );
        // 30 s after the first drop is not enough; 30 s after the second is.
        assert!(
            retirement
                .sweep(present.clone(), &set(&["tex/a.png"]), t0 + RETIRE_AFTER)
                .is_empty()
        );
        assert_eq!(
            retirement.sweep(present, &set(&["tex/a.png"]), t25 + RETIRE_AFTER),
            files(&["tex/b.png"])
        );
    }

    #[test]
    fn leftovers_from_an_earlier_daemon_count_from_first_sight() {
        let mut retirement = Retirement::default();
        let start = Instant::now();
        let present = files(&["tex/old.png", "tex/old.png.tmp", "tex/new.png"]);
        let referenced = set(&["tex/new.png"]);
        assert!(
            retirement
                .sweep(present.clone(), &referenced, start)
                .is_empty()
        );
        assert!(
            retirement
                .sweep(
                    present.clone(),
                    &referenced,
                    start + Duration::from_secs(29)
                )
                .is_empty()
        );
        let mut expired = retirement.sweep(present, &referenced, start + RETIRE_AFTER);
        expired.sort();
        assert_eq!(expired, files(&["tex/old.png", "tex/old.png.tmp"]));
    }
}
#[cfg(test)]
mod handoff_tests {
    use super::{fixture_frame, great_circle_km, handoff, site_table, with_archived_station};
    use crate::protocol::{SiteKind, Station};

    fn station(id: &str, lat: f64, lon: f64) -> Station {
        Station {
            id: id.into(),
            name: id.into(),
            state: String::new(),
            lat,
            lon,
            alt_m: 0.0,
            kind: SiteKind::Polar,
            ..Station::default()
        }
    }
    /// A point `fraction` of the way from `a` to `b` along the parallel.
    fn between(a: &Station, b: &Station, fraction: f64) -> (f64, f64) {
        (
            a.lat + (b.lat - a.lat) * fraction,
            a.lon + (b.lon - a.lon) * fraction,
        )
    }

    #[test]
    fn distances_match_known_values() {
        let d = great_circle_km(35.333361, -97.277761, 36.740617, -98.127717);
        assert!((d - 174.1).abs() < 0.2, "KTLX to KVNX: {d}");
        assert_eq!(great_circle_km(10.0, 20.0, 10.0, 20.0), 0.0);
    }

    #[test]
    fn the_midpoint_between_two_stations_keeps_whichever_is_held() {
        let a = station("AAAA", 35.0, -97.0);
        let b = station("BBBB", 35.0, -95.0);
        let sites = [a.clone(), b.clone()];
        for fraction in [0.45, 0.5, 0.55] {
            let (lat, lon) = between(&a, &b, fraction);
            assert!(
                handoff(&sites, "AAAA", lat, lon).is_none(),
                "{fraction} from A"
            );
            assert!(
                handoff(&sites, "BBBB", lat, lon).is_none(),
                "{fraction} from B"
            );
        }
        // Past the dead band the nearer station takes over, and only it.
        let (lat, lon) = between(&a, &b, 0.6);
        assert_eq!(
            handoff(&sites, "AAAA", lat, lon).map(|s| &s.id[..]),
            Some("BBBB")
        );
        assert!(handoff(&sites, "BBBB", lat, lon).is_none());
        let (lat, lon) = between(&a, &b, 0.4);
        assert_eq!(
            handoff(&sites, "BBBB", lat, lon).map(|s| &s.id[..]),
            Some("AAAA")
        );
        assert!(handoff(&sites, "AAAA", lat, lon).is_none());
    }

    #[test]
    fn co_located_stations_need_a_kilometre_to_swap() {
        // KOUN and KCRI are 300 m apart; near them the ratio alone would flap.
        // SMHI has no such pair, so the NEXRAD positions stand in.
        let sites = [
            station("KTLX", 35.333361, -97.277761),
            station("KOUN", 35.236058, -97.46235),
            station("KCRI", 35.238333, -97.46),
        ];
        let koun = &sites[1];
        assert!(handoff(&sites, "KCRI", koun.lat, koun.lon).is_none());
        assert!(handoff(&sites, "KOUN", koun.lat + 0.002, koun.lon).is_none());
        // From Oklahoma City's radar the Norman pair is a real hand-off.
        assert!(handoff(&sites, "KTLX", koun.lat, koun.lon).is_some());
    }

    #[test]
    fn the_fixture_home_view_stays_on_ktlx() {
        // The archived KTLX scan joins the daemon's table, not hello's.
        let sites = with_archived_station(site_table().sites, &fixture_frame());
        assert_eq!(sites.len(), site_table().sites.len() + 1);
        // The window's home view sits 5 km west and 15 km north of the site.
        assert!(handoff(&sites, "KTLX", 35.4681, -97.3326).is_none());
        // A station outside the table (or archived) hands off at once.
        assert_eq!(
            handoff(&sites, "ZZZZ", 35.333361, -97.277761).map(|s| &s.id[..]),
            Some("KTLX")
        );
    }

    #[test]
    fn the_site_table_is_the_nordic_network() {
        let sites = site_table().sites;
        // SMHI's 12 radars and ORD's 29 (NO, FI, DK: S15) from sites.json,
        // then the composites: SMHI's (S8) and OPERA's Nordic crop (S16),
        // and My mosaic (S25). S36 took Spain and Iberia out.
        assert_eq!(sites.len(), 44);
        assert_eq!(
            sites.iter().filter(|s| s.kind == SiteKind::Polar).count(),
            41
        );
        assert_eq!(
            sites
                .iter()
                .filter(|s| s.kind == SiteKind::Grid)
                .map(|s| &s.id[..])
                .collect::<Vec<_>>(),
            ["sweden", "nordic", "mymosaic"]
        );
        // My mosaic is never a target, and nothing is handed off to from it.
        assert!(handoff(&sites, "mymosaic", 57.71, 11.97).is_none());
        assert!(super::parse_warm("mymosaic,vara", &sites) == ["vara"]);
        // The Nordic composite is never a target either, and while it is
        // selected nothing is handed off to.
        assert!(handoff(&sites, "nordic", 59.33, 18.07).is_none());
        assert!(handoff(&sites, "", 62.25, 18.0).is_some_and(|s| s.kind == SiteKind::Polar));
        for s in &sites {
            // Every station is inside the Nordic box: S36 took Spain's
            // radars and the Iberian composite out, and with them the
            // Iberian and Canary envelopes.
            assert!(
                (53.0..=71.5).contains(&s.lat) && (3.0..=33.0).contains(&s.lon),
                "{}",
                s.id
            );
            assert_ne!(s.country, "ES", "{}", s.id);
        }
        // A place in Spain has no radar to follow at all now: the nearest
        // station is a Nordic one, and nothing hands off to a composite.
        let nearest = |lat, lon| handoff(&sites, "", lat, lon).map(|s| s.id.clone());
        assert!(
            nearest(40.42, -3.70).is_some_and(|id| {
                sites
                    .iter()
                    .any(|s| s.id == id && s.kind == SiteKind::Polar)
            }),
            "Madrid"
        );
        assert!(handoff(&sites, "nordic", 40.42, -3.70).is_none());
        assert!(handoff(&sites, "sweden", 40.42, -3.70).is_none());
        // Following from Gothenburg settles on Vara; from Stockholm, Bålsta.
        let nearest = |lat, lon| handoff(&sites, "", lat, lon).map(|s| s.id.clone());
        assert_eq!(nearest(57.71, 11.97).as_deref(), Some("vara"));
        assert_eq!(nearest(59.33, 18.07).as_deref(), Some("balsta"));
        // The composite sits at the middle of Sweden but is never a target,
        // and while it is selected nothing is handed off to.
        assert!(nearest(62.0, 16.0).is_some_and(|id| id != "sweden"));
        assert!(handoff(&sites, "sweden", 57.71, 11.97).is_none());
        assert!(handoff(&sites, "sweden", 62.0, 16.0).is_none());
        // Leaving a radar still hands off to the next radar.
        assert_eq!(
            handoff(&sites, "vara", 59.33, 18.07).map(|s| &s.id[..]),
            Some("balsta")
        );
        // Across the border, the nearest radar of any country: Oslo is
        // Hurum's, Helsinki Vihti's, Copenhagen Stevns'.
        assert_eq!(nearest(59.91, 10.75).as_deref(), Some("nohur"));
        assert_eq!(nearest(60.17, 24.94).as_deref(), Some("fivih"));
        assert_eq!(nearest(55.68, 12.57).as_deref(), Some("dkste"));
    }
}
