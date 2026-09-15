//! The ORD provider (S15, DEC-13): the Norwegian, Finnish and Danish radars
//! from EUMETNET Open Radar Data's public 24-hour S3 cache.
//!
//! The cache keeps every file ORD receives for about a day and a half, under
//! `YYYY/MM/DD/<CC>/<nod>/<PVOL|SCAN>/<nod>@YYYYMMDDTHHMM@<elevations>@<quantities>.h5`.
//! A station's `sourceId` (`sites.json`) is its `<CC>/<nod>/<PVOL|SCAN>`.
//! Anonymous S3 `ListObjectsV2` lists a day's files, the objects answer
//! `Range` requests, and there is no rate limit (no `X-RateLimit` headers,
//! measured), unlike the ORD API's 200 requests an hour: this provider never
//! calls the API.
//!
//! - Each poll lists the station's files since two cadences before the
//!   newest one dealt with (`start-after`), so a poll costs one small
//!   request. The first poll lists the last `BACKFILL` cadences, which is
//!   also the backfill's listing.
//! - Per nominal time it reads one file: one whose quantities hold `DBZH`
//!   (else `TH`), with the lowest elevation (`choose`). MET Norway splits a
//!   volume per quantity and adds VRADH-only files; FMI writes one SCAN file
//!   per elevation, a few seconds apart; DMI one volume of every quantity.
//!   The newest time waits until its best file is as good as the previous
//!   time's (`complete`), so a Finnish 0.7° scan or a Norwegian TH file that
//!   lands first is not taken for the frame. AEMET (Spain, S32) publishes a
//!   long-range volume at :x0 and a Doppler volume at :x7 every 10 minutes;
//!   only the first is read (`one_task`).
//! - A file is read through `RangeReader` (`plan_for` its listed size and
//!   the product: whole up to `WHOLE_UP_TO` for the lowest scan, up to
//!   `PRODUCT_WHOLE_UP_TO` for any other, else `PLAN`'s 8 KiB blocks) and
//!   decoded by `odim::decode_lowest` at the lowest tilt, like SMHI's
//!   (DEC-2), or by `products::decode_volume` for another product.
//! - Failures back off like SMHI's poller; the second in a row reports
//!   `offline`. A station whose newest file is older than the `unavailable`
//!   threshold is `Silent`.

use super::{Event, ProviderId, RangePlan, Scan, Spec, Staleness};
use crate::odim::{self, Tilt};
use crate::products::Want;
use crate::protocol::Station;
use crate::smhi_live::{Decode, Fail, RangeReader, RangeSource, back_off};
use chrono::{DateTime, NaiveDateTime};
use reqwest::header::{CONTENT_RANGE, RANGE, RETRY_AFTER};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::runtime::Handle;
use tokio::sync::{Semaphore, mpsc::Sender, oneshot};
use tokio::task::spawn_blocking;
use tokio::time::sleep;

/// The cache's root.
pub const CACHE: &str = "https://s3.waw3-1.cloudferro.com/openradar-24h";
/// The credit a row gets when it names none; every row names its owner
/// (`MET Norway`, `FMI`, `DMI`).
pub const ATTRIBUTION: &str = "EUMETNET OPERA, CC BY 4.0";
const USER_AGENT: &str = concat!(
    "omastorm-se/",
    env!("CARGO_PKG_VERSION"),
    " (fork of https://omastorm.com; EUMETNET Open Radar Data)"
);
/// Every NO, FI and DK radar publishes every 5 minutes (measured: the
/// median gap of each radar's day, `scripts/fetch-ord-sites.sh`).
const CADENCE: Duration = Duration::from_secs(5 * 60);
const CADENCE_MS: i64 = 5 * 60 * 1000;
/// Frames the loop holds after a join, counting the live one. The cache has
/// no request limit, so the full ring (`catalog::RING`), like SMHI's.
pub const BACKFILL: usize = 60;
/// Frames any other product backfills after a switch: two hours, what the
/// clients buffer. A file is one request (`plan_for`), so deeper than
/// SMHI's `PRODUCT_BACKFILL` (S26).
pub const PRODUCT_BACKFILL: usize = 24;
/// The same for a per-angle station (S24a, FMI): five files a volume, so an
/// hour, like SMHI's `PRODUCT_BACKFILL`.
pub const SET_BACKFILL: usize = 12;
/// Between listing polls. Files land 1.5–6 minutes after their nominal time.
const POLL: Duration = Duration::from_secs(60);
const MAX_BACK_OFF: Duration = Duration::from_secs(600);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const OFFLINE_AFTER: u32 = 2;
const BACKFILL_DELAY: Duration = Duration::from_secs(3);
const BACKFILL_PACE: Duration = Duration::from_millis(250);
/// Decode failures of one file before the poller stops retrying it.
const GIVE_UP_AFTER: u32 = 2;
/// Files in a row that do not decode before a backfill stops (S20): a
/// failure that repeats is the reader's or the product's, not the file's,
/// and every further try would cost a whole read.
const BACKFILL_GIVE_UP: u32 = 2;
/// A file older than this is not fetched: the cache's day.
const HORIZON_MS: i64 = 24 * 60 * 60 * 1000;
/// A catalogued sweep start belongs to the file whose nominal time is this
/// close. MET Norway's lowest tilt starts up to a minute before it
/// (Hurum 09:29:06 for 09:30), the others just after.
const MATCH_MS: i64 = 150 * 1000;
/// The newest file this far past its peers' elevation still counts as
/// complete: DMI's lowest tilt wanders 0.46–0.51°, FMI's are 0.2° apart.
const ELEVATION_SLACK: f64 = 0.1;
/// `unavailable` for this provider: a station whose newest file is this
/// old is silent.
const SILENT_AFTER_MS: i64 = SPEC.staleness.unavailable.as_millis() as i64;

/// The range plan for a file larger than `WHOLE_UP_TO` (DMI's volumes,
/// 0.9–1.3 MB of 10 tilts × 8 quantities): every tilt's `where` is read to
/// find the lowest, and those reads are spread through the file, so small
/// blocks. S18 measured dksin 09:40 (1,293,200 B): 16 KiB blocks read 25
/// requests and 459 KB, 8 KiB blocks 27 requests and 279 KB, 64 KiB blocks
/// 95% of the file; the sweep is identical under every plan.
pub const PLAN: RangePlan = RangePlan {
    prefetch: 64 * 1024,
    block: 8 * 1024,
    max_requests: 96,
    max_bytes: 4 * 1024 * 1024,
};

/// Files up to this size are read whole, in one request: MET Norway's
/// (0.40–0.58 MB measured) and FMI's (~0.15 MB), where the lowest tilt and
/// its metadata are most of the file anyway (S18: nohur 18 requests for 77%
/// of the file under 16 KiB blocks, fikor 3 for 67%). DMI's are 0.88 MB up.
pub const WHOLE_UP_TO: u64 = 768 * 1024;

/// For any product but the lowest scan, files up to this size are read
/// whole (S26): a product needs several tilts spread through the file, so
/// the block plan fetched most of it anyway, in many requests (norsa's
/// 12-angle files under CMAX: 32 requests for 752,261 of 801,413 bytes; DMI's
/// 1.3 MB volumes 44 requests for 565 KB). The cache has no request limit;
/// one request is what costs, not the bytes.
pub const PRODUCT_WHOLE_UP_TO: u64 = 2 * 1024 * 1024;

/// The plan for a file of `size` bytes (from the listing) read for `want`:
/// all of it in the first request up to `WHOLE_UP_TO` for the lowest scan,
/// `PRODUCT_WHOLE_UP_TO` for any other product, else `PLAN`.
pub fn plan_for(size: Option<u64>, want: Want) -> RangePlan {
    let whole_up_to = if want.is_lowest() {
        WHOLE_UP_TO
    } else {
        PRODUCT_WHOLE_UP_TO
    };
    match size {
        Some(size) if size > 0 && size <= whole_up_to => RangePlan {
            prefetch: size.next_multiple_of(PLAN.block),
            ..PLAN
        },
        _ => PLAN,
    }
}

pub const SPEC: Spec = Spec {
    id: ProviderId::Ord,
    name: "EUMETNET Open Radar Data (24-hour S3 cache)",
    attribution: ATTRIBUTION,
    country: "",
    // MET Norway's and DMI's lowest tilts reach 238–240 km, FMI's 250 km;
    // every row names its own.
    range_km: 240.0,
    cadence: CADENCE,
    staleness: Staleness::from_cadence(CADENCE),
    backfill: BACKFILL,
    ranges: PLAN,
};

/// Development only (S32): a base URL that replaces `CACHE`, so an offline
/// replay can serve saved files and listings from a local server. Unset in
/// every real run.
pub const BASE_ENV: &str = "OMASTORM_ORD_BASE";

/// AEMET's radars (Spain, S32) publish their long-range volume every 10
/// minutes (measured: the median gap of each of the 11, 2026-09-15), landing
/// about 5 minutes after the nominal time.
pub const ES_CADENCE: Duration = Duration::from_secs(10 * 60);

/// A station's cadence: its country's (S32: Spain's 10 minutes), else the
/// provider's 5.
pub fn cadence_for(station: &Station) -> Duration {
    match station.country.as_str() {
        "ES" => ES_CADENCE,
        _ => CADENCE,
    }
}

/// Between listing polls for a Spanish station (review N3): its files come
/// every 10 minutes, so a listing every 2 halves its requests and adds at
/// most a minute to a file's wait (it lands ~5 min after its time: 7 min at
/// worst, inside the mosaic's `DUE_MS` 8 and the 20-min `stale`).
pub const ES_POLL: Duration = Duration::from_secs(2 * 60);

/// The listing poll for `station`: `ES_POLL` for AEMET's instead of the
/// provider's `POLL`; any other interval (a test's) stays as it is.
pub fn poll_for(station: &Station, poll: Duration) -> Duration {
    if poll == POLL && station.country == "ES" {
        ES_POLL
    } else {
        poll
    }
}

/// A station's staleness thresholds: its country's cadence (S32: a Spanish
/// frame is 15 minutes old just before the next lands, so `SPEC`'s 15-minute
/// `stale` would flash every cycle), else the provider's.
pub fn staleness_for(station: &Station) -> Staleness {
    match station.country.as_str() {
        "ES" => Staleness::from_cadence(cadence_for(station)),
        _ => SPEC.staleness,
    }
}

/// One volume fetch at a time for this provider (SMHI's poller has its own
/// permit; the engine polls one station at a time).
static FETCHER: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn format_ms(ms: i64, format: &str) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|t| t.format(format).to_string())
        .unwrap_or_default()
}

fn live_log(site: &str, message: impl fmt::Display) {
    eprintln!(
        "{} Live {site}: {message}",
        format_ms(now_ms(), "%Y-%m-%dT%H:%M:%SZ")
    );
}

// ---------------------------------------------------------------------------
// Files and listings
// ---------------------------------------------------------------------------

/// One file of a station's listing that holds reflectivity.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    /// The whole object key.
    pub key: String,
    /// The nominal time in the name, epoch milliseconds.
    pub valid_ms: i64,
    /// Index of its best quantity in `odim::REFLECTIVITY` (0 = DBZH).
    pub rank: usize,
    /// The lowest elevation its name lists, degrees.
    pub lowest_deg: f64,
    /// The object's size, when the listing gave it (`<Size>`).
    pub size: Option<u64>,
    /// S24a: the files of its nominal time, one per angle, ascending, when
    /// a per-angle station (FMI's `SCAN`) is read for a product other than
    /// the lowest scan (`choose_sets`); empty otherwise.
    pub parts: Vec<Listed>,
}

/// The files of the station's reflectivity task (S32). AEMET publishes two
/// tasks every 10 minutes, each its own nominal time: at :x0 the long-range
/// volume (`DBZH_TH`, 0.5/1.3/2.1°, 250 km) and at :x7 a Doppler volume
/// (`DBZH_VRADH`, 0.5/1.5°, 150 km). Read alike they would alternate range,
/// angles and gate size frame by frame, so while the listing holds a file
/// with `TH`, the Doppler task's files (`Listed::doppler`) are left out. No
/// other radar is affected: MET Norway's VRADH files hold no reflectivity
/// (`Listed::parse` drops them), FMI's and DMI's hold `TH` too. AEMET's
/// (`es…` node codes) are left out always (review S1): with the long-range
/// task down the station goes stale, rather than showing a 150 km scan.
fn one_task(listed: &[Listed]) -> impl Iterator<Item = &Listed> {
    let with_th = listed.iter().any(|f| f.quantities().contains(&"TH"));
    listed
        .iter()
        .filter(move |f| !((with_th || f.aemet()) && f.doppler()))
}

/// Whether a station's files hold one scan each (FMI's `SCAN`, S24a): any
/// product but the lowest scan then reads every file of a nominal time as
/// one volume.
pub fn per_angle(source: &str) -> bool {
    source.ends_with("/SCAN")
}

/// The tilt store's source for one nominal time of a per-angle station's
/// files, read as one volume (S24a): the key without its angle and
/// quantities, `2026/09/15/FI/fikor/SCAN/fikor@20260915T0000`.
pub fn set_source(key: &str) -> String {
    let mut parts = key.splitn(3, '@');
    match (parts.next(), parts.next()) {
        (Some(path), Some(time)) => format!("{path}@{time}"),
        _ => key.to_owned(),
    }
}

/// Whether a stored volume's source names such a set rather than one file.
pub fn is_set_source(source: &str) -> bool {
    source.matches('@').count() == 1
}

/// Per nominal time, every angle's file (S24a, a per-angle station read for
/// a product): the best quantity per angle, ascending by angle in `parts`,
/// the lowest as the entry itself; oldest first. The newest time is left
/// out while it has fewer angles than the time before, or its lowest file
/// is not as good (`choose`'s rule): the rest of it is still arriving.
pub fn choose_sets(listed: &[Listed]) -> Vec<Listed> {
    let mut times: BTreeMap<i64, BTreeMap<i64, Listed>> = BTreeMap::new();
    for file in one_task(listed) {
        let angles = times.entry(file.valid_ms).or_default();
        let tenths = (file.lowest_deg * 10.0).round() as i64;
        match angles.get(&tenths) {
            Some(held) if held.rank <= file.rank => {}
            _ => {
                angles.insert(tenths, file.clone());
            }
        }
    }
    let mut chosen: Vec<Listed> = times
        .into_values()
        .filter_map(|angles| {
            let parts: Vec<Listed> = angles.into_values().collect();
            let lowest = parts.first()?.clone();
            Some(Listed { parts, ..lowest })
        })
        .collect();
    if let [.., earlier, newest] = &chosen[..]
        && (newest.parts.len() < earlier.parts.len() || !newest.complete_after(earlier))
    {
        chosen.pop();
    }
    chosen
}

impl Listed {
    /// A key of the cache, when it names a file with DBZH or TH:
    /// `…/nohur@20260914T0930@0.5_1.0_…@DBZH.h5`.
    pub fn parse(key: &str) -> Option<Listed> {
        let name = key.rsplit('/').next()?.strip_suffix(".h5")?;
        let parts: Vec<&str> = name.split('@').collect();
        let [_, time, elevations, quantities] = parts[..] else {
            return None;
        };
        let valid_ms = NaiveDateTime::parse_from_str(time, "%Y%m%dT%H%M")
            .ok()?
            .and_utc()
            .timestamp_millis();
        let quantities: Vec<&str> = quantities.split('_').collect();
        let rank = odim::REFLECTIVITY
            .iter()
            .position(|q| quantities.contains(q))?;
        let lowest_deg = elevations
            .split('_')
            .map(|e| e.parse::<f64>().ok())
            .collect::<Option<Vec<f64>>>()?
            .into_iter()
            .reduce(f64::min)?;
        Some(Listed {
            key: key.to_owned(),
            valid_ms,
            rank,
            lowest_deg,
            size: None,
            parts: Vec::new(),
        })
    }

    /// The quantities its name lists (`DBZH_TH` → `["DBZH", "TH"]`).
    fn quantities(&self) -> Vec<&str> {
        self.key
            .rsplit('/')
            .next()
            .and_then(|name| name.strip_suffix(".h5"))
            .and_then(|name| name.split('@').nth(3))
            .map_or_else(Vec::new, |q| q.split('_').collect())
    }

    /// Whether it is a Doppler task's file: radial velocity (`VRADH`) with
    /// reflectivity beside it but no `TH` (S32, AEMET's `…@0.5_1.5@DBZH_VRADH.h5`
    /// at :x7, 150 km of 500 m gates, beside the long-range
    /// `…@0.5_1.3_2.1@DBZH_TH.h5` at :x0, 250 km of 1 km gates).
    fn doppler(&self) -> bool {
        let q = self.quantities();
        q.contains(&"VRADH") && !q.contains(&"TH")
    }

    /// Whether it is one of AEMET's (Spain: the node code starts `es`).
    fn aemet(&self) -> bool {
        self.key
            .rsplit('/')
            .next()
            .is_some_and(|n| n.starts_with("es"))
    }

    /// Whether this file is the better one to read for its time.
    fn beats(&self, other: &Listed) -> bool {
        (self.rank, self.lowest_deg) < (other.rank, other.lowest_deg)
    }

    /// Whether this file is as good as `earlier`'s: the same quantity or a
    /// better one, and no higher elevation (give or take `ELEVATION_SLACK`).
    fn complete_after(&self, earlier: &Listed) -> bool {
        self.rank <= earlier.rank && self.lowest_deg <= earlier.lowest_deg + ELEVATION_SLACK
    }
}

/// Per nominal time, the file to read, oldest first. The newest time is
/// left out while its best file is worse than the time before's (the rest
/// of it is still arriving); the next poll sees it again.
pub fn choose(listed: &[Listed]) -> Vec<Listed> {
    let mut best: BTreeMap<i64, Listed> = BTreeMap::new();
    for file in one_task(listed) {
        match best.get(&file.valid_ms) {
            Some(held) if !file.beats(held) => {}
            _ => {
                best.insert(file.valid_ms, file.clone());
            }
        }
    }
    let mut chosen: Vec<Listed> = best.into_values().collect();
    if let [.., earlier, newest] = &chosen[..]
        && !newest.complete_after(earlier)
    {
        chosen.pop();
    }
    chosen
}

/// Whether a catalogued sweep start is the file valid at `valid_ms`.
pub fn covered(valid_ms: i64, known: &[i64]) -> bool {
    known
        .iter()
        .any(|&start| (start - valid_ms).abs() < MATCH_MS)
}

/// What a backfill fetches: of the newest `n` files up to and including the
/// live one, those the catalog lacks, newest first. The live file counts
/// toward `n` but is left to the live path.
pub fn backfill_targets(
    mut chosen: Vec<Listed>,
    live_ms: i64,
    known: &[i64],
    n: usize,
) -> Vec<Listed> {
    chosen.retain(|f| f.valid_ms <= live_ms);
    chosen.sort_by_key(|f| std::cmp::Reverse(f.valid_ms));
    chosen.dedup_by(|a, b| a.valid_ms == b.valid_ms);
    chosen.truncate(n);
    chosen.retain(|f| f.valid_ms != live_ms && !covered(f.valid_ms, known));
    chosen
}

/// The station's day prefix: `2026/09/14/NO/nohur/PVOL/`.
pub fn day_prefix(source: &str, ms: i64) -> String {
    format!("{}/{source}/", format_ms(ms, "%Y/%m/%d"))
}

/// S3 query encoding: everything but the unreserved characters.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// One page of `ListObjectsV2`.
pub fn list_url(
    base: &str,
    prefix: &str,
    start_after: Option<&str>,
    token: Option<&str>,
) -> String {
    let mut url = format!("{base}/?list-type=2&prefix={}", encode(prefix));
    if let Some(after) = start_after {
        url += &format!("&start-after={}", encode(after));
    }
    if let Some(token) = token {
        url += &format!("&continuation-token={}", encode(token));
    }
    url
}

/// The text of every `<tag>…</tag>` in `xml`, unescaped.
fn elements(xml: &str, tag: &str) -> Vec<String> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find(&open) {
        rest = &rest[at + open.len()..];
        let Some(end) = rest.find(&close) else { break };
        out.push(
            rest[..end]
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&"),
        );
        rest = &rest[end + close.len()..];
    }
    out
}

/// A `ListObjectsV2` answer: its keys, and the token of the next page when
/// it is truncated.
pub fn parse_listing(xml: &str) -> Result<(Vec<String>, Option<String>), String> {
    if !xml.contains("<ListBucketResult") {
        return Err("not an S3 listing".into());
    }
    let truncated = elements(xml, "IsTruncated").first().map(String::as_str) == Some("true");
    let token = elements(xml, "NextContinuationToken").into_iter().next();
    if truncated && token.is_none() {
        return Err("a truncated listing without a continuation token".into());
    }
    Ok((elements(xml, "Key"), token.filter(|_| truncated)))
}

/// The listing's keys that name reflectivity files, each with the size the
/// listing gives it (one `<Size>` per `<Contents>`, in order; none if the
/// counts differ).
fn with_sizes(keys: Vec<String>, xml: &str) -> Vec<Listed> {
    let sizes: Vec<Option<u64>> = elements(xml, "Size")
        .iter()
        .map(|s| s.trim().parse().ok())
        .collect();
    let sizes = if sizes.len() == keys.len() {
        sizes
    } else {
        vec![None; keys.len()]
    };
    keys.iter()
        .zip(sizes)
        .filter_map(|(key, size)| Listed::parse(key).map(|file| Listed { size, ..file }))
        .collect()
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

fn retry_of(fail: &Fail) -> Option<Duration> {
    match fail {
        Fail::Status(_, after) => *after,
        _ => None,
    }
}

/// `bytes 0-65535/427967` as (0, Some(427967)); `*` as an unknown length.
fn parse_content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let (span, total) = value.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (start, _) = span.split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        total => Some(total.parse().ok()?),
    };
    Some((start.trim().parse().ok()?, total))
}

#[derive(Clone)]
struct Http {
    client: reqwest::Client,
}

impl Http {
    fn new() -> Result<Self, String> {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(CALL_TIMEOUT)
            .build()
            .map(|client| Http { client })
            .map_err(|e| format!("building the HTTP client: {e}"))
    }

    async fn text(&self, url: &str) -> Result<String, Fail> {
        let response = self.client.get(url).send().await.map_err(|e| {
            crate::netstats::failed(url);
            Fail::Transport(e.to_string())
        })?;
        crate::netstats::answered(url, &response);
        if response.status().as_u16() != 200 {
            return Err(Fail::Status(
                response.status().as_u16(),
                retry_after(response.headers()),
            ));
        }
        response
            .text()
            .await
            .map_err(|e| Fail::Transport(e.to_string()))
    }

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

    /// Every file of `source` listed from `since_ms` (its nominal time) to
    /// `now_ms`, across UTC days, one page at a time.
    async fn list_since(
        &self,
        base: &str,
        source: &str,
        nod: &str,
        since_ms: i64,
        now_ms: i64,
    ) -> Result<Vec<Listed>, Fail> {
        let day_ms = 24 * 60 * 60 * 1000;
        let mut listed = Vec::new();
        let mut day = since_ms.div_euclid(day_ms) * day_ms;
        let mut first = true;
        while day <= now_ms {
            let prefix = day_prefix(source, day);
            let after =
                first.then(|| format!("{prefix}{nod}@{}", format_ms(since_ms, "%Y%m%dT%H%M")));
            let mut token: Option<String> = None;
            loop {
                let url = list_url(base, &prefix, after.as_deref(), token.as_deref());
                let xml = self.text(&url).await?;
                let (keys, next) = parse_listing(&xml).map_err(Fail::Answer)?;
                listed.extend(with_sizes(keys, &xml));
                match next {
                    Some(next) => token = Some(next),
                    None => break,
                }
            }
            day += day_ms;
            first = false;
        }
        Ok(listed)
    }
}

/// Ranged requests from the blocking decode thread, answered on the
/// engine's runtime; the last HTTP failure is kept for the poller.
struct HttpRanges {
    http: Http,
    url: String,
    runtime: Handle,
    failure: Arc<Mutex<Option<Fail>>>,
}

impl RangeSource for HttpRanges {
    fn get(&mut self, offset: u64, len: u64) -> io::Result<(Vec<u8>, Option<u64>)> {
        let (tx, rx) = oneshot::channel();
        let (http, url) = (self.http.clone(), self.url.clone());
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

enum FileError {
    /// The cache failed to answer: back off.
    Net(Fail),
    /// The bytes were unusable: give up on that file only.
    Decode(String),
}

/// The file's place in the tilt store (S27), when the poller has a store.
fn slot(cfg: &Config, site: &str, file: &Listed) -> Option<crate::tilts::Slot> {
    cfg.store.clone().map(|store| crate::tilts::Slot {
        store,
        station: site.to_owned(),
        time_ms: file.valid_ms,
        source: file.key.clone(),
    })
}

/// A frame's provenance when the tilt store made it.
fn store_provenance(key: &str, want: Want) -> String {
    format!(
        "ORD {}{}: {}",
        key.rsplit('/').next().unwrap_or(key),
        crate::products::provenance_tag(want),
        crate::tilts::FROM_STORE
    )
}

/// `want` of a file from the tilt store alone, when it holds every tilt:
/// no request, no fetch permit.
async fn from_store(slot: Option<crate::tilts::Slot>, want: Want) -> Option<Scan> {
    let slot = slot?;
    spawn_blocking(move || slot.compose(want, Tilt::Lowest))
        .await
        .ok()?
        .ok()?
}

/// Read one nominal time of a per-angle station's files as one volume
/// (S24a, FMI): composed from the tilt store when it holds the set, else
/// every file read whole (`plan_for`: one request each) under the provider's
/// fetch permit and kept in the store as one volume (`set_source`, the full
/// angle table, datasets ascending by angle), so every other product of that
/// time then costs no request.
async fn fetch_set(
    http: &Http,
    cfg: &Config,
    site: &str,
    set: &Listed,
) -> Result<(Scan, String), FileError> {
    let want = cfg.want;
    let source = set_source(&set.key);
    let files = set.parts.len();
    // S24a review #7: a time with fewer files than the radar's angles
    // (one never came) is read as what there is, and says so.
    let expected = cfg.angles_per_set.unwrap_or(files).max(files);
    let count = if files < expected {
        format!("{files} of {expected} files")
    } else {
        format!("{files} files")
    };
    let name = format!(
        "{} ({count}){}",
        source.rsplit('/').next().unwrap_or(&source),
        crate::products::provenance_tag(want)
    );
    let slot = cfg.store.clone().map(|store| crate::tilts::Slot {
        store,
        station: site.to_owned(),
        time_ms: set.valid_ms,
        source: source.clone(),
    });
    if let Some(scan) = from_store(slot.clone(), want).await {
        return Ok((scan, format!("ORD {name}: {}", crate::tilts::FROM_STORE)));
    }
    if files < expected {
        live_log(
            site,
            format_args!("{name}: a partial set, stored as {files} scans"),
        );
    }
    let permit = FETCHER
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| FileError::Decode(e.to_string()))?;
    let failure = Arc::new(Mutex::new(None));
    let reads: Vec<(HttpRanges, RangePlan)> = set
        .parts
        .iter()
        .map(|part| {
            let source = HttpRanges {
                http: http.clone(),
                url: format!("{}/{}", cfg.base, part.key),
                runtime: Handle::current(),
                failure: failure.clone(),
            };
            (source, plan_for(part.size, want))
        })
        .collect();
    let joined = spawn_blocking(move || -> Result<(Scan, u32, u64, u64), String> {
        let _permit = permit;
        let (mut requests, mut bytes, mut total) = (0u32, 0u64, 0u64);
        let mut tilts: Vec<crate::products::Tilt> = Vec::new();
        for (source, plan) in reads {
            let reader = RangeReader::open_planned(Box::new(source), plan, Duration::from_secs(1))
                .map_err(|e| e.to_string())?;
            let traffic = reader.traffic();
            let scans = crate::products::scans_of(reader);
            requests += traffic.requests();
            bytes += traffic.bytes();
            total += traffic.total();
            tilts.extend(scans?);
        }
        // Ascending by angle (stable): the store's dataset order.
        tilts.sort_by(|a, b| a.elangle.total_cmp(&b.elangle));
        if let Some(slot) = &slot {
            let table: Vec<crate::products::TiltInfo> =
                tilts.iter().map(crate::products::Tilt::info).collect();
            let refs: Vec<(usize, &crate::products::Tilt)> = tilts.iter().enumerate().collect();
            if let Err(e) = slot.store.save(
                &slot.station,
                slot.time_ms,
                &slot.source,
                Some(&table),
                &refs,
            ) {
                live_log(
                    &slot.station,
                    format_args!("{}: not stored: {e}", slot.source),
                );
            }
        }
        crate::products::assemble(want, tilts).map(|scan| (scan, requests, bytes, total))
    })
    .await;
    match joined {
        Ok(Ok((scan, requests, bytes, total))) => Ok((
            scan,
            format!("ORD {name}: {requests} range requests, {bytes} of {total} bytes"),
        )),
        Ok(Err(e)) => Err(match failure.lock().unwrap().take() {
            Some(fail @ (Fail::Status(..) | Fail::Transport(_))) => FileError::Net(fail),
            _ => FileError::Decode(format!("{name}: {e}")),
        }),
        Err(e) => Err(FileError::Decode(format!("the decoder failed: {e}"))),
    }
}

/// Read and decode one file with ranged requests, holding the provider's
/// fetch permit. Returns the scan and its provenance. A file the tilt store
/// holds is composed from it, with no request (S27).
async fn fetch_file(
    http: &Http,
    cfg: &Config,
    site: &str,
    file: &Listed,
) -> Result<(Scan, String), FileError> {
    if !file.parts.is_empty() {
        return fetch_set(http, cfg, site, file).await;
    }
    let (base, decode, want) = (cfg.base.as_str(), cfg.decode, cfg.want);
    // S24a review #1: a per-angle station's lowest scan comes from that
    // time's stored set when there is one: no request, and the set stays.
    if want.is_lowest() && cfg.angles_per_set.is_some() {
        let set = cfg.store.clone().map(|store| crate::tilts::Slot {
            store,
            station: site.to_owned(),
            time_ms: file.valid_ms,
            source: set_source(&file.key),
        });
        if let Some(scan) = from_store(set, want).await {
            return Ok((scan, store_provenance(&set_source(&file.key), want)));
        }
    }
    let slot = slot(cfg, site, file);
    if let Some(scan) = from_store(slot.clone(), want).await {
        return Ok((scan, store_provenance(&file.key, want)));
    }
    let permit = FETCHER
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| FileError::Decode(e.to_string()))?;
    let failure = Arc::new(Mutex::new(None));
    let source = HttpRanges {
        http: http.clone(),
        url: format!("{base}/{}", file.key),
        runtime: Handle::current(),
        failure: failure.clone(),
    };
    let plan = plan_for(file.size, want);
    let joined = spawn_blocking(move || {
        let _permit = permit;
        let reader = RangeReader::open_planned(Box::new(source), plan, Duration::from_secs(1))
            .map_err(|e| e.to_string())?;
        let traffic = reader.traffic();
        decode(reader, want, slot.as_ref()).map(|scan| (scan, traffic))
    })
    .await;
    let name = file.key.rsplit('/').next().unwrap_or(&file.key);
    match joined {
        Ok(Ok((scan, traffic))) => Ok((
            scan,
            format!(
                "ORD {name}{}: {} range requests, {} of {} bytes",
                crate::products::provenance_tag(want),
                traffic.requests(),
                traffic.bytes(),
                traffic.total()
            ),
        )),
        Ok(Err(e)) => Err(match failure.lock().unwrap().take() {
            Some(fail @ (Fail::Status(..) | Fail::Transport(_))) => FileError::Net(fail),
            _ => FileError::Decode(format!("{name}: {e}")),
        }),
        Err(e) => Err(FileError::Decode(format!("the decoder failed: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// The poller
// ---------------------------------------------------------------------------

/// What the poller needs from its surroundings; tests point `base` at a
/// local server, shorten the waits and fix the clock.
#[derive(Clone)]
pub struct Config {
    pub base: String,
    pub poll: Duration,
    pub max_back_off: Duration,
    pub backfill_delay: Duration,
    pub backfill_pace: Duration,
    pub now_ms: fn() -> i64,
    pub decode: Decode,
    /// The product the poller follows (S20); its backfill depth too.
    pub want: Want,
    /// The tilt store files are read through (S27); `None` reads every
    /// file from the cache, as before.
    pub store: Option<Arc<crate::tilts::Store>>,
    /// Files a backfill reaches back, counting the live one, when not the
    /// product's own depth (S25: a mosaic radar's hour, `poll_lowest`).
    pub depth: Option<usize>,
    /// S24a: `Some(n)` when the station's files hold one scan each (FMI's
    /// `SCAN`), `n` its nominal angles, the files of a whole set;
    /// `poll_with` sets it from the station.
    pub angles_per_set: Option<usize>,
}

impl Config {
    /// Whether this poller reads a per-angle station's files as one volume
    /// (S24a): for any product but the lowest scan.
    fn sets(&self) -> bool {
        self.angles_per_set.is_some() && !self.want.is_lowest()
    }

    pub fn ord() -> Self {
        Config {
            // Development only (S32): `OMASTORM_ORD_BASE` points the poller
            // at a local copy of the cache, for offline replays.
            base: std::env::var(BASE_ENV)
                .ok()
                .filter(|b| !b.is_empty())
                .unwrap_or_else(|| CACHE.to_owned()),
            poll: POLL,
            max_back_off: MAX_BACK_OFF,
            backfill_delay: BACKFILL_DELAY,
            backfill_pace: BACKFILL_PACE,
            now_ms,
            decode,
            want: Want::Lowest,
            store: None,
            depth: None,
            angles_per_set: None,
        }
    }
}

/// The lowest tilt's reflectivity (`odim.rs`), whichever dataset holds it;
/// for another product the angles it needs (`products.rs`). A volume file
/// (MET Norway's, DMI's) holds every angle; FMI's hold one each, and for
/// any product but the lowest scan the poller reads a nominal time's files
/// as one volume instead (S24a, `fetch_set`), past this function.
/// Since S27 through the tilt store (`tilts::decode`).
fn decode(
    reader: RangeReader,
    want: Want,
    slot: Option<&crate::tilts::Slot>,
) -> Result<Scan, String> {
    crate::tilts::decode(reader, want, Tilt::Lowest, slot)
}

/// Poll the cache for `station` until the task is aborted or the event
/// channel closes. Nothing is replayed, so `skip_known` changes nothing.
pub async fn poll(
    station: Station,
    events: Sender<Event>,
    cached: Vec<i64>,
    _skip_known: bool,
    want: Want,
) {
    let cfg = Config {
        want,
        store: crate::tilts::shared(),
        ..Config::ord()
    };
    poll_with(cfg, station, events, cached).await;
}

/// `want` (the lowest scan, or every scan for a height set, S30) of
/// `station`, through the tilt store, backfilling `depth` files counting
/// the live one: one of My mosaic's radars (S25, `mosaic.rs`), whose events
/// go to the mosaic, not to `main.rs`.
pub async fn poll_lowest(
    station: Station,
    events: Sender<Event>,
    known: Vec<i64>,
    depth: usize,
    want: Want,
) {
    let cfg = Config {
        want,
        store: crate::tilts::shared(),
        depth: Some(depth),
        ..Config::ord()
    };
    poll_with(cfg, station, events, known).await;
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// SMHI's: the event, then the lowest scan its read made for free.
use crate::smhi_live::send;

/// See the module documentation. `cached` holds the start times of the
/// frames already catalogued for the station.
pub async fn poll_with(cfg: Config, station: Station, events: Sender<Event>, cached: Vec<i64>) {
    let site = station.id.clone();
    let source = station.source.clone();
    // S24a: a per-angle station (FMI) read for a product other than the
    // lowest scan reads every file of a nominal time as one volume.
    let cfg = Config {
        angles_per_set: per_angle(&source)
            .then(|| crate::products::nominal_angles(&station).len().max(1)),
        // Review N3: Spain's 10-minute files are listed every 2 minutes.
        poll: poll_for(&station, cfg.poll),
        ..cfg
    };
    let sets = cfg.sets();
    let Some(nod) = source.split('/').nth(1).map(str::to_owned) else {
        let reason = format!("{site} names no ORD cache path (sourceId {source:?})");
        send(&events, Event::Offline { site, reason }).await;
        return;
    };
    let http = match Http::new() {
        Ok(http) => http,
        Err(reason) => {
            send(&events, Event::Offline { site, reason }).await;
            return;
        }
    };
    let window_ms = (BACKFILL as i64 + 2) * CADENCE_MS;
    let mut known = cached;
    // The newest file dealt with: fetched, catalogued, too old, or given up on.
    let mut newest: Option<Listed> = None;
    // The newest nominal time listed at all, for silence.
    let mut latest_ms: Option<i64> = None;
    let mut first_listing: Option<Vec<Listed>> = None;
    let mut decode_failures: HashMap<String, u32> = HashMap::new();
    let mut failures = 0u32;
    let mut silent = false;
    let mut backfilling: Option<AbortOnDrop> = None;
    loop {
        let now = (cfg.now_ms)();
        let since = newest
            .as_ref()
            .map_or(now - window_ms, |n| n.valid_ms - 2 * CADENCE_MS);
        let mut failed: Option<Fail> = None;
        let mut listed_ok = false;
        match http.list_since(&cfg.base, &source, &nod, since, now).await {
            Err(fail) => failed = Some(fail),
            Ok(listed) => {
                listed_ok = true;
                if let Some(ms) = listed.iter().map(|f| f.valid_ms).max() {
                    latest_ms = Some(latest_ms.map_or(ms, |held| held.max(ms)));
                }
                let chosen = if sets {
                    choose_sets(&listed)
                } else {
                    choose(&listed)
                };
                let next = chosen
                    .last()
                    .filter(|f| newest.as_ref().is_none_or(|n| f.valid_ms > n.valid_ms))
                    .cloned();
                first_listing.get_or_insert(chosen);
                if let Some(file) = next {
                    let first = newest.is_none();
                    let handled = if covered(file.valid_ms, &known) {
                        if first && !send(&events, Event::Current { site: site.clone() }).await {
                            return;
                        }
                        true
                    } else if now - file.valid_ms >= HORIZON_MS {
                        true
                    } else {
                        match fetch_file(&http, &cfg, &site, &file).await {
                            Ok((sweep, provenance)) => {
                                known.push(sweep.start_ms());
                                let event = Event::Sweep {
                                    site: site.clone(),
                                    sweep,
                                    complete: true,
                                    provenance,
                                };
                                if !send(&events, event).await {
                                    return;
                                }
                                true
                            }
                            Err(FileError::Net(fail)) => {
                                failed = Some(fail);
                                false
                            }
                            Err(FileError::Decode(e)) => {
                                let tries = decode_failures.entry(file.key.clone()).or_default();
                                *tries += 1;
                                live_log(&site, format_args!("{e} (try {tries})"));
                                *tries >= GIVE_UP_AFTER
                            }
                        }
                    };
                    if handled {
                        newest = Some(file);
                    }
                }
            }
        }

        // Silence, judged after any fetch: the last frame shows, then
        // `unavailable`.
        if listed_ok {
            let quiet_since = match latest_ms {
                Some(ms) if now - ms >= SILENT_AFTER_MS => Some(Some(ms)),
                None => Some(None),
                _ => None,
            };
            if let Some(since) = quiet_since
                && !silent
            {
                let reason = match since {
                    Some(ms) => format!(
                        "{site} has published nothing since {}",
                        format_ms(ms, "%Y-%m-%d %H:%MZ")
                    ),
                    None => format!(
                        "ORD's cache holds no {site} file from the last {} hours",
                        window_ms / 3_600_000
                    ),
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
            silent = quiet_since.is_some();
        }

        if backfilling.is_none()
            && failed.is_none()
            && let Some(live) = &newest
            && now - live.valid_ms < HORIZON_MS
        {
            let chosen = first_listing.take().unwrap_or_default();
            backfilling = Some(AbortOnDrop(tokio::spawn(backfill(
                cfg.clone(),
                http.clone(),
                site.clone(),
                events.clone(),
                chosen,
                live.valid_ms,
                known.clone(),
            ))));
        }

        let mut wait = cfg.poll;
        if let Some(fail) = failed {
            failures += 1;
            wait = back_off(cfg.poll, cfg.max_back_off, failures, retry_of(&fail));
            let reason = format!("ORD cache: {fail}");
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

/// First every file of the loop the tilt store can make and the catalog
/// lacks, newest first, with no request and no pacing (S27); then each
/// remaining target (`backfill_targets` of `chosen`) in turn, newest
/// first, as `Event::Backfill`. A network failure ends the backfill; a
/// file that does not decode is skipped.
async fn backfill(
    cfg: Config,
    http: Http,
    site: String,
    events: Sender<Event>,
    chosen: Vec<Listed>,
    live_ms: i64,
    mut known: Vec<i64>,
) {
    // S24a: the poller reads a per-angle station's files as one volume.
    let sets = cfg.sets();
    sleep(cfg.backfill_delay).await;
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
            // S24a: a per-angle station's product comes only from a stored
            // set of its files, never from one file's volume (its lowest
            // scan alone).
            if covered(valid_ms, &known) || (sets && !is_set_source(&key)) {
                continue;
            }
            let file = Listed {
                key,
                valid_ms,
                rank: 0,
                lowest_deg: 0.0,
                size: None,
                parts: Vec::new(),
            };
            let Some(sweep) = from_store(slot(&cfg, &site, &file), cfg.want).await else {
                continue;
            };
            known.push(sweep.start_ms());
            done.push(valid_ms);
            let event = Event::Backfill {
                site: site.clone(),
                sweep,
                provenance: store_provenance(&file.key, cfg.want),
            };
            if !send(&events, event).await {
                return;
            }
            stored += 1;
        }
    }
    let mut targets = backfill_targets(
        chosen,
        live_ms,
        &known,
        cfg.depth.unwrap_or_else(|| {
            let product = if sets { SET_BACKFILL } else { PRODUCT_BACKFILL };
            cfg.want.backfill(BACKFILL, product)
        }),
    );
    targets.retain(|f| !done.contains(&f.valid_ms));
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
    for file in targets {
        match fetch_file(&http, &cfg, &site, &file).await {
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
                undecoded = 0;
            }
            Err(FileError::Net(fail)) => {
                live_log(
                    &site,
                    format_args!("backfill {}: {fail}; stopping", file.key),
                );
                break;
            }
            Err(FileError::Decode(e)) => {
                live_log(&site, format_args!("backfill: {e}"));
                undecoded += 1;
                if undecoded >= BACKFILL_GIVE_UP {
                    live_log(
                        &site,
                        format_args!(
                            "backfill: {undecoded} files in a row did not decode; stopping"
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
            "backfilled {fetched} of {wanted} earlier files, {stored} more from the tilt store"
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
    use crate::sweep::{Ray, Sweep};
    use std::io::{Read, Seek, SeekFrom};
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::mpsc;

    fn utc(day: u32, hour: u32, minute: u32) -> i64 {
        chrono::NaiveDate::from_ymd_opt(2026, 9, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    const NO: &str = "2026/09/14/NO/nohur/PVOL/nohur@20260914T0930@0.5_1.0_1.6_2.4_3.2_4.2_5.4_6.8_8.5_10.4_12.8_15.5@DBZH.h5";

    #[test]
    fn cache_keys_parse_to_time_quantity_and_elevation() {
        let no = Listed::parse(NO).unwrap();
        assert_eq!(
            (no.valid_ms, no.rank, no.lowest_deg),
            (utc(14, 9, 30), 0, 0.5)
        );
        let th = Listed::parse(&NO.replace("@DBZH.h5", "@TH.h5")).unwrap();
        assert_eq!(th.rank, 1);
        let fi = Listed::parse("2026/09/14/FI/fikor/SCAN/fikor@20260914T0940@0.7@DBZH_TH_VRADH.h5")
            .unwrap();
        assert_eq!((fi.rank, fi.lowest_deg), (0, 0.7));
        let dk = Listed::parse(
            "2026/09/14/DK/dksin/PVOL/dksin@20260914T0940@0.49_0.66_0.96@DBZH_LDR_PHIDP_RHOHV_TH_VRAD_WRAD_ZDR.h5",
        )
        .unwrap();
        assert_eq!((dk.rank, dk.lowest_deg), (0, 0.49));
        // No reflectivity, or not a radar file: skipped.
        assert_eq!(
            Listed::parse("2026/09/14/NO/nohur/PVOL/nohur@20260914T0936@2.6_5.2@VRADH.h5"),
            None
        );
        assert_eq!(Listed::parse("2026/09/14/NO/nohur/PVOL/readme.txt"), None);
        assert_eq!(Listed::parse("a/nohur@2026091409@0.5@DBZH.h5"), None);
        assert_eq!(Listed::parse("a/nohur@20260914T0930@x@DBZH.h5"), None);
    }

    fn file(minute: u32, quantities: &str, elevation: &str) -> Listed {
        Listed::parse(&format!(
            "2026/09/14/XX/xxxxx/SCAN/xxxxx@20260914T09{minute:02}@{elevation}@{quantities}.h5"
        ))
        .unwrap()
    }

    #[test]
    fn one_file_per_time_dbzh_first_then_the_lowest_tilt() {
        // MET Norway: TH, DBZH and a VRADH-only file per time (the last is
        // never listed as reflectivity).
        let chosen = choose(&[
            file(30, "TH", "0.5_1.0"),
            file(30, "DBZH", "0.5_1.0"),
            file(35, "TH", "0.5_2.6"),
            file(35, "DBZH", "0.5_2.6"),
        ]);
        assert_eq!(chosen.len(), 2);
        assert!(chosen.iter().all(|f| f.rank == 0));
        assert!(chosen[0].valid_ms < chosen[1].valid_ms);
        // FMI: one file per elevation; the lowest wins.
        let chosen = choose(&[
            file(35, "DBZH_TH_VRADH", "0.7"),
            file(35, "DBZH_TH_VRADH", "0.5"),
            file(35, "DBZH_TH_VRADH", "1.5"),
        ]);
        assert_eq!(chosen[0].lowest_deg, 0.5);
    }

    #[test]
    fn the_newest_time_waits_for_its_lowest_scan_and_its_dbzh() {
        let before = [
            file(35, "DBZH_TH_VRADH", "0.5"),
            file(35, "DBZH_TH_VRADH", "0.7"),
        ];
        // FMI's 0.7° landed before the 0.5°: 09:40 is not ready.
        let early = choose(&[&before[..], &[file(40, "DBZH_TH_VRADH", "0.7")]].concat());
        assert_eq!(early.last().unwrap().valid_ms, utc(14, 9, 35));
        let ready = choose(
            &[
                &before[..],
                &[
                    file(40, "DBZH_TH_VRADH", "0.7"),
                    file(40, "DBZH_TH_VRADH", "0.5"),
                ],
            ]
            .concat(),
        );
        assert_eq!(ready.last().unwrap().valid_ms, utc(14, 9, 40));
        // MET Norway's TH file lands first: wait for the DBZH one.
        let th_first = choose(&[file(35, "DBZH", "0.5"), file(40, "TH", "0.5")]);
        assert_eq!(th_first.last().unwrap().valid_ms, utc(14, 9, 35));
        // DMI's lowest tilt wanders a few hundredths: still complete.
        let dk = choose(&[
            file(35, "DBZH_TH", "0.46_0.66"),
            file(40, "DBZH_TH", "0.51_0.65"),
        ]);
        assert_eq!(dk.last().unwrap().valid_ms, utc(14, 9, 40));
        // A lone time has nothing to wait for; an older incomplete time is
        // taken as it is once a later one exists.
        assert_eq!(choose(&[file(40, "TH", "0.7")]).len(), 1);
        let healed = choose(&[
            file(35, "DBZH", "0.5"),
            file(40, "TH", "0.5"),
            file(45, "TH", "0.5"),
        ]);
        assert_eq!(healed.len(), 3);
    }

    #[test]
    fn aemet_s_doppler_task_is_left_out() {
        // S32: every 10 minutes the long-range volume at :x0 and the Doppler
        // volume at :x7, each its own nominal time.
        let es = |minute: u32, rest: &str| {
            Listed::parse(&format!(
                "2026/09/15/ES/esahr/PVOL/esahr@20260915T09{minute:02}@{rest}.h5"
            ))
            .unwrap()
        };
        let listed = [
            es(20, "0.5_1.3_2.1@DBZH_TH"),
            es(27, "0.5_1.5@DBZH_VRADH"),
            es(30, "0.5_1.3_2.1@DBZH_TH"),
            es(37, "0.5_1.5@DBZH_VRADH"),
        ];
        assert!(listed[1].doppler() && !listed[0].doppler());
        let times =
            |chosen: Vec<Listed>| -> Vec<i64> { chosen.iter().map(|f| f.valid_ms).collect() };
        assert_eq!(times(choose(&listed)), [utc(15, 9, 20), utc(15, 9, 30)]);
        assert_eq!(
            times(choose_sets(&listed)),
            [utc(15, 9, 20), utc(15, 9, 30)]
        );
        // Review S1: AEMET's Doppler file is not read even alone (the
        // long-range task down: the station goes stale instead); another
        // network's, with no TH file to prefer, still is (the generic rule).
        assert!(choose(&[es(27, "0.5_1.5@DBZH_VRADH")]).is_empty());
        assert!(choose_sets(&[es(27, "0.5_1.5@DBZH_VRADH")]).is_empty());
        assert_eq!(choose(&[file(27, "DBZH_VRADH", "0.5_1.5")]).len(), 1);
        // Review N3: Spain's listings every 2 minutes; a test's interval and
        // every other country's stay.
        let station = |country: &str| Station {
            country: country.into(),
            ..Station::default()
        };
        assert_eq!(poll_for(&station("ES"), POLL), ES_POLL);
        assert_eq!(poll_for(&station("NO"), POLL), POLL);
        let quick = Duration::from_millis(40);
        assert_eq!(poll_for(&station("ES"), quick), quick);
        assert_eq!(cadence_for(&station("ES")), ES_CADENCE);
        assert_eq!(cadence_for(&station("FI")), CADENCE);
        // FMI's DBZH_TH_VRADH scans are no Doppler task: all three kept.
        assert!(!file(35, "DBZH_TH_VRADH", "0.5").doppler());
    }

    #[test]
    fn a_catalogued_sweep_covers_its_file_from_a_minute_early() {
        let valid = utc(14, 9, 30);
        // Hurum's 09:30 file starts at 09:29:06.
        assert!(covered(valid, &[valid - 54_000]));
        assert!(covered(valid, &[valid + 1_000]));
        assert!(!covered(valid, &[valid - 5 * 60_000 + 1_000]));
        assert!(!covered(valid, &[valid + 5 * 60_000 - 54_000]));
        assert!(!covered(valid, &[]));
    }

    #[test]
    fn backfill_takes_the_newest_sixty_the_catalog_lacks() {
        let chosen: Vec<Listed> = (0..70)
            .map(|i| {
                let ms = utc(14, 9, 40) - i * CADENCE_MS;
                Listed::parse(&format!(
                    "p/nohur@{}@0.5@DBZH.h5",
                    format_ms(ms, "%Y%m%dT%H%M")
                ))
                .unwrap()
            })
            .collect();
        let live = utc(14, 9, 40);
        let known = [utc(14, 9, 35) - 50_000];
        let targets = backfill_targets(chosen, live, &known, 60);
        assert_eq!(
            targets.len(),
            58,
            "60 less the live one and a catalogued one"
        );
        assert_eq!(targets[0].valid_ms, utc(14, 9, 30));
        assert!(targets.windows(2).all(|w| w[0].valid_ms > w[1].valid_ms));
    }

    #[test]
    fn listings_parse_and_urls_encode() {
        let xml = r#"<?xml version="1.0"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>openradar-24h</Name><IsTruncated>true</IsTruncated><Contents><Key>2026/09/14/NO/nohur/PVOL/a&amp;b@x.h5</Key><Size>1</Size></Contents><Contents><Key>k2</Key></Contents><NextContinuationToken>1/2+3=</NextContinuationToken></ListBucketResult>"#;
        let (keys, token) = parse_listing(xml).unwrap();
        assert_eq!(keys, ["2026/09/14/NO/nohur/PVOL/a&b@x.h5", "k2"]);
        assert_eq!(token.as_deref(), Some("1/2+3="));
        let last = xml.replace("<IsTruncated>true", "<IsTruncated>false");
        assert_eq!(parse_listing(&last).unwrap().1, None);
        assert!(parse_listing("<Error><Code>NoSuchBucket</Code></Error>").is_err());
        assert_eq!(
            list_url(
                "B",
                "2026/09/14/NO/nohur/PVOL/",
                Some("x/nohur@20260914T0920"),
                Some("1/2+3=")
            ),
            "B/?list-type=2&prefix=2026%2F09%2F14%2FNO%2Fnohur%2FPVOL%2F\
             &start-after=x%2Fnohur%4020260914T0920&continuation-token=1%2F2%2B3%3D"
        );
        assert_eq!(
            day_prefix("FI/fikor/SCAN", utc(14, 23, 59)),
            "2026/09/14/FI/fikor/SCAN/"
        );
    }

    #[test]
    fn ords_spec_is_five_minutes_and_sixty_frames() {
        assert_eq!(SPEC.cadence, Duration::from_secs(300));
        assert_eq!(
            SPEC.staleness,
            Staleness::from_cadence(Duration::from_secs(300))
        );
        assert_eq!(SILENT_AFTER_MS, 30 * 60 * 1000);
        assert_eq!(SPEC.backfill, crate::catalog::RING);
    }

    // -----------------------------------------------------------------------
    // The poller against a stand-in cache
    // -----------------------------------------------------------------------

    type Served = Arc<StdMutex<Vec<String>>>;

    /// `%XX` back to bytes.
    fn decode_query(value: &str) -> String {
        let bytes = value.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() + 1 {
                out.push(u8::from_str_radix(&value[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    const FILE_BYTES: usize = 200_000;

    /// A stand-in file: its key at the start, zeros after.
    fn fake_file(key: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; FILE_BYTES];
        bytes[..key.len()].copy_from_slice(key.as_bytes());
        bytes
    }

    /// A stand-in S3 bucket holding `keys`, pages of `page` keys; `fail`
    /// answers every listing with that status instead.
    async fn bucket(
        keys: Vec<String>,
        page: usize,
        fail: Option<u16>,
        body: fn(&str) -> Vec<u8>,
    ) -> (String, Served) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served: Served = Arc::default();
        let log = served.clone();
        let keys = Arc::new(keys);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (keys, log) = (keys.clone(), log.clone());
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
                    let (status, headers, body) = if let Some(query) = path.strip_prefix("/?") {
                        let q: HashMap<String, String> = query
                            .split('&')
                            .filter_map(|kv| kv.split_once('='))
                            .map(|(k, v)| (k.to_owned(), decode_query(v)))
                            .collect();
                        let mut entry = format!(
                            "list {} after={}",
                            q["prefix"],
                            q.get("start-after").map_or("-", String::as_str)
                        );
                        if let Some(token) = q.get("continuation-token") {
                            entry += &format!(" token={token}");
                        }
                        log.lock().unwrap().push(entry);
                        if let Some(code) = fail {
                            (code, String::new(), b"<Error/>".to_vec())
                        } else {
                            let from: usize = q
                                .get("continuation-token")
                                .map_or(0, |t| t.parse().unwrap());
                            let matching: Vec<&String> = keys
                                .iter()
                                .filter(|k| k.starts_with(&q["prefix"]))
                                .filter(|k| {
                                    q.get("start-after").is_none_or(|a| k.as_str() > a.as_str())
                                })
                                .collect();
                            let end = (from + page).min(matching.len());
                            let mut xml = String::from("<ListBucketResult>");
                            for k in &matching[from..end] {
                                xml += &format!("<Contents><Key>{k}</Key></Contents>");
                            }
                            let more = end < matching.len();
                            xml += &format!("<IsTruncated>{more}</IsTruncated>");
                            if more {
                                xml += &format!(
                                    "<NextContinuationToken>{end}</NextContinuationToken>"
                                );
                            }
                            xml += "</ListBucketResult>";
                            (200, String::new(), xml.into_bytes())
                        }
                    } else {
                        let key = path.trim_start_matches('/').to_owned();
                        let key = decode_query(&key);
                        let bytes = body(&key);
                        let (a, b) = range
                            .as_deref()
                            .and_then(|r| r.trim().split_once('-'))
                            .unwrap();
                        let a: usize = a.parse().unwrap();
                        let b = b.parse::<usize>().unwrap().min(bytes.len() - 1);
                        log.lock().unwrap().push(format!("get {key} {a}-{b}"));
                        (
                            206,
                            format!("Content-Range: bytes {a}-{b}/{}\r\n", bytes.len()),
                            bytes[a..=b].to_vec(),
                        )
                    };
                    let out = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
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

    /// Reads the key back from the file's first bytes, touches a block
    /// deeper in, and reports a sweep starting 54 s before the key's time
    /// (as MET Norway's do).
    fn stub_decode(
        mut reader: RangeReader,
        _want: Want,
        _slot: Option<&crate::tilts::Slot>,
    ) -> Result<Scan, String> {
        let mut head = [0u8; 160];
        reader.read_exact(&mut head).map_err(|e| e.to_string())?;
        let key = std::str::from_utf8(head.split(|&b| b == 0).next().unwrap()).unwrap();
        if key.contains("@TH.h5") {
            return Err(format!("{key}: the poller should read DBZH"));
        }
        let time = key.rsplit('/').next().unwrap().split('@').nth(1).unwrap();
        let valid = NaiveDateTime::parse_from_str(time, "%Y%m%dT%H%M")
            .map_err(|e| e.to_string())?
            .and_utc()
            .timestamp_millis();
        reader
            .seek(SeekFrom::Start(150_000))
            .map_err(|e| e.to_string())?;
        let mut buf = vec![0; 1000];
        reader.read_exact(&mut buf).map_err(|e| e.to_string())?;
        Ok(Scan::Polar(stub_sweep(valid - 54_000)))
    }

    fn stub_sweep(start_ms: i64) -> Sweep {
        Sweep {
            rays: vec![Ray {
                azimuth_deg: 0.25,
                elevation_deg: 0.5,
                time_ms: start_ms,
                codes: vec![0; 4],
            }],
            start_ms,
            end_ms: start_ms + 60_000,
            gates: 4,
            first_gate_m: 125,
            gate_spacing_m: 250,
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

    /// 09:46 on 2026-09-14, just after Hurum's 09:40 files landed.
    fn recorded() -> i64 {
        utc(14, 9, 46)
    }

    /// Just after midnight on 2026-09-15.
    fn after_midnight() -> i64 {
        utc(15, 0, 6)
    }

    /// Hurum's files every 5 minutes from `from` to `to` (inclusive): a TH
    /// and a DBZH volume per time, a VRADH-only file after each.
    fn hurum(from: i64, to: i64) -> Vec<String> {
        let mut keys = Vec::new();
        let mut t = from;
        while t <= to {
            let (day, stamp) = (format_ms(t, "%Y/%m/%d"), format_ms(t, "%Y%m%dT%H%M"));
            let later = format_ms(t + 60_000, "%Y%m%dT%H%M");
            for q in ["DBZH", "TH"] {
                keys.push(format!(
                    "{day}/NO/nohur/PVOL/nohur@{stamp}@0.5_1.0_1.6@{q}.h5"
                ));
            }
            keys.push(format!(
                "{day}/NO/nohur/PVOL/nohur@{later}@2.6_5.2@VRADH.h5"
            ));
            t += CADENCE_MS;
        }
        keys.sort();
        keys
    }

    fn station() -> Station {
        let mut s = crate::providers::table()
            .sites
            .into_iter()
            .find(|s| s.id == "nohur")
            .unwrap();
        assert_eq!(s.source, "NO/nohur/PVOL");
        s.source = "NO/nohur/PVOL".into();
        s
    }

    /// Collect `event`, but not the backfill's plan and end (S31).
    fn keep(events: &mut Vec<Event>, event: Event) {
        if !matches!(event, Event::HistoryPlan { .. } | Event::HistoryEnd { .. }) {
            events.push(event);
        }
    }

    fn describe(event: &Event) -> String {
        match event {
            Event::Sweep { sweep, .. } => {
                format!("sweep {}", format_ms(sweep.start_ms(), "%H:%M:%S"))
            }
            Event::Backfill { sweep, .. } => {
                format!("backfill {}", format_ms(sweep.start_ms(), "%H:%M:%S"))
            }
            Event::Current { .. } => "current".into(),
            Event::Offline { reason, .. } => format!("offline {reason}"),
            Event::Silent { reason, .. } => format!("silent {reason}"),
            Event::Progress { .. } => "progress".into(),
            Event::HistoryPlan { frames, .. } => format!("plan {frames}"),
            Event::HistoryEnd { .. } => "history end".into(),
        }
    }

    fn run(
        keys: Vec<String>,
        fail: Option<u16>,
        now_ms: fn() -> i64,
        cached: Vec<i64>,
        until: impl Fn(&[Event]) -> bool,
        linger: Duration,
    ) -> (Vec<Event>, Vec<String>) {
        run_as(
            (Want::Lowest, stub_decode),
            keys,
            fail,
            now_ms,
            cached,
            until,
            linger,
        )
    }

    /// `run` for a poller following `want` through `decode`.
    fn run_as(
        (want, decode): (Want, Decode),
        keys: Vec<String>,
        fail: Option<u16>,
        now_ms: fn() -> i64,
        cached: Vec<i64>,
        until: impl Fn(&[Event]) -> bool,
        linger: Duration,
    ) -> (Vec<Event>, Vec<String>) {
        run_full(
            (want, decode, None, fake_file),
            keys,
            fail,
            now_ms,
            cached,
            until,
            linger,
        )
    }

    /// The poller's product, decoder and tilt store (S27), and what the
    /// bucket serves for a key.
    type Setup = (
        Want,
        Decode,
        Option<Arc<crate::tilts::Store>>,
        fn(&str) -> Vec<u8>,
    );

    /// `run_as` through a tilt store, the bucket serving `body` for a key.
    fn run_full(
        setup: Setup,
        keys: Vec<String>,
        fail: Option<u16>,
        now_ms: fn() -> i64,
        cached: Vec<i64>,
        until: impl Fn(&[Event]) -> bool,
        linger: Duration,
    ) -> (Vec<Event>, Vec<String>) {
        run_station(station(), setup, keys, fail, now_ms, cached, until, linger)
    }

    /// `run_full` for any station (S24a: an FMI radar's per-angle files).
    #[allow(clippy::too_many_arguments)]
    fn run_station(
        station: Station,
        (want, decode, store, body): Setup,
        keys: Vec<String>,
        fail: Option<u16>,
        now_ms: fn() -> i64,
        cached: Vec<i64>,
        until: impl Fn(&[Event]) -> bool,
        linger: Duration,
    ) -> (Vec<Event>, Vec<String>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (base, served) = bucket(keys, 100, fail, body).await;
            let cfg = Config {
                base,
                poll: Duration::from_millis(40),
                max_back_off: Duration::from_millis(200),
                backfill_delay: Duration::ZERO,
                backfill_pace: Duration::ZERO,
                now_ms,
                decode,
                want,
                store,
                depth: None,
                angles_per_set: None,
            };
            let (tx, mut rx) = mpsc::channel(16);
            let poller = tokio::spawn(poll_with(cfg, station, tx, cached));
            let mut events = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
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

    /// S20's multi-angle Hurum file, whatever the key.
    fn nohur_file(_key: &str) -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../data/raw/ord_nohur_202609140930_tilts.h5"
        ))
        .unwrap()
    }

    /// S27's exit, offline: CMAX reads each file once and keeps its tilts
    /// (the real decoder, through the tilt store); a switch to CAPPI1, back
    /// to CMAX, then to the lowest scan reads no file at all, listings
    /// aside, and each run makes the same frames. Every key serves the
    /// same file, whose scan starts 09:29:06, so the poller counts 09:30
    /// as catalogued once it has read it: 8 files of the 9 times.
    #[test]
    fn a_second_product_reads_no_file() {
        let dir = std::env::temp_dir().join(format!("omastorm-ord-tilts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Arc::new(crate::tilts::Store::open(dir.clone(), u64::MAX).unwrap());
        let keys = hurum(utc(14, 9, 0), utc(14, 9, 40));
        const FILES: usize = 8;
        let mut gets = Vec::new();
        for want in [
            Want::ColMax,
            Want::Cappi(1000.0, 1000),
            Want::ColMax,
            Want::Lowest,
        ] {
            // A product sends its free lowest scan too.
            let expected = if want.is_lowest() { FILES } else { 2 * FILES };
            let (events, served) = run_full(
                (want, decode, Some(store.clone()), nohur_file),
                keys.clone(),
                None,
                recorded,
                Vec::new(),
                |events| events.len() >= expected,
                Duration::from_millis(300),
            );
            let label = want.variant();
            assert_eq!(events.len(), expected, "{label}");
            let from_store = events
                .iter()
                .filter(|e| match e {
                    Event::Sweep { provenance, .. } | Event::Backfill { provenance, .. } => {
                        provenance.contains(crate::tilts::FROM_STORE)
                    }
                    _ => false,
                })
                .count();
            let read = served.iter().filter(|s| s.starts_with("get")).count();
            eprintln!("{label}: {read} file requests, {from_store} frames from the store");
            gets.push(read);
            if gets.len() > 1 {
                assert_eq!(read, 0, "{label}: {served:?}");
                // The free lowest scans name the read they rode along with.
                assert_eq!(from_store, FILES, "{label}");
            }
        }
        assert!(gets[0] >= FILES, "{gets:?}");
        let used = store.usage(Some("nohur")).unwrap();
        assert_eq!(
            used.tilts,
            FILES as u64 * 4,
            "the fixture's four tilts a file"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// S24a: Korppoo's five one-angle files every 5 minutes from `from` to
    /// `to` (inclusive), as ORD's cache names them.
    fn fikor_keys(from: i64, to: i64) -> Vec<String> {
        let mut keys = Vec::new();
        let mut t = from;
        while t <= to {
            let (day, stamp) = (format_ms(t, "%Y/%m/%d"), format_ms(t, "%Y%m%dT%H%M"));
            for angle in ["0.5", "0.7", "1.5", "3.0", "5.0"] {
                keys.push(format!(
                    "{day}/FI/fikor/SCAN/fikor@{stamp}@{angle}@DBZH_TH_VRADH.h5"
                ));
            }
            t += CADENCE_MS;
        }
        keys.sort();
        keys
    }

    /// The fixture file of a key's angle (S24a), whatever its time: every
    /// time's scans start 2026-09-15 00:00.
    fn fikor_file(key: &str) -> Vec<u8> {
        let angle = key.split('@').nth(2).unwrap().replace('.', "");
        std::fs::read(format!(
            "{}/../data/raw/ord_fikor_202609150000_scan{angle}.h5",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn korppoo() -> Station {
        let s = crate::providers::table()
            .sites
            .into_iter()
            .find(|s| s.id == "fikor")
            .unwrap();
        assert_eq!(s.source, "FI/fikor/SCAN");
        s
    }

    #[test]
    fn a_set_is_every_angle_of_a_time_and_waits_for_them() {
        let key = |m: u32, angle: &str, q: &str| {
            format!("2026/09/15/FI/fikor/SCAN/fikor@20260915T00{m:02}@{angle}@{q}.h5")
        };
        let mut keys = vec![
            key(0, "0.5", "DBZH_TH_VRADH"),
            key(0, "0.7", "TH"),
            key(0, "0.7", "DBZH_TH_VRADH"),
            key(0, "1.5", "DBZH_TH_VRADH"),
            key(0, "5.0", "VRADH"),
            key(5, "0.5", "DBZH_TH_VRADH"),
            key(5, "0.7", "DBZH_TH_VRADH"),
            key(5, "1.5", "DBZH_TH_VRADH"),
            key(10, "0.5", "DBZH_TH_VRADH"),
            key(10, "0.7", "DBZH_TH_VRADH"),
        ];
        let sets = |keys: &[String]| {
            choose_sets(
                &keys
                    .iter()
                    .filter_map(|k| Listed::parse(k))
                    .collect::<Vec<_>>(),
            )
        };
        let chosen = sets(&keys);
        assert_eq!(chosen.len(), 2, "00:10 has 2 angles of 3 so far");
        for set in &chosen {
            let angles: Vec<f64> = set.parts.iter().map(|p| p.lowest_deg).collect();
            assert_eq!(angles, [0.5, 0.7, 1.5], "a VRADH-only file is no part");
            assert!(set.parts.iter().all(|p| p.rank == 0), "DBZH before TH");
            assert_eq!(set.key, set.parts[0].key, "the lowest file names the set");
        }
        keys.push(key(10, "1.5", "DBZH_TH_VRADH"));
        assert_eq!(sets(&keys).len(), 3, "complete now");
        let at = |i: usize| sets(&keys)[i].valid_ms;
        assert_eq!((at(0), at(2)), (utc(15, 0, 0), utc(15, 0, 10)));
        // The set's name in the tilt store, and what names one.
        let first = &keys[0];
        assert_eq!(
            set_source(first),
            "2026/09/15/FI/fikor/SCAN/fikor@20260915T0000"
        );
        assert!(is_set_source(&set_source(first)) && !is_set_source(first));
        assert!(per_angle("FI/fikor/SCAN") && !per_angle("NO/nohur/PVOL"));
    }

    /// S24a's exit for FMI, offline, through the real decoder and a tilt
    /// store: Storm height reads each time's five files once and keeps them
    /// as one volume, and every frame is `products::assemble` of the five
    /// fixture files; Rain mass then reads no file at all; the lowest scan
    /// reads its one live file and makes the rest from the stored sets.
    /// Every key serves its angle's fixture file, whose scans start 00:00,
    /// so 00:00 counts as catalogued once any is read: 8 times of 9.
    #[test]
    fn per_angle_files_are_read_as_one_volume() {
        use std::collections::HashSet;
        let dir = std::env::temp_dir().join(format!("omastorm-ord-sets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Arc::new(crate::tilts::Store::open(dir.clone(), u64::MAX).unwrap());
        let keys = fikor_keys(utc(15, 0, 0), utc(15, 0, 40));
        fn clock() -> i64 {
            utc(15, 0, 46)
        }
        const TIMES: usize = 8;
        let station = korppoo();
        let expected = |want: Want| -> Vec<u8> {
            let tilts: Vec<crate::products::Tilt> = ["05", "07", "15", "30", "50"]
                .iter()
                .flat_map(|a| {
                    crate::products::scans_of(std::io::Cursor::new(fikor_file(&format!(
                        "x@y@{a}@z"
                    ))))
                    .unwrap()
                })
                .collect();
            let Scan::Product(sweep, ..) = crate::products::assemble(want, tilts).unwrap() else {
                panic!()
            };
            sweep.rays.iter().flat_map(|r| r.codes.clone()).collect()
        };
        let provenance = |e: &Event| match e {
            Event::Sweep { provenance, .. } | Event::Backfill { provenance, .. } => {
                provenance.clone()
            }
            _ => String::new(),
        };
        for (i, want) in [Want::EchoTop(station.alt_m), Want::Vil]
            .into_iter()
            .enumerate()
        {
            let (events, served) = run_station(
                station.clone(),
                (want, decode, Some(store.clone()), fikor_file),
                keys.clone(),
                None,
                clock,
                Vec::new(),
                |events| events.len() >= 2 * TIMES,
                Duration::from_millis(300),
            );
            let label = want.variant();
            assert_eq!(
                events.len(),
                2 * TIMES,
                "{label}: a product and its lowest scan each"
            );
            let codes = expected(want);
            let mut products = 0;
            for event in &events {
                let (Event::Sweep { sweep, .. } | Event::Backfill { sweep, .. }) = event else {
                    panic!("{label}: {}", describe(event))
                };
                if let Scan::Product(sweep, w, _) = sweep {
                    assert_eq!(*w, want);
                    let got: Vec<u8> = sweep.rays.iter().flat_map(|r| r.codes.clone()).collect();
                    assert!(got == codes, "{label}: the assembled volume's codes");
                    // Fetched: the set's five files named; from the store
                    // (the second run's backfill): the set's source.
                    let named = if i == 0 {
                        format!("(5 files) [{label}]")
                    } else {
                        format!("[{label}]")
                    };
                    assert!(provenance(event).contains(&named), "{}", provenance(event));
                    products += 1;
                }
            }
            assert_eq!(products, TIMES, "{label}");
            let gets: Vec<&String> = served.iter().filter(|s| s.starts_with("get")).collect();
            let files: HashSet<&str> = gets.iter().map(|g| g.split(' ').nth(1).unwrap()).collect();
            if i == 0 {
                assert_eq!(files.len(), 5 * TIMES, "every angle of every time, once");
            } else {
                assert!(gets.is_empty(), "{label}: {gets:?}");
                // Every product from the stored sets (their lowest scans
                // ride along under S26's own provenance).
                let stored = events
                    .iter()
                    .filter(|e| {
                        matches!(
                            e,
                            Event::Sweep {
                                sweep: Scan::Product(..),
                                ..
                            } | Event::Backfill {
                                sweep: Scan::Product(..),
                                ..
                            }
                        ) && provenance(e).contains(crate::tilts::FROM_STORE)
                    })
                    .count();
                assert_eq!(stored, TIMES, "{label}");
            }
        }
        let volumes = store.volumes("fikor").unwrap();
        assert_eq!(volumes.len(), TIMES);
        assert!(volumes.iter().all(|(_, source)| is_set_source(source)));
        let one = store.volume("fikor", volumes[0].0).unwrap().unwrap();
        assert_eq!(one.angles.map(|a| a.len()), Some(5));
        assert_eq!(one.tilts.len(), 5);
        // The lowest scan (review #1): every time from its stored set, no
        // file read at all, and the sets stay whole.
        let (events, served) = run_station(
            station,
            (Want::Lowest, decode, Some(store.clone()), fikor_file),
            keys,
            None,
            clock,
            Vec::new(),
            |events| events.len() >= TIMES,
            Duration::from_millis(300),
        );
        assert_eq!(
            events.len(),
            TIMES,
            "{:?}",
            events.iter().map(describe).collect::<Vec<_>>()
        );
        let gets: Vec<&String> = served.iter().filter(|s| s.starts_with("get")).collect();
        assert!(gets.is_empty(), "{gets:?}");
        let stored = events
            .iter()
            .filter(|e| provenance(e).contains(crate::tilts::FROM_STORE))
            .count();
        assert_eq!(stored, TIMES);
        let volumes = store.volumes("fikor").unwrap();
        assert_eq!(volumes.len(), TIMES);
        for (t, source) in &volumes {
            assert!(is_set_source(source), "{source}");
            assert_eq!(store.volume("fikor", *t).unwrap().unwrap().tilts.len(), 5);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Review #7: a time whose set misses a file for good (00:05 without its
    /// 5.0 degree scan) is read as its four files and named so.
    #[test]
    fn a_partial_set_is_named() {
        let keys: Vec<String> = fikor_keys(utc(15, 0, 0), utc(15, 0, 10))
            .into_iter()
            .filter(|k| !k.contains("T0005@5.0@"))
            .collect();
        fn clock() -> i64 {
            utc(15, 0, 16)
        }
        let (events, _) = run_station(
            korppoo(),
            (Want::Vil, decode, None, fikor_file),
            keys,
            None,
            clock,
            Vec::new(),
            |events| events.len() >= 4,
            Duration::from_millis(300),
        );
        let named: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Event::Sweep { provenance, .. } | Event::Backfill { provenance, .. } => {
                    Some(provenance.clone())
                }
                _ => None,
            })
            .filter(|p| p.contains("[vil]:"))
            .collect();
        assert!(
            named
                .iter()
                .any(|p| p.contains("fikor@20260915T0005 (4 of 5 files) [vil]")),
            "{named:?}"
        );
        assert!(
            named
                .iter()
                .any(|p| p.contains("fikor@20260915T0010 (5 files) [vil]")),
            "{named:?}"
        );
    }

    #[test]
    fn the_poller_reads_the_newest_dbzh_then_backfills_sixty_frames() {
        let backfills = |events: &[Event]| {
            events
                .iter()
                .filter(|e| matches!(e, Event::Backfill { .. }))
                .count()
        };
        let (events, served) = run(
            hurum(utc(14, 0, 0), utc(14, 9, 40)),
            None,
            recorded,
            Vec::new(),
            |events| backfills(events) >= 59,
            Duration::from_millis(300),
        );
        let described: Vec<String> = events.iter().map(describe).collect();
        assert_eq!(described[0], "sweep 09:39:06", "{described:?}");
        assert_eq!(backfills(&events), 59, "{described:?}");
        assert_eq!(described[1], "backfill 09:34:06");
        assert_eq!(described.last().unwrap(), "backfill 04:44:06");
        assert_eq!(
            described.len(),
            60,
            "nothing more once caught up: {described:?}"
        );
        // The first listing reaches back 62 cadences, in two pages of the
        // stand-in's 100 keys; later ones start two cadences before the
        // live file and fit one page.
        let lists: Vec<&String> = served.iter().filter(|s| s.starts_with("list")).collect();
        let first =
            "list 2026/09/14/NO/nohur/PVOL/ after=2026/09/14/NO/nohur/PVOL/nohur@20260914T0436";
        assert_eq!(lists[0], first);
        assert_eq!(*lists[1], format!("{first} token=100"));
        assert!(lists.len() > 3, "{lists:?}");
        assert!(
            lists[2..]
                .iter()
                .all(|l| l.ends_with("nohur@20260914T0930")),
            "{lists:?}"
        );
        // Only DBZH files were read, each once, in a few ranges.
        let gets: Vec<&String> = served.iter().filter(|s| s.starts_with("get")).collect();
        assert!(gets.iter().all(|g| g.contains("@DBZH.h5")), "{gets:?}");
        let files: std::collections::HashSet<&str> =
            gets.iter().map(|g| g.split(' ').nth(1).unwrap()).collect();
        assert_eq!(files.len(), 60);
        assert!(gets.len() <= 60 * 3, "{} range requests", gets.len());
    }

    /// A product backfills its own depth, and every read that decoded the
    /// lowest scan on the way sends it too, as a second event of the same
    /// kind (S26), from the same file: no request more.
    #[test]
    fn a_product_read_sends_its_lowest_scan_too() {
        let depth = PRODUCT_BACKFILL;
        let (events, served) = run_as(
            (Want::ColMax, stub_product),
            hurum(utc(14, 0, 0), utc(14, 9, 40)),
            None,
            recorded,
            Vec::new(),
            |events| events.len() >= 2 * depth,
            Duration::from_millis(300),
        );
        assert_eq!(events.len(), 2 * depth, "caught up, nothing more");
        for (i, pair) in events.chunks(2).enumerate() {
            let (product, lowest) = match pair {
                [
                    Event::Sweep { sweep: p, .. },
                    Event::Sweep {
                        sweep: l,
                        provenance,
                        ..
                    },
                ] if i == 0 => (p, (l, provenance)),
                [
                    Event::Backfill { sweep: p, .. },
                    Event::Backfill {
                        sweep: l,
                        provenance,
                        ..
                    },
                ] if i > 0 => (p, (l, provenance)),
                other => panic!(
                    "pair {i}: {:?}",
                    other.iter().map(describe).collect::<Vec<_>>()
                ),
            };
            assert!(matches!(product, Scan::Product(_, Want::ColMax, None)));
            assert!(matches!(lowest.0, Scan::Polar(_)));
            assert_eq!(lowest.0.start_ms(), product.start_ms());
            let free =
                format!("[cmax], its lowest scan: 0 range requests, 0 of {FILE_BYTES} bytes");
            assert!(lowest.1.ends_with(&free), "{}", lowest.1);
        }
        let files: std::collections::HashSet<&str> = served
            .iter()
            .filter(|s| s.starts_with("get"))
            .map(|g| g.split(' ').nth(1).unwrap())
            .collect();
        assert_eq!(files.len(), depth, "one read per frame pair");
    }

    #[test]
    fn a_catalogued_newest_file_is_current_and_not_fetched() {
        let cached: Vec<i64> = (0..60)
            .map(|i| utc(14, 9, 40) - 54_000 - i * CADENCE_MS)
            .collect();
        let (events, served) = run(
            hurum(utc(14, 0, 0), utc(14, 9, 40)),
            None,
            recorded,
            cached,
            |events| !events.is_empty(),
            Duration::from_millis(300),
        );
        let described: Vec<String> = events.iter().map(describe).collect();
        assert_eq!(described, ["current"]);
        assert!(!served.iter().any(|s| s.starts_with("get")), "{served:?}");
    }

    #[test]
    fn the_listing_spans_midnight() {
        let (events, served) = run(
            hurum(utc(14, 22, 0), utc(15, 0, 0)),
            None,
            after_midnight,
            Vec::new(),
            |events| events.len() >= 5,
            Duration::from_millis(100),
        );
        assert_eq!(describe(&events[0]), "sweep 23:59:06");
        assert_eq!(describe(&events[1]), "backfill 23:54:06");
        assert!(served.contains(
            &"list 2026/09/14/NO/nohur/PVOL/ after=2026/09/14/NO/nohur/PVOL/nohur@20260914T1856".into()
        ));
        assert!(
            served.contains(&"list 2026/09/15/NO/nohur/PVOL/ after=-".into()),
            "{served:?}"
        );
    }

    #[test]
    fn a_radar_gone_quiet_is_silent_and_one_with_nothing_too() {
        // Hurum's last files are from 09:00, 46 minutes ago.
        let (events, _) = run(
            hurum(utc(14, 8, 0), utc(14, 9, 0)),
            None,
            recorded,
            Vec::new(),
            |events| events.iter().any(|e| matches!(e, Event::Silent { .. })),
            Duration::from_millis(100),
        );
        let silent: Vec<String> = events
            .iter()
            .filter(|e| matches!(e, Event::Silent { .. }))
            .map(describe)
            .collect();
        assert_eq!(
            silent,
            ["silent nohur has published nothing since 2026-09-14 09:00Z"]
        );
        // The last file still shows.
        assert_eq!(describe(&events[0]), "sweep 08:59:06");
        let (events, served) = run(
            Vec::new(),
            None,
            recorded,
            Vec::new(),
            |events| !events.is_empty(),
            Duration::from_millis(200),
        );
        let described: Vec<String> = events.iter().map(describe).collect();
        assert_eq!(
            described,
            ["silent ORD's cache holds no nohur file from the last 5 hours"]
        );
        assert!(!served.iter().any(|s| s.starts_with("get")));
    }

    #[test]
    fn a_failing_cache_reports_offline_on_the_second_failure() {
        let (events, _) = run(
            hurum(utc(14, 0, 0), utc(14, 9, 40)),
            Some(503),
            recorded,
            Vec::new(),
            |events| !events.is_empty(),
            Duration::ZERO,
        );
        assert_eq!(describe(&events[0]), "offline ORD cache: HTTP 503");
    }

    /// A file on disk answering ranged requests, as the cache does.
    struct Local(Vec<u8>);
    impl RangeSource for Local {
        fn get(&mut self, offset: u64, len: u64) -> io::Result<(Vec<u8>, Option<u64>)> {
            let total = self.0.len() as u64;
            let end = (offset + len).min(total);
            Ok((self.0[offset as usize..end as usize].to_vec(), Some(total)))
        }
    }

    /// Requests, bytes and file length for decoding `bytes` under `plan`,
    /// and a digest of the sweep, so plans can be compared on real files.
    fn cost(bytes: &[u8], plan: RangePlan) -> (u32, u64, u64, u64) {
        cost_of(bytes, plan, Want::Lowest)
    }

    /// `cost` for `want`.
    fn cost_of(bytes: &[u8], plan: RangePlan, want: Want) -> (u32, u64, u64, u64) {
        let reader =
            RangeReader::open_planned(Box::new(Local(bytes.to_vec())), plan, Duration::ZERO)
                .unwrap();
        let traffic = reader.traffic();
        let sweep = match decode(reader, want, None).unwrap() {
            Scan::Polar(sweep) | Scan::Product(sweep, ..) => sweep,
            Scan::Grid(_) | Scan::Mosaic(_) => panic!("a radar decodes to a polar sweep"),
        };
        let mut digest = 0xcbf2_9ce4_8422_2325u64;
        for ray in &sweep.rays {
            for &code in &ray.codes {
                digest = (digest ^ u64::from(code)).wrapping_mul(0x100_0000_01b3);
            }
        }
        (traffic.requests(), traffic.bytes(), traffic.total(), digest)
    }

    /// A file the listing sizes at most `WHOLE_UP_TO` comes in one request
    /// and decodes to the same sweep as under the block plan.
    #[test]
    fn small_files_are_read_whole_and_decode_the_same() {
        let lowest = Want::Lowest;
        assert_eq!(plan_for(None, lowest), PLAN);
        assert_eq!(plan_for(Some(0), lowest), PLAN);
        assert_eq!(plan_for(Some(875_975), lowest), PLAN, "DMI's smallest seen");
        assert_eq!(
            plan_for(Some(575_334), lowest).prefetch,
            581_632,
            "MET Norway's largest seen"
        );
        for name in [
            "ord_nohur_202609140930.h5",
            "ord_fikor_202609140940.h5",
            "ord_dksin_202609140940.h5",
        ] {
            let path = format!("{}/../data/raw/{name}", env!("CARGO_MANIFEST_DIR"));
            let bytes = std::fs::read(path).unwrap();
            let len = bytes.len() as u64;
            let blocks = cost(&bytes, PLAN);
            let whole = cost(&bytes, plan_for(Some(len), lowest));
            assert_eq!(whole.3, blocks.3, "{name}: the same sweep");
            assert_eq!(
                (whole.0, whole.1),
                (1, len),
                "{name}: one request, all of it"
            );
        }
        let xml = "<ListBucketResult>\
            <Contents><Key>2026/09/14/NO/nohur/PVOL/nohur@20260914T0930@0.5_1.0@DBZH.h5</Key><Size>427967</Size></Contents>\
            <Contents><Key>2026/09/14/NO/nohur/PVOL/readme.txt</Key><Size>5</Size></Contents>\
            <IsTruncated>false</IsTruncated></ListBucketResult>";
        let (keys, _) = parse_listing(xml).unwrap();
        let listed = with_sizes(keys, xml);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].size, Some(427_967));
        let (keys, _) = parse_listing("<ListBucketResult><Contents><Key>a/nohur@20260914T0930@0.5@DBZH.h5</Key></Contents></ListBucketResult>").unwrap();
        assert_eq!(
            with_sizes(keys, "")[0].size,
            None,
            "no <Size>: the block plan"
        );
    }

    /// Any product but the lowest scan reads a file up to
    /// `PRODUCT_WHOLE_UP_TO` whole, in one request (S26), and makes the same
    /// product as under the block plan; the lowest scan keeps its plan.
    #[test]
    fn a_product_reads_the_file_whole_up_to_two_mib() {
        for want in [Want::ColMax, Want::Cappi(1000.0, 1000), Want::Angle(4.8)] {
            let plan = |size| plan_for(Some(size), want);
            assert_eq!(plan(801_413).prefetch, 802_816, "norsa's 12-angle file");
            assert_eq!(plan(1_293_200).prefetch, 1_294_336, "DMI's largest seen");
            assert_eq!(plan(PRODUCT_WHOLE_UP_TO + 1), PLAN);
            assert_eq!(plan_for(None, want), PLAN);
        }
        assert_eq!(plan_for(Some(801_413), Want::Lowest), PLAN);
        for name in [
            "ord_nohur_202609140930_tilts.h5",
            "ord_dksin_202609140940_tilts.h5",
        ] {
            let path = format!("{}/../data/raw/{name}", env!("CARGO_MANIFEST_DIR"));
            let bytes = std::fs::read(path).unwrap();
            let len = bytes.len() as u64;
            for want in [Want::ColMax, Want::Cappi(2000.0, 2000)] {
                let blocks = cost_of(&bytes, PLAN, want);
                let whole = cost_of(&bytes, plan_for(Some(len), want), want);
                assert_eq!(whole.3, blocks.3, "{name}: the same product");
                assert_eq!((whole.0, whole.1), (1, len), "{name}: one request");
                assert!(blocks.0 > 1, "{name}: {} requests in blocks", blocks.0);
            }
        }
    }

    /// S18's tuning table over whole cache files in `OMASTORM_ORD_SAMPLES`:
    /// `cargo test ord_plan_table -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn ord_plan_table() {
        let dir = std::env::var("OMASTORM_ORD_SAMPLES").expect("OMASTORM_ORD_SAMPLES");
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "h5"))
            .collect();
        files.sort();
        const K: u64 = 1024;
        for path in files {
            let bytes = std::fs::read(&path).unwrap();
            let (_, _, _, want) = cost(&bytes, PLAN);
            println!("{} ({} B)", path.display(), bytes.len());
            let (requests, fetched, _, digest) =
                cost(&bytes, plan_for(Some(bytes.len() as u64), Want::Lowest));
            assert_eq!(digest, want);
            println!("  chosen (plan_for): {requests} requests, {fetched} B");
            for prefetch in [16 * K, 64 * K, 256 * K, 512 * K, 1024 * K, 2048 * K] {
                // The reader keeps whole blocks, so a prefetch is a whole
                // number of them.
                for block in [4 * K, 8 * K, 16 * K, 32 * K, 64 * K] {
                    if prefetch % block != 0 {
                        continue;
                    }
                    let plan = RangePlan {
                        prefetch,
                        block,
                        max_requests: 96,
                        max_bytes: 4 * 1024 * K,
                    };
                    let (requests, fetched, total, digest) = cost(&bytes, plan);
                    assert_eq!(digest, want, "the same sweep under every plan");
                    println!(
                        "  prefetch {:>4} KiB block {:>3} KiB: {requests:>2} requests, {fetched:>8} B ({:.0}%)",
                        prefetch / K,
                        block / K,
                        100.0 * fetched as f64 / total as f64
                    );
                }
            }
        }
    }
}
