//! The OPERA provider (stream S16, DEC-14): EUMETNET OPERA's CIRRUS
//! maximum-reflectivity composite of Europe, cut to the Nordic box and
//! listed as the grid station `nordic`, next to SMHI's `sweden`; since S33
//! (DEC-17) also cut to the Iberian box as `iberia` (`CUTS`), where
//! Portugal's radars, which have no volumes in Open Radar Data, show.
//!
//! - One file holds every box. The engine's OPERA pollers (the selected
//!   box and any keep-warm one) share a `Hub`: a poller's read is cut for
//!   each other box being polled that lacks that time, and its listing
//!   serves the others for a `POLL`, so both boxes shown cost the listings
//!   and file reads of one, plus the other box's chunks in the same read.
//!
//! - Source: the open 24-hour S3 cache of EUMETNET's Open Radar Data,
//!   `openradar-24h/YYYY/MM/DD/OPERA/COMP/OPERA@YYYYMMDDTHHMM@0@DBZH.h5`:
//!   one file every 5 minutes, published about 4 minutes after its nominal
//!   time, CC BY 4.0 ("EUMETNET OPERA"). No key, and not the MeteoGate
//!   API's 200 requests an hour (OPEN-4).
//! - Listing: S3 `ListObjectsV2` on the day's prefix, starting after a key
//!   (`start-after`), so an answer holds the last few files (~6 KB) rather
//!   than the whole day (~100 KB by 10:00 UTC, and past S3's 1000-key page
//!   by evening; a cut-short answer is paged through anyway).
//! - Each file is ~1.9 MB; the decoder reads its metadata and the 11 of 30
//!   chunks under the Nordic box through the range reader (`composite.rs`,
//!   DEC-2), ~0.9 MB a frame.
//! - The poller asks only when a new file can be there: at the start, then
//!   from `PUBLISH_DELAY_MS` after each next nominal time, once a `POLL`
//!   until it comes. A join backfills `BACKFILL` frames.

use super::{Event, ProviderId, RangePlan, Scan, Spec, Staleness};
use crate::composite::{self, Grid, LonLatBox};
use crate::protocol::{SiteKind, Station};
use crate::smhi_live::{
    AbortOnDrop, FETCHER, Fail, Fetched, Http, HttpRanges, RangeReader, Volume, back_off,
    backfill_targets, covered,
};
use chrono::{DateTime, NaiveDateTime};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::runtime::Handle;
use tokio::sync::mpsc::Sender;
use tokio::task::spawn_blocking;
use tokio::time::sleep;

/// The credit OPERA's licence asks for.
pub const ATTRIBUTION: &str = "EUMETNET OPERA, CC BY 4.0";
/// `iberia`'s credit also names the OPERA members whose radars it shows
/// (S33): Spain's AEMET, Portugal's IPMA, and Météo-France, whose radars
/// cover the box's corner of southern France (review N6).
pub const IBERIA_ATTRIBUTION: &str = "EUMETNET OPERA (AEMET, IPMA, Météo-France), CC BY 4.0";
/// The public 24-hour cache of EUMETNET's Open Radar Data.
pub const BASE: &str = "https://s3.waw3-1.cloudferro.com/openradar-24h";
/// Development only (S33, like S32's `OMASTORM_ORD_BASE`): points the
/// provider at a stand-in for `BASE`, for tests and offline replays against
/// a local copy of the cache. Unset (always, in use), it reads `BASE`.
pub const BASE_ENV: &str = "OMASTORM_OPERA_BASE";
/// What the texture covers: the embedded geography's box (`engine/build.rs`,
/// 3–33° E, 53–71.5° N), 1670 × 2297 texels of 2 km (DEC-14).
pub const NORDIC: LonLatBox = LonLatBox {
    west: 3.0,
    east: 33.0,
    south: 53.0,
    north: 71.5,
};
/// `iberia`'s box (S33, DEC-17): mainland Portugal and Spain with the
/// Balearics, 835 × 690 texels of 2 km, all on OPERA's grid, in 5 of its
/// 30 chunks (~47 KB a file). The Canaries and Madeira are off the grid.
pub const IBERIA: LonLatBox = LonLatBox {
    west: -10.5,
    east: 4.5,
    south: 35.0,
    north: 44.5,
};

/// A box the composite is cut to, listed as a grid station whose id is a
/// region word (DEC-12).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cut {
    pub id: &'static str,
    pub name: &'static str,
    /// Where `hello` places the station: the middle of the box, where the
    /// clients centre their first view of it.
    pub lat: f64,
    pub lon: f64,
    pub area: LonLatBox,
    pub attribution: &'static str,
    /// Whether the engine makes S24b's products over the box from its
    /// radars' volumes (`mosaic::grid_box`). `iberia` offers only the
    /// composite (S33).
    pub products: bool,
    /// Whether following hands off to it where no radar reaches the view
    /// centre (S33: Portugal's radars can be seen only through `iberia`).
    pub handoff: bool,
}

/// OPERA's boxes, in `hello`'s order. `nordic` is DEC-14's, unchanged.
pub const CUTS: [Cut; 2] = [
    Cut {
        id: "nordic",
        name: "Nordic",
        lat: 62.25,
        lon: 18.0,
        area: NORDIC,
        attribution: ATTRIBUTION,
        products: true,
        handoff: false,
    },
    Cut {
        id: "iberia",
        name: "Iberia",
        lat: 39.75,
        lon: -3.0,
        area: IBERIA,
        attribution: IBERIA_ATTRIBUTION,
        products: false,
        handoff: true,
    },
];

/// The box station `id` is cut to, if it is one of OPERA's.
pub fn cut(id: &str) -> Option<&'static Cut> {
    CUTS.iter().find(|c| c.id == id)
}
/// One composite every 5 minutes.
const CADENCE: Duration = Duration::from_secs(5 * 60);
const CADENCE_MS: i64 = 5 * 60 * 1000;
/// A file lands about 4 minutes after its nominal time (08:10Z at
/// 08:14:02, measured 2026-09-14); asking from 3 minutes on finds it on the
/// first or second try.
const PUBLISH_DELAY_MS: i64 = 3 * 60 * 1000;
/// Between listings while a file is due.
const POLL: Duration = Duration::from_secs(60);
const MAX_BACK_OFF: Duration = Duration::from_secs(600);
/// One failure is retried quietly; this many in a row report `offline`.
const OFFLINE_AFTER: u32 = 2;
/// Decode failures of one file before the poller stops retrying it.
const GIVE_UP_AFTER: u32 = 2;
/// Frames a join fetches, counting the live one: 24 × ~0.9 MB ≈ 21 MB, two
/// hours of loop (DEC-14). The catalog still keeps up to 60 as live frames
/// accumulate.
pub const BACKFILL: usize = 24;
const _: () = assert!(BACKFILL <= crate::catalog::RING);
const BACKFILL_DELAY: Duration = Duration::from_secs(3);
/// Between backfill files, to be gentle with the cache.
const BACKFILL_PACE: Duration = Duration::from_millis(500);
/// The cache keeps 24 hours.
const HORIZON_MS: i64 = 24 * 60 * 60 * 1000;
/// How far back the first listing of a join looks.
const FIRST_LOOK_MS: i64 = 60 * 60 * 1000;
/// Pages of one day listing, at most (1000 keys each).
const MAX_PAGES: usize = 5;
/// The first pause before re-asking for a file's unknown length (the
/// cache always names it, so this is only a safeguard).
const LENGTH_PAUSE: Duration = Duration::from_secs(1);
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The range reader's plan for one file: the metadata sits in the first
/// ~13 KB, the rest is a few runs of adjacent chunks (one request each).
/// The budget only stops a decoder that wanders; a stormy Nordic crop is
/// under 1.5 MB.
pub const PLAN: RangePlan = RangePlan {
    prefetch: 16 * 1024,
    block: 4 * 1024,
    max_requests: 32,
    max_bytes: 4 * 1024 * 1024,
};

pub const SPEC: Spec = Spec {
    id: ProviderId::Opera,
    name: "EUMETNET OPERA",
    attribution: ATTRIBUTION,
    // A multi-country composite (docs/protocol.md).
    country: "",
    range_km: 0.0,
    cadence: CADENCE,
    staleness: Staleness::from_cadence(CADENCE),
    backfill: BACKFILL,
    ranges: PLAN,
};

impl Cut {
    /// The composite as `hello` lists it: the middle of the box, with no
    /// antenna.
    pub fn station(&self) -> Station {
        Station {
            id: self.id.to_owned(),
            name: self.name.to_owned(),
            state: String::new(),
            lat: self.lat,
            lon: self.lon,
            alt_m: 0.0,
            kind: SiteKind::Grid,
            country: String::new(),
            provider: ProviderId::Opera,
            range_km: 0.0,
            attribution: self.attribution.to_owned(),
            aliases: Vec::new(),
            source: String::new(),
        }
    }
}

/// Every OPERA box as `hello` lists it.
pub fn stations() -> Vec<Station> {
    CUTS.iter().map(Cut::station).collect()
}

/// The decoder: one read of a composite, cut to each of `areas` in order.
pub fn decode(reader: RangeReader, areas: &[LonLatBox]) -> Result<Vec<Grid>, String> {
    composite::decode_boxes(reader, areas).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// One read, every box (S33)
// ---------------------------------------------------------------------------

/// A cut made for another box's poller waits this long for it...
const CUT_TTL: Duration = Duration::from_secs(10 * 60);
/// ...and is made only while the cuts waiting fit in this many bytes of
/// codes (review SF2): about four Nordic cuts (3.8 MB each) or 27 Iberian
/// ones (0.58 MB). A made cut is never dropped to make room, so no read's
/// bytes for another box are wasted; past the room a box reads its own.
const CUT_ROOM: usize = 16 * 1024 * 1024;

/// What the engine's OPERA pollers share (S33): the boxes being polled and
/// the times each already has, cuts one poller's read made for another,
/// and the newest listing.
#[derive(Default)]
pub struct Hub {
    watching: Mutex<HashMap<String, Watch>>,
    cuts: Mutex<Vec<Cutout>>,
    listing: Mutex<Option<Listing>>,
    /// Held across "another box's listing, else list": pollers woken
    /// together (after one read served both) would otherwise both ask.
    listing_turn: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Watch {
    pollers: usize,
    /// Start times the box has: catalogued when its poller started,
    /// fetched or cut since.
    known: Vec<i64>,
    /// Valid times the box's running backfill still wants (review SF2):
    /// another box's backfill read is cut for it only at these.
    wants: Vec<i64>,
}

/// A backfill's declared wants; they end with it.
struct Wanting {
    hub: Arc<Hub>,
    site: String,
}

impl Drop for Wanting {
    fn drop(&mut self) {
        if let Some(w) = lock(&self.hub.watching).get_mut(&self.site) {
            w.wants.clear();
        }
    }
}

struct Cutout {
    key: String,
    site: String,
    grid: Grid,
    provenance: String,
    made: Instant,
}

struct Listing {
    site: String,
    at: Instant,
    since_ms: i64,
    volumes: Vec<Volume>,
}

/// The engine's hub.
static HUB: LazyLock<Arc<Hub>> = LazyLock::new(Default::default);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A poller's place in the hub. When the last poller of a box ends (or is
/// aborted), the box leaves the hub with any cuts still waiting for it.
struct Watching {
    hub: Arc<Hub>,
    site: String,
}

impl Drop for Watching {
    fn drop(&mut self) {
        let mut watching = lock(&self.hub.watching);
        if let Some(w) = watching.get_mut(&self.site) {
            w.pollers = w.pollers.saturating_sub(1);
            if w.pollers == 0 {
                watching.remove(&self.site);
                lock(&self.hub.cuts).retain(|c| c.site != self.site);
            }
        }
    }
}

impl Hub {
    fn watch(self: &Arc<Self>, site: &str, known: &[i64]) -> Watching {
        let mut watching = lock(&self.watching);
        let w = watching.entry(site.to_owned()).or_default();
        w.pollers += 1;
        w.known.extend_from_slice(known);
        Watching {
            hub: self.clone(),
            site: site.to_owned(),
        }
    }

    /// `site` now has the composite starting at `ms`.
    fn got(&self, site: &str, ms: i64) {
        if let Some(w) = lock(&self.watching).get_mut(site) {
            w.known.push(ms);
            w.wants.retain(|&t| !covered(t, &[ms]));
        }
    }

    /// Review SF2: `site`'s backfill wants the composites valid at `times`,
    /// until the returned guard drops (the backfill ends).
    fn want(self: &Arc<Self>, site: &str, times: &[i64]) -> Wanting {
        if let Some(w) = lock(&self.watching).get_mut(site) {
            w.wants = times.to_vec();
        }
        Wanting {
            hub: self.clone(),
            site: site.to_owned(),
        }
    }

    /// The boxes a live read of `volume` is cut to: `site`'s own first,
    /// then every other box being polled that lacks that time, has no cut
    /// of it waiting, and whose cut fits the room. Empty when `site` is not
    /// one of OPERA's boxes.
    fn boxes_for(&self, site: &str, volume: &Volume) -> Vec<&'static Cut> {
        self.cut_for(site, volume, false)
    }

    /// `boxes_for` of a backfill read (review SF2): another box only where
    /// its own backfill is running and wants that time, so a backfill ahead
    /// of the other box's cuts nothing that would wait for long.
    fn boxes_for_backfill(&self, site: &str, volume: &Volume) -> Vec<&'static Cut> {
        self.cut_for(site, volume, true)
    }

    fn cut_for(&self, site: &str, volume: &Volume, backfill: bool) -> Vec<&'static Cut> {
        let Some(own) = cut(site) else {
            return Vec::new();
        };
        let watching = lock(&self.watching);
        let cuts = lock(&self.cuts);
        let mut room = CUT_ROOM.saturating_sub(cuts.iter().map(|c| c.grid.codes.len()).sum());
        let mut boxes = vec![own];
        for other in CUTS.iter().filter(|c| c.id != site) {
            let Some(w) = watching.get(other.id) else {
                continue;
            };
            let lacks = !covered(volume.valid_ms, &w.known);
            let wanted = !backfill || w.wants.contains(&volume.valid_ms);
            let waiting = cuts
                .iter()
                .any(|c| c.site == other.id && c.key == volume.key);
            let (width, height) = composite::texture_size(other.area);
            let size = width as usize * height as usize;
            if lacks && wanted && !waiting && size <= room {
                room -= size;
                boxes.push(other);
            }
        }
        boxes
    }

    /// Keep a cut of `key` for `site`'s poller, until it is taken or its
    /// `CUT_TTL` passes.
    fn keep(&self, key: &str, site: &str, grid: Grid, provenance: String) {
        let mut cuts = lock(&self.cuts);
        cuts.retain(|c| c.made.elapsed() < CUT_TTL);
        cuts.push(Cutout {
            key: key.to_owned(),
            site: site.to_owned(),
            grid,
            provenance,
            made: Instant::now(),
        });
    }

    /// The cut of `key` waiting for `site`, if another poller's read made
    /// one.
    fn take(&self, key: &str, site: &str) -> Option<(Grid, String)> {
        let mut cuts = lock(&self.cuts);
        cuts.retain(|c| c.made.elapsed() < CUT_TTL);
        let at = cuts.iter().position(|c| c.key == key && c.site == site)?;
        let c = cuts.remove(at);
        Some((c.grid, c.provenance))
    }

    fn remember_listing(&self, site: &str, since_ms: i64, volumes: &[Volume]) {
        *lock(&self.listing) = Some(Listing {
            site: site.to_owned(),
            at: Instant::now(),
            since_ms,
            volumes: volumes.to_vec(),
        });
    }

    /// Another box's listing, from less than `fresh` ago and starting no
    /// later than `since_ms`, cut to `since_ms` on: what `site`'s own
    /// listing would say, up to a `POLL` late.
    fn listed_by_another(&self, site: &str, since_ms: i64, fresh: Duration) -> Option<Vec<Volume>> {
        let listing = lock(&self.listing);
        let l = listing.as_ref()?;
        (l.site != site && l.at.elapsed() < fresh && l.since_ms <= since_ms).then(|| {
            l.volumes
                .iter()
                .filter(|v| v.valid_ms >= since_ms)
                .cloned()
                .collect()
        })
    }
}

// ---------------------------------------------------------------------------
// Keys and listings
// ---------------------------------------------------------------------------

fn utc(ms: i64, format: &str) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|t| t.format(format).to_string())
        .unwrap_or_default()
}

/// `2026/09/14/OPERA/COMP/`: where a day's composites are.
fn day_prefix(ms: i64) -> String {
    format!("{}/OPERA/COMP/", utc(ms, "%Y/%m/%d"))
}

/// The start of every key of the composites valid at `ms` (all products):
/// `2026/09/14/OPERA/COMP/OPERA@20260914T0810`.
fn stamp_prefix(ms: i64) -> String {
    format!("{}OPERA@{}", day_prefix(ms), utc(ms, "%Y%m%dT%H%M"))
}

/// The DBZH composite valid at `valid_ms`.
pub fn volume(base: &str, valid_ms: i64) -> Volume {
    let key = format!("{}@0@DBZH.h5", stamp_prefix(valid_ms));
    Volume {
        key: format!("OPERA@{}@0@DBZH", utc(valid_ms, "%Y%m%dT%H%M")),
        valid_ms,
        url: format!("{base}/{key}"),
    }
}

/// `ListObjectsV2` of the day holding `day_ms`, for keys sorting after
/// `after` (a key or a key's start, so the composites valid at a
/// `stamp_prefix` time are included).
pub fn listing_url(base: &str, day_ms: i64, after: &str) -> String {
    format!(
        "{base}/?list-type=2&prefix={}&start-after={}",
        day_prefix(day_ms),
        after.replace('@', "%40")
    )
}

/// The DBZH composite a key names, if it names one.
fn listed_volume(key: &str, base: &str) -> Option<Volume> {
    let name = key.rsplit('/').next()?;
    let stamp = name.strip_prefix("OPERA@")?.strip_suffix("@0@DBZH.h5")?;
    let valid_ms = NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M")
        .ok()?
        .and_utc()
        .timestamp_millis();
    let volume = volume(base, valid_ms);
    // Only a key exactly where `volume` builds it: every request stays
    // under `base`, at a URL this module can name.
    (volume.url == format!("{base}/{key}")).then_some(volume)
}

/// The DBZH composites a `ListObjectsV2` answer names, and the key to
/// continue after when the answer was cut short.
pub fn listed(body: &[u8], base: &str) -> Result<(Vec<Volume>, Option<String>), String> {
    let text = std::str::from_utf8(body).map_err(|_| "the listing is not UTF-8".to_owned())?;
    if !text.contains("<ListBucketResult") {
        let start: String = text.chars().take(80).collect();
        return Err(format!("not an S3 listing: {start:?}"));
    }
    let keys: Vec<&str> = text
        .split("<Key>")
        .skip(1)
        .filter_map(|s| s.split_once("</Key>").map(|(key, _)| key))
        .collect();
    let volumes = keys.iter().filter_map(|k| listed_volume(k, base)).collect();
    let more = text
        .contains("<IsTruncated>true</IsTruncated>")
        .then(|| keys.last().map(|k| (*k).to_owned()))
        .flatten();
    Ok((volumes, more))
}

/// Every composite valid from `since_ms` to `until_ms`, from each day
/// listing that can hold one, oldest first. Returns the requests it made.
async fn list_between(
    http: &Http,
    base: &str,
    since_ms: i64,
    until_ms: i64,
) -> Result<(Vec<Volume>, u32), Fail> {
    let mut found = Vec::new();
    let mut requests = 0;
    let mut day = since_ms - since_ms.rem_euclid(DAY_MS);
    while day <= until_ms {
        let mut after = stamp_prefix(since_ms.max(day));
        for _ in 0..MAX_PAGES {
            requests += 1;
            let Fetched::Body(body, _) = http.get(&listing_url(base, day, &after), None).await?
            else {
                break;
            };
            let (volumes, more) = listed(&body, base).map_err(Fail::Answer)?;
            found.extend(volumes);
            match more {
                Some(key) => after = key,
                None => break,
            }
        }
        day += DAY_MS;
    }
    found.retain(|v| v.valid_ms >= since_ms && v.valid_ms <= until_ms);
    found.sort_by_key(|v| v.valid_ms);
    found.dedup_by_key(|v| v.valid_ms);
    Ok((found, requests))
}

// ---------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------

/// One read cut to `areas`, the reader's own box first (S33). When that
/// read cannot be decoded and it served other boxes too, the own box alone
/// is read once more (review SF1), so a fault in another box's chunks never
/// costs the reader its frame; a network failure (`net_failed`) is not
/// retried here, the poller backs off. Returns the grids (only the own
/// box's after a retry), the traffic of the read that served, and the
/// first read's fault when there was a retry.
fn cut_boxes(
    mut open: impl FnMut() -> std::io::Result<RangeReader>,
    areas: &[LonLatBox],
    net_failed: impl Fn() -> bool,
) -> Result<(Vec<Grid>, String, Option<String>), String> {
    let mut read = |areas: &[LonLatBox]| -> Result<(Vec<Grid>, String), String> {
        let reader = open().map_err(|e| e.to_string())?;
        let traffic = reader.traffic();
        let grids = decode(reader, areas)?;
        let read = format!(
            "{} range requests, {} of {} bytes",
            traffic.requests(),
            traffic.bytes(),
            traffic.total()
        );
        Ok((grids, read))
    };
    match read(areas) {
        Ok((grids, traffic)) => Ok((grids, traffic, None)),
        Err(fault) if areas.len() > 1 && !net_failed() => {
            let (grids, traffic) = read(&areas[..1])?;
            Ok((grids, traffic, Some(fault)))
        }
        Err(fault) => Err(fault),
    }
}

enum Failure {
    /// The cache failed to answer: back off.
    Net(Fail),
    /// The answer or the bytes were unusable: give up on that file only.
    Decode(String),
}

/// `site`'s grid of one composite and its provenance, holding the engine's
/// single fetch permit: the cut another box's poller left in `hub`, or one
/// read with ranged requests under `PLAN`, cut for `site` and for every
/// other box being polled that lacks it (S33).
async fn fetch(
    http: &Http,
    hub: &Arc<Hub>,
    site: &str,
    volume: &Volume,
    backfill: bool,
) -> Result<(Scan, String), Failure> {
    let permit = FETCHER
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| Failure::Decode(e.to_string()))?;
    // Under the permit: the read that held it before may have cut this box.
    if let Some((grid, provenance)) = hub.take(&volume.key, site) {
        drop(permit);
        hub.got(site, grid.start_ms);
        return Ok((Scan::Grid(grid), provenance));
    }
    let boxes = if backfill {
        hub.boxes_for_backfill(site, volume)
    } else {
        hub.boxes_for(site, volume)
    };
    if boxes.is_empty() {
        return Err(Failure::Decode(format!(
            "{site} is not one of OPERA's boxes"
        )));
    }
    let failure = Arc::new(Mutex::new(None));
    let (http, url, runtime, net) = (
        http.clone(),
        volume.url.clone(),
        Handle::current(),
        failure.clone(),
    );
    let (shared, key, reader_site) = (hub.clone(), volume.key.clone(), site.to_owned());
    let joined = spawn_blocking(move || {
        let _permit = permit;
        let open = || {
            let source = HttpRanges {
                http: http.clone(),
                url: url.clone(),
                runtime: runtime.clone(),
                failure: net.clone(),
            };
            RangeReader::open_planned(Box::new(source), PLAN, LENGTH_PAUSE)
        };
        let areas: Vec<LonLatBox> = boxes.iter().map(|c| c.area).collect();
        let (grids, read, fault) = cut_boxes(open, &areas, || net.lock().unwrap().is_some())?;
        let mut grids = grids.into_iter();
        let own = grids.next().ok_or("the read gave no grid")?;
        let mut names: Vec<&str> = boxes.iter().map(|c| c.id).collect();
        if let Some(fault) = fault {
            log(
                &reader_site,
                format_args!(
                    "{key}: the read for {} failed ({fault}); read again for {reader_site} alone",
                    names.join(" and ")
                ),
            );
            names.truncate(1);
        }
        // The other boxes' cuts wait in the hub before the permit goes, so
        // a poller waiting for it finds them.
        for (other, grid) in boxes[1..].iter().zip(grids) {
            let provenance = format!(
                "EUMETNET OPERA {key}: cut from the read made for {reader_site} ({read}, for {})",
                names.join(" and ")
            );
            shared.keep(&key, other.id, grid, provenance);
        }
        let mut provenance = format!("EUMETNET OPERA {key}: {read}");
        if names.len() > 1 {
            provenance += &format!(", cut for {}", names.join(" and "));
        }
        Ok::<_, String>((own, provenance))
    })
    .await;
    match joined {
        Ok(Ok((grid, provenance))) => {
            hub.got(site, grid.start_ms);
            Ok((Scan::Grid(grid), provenance))
        }
        Ok(Err(e)) => Err(match failure.lock().unwrap().take() {
            Some(fail @ (Fail::Status(..) | Fail::Transport(_))) => Failure::Net(fail),
            _ => Failure::Decode(e),
        }),
        Err(e) => Err(Failure::Decode(format!("the decoder failed: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// The poller
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn log(site: &str, message: impl fmt::Display) {
    eprintln!(
        "{} Live {site}: {message}",
        utc(now_ms(), "%Y-%m-%dT%H:%M:%SZ")
    );
}

/// What the poller needs from its surroundings; tests shorten the waits.
#[derive(Clone)]
pub struct Config {
    pub base: String,
    pub poll: Duration,
    pub max_back_off: Duration,
    pub backfill_delay: Duration,
    pub backfill_pace: Duration,
    pub now_ms: fn() -> i64,
    /// What the engine's OPERA pollers share (S33).
    pub hub: Arc<Hub>,
}

impl Config {
    pub fn opera() -> Self {
        let base = std::env::var(BASE_ENV)
            .ok()
            .map(|b| b.trim().trim_end_matches('/').to_owned())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| BASE.to_owned());
        Config {
            base,
            poll: POLL,
            max_back_off: MAX_BACK_OFF,
            backfill_delay: BACKFILL_DELAY,
            backfill_pace: BACKFILL_PACE,
            now_ms,
            hub: HUB.clone(),
        }
    }
}

/// Whether a new composite can be there, given the newest one dealt with:
/// always at the start, else from `PUBLISH_DELAY_MS` after the next
/// nominal time.
pub fn due(newest_ms: Option<i64>, now: i64) -> bool {
    newest_ms.is_none_or(|newest| now >= newest + CADENCE_MS + PUBLISH_DELAY_MS)
}

/// Where a listing starts: the next nominal time after the newest one dealt
/// with, or `FIRST_LOOK_MS` back at the start; never before the cache's
/// horizon.
pub fn listing_since(newest_ms: Option<i64>, now: i64) -> i64 {
    newest_ms
        .map_or(now - FIRST_LOOK_MS, |newest| newest + CADENCE_MS)
        .max(now - HORIZON_MS + CADENCE_MS)
}

/// Poll OPERA for `station` until the task is aborted or the event channel
/// closes.
pub async fn poll(station: Station, events: Sender<Event>, cached: Vec<i64>, _skip_known: bool) {
    let cfg = Config::opera();
    if cfg.base != BASE {
        log(
            &station.id,
            format_args!("{BASE_ENV}: reading {}", cfg.base),
        );
    }
    poll_with(cfg, station.id, events, cached).await;
}

async fn send(events: &Sender<Event>, event: Event) -> bool {
    events.send(event).await.is_ok()
}

/// Poll `site` until the task is aborted or the event channel closes.
/// `cached` holds the start times of the frames already catalogued, so
/// neither the live path nor the backfill fetches them again.
pub async fn poll_with(cfg: Config, site: String, events: Sender<Event>, cached: Vec<i64>) {
    // S33: in the hub from the start, so another box's read is cut for it.
    let _watching = cfg.hub.watch(&site, &cached);
    let http = match Http::new() {
        Ok(http) => http,
        Err(e) => {
            send(&events, Event::Offline { site, reason: e }).await;
            return;
        }
    };
    let mut known = cached;
    // The newest composite dealt with (fetched, found in the catalog, or
    // given up on), and the newest the cache has listed.
    let mut newest: Option<Volume> = None;
    let mut latest_listed: Option<i64> = None;
    let mut tries: HashMap<String, u32> = HashMap::new();
    let mut failures = 0u32;
    let mut silent = false;
    let mut backfilling: Option<AbortOnDrop> = None;
    loop {
        let now = (cfg.now_ms)();
        let newest_ms = newest.as_ref().map(|v| v.valid_ms);
        let mut failed: Option<Fail> = None;
        let mut caught_up = !due(newest_ms, now);
        if !caught_up {
            // S33: another box's poller may have just asked the same; one
            // poller at a time asks, so the second finds the first's answer.
            let since = listing_since(newest_ms, now);
            let turn = cfg.hub.listing_turn.lock().await;
            let listed = match cfg.hub.listed_by_another(&site, since, cfg.poll) {
                Some(volumes) => Ok((volumes, 0)),
                None => list_between(&http, &cfg.base, since, now)
                    .await
                    .inspect(|(volumes, _)| cfg.hub.remember_listing(&site, since, volumes)),
            };
            drop(turn);
            match listed {
                Ok((volumes, _)) => match volumes.into_iter().next_back() {
                    None => caught_up = newest.is_some(),
                    Some(latest) => {
                        latest_listed = latest_listed.max(Some(latest.valid_ms));
                        let first = newest.is_none();
                        if covered(latest.valid_ms, &known) {
                            if first && !send(&events, Event::Current { site: site.clone() }).await
                            {
                                return;
                            }
                            newest = Some(latest);
                            caught_up = true;
                        } else {
                            match fetch(&http, &cfg.hub, &site, &latest, false).await {
                                Ok((scan, provenance)) => {
                                    known.push(scan.start_ms());
                                    let event = Event::Sweep {
                                        site: site.clone(),
                                        sweep: scan,
                                        complete: true,
                                        provenance,
                                    };
                                    if !send(&events, event).await {
                                        return;
                                    }
                                    newest = Some(latest);
                                    caught_up = true;
                                }
                                Err(Failure::Net(fail)) => failed = Some(fail),
                                Err(Failure::Decode(e)) => {
                                    let n = tries.entry(latest.key.clone()).or_default();
                                    *n += 1;
                                    log(&site, format_args!("{}: {e} (try {n})", latest.key));
                                    if *n >= GIVE_UP_AFTER {
                                        newest = Some(latest);
                                    }
                                }
                            }
                        }
                    }
                },
                Err(fail) => failed = Some(fail),
            }
        }

        // Silence is judged after any fetch, so a composite that stopped an
        // hour ago shows its last frame, then `unavailable`.
        let now = (cfg.now_ms)();
        let latest_ms = latest_listed.max(newest.as_ref().map(|v| v.valid_ms));
        let quiet = failed.is_none()
            && match latest_ms {
                Some(ms) => now - ms >= SPEC.staleness.unavailable.as_millis() as i64,
                None => true,
            };
        if quiet && !silent {
            let reason = match latest_ms {
                Some(ms) => format!(
                    "OPERA has published no composite since {}",
                    utc(ms, "%Y-%m-%d %H:%MZ")
                ),
                None => "OPERA lists no composite in the last hour".to_owned(),
            };
            if !send(
                &events,
                Event::Silent {
                    site: site.clone(),
                    reason,
                },
            )
            .await
            {
                return;
            }
        }
        silent = quiet;

        if backfilling.is_none()
            && caught_up
            && let Some(live) = &newest
            && now - live.valid_ms < HORIZON_MS
        {
            backfilling = Some(AbortOnDrop(tokio::spawn(backfill(
                cfg.clone(),
                http.clone(),
                site.clone(),
                events.clone(),
                live.valid_ms,
                known.clone(),
            ))));
        }

        let mut wait = cfg.poll;
        if let Some(fail) = failed {
            failures += 1;
            wait = back_off(cfg.poll, cfg.max_back_off, failures, fail.retry_after());
            let reason = format!("EUMETNET OPERA: {fail}");
            if failures < OFFLINE_AFTER {
                log(
                    &site,
                    format_args!("{reason}; retrying in {}s", wait.as_secs()),
                );
            } else if !send(
                &events,
                Event::Offline {
                    site: site.clone(),
                    reason,
                },
            )
            .await
            {
                return;
            }
        } else {
            failures = 0;
        }
        sleep(wait).await;
    }
}

/// The listing of the `BACKFILL` composites up to the live one, then each
/// the catalog lacks, newest first, as `Event::Backfill`. A network failure
/// ends the backfill (the live poller backs off on its own); a file that
/// does not decode is skipped.
async fn backfill(
    cfg: Config,
    http: Http,
    site: String,
    events: Sender<Event>,
    live_ms: i64,
    known: Vec<i64>,
) {
    sleep(cfg.backfill_delay).await;
    let since = live_ms - BACKFILL as i64 * CADENCE_MS;
    let listed = match list_between(&http, &cfg.base, since, live_ms).await {
        Ok((listed, _)) => listed,
        Err(fail) => {
            log(&site, format_args!("backfill listing: {fail}"));
            let end = Event::HistoryEnd {
                site,
                want: crate::products::Want::Lowest,
            };
            send(&events, end).await;
            return;
        }
    };
    let targets = backfill_targets(listed, live_ms, &known, BACKFILL);
    let wanted = targets.len();
    // Review SF2: another box's backfill cuts these for this one.
    let times: Vec<i64> = targets.iter().map(|v| v.valid_ms).collect();
    let _wanting = cfg.hub.want(&site, &times);
    // S31: what the history will bring, for `state.loading`.
    let plan = Event::HistoryPlan {
        site: site.clone(),
        want: crate::products::Want::Lowest,
        frames: wanted,
    };
    if !send(&events, plan).await {
        return;
    }
    let mut fetched = 0;
    for volume in targets {
        match fetch(&http, &cfg.hub, &site, &volume, true).await {
            Ok((scan, provenance)) => {
                let event = Event::Backfill {
                    site: site.clone(),
                    sweep: scan,
                    provenance,
                };
                if !send(&events, event).await {
                    return;
                }
                fetched += 1;
            }
            Err(Failure::Net(fail)) => {
                log(
                    &site,
                    format_args!("backfill {}: {fail}; stopping", volume.key),
                );
                break;
            }
            Err(Failure::Decode(e)) => log(&site, format_args!("backfill {}: {e}", volume.key)),
        }
        sleep(cfg.backfill_pace).await;
    }
    log(
        &site,
        format_args!("backfilled {fetched} of {wanted} earlier composites"),
    );
    let end = Event::HistoryEnd {
        site,
        want: crate::products::Want::Lowest,
    };
    send(&events, end).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    const LISTING: &str = include_str!("../../tests/opera/listing-20260914-after0930.xml");

    fn at(day: u32, hour: u32, minute: u32) -> i64 {
        NaiveDate::from_ymd_opt(2026, 9, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    #[test]
    fn keys_and_urls_are_the_caches() {
        let v = volume(BASE, at(14, 8, 10));
        assert_eq!(v.key, "OPERA@20260914T0810@0@DBZH");
        assert_eq!(
            v.url,
            "https://s3.waw3-1.cloudferro.com/openradar-24h/2026/09/14/OPERA/COMP/OPERA@20260914T0810@0@DBZH.h5"
        );
        assert_eq!(
            listing_url(BASE, at(14, 9, 30), &stamp_prefix(at(14, 9, 30))),
            "https://s3.waw3-1.cloudferro.com/openradar-24h/?list-type=2&prefix=2026/09/14/OPERA/COMP/&start-after=2026/09/14/OPERA/COMP/OPERA%4020260914T0930"
        );
    }

    #[test]
    fn a_listing_names_its_dbzh_composites_only() {
        let (volumes, more) = listed(LISTING.as_bytes(), BASE).unwrap();
        assert!(more.is_none());
        // 22 keys: ACRR, RATE and TIFF files besides DBZH from 09:30 to 10:00.
        let times: Vec<i64> = volumes.iter().map(|v| v.valid_ms).collect();
        let expected: Vec<i64> = (0..7).map(|k| at(14, 9, 30) + k * CADENCE_MS).collect();
        assert_eq!(times, expected);
        assert_eq!(volumes[0], volume(BASE, at(14, 9, 30)));
        assert!(listed(b"<html>rate limited</html>", BASE).is_err());
        // A cut-short page continues after its last key.
        let cut = LISTING.replace(
            "<IsTruncated>false</IsTruncated>",
            "<IsTruncated>true</IsTruncated>",
        );
        let (_, more) = listed(cut.as_bytes(), BASE).unwrap();
        assert_eq!(
            more.as_deref(),
            Some("2026/09/14/OPERA/COMP/OPERA@20260914T1000@0@DBZH.tiff")
        );
    }

    #[test]
    fn the_poller_asks_only_when_a_composite_can_be_there() {
        assert!(due(None, at(14, 9, 0)));
        let newest = Some(at(14, 8, 10));
        assert!(!due(newest, at(14, 8, 17)));
        assert!(due(newest, at(14, 8, 18)));
        assert!(due(newest, at(14, 9, 0)));
        // The listing starts at the next nominal time, or an hour back.
        assert_eq!(listing_since(newest, at(14, 8, 18)), at(14, 8, 15));
        assert_eq!(listing_since(None, at(14, 9, 0)), at(14, 8, 0));
        // Never before the cache's 24 hours.
        assert_eq!(
            listing_since(Some(at(12, 0, 0)), at(14, 9, 0)),
            at(13, 9, 5)
        );
    }

    #[test]
    fn the_spec_and_the_stations() {
        assert_eq!(SPEC.staleness.stale, Duration::from_secs(15 * 60));
        assert_eq!(SPEC.staleness.unavailable, Duration::from_secs(30 * 60));
        assert_eq!(SPEC.backfill, 24);
        let stations = stations();
        let ids: Vec<&str> = stations.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["nordic", "iberia"]);
        for (s, c) in stations.iter().zip(&CUTS) {
            assert_eq!((s.kind, s.provider), (SiteKind::Grid, ProviderId::Opera));
            assert_eq!((s.country.as_str(), s.range_km), ("", 0.0));
            // Each station sits in the middle of its own box.
            assert_eq!(s.lat, (c.area.south + c.area.north) / 2.0);
            assert_eq!(s.lon, (c.area.west + c.area.east) / 2.0);
            assert_eq!(cut(&s.id), Some(c));
        }
        // nordic as DEC-14 lists it; iberia names the members it shows.
        let nordic = &stations[0];
        assert_eq!(
            (nordic.name.as_str(), nordic.lat, nordic.lon),
            ("Nordic", 62.25, 18.0)
        );
        assert_eq!(nordic.attribution, "EUMETNET OPERA, CC BY 4.0");
        assert_eq!(
            stations[1].attribution,
            "EUMETNET OPERA (AEMET, IPMA, Météo-France), CC BY 4.0"
        );
        assert_eq!((CUTS[0].products, CUTS[0].handoff), (true, false));
        assert_eq!((CUTS[1].products, CUTS[1].handoff), (false, true));
        assert!(cut("sweden").is_none());
    }

    #[test]
    fn the_iberian_box_is_its_goldens() {
        let golden: serde_json::Value =
            serde_json::from_str(include_str!("../../../golden/iberia-20260915/grid.json"))
                .unwrap();
        let b = &golden["box"];
        assert_eq!(
            IBERIA,
            LonLatBox {
                west: b["west"].as_f64().unwrap(),
                east: b["east"].as_f64().unwrap(),
                south: b["south"].as_f64().unwrap(),
                north: b["north"].as_f64().unwrap(),
            }
        );
        // Portugal's three mainland radars and Spain's nine mainland ones
        // (reference-radars.json) fall inside it.
        for (lat, lon) in [
            (40.845, -8.2797),
            (39.0714, -8.4001),
            (37.3041, -7.953),
            (36.61343, -4.65933),
            (43.40333, -2.84194),
            (41.40818, 1.88489),
        ] {
            assert!((IBERIA.south..IBERIA.north).contains(&lat));
            assert!((IBERIA.west..IBERIA.east).contains(&lon));
        }
    }

    #[test]
    fn only_nordic_offers_products_and_only_from_radars_reaching_it() {
        let sites = crate::providers::table().sites;
        let station = |id: &str| sites.iter().find(|s| s.id == id).unwrap().clone();
        let (nordic, iberia) = (station("nordic"), station("iberia"));
        assert_eq!(crate::mosaic::grid_box(&nordic), Some(NORDIC));
        assert_eq!(crate::mosaic::grid_box(&iberia), None);
        assert!(!crate::products::for_station(&nordic).0.is_empty());
        // iberia: the composite only (no product menu, no radars to cut).
        assert!(crate::products::for_station(&iberia).0.is_empty());
        // nordic's products still come from every radar of the Nordic table...
        let polar = sites.iter().filter(|s| s.kind == SiteKind::Polar).count();
        assert_eq!(crate::mosaic::grid_radars(&nordic, &sites).len(), polar);
        // ...and never from an Iberian radar.
        let mut madrid = station("vara");
        (madrid.id, madrid.lat, madrid.lon, madrid.range_km) =
            ("estjv".into(), 40.17592, -3.71368, 240.0);
        let with_spain = [sites.clone(), vec![madrid]].concat();
        let radars = crate::mosaic::grid_radars(&nordic, &with_spain);
        assert_eq!(radars.len(), polar);
        assert!(radars.iter().all(|s| s.id != "estjv"));
    }

    /// A composite served from memory in ranges.
    struct Served(Vec<u8>);
    impl crate::smhi_live::RangeSource for Served {
        fn get(&mut self, offset: u64, len: u64) -> std::io::Result<(Vec<u8>, Option<u64>)> {
            let start = (offset as usize).min(self.0.len());
            let end = (start + len as usize).min(self.0.len());
            Ok((self.0[start..end].to_vec(), Some(self.0.len() as u64)))
        }
    }

    fn iberian_fixture() -> Vec<u8> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../data/raw/opera_iberia_202609151700.h5"
        );
        std::fs::read(path).expect("run bash scripts/extract-fixtures.sh first")
    }

    #[test]
    fn a_fault_in_another_boxs_chunks_never_costs_the_readers_frame() {
        // Review SF1: the Iberian fixture with chunk (5, 1), which only
        // iberia needs, spoiled in the middle of its deflate stream.
        let golden: serde_json::Value =
            serde_json::from_str(include_str!("../../../golden/iberia-20260915/grid.json"))
                .unwrap();
        let chunk = golden["allocatedChunks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["row"] == 5 && c["col"] == 1)
            .unwrap();
        let (at, size) = (
            chunk["address"].as_u64().unwrap() as usize,
            chunk["size"].as_u64().unwrap() as usize,
        );
        let good = iberian_fixture();
        let mut bad = good.clone();
        for b in &mut bad[at + size / 3..at + size / 3 + 256] {
            *b ^= 0x5a;
        }
        let opener = |bytes: &Vec<u8>| {
            let bytes = bytes.clone();
            move || RangeReader::open_planned(Box::new(Served(bytes.clone())), PLAN, Duration::ZERO)
        };
        // Sound: one read, both grids, no retry.
        let (grids, _, fault) = cut_boxes(opener(&good), &[NORDIC, IBERIA], || false).unwrap();
        assert_eq!((grids.len(), fault), (2, None));
        // Spoiled: iberia's own read fails...
        assert!(cut_boxes(opener(&bad), &[IBERIA], || false).is_err());
        // ...nordic's read cut for iberia too fails on iberia's chunk, so
        // nordic's box is read again alone and nordic keeps its frame.
        let (grids, _, fault) = cut_boxes(opener(&bad), &[NORDIC, IBERIA], || false).unwrap();
        assert_eq!(grids.len(), 1);
        assert_eq!((grids[0].width, grids[0].height), (1670, 2297));
        assert!(fault.is_some());
        // A network failure is not read again: the poller backs off.
        assert!(cut_boxes(opener(&bad), &[NORDIC, IBERIA], || true).is_err());
    }

    fn a_grid(start_ms: i64) -> Grid {
        Grid {
            width: 1,
            height: 1,
            codes: vec![0],
            start_ms,
            end_ms: start_ms,
            elevation_deg: 0.0,
            west: 0.0,
            east: 0.0,
            north: 0.0,
            south: 0.0,
            source_projdef: String::new(),
        }
    }

    fn ids(boxes: Vec<&Cut>) -> Vec<&str> {
        boxes.iter().map(|c| c.id).collect()
    }

    #[test]
    fn a_read_is_cut_for_every_other_box_being_polled_that_lacks_it() {
        let hub = Arc::new(Hub::default());
        let v = volume(BASE, at(15, 17, 0));
        // Alone, a read is cut for its own box only.
        assert_eq!(ids(hub.boxes_for("nordic", &v)), ["nordic"]);
        let nordic = hub.watch("nordic", &[]);
        let iberia = hub.watch("iberia", &[]);
        assert_eq!(ids(hub.boxes_for("nordic", &v)), ["nordic", "iberia"]);
        assert_eq!(ids(hub.boxes_for("iberia", &v)), ["iberia", "nordic"]);
        // A box that has the time is not cut again.
        hub.got("iberia", at(15, 17, 0));
        assert_eq!(ids(hub.boxes_for("nordic", &v)), ["nordic"]);
        // Nor one whose cut is waiting.
        let v2 = volume(BASE, at(15, 17, 5));
        hub.keep(&v2.key, "iberia", a_grid(at(15, 17, 5)), "p".into());
        assert_eq!(ids(hub.boxes_for("nordic", &v2)), ["nordic"]);
        assert!(hub.take(&v2.key, "nordic").is_none());
        let (grid, provenance) = hub.take(&v2.key, "iberia").unwrap();
        assert_eq!((grid.start_ms, provenance.as_str()), (at(15, 17, 5), "p"));
        assert!(hub.take(&v2.key, "iberia").is_none());
        // A box no poller shows any more loses its waiting cuts and is no
        // longer cut for; a second poller of a box keeps it in.
        let again = hub.watch("iberia", &[]);
        hub.keep(&v2.key, "iberia", a_grid(at(15, 17, 5)), "p".into());
        drop(iberia);
        assert!(hub.take(&v2.key, "iberia").is_some());
        hub.keep(&v2.key, "iberia", a_grid(at(15, 17, 5)), "p".into());
        drop(again);
        assert!(hub.take(&v2.key, "iberia").is_none());
        assert_eq!(
            ids(hub.boxes_for("nordic", &volume(BASE, at(15, 17, 10)))),
            ["nordic"]
        );
        // A station that is not a box is cut for nothing.
        assert!(hub.boxes_for("vara", &v).is_empty());
        drop(nordic);
        assert!(lock(&hub.watching).is_empty());
    }

    #[test]
    fn cuts_are_made_while_they_fit_and_never_dropped_for_room() {
        // Review SF2: iberia's live reads cut for nordic (3.8 MB each) only
        // while the waiting cuts fit; none made is ever dropped.
        let hub = Arc::new(Hub::default());
        let (_n, _i) = (hub.watch("nordic", &[]), hub.watch("iberia", &[]));
        let (w, h) = composite::texture_size(NORDIC);
        let mut made = 0;
        for k in 0..6 {
            let v = volume(BASE, at(15, 17, 0) + k * CADENCE_MS);
            if hub.boxes_for("iberia", &v).len() == 2 {
                let mut grid = a_grid(v.valid_ms);
                grid.codes = vec![0; w as usize * h as usize];
                hub.keep(&v.key, "nordic", grid, String::new());
                made += 1;
            }
        }
        assert_eq!(made, CUT_ROOM / (w as usize * h as usize));
        assert_eq!(made, 4);
        assert_eq!(lock(&hub.cuts).len(), 4);
        // The first is still there; taking it makes room for the next.
        assert!(
            hub.take(&volume(BASE, at(15, 17, 0)).key, "nordic")
                .is_some()
        );
        let next = volume(BASE, at(15, 18, 0));
        assert_eq!(ids(hub.boxes_for("iberia", &next)), ["iberia", "nordic"]);
        // An Iberian cut is small: it still fits beside three Nordic ones.
        assert_eq!(ids(hub.boxes_for("nordic", &next)), ["nordic", "iberia"]);
    }

    #[test]
    fn a_backfill_read_is_cut_only_for_times_another_backfill_wants() {
        let hub = Arc::new(Hub::default());
        let (_n, _i) = (hub.watch("nordic", &[]), hub.watch("iberia", &[]));
        let (v1, v2) = (volume(BASE, at(15, 17, 0)), volume(BASE, at(15, 17, 5)));
        // nordic has no backfill running: iberia's backfill cuts nothing
        // for it, though its live reads still would.
        assert_eq!(ids(hub.boxes_for_backfill("iberia", &v1)), ["iberia"]);
        assert_eq!(ids(hub.boxes_for("iberia", &v1)), ["iberia", "nordic"]);
        // nordic's backfill wants v1 only.
        let wanting = hub.want("nordic", &[v1.valid_ms]);
        assert_eq!(
            ids(hub.boxes_for_backfill("iberia", &v1)),
            ["iberia", "nordic"]
        );
        assert_eq!(ids(hub.boxes_for_backfill("iberia", &v2)), ["iberia"]);
        // Once nordic has it, it wants it no more.
        hub.got("nordic", v1.valid_ms);
        assert_eq!(ids(hub.boxes_for_backfill("iberia", &v1)), ["iberia"]);
        // Its backfill over, nothing is wanted.
        let again = hub.want("nordic", &[v2.valid_ms]);
        assert_eq!(
            ids(hub.boxes_for_backfill("iberia", &v2)),
            ["iberia", "nordic"]
        );
        drop((wanting, again));
        assert_eq!(ids(hub.boxes_for_backfill("iberia", &v2)), ["iberia"]);
    }

    #[test]
    fn another_boxes_fresh_listing_serves() {
        let hub = Hub::default();
        let listed = [volume(BASE, at(15, 16, 55)), volume(BASE, at(15, 17, 0))];
        hub.remember_listing("nordic", at(15, 16, 0), &listed);
        // Never a poller's own listing: it asks again.
        assert!(
            hub.listed_by_another("nordic", at(15, 16, 0), POLL)
                .is_none()
        );
        // From another box: the part from its own start on.
        let got = hub
            .listed_by_another("iberia", at(15, 17, 0), POLL)
            .unwrap();
        assert_eq!(got, [volume(BASE, at(15, 17, 0))]);
        // Not one that starts later than the asker needs...
        assert!(
            hub.listed_by_another("iberia", at(15, 15, 0), POLL)
                .is_none()
        );
        // ...nor an old one.
        assert!(
            hub.listed_by_another("iberia", at(15, 17, 0), Duration::ZERO)
                .is_none()
        );
    }

    /// A stand-in for the cache holding `files` (key, bytes): listings with
    /// `start-after`, ranged reads. The log names each request.
    async fn bucket(files: Vec<(String, Vec<u8>)>) -> (String, Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = served.clone();
        let files = Arc::new(files);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (files, log) = (files.clone(), log.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&head).into_owned();
                    let path = text
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let range = text.lines().find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("range: bytes=")
                            .map(str::to_owned)
                    });
                    let (status, extra, body) = if let Some(query) = path.strip_prefix("/?") {
                        let q: HashMap<&str, String> = query
                            .split('&')
                            .filter_map(|kv| kv.split_once('='))
                            .map(|(k, v)| (k, v.replace("%40", "@")))
                            .collect();
                        log.lock()
                            .unwrap()
                            .push(format!("list after={}", q["start-after"]));
                        let keys: String = files
                            .iter()
                            .filter(|(k, _)| k.starts_with(&q["prefix"]) && *k > q["start-after"])
                            .map(|(k, _)| format!("<Contents><Key>{k}</Key></Contents>"))
                            .collect();
                        let xml = format!(
                            "<ListBucketResult><IsTruncated>false</IsTruncated>{keys}</ListBucketResult>"
                        );
                        (200, String::new(), xml.into_bytes())
                    } else {
                        let key = path.trim_start_matches('/');
                        match (files.iter().find(|(k, _)| k == key), range) {
                            (Some((_, bytes)), Some(range)) => {
                                log.lock().unwrap().push(format!("get {key} {range}"));
                                let (a, b) = range.split_once('-').unwrap();
                                let a: usize = a.parse().unwrap();
                                let b = b.parse::<usize>().unwrap().min(bytes.len() - 1);
                                let header =
                                    format!("Content-Range: bytes {a}-{b}/{}\r\n", bytes.len());
                                (206, header, bytes[a..=b].to_vec())
                            }
                            _ => (404, String::new(), Vec::new()),
                        }
                    };
                    let out = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(out.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        (base, served)
    }

    #[test]
    fn both_boxes_shown_read_each_file_once() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../data/raw/opera_iberia_202609151700.h5"
        );
        let bytes = std::fs::read(fixture).expect("run bash scripts/extract-fixtures.sh first");
        let key = "2026/09/15/OPERA/COMP/OPERA@20260915T1700@0@DBZH.h5".to_owned();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (mut sweeps, served) = runtime.block_on(async {
            let (base, served) = bucket(vec![(key.clone(), bytes)]).await;
            let cfg = Config {
                base,
                poll: POLL,
                max_back_off: MAX_BACK_OFF,
                backfill_delay: Duration::from_millis(50),
                backfill_pace: Duration::ZERO,
                now_ms: || at(15, 17, 9),
                hub: Arc::new(Hub::default()),
            };
            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            let pollers = [
                tokio::spawn(poll_with(
                    cfg.clone(),
                    "nordic".into(),
                    tx.clone(),
                    Vec::new(),
                )),
                tokio::spawn(poll_with(cfg.clone(), "iberia".into(), tx, Vec::new())),
            ];
            let mut sweeps = Vec::new();
            let mut ended = 0;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            while (sweeps.len() < 2 || ended < 2) && tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                    Ok(Some(Event::Sweep {
                        site,
                        sweep: Scan::Grid(grid),
                        provenance,
                        ..
                    })) => sweeps.push((site, grid.width, grid.height, provenance)),
                    Ok(Some(Event::HistoryEnd { .. })) => ended += 1,
                    Ok(Some(_)) => {}
                    _ => break,
                }
            }
            for p in pollers {
                p.abort();
            }
            (sweeps, served.lock().unwrap().clone())
        });
        sweeps.sort();
        let sizes: Vec<(&str, u32, u32)> = sweeps
            .iter()
            .map(|(s, w, h, _)| (s.as_str(), *w, *h))
            .collect();
        assert_eq!(
            sizes,
            [("iberia", 835, 690), ("nordic", 1670, 2297)],
            "{served:?}"
        );
        // The file was opened once (one metadata prefetch) and its ranges
        // read once, for both boxes; one of them was cut from the other's
        // read.
        let prefetches = served.iter().filter(|e| e.ends_with(" 0-16383")).count();
        assert_eq!(prefetches, 1, "{served:?}");
        let gets: Vec<&String> = served.iter().filter(|e| e.starts_with("get ")).collect();
        let mut unique = gets.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), gets.len(), "a range read twice: {served:?}");
        assert!(
            sweeps
                .iter()
                .any(|(_, _, _, p)| p.contains("cut from the read made for")),
            "{sweeps:?}"
        );
        // Listings: the live one of one box (taking turns, the other box
        // reads its answer) and each backfill's.
        let listings = served.iter().filter(|e| e.starts_with("list ")).count();
        assert_eq!(listings, 3, "{served:?}");
        eprintln!(
            "both boxes: {} requests ({} range requests, {listings} listings): {served:?}",
            served.len(),
            gets.len()
        );
    }

    #[test]
    fn the_nordic_box_is_the_goldens() {
        let golden: serde_json::Value =
            serde_json::from_str(include_str!("../../../golden/nordic-20260914/grid.json"))
                .unwrap();
        let b = &golden["box"];
        assert_eq!(
            NORDIC,
            LonLatBox {
                west: b["west"].as_f64().unwrap(),
                east: b["east"].as_f64().unwrap(),
                south: b["south"].as_f64().unwrap(),
                north: b["north"].as_f64().unwrap(),
            }
        );
    }
}
