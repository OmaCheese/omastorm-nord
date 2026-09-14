//! Request accounting per provider (S18's soak). Every HTTP request the
//! engine sends is counted by where it went (`Kind::of`), and `report`
//! writes one compact line per provider that saw traffic to `engine.log`
//! every `PERIOD`, on the wall clock's quarter hours:
//!
//! `2026-09-14T12:15:00Z Net ord: requests=312 bytes=4102345 http429=0 http5xx=0 http4xx=0 failed=0 secs=900`
//!
//! `bytes` is what the answers declared (`Content-Length`), `http4xx` the
//! other 4xx answers (SMHI's 404 probes are normal), `failed` requests that
//! got no answer. `scripts/soak-report.sh` sums the lines per hour. An idle
//! engine writes nothing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Between report lines, unless `OMASTORM_NET_LOG` names other seconds.
pub const PERIOD: Duration = Duration::from_secs(15 * 60);
const PERIOD_ENV: &str = "OMASTORM_NET_LOG";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Smhi,
    Ord,
    Opera,
    Tiles,
    Other,
}

const KINDS: [Kind; 5] = [Kind::Smhi, Kind::Ord, Kind::Opera, Kind::Tiles, Kind::Other];

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Smhi => "smhi",
            Kind::Ord => "ord",
            Kind::Opera => "opera",
            Kind::Tiles => "tiles",
            Kind::Other => "other",
        }
    }

    /// The provider a URL belongs to: SMHI's open data, the ORD cache
    /// (OPERA's composites live in the same bucket under `OPERA/`), the
    /// vector tiles, or anything else.
    pub fn of(url: &str) -> Kind {
        let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
        let host = rest.split(['/', '?']).next().unwrap_or_default();
        let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
        if host.ends_with("smhi.se") {
            Kind::Smhi
        } else if host.ends_with("cloudferro.com") {
            if rest.contains("OPERA") {
                Kind::Opera
            } else {
                Kind::Ord
            }
        } else if host.ends_with("openfreemap.org") {
            Kind::Tiles
        } else {
            Kind::Other
        }
    }
}

struct Counter {
    requests: AtomicU64,
    bytes: AtomicU64,
    http429: AtomicU64,
    http5xx: AtomicU64,
    http4xx: AtomicU64,
    failed: AtomicU64,
}

impl Counter {
    const fn new() -> Self {
        Counter {
            requests: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            http429: AtomicU64::new(0),
            http5xx: AtomicU64::new(0),
            http4xx: AtomicU64::new(0),
            failed: AtomicU64::new(0),
        }
    }
}

static COUNTERS: [Counter; KINDS.len()] = [const { Counter::new() }; KINDS.len()];

fn counter(kind: Kind) -> &'static Counter {
    &COUNTERS[kind as usize]
}

/// A request to `url` answered with `status`, declaring `bytes` of body.
pub fn record(url: &str, status: u16, bytes: u64) {
    let c = counter(Kind::of(url));
    c.requests.fetch_add(1, Ordering::Relaxed);
    c.bytes.fetch_add(bytes, Ordering::Relaxed);
    let class = match status {
        429 => &c.http429,
        500..=599 => &c.http5xx,
        400..=499 => &c.http4xx,
        _ => return,
    };
    class.fetch_add(1, Ordering::Relaxed);
}

/// `record` for a reqwest answer.
pub fn answered(url: &str, response: &reqwest::Response) {
    record(
        url,
        response.status().as_u16(),
        response.content_length().unwrap_or(0),
    );
}

/// A request to `url` that got no answer.
pub fn failed(url: &str) {
    let c = counter(Kind::of(url));
    c.requests.fetch_add(1, Ordering::Relaxed);
    c.failed.fetch_add(1, Ordering::Relaxed);
}

/// What a provider has seen since the last `take`.
#[derive(Debug, Default, PartialEq)]
pub struct Totals {
    pub requests: u64,
    pub bytes: u64,
    pub http429: u64,
    pub http5xx: u64,
    pub http4xx: u64,
    pub failed: u64,
}

/// The provider's totals since the last call, and zero them.
pub fn take(kind: Kind) -> Totals {
    let c = counter(kind);
    let swap = |a: &AtomicU64| a.swap(0, Ordering::Relaxed);
    Totals {
        requests: swap(&c.requests),
        bytes: swap(&c.bytes),
        http429: swap(&c.http429),
        http5xx: swap(&c.http5xx),
        http4xx: swap(&c.http4xx),
        failed: swap(&c.failed),
    }
}

/// One report line (without its timestamp).
pub fn line(kind: Kind, t: &Totals, secs: u64) -> String {
    format!(
        "Net {}: requests={} bytes={} http429={} http5xx={} http4xx={} failed={} secs={secs}",
        kind.name(),
        t.requests,
        t.bytes,
        t.http429,
        t.http5xx,
        t.http4xx,
        t.failed
    )
}

/// The report period: `OMASTORM_NET_LOG` seconds when set and positive.
pub fn period() -> Duration {
    std::env::var(PERIOD_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map_or(PERIOD, Duration::from_secs)
}

/// Milliseconds from `now_ms` to the next multiple of `period`.
fn until_boundary(now_ms: u64, period: Duration) -> u64 {
    let p = period.as_millis().max(1) as u64;
    p - now_ms % p
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Write the report lines forever, at each multiple of `period` since the
/// epoch; the first line covers the time since the engine started.
pub async fn report(period: Duration) {
    let mut since = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(until_boundary(now_ms(), period))).await;
        let secs = since.elapsed().as_secs_f64().round() as u64;
        since = Instant::now();
        let at = chrono::DateTime::from_timestamp_millis(now_ms() as i64)
            .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_default();
        for kind in KINDS {
            let totals = take(kind);
            if totals != Totals::default() {
                eprintln!("{at} {}", line(kind, &totals, secs));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_counted_under_their_provider() {
        let cases = [
            (
                "https://opendata-download-radar.smhi.se/api/version/latest/area/vara/product/qcvol",
                Kind::Smhi,
            ),
            (
                "https://s3.waw3-1.cloudferro.com/openradar-24h/2026/09/14/NO/nohur/PVOL/nohur@20260914T0930@0.5@DBZH.h5",
                Kind::Ord,
            ),
            (
                "https://s3.waw3-1.cloudferro.com/openradar-24h/?list-type=2&prefix=2026%2F09%2F14%2FDK%2Fdksin%2FPVOL%2F",
                Kind::Ord,
            ),
            (
                "https://s3.waw3-1.cloudferro.com/openradar-24h/2026/09/14/OPERA/COMP/OPERA@20260914T0810@0@DBZH.h5",
                Kind::Opera,
            ),
            (
                "https://s3.waw3-1.cloudferro.com/openradar-24h/?list-type=2&prefix=2026%2F09%2F14%2FOPERA%2FCOMP%2F",
                Kind::Opera,
            ),
            (
                "https://tiles.openfreemap.org/planet/20260910/5/17/9.pbf",
                Kind::Tiles,
            ),
            ("http://127.0.0.1:4711/?list-type=2", Kind::Other),
        ];
        for (url, kind) in cases {
            assert_eq!(Kind::of(url), kind, "{url}");
        }
    }

    #[test]
    fn totals_count_statuses_and_reset_when_taken() {
        // `Other` is this test's own; the pollers' tests talk to 127.0.0.1
        // too, so only compare what this test adds.
        let url = "http://netstats.test/x";
        let _ = take(Kind::Other);
        record(url, 206, 16_384);
        record(url, 200, 100);
        record(url, 304, 0);
        record(url, 404, 0);
        record(url, 429, 0);
        record(url, 503, 0);
        failed(url);
        let t = take(Kind::Other);
        assert!(t.requests >= 7 && t.bytes >= 16_484, "{t:?}");
        assert!(t.http429 >= 1 && t.http5xx >= 1 && t.http4xx >= 1 && t.failed >= 1);
        let line = line(
            Kind::Ord,
            &Totals {
                requests: 312,
                bytes: 4_102_345,
                ..Totals::default()
            },
            900,
        );
        assert_eq!(
            line,
            "Net ord: requests=312 bytes=4102345 http429=0 http5xx=0 http4xx=0 failed=0 secs=900"
        );
    }

    #[test]
    fn reports_land_on_the_periods_boundaries() {
        let quarter = Duration::from_secs(900);
        assert_eq!(until_boundary(0, quarter), 900_000);
        assert_eq!(until_boundary(1_000, quarter), 899_000);
        assert_eq!(until_boundary(899_999, quarter), 1);
    }
}
