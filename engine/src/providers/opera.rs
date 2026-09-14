//! The OPERA provider (stream S16, DEC-14): EUMETNET OPERA's CIRRUS
//! maximum-reflectivity composite of Europe, cut to the Nordic box and
//! listed as the grid station `nordic`, next to SMHI's `sweden`.
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::runtime::Handle;
use tokio::sync::mpsc::Sender;
use tokio::task::spawn_blocking;
use tokio::time::sleep;

/// The station id: a region word (DEC-12).
pub const ID: &str = "nordic";
/// The credit OPERA's licence asks for.
pub const ATTRIBUTION: &str = "EUMETNET OPERA, CC BY 4.0";
/// The public 24-hour cache of EUMETNET's Open Radar Data.
pub const BASE: &str = "https://s3.waw3-1.cloudferro.com/openradar-24h";
/// What the texture covers: the embedded geography's box (`engine/build.rs`,
/// 3–33° E, 53–71.5° N), 1670 × 2297 texels of 2 km (DEC-14).
pub const NORDIC: LonLatBox = LonLatBox {
    west: 3.0,
    east: 33.0,
    south: 53.0,
    north: 71.5,
};
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

/// The Nordic composite as `hello` lists it: the middle of the box, with no
/// antenna.
pub fn station() -> Station {
    Station {
        id: ID.to_owned(),
        name: "Nordic".to_owned(),
        state: String::new(),
        lat: 62.25,
        lon: 18.0,
        alt_m: 0.0,
        kind: SiteKind::Grid,
        country: String::new(),
        provider: ProviderId::Opera,
        range_km: 0.0,
        attribution: ATTRIBUTION.to_owned(),
        aliases: Vec::new(),
        source: String::new(),
    }
}

/// The decoder: the Nordic crop of a composite.
pub fn decode(reader: RangeReader) -> Result<Grid, String> {
    composite::decode_box(reader, Some(NORDIC)).map_err(|e| e.to_string())
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

enum Failure {
    /// The cache failed to answer: back off.
    Net(Fail),
    /// The answer or the bytes were unusable: give up on that file only.
    Decode(String),
}

/// Read and decode one composite with ranged requests under `PLAN`,
/// holding the engine's single fetch permit. Returns the grid and its
/// provenance.
async fn fetch(http: &Http, volume: &Volume) -> Result<(Scan, String), Failure> {
    let permit = FETCHER
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| Failure::Decode(e.to_string()))?;
    let failure = Arc::new(Mutex::new(None));
    let source = HttpRanges {
        http: http.clone(),
        url: volume.url.clone(),
        runtime: Handle::current(),
        failure: failure.clone(),
    };
    let joined = spawn_blocking(move || {
        let _permit = permit;
        let reader = RangeReader::open_planned(Box::new(source), PLAN, LENGTH_PAUSE)
            .map_err(|e| e.to_string())?;
        let traffic = reader.traffic();
        decode(reader).map(|grid| (grid, traffic))
    })
    .await;
    match joined {
        Ok(Ok((grid, traffic))) => {
            let provenance = format!(
                "EUMETNET OPERA {}: {} range requests, {} of {} bytes",
                volume.key,
                traffic.requests(),
                traffic.bytes(),
                traffic.total()
            );
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
}

impl Config {
    pub fn opera() -> Self {
        Config {
            base: BASE.to_owned(),
            poll: POLL,
            max_back_off: MAX_BACK_OFF,
            backfill_delay: BACKFILL_DELAY,
            backfill_pace: BACKFILL_PACE,
            now_ms,
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
    poll_with(Config::opera(), station.id, events, cached).await;
}

async fn send(events: &Sender<Event>, event: Event) -> bool {
    events.send(event).await.is_ok()
}

/// Poll `site` until the task is aborted or the event channel closes.
/// `cached` holds the start times of the frames already catalogued, so
/// neither the live path nor the backfill fetches them again.
pub async fn poll_with(cfg: Config, site: String, events: Sender<Event>, cached: Vec<i64>) {
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
            match list_between(&http, &cfg.base, listing_since(newest_ms, now), now).await {
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
                            match fetch(&http, &latest).await {
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
            return;
        }
    };
    let targets = backfill_targets(listed, live_ms, &known, BACKFILL);
    let wanted = targets.len();
    let mut fetched = 0;
    for volume in targets {
        match fetch(&http, &volume).await {
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
    fn the_spec_and_the_station() {
        assert_eq!(SPEC.staleness.stale, Duration::from_secs(15 * 60));
        assert_eq!(SPEC.staleness.unavailable, Duration::from_secs(30 * 60));
        assert_eq!(SPEC.backfill, 24);
        let s = station();
        assert_eq!((s.id.as_str(), s.kind, s.provider), (ID, SiteKind::Grid, ProviderId::Opera));
        assert_eq!((s.country.as_str(), s.range_km), ("", 0.0));
        assert_eq!(s.attribution, "EUMETNET OPERA, CC BY 4.0");
        // The station sits inside its own box.
        assert!((NORDIC.south..NORDIC.north).contains(&s.lat));
        assert!((NORDIC.west..NORDIC.east).contains(&s.lon));
    }

    #[test]
    fn the_nordic_box_is_the_goldens() {
        let golden: serde_json::Value =
            serde_json::from_str(include_str!("../../../golden/nordic-20260914/grid.json")).unwrap();
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
