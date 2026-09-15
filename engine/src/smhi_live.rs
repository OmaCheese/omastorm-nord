//! SMHI live polar volumes: the poller that replaces upstream's NOAA chunk
//! follower (`live.rs` and `live_index.rs`; plan Phase 2, stream S3).
//!
//! SMHI publishes one whole ODIM volume per site every 5 minutes, about
//! 4–5 minutes after its valid time. There is no sweep to assemble and
//! nothing partial to paint, so every frame this module reports is complete.
//!
//! - `area/{site}/product/qcvol.json` is polled every `POLL`, and its
//!   `lastFiles` names the newest volume. The request carries
//!   `If-Modified-Since` from the previous answer, and a `304` costs nothing.
//!   SMHI restamps the listing each time it regenerates it, though, so a
//!   `200` is normal, and the volume key decides whether anything is new.
//! - A new volume is read with HTTP range requests (DEC-2): a 64 KiB
//!   prefetch, then 4 KiB blocks. That comes to about 7 requests and 100 KB
//!   of a 15 MB file, handed to the ODIM decoder as `Read + Seek`. Reads
//!   always go to the dated URL: `lastFiles` links `latest.h5`, which moves
//!   under a reader every 5 minutes.
//! - Backfill reads the day listing, plus yesterday's while today's is
//!   short (just after midnight UTC). It fetches whichever of the newest
//!   `BACKFILL` volumes the catalog lacks, newest first.
//! - 429 and 5xx answers back off, honouring `Retry-After`, and so do
//!   transport errors. The second failure in a row reports `offline`. Only
//!   one volume download runs at a time in the whole engine (`FETCHER`).
//! - A station whose newest volume is older than `SILENT_AFTER_MS` is
//!   reported `Silent`, which the UI shows as `unavailable`. Leksand has
//!   published nothing since January 2026.
//! - The national composite (area `sweden`, product `comp`, S8) goes through
//!   the same poller: `product` picks the product from the area, and
//!   `composite.rs` decodes its files into a `Scan::Grid`.

use crate::composite::Grid;
use crate::products::Want;
use crate::sweep::Sweep;
use chrono::{DateTime, NaiveDateTime};
use reqwest::header::{
    CONTENT_RANGE, HeaderMap, IF_MODIFIED_SINCE, LAST_MODIFIED, RANGE, RETRY_AFTER,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::runtime::Handle;
use tokio::sync::{Semaphore, mpsc::Sender, oneshot};
use tokio::task::spawn_blocking;
use tokio::time::sleep;

/// SMHI's open radar API root. Data licence CC BY 4.0: credit "SMHI".
pub const API: &str = "https://opendata-download-radar.smhi.se/api/version/latest";
/// The quality-controlled polar volume (plan section 2).
pub const PRODUCT: &str = "qcvol";
const USER_AGENT: &str = concat!(
    "omastorm-se/",
    env!("CARGO_PKG_VERSION"),
    " (fork of https://omastorm.com; SMHI open data)"
);
/// Between listing polls. SMHI's cadence is 5 minutes.
const POLL: Duration = Duration::from_secs(60);
/// The ceiling of the doubling back-off after failures.
const MAX_BACK_OFF: Duration = Duration::from_secs(600);
/// The most of a `Retry-After` honoured, so a bad header cannot park the
/// poller for a day.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(3600);
/// Each HTTP request, listing or range, as a whole.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// One failure is retried quietly; this many in a row report `offline`.
const OFFLINE_AFTER: u32 = 2;
/// Frames the loop holds after a join, counting the live one (DEC-2 passed:
/// a volume costs ~100 KB, so 60 of them is ~6 MB). Matches `catalog::RING`.
pub const BACKFILL: usize = 60;
/// Frames any other product backfills after a switch (S26): an hour. A
/// product volume costs ~39 range requests and ~740 KB (all ten tilts for
/// CAPPI and CMAX, S20), so 24 frames were ~950 requests and 18 MB a
/// switch; its ring still fills to 60 live.
pub const PRODUCT_BACKFILL: usize = 12;
/// The backfill starts this long after a join, so a hand-off passed while
/// panning costs nothing.
const BACKFILL_DELAY: Duration = Duration::from_secs(3);
/// Between backfill volumes, to be gentle with SMHI.
const BACKFILL_PACE: Duration = Duration::from_millis(250);
/// A station whose newest volume is this old has gone quiet: `Silent`. The
/// SMHI provider's `unavailable` threshold (30 minutes, DEC-9).
const SILENT_AFTER_MS: i64 = crate::providers::smhi::SPEC
    .staleness
    .unavailable
    .as_millis() as i64;
/// A newest volume older than this is neither fetched nor backfilled: the
/// day listings cannot reach it, and a months-old picture is not "live".
const HORIZON_MS: i64 = 24 * 60 * 60 * 1000;
/// A catalogued sweep (by its first ray's time) belongs to the volume whose
/// valid time is at most this far after it...
const MATCH_BEFORE_MS: i64 = 60 * 1000;
/// ...or less than this far before it. The lowest tilt starts a few seconds
/// after the valid time, and volumes are 5 minutes apart.
const MATCH_AFTER_MS: i64 = 4 * 60 * 1000;
/// The first range request: the superblock, the root group and
/// `/dataset1`'s metadata all sit in the first ~55 KB (DEC-2).
pub const PREFETCH: u64 = 64 * 1024;
/// Every later range request is a run of these.
pub const BLOCK: u64 = 4 * 1024;
/// Budget per volume. DEC-2 measured 7 requests and 98,304 B, so these
/// only stop a decoder that wanders through the whole 15 MB file.
const MAX_REQUESTS: u32 = 64;
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// A range reader's plan for one file: the first request's length, the
/// block every later request is a run of, and the most requests and bytes
/// the file may cost. Each provider names its own (`providers::Spec`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RangePlan {
    pub prefetch: u64,
    pub block: u64,
    pub max_requests: u32,
    pub max_bytes: u64,
}

/// DEC-2's plan for SMHI's volumes and composites.
pub const DEC2: RangePlan = RangePlan {
    prefetch: PREFETCH,
    block: BLOCK,
    max_requests: MAX_REQUESTS,
    max_bytes: MAX_BYTES,
};
/// The plan for a product other than the lowest scan (S20), which reads
/// DBZH from several tilts spread through the 15 MB volume (all ten for a
/// pseudo-CAPPI or the column maximum). Measured offline on the whole Vara
/// volume of 2026-09-13 (`products::tests::range_cost_per_product`): 4 KiB
/// blocks take 99 requests and 672 KB, 16 KiB blocks 40 requests and
/// 786 KB, 64 KiB blocks 18 requests and 1.25 MB; one other angle takes 25
/// requests and 475 KB at 16 KiB. DEC-2's budget of 64 requests stops the
/// first at the second tilt.
pub const PRODUCT_PLAN: RangePlan = RangePlan {
    prefetch: PREFETCH,
    block: 16 * 1024,
    max_requests: 96,
    max_bytes: MAX_BYTES,
};
/// Decode failures of one volume before the poller stops retrying it.
const GIVE_UP_AFTER: u32 = 2;
/// Volumes in a row that do not decode before a backfill stops (S20): a
/// failure that repeats is the reader's or the product's, not the volume's,
/// and every further try would cost a whole read.
const BACKFILL_GIVE_UP: u32 = 2;
/// A prefetch answered `bytes 0-65535/*` (SMHI's cache still fetching the
/// volume) is asked again after this pause, then twice and four times it...
const LENGTH_PAUSE: Duration = Duration::from_secs(1);
/// ...this many times, before the volume is given up on.
const LENGTH_RETRIES: u32 = 3;

/// One volume download at a time in the whole engine. The permit travels
/// into the blocking decode, so an aborted poller's in-flight read still
/// holds it until it ends.
pub(crate) static FETCHER: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// `2026-09-13 16:50Z`, as SMHI's `valid` reads.
fn stamp(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%d %H:%MZ").to_string())
        .unwrap_or_default()
}

fn live_log(site: &str, message: impl fmt::Display) {
    let now = DateTime::from_timestamp_millis(now_ms())
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    eprintln!("{now} Live {site}: {message}");
}

/// What a decoder makes of one file: a radar's lowest sweep, or the
/// national composite reprojected to its grid texture (`composite.rs`).
pub enum Scan {
    Polar(Sweep),
    Grid(Grid),
    /// A radar's product other than its lowest scan (S20, `products.rs`):
    /// one other angle, or one drawn on the lowest scan's rays and gates.
    /// The lowest scan rides along when the read decoded it anyway (S26:
    /// CAPPI, CMAX, HYBRID), until the poller sends it on as a frame of its
    /// own (`Event::free_lowest`).
    Product(Sweep, Want, Option<Box<Sweep>>),
    /// My mosaic's frame (S25, `mosaic.rs`): a grid made by the engine from
    /// the lowest scans of the chosen radars, with what names and credits it.
    Mosaic(Box<crate::mosaic::Built>),
}

impl Scan {
    /// The frame's start: the first radial, or the composite's nominal time.
    pub fn start_ms(&self) -> i64 {
        match self {
            Scan::Polar(sweep) | Scan::Product(sweep, ..) => sweep.start_ms,
            Scan::Grid(grid) => grid.start_ms,
            Scan::Mosaic(built) => built.grid.start_ms,
        }
    }
    pub fn end_ms(&self) -> i64 {
        match self {
            Scan::Polar(sweep) | Scan::Product(sweep, ..) => sweep.end_ms,
            Scan::Grid(grid) => grid.end_ms,
            Scan::Mosaic(built) => built.grid.end_ms,
        }
    }
    /// A short description for the log.
    pub fn describe(&self) -> String {
        match self {
            Scan::Polar(sweep) => format!("{} rays", sweep.rays.len()),
            Scan::Product(sweep, want, _) => {
                format!("{} rays of {}", sweep.rays.len(), want.variant())
            }
            Scan::Grid(grid) => grid.describe(),
            Scan::Mosaic(built) => format!("mosaic {}", built.grid.describe()),
        }
    }
}

/// SMHI's product for an area: the national composite for `sweden`, the
/// quality-controlled polar volume for each radar.
pub fn product(site: &str) -> &'static str {
    if site == crate::composite::AREA {
        crate::composite::PRODUCT
    } else {
        PRODUCT
    }
}

/// What the poller reports to `main.rs`.
pub enum Event {
    /// An earlier volume, fetched after a join. The timeline gains history
    /// and the frame on screen stays.
    Backfill {
        site: String,
        sweep: Scan,
        provenance: String,
    },
    /// The newest volume. `complete` is always true, because SMHI publishes
    /// whole volumes. The field stays so the frame path in `main.rs` still
    /// serves both sources.
    Sweep {
        site: String,
        sweep: Scan,
        complete: bool,
        provenance: String,
    },
    /// The newest volume is already in the catalog. The feed is up, so a
    /// station opened on a cached frame can leave `loading` without paying
    /// for the volume again.
    Current { site: String },
    /// SMHI could not be reached or read. The frame on screen stays.
    Offline { site: String, reason: String },
    /// SMHI answered, and the station has published nothing recent.
    Silent { site: String, reason: String },
    /// S31: how far a made frame's fill has come (`mosaic::run`, for the
    /// ring `variant`), or `None` once it is over (`loading.rs`).
    Progress {
        site: String,
        variant: String,
        progress: Option<crate::loading::Progress>,
    },
    /// S31: the backfill's plan for `want`: `frames` earlier frames, those
    /// the tilt store gave (already sent) and those it fetches next.
    HistoryPlan {
        site: String,
        want: Want,
        frames: usize,
    },
    /// S31: the backfill for `want` ended, whether or not every planned
    /// frame came.
    HistoryEnd { site: String, want: Want },
}

impl Event {
    /// This event, and, for a product read that decoded the lowest scan on
    /// the way (S26: CAPPI, CMAX and HYBRID are drawn on its rays and gates,
    /// `products::decode_volume`), that lowest scan as a second event of the
    /// same kind. `main.rs` catalogues it in the station's `e0` ring, so the
    /// ring keeps filling while a product is selected, at no extra request.
    /// Its provenance counts 0 requests and 0 bytes (`soak-report.sh` sums
    /// them), and names the read it came from.
    pub fn free_lowest(mut self) -> (Event, Option<Event>) {
        let provenance_of = |provenance: &str| {
            let (head, tail) = provenance.split_once(": ").unwrap_or((provenance, ""));
            let total = tail
                .rsplit_once(" of ")
                .map_or("0 bytes", |(_, total)| total);
            format!("{head}, its lowest scan: 0 range requests, 0 of {total}")
        };
        let free = match &mut self {
            Event::Sweep {
                site,
                sweep: Scan::Product(_, _, lowest),
                complete,
                provenance,
            } => lowest.take().map(|lowest| Event::Sweep {
                site: site.clone(),
                sweep: Scan::Polar(*lowest),
                complete: *complete,
                provenance: provenance_of(provenance),
            }),
            Event::Backfill {
                site,
                sweep: Scan::Product(_, _, lowest),
                provenance,
            } => lowest.take().map(|lowest| Event::Backfill {
                site: site.clone(),
                sweep: Scan::Polar(*lowest),
                provenance: provenance_of(provenance),
            }),
            _ => None,
        };
        (self, free)
    }
}

// ---------------------------------------------------------------------------
// Listings
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Listing {
    #[serde(rename = "lastFiles", default)]
    last_files: Option<Vec<Listed>>,
}

/// `…/qcvol/YYYY/MM/DD.json`. A day with no volumes (Leksand) is
/// `"files": []` with `null` beside it.
#[derive(Deserialize)]
struct DayListing {
    #[serde(default)]
    files: Option<Vec<Listed>>,
}

#[derive(Deserialize)]
struct Listed {
    key: String,
    valid: String,
    #[serde(default)]
    formats: Option<Vec<Format>>,
}

#[derive(Deserialize)]
struct Format {
    key: String,
}

/// One published volume.
#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    /// `radar_vara_qcvol_202609131650`.
    pub key: String,
    /// The nominal start (`valid`), milliseconds since the epoch.
    pub valid_ms: i64,
    /// The dated, immutable download URL.
    pub url: String,
}

fn valid_ms(valid: &str) -> Option<i64> {
    NaiveDateTime::parse_from_str(valid.trim(), "%Y-%m-%d %H:%M")
        .ok()
        .map(|t| t.and_utc().timestamp_millis())
}

/// `YYYY/MM/DD` of a UTC instant, the way SMHI's paths name days.
fn day_path(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y/%m/%d").to_string())
        .unwrap_or_default()
}

pub fn listing_url(base: &str, site: &str) -> String {
    format!("{base}/area/{site}/product/{}.json", product(site))
}

pub fn day_listing_url(base: &str, site: &str, day_ms: i64) -> String {
    format!(
        "{base}/area/{site}/product/{}/{}.json",
        product(site),
        day_path(day_ms)
    )
}

/// The dated URL of a volume. Built rather than read from the listing:
/// `lastFiles` links the moving `latest.h5`, and building it keeps every
/// request under `base`. The day listing test checks that the built URL
/// matches every link SMHI lists.
pub fn volume_url(base: &str, site: &str, key: &str, valid_ms: i64) -> String {
    format!(
        "{base}/area/{site}/product/{}/{}/{key}.h5",
        product(site),
        day_path(valid_ms)
    )
}

fn volumes(files: Vec<Listed>, base: &str, site: &str) -> Vec<Volume> {
    files
        .into_iter()
        .filter(|f| {
            f.formats
                .as_ref()
                .is_none_or(|formats| formats.iter().any(|x| x.key == "h5"))
        })
        .filter_map(|f| {
            let valid_ms = valid_ms(&f.valid)?;
            Some(Volume {
                url: volume_url(base, site, &f.key, valid_ms),
                key: f.key,
                valid_ms,
            })
        })
        .collect()
}

/// The newest volume `qcvol.json` lists, if any.
pub fn newest_volume(body: &[u8], base: &str, site: &str) -> Result<Option<Volume>, String> {
    let listing: Listing =
        serde_json::from_slice(body).map_err(|e| format!("reading the listing: {e}"))?;
    Ok(volumes(listing.last_files.unwrap_or_default(), base, site)
        .into_iter()
        .max_by_key(|v| v.valid_ms))
}

/// Every volume a day listing names.
pub fn day_volumes(body: &[u8], base: &str, site: &str) -> Result<Vec<Volume>, String> {
    let listing: DayListing =
        serde_json::from_slice(body).map_err(|e| format!("reading the day listing: {e}"))?;
    Ok(volumes(listing.files.unwrap_or_default(), base, site))
}

/// Whether a catalogued sweep start (`known`) is the volume valid at
/// `valid_ms`.
pub fn covered(valid_ms: i64, known: &[i64]) -> bool {
    known
        .iter()
        .any(|&start| start >= valid_ms - MATCH_BEFORE_MS && start < valid_ms + MATCH_AFTER_MS)
}

/// Today's listing alone cannot fill the loop just after midnight UTC.
pub fn needs_yesterday(today: usize, n: usize) -> bool {
    today < n
}

/// What a backfill fetches: of the newest `n` volumes up to and including
/// the live one (valid at `live_ms`), those the catalog lacks, newest
/// first. The live volume counts toward `n` but is left to the live path.
pub fn backfill_targets(
    mut listed: Vec<Volume>,
    live_ms: i64,
    known: &[i64],
    n: usize,
) -> Vec<Volume> {
    listed.retain(|v| v.valid_ms <= live_ms);
    listed.sort_by(|a, b| b.valid_ms.cmp(&a.valid_ms).then_with(|| a.key.cmp(&b.key)));
    listed.dedup_by(|a, b| a.key == b.key);
    listed.truncate(n);
    listed.retain(|v| v.valid_ms != live_ms && !covered(v.valid_ms, known));
    listed
}

/// How long to wait after `failures` failures in a row: the poll interval
/// doubled per failure up to `max`, but never less than the server's
/// `Retry-After`.
pub fn back_off(
    poll: Duration,
    max: Duration,
    failures: u32,
    retry_after: Option<Duration>,
) -> Duration {
    let doubled = poll.saturating_mul(1u32 << failures.min(16));
    doubled
        .min(max)
        .max(retry_after.unwrap_or_default().min(RETRY_AFTER_CAP))
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// Why a request failed.
#[derive(Debug, Clone)]
pub enum Fail {
    /// An unexpected status, with the `Retry-After` it carried.
    Status(u16, Option<Duration>),
    Transport(String),
    /// The answer arrived but was not what was asked for.
    Answer(String),
}

impl Fail {
    pub(crate) fn retry_after(&self) -> Option<Duration> {
        match self {
            Fail::Status(_, after) => *after,
            _ => None,
        }
    }
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fail::Status(code, Some(after)) => {
                write!(f, "HTTP {code} (retry after {}s)", after.as_secs())
            }
            Fail::Status(code, None) => write!(f, "HTTP {code}"),
            Fail::Transport(e) => write!(f, "{e}"),
            Fail::Answer(e) => write!(f, "{e}"),
        }
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

pub(crate) enum Fetched {
    NotModified,
    Body(Vec<u8>, Option<String>),
}

/// The HTTP client every poller shares the shape of (`providers/opera.rs`
/// uses it too).
#[derive(Clone)]
pub(crate) struct Http {
    client: reqwest::Client,
}

impl Http {
    pub(crate) fn new() -> Result<Self, String> {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(CALL_TIMEOUT)
            .build()
            .map(|client| Http { client })
            .map_err(|e| format!("building the HTTP client: {e}"))
    }

    /// A listing, conditional on `since` (a previous `Last-Modified`).
    pub(crate) async fn get(&self, url: &str, since: Option<&str>) -> Result<Fetched, Fail> {
        let mut request = self.client.get(url);
        if let Some(since) = since {
            request = request.header(IF_MODIFIED_SINCE, since);
        }
        let response = request.send().await.map_err(|e| {
            crate::netstats::failed(url);
            Fail::Transport(e.to_string())
        })?;
        crate::netstats::answered(url, &response);
        match response.status().as_u16() {
            304 => return Ok(Fetched::NotModified),
            200 => {}
            code => return Err(Fail::Status(code, retry_after(response.headers()))),
        }
        let modified = response
            .headers()
            .get(LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = response
            .bytes()
            .await
            .map_err(|e| Fail::Transport(e.to_string()))?;
        Ok(Fetched::Body(body.to_vec(), modified))
    }

    /// `len` bytes of `url` from `offset` (fewer at the end of the file),
    /// and the file's total length from `Content-Range` when the answer
    /// names one.
    async fn range(
        &self,
        url: &str,
        offset: u64,
        len: u64,
    ) -> Result<(Vec<u8>, Option<u64>), Fail> {
        let response = self
            .client
            .get(url)
            .header(RANGE, format!("bytes={offset}-{}", offset + len - 1))
            .send()
            .await
            .map_err(|e| {
                crate::netstats::failed(url);
                Fail::Transport(e.to_string())
            })?;
        crate::netstats::answered(url, &response);
        let code = response.status().as_u16();
        if code != 206 {
            // A 200 would be the whole 15 MB: drop it unread.
            return Err(Fail::Status(code, retry_after(response.headers())));
        }
        let range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let (start, total) = parse_content_range(&range)
            .ok_or_else(|| Fail::Answer(format!("unreadable Content-Range {range:?}")))?;
        if start != offset {
            return Err(Fail::Answer(format!("asked for {offset}, got {range}")));
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| Fail::Transport(e.to_string()))?;
        let expected = total.map_or(len, |total| len.min(total.saturating_sub(offset)));
        if body.len() as u64 != expected {
            return Err(Fail::Answer(format!(
                "short range: {} of {expected} bytes at {offset}",
                body.len()
            )));
        }
        Ok((body.to_vec(), total))
    }
}

/// `bytes 0-65535/14701179` as (0, Some(14701179)). `bytes 0-65535/*`, a
/// length the server does not know yet, is (0, None): SMHI's cache answers
/// that way while it is still fetching a volume it did not hold.
fn parse_content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (span, total) = rest.split_once('/')?;
    let (start, _) = span.split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        total => Some(total.parse().ok()?),
    };
    Some((start.trim().parse().ok()?, total))
}

/// The signature an HDF5 superblock starts with. SMHI's volumes have no
/// user block, so it sits at offset 0.
const HDF5_SIGNATURE: &[u8; 8] = b"\x89HDF\r\n\x1a\n";

/// The file length an HDF5 superblock records (its end-of-file address),
/// for an answer whose `Content-Range` names none. It equals the file size
/// on SMHI's vara 2026-09-13 10:55 volume (14,701,179 bytes).
fn hdf5_eof(head: &[u8]) -> Option<u64> {
    if !head.starts_with(HDF5_SIGNATURE) {
        return None;
    }
    // Where the size of offsets sits, and where the base address starts;
    // the end-of-file address follows the base and free-space addresses.
    let (offsets, base) = match *head.get(8)? {
        0 => (*head.get(13)?, 24),
        1 => (*head.get(13)?, 28),
        2 | 3 => (*head.get(9)?, 12),
        _ => return None,
    };
    let size = usize::from(offsets);
    if !matches!(size, 2 | 4 | 8) {
        return None;
    }
    let at = base + 2 * size;
    let bytes = head.get(at..at + size)?;
    if bytes.iter().all(|&b| b == 0xff) {
        return None; // the undefined address
    }
    let mut value = [0u8; 8];
    value[..size].copy_from_slice(bytes);
    Some(u64::from_le_bytes(value)).filter(|&eof| eof > 0)
}

// ---------------------------------------------------------------------------
// Range reader
// ---------------------------------------------------------------------------

/// Something that answers one ranged request: the bytes, and the file's
/// total length when the answer names it.
pub trait RangeSource: Send {
    fn get(&mut self, offset: u64, len: u64) -> io::Result<(Vec<u8>, Option<u64>)>;
}

/// What a reader has fetched so far. Shared, so it can be read after the
/// decoder has consumed the reader.
#[derive(Default, Debug)]
pub struct Traffic {
    requests: AtomicU32,
    bytes: AtomicU64,
    len: AtomicU64,
}

impl Traffic {
    pub fn requests(&self) -> u32 {
        self.requests.load(Ordering::Relaxed)
    }
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
    /// The whole file's length.
    pub fn total(&self) -> u64 {
        self.len.load(Ordering::Relaxed)
    }
}

/// `Read + Seek` over ranged requests, behind a block cache (DEC-2). The
/// first request is `PREFETCH` bytes from the start; each later miss fetches
/// the run of adjacent missing `BLOCK`s it needs, in one request. It fails
/// if the file's length changes between requests, or if a volume would
/// cost more than `MAX_REQUESTS` or `MAX_BYTES`.
pub struct RangeReader {
    source: Box<dyn RangeSource>,
    plan: RangePlan,
    len: u64,
    pos: u64,
    blocks: HashMap<u64, Vec<u8>>,
    traffic: Arc<Traffic>,
}

impl RangeReader {
    /// `open_with` and the real pause; the poller passes its config's.
    #[cfg(test)]
    pub fn open(source: Box<dyn RangeSource>) -> io::Result<Self> {
        Self::open_with(source, LENGTH_PAUSE)
    }

    /// Read the prefetch. The file's length comes from `Content-Range`, or,
    /// when SMHI's cache answers `bytes 0-65535/*` (a volume it is still
    /// fetching), from the HDF5 superblock's end-of-file address. Failing
    /// both, ask again after `pause`, twice that, and four times that
    /// (`LENGTH_RETRIES`), then give up on the volume.
    pub fn open_with(source: Box<dyn RangeSource>, pause: Duration) -> io::Result<Self> {
        Self::open_planned(source, DEC2, pause)
    }

    /// `open_with` under another provider's `plan`. The prefetch is rounded
    /// up to a whole number of blocks (S26): the reader keeps whole blocks,
    /// and a prefetch's short last chunk would be read past its end.
    pub fn open_planned(
        mut source: Box<dyn RangeSource>,
        plan: RangePlan,
        pause: Duration,
    ) -> io::Result<Self> {
        if plan.block == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a range plan's block is 0 bytes",
            ));
        }
        let plan = RangePlan {
            prefetch: plan.prefetch.next_multiple_of(plan.block),
            ..plan
        };
        let mut requests = 0u32;
        let mut fetched = 0u64;
        let (head, len) = loop {
            let (head, total) = source.get(0, plan.prefetch)?;
            requests += 1;
            fetched += head.len() as u64;
            match total.or_else(|| hdf5_eof(&head)) {
                Some(len) => break (head, len),
                None if requests > LENGTH_RETRIES => {
                    return Err(io::Error::other(format!(
                        "the volume's length is still unknown after {requests} requests"
                    )));
                }
                None => std::thread::sleep(pause * (1 << (requests - 1))),
            }
        };
        if head.len() as u64 != plan.prefetch.min(len) {
            return Err(io::Error::other(format!(
                "short prefetch: {} of {} bytes",
                head.len(),
                plan.prefetch.min(len)
            )));
        }
        let traffic = Arc::new(Traffic::default());
        traffic.requests.store(requests, Ordering::Relaxed);
        traffic.bytes.store(fetched, Ordering::Relaxed);
        traffic.len.store(len, Ordering::Relaxed);
        let mut reader = RangeReader {
            source,
            plan,
            len,
            pos: 0,
            blocks: HashMap::new(),
            traffic,
        };
        reader.store(0, &head);
        Ok(reader)
    }

    pub fn traffic(&self) -> Arc<Traffic> {
        self.traffic.clone()
    }

    fn store(&mut self, first_block: u64, bytes: &[u8]) {
        for (i, chunk) in bytes.chunks(self.plan.block as usize).enumerate() {
            self.blocks.insert(first_block + i as u64, chunk.to_vec());
        }
    }

    /// Fetch whichever of blocks `first..=last` are missing, one request per
    /// run of adjacent missing blocks.
    fn ensure(&mut self, first: u64, last: u64) -> io::Result<()> {
        let mut block = first;
        while block <= last {
            if self.blocks.contains_key(&block) {
                block += 1;
                continue;
            }
            let run = block;
            while block <= last && !self.blocks.contains_key(&block) {
                block += 1;
            }
            let start = run * self.plan.block;
            let end = (block * self.plan.block).min(self.len);
            let requests = self.traffic.requests();
            let bytes = self.traffic.bytes();
            if requests >= self.plan.max_requests || bytes + (end - start) > self.plan.max_bytes {
                return Err(io::Error::other(format!(
                    "range budget spent: {requests} requests, {bytes} bytes"
                )));
            }
            let (got, total) = self.source.get(start, end - start)?;
            if let Some(total) = total
                && total != self.len
            {
                return Err(io::Error::other(format!(
                    "the volume changed while it was read ({} bytes, now {total})",
                    self.len
                )));
            }
            if got.len() as u64 != end - start {
                return Err(io::Error::other(format!(
                    "short range: {} of {} bytes at {start}",
                    got.len(),
                    end - start
                )));
            }
            self.traffic.requests.fetch_add(1, Ordering::Relaxed);
            self.traffic.bytes.fetch_add(end - start, Ordering::Relaxed);
            self.store(run, &got);
        }
        Ok(())
    }
}

impl Read for RangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let size = self.plan.block;
        let end = (self.pos + buf.len() as u64).min(self.len);
        self.ensure(self.pos / size, (end - 1) / size)?;
        let mut out = 0;
        while self.pos < end {
            let block = self.pos / size;
            let bytes = &self.blocks[&block];
            let at = (self.pos - block * size) as usize;
            let n = (bytes.len() - at).min((end - self.pos) as usize);
            buf[out..out + n].copy_from_slice(&bytes[at..at + n]);
            out += n;
            self.pos += n as u64;
        }
        Ok(out)
    }
}

impl Seek for RangeReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = pos.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the volume",
            )
        })?;
        Ok(self.pos)
    }
}

/// Ranged requests from the blocking decode thread, answered by the async
/// client on the engine's runtime. The last HTTP failure is kept, so the
/// poller can tell a network problem (back off) from a bad volume.
pub(crate) struct HttpRanges {
    pub(crate) http: Http,
    pub(crate) url: String,
    pub(crate) runtime: Handle,
    pub(crate) failure: Arc<Mutex<Option<Fail>>>,
}

impl RangeSource for HttpRanges {
    fn get(&mut self, offset: u64, len: u64) -> io::Result<(Vec<u8>, Option<u64>)> {
        let (tx, rx) = oneshot::channel();
        let http = self.http.clone();
        let url = self.url.clone();
        self.runtime.spawn(async move {
            let _ = tx.send(http.range(&url, offset, len).await);
        });
        match rx.blocking_recv() {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(fail)) => {
                let message = format!("range {offset}+{len}: {fail}");
                *self.failure.lock().unwrap() = Some(fail);
                Err(io::Error::other(message))
            }
            Err(_) => Err(io::Error::other("the runtime dropped a range request")),
        }
    }
}

/// A file's decoder: the lowest-tilt DBZH sweep of a volume (S2, `odim.rs`)
/// or the composite's grid (S8, `composite.rs`); since S20 the product the
/// poller follows (`products::Want`), which a composite ignores; since S27
/// the volume's place in the tilt store (`tilts::Slot`), when there is one.
pub type Decode = fn(RangeReader, Want, Option<&crate::tilts::Slot>) -> Result<Scan, String>;

enum VolumeError {
    /// SMHI failed to answer: back off.
    Net(Fail),
    /// The answer or the bytes were unusable: give up on that volume only.
    Decode(String),
}

/// The volume's place in the tilt store, when the poller has a store.
fn slot(cfg: &Config, site: &str, volume: &Volume) -> Option<crate::tilts::Slot> {
    cfg.store.clone().map(|store| crate::tilts::Slot {
        store,
        station: site.to_owned(),
        time_ms: volume.valid_ms,
        source: volume.key.clone(),
    })
}

/// A frame's provenance when the tilt store made it.
fn store_provenance(product: &str, volume: &Volume, want: Want) -> String {
    format!(
        "SMHI {product} {}{}: {}",
        volume.key,
        crate::products::provenance_tag(want),
        crate::tilts::FROM_STORE
    )
}

/// `want` of a volume from the tilt store alone (S27), when it holds
/// every tilt: no request, no fetch permit.
async fn from_store(slot: Option<crate::tilts::Slot>, want: Want) -> Option<Scan> {
    let slot = slot?;
    spawn_blocking(move || slot.compose(want, crate::odim::Tilt::First))
        .await
        .ok()?
        .ok()?
}

/// Read and decode one volume with ranged requests, holding the engine's
/// single fetch permit. Returns the sweep and its provenance. A volume the
/// tilt store holds is composed from it, with no request (S27).
async fn fetch_volume(
    http: &Http,
    cfg: &Config,
    site: &str,
    volume: &Volume,
) -> Result<(Scan, String), VolumeError> {
    let (product, decode, want, length_pause) =
        (product(site), cfg.decode, cfg.want, cfg.length_pause);
    let slot = slot(cfg, site, volume);
    if let Some(scan) = from_store(slot.clone(), want).await {
        return Ok((scan, store_provenance(product, volume, want)));
    }
    let permit = FETCHER
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| VolumeError::Decode(e.to_string()))?;
    let failure = Arc::new(Mutex::new(None));
    let source = HttpRanges {
        http: http.clone(),
        url: volume.url.clone(),
        runtime: Handle::current(),
        failure: failure.clone(),
    };
    let joined = spawn_blocking(move || {
        let _permit = permit;
        // The lowest scan keeps DEC-2's plan; any other product reads
        // several tilts under `PRODUCT_PLAN` (S20).
        let reader = if want.is_lowest() {
            RangeReader::open_with(Box::new(source), length_pause)
        } else {
            RangeReader::open_planned(Box::new(source), PRODUCT_PLAN, length_pause)
        }
        .map_err(|e| e.to_string())?;
        let traffic = reader.traffic();
        decode(reader, want, slot.as_ref()).map(|sweep| (sweep, traffic))
    })
    .await;
    match joined {
        Ok(Ok((sweep, traffic))) => {
            let provenance = format!(
                "SMHI {product} {}{}: {} range requests, {} of {} bytes",
                volume.key,
                crate::products::provenance_tag(want),
                traffic.requests(),
                traffic.bytes(),
                traffic.total()
            );
            Ok((sweep, provenance))
        }
        Ok(Err(e)) => Err(match failure.lock().unwrap().take() {
            Some(fail @ (Fail::Status(..) | Fail::Transport(_))) => VolumeError::Net(fail),
            // An answer that was not what was asked for (a length that
            // stays unknown, a short range) spoils this volume alone.
            _ => VolumeError::Decode(e),
        }),
        Err(e) => Err(VolumeError::Decode(format!("the decoder failed: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// The poller
// ---------------------------------------------------------------------------

/// What the poller needs from its surroundings. `Config::smhi` is the real
/// one. Tests point `base` at a local server, shorten the waits and fix
/// the clock.
#[derive(Clone)]
pub struct Config {
    pub base: String,
    pub poll: Duration,
    pub max_back_off: Duration,
    pub backfill_delay: Duration,
    pub backfill_pace: Duration,
    /// The first pause before re-asking for a volume's unknown length.
    pub length_pause: Duration,
    pub now_ms: fn() -> i64,
    pub decode: Decode,
    /// The product the poller follows (S20); its backfill depth too.
    pub want: Want,
    /// The tilt store volumes are read through (S27); `None` reads every
    /// volume from SMHI, as before.
    pub store: Option<Arc<crate::tilts::Store>>,
    /// Volumes a backfill reaches back, counting the live one, when not the
    /// product's own depth (S25: a mosaic radar's hour, `poll_lowest`).
    pub depth: Option<usize>,
}

impl Config {
    pub fn smhi(decode: Decode) -> Self {
        Config {
            base: API.to_owned(),
            poll: POLL,
            max_back_off: MAX_BACK_OFF,
            backfill_delay: BACKFILL_DELAY,
            backfill_pace: BACKFILL_PACE,
            length_pause: LENGTH_PAUSE,
            now_ms,
            decode,
            want: Want::Lowest,
            store: None,
            depth: None,
        }
    }
}

/// S2's ODIM decoder over the ranged reader: `/dataset1` for the lowest
/// scan, the angles a product needs for any other (`products.rs`), through
/// the tilt store (`tilts::decode`).
fn decode(
    reader: RangeReader,
    want: Want,
    slot: Option<&crate::tilts::Slot>,
) -> Result<Scan, String> {
    crate::tilts::decode(reader, want, crate::odim::Tilt::First, slot)
}

/// The composite has no angles: every product is the composite, and it
/// has no tilts to store.
fn decode_composite(
    reader: RangeReader,
    _want: Want,
    _slot: Option<&crate::tilts::Slot>,
) -> Result<Scan, String> {
    crate::composite::decode_scan(reader)
}

/// Poll SMHI for `site` until the task is aborted or the event channel
/// closes (`poll_with` has the details). The composite's area gets the
/// composite decoder, every radar the ODIM volume decoder for `want`.
/// `skip_known` is upstream's respawn flag, kept so `main.rs` calls both
/// pollers alike. SMHI volumes are never replayed, and catalogued ones are
/// never fetched, so it changes nothing.
pub async fn poll(
    site: String,
    events: Sender<Event>,
    cached: Vec<i64>,
    _skip_known: bool,
    want: Want,
) {
    let cfg = if site == crate::composite::AREA {
        Config::smhi(decode_composite)
    } else {
        Config {
            want,
            store: crate::tilts::shared(),
            ..Config::smhi(decode)
        }
    };
    poll_with(cfg, site, events, cached).await;
}

/// `want` (the lowest scan, or every scan for a height set, S30) of radar
/// `site`, through the tilt store, backfilling `depth` volumes counting the
/// live one: one of My mosaic's radars (S25, `mosaic.rs`), whose events go
/// to the mosaic, not to `main.rs`.
pub async fn poll_lowest(
    site: String,
    events: Sender<Event>,
    known: Vec<i64>,
    depth: usize,
    want: Want,
) {
    let cfg = Config {
        want,
        store: crate::tilts::shared(),
        depth: Some(depth),
        ..Config::smhi(decode)
    };
    poll_with(cfg, site, events, known).await;
}

/// Aborts its task when dropped, so a poller that is replaced takes its
/// backfill with it.
pub(crate) struct AbortOnDrop(pub(crate) tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Send `event`, then the lowest scan its read made for free, if any.
pub(crate) async fn send(events: &Sender<Event>, event: Event) -> bool {
    let (event, free) = event.free_lowest();
    events.send(event).await.is_ok()
        && match free {
            Some(free) => events.send(free).await.is_ok(),
            None => true,
        }
}

/// SMHI's cadence: one volume per site every 5 minutes.
const CADENCE_MS: i64 = 5 * 60 * 1000;
/// `qcvol.json` should list a volume about 5 minutes after its valid time,
/// but it is served through a cache and has been seen 12 minutes behind
/// the day listing. A volume this far past its valid time is probed
/// directly at its dated URL. SMHI names volumes predictably, and answers
/// 404 until one is published.
const PROBE_AFTER_MS: i64 = 6 * 60 * 1000;

/// The volume SMHI publishes for `site` at `valid_ms`, by its dated URL.
pub fn next_volume(base: &str, site: &str, valid_ms: i64) -> Volume {
    let stamp = DateTime::from_timestamp_millis(valid_ms)
        .map(|t| t.format("%Y%m%d%H%M").to_string())
        .unwrap_or_default();
    let key = format!("radar_{site}_{}_{stamp}", product(site));
    Volume {
        url: volume_url(base, site, &key, valid_ms),
        key,
        valid_ms,
    }
}

/// The live path's memory between polls.
struct Live {
    /// Start times of every frame catalogued or delivered.
    known: Vec<i64>,
    /// The newest volume dealt with: fetched, found in the catalog, too old
    /// to fetch, or given up on.
    newest: Option<Volume>,
    decode_failures: HashMap<String, u32>,
}

enum Outcome {
    /// Dealt with, with an event for `main.rs` when there is one.
    Handled(Option<Event>),
    /// Not published yet (a probe's 404), or a decode failure to retry.
    Pending,
    Failed(Fail),
}

impl Live {
    fn is_newer(&self, volume: &Volume) -> bool {
        self.newest
            .as_ref()
            .is_none_or(|newest| volume.valid_ms > newest.valid_ms)
    }

    /// The next volume, once it is overdue and while the station is not
    /// silent.
    fn next_due(&self, now: i64, base: &str, site: &str) -> Option<Volume> {
        let newest = self.newest.as_ref()?;
        let next_ms = newest.valid_ms + CADENCE_MS;
        let overdue = now >= next_ms + PROBE_AFTER_MS;
        let quiet = now - newest.valid_ms >= SILENT_AFTER_MS;
        (overdue && !quiet).then(|| next_volume(base, site, next_ms))
    }

    /// Deal with `volume`: nothing to fetch if the catalog has it or it is
    /// too old, else read it. `probe` marks a volume SMHI has not listed,
    /// for which a 404 only means "not yet".
    async fn take(
        &mut self,
        cfg: &Config,
        http: &Http,
        site: &str,
        volume: Volume,
        probe: bool,
    ) -> Outcome {
        let first = self.newest.is_none();
        let outcome = if covered(volume.valid_ms, &self.known) {
            Outcome::Handled(first.then(|| Event::Current {
                site: site.to_owned(),
            }))
        } else if (cfg.now_ms)() - volume.valid_ms >= HORIZON_MS {
            Outcome::Handled(None)
        } else {
            match fetch_volume(http, cfg, site, &volume).await {
                Ok((sweep, provenance)) => {
                    self.known.push(sweep.start_ms());
                    Outcome::Handled(Some(Event::Sweep {
                        site: site.to_owned(),
                        sweep,
                        complete: true,
                        provenance,
                    }))
                }
                Err(VolumeError::Net(Fail::Status(404, _))) if probe => Outcome::Pending,
                Err(VolumeError::Net(fail)) => Outcome::Failed(fail),
                Err(VolumeError::Decode(e)) => {
                    let tries = self.decode_failures.entry(volume.key.clone()).or_default();
                    *tries += 1;
                    live_log(site, format_args!("{}: {e} (try {tries})", volume.key));
                    if *tries >= GIVE_UP_AFTER {
                        Outcome::Handled(None)
                    } else {
                        Outcome::Pending
                    }
                }
            }
        };
        if matches!(outcome, Outcome::Handled(_)) {
            self.newest = Some(volume);
        }
        outcome
    }
}

/// Poll `site` until the task is aborted or the event channel closes.
/// `cached` holds the start times of the frames already catalogued for the
/// station, so neither the live path nor the backfill fetches them again.
///
/// Each round reads the listing and deals with its newest volume, then,
/// if the listing lags, probes the next volume by its dated URL (one
/// download per round). Once the live path has caught up with what SMHI
/// has published, the backfill starts.
pub async fn poll_with(cfg: Config, site: String, events: Sender<Event>, cached: Vec<i64>) {
    let http = match Http::new() {
        Ok(http) => http,
        Err(e) => {
            send(&events, Event::Offline { site, reason: e }).await;
            return;
        }
    };
    let url = listing_url(&cfg.base, &site);
    let mut live = Live {
        known: cached,
        newest: None,
        decode_failures: HashMap::new(),
    };
    let mut modified: Option<String> = None;
    // The last listing's newest volume, and whether it listed none at all.
    let mut listed: Option<Volume> = None;
    let mut lists_nothing = false;
    let mut failures = 0u32;
    let mut silent = false;
    let mut backfilling: Option<AbortOnDrop> = None;
    loop {
        let mut wait = cfg.poll;
        let mut failed: Option<Fail> = None;
        let mut listed_at = None;
        match http.get(&url, modified.as_deref()).await {
            Ok(Fetched::NotModified) => {}
            Ok(Fetched::Body(body, stamp_)) => match newest_volume(&body, &cfg.base, &site) {
                Ok(newest) => {
                    lists_nothing = newest.is_none();
                    listed = newest;
                    listed_at = Some(stamp_);
                }
                Err(e) => failed = Some(Fail::Answer(e)),
            },
            Err(fail) => failed = Some(fail),
        }

        // The listing's newest volume, when it is newer than anything dealt
        // with.
        if let Some(volume) = listed.clone().filter(|v| live.is_newer(v)) {
            match live.take(&cfg, &http, &site, volume, false).await {
                Outcome::Handled(Some(event)) => {
                    if !send(&events, event).await {
                        return;
                    }
                }
                Outcome::Handled(None) | Outcome::Pending => {}
                Outcome::Failed(fail) => failed = Some(fail),
            }
        }

        // A lagging listing: probe the next volume once it is overdue.
        // Catalogued volumes cost nothing and are skipped through; one
        // download per round.
        let mut caught_up = false;
        if failed.is_none() {
            let now = (cfg.now_ms)();
            loop {
                let Some(next) = live.next_due(now, &cfg.base, &site) else {
                    caught_up = true;
                    break;
                };
                let free = covered(next.valid_ms, &live.known);
                match live.take(&cfg, &http, &site, next, true).await {
                    Outcome::Handled(Some(event)) => {
                        if !send(&events, event).await {
                            return;
                        }
                    }
                    Outcome::Handled(None) => {}
                    Outcome::Pending => {
                        caught_up = true;
                        break;
                    }
                    Outcome::Failed(fail) => {
                        failed = Some(fail);
                        break;
                    }
                }
                if !free {
                    break;
                }
            }
        }

        // Only a listing whose newest volume is dealt with may be answered
        // 304 next time; otherwise a failed read would never be retried.
        if let Some(stamp_) = listed_at
            && listed.as_ref().is_none_or(|v| !live.is_newer(v))
        {
            modified = stamp_;
        }

        // Silence is judged after any fetch, so a station that went quiet
        // an hour ago shows its last volume, then `unavailable`.
        let latest_ms = listed
            .iter()
            .chain(live.newest.iter())
            .map(|v| v.valid_ms)
            .max();
        let quiet_since = match latest_ms {
            Some(ms) if (cfg.now_ms)() - ms >= SILENT_AFTER_MS => Some(Some(ms)),
            None if lists_nothing => Some(None),
            _ => None,
        };
        if let Some(since) = quiet_since
            && !silent
        {
            let reason = match since {
                Some(ms) => format!("{site} has published nothing since {}", stamp(ms)),
                None => format!("SMHI lists no {} volume for {site}", product(&site)),
            };
            let event = Event::Silent {
                site: site.clone(),
                reason,
            };
            if !send(&events, event).await {
                return;
            }
        }
        silent = quiet_since.is_some();

        if backfilling.is_none()
            && caught_up
            && let Some(newest) = &live.newest
            && (cfg.now_ms)() - newest.valid_ms < HORIZON_MS
        {
            backfilling = Some(AbortOnDrop(tokio::spawn(backfill(
                cfg.clone(),
                http.clone(),
                site.clone(),
                events.clone(),
                newest.valid_ms,
                live.known.clone(),
            ))));
        }

        // A round counts as a failure if the listing or a volume failed, so
        // a volume throttled over and over still backs off.
        if let Some(fail) = failed {
            failures += 1;
            wait = back_off(cfg.poll, cfg.max_back_off, failures, fail.retry_after());
            let reason = format!("SMHI: {fail}");
            if failures < OFFLINE_AFTER {
                live_log(
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

/// The day listing(s) around `live_ms`, then each target volume in turn,
/// newest first, as `Event::Backfill`. A network failure ends the backfill
/// (the live poller backs off on its own); a volume that does not decode is
/// skipped.
async fn backfill(
    cfg: Config,
    http: Http,
    site: String,
    events: Sender<Event>,
    live_ms: i64,
    mut known: Vec<i64>,
) {
    sleep(cfg.backfill_delay).await;
    // The tilt store first (S27): every stored volume of the loop the
    // catalog lacks, newest first, with no request and no pacing.
    let mut stored = 0;
    // Nominal times the store made, whatever their scans' start times.
    let mut done: Vec<i64> = Vec::new();
    if let Some(store) = cfg.store.clone() {
        let station = site.clone();
        let volumes = spawn_blocking(move || store.volumes(&station))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        let loop_ = volumes
            .into_iter()
            .filter(|(t, _)| *t < live_ms && live_ms - *t < HORIZON_MS)
            .take(cfg.depth.unwrap_or(BACKFILL).saturating_sub(1));
        for (valid_ms, key) in loop_ {
            if covered(valid_ms, &known) {
                continue;
            }
            let volume = Volume {
                key,
                valid_ms,
                url: String::new(),
            };
            let Some(sweep) = from_store(slot(&cfg, &site, &volume), cfg.want).await else {
                continue;
            };
            known.push(sweep.start_ms());
            done.push(valid_ms);
            let event = Event::Backfill {
                site: site.clone(),
                sweep,
                provenance: store_provenance(product(&site), &volume, cfg.want),
            };
            if !send(&events, event).await {
                return;
            }
            stored += 1;
        }
    }
    let day = async |day_ms: i64| -> Result<Vec<Volume>, Fail> {
        match http
            .get(&day_listing_url(&cfg.base, &site, day_ms), None)
            .await
        {
            Ok(Fetched::Body(body, _)) => {
                day_volumes(&body, &cfg.base, &site).map_err(Fail::Answer)
            }
            Ok(Fetched::NotModified) => Ok(Vec::new()),
            Err(Fail::Status(404, _)) => Ok(Vec::new()),
            Err(fail) => Err(fail),
        }
    };
    let mut listed = match day(live_ms).await {
        Ok(listed) => listed,
        Err(fail) => {
            live_log(&site, format_args!("backfill listing: {fail}"));
            let end = Event::HistoryEnd {
                site,
                want: cfg.want,
            };
            send(&events, end).await;
            return;
        }
    };
    // The lowest scan backfills the whole ring, another product an hour.
    let depth = cfg
        .depth
        .unwrap_or_else(|| cfg.want.backfill(BACKFILL, PRODUCT_BACKFILL));
    if needs_yesterday(listed.len(), depth) {
        match day(live_ms - 24 * 60 * 60 * 1000).await {
            Ok(earlier) => listed.extend(earlier),
            Err(fail) => live_log(&site, format_args!("backfill listing (yesterday): {fail}")),
        }
    }
    let mut targets = backfill_targets(listed, live_ms, &known, depth);
    targets.retain(|v| !done.contains(&v.valid_ms));
    let wanted = targets.len();
    // S31: what the history will bring, for `state.loading`.
    let plan = Event::HistoryPlan {
        site: site.clone(),
        want: cfg.want,
        frames: stored + wanted,
    };
    if !send(&events, plan).await {
        return;
    }
    let mut fetched = 0;
    let mut undecoded = 0;
    for volume in targets {
        match fetch_volume(&http, &cfg, &site, &volume).await {
            Ok((sweep, provenance)) => {
                let event = Event::Backfill {
                    site: site.clone(),
                    sweep,
                    provenance,
                };
                if !send(&events, event).await {
                    return;
                }
                fetched += 1;
            }
            Err(VolumeError::Net(fail)) => {
                live_log(
                    &site,
                    format_args!("backfill {}: {fail}; stopping", volume.key),
                );
                break;
            }
            Err(VolumeError::Decode(e)) => {
                live_log(&site, format_args!("backfill {}: {e}", volume.key));
                undecoded += 1;
                if undecoded >= BACKFILL_GIVE_UP {
                    live_log(
                        &site,
                        format_args!(
                            "backfill: {undecoded} volumes in a row did not decode; stopping"
                        ),
                    );
                    break;
                }
            }
        }
        sleep(cfg.backfill_pace).await;
    }
    live_log(
        &site,
        format_args!(
            "backfilled {fetched} of {wanted} earlier volumes, {stored} more from the tilt store"
        ),
    );
    let end = Event::HistoryEnd {
        site,
        want: cfg.want,
    };
    send(&events, end).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sweep::Ray;
    use chrono::NaiveDate;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::AtomicU32;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::mpsc;

    const VARA: &str = include_str!("../tests/smhi/vara-qcvol-20260913T1656Z.json");
    const VARA_DAY: &str = include_str!("../tests/smhi/vara-day-20260913.json");
    const LEKSAND: &str = include_str!("../tests/smhi/leksand-qcvol-20260913T1656Z.json");
    const LEKSAND_DAY: &str = include_str!("../tests/smhi/leksand-day-20260913.json");
    /// `Last-Modified` of the recorded vara listing.
    const VARA_MODIFIED: &str = "Sun, 13 Sep 2026 16:56:01 GMT";

    fn utc(day: u32, hour: u32, minute: u32, second: u32) -> i64 {
        NaiveDate::from_ymd_opt(2026, 9, day)
            .unwrap()
            .and_hms_opt(hour, minute, second)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    /// When the fixtures were recorded.
    fn recorded() -> i64 {
        utc(13, 16, 56, 30)
    }

    #[test]
    fn the_listing_names_the_newest_volume_by_its_dated_url() {
        let newest = newest_volume(VARA.as_bytes(), API, "vara")
            .unwrap()
            .unwrap();
        // Recorded at 16:56 from a cached copy: it lags the day listing
        // (which reaches 16:50) by two volumes.
        assert_eq!(newest.key, "radar_vara_qcvol_202609131640");
        assert_eq!(newest.valid_ms, utc(13, 16, 40, 0));
        // Not the listed `latest.h5`, which moves under a ranged reader.
        assert_eq!(
            newest.url,
            "https://opendata-download-radar.smhi.se/api/version/latest/area/vara/product/qcvol/2026/09/13/radar_vara_qcvol_202609131640.h5"
        );
        assert!(VARA.contains("/qcvol/latest.h5"));
    }

    #[test]
    fn the_next_volume_is_named_the_way_smhi_lists_it() {
        let listed = day_volumes(VARA_DAY.as_bytes(), API, "vara").unwrap();
        for pair in listed.windows(2) {
            assert_eq!(
                next_volume(API, "vara", pair[0].valid_ms + CADENCE_MS),
                pair[1]
            );
        }
        // Across midnight, into the next day's directory.
        let next = next_volume(API, "vara", utc(14, 0, 0, 0));
        assert_eq!(next.key, "radar_vara_qcvol_202609140000");
        assert!(
            next.url
                .ends_with("/qcvol/2026/09/14/radar_vara_qcvol_202609140000.h5")
        );
    }

    #[test]
    fn built_urls_match_every_link_in_the_day_listing() {
        let listed = day_volumes(VARA_DAY.as_bytes(), API, "vara").unwrap();
        assert_eq!(listed.len(), 203);
        let raw: serde_json::Value = serde_json::from_str(VARA_DAY).unwrap();
        for (volume, file) in listed.iter().zip(raw["files"].as_array().unwrap()) {
            assert_eq!(volume.key, file["key"].as_str().unwrap());
            assert_eq!(volume.url, file["formats"][0]["link"].as_str().unwrap());
        }
        assert_eq!(listed.first().unwrap().valid_ms, utc(13, 0, 0, 0));
        assert_eq!(listed.last().unwrap().valid_ms, utc(13, 16, 50, 0));
        assert_eq!(
            day_listing_url(API, "vara", utc(13, 16, 50, 0)),
            format!("{API}/area/vara/product/qcvol/2026/09/13.json")
        );
    }

    #[test]
    fn leksand_lists_a_january_volume_and_an_empty_day() {
        let newest = newest_volume(LEKSAND.as_bytes(), API, "leksand")
            .unwrap()
            .unwrap();
        assert_eq!(
            newest.valid_ms,
            NaiveDate::from_ymd_opt(2026, 1, 13)
                .unwrap()
                .and_hms_opt(12, 40, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        );
        assert!(recorded() - newest.valid_ms >= HORIZON_MS);
        assert!(
            day_volumes(LEKSAND_DAY.as_bytes(), API, "leksand")
                .unwrap()
                .is_empty()
        );
        assert!(
            newest_volume(b"{\"lastFiles\":null}", API, "leksand")
                .unwrap()
                .is_none()
        );
        assert!(newest_volume(b"<html>", API, "leksand").is_err());
    }

    #[test]
    fn a_catalogued_sweep_covers_only_its_own_volume() {
        let valid = utc(13, 16, 50, 0);
        let start = utc(13, 16, 50, 3);
        assert!(covered(valid, &[start]));
        assert!(!covered(valid + 5 * 60_000, &[start]), "the next volume");
        assert!(
            !covered(valid - 5 * 60_000, &[start]),
            "the previous volume"
        );
        assert!(!covered(valid, &[]));
    }

    #[test]
    fn backfill_takes_the_newest_sixty_the_catalog_lacks() {
        let listed = day_volumes(VARA_DAY.as_bytes(), API, "vara").unwrap();
        let live = utc(13, 16, 50, 0);
        let targets = backfill_targets(listed.clone(), live, &[], BACKFILL);
        // Sixty frames with the live one: 16:45 back to 11:55.
        assert_eq!(targets.len(), BACKFILL - 1);
        assert_eq!(targets.first().unwrap().valid_ms, utc(13, 16, 45, 0));
        assert_eq!(targets.last().unwrap().valid_ms, utc(13, 11, 55, 0));
        assert!(targets.windows(2).all(|w| w[0].valid_ms > w[1].valid_ms));
        // Catalogued volumes are not fetched again, and nothing older than
        // the loop is fetched in their place.
        let known = [utc(13, 16, 40, 4), utc(13, 12, 0, 2), utc(13, 9, 0, 3)];
        let targets = backfill_targets(listed.clone(), live, &known, BACKFILL);
        assert_eq!(targets.len(), BACKFILL - 3);
        assert_eq!(targets.last().unwrap().valid_ms, utc(13, 11, 55, 0));
        // A live volume older than the listing's newest (a lagging
        // `lastFiles`) keeps the loop behind it.
        let targets = backfill_targets(listed, utc(13, 12, 0, 0), &[], BACKFILL);
        assert_eq!(targets.first().unwrap().valid_ms, utc(13, 11, 55, 0));
    }

    #[test]
    fn after_midnight_yesterday_fills_the_loop() {
        let yesterday = day_volumes(VARA_DAY.as_bytes(), API, "vara").unwrap();
        // Just after midnight: the 14th lists 00:00 and 00:05.
        let today: Vec<Volume> = [(0, "0000"), (5, "0005")]
            .iter()
            .map(|&(minute, hhmm)| {
                let key = format!("radar_vara_qcvol_20260914{hhmm}");
                let valid_ms = utc(14, 0, minute, 0);
                Volume {
                    url: volume_url(API, "vara", &key, valid_ms),
                    key,
                    valid_ms,
                }
            })
            .collect();
        assert!(today[1].url.contains("/qcvol/2026/09/14/"));
        assert!(needs_yesterday(today.len(), BACKFILL));
        assert!(!needs_yesterday(yesterday.len(), BACKFILL));
        let live = utc(14, 0, 5, 0);
        let mut listed = today;
        listed.extend(yesterday);
        let targets = backfill_targets(listed, live, &[], BACKFILL);
        assert_eq!(targets.len(), BACKFILL - 1);
        assert_eq!(targets[0].valid_ms, utc(14, 0, 0, 0));
        assert_eq!(targets[1].valid_ms, utc(13, 16, 50, 0));
        // Today's 00:00 and 58 of the 13th, which was recorded at 16:56 and
        // ends at 16:50.
        assert_eq!(targets.last().unwrap().valid_ms, utc(13, 12, 5, 0));
    }

    #[test]
    fn failures_back_off_doubling_and_honour_retry_after() {
        let (poll, max) = (POLL, MAX_BACK_OFF);
        assert_eq!(back_off(poll, max, 1, None), Duration::from_secs(120));
        assert_eq!(back_off(poll, max, 2, None), Duration::from_secs(240));
        assert_eq!(back_off(poll, max, 3, None), Duration::from_secs(480));
        assert_eq!(back_off(poll, max, 4, None), max);
        assert_eq!(back_off(poll, max, 40, None), max);
        let after = Some(Duration::from_secs(900));
        assert_eq!(back_off(poll, max, 1, after), Duration::from_secs(900));
        let after = Some(Duration::from_secs(86_400));
        assert_eq!(back_off(poll, max, 1, after), RETRY_AFTER_CAP);
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "120".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(120)));
        headers.insert(
            RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), None);
        assert_eq!(
            parse_content_range("bytes 0-65535/14701179"),
            Some((0, Some(14_701_179)))
        );
        assert_eq!(parse_content_range("bytes */14701179"), None);
        // Seen from SMHI on 2026-09-13 while its cache fetched a volume.
        assert_eq!(parse_content_range("bytes 0-65535/*"), Some((0, None)));
    }

    /// A file served from memory, recording each request.
    struct Local {
        bytes: Vec<u8>,
        log: Arc<StdMutex<Vec<(u64, u64)>>>,
    }
    impl RangeSource for Local {
        fn get(&mut self, offset: u64, len: u64) -> io::Result<(Vec<u8>, Option<u64>)> {
            self.log.lock().unwrap().push((offset, len));
            let total = self.bytes.len() as u64;
            let end = (offset + len).min(total);
            Ok((
                self.bytes[offset as usize..end as usize].to_vec(),
                Some(total),
            ))
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 % 251) as u8).collect()
    }

    /// DEC-2's measured read of vara 10:55Z, replayed: the prefetch and the
    /// six block runs cost 7 requests and 98,304 bytes.
    #[test]
    fn the_reader_reproduces_dec2s_request_count() {
        let bytes = pattern(14_701_179);
        let log = Arc::new(StdMutex::new(Vec::new()));
        let source = Local {
            bytes: bytes.clone(),
            log: log.clone(),
        };
        let mut reader = RangeReader::open(Box::new(source)).unwrap();
        assert_eq!(reader.traffic().total(), 14_701_179);
        for (offset, len) in [
            (40_000u64, 2_000usize),
            (425_984, 4096),
            (372_736, 4096),
            (266_240, 4096),
            (159_744, 4096),
            (479_232, 4096),
            (532_480, 12_288),
            (425_990, 100),
        ] {
            reader.seek(SeekFrom::Start(offset)).unwrap();
            let mut buf = vec![0; len];
            reader.read_exact(&mut buf).unwrap();
            assert_eq!(buf, bytes[offset as usize..offset as usize + len]);
        }
        let traffic = reader.traffic();
        assert_eq!((traffic.requests(), traffic.bytes()), (7, 98_304));
        assert_eq!(log.lock().unwrap()[0], (0, PREFETCH));
        assert_eq!(log.lock().unwrap()[6], (532_480, 12_288));
        // Seeks relative to the end and past it.
        assert_eq!(reader.seek(SeekFrom::End(-10)).unwrap(), 14_701_169);
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, bytes[14_701_169..]);
        assert!(reader.seek(SeekFrom::Current(-20_000_000)).is_err());
    }

    #[test]
    fn a_file_smaller_than_the_prefetch_costs_one_request() {
        let bytes = pattern(10_000);
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mut reader = RangeReader::open(Box::new(Local {
            bytes: bytes.clone(),
            log: log.clone(),
        }))
        .unwrap();
        let mut all = Vec::new();
        reader.read_to_end(&mut all).unwrap();
        assert_eq!(all, bytes);
        assert_eq!(reader.traffic().requests(), 1, "the prefetch holds it all");
    }

    /// A plan whose prefetch is not a whole number of blocks (S26): the
    /// reader rounds it up, so a read across the prefetch's end neither runs
    /// past a short chunk nor asks again for bytes it holds. A block of 0
    /// bytes is refused.
    #[test]
    fn a_prefetch_is_rounded_up_to_whole_blocks() {
        let bytes = pattern(50_000);
        let log = Arc::new(StdMutex::new(Vec::new()));
        let plan = RangePlan {
            prefetch: 10_000,
            block: 4096,
            max_requests: 8,
            max_bytes: 1 << 20,
        };
        let local = |log: &Arc<StdMutex<Vec<(u64, u64)>>>| {
            Box::new(Local {
                bytes: bytes.clone(),
                log: log.clone(),
            })
        };
        let mut reader = RangeReader::open_planned(local(&log), plan, Duration::ZERO).unwrap();
        // Across the asked prefetch's end (10,000) and the rounded one's.
        reader.seek(SeekFrom::Start(9_000)).unwrap();
        let mut buf = vec![0; 8_000];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, bytes[9_000..17_000]);
        assert_eq!(*log.lock().unwrap(), [(0, 12_288), (12_288, 8_192)]);
        // The file's short last block.
        reader.seek(SeekFrom::End(-100)).unwrap();
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, bytes[49_900..]);
        assert_eq!(log.lock().unwrap()[2], (49_152, 848));
        let zero = RangePlan { block: 0, ..plan };
        assert!(RangeReader::open_planned(local(&log), zero, Duration::ZERO).is_err());
    }

    /// The file changes length between requests: SMHI replaced it.
    struct Moving(u64);
    impl RangeSource for Moving {
        fn get(&mut self, _offset: u64, len: u64) -> io::Result<(Vec<u8>, Option<u64>)> {
            self.0 += 1;
            Ok((vec![0; len as usize], Some(1_000_000 + self.0)))
        }
    }

    #[test]
    fn a_volume_that_changes_or_wanders_is_abandoned() {
        let mut reader = RangeReader::open(Box::new(Moving(0))).unwrap();
        reader.seek(SeekFrom::Start(500_000)).unwrap();
        let err = reader.read(&mut [0; 16]).unwrap_err();
        assert!(err.to_string().contains("changed"), "{err}");

        let log = Arc::new(StdMutex::new(Vec::new()));
        let mut reader = RangeReader::open(Box::new(Local {
            bytes: pattern(15_000_000),
            log,
        }))
        .unwrap();
        // A decoder reading every other block of the whole file.
        let mut spent = None;
        for n in 0..2000u64 {
            reader
                .seek(SeekFrom::Start(PREFETCH + n * 2 * BLOCK))
                .unwrap();
            if let Err(e) = reader.read(&mut [0; 16]) {
                spent = Some(e);
                break;
            }
        }
        assert!(spent.unwrap().to_string().contains("budget"));
        assert!(reader.traffic().requests() <= MAX_REQUESTS);
    }

    // -----------------------------------------------------------------------
    // The poller against a local server serving the recorded listings.
    // -----------------------------------------------------------------------

    struct Request {
        path: String,
        headers: HashMap<String, String>,
    }

    struct Response {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    }

    impl Response {
        fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
            Response {
                status,
                headers: Vec::new(),
                body: body.into(),
            }
        }
    }

    type Handler = Arc<dyn Fn(&Request) -> Response + Send + Sync>;
    type Served = Arc<StdMutex<Vec<String>>>;

    /// A minimal HTTP/1.1 server, one request per connection. The log
    /// records `path` plus `range=` and `since=` where present.
    async fn serve(handler: Handler) -> (String, Served) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served: Served = Arc::default();
        let log = served.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let handler = handler.clone();
                let log = log.clone();
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
                    let mut lines = text.lines();
                    let path = lines
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or_default()
                        .to_owned();
                    let headers = lines
                        .filter_map(|l| l.split_once(':'))
                        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
                        .collect::<HashMap<_, _>>();
                    let request = Request { path, headers };
                    let mut entry = request.path.clone();
                    if let Some(range) = request.headers.get("range") {
                        entry += &format!(" range={range}");
                    }
                    if let Some(since) = request.headers.get("if-modified-since") {
                        entry += &format!(" since={since}");
                    }
                    log.lock().unwrap().push(entry);
                    let response = handler(&request);
                    let mut out = format!(
                        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                        response.status,
                        response.body.len()
                    );
                    for (k, v) in &response.headers {
                        out += &format!("{k}: {v}\r\n");
                    }
                    out += "\r\n";
                    let _ = stream.write_all(out.as_bytes()).await;
                    let _ = stream.write_all(&response.body).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        (base, served)
    }

    const VOLUME_BYTES: usize = 600_000;

    /// A stand-in volume: its key at the start, a pattern after.
    fn fake_volume(key: &str) -> Vec<u8> {
        let mut bytes = pattern(VOLUME_BYTES);
        bytes[..key.len()].copy_from_slice(key.as_bytes());
        bytes[key.len()] = 0;
        bytes
    }

    /// Serve `bytes` for a `Range: bytes=a-b` request.
    fn ranged(request: &Request, bytes: &[u8]) -> Response {
        let Some((a, b)) = request
            .headers
            .get("range")
            .and_then(|r| r.strip_prefix("bytes="))
            .and_then(|r| r.split_once('-'))
        else {
            return Response::new(200, bytes.to_vec());
        };
        let total = bytes.len();
        let a: usize = a.parse().unwrap();
        let b = b.parse::<usize>().unwrap().min(total - 1);
        let mut response = Response::new(206, bytes[a..=b].to_vec());
        response
            .headers
            .push(("Content-Range", format!("bytes {a}-{b}/{total}")));
        response
    }

    /// SMHI for one site from the recorded listings: `qcvol.json` answers
    /// 304 to its own `Last-Modified`, the day listing is the recorded day,
    /// and every dated volume URL serves a stand-in volume.
    fn smhi(site: &'static str, listing: &'static str, day: &'static str) -> Handler {
        Arc::new(move |request: &Request| {
            let prefix = format!("/area/{site}/product/qcvol");
            let Some(rest) = request.path.strip_prefix(&prefix) else {
                return Response::new(404, "no such area");
            };
            match rest {
                ".json" => {
                    if request.headers.get("if-modified-since").map(String::as_str)
                        == Some(VARA_MODIFIED)
                    {
                        return Response::new(304, "");
                    }
                    let mut response = Response::new(200, listing);
                    response
                        .headers
                        .push(("Last-Modified", VARA_MODIFIED.into()));
                    response
                }
                "/2026/09/13.json" => Response::new(200, day),
                _ => match rest
                    .strip_prefix("/2026/09/13/")
                    .and_then(|f| f.strip_suffix(".h5"))
                {
                    Some(key) => ranged(request, &fake_volume(key)),
                    None => Response::new(404, "not found"),
                },
            }
        })
    }

    /// The stand-in decoder: reads the key back from the volume's first
    /// bytes, touches two blocks deeper in (as a real decode would), and
    /// reports a sweep that starts 3 s after the key's valid time.
    fn stub_decode(
        mut reader: RangeReader,
        _want: Want,
        _slot: Option<&crate::tilts::Slot>,
    ) -> Result<Scan, String> {
        let mut head = [0u8; 64];
        reader.read_exact(&mut head).map_err(|e| e.to_string())?;
        let key = head.split(|&b| b == 0).next().unwrap();
        let key = std::str::from_utf8(key).map_err(|e| e.to_string())?;
        let digits = key.rsplit('_').next().unwrap_or_default();
        let valid = NaiveDateTime::parse_from_str(digits, "%Y%m%d%H%M")
            .map_err(|e| format!("{key}: {e}"))?
            .and_utc()
            .timestamp_millis();
        for (offset, len) in [(425_984, 100), (532_480, 12_000)] {
            reader
                .seek(SeekFrom::Start(offset))
                .map_err(|e| e.to_string())?;
            let mut buf = vec![0; len];
            reader.read_exact(&mut buf).map_err(|e| e.to_string())?;
        }
        Ok(Scan::Polar(stub_sweep(valid + 3_000)))
    }

    fn stub_sweep(start_ms: i64) -> Sweep {
        Sweep {
            rays: vec![Ray {
                azimuth_deg: 0.5,
                elevation_deg: 0.5,
                time_ms: start_ms,
                codes: vec![0; 4],
            }],
            start_ms,
            end_ms: start_ms + 30_000,
            gates: 4,
            first_gate_m: 250,
            gate_spacing_m: 500,
            scale: 2.0,
            offset: 66.0,
            code1_status: crate::sweep::OUTSIDE_COVERAGE,
        }
    }

    /// `stub_decode` for a product: the product, with the lowest scan it
    /// read on the way (`products::decode_volume`).
    fn stub_product(
        reader: RangeReader,
        want: Want,
        slot: Option<&crate::tilts::Slot>,
    ) -> Result<Scan, String> {
        let Scan::Polar(sweep) = stub_decode(reader, want, slot)? else {
            unreachable!()
        };
        let lowest = stub_sweep(sweep.start_ms);
        Ok(Scan::Product(sweep, want, Some(Box::new(lowest))))
    }

    fn config(base: String) -> Config {
        Config {
            base,
            poll: Duration::from_millis(40),
            max_back_off: Duration::from_millis(200),
            backfill_delay: Duration::ZERO,
            backfill_pace: Duration::ZERO,
            length_pause: Duration::ZERO,
            now_ms: recorded,
            decode: stub_decode,
            want: Want::Lowest,
            store: None,
            depth: None,
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// Collect `event`, but not the backfill's plan and end (S31), which
    /// `backfill_reports_its_plan_and_end` checks alone.
    fn keep(events: &mut Vec<Event>, event: Event) {
        if !matches!(event, Event::HistoryPlan { .. } | Event::HistoryEnd { .. }) {
            events.push(event);
        }
    }

    /// A readable summary of one event.
    fn describe(event: &Event) -> String {
        match event {
            Event::Sweep {
                sweep, complete, ..
            } => format!("sweep {} {complete}", stamp(sweep.start_ms())),
            Event::Backfill { sweep, .. } => format!("backfill {}", stamp(sweep.start_ms())),
            Event::Current { .. } => "current".into(),
            Event::Offline { reason, .. } => format!("offline {reason}"),
            Event::Silent { reason, .. } => format!("silent {reason}"),
            Event::Progress { .. } => "progress".into(),
            Event::HistoryPlan { frames, .. } => format!("plan {frames}"),
            Event::HistoryEnd { .. } => "history end".into(),
        }
    }

    /// Run a poller until `until` holds for the events so far (or 10 s),
    /// then `linger` longer, collecting anything more.
    fn run(
        site: &'static str,
        handler: Handler,
        cached: Vec<i64>,
        until: impl Fn(&[Event]) -> bool,
        linger: Duration,
    ) -> (Vec<Event>, Vec<String>) {
        run_as(
            (Want::Lowest, stub_decode),
            site,
            handler,
            cached,
            until,
            linger,
        )
    }

    /// `run` for a poller following `want` through `decode`.
    fn run_as(
        (want, decode): (Want, Decode),
        site: &'static str,
        handler: Handler,
        cached: Vec<i64>,
        until: impl Fn(&[Event]) -> bool,
        linger: Duration,
    ) -> (Vec<Event>, Vec<String>) {
        runtime().block_on(async {
            let (base, served) = serve(handler).await;
            let (tx, mut rx) = mpsc::channel(16);
            let cfg = Config {
                want,
                decode,
                ..config(base)
            };
            let poller = tokio::spawn(poll_with(cfg, site.into(), tx, cached));
            let mut events = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while !until(&events) {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some(event)) => keep(&mut events, event),
                    Ok(None) | Err(_) => break,
                }
            }
            let deadline = tokio::time::Instant::now() + linger;
            while let Ok(Some(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
                keep(&mut events, event);
            }
            poller.abort();
            let served = served.lock().unwrap().clone();
            (events, served)
        })
    }

    /// The poller test against the recorded listings. The listing names
    /// 16:40 while the day listing already reaches 16:50, so the poller
    /// publishes 16:40, overtakes the lagging listing by probing 16:45 and
    /// 16:50, then backfills the rest of the 60-frame loop. After that,
    /// conditional polls fetch nothing more.
    /// Another product backfills an hour (S26: a product volume costs ~39
    /// requests), and every read that decoded the lowest scan on the way
    /// sends it too, as a second event, from the same volume.
    /// S31: before it fetches, the backfill says how many earlier frames it
    /// will bring (for `state.loading`), and when it has ended; every
    /// planned frame comes between the two.
    #[test]
    fn backfill_reports_its_plan_and_end() {
        let seen = runtime().block_on(async {
            let (base, _served) = serve(smhi("vara", VARA, VARA_DAY)).await;
            let (tx, mut rx) = mpsc::channel(16);
            let cfg = Config {
                want: Want::ColMax,
                decode: stub_product,
                ..config(base)
            };
            let poller = tokio::spawn(poll_with(cfg, "vara".into(), tx, Vec::new()));
            let mut seen: Vec<String> = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while seen.last().is_none_or(|s| s != "history end") {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some(event)) => seen.push(describe(&event)),
                    Ok(None) | Err(_) => break,
                }
            }
            poller.abort();
            seen
        });
        // The live path's three volumes (each with its free lowest scan),
        // then the plan: the hour's other nine product frames...
        let plan = seen
            .iter()
            .position(|s| s.starts_with("plan "))
            .expect("a plan");
        assert_eq!(plan, 6, "{seen:?}");
        assert_eq!(seen[plan], format!("plan {}", PRODUCT_BACKFILL - 3));
        assert_eq!(seen[plan + 1], "backfill 2026-09-13 16:35Z");
        // ...which all come, with their lowest scans, before the end.
        let end = seen
            .iter()
            .position(|s| s == "history end")
            .expect("an end");
        assert_eq!(end - plan - 1, 2 * (PRODUCT_BACKFILL - 3), "{seen:?}");
        assert_eq!(seen[end - 1], "backfill 2026-09-13 15:55Z");
    }

    #[test]
    fn a_product_backfills_an_hour_and_sends_its_lowest_scans_too() {
        let (events, served) = run_as(
            (Want::ColMax, stub_product),
            "vara",
            smhi("vara", VARA, VARA_DAY),
            Vec::new(),
            |events| events.len() >= 2 * PRODUCT_BACKFILL,
            Duration::from_millis(300),
        );
        let seen: Vec<String> = events.iter().map(describe).collect();
        assert_eq!(seen.len(), 2 * PRODUCT_BACKFILL, "{seen:?}");
        assert_eq!(seen[0], "sweep 2026-09-13 16:40Z true");
        assert_eq!(seen[5], "sweep 2026-09-13 16:50Z true");
        assert_eq!(seen[6], "backfill 2026-09-13 16:35Z");
        assert_eq!(seen[2 * PRODUCT_BACKFILL - 1], "backfill 2026-09-13 15:55Z");
        for (i, pair) in events.chunks(2).enumerate() {
            let scans = pair.iter().map(|e| match e {
                Event::Sweep { sweep, .. } | Event::Backfill { sweep, .. } => sweep,
                _ => panic!("{}", describe(e)),
            });
            let [product, lowest] = scans.collect::<Vec<_>>()[..] else {
                unreachable!()
            };
            assert!(
                matches!(product, Scan::Product(_, Want::ColMax, None)),
                "{i}"
            );
            assert!(matches!(lowest, Scan::Polar(_)), "{i}");
            assert_eq!(seen[2 * i], seen[2 * i + 1]);
        }
        let volumes = served.iter().filter(|s| s.contains(".h5")).count();
        assert_eq!(volumes, PRODUCT_BACKFILL * 3, "one read per pair");
    }

    #[test]
    fn the_poller_publishes_the_newest_volume_then_backfills_sixty_frames() {
        let (events, served) = run(
            "vara",
            smhi("vara", VARA, VARA_DAY),
            Vec::new(),
            |events| events.len() >= BACKFILL,
            Duration::from_millis(300),
        );
        let seen: Vec<String> = events.iter().map(describe).collect();
        assert_eq!(seen.len(), BACKFILL, "{seen:?}");
        assert_eq!(
            seen[..3],
            [
                "sweep 2026-09-13 16:40Z true",
                "sweep 2026-09-13 16:45Z true",
                "sweep 2026-09-13 16:50Z true"
            ]
        );
        assert_eq!(seen[3], "backfill 2026-09-13 16:35Z");
        assert_eq!(seen[BACKFILL - 1], "backfill 2026-09-13 11:55Z");
        let Event::Sweep { provenance, .. } = &events[0] else {
            unreachable!()
        };
        assert_eq!(
            provenance,
            "SMHI qcvol radar_vara_qcvol_202609131640: 3 range requests, 81920 of 600000 bytes"
        );
        // Only ranged reads of dated volumes, each read once, 3 requests
        // apiece here (the real decoder's 7 are the reader test above). One
        // day listing: the 13th holds 203 volumes, so yesterday is not
        // asked for.
        let volumes: Vec<&String> = served.iter().filter(|s| s.contains(".h5")).collect();
        assert_eq!(volumes.len(), BACKFILL * 3, "{volumes:?}");
        assert!(
            volumes
                .iter()
                .all(|s| s.contains(" range=bytes=") && !s.contains("latest"))
        );
        assert_eq!(
            served
                .iter()
                .filter(|s| s.contains("202609131640.h5"))
                .count(),
            3
        );
        assert_eq!(
            served
                .iter()
                .filter(|s| s.ends_with("/2026/09/13.json"))
                .count(),
            1
        );
        assert!(!served.iter().any(|s| s.contains("/2026/09/12")));
        // Every poll after the first is conditional, and a 304 fetches
        // nothing.
        let listings: Vec<&String> = served.iter().filter(|s| s.contains("qcvol.json")).collect();
        assert!(listings.len() >= 3, "{listings:?}");
        assert!(listings[0].ends_with("/area/vara/product/qcvol.json"));
        assert!(
            listings[1..]
                .iter()
                .all(|s| s.ends_with(&format!("since={VARA_MODIFIED}")))
        );
    }

    #[test]
    fn a_catalogued_newest_volume_is_current_and_not_fetched() {
        // The catalog holds the newest volume and the half hour of the loop
        // before it: 16:50 back to 14:25.
        let cached: Vec<i64> = (0..30).map(|i| utc(13, 16, 50, 3) - i * 300_000).collect();
        let (events, served) = run(
            "vara",
            smhi("vara", VARA, VARA_DAY),
            cached,
            |events| events.len() > 30,
            Duration::from_millis(100),
        );
        let seen: Vec<String> = events.iter().map(describe).collect();
        assert_eq!(seen.len(), 31, "{seen:?}");
        assert_eq!(seen[0], "current");
        assert_eq!(seen[1], "backfill 2026-09-13 14:20Z");
        assert_eq!(seen[30], "backfill 2026-09-13 11:55Z");
        assert!(!served.iter().any(|s| s.contains("202609131650.h5")));
    }

    #[test]
    fn leksand_is_silent_and_costs_one_listing() {
        let (events, served) = run(
            "leksand",
            smhi("leksand", LEKSAND, LEKSAND_DAY),
            Vec::new(),
            |events| !events.is_empty(),
            Duration::from_millis(150),
        );
        let seen: Vec<String> = events.iter().map(describe).collect();
        assert_eq!(
            seen,
            ["silent leksand has published nothing since 2026-01-13 12:40Z"]
        );
        // Reported once, while polls go on. Neither the January volume nor
        // any day listing is fetched.
        assert!(served.len() >= 2, "{served:?}");
        assert!(
            served
                .iter()
                .all(|s| s.contains("/area/leksand/product/qcvol.json")),
            "{served:?}"
        );
    }

    #[test]
    fn a_failing_listing_backs_off_and_reports_offline_on_the_second_failure() {
        let handler: Handler = Arc::new(|_: &Request| {
            let mut response = Response::new(503, "busy");
            response.headers.push(("Retry-After", "0".into()));
            response
        });
        let (events, served) = run(
            "vara",
            handler,
            Vec::new(),
            |events| !events.is_empty(),
            Duration::ZERO,
        );
        let seen: Vec<String> = events.iter().map(describe).collect();
        assert_eq!(seen, ["offline SMHI: HTTP 503 (retry after 0s)"]);
        assert_eq!(served.len(), 2, "one quiet retry, then offline");
    }

    #[test]
    fn a_throttled_volume_backs_off_and_is_read_on_the_next_poll() {
        let volume_hits = Arc::new(AtomicU32::new(0));
        let hits = volume_hits.clone();
        let inner = smhi("vara", VARA, "{\"files\":[]}");
        let handler: Handler = Arc::new(move |request: &Request| {
            if request.path.ends_with(".h5") && hits.fetch_add(1, Ordering::Relaxed) == 0 {
                return Response::new(429, "slow down");
            }
            inner(request)
        });
        let (events, served) = run(
            "vara",
            handler,
            Vec::new(),
            |events| !events.is_empty(),
            Duration::ZERO,
        );
        let seen: Vec<String> = events.iter().map(describe).collect();
        assert_eq!(seen, ["sweep 2026-09-13 16:40Z true"]);
        let listing_polls = served.iter().filter(|s| s.contains("qcvol.json")).count();
        assert_eq!(listing_polls, 2, "{served:?}");
    }

    #[test]
    fn a_probe_answered_404_is_not_a_failure() {
        // SMHI has published up to 16:45 and the listing still says 16:40.
        let inner = smhi("vara", VARA, "{\"files\":[]}");
        let handler: Handler = Arc::new(move |request: &Request| {
            if request.path.ends_with("_202609131650.h5") {
                return Response::new(404, "not yet");
            }
            inner(request)
        });
        let (events, served) = run(
            "vara",
            handler,
            Vec::new(),
            |events| events.len() >= 2,
            // Long enough for a second probe round even under `mise check`'s
            // load (S16: 300 ms saw only 3 polls there, the last one's probe
            // not yet served).
            Duration::from_secs(2),
        );
        let seen: Vec<String> = events.iter().map(describe).collect();
        assert_eq!(
            seen,
            [
                "sweep 2026-09-13 16:40Z true",
                "sweep 2026-09-13 16:45Z true"
            ]
        );
        // One probe per poll, each a single prefetch request.
        let probes = served
            .iter()
            .filter(|s| s.contains("_202609131650.h5"))
            .count();
        let polls = served.iter().filter(|s| s.contains("qcvol.json")).count();
        assert!(probes >= 2 && probes <= polls, "{served:?}");
        // Caught up with what is published, so the backfill ran.
        assert!(served.iter().any(|s| s.ends_with("/2026/09/13.json")));
    }

    /// Answers without a total length `unknown` times, then like `Local`.
    struct Warming {
        unknown: u32,
        inner: Local,
    }
    impl RangeSource for Warming {
        fn get(&mut self, offset: u64, len: u64) -> io::Result<(Vec<u8>, Option<u64>)> {
            let (bytes, total) = self.inner.get(offset, len)?;
            if self.unknown > 0 {
                self.unknown -= 1;
                return Ok((bytes, None));
            }
            Ok((bytes, total))
        }
    }

    #[test]
    fn an_unknown_length_is_asked_again_then_given_up_on() {
        let local = || Local {
            bytes: pattern(200_000),
            log: Arc::default(),
        };
        let warming = Warming {
            unknown: 2,
            inner: local(),
        };
        let mut reader = RangeReader::open_with(Box::new(warming), Duration::ZERO).unwrap();
        assert_eq!(reader.traffic().total(), 200_000);
        assert_eq!(reader.traffic().requests(), 3);
        // A later block may also arrive without a total.
        reader.seek(SeekFrom::Start(150_000)).unwrap();
        reader.read_exact(&mut [0; 16]).unwrap();
        let cold = Warming {
            unknown: u32::MAX,
            inner: local(),
        };
        let err = RangeReader::open_with(Box::new(cold), Duration::ZERO)
            .err()
            .unwrap();
        assert!(err.to_string().contains("still unknown after 4"), "{err}");
    }

    /// The first 44 bytes of an SMHI volume: superblock version 1, 4-byte
    /// offsets, and the end-of-file address at byte 36.
    fn superblock(eof: [u8; 4]) -> Vec<u8> {
        let mut bytes = HDF5_SIGNATURE.to_vec();
        bytes.extend_from_slice(&[1, 0, 0, 0, 0, 4, 4, 0, 1, 0, 1, 0, 0, 0, 0, 0]);
        bytes.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff]);
        bytes.extend_from_slice(&eof);
        bytes.extend_from_slice(&[0xff; 4]);
        bytes
    }

    #[test]
    fn the_superblock_names_the_length_a_cold_cache_does_not() {
        // vara 2026-09-13 10:55, whose file is 14,701,179 bytes.
        assert_eq!(
            hdf5_eof(&superblock([0x7b, 0x52, 0xe0, 0x00])),
            Some(14_701_179)
        );
        // vara 2026-09-12 18:00, read cold as `bytes 0-65535/*`.
        assert_eq!(
            hdf5_eof(&superblock([0x08, 0x46, 0x38, 0x01])),
            Some(20_465_160)
        );
        assert_eq!(hdf5_eof(&superblock([0xff; 4])), None, "undefined address");
        assert_eq!(hdf5_eof(&pattern(64)), None, "not HDF5");
        assert_eq!(hdf5_eof(&superblock([1, 2, 3, 4])[..30]), None, "cut short");
        // Version 2: 8-byte offsets, the address after base and extension.
        let mut v2 = HDF5_SIGNATURE.to_vec();
        v2.extend_from_slice(&[2, 8, 8, 0]);
        v2.extend_from_slice(&[0; 16]);
        v2.extend_from_slice(&123_456_789u64.to_le_bytes());
        assert_eq!(hdf5_eof(&v2), Some(123_456_789));
        // A cold answer for an HDF5 volume opens at once, on the
        // superblock's word, and later blocks without a total still read.
        let mut bytes = superblock([0x40, 0x0d, 0x03, 0x00]); // 200,000
        bytes.resize(200_000, 7);
        let cold = Warming {
            unknown: u32::MAX,
            inner: Local {
                bytes,
                log: Arc::default(),
            },
        };
        let mut reader = RangeReader::open_with(Box::new(cold), Duration::ZERO).unwrap();
        let traffic = reader.traffic();
        assert_eq!((traffic.requests(), traffic.total()), (1, 200_000));
        reader.seek(SeekFrom::Start(150_000)).unwrap();
        reader.read_exact(&mut [0; 16]).unwrap();
    }

    /// Backfill volumes whose length SMHI's cache does not know yet: 16:30
    /// once, so it is read on the next ask, and 16:35 always, so it is
    /// skipped and the backfill carries on to the end of the loop.
    #[test]
    fn a_volume_of_unknown_length_does_not_end_the_backfill() {
        let inner = smhi("vara", VARA, VARA_DAY);
        let first_1630 = Arc::new(AtomicU32::new(0));
        let handler: Handler = Arc::new(move |request: &Request| {
            let mut response = inner(request);
            let cold = request.path.ends_with("_202609131635.h5")
                || (request.path.ends_with("_202609131630.h5")
                    && first_1630.fetch_add(1, Ordering::Relaxed) == 0);
            if cold {
                for (name, value) in &mut response.headers {
                    if *name == "Content-Range" {
                        *value = value.replace("/600000", "/*");
                    }
                }
            }
            response
        });
        let (events, served) = run(
            "vara",
            handler,
            Vec::new(),
            |events| events.len() >= BACKFILL - 1,
            Duration::from_millis(200),
        );
        let seen: Vec<String> = events.iter().map(describe).collect();
        assert_eq!(seen.len(), BACKFILL - 1, "{seen:?}");
        assert!(seen.contains(&"backfill 2026-09-13 16:30Z".to_owned()));
        assert!(!seen.contains(&"backfill 2026-09-13 16:35Z".to_owned()));
        assert_eq!(seen.last().unwrap(), "backfill 2026-09-13 11:55Z");
        let asked = |key: &str| served.iter().filter(|s| s.contains(key)).count();
        assert_eq!(asked("_202609131635.h5"), 4, "the prefetch, asked 4 times");
        assert_eq!(asked("_202609131630.h5"), 4, "2 prefetches and 2 blocks");
    }
}
