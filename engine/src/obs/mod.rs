//! Weather station observations for the weather layers (S42,
//! docs/protocol.md "Weather layers"): air temperature and wind from the
//! national networks' latest reports, normalised to one station list.
//!
//! One module per provider parses that provider's bulk answer (`smhi`,
//! `fmi`, `dmi`; `frost` is Norway, skipped without a client ID). This
//! module decides when a provider is due, fetches and caches its bodies
//! under `$XDG_CACHE_HOME/omastorm-se/obs/`, drops stale stations, and
//! hands the `obs` line to the clients that have a layer on (`Hub`).
//!
//! The M1 rule: nothing is fetched while no client has a layer on, and each
//! provider at most once per `MIN_AGE`.

pub mod dmi;
pub mod fmi;
pub mod frost;
pub mod smhi;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, mpsc::Sender};

/// A provider's bulk files are fetched at most this often.
pub const MIN_AGE: Duration = Duration::from_secs(10 * 60);
/// A station whose newest observation is older than this is left out.
pub const STALE_MS: i64 = 90 * 60 * 1000;
/// How often the list is rebuilt while a layer is on (stale stations drop
/// out, due providers are fetched).
const TICK: Duration = Duration::from_secs(30);
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// The biggest body accepted (FMI's answer is about 300 kB, DMI's station
/// list about 460 kB).
const MAX_BODY: usize = 8 << 20;
const USER_AGENT: &str = concat!(
    "omastorm-se/",
    env!("CARGO_PKG_VERSION"),
    " (fork of https://omastorm.com; weather layers)"
);

/// One station's latest observation, as it goes on the wire.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Station {
    /// `<provider>:<station id>`.
    pub id: String,
    pub name: String,
    pub provider: &'static str,
    pub lat: f64,
    pub lon: f64,
    /// The observation's time, ISO 8601 UTC.
    pub time: String,
    #[serde(rename = "tempC")]
    pub temp_c: Option<f64>,
    pub wind_ms: Option<f64>,
    /// Where the wind blows from, degrees clockwise from north.
    pub wind_dir_deg: Option<f64>,
    pub gust_ms: Option<f64>,
    /// `time` in milliseconds since the epoch, for the stale rule.
    #[serde(skip)]
    pub ms: i64,
}

impl Station {
    fn has_value(&self) -> bool {
        self.temp_c.is_some()
            || self.wind_ms.is_some()
            || self.wind_dir_deg.is_some()
            || self.gust_ms.is_some()
    }
}

/// ISO 8601 UTC, seconds precision, for `ms` since the epoch.
pub fn iso(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Milliseconds since the epoch of an RFC 3339 time.
pub fn parse_iso(text: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|t| t.timestamp_millis())
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// A finite number, or `None` (NaN, infinities, absurd values).
fn finite(value: f64) -> Option<f64> {
    (value.is_finite() && value.abs() < 1000.0).then_some(value)
}

/// The stations worth sending: an observation within `STALE_MS` of `now`
/// (and not from the future by more than 10 minutes), at least one value,
/// a position on the globe.
pub fn fresh(stations: Vec<Station>, now: i64) -> Vec<Station> {
    stations
        .into_iter()
        .filter(|s| {
            s.has_value()
                && now - s.ms <= STALE_MS
                && s.ms - now <= 10 * 60 * 1000
                && (-90.0..=90.0).contains(&s.lat)
                && (-180.0..=180.0).contains(&s.lon)
        })
        .collect()
}

/// A provider's new list, with the stations of the previous one it no
/// longer names kept while they are fresh: SMHI's latest-hour file drops a
/// station until its report for the new hour is in, which would otherwise
/// empty a third of Sweden for the first half of every hour.
pub fn merge(previous: Vec<Station>, next: Vec<Station>, now: i64) -> Vec<Station> {
    let named: std::collections::HashSet<String> = next.iter().map(|s| s.id.clone()).collect();
    let mut out = next;
    out.extend(
        fresh(previous, now)
            .into_iter()
            .filter(|s| !named.contains(&s.id)),
    );
    out
}

/// One file a provider is read from, and how old its cached copy may get.
pub struct Part {
    /// The cache file's name under `obs/`.
    pub name: &'static str,
    pub url: String,
    pub max_age: Duration,
    /// Sent with the Frost client ID as basic auth (never in the URL).
    pub auth: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Id {
    Smhi,
    Fmi,
    Dmi,
    Frost,
}

pub const PROVIDERS: [Id; 4] = [Id::Smhi, Id::Fmi, Id::Dmi, Id::Frost];

impl Id {
    pub fn key(self) -> &'static str {
        match self {
            Id::Smhi => "smhi",
            Id::Fmi => "fmi",
            Id::Dmi => "dmi",
            Id::Frost => "frost",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Id::Smhi => "SMHI",
            Id::Fmi => "FMI",
            Id::Dmi => "DMI",
            Id::Frost => "MET Norway",
        }
    }
    pub fn attribution(self) -> &'static str {
        match self {
            Id::Smhi => "SMHI, CC BY 4.0",
            Id::Fmi => "FMI, CC BY 4.0",
            Id::Dmi => "DMI, CC BY 4.0",
            Id::Frost => "MET Norway, CC BY 4.0",
        }
    }
    /// The files to read at `now` (Frost's depend on its cached station
    /// list in `dir`), or why the provider is skipped.
    fn parts(self, now: i64, dir: &Path) -> Result<Vec<Part>, String> {
        match self {
            Id::Smhi => Ok(smhi::parts()),
            Id::Fmi => Ok(fmi::parts(now)),
            Id::Dmi => Ok(dmi::parts()),
            Id::Frost => frost::parts(dir),
        }
    }
    /// Whether a part cached `age` ago at `fetched` needs fetching at `now`.
    fn due(self, part: &Part, fetched: Option<i64>, now: i64) -> bool {
        let Some(fetched) = fetched else { return true };
        if now - fetched < part.max_age.as_millis() as i64 {
            return false;
        }
        match self {
            Id::Smhi => smhi::due(fetched, now),
            _ => true,
        }
    }
    fn parse(self, bodies: &[Vec<u8>], now: i64) -> Result<Vec<Station>, String> {
        match self {
            Id::Smhi => smhi::parse(bodies),
            Id::Fmi => fmi::parse(&bodies[0]),
            Id::Dmi => dmi::parse(&bodies[0], &bodies[1]),
            Id::Frost => frost::parse(bodies),
        }
        .map(|stations| fresh(stations, now))
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// `ok`, `failed`, or `skipped`.
    pub status: &'static str,
    pub stations: usize,
    pub attribution: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Serialize)]
struct ObsLine<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    v: u32,
    time: String,
    source: &'static str,
    stations: Vec<&'a Station>,
    providers: &'a [ProviderInfo],
    attribution: String,
}

/// The `obs` line for these providers' stations at `now` (stale ones
/// dropped), or `None` when nothing has been read yet.
pub fn line(lists: &[(ProviderInfo, Vec<Station>)], now: i64) -> String {
    let stations: Vec<&Station> = lists
        .iter()
        .flat_map(|(_, list)| list.iter())
        .filter(|s| now - s.ms <= STALE_MS)
        .collect();
    let mut providers: Vec<ProviderInfo> = Vec::new();
    for (info, list) in lists {
        let mut info = info.clone();
        info.stations = list.iter().filter(|s| now - s.ms <= STALE_MS).count();
        providers.push(info);
    }
    let names: Vec<&str> = providers
        .iter()
        .filter(|p| p.stations > 0)
        .map(|p| p.name)
        .collect();
    let attribution = if names.is_empty() {
        String::new()
    } else {
        format!("{} (CC BY 4.0)", names.join(", "))
    };
    let newest = stations.iter().map(|s| s.ms).max().unwrap_or(0);
    let message = ObsLine {
        kind: "obs",
        v: crate::protocol::VERSION,
        time: if newest > 0 {
            iso(newest)
        } else {
            String::new()
        },
        source: "stations",
        stations,
        providers: &providers,
        attribution,
    };
    let mut text = serde_json::to_string(&message).expect("obs serializes");
    text.push('\n');
    text
}

/// Which layers a client shows.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Layers {
    pub temp: bool,
    pub wind: bool,
}

impl Layers {
    pub fn any(self) -> bool {
        self.temp || self.wind
    }
}

/// The engine's one hub: `receive` sets layers, `run` publishes.
pub static HUB: LazyLock<Hub> = LazyLock::new(Hub::default);

/// The clients' layers and the newest `obs` line. Separate from the
/// engine's shared state: layers are per client.
#[derive(Default)]
pub struct Hub {
    inner: Mutex<HubInner>,
    wake: Notify,
}

#[derive(Default)]
struct HubInner {
    clients: HashMap<u64, (Layers, Sender<String>)>,
    last: Option<String>,
}

impl Hub {
    /// `set_layers` from `client`. A client turning its first layer on gets
    /// the newest list at once, and the fetcher wakes.
    pub fn set(&self, client: u64, layers: Layers, reply: &Sender<String>) {
        let mut inner = self.inner.lock().unwrap();
        let before = inner
            .clients
            .get(&client)
            .map(|(l, _)| *l)
            .unwrap_or_default();
        if layers.any() {
            inner.clients.insert(client, (layers, reply.clone()));
            if !before.any() {
                if let Some(last) = &inner.last {
                    let _ = reply.try_send(last.clone());
                }
                self.wake.notify_one();
            }
        } else {
            inner.clients.remove(&client);
        }
    }
    pub fn remove(&self, client: u64) {
        self.inner.lock().unwrap().clients.remove(&client);
    }
    /// Whether any client has a layer on.
    pub fn wanted(&self) -> bool {
        !self.inner.lock().unwrap().clients.is_empty()
    }
    /// Send `line` to every client with a layer on, when it differs from
    /// the last one sent.
    fn publish(&self, line: String) {
        let mut inner = self.inner.lock().unwrap();
        if inner.last.as_deref() == Some(line.as_str()) {
            return;
        }
        for (_, tx) in inner.clients.values() {
            let _ = tx.try_send(line.clone());
        }
        inner.last = Some(line);
    }
}

/// `$XDG_CACHE_HOME/omastorm-se/obs`.
fn cache_dir() -> io::Result<PathBuf> {
    let dir = crate::osm::cache_root()?.join("obs");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn modified_ms(path: &Path) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

/// Development and tests: every provider's URL goes to this scheme and
/// host instead (the path and query kept), e.g. a local fixture server or
/// a closed port.
const BASE_ENV: &str = "OMASTORM_OBS_BASE";

/// `url` with its scheme and host replaced by `base`.
fn rebase(url: &str, base: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.find('/').map_or("", |at| &rest[at..]);
    format!("{}{path}", base.trim_end_matches('/'))
}

fn stamp() -> String {
    iso(now_ms())
}

/// Reads the providers, fetching what is due.
struct Fetcher {
    client: reqwest::Client,
    dir: PathBuf,
    lists: Vec<(ProviderInfo, Vec<Station>)>,
}

impl Fetcher {
    fn new() -> Result<Fetcher, String> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|e| format!("building the HTTP client: {e}"))?;
        let dir = cache_dir().map_err(|e| format!("the obs cache: {e}"))?;
        let lists = PROVIDERS
            .iter()
            .map(|id| {
                (
                    ProviderInfo {
                        id: id.key(),
                        name: id.name(),
                        status: "ok",
                        stations: 0,
                        attribution: id.attribution(),
                        note: String::new(),
                    },
                    Vec::new(),
                )
            })
            .collect();
        Ok(Fetcher { client, dir, lists })
    }

    async fn get(&self, url: &str, auth: bool) -> Result<Vec<u8>, String> {
        let url = match std::env::var(BASE_ENV) {
            Ok(base) if !base.is_empty() => rebase(url, &base),
            _ => url.to_owned(),
        };
        let mut request = self.client.get(&url);
        if auth {
            let id = frost::client_id().ok_or("no Frost client ID")?;
            request = request.basic_auth(id, Some(""));
        }
        // The error names the URL, which never holds the ID.
        let response = request.send().await.map_err(|e| format!("{e}"))?;
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

    /// Bring provider `index` up to date: fetch the parts that are due (a
    /// failure keeps the cached copy), parse whatever changed.
    async fn update(&mut self, index: usize, now: i64, first: bool) {
        let id = PROVIDERS[index];
        let mut fetched = 0;
        let mut bytes = 0;
        let mut failure = None;
        // Frost's observation parts exist once its station list is cached,
        // so a first fetch of the list is followed by a second round.
        let mut parts = Vec::new();
        for _round in 0..2 {
            let now_parts = match id.parts(now, &self.dir) {
                Ok(parts) => parts,
                Err(note) => {
                    let (info, list) = &mut self.lists[index];
                    if info.status != "skipped" {
                        eprintln!("{} Obs {}: skipped: {note}", stamp(), id.key());
                    }
                    info.status = "skipped";
                    info.note = note;
                    list.clear();
                    return;
                }
            };
            if now_parts.len() == parts.len() {
                break;
            }
            parts = now_parts;
            for part in &parts {
                let path = self.dir.join(part.name);
                if !id.due(part, modified_ms(&path), now) {
                    continue;
                }
                match self.get(&part.url, part.auth).await {
                    Ok(body) => {
                        fetched += 1;
                        bytes += body.len();
                        if let Err(e) = write_atomic(&path, &body) {
                            eprintln!("{} Obs {}: caching {}: {e}", stamp(), id.key(), part.name);
                        }
                    }
                    Err(e) => {
                        fetched += 1;
                        failure = Some(format!("{}: {e}", part.name));
                        // Keep the cached copy; retry after `MIN_AGE`, not
                        // every tick: touch it so its age restarts.
                        if path.exists() {
                            let _ = fs::File::options()
                                .append(true)
                                .open(&path)
                                .and_then(|f| f.set_modified(SystemTime::now()));
                        } else {
                            let _ = write_atomic(&path, b"");
                        }
                    }
                }
            }
        }
        if fetched == 0 && !first {
            return;
        }
        let bodies: Vec<Vec<u8>> = parts
            .iter()
            .map(|p| fs::read(self.dir.join(p.name)).unwrap_or_default())
            .collect();
        let parsed = if bodies.iter().all(|b| !b.is_empty()) {
            id.parse(&bodies, now)
        } else {
            Err("nothing cached".into())
        };
        let (info, list) = &mut self.lists[index];
        match (parsed, failure) {
            (Ok(stations), None) => {
                info.status = "ok";
                info.note.clear();
                if fetched > 0 {
                    eprintln!(
                        "{} Obs {}: requests={fetched} bytes={bytes} stations={}",
                        stamp(),
                        id.key(),
                        stations.len()
                    );
                }
                *list = merge(std::mem::take(list), stations, now);
            }
            (parsed, failure) => {
                let note = failure
                    .or_else(|| parsed.as_ref().err().cloned())
                    .unwrap_or_default();
                eprintln!(
                    "{} Obs {}: requests={fetched} failed: {note}",
                    stamp(),
                    id.key()
                );
                info.status = "failed";
                info.note = note;
                if let Ok(stations) = parsed {
                    *list = stations;
                }
            }
        }
    }
}

/// The fetcher's task: while a client has a layer on, keep every provider
/// fresh and publish the list when it changes; otherwise sleep until one
/// turns a layer on.
pub async fn run() {
    let hub = &*HUB;
    let mut fetcher = match Fetcher::new() {
        Ok(fetcher) => fetcher,
        Err(e) => return eprintln!("{} Obs: disabled: {e}", stamp()),
    };
    let mut first = true;
    loop {
        if !hub.wanted() {
            hub.wake.notified().await;
            continue;
        }
        let now = now_ms();
        for index in 0..PROVIDERS.len() {
            fetcher.update(index, now, first).await;
        }
        first = false;
        hub.publish(line(&fetcher.lists, now_ms()));
        let _ = tokio::time::timeout(TICK, hub.wake.notified()).await;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Read;

    pub(crate) fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/../data/fixtures/obs/{name}", env!("CARGO_MANIFEST_DIR"));
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(fs::File::open(&path).unwrap())
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    fn station(ms: i64, temp: Option<f64>) -> Station {
        Station {
            id: "x:1".into(),
            name: "X".into(),
            provider: "smhi",
            lat: 59.0,
            lon: 18.0,
            time: iso(ms),
            temp_c: temp,
            wind_ms: None,
            wind_dir_deg: None,
            gust_ms: None,
            ms,
        }
    }

    #[test]
    fn a_base_replaces_scheme_and_host() {
        assert_eq!(
            rebase("https://opendata.fmi.fi/wfs?a=b", "http://127.0.0.1:9/"),
            "http://127.0.0.1:9/wfs?a=b"
        );
    }

    #[test]
    fn stale_empty_and_future_stations_are_dropped() {
        let now = parse_iso("2026-09-22T18:40:00Z").unwrap();
        let min = 60 * 1000;
        let kept = fresh(
            vec![
                station(now - 89 * min, Some(1.0)),
                station(now - 91 * min, Some(1.0)),
                station(now - 5 * min, None),
                station(now + 30 * min, Some(1.0)),
            ],
            now,
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].ms, now - 89 * min);
    }

    #[test]
    fn a_station_missing_from_the_new_list_stays_while_fresh() {
        let now = parse_iso("2026-09-22T19:20:00Z").unwrap();
        let min = 60 * 1000;
        let mut a = station(now - 80 * min, Some(1.0));
        a.id = "smhi:a".into();
        let mut b = station(now - 80 * min, Some(2.0));
        b.id = "smhi:b".into();
        let mut b2 = station(now - 20 * min, Some(3.0));
        b2.id = "smhi:b".into();
        let mut old = station(now - 100 * min, Some(4.0));
        old.id = "smhi:old".into();
        let merged = merge(vec![a, b, old], vec![b2], now);
        let ids: Vec<&str> = merged.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["smhi:b", "smhi:a"]);
        assert_eq!(merged[0].temp_c, Some(3.0));
    }

    #[test]
    fn the_line_names_only_providers_with_stations() {
        let now = parse_iso("2026-09-22T18:40:00Z").unwrap();
        let info = |id: Id, status| ProviderInfo {
            id: id.key(),
            name: id.name(),
            status,
            stations: 0,
            attribution: id.attribution(),
            note: String::new(),
        };
        let lists = vec![
            (info(Id::Smhi, "ok"), vec![station(now - 60_000, Some(3.5))]),
            (info(Id::Frost, "skipped"), vec![]),
        ];
        let text = line(&lists, now);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["type"], "obs");
        assert_eq!(value["v"], 2);
        assert_eq!(value["source"], "stations");
        assert_eq!(value["attribution"], "SMHI (CC BY 4.0)");
        assert_eq!(value["time"], "2026-09-22T18:39:00Z");
        let s = &value["stations"][0];
        assert_eq!(s["tempC"], 3.5);
        assert!(s["windMs"].is_null() && s["gustMs"].is_null());
        assert!(s.get("ms").is_none());
        assert_eq!(value["providers"][1]["status"], "skipped");
        assert_eq!(value["providers"][0]["stations"], 1);
        // Stale by the time the line is built: dropped from the list.
        let later = line(&lists, now + 2 * STALE_MS);
        let value: serde_json::Value = serde_json::from_str(&later).unwrap();
        assert_eq!(value["stations"].as_array().unwrap().len(), 0);
        assert_eq!(value["attribution"], "");
    }

    #[test]
    fn the_hub_sends_only_to_clients_with_a_layer_on() {
        let hub = Hub::default();
        let (tx1, mut rx1) = tokio::sync::mpsc::channel(4);
        let (tx2, mut rx2) = tokio::sync::mpsc::channel(4);
        assert!(!hub.wanted());
        hub.set(
            1,
            Layers {
                temp: true,
                wind: false,
            },
            &tx1,
        );
        hub.set(2, Layers::default(), &tx2);
        assert!(hub.wanted());
        hub.publish("a\n".into());
        hub.publish("a\n".into());
        assert_eq!(rx1.try_recv().unwrap(), "a\n");
        assert!(rx1.try_recv().is_err(), "an unchanged line is not re-sent");
        assert!(rx2.try_recv().is_err(), "layers off: no obs");
        // Turning a layer on gets the newest list at once.
        hub.set(
            2,
            Layers {
                temp: false,
                wind: true,
            },
            &tx2,
        );
        assert_eq!(rx2.try_recv().unwrap(), "a\n");
        hub.set(1, Layers::default(), &tx1);
        hub.remove(2);
        assert!(!hub.wanted());
    }
}
