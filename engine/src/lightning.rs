//! Lightning strikes (S44, docs/protocol.md "Lightning"): the located
//! flashes of NORDLIS, the Nordic lightning network, from FMI's open WFS.
//!
//! One poller, independent of the radar's: while at least one client has
//! lightning on (`set_layers` `lightning`), it asks FMI for the strikes
//! since its last fetch at most once every `POLL`, keeps the last `KEEP` in
//! memory, and writes them as one packed file under `tex/` (16 bytes a
//! strike, never a JSON array on the socket). Each client with lightning
//! on gets a `lightning` line naming the file. With no client on, nothing
//! is fetched (the M1 rule).
//!
//! `OMASTORM_LIGHTNING_REPLAY` replays a stored storm instead (an SMHI
//! archive day file or an FMI answer), shifted to end now, so captures and
//! checks work on a quiet day.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, mpsc::Sender};

/// The shortest time between two fetches.
pub const POLL: Duration = Duration::from_secs(60);
/// How long a strike stays drawn, fading (the human's call: 30 minutes).
pub const TRAIL: Duration = Duration::from_secs(30 * 60);
/// How far back strikes are kept: the live loop's backfill (60 volumes of
/// 5 minutes).
pub const KEEP: Duration = Duration::from_secs(5 * 60 * 60);
/// Each fetch asks again for this much before the last one's end: strikes
/// reach FMI's feed late.
const OVERLAP: Duration = Duration::from_secs(15 * 60);
/// The longest wait after failures (1, 2, 4 … minutes).
const BACKOFF_MAX: Duration = Duration::from_secs(15 * 60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// A busy 5-hour window is about 1.5 MB of GML.
const MAX_BODY: usize = 32 << 20;
const USER_AGENT: &str = concat!(
    "omastorm-se/",
    env!("CARGO_PKG_VERSION"),
    " (fork of https://omastorm.com; lightning)"
);
pub const SOURCE: &str = "FMI NORDLIS";
pub const ATTRIBUTION: &str = "FMI NORDLIS, CC BY 4.0";
/// The Nordic box, as FMI's `bbox` (lon, lat, lon, lat).
const BBOX: &str = "3,53,33,71.5";
const FMI_URL: &str = "https://opendata.fmi.fi/wfs";
const QUERY: &str = "fmi::observations::lightning::multipointcoverage";
/// Development and tests: FMI's scheme and host go here instead.
const BASE_ENV: &str = "OMASTORM_LIGHTNING_BASE";
const REPLAY_ENV: &str = "OMASTORM_LIGHTNING_REPLAY";
const REPLAY_END_ENV: &str = "OMASTORM_LIGHTNING_REPLAY_END";
/// The strikes file's magic and sizes.
pub const MAGIC: &[u8; 4] = b"OSL1";
pub const HEADER: usize = 16;
pub const RECORD: usize = 16;

/// One located pulse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strike {
    /// Milliseconds since the epoch.
    pub ms: i64,
    pub lat: f32,
    pub lon: f32,
    /// Peak current, kA; the sign is the polarity.
    pub ka: i16,
    pub cloud: bool,
    pub multiplicity: u8,
}

impl Strike {
    /// Two rows are one strike when time, place and current agree (FMI's
    /// answer repeats rows).
    fn key(&self) -> (i64, u32, u32, i16, bool) {
        (
            self.ms,
            self.lat.to_bits(),
            self.lon.to_bits(),
            self.ka,
            self.cloud,
        )
    }
    fn valid(&self) -> bool {
        (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lon)
            && self.ms > 0
    }
}

fn iso(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_iso(text: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|t| t.timestamp_millis())
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn clamp_ka(value: f64) -> i16 {
    value.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16
}

/// The text between `<tag…>` and `</tag>`, the first such element.
fn element<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = text.find(&format!("<{tag}"))?;
    let start = open + text[open..].find('>')? + 1;
    let end = start + text[start..].find(&format!("</{tag}>"))?;
    Some(&text[start..end])
}

/// Parse FMI's `multipointcoverage` answer: `positions` rows `lat lon
/// unixtime` and tuples in the order the `swe:field`s name them. An answer
/// with no member is a quiet window (no strikes), not an error.
pub fn parse_fmi(body: &[u8]) -> Result<Vec<Strike>, String> {
    let text = std::str::from_utf8(body).map_err(|_| "the answer is not UTF-8".to_owned())?;
    if !text.contains("FeatureCollection") {
        let reason = element(text, "ExceptionText")
            .map(str::trim)
            .unwrap_or("not a WFS feature collection");
        return Err(format!("FMI: {reason}"));
    }
    let Some(positions) = element(text, "gmlcov:positions") else {
        return Ok(Vec::new());
    };
    let tuples = element(text, "gml:doubleOrNilReasonTupleList").unwrap_or("");
    // The field order, e.g. multiplicity peak_current cloud_indicator ellipse_major.
    let mut fields = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("<swe:field") {
        rest = &rest[at + 10..];
        let name = rest
            .find("name=\"")
            .map(|n| &rest[n + 6..])
            .and_then(|r| r.split('"').next())
            .unwrap_or("");
        fields.push(name.to_owned());
    }
    let column = |name: &str| fields.iter().position(|f| f == name);
    let (Some(ka_at), Some(cloud_at)) = (column("peak_current"), column("cloud_indicator")) else {
        return Err("FMI: no peak_current or cloud_indicator field".into());
    };
    let multiplicity_at = column("multiplicity");
    let position_rows: Vec<&str> = positions
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let tuple_rows: Vec<&str> = tuples
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if position_rows.len() != tuple_rows.len() {
        return Err(format!(
            "FMI: {} positions but {} tuples",
            position_rows.len(),
            tuple_rows.len()
        ));
    }
    let number = |s: Option<&str>| s.and_then(|s| s.parse::<f64>().ok()).filter(|v| v.is_finite());
    let mut out = Vec::with_capacity(position_rows.len());
    for (p, t) in position_rows.iter().zip(&tuple_rows) {
        let p: Vec<&str> = p.split_whitespace().collect();
        let t: Vec<&str> = t.split_whitespace().collect();
        let (Some(lat), Some(lon), Some(unix)) = (
            number(p.first().copied()),
            number(p.get(1).copied()),
            number(p.get(2).copied()),
        ) else {
            continue;
        };
        let Some(ka) = number(t.get(ka_at).copied()) else {
            continue;
        };
        let cloud = number(t.get(cloud_at).copied()).unwrap_or(0.0) >= 0.5;
        let multiplicity = multiplicity_at
            .and_then(|at| number(t.get(at).copied()))
            .unwrap_or(0.0)
            .clamp(0.0, 255.0) as u8;
        let strike = Strike {
            ms: (unix * 1000.0).round() as i64,
            lat: lat as f32,
            lon: lon as f32,
            ka: clamp_ka(ka),
            cloud,
            multiplicity,
        };
        if strike.valid() {
            out.push(strike);
        }
    }
    Ok(dedupe(out))
}

/// One SMHI archive object (UALF fields; the rest are ignored).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ualf {
    year: i32,
    month: u32,
    day: u32,
    hours: u32,
    minutes: u32,
    seconds: u32,
    #[serde(default)]
    nanoseconds: u32,
    lat: f64,
    lon: f64,
    peak_current: f64,
    #[serde(default)]
    multiplicity: f64,
    cloud_indicator: u8,
}

#[derive(Deserialize)]
struct SmhiDay {
    values: Vec<Ualf>,
}

/// Parse an SMHI lightning archive day file (`data.json`).
pub fn parse_smhi(body: &[u8]) -> Result<Vec<Strike>, String> {
    let day: SmhiDay = serde_json::from_slice(body).map_err(|e| format!("SMHI: {e}"))?;
    let mut out = Vec::with_capacity(day.values.len());
    for v in day.values {
        let Some(date) = chrono::NaiveDate::from_ymd_opt(v.year, v.month, v.day) else {
            continue;
        };
        let Some(time) = date.and_hms_nano_opt(v.hours, v.minutes, v.seconds, v.nanoseconds)
        else {
            continue;
        };
        let strike = Strike {
            ms: time.and_utc().timestamp_millis(),
            lat: v.lat as f32,
            lon: v.lon as f32,
            ka: clamp_ka(v.peak_current),
            cloud: v.cloud_indicator == 1,
            multiplicity: v.multiplicity.clamp(0.0, 255.0) as u8,
        };
        if strike.valid() {
            out.push(strike);
        }
    }
    Ok(dedupe(out))
}

/// Sorted oldest first, repeats dropped.
fn dedupe(mut strikes: Vec<Strike>) -> Vec<Strike> {
    strikes.sort_by_key(Strike::key);
    strikes.dedup_by_key(|s| s.key());
    strikes
}

/// The strikes file: `OSL1`, count, base time, then 16 bytes a strike
/// (docs/protocol.md "Lightning"). `strikes` must be sorted.
pub fn pack(strikes: &[Strike]) -> Vec<u8> {
    let base = strikes.first().map_or(0, |s| s.ms);
    let mut out = Vec::with_capacity(HEADER + RECORD * strikes.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(strikes.len() as u32).to_le_bytes());
    out.extend_from_slice(&(base as f64).to_le_bytes());
    for s in strikes {
        let dt = (s.ms - base).clamp(0, u32::MAX as i64) as u32;
        out.extend_from_slice(&dt.to_le_bytes());
        out.extend_from_slice(&s.lat.to_le_bytes());
        out.extend_from_slice(&s.lon.to_le_bytes());
        out.extend_from_slice(&s.ka.to_le_bytes());
        out.push(u8::from(s.cloud));
        out.push(s.multiplicity);
    }
    out
}

/// The strikes of a file `pack` wrote (tests).
#[cfg(test)]
pub fn unpack(bytes: &[u8]) -> Result<Vec<Strike>, String> {
    if bytes.len() < HEADER || &bytes[..4] != MAGIC {
        return Err("not a strikes file".into());
    }
    let count = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let base = f64::from_le_bytes(bytes[8..16].try_into().unwrap()) as i64;
    if bytes.len() != HEADER + RECORD * count {
        return Err("strikes file length does not match its count".into());
    }
    Ok(bytes[HEADER..]
        .as_chunks::<RECORD>()
        .0
        .iter()
        .map(|r| Strike {
            ms: base + u32::from_le_bytes(r[0..4].try_into().unwrap()) as i64,
            lat: f32::from_le_bytes(r[4..8].try_into().unwrap()),
            lon: f32::from_le_bytes(r[8..12].try_into().unwrap()),
            ka: i16::from_le_bytes(r[12..14].try_into().unwrap()),
            cloud: r[14] & 1 == 1,
            multiplicity: r[15],
        })
        .collect())
}

/// FNV-1a, 64 bits: the file's name follows its contents.
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The FMI request for strikes from `start` to `end` (ms).
pub fn url(start: i64, end: i64) -> String {
    format!(
        "{FMI_URL}?service=WFS&version=2.0.0&request=getFeature&storedquery_id={QUERY}&bbox={BBOX}&starttime={}&endtime={}",
        iso(start),
        iso(end)
    )
}

/// `url` with its scheme and host replaced by `base`.
fn rebase(url: &str, base: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.find('/').map_or("", |at| &rest[at..]);
    format!("{}{path}", base.trim_end_matches('/'))
}

/// The strikes held, newest `KEEP`, sorted and without repeats.
#[derive(Default)]
pub struct Ring {
    strikes: Vec<Strike>,
}

impl Ring {
    /// Add `new` and drop what is older than `KEEP` before `to`; the number
    /// of strikes not held before.
    pub fn merge(&mut self, new: Vec<Strike>, to: i64) -> usize {
        let known: std::collections::HashSet<_> = self.strikes.iter().map(Strike::key).collect();
        let added: Vec<Strike> = new
            .into_iter()
            .filter(|s| !known.contains(&s.key()))
            .collect();
        let count = added.len();
        self.strikes.extend(added);
        self.strikes = dedupe(std::mem::take(&mut self.strikes));
        let cut = to - KEEP.as_millis() as i64;
        self.strikes.retain(|s| s.ms >= cut);
        count
    }
    pub fn strikes(&self) -> &[Strike] {
        &self.strikes
    }
}

/// What the `lightning` line says, beside the file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub failed: bool,
    pub note: String,
    pub from: i64,
    pub to: i64,
    pub path: String,
    pub count: usize,
    pub cloud_to_ground: usize,
    pub newest: i64,
    pub replay: Option<Replay>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Replay {
    pub source: String,
    pub attribution: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Line<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    v: u32,
    status: &'static str,
    path: &'a str,
    from: String,
    to: String,
    count: usize,
    cloud_to_ground: usize,
    newest: String,
    source: &'a str,
    attribution: &'a str,
    trail_s: u64,
    replay: bool,
    #[serde(skip_serializing_if = "str::is_empty")]
    note: &'a str,
}

/// The `lightning` line for `status`.
pub fn line(status: &Status) -> String {
    let (source, attribution) = match &status.replay {
        Some(r) => (r.source.as_str(), r.attribution.as_str()),
        None => (SOURCE, ATTRIBUTION),
    };
    let message = Line {
        kind: "lightning",
        v: crate::protocol::VERSION,
        status: if status.failed { "failed" } else { "ok" },
        path: &status.path,
        from: if status.from > 0 { iso(status.from) } else { String::new() },
        to: if status.to > 0 { iso(status.to) } else { String::new() },
        count: status.count,
        cloud_to_ground: status.cloud_to_ground,
        newest: if status.newest > 0 { iso(status.newest) } else { String::new() },
        source,
        attribution,
        trail_s: TRAIL.as_secs(),
        replay: status.replay.is_some(),
        note: &status.note,
    };
    let mut text = serde_json::to_string(&message).expect("lightning serializes");
    text.push('\n');
    text
}

/// `hello.lightning`.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub source: &'static str,
    pub attribution: &'static str,
    pub trail_s: u64,
    pub keep_s: u64,
    pub poll_s: u64,
}

pub fn info() -> Info {
    Info {
        source: SOURCE,
        attribution: ATTRIBUTION,
        trail_s: TRAIL.as_secs(),
        keep_s: KEEP.as_secs(),
        poll_s: POLL.as_secs(),
    }
}

/// The engine's one hub: `set` from `set_layers`, `run` publishes.
pub static HUB: LazyLock<Hub> = LazyLock::new(Hub::default);

/// Which clients have lightning on, and the newest line and file.
#[derive(Default)]
pub struct Hub {
    inner: Mutex<HubInner>,
    wake: Notify,
}

#[derive(Default)]
struct HubInner {
    clients: HashMap<u64, Sender<String>>,
    last: Option<String>,
    path: Option<String>,
}

impl Hub {
    /// `set_layers` from `client`. Turning it on gets the newest line at
    /// once, and wakes the poller.
    pub fn set(&self, client: u64, on: bool, reply: &Sender<String>) {
        let mut inner = self.inner.lock().unwrap();
        if !on {
            inner.clients.remove(&client);
            return;
        }
        if inner.clients.insert(client, reply.clone()).is_none() {
            if let Some(last) = &inner.last {
                let _ = reply.try_send(last.clone());
            }
            self.wake.notify_one();
        }
    }
    pub fn remove(&self, client: u64) {
        self.inner.lock().unwrap().clients.remove(&client);
    }
    pub fn wanted(&self) -> bool {
        !self.inner.lock().unwrap().clients.is_empty()
    }
    /// The newest strikes file (`tex/…`), which texture cleanup keeps.
    pub fn path(&self) -> Option<String> {
        self.inner.lock().unwrap().path.clone()
    }
    fn publish(&self, line: String, path: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.path = (!path.is_empty()).then(|| path.to_owned());
        if inner.last.as_deref() == Some(line.as_str()) {
            return;
        }
        for tx in inner.clients.values() {
            let _ = tx.try_send(line.clone());
        }
        inner.last = Some(line);
    }
}

fn stamp() -> String {
    iso(now_ms())
}

/// Read a file, gunzipping a `.gz`.
fn read_maybe_gz(path: &Path) -> io::Result<Vec<u8>> {
    let bytes = fs::read(path)?;
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&bytes[..]).read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(bytes)
    }
}

/// A stored storm: its strikes, and whose they are.
pub fn load_replay(path: &Path) -> Result<(Vec<Strike>, Replay), String> {
    let body = read_maybe_gz(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let head = String::from_utf8_lossy(&body[..body.len().min(512)]).to_string();
    if head.trim_start().starts_with('<') {
        let strikes = parse_fmi(&body)?;
        Ok((
            strikes,
            Replay {
                source: "FMI NORDLIS (replay)".into(),
                attribution: ATTRIBUTION.into(),
            },
        ))
    } else {
        let strikes = parse_smhi(&body)?;
        Ok((
            strikes,
            Replay {
                source: "SMHI lightning archive (replay)".into(),
                attribution: "SMHI, CC BY 4.0".into(),
            },
        ))
    }
}

/// `strikes` moved so the newest falls at `end`.
pub fn shift(strikes: &mut [Strike], end: i64) {
    let Some(newest) = strikes.iter().map(|s| s.ms).max() else {
        return;
    };
    let by = end - newest;
    for s in strikes {
        s.ms += by;
    }
}

/// Write the ring as `tex/lightning-<hash>.bin` under `dir` (unless it is
/// there already) and fill `status` from it.
fn publish_file(dir: &Path, ring: &Ring, status: &mut Status) -> io::Result<()> {
    let strikes = ring.strikes();
    status.count = strikes.len();
    status.cloud_to_ground = strikes.iter().filter(|s| !s.cloud).count();
    status.newest = strikes.last().map_or(0, |s| s.ms);
    if strikes.is_empty() {
        status.path.clear();
        return Ok(());
    }
    let bytes = pack(strikes);
    let name = format!("tex/lightning-{:016x}.bin", hash(&bytes));
    let path = dir.join(&name);
    if !path.exists() {
        let tmp = dir.join(format!("{name}.tmp"));
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, &path)?;
    }
    status.path = name;
    Ok(())
}

/// Fetch one window from FMI.
async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let url = match std::env::var(BASE_ENV) {
        Ok(base) if !base.is_empty() => rebase(url, &base),
        _ => url.to_owned(),
    };
    let response = match client.get(&url).send().await {
        Ok(response) => response,
        Err(e) => {
            crate::netstats::failed(&url);
            return Err(format!("{e}"));
        }
    };
    crate::netstats::answered(&url, &response);
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
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

/// The wait after `failures` failures in a row.
pub fn backoff(failures: u32) -> Duration {
    if failures == 0 {
        return POLL;
    }
    let factor = 1u32 << (failures - 1).min(8);
    (POLL * factor).min(BACKOFF_MAX)
}

/// The window to ask for at `now`, after a fetch that ended at `last_to`.
pub fn window(last_to: Option<i64>, now: i64) -> (i64, i64) {
    let keep = KEEP.as_millis() as i64;
    let start = match last_to {
        Some(to) => (to - OVERLAP.as_millis() as i64).max(now - keep),
        None => now - keep,
    };
    (start, now)
}

/// The poller's task (`dir` is the runtime directory, with `tex/`).
pub async fn run(dir: PathBuf) {
    let hub = &*HUB;
    match std::env::var(REPLAY_ENV) {
        Ok(path) if !path.is_empty() => return replay(dir, PathBuf::from(path)).await,
        _ => {}
    }
    let client = match reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(FETCH_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            eprintln!("{} Lightning: disabled: {e}", stamp());
            return;
        }
    };
    let mut ring = Ring::default();
    let mut status = Status::default();
    let mut last_to: Option<i64> = None;
    // The start of the first window fetched: what the ring covers.
    let mut covered_from: Option<i64> = None;
    let mut last_try: Option<tokio::time::Instant> = None;
    let mut failures = 0u32;
    loop {
        if !hub.wanted() {
            hub.wake.notified().await;
            continue;
        }
        let wait = backoff(failures);
        if let Some(at) = last_try {
            let due = at + wait;
            if tokio::time::Instant::now() < due {
                // Sleep until due; a newly switched-on client gets the held
                // line from `set` meanwhile.
                let _ = tokio::time::timeout_at(due, hub.wake.notified()).await;
                continue;
            }
        }
        last_try = Some(tokio::time::Instant::now());
        let now = now_ms();
        let (start, end) = window(last_to, now);
        let url = url(start, end);
        match fetch(&client, &url).await.and_then(|body| {
            let strikes = parse_fmi(&body)?;
            Ok((body.len(), strikes))
        }) {
            Ok((bytes, strikes)) => {
                failures = 0;
                let got = strikes.len();
                let new = ring.merge(strikes, end);
                eprintln!(
                    "{} Lightning fmi: requests=1 bytes={bytes} strikes={got} new={new} held={}",
                    stamp(),
                    ring.strikes().len()
                );
                last_to = Some(end);
                status.failed = false;
                status.note.clear();
                status.to = end;
                // A gap while nobody watched is re-fetched (`window`), so
                // the ring covers from its first window on.
                let first = *covered_from.get_or_insert(start);
                status.from = first.max(end - KEEP.as_millis() as i64);
                if let Err(e) = publish_file(&dir, &ring, &mut status) {
                    eprintln!("{} Lightning: writing the strikes file: {e}", stamp());
                }
            }
            Err(e) => {
                failures += 1;
                eprintln!(
                    "{} Lightning fmi: requests=1 failed: {e} (next try in {} s)",
                    stamp(),
                    backoff(failures).as_secs()
                );
                status.failed = true;
                status.note = e;
            }
        }
        hub.publish(line(&status), &status.path);
    }
}

/// Replay a stored storm: no request, ever.
async fn replay(dir: PathBuf, path: PathBuf) {
    let hub = &*HUB;
    let mut status = Status::default();
    match load_replay(&path) {
        Ok((mut strikes, who)) => {
            let end = std::env::var(REPLAY_END_ENV)
                .ok()
                .and_then(|t| parse_iso(&t))
                .unwrap_or_else(now_ms);
            shift(&mut strikes, end);
            let mut ring = Ring::default();
            ring.merge(strikes, end);
            eprintln!(
                "{} Lightning replay: {} strikes from {} ending {}",
                stamp(),
                ring.strikes().len(),
                path.display(),
                iso(end)
            );
            status.to = end;
            status.from = ring.strikes().first().map_or(end, |s| s.ms);
            status.replay = Some(who);
            if let Err(e) = publish_file(&dir, &ring, &mut status) {
                eprintln!("{} Lightning: writing the strikes file: {e}", stamp());
            }
        }
        Err(e) => {
            eprintln!("{} Lightning replay: {e}", stamp());
            status.failed = true;
            status.note = e;
            status.replay = Some(Replay {
                source: "replay".into(),
                attribution: String::new(),
            });
        }
    }
    let text = line(&status);
    loop {
        if hub.wanted() {
            hub.publish(text.clone(), &status.path);
        } else {
            // Keep the file referenced while nobody watches.
            hub.inner.lock().unwrap().path =
                (!status.path.is_empty()).then(|| status.path.clone());
        }
        hub.wake.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/../data/fixtures/lightning/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        read_maybe_gz(Path::new(&path)).unwrap()
    }

    #[test]
    fn fmi_answer_parses_and_repeats_drop() {
        let strikes = parse_fmi(&fixture("fmi_lightning_20260923T0534Z.xml.gz")).unwrap();
        assert_eq!(strikes.len(), 190, "194 rows, 4 of them repeats");
        assert_eq!(strikes.iter().filter(|s| !s.cloud).count(), 124);
        let first = strikes[0];
        assert_eq!(first.ms, 1_790_057_972_000);
        assert!((first.lat - 56.0904).abs() < 1e-4 || (first.lat - 56.1362).abs() < 1e-4);
        assert!(strikes.windows(2).all(|w| w[0].ms <= w[1].ms));
        assert!(strikes.iter().all(|s| (53.0..=71.5).contains(&s.lat)));
        // Polarity kept: a negative ground flash of -126 kA is in there.
        assert!(strikes.iter().any(|s| s.ka == -126 && !s.cloud));
        assert!(strikes.iter().any(|s| s.multiplicity == 3));
    }

    #[test]
    fn a_quiet_window_is_no_strikes_not_an_error() {
        let strikes = parse_fmi(&fixture("fmi_lightning_empty_20260923T0535Z.xml.gz")).unwrap();
        assert!(strikes.is_empty());
    }

    #[test]
    fn an_exception_or_garbage_is_an_error() {
        let report = br#"<?xml version="1.0"?><ExceptionReport><Exception><ExceptionText>Invalid time interval!</ExceptionText></Exception></ExceptionReport>"#;
        assert_eq!(parse_fmi(report).unwrap_err(), "FMI: Invalid time interval!");
        assert!(parse_fmi(b"<html>busy</html>").is_err());
        let broken = String::from_utf8(fixture("fmi_lightning_20260923T0534Z.xml.gz"))
            .unwrap()
            .replacen("1 6 1 0.8\n", "", 1);
        assert!(parse_fmi(broken.as_bytes()).unwrap_err().contains("positions but"));
    }

    #[test]
    fn the_storm_hour_parses() {
        let t = std::time::Instant::now();
        let strikes = parse_smhi(&fixture("smhi_lightning_20260705T11Z.json.gz")).unwrap();
        eprintln!("storm hour parsed in {:?}", t.elapsed());
        assert_eq!(strikes.len(), 7759);
        assert_eq!(strikes.iter().filter(|s| !s.cloud).count(), 1473);
        let from = parse_iso("2026-07-05T11:00:00Z").unwrap();
        let to = parse_iso("2026-07-05T12:00:00Z").unwrap();
        assert!(strikes.iter().all(|s| s.ms >= from && s.ms < to));
        assert!(strikes.iter().all(|s| (56.0..69.0).contains(&s.lat)));
        // The file is 16 bytes a strike and reads back exactly.
        let bytes = pack(&strikes);
        assert_eq!(bytes.len(), HEADER + RECORD * 7759);
        assert_eq!(unpack(&bytes).unwrap(), strikes);
    }

    #[test]
    fn the_ring_keeps_five_hours_without_repeats() {
        let s = |ms: i64, lat: f32| Strike {
            ms,
            lat,
            lon: 15.0,
            ka: -10,
            cloud: false,
            multiplicity: 1,
        };
        let h = 3_600_000;
        let now = 100 * h;
        let mut ring = Ring::default();
        assert_eq!(ring.merge(vec![s(now - 6 * h, 60.0), s(now - h, 60.0)], now), 2);
        assert_eq!(ring.strikes().len(), 1, "older than five hours: dropped");
        // The overlap brings the same strike again: not new.
        assert_eq!(ring.merge(vec![s(now - h, 60.0), s(now, 61.0)], now), 1);
        assert_eq!(ring.strikes().len(), 2);
    }

    #[test]
    fn windows_overlap_and_backoff_grows() {
        let now = 50 * 3_600_000;
        assert_eq!(window(None, now), (now - KEEP.as_millis() as i64, now));
        assert_eq!(window(Some(now - 60_000), now), (now - 60_000 - 15 * 60_000, now));
        // A long pause asks for no more than the ring keeps.
        assert_eq!(window(Some(now - 24 * 3_600_000), now).0, now - KEEP.as_millis() as i64);
        assert_eq!(backoff(0), POLL);
        assert_eq!(backoff(1), POLL);
        assert_eq!(backoff(2), POLL * 2);
        assert_eq!(backoff(3), POLL * 4);
        assert_eq!(backoff(10), BACKOFF_MAX);
        assert!(url(0, 60_000).contains("starttime=1970-01-01T00:00:00Z&endtime=1970-01-01T00:01:00Z"));
        assert!(url(0, 1).contains("bbox=3,53,33,71.5"));
    }

    #[test]
    fn replay_ends_at_the_given_time_and_the_line_says_so() {
        let path = format!(
            "{}/../data/fixtures/lightning/smhi_lightning_20260705T11Z.json.gz",
            env!("CARGO_MANIFEST_DIR")
        );
        let (mut strikes, who) = load_replay(Path::new(&path)).unwrap();
        let end = parse_iso("2026-09-23T06:00:00Z").unwrap();
        shift(&mut strikes, end);
        assert_eq!(strikes.iter().map(|s| s.ms).max(), Some(end));
        let dir = std::env::temp_dir().join(format!("oms-lightning-{}", std::process::id()));
        fs::create_dir_all(dir.join("tex")).unwrap();
        let mut ring = Ring::default();
        ring.merge(strikes, end);
        let mut status = Status {
            to: end,
            replay: Some(who),
            ..Status::default()
        };
        publish_file(&dir, &ring, &mut status).unwrap();
        assert!(status.path.starts_with("tex/lightning-") && status.path.ends_with(".bin"));
        assert!(crate::protocol::is_texture_path(&status.path));
        let bytes = fs::read(dir.join(&status.path)).unwrap();
        assert_eq!(unpack(&bytes).unwrap().len(), 7759);
        let value: serde_json::Value = serde_json::from_str(&line(&status)).unwrap();
        assert_eq!(value["type"], "lightning");
        assert_eq!(value["v"], 2);
        assert_eq!(value["status"], "ok");
        assert_eq!(value["count"], 7759);
        assert_eq!(value["cloudToGround"], 1473);
        assert_eq!(value["replay"], true);
        assert_eq!(value["attribution"], "SMHI, CC BY 4.0");
        assert_eq!(value["trailS"], 1800);
        assert_eq!(value["newest"], "2026-09-23T06:00:00Z");
        assert!(value.get("note").is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_hub_sends_only_to_clients_with_lightning_on() {
        let hub = Hub::default();
        let (tx1, mut rx1) = tokio::sync::mpsc::channel(4);
        let (tx2, mut rx2) = tokio::sync::mpsc::channel(4);
        hub.set(1, true, &tx1);
        hub.set(2, false, &tx2);
        assert!(hub.wanted());
        hub.publish("a\n".into(), "tex/lightning-1.bin");
        hub.publish("a\n".into(), "tex/lightning-1.bin");
        assert_eq!(rx1.try_recv().unwrap(), "a\n");
        assert!(rx1.try_recv().is_err(), "an unchanged line is not re-sent");
        assert!(rx2.try_recv().is_err());
        assert_eq!(hub.path().as_deref(), Some("tex/lightning-1.bin"));
        hub.set(2, true, &tx2);
        assert_eq!(rx2.try_recv().unwrap(), "a\n");
        hub.set(1, false, &tx1);
        hub.remove(2);
        assert!(!hub.wanted());
    }
}
