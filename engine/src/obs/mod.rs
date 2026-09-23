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
pub mod grid;
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
/// S47: how soon a line a client's full queue refused is offered again.
pub const RETRY: Duration = Duration::from_secs(1);
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
    /// Whether a part whose good copy was cached at `fetched` (`None`:
    /// none, or one that does not read) needs fetching at `now`, when its
    /// last failed attempt was at `failed`. A failure is retried after
    /// `MIN_AGE`, whatever the part's own age (review M1: a daily list that
    /// failed once must not lock its provider out for a day).
    fn due(self, part: &Part, fetched: Option<i64>, failed: Option<i64>, now: i64) -> bool {
        if failed.is_some_and(|t| now - t < MIN_AGE.as_millis() as i64) {
            return false;
        }
        let Some(fetched) = fetched else { return true };
        if now - fetched < part.max_age.as_millis() as i64 {
            return false;
        }
        match self {
            Id::Smhi => smhi::due(fetched, now),
            _ => true,
        }
    }
    /// Whether a body is worth caching for its part's whole age: a daily
    /// list is checked (a 200 with a bad body is a failure); the others are
    /// checked when the provider parses them.
    fn usable(self, part: &Part, body: &[u8]) -> bool {
        if part.max_age <= MIN_AGE {
            return !body.is_empty();
        }
        match self {
            Id::Dmi => dmi::station_list_ok(body),
            Id::Frost => frost::station_list_ok(body),
            _ => !body.is_empty(),
        }
    }
    /// Whether the provider can be read without this part (DMI's names:
    /// stations are then called "DMI <id>").
    fn optional(self, index: usize) -> bool {
        self == Id::Dmi && index == 1
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
    /// `ok`, `failed`, or `skipped`; S47: `loading` until its first
    /// answer since the engine started or a reset.
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
    /// S47: the layer as a whole, `loading` (a provider has not answered
    /// yet), `ok`, or `error` (no provider could be read).
    status: &'static str,
    /// S47: the providers that have answered, of all, while `loading`.
    percent: u32,
    /// S47: seconds since the newest observation, absent with none.
    #[serde(rename = "ageSeconds", skip_serializing_if = "Option::is_none")]
    age_seconds: Option<i64>,
}

/// S47: the layer's status and percent from its providers' statuses.
pub fn layer_status(providers: &[ProviderInfo]) -> (&'static str, u32) {
    let total = providers.len().max(1) as u32;
    let answered = providers.iter().filter(|p| p.status != "loading").count() as u32;
    if answered < total {
        return ("loading", answered * 100 / total);
    }
    if providers.iter().all(|p| p.status == "failed" || p.status == "skipped") {
        return ("error", 100);
    }
    ("ok", 100)
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
    let (status, percent) = layer_status(&providers);
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
        status,
        percent,
        age_seconds: (newest > 0).then(|| ((now - newest) / 1000).max(0)),
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
    /// S47: a reset aborts the fetch in flight.
    abort: Notify,
}

/// A client with a layer on, and which `obs` line it has had.
struct Watcher {
    layers: Layers,
    tx: Sender<String>,
    /// The `generation` of the last line that reached its queue; behind
    /// the hub's, it has not had the newest.
    delivered: u64,
}

#[derive(Default)]
struct HubInner {
    clients: HashMap<u64, Watcher>,
    last: Option<String>,
    /// Bumped whenever `last` changes (from 1; a watcher at 0 has had none).
    generation: u64,
    /// S47: a reset the fetcher has not applied yet.
    reset: bool,
}

impl HubInner {
    /// S47: hand `last` to every watcher that has not had it. A full queue
    /// (a client busy drawing) keeps it owed rather than losing it: the
    /// stations bug was a line dropped here once and never sent again.
    /// True while one is still owed.
    fn deliver(&mut self) -> bool {
        let Some(last) = &self.last else { return false };
        let mut owed = false;
        for watcher in self.clients.values_mut() {
            if watcher.delivered == self.generation {
                continue;
            }
            match watcher.tx.try_send(last.clone()) {
                Ok(()) => watcher.delivered = self.generation,
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => owed = true,
                // Gone: its connection's cleanup removes it.
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
            }
        }
        owed
    }
}

impl Hub {
    /// `set_layers` from `client`. S47: any `set_layers` with a layer on
    /// is answered with the newest list (a client that lost it may ask
    /// again); the first one on wakes the fetcher.
    pub fn set(&self, client: u64, layers: Layers, reply: &Sender<String>) {
        let mut inner = self.inner.lock().unwrap();
        let before = inner
            .clients
            .get(&client)
            .map(|w| w.layers)
            .unwrap_or_default();
        if layers.any() {
            inner.clients.insert(
                client,
                Watcher {
                    layers,
                    tx: reply.clone(),
                    delivered: 0,
                },
            );
            if inner.deliver() || !before.any() {
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
    /// Send `line` to every client with a layer on that has not had it.
    /// True while a client is still owed it (its queue was full).
    fn publish(&self, line: String) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.last.as_deref() != Some(line.as_str()) {
            inner.last = Some(line);
            inner.generation += 1;
        }
        inner.deliver()
    }
    /// S47 (`reset`): forget the list; the fetcher drops its lists and
    /// cached bodies, aborts a fetch in flight, and starts again.
    pub fn reset(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.last = None;
        inner.generation += 1;
        inner.reset = true;
        drop(inner);
        self.abort.notify_one();
        self.wake.notify_one();
    }
    fn take_reset(&self) -> bool {
        std::mem::take(&mut self.inner.lock().unwrap().reset)
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
    /// When each part (by cache name) last failed, in memory only.
    failed: HashMap<&'static str, i64>,
}

/// Every provider at `status` with `note`, no stations.
fn blank_lists(status: &'static str, note: &str) -> Vec<(ProviderInfo, Vec<Station>)> {
    PROVIDERS
        .iter()
        .map(|id| {
            (
                ProviderInfo {
                    id: id.key(),
                    name: id.name(),
                    status,
                    stations: 0,
                    attribution: id.attribution(),
                    note: note.to_owned(),
                },
                Vec::new(),
            )
        })
        .collect()
}

impl Fetcher {
    fn new() -> Result<Fetcher, String> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|e| format!("building the HTTP client: {e}"))?;
        let dir = cache_dir().map_err(|e| format!("the obs cache: {e}"))?;
        Ok(Fetcher {
            client,
            dir,
            lists: blank_lists("loading", ""),
            failed: HashMap::new(),
        })
    }

    /// S47 (`reset`): drop the lists, the failures and the cached bodies
    /// of the observations (the daily station lists are kept: they are not
    /// observations), so every provider is read again at once.
    fn reset(&mut self) {
        self.lists = blank_lists("loading", "");
        self.failed.clear();
        let now = now_ms();
        for id in PROVIDERS {
            for part in id.parts(now, &self.dir).unwrap_or_default() {
                if part.max_age <= MIN_AGE {
                    let _ = fs::remove_file(self.dir.join(part.name));
                }
            }
        }
    }

    /// Whether a provider has not answered since the start or a reset.
    fn loading(&self) -> bool {
        self.lists.iter().any(|(info, _)| info.status == "loading")
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
                // A cached copy that does not read counts as none.
                let cached = modified_ms(&path)
                    .filter(|_| id.usable(part, &fs::read(&path).unwrap_or_default()));
                if !id.due(part, cached, self.failed.get(part.name).copied(), now) {
                    continue;
                }
                fetched += 1;
                let got = match self.get(&part.url, part.auth).await {
                    Ok(body) if id.usable(part, &body) => Ok(body),
                    Ok(_) => Err("an answer that does not read".to_owned()),
                    Err(e) => Err(e),
                };
                match got {
                    Ok(body) => {
                        bytes += body.len();
                        self.failed.remove(part.name);
                        // S47: the directory may have gone under a running
                        // engine (a cleared cache); every provider then
                        // failed "nothing cached" until a restart.
                        let _ = fs::create_dir_all(&self.dir);
                        if let Err(e) = write_atomic(&path, &body) {
                            eprintln!("{} Obs {}: caching {}: {e}", stamp(), id.key(), part.name);
                        }
                    }
                    Err(e) => {
                        // Keep any cached copy; retry after `MIN_AGE`.
                        failure = Some(format!("{}: {e}", part.name));
                        self.failed.insert(part.name, now);
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
        let parsed = if bodies
            .iter()
            .enumerate()
            .all(|(i, b)| !b.is_empty() || id.optional(i))
        {
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
                // Review N3: what did read joins what is still fresh.
                if let Ok(stations) = parsed {
                    *list = merge(std::mem::take(list), stations, now);
                }
            }
        }
    }
}

/// S47: `work`, or `None` as soon as `abort` is notified (a reset), which
/// drops the work where it stands (the engine has no `select!`).
pub async fn or_abort<F: std::future::Future>(work: F, abort: &Notify) -> Option<F::Output> {
    use std::task::Poll;
    let mut work = std::pin::pin!(work);
    let mut aborted = std::pin::pin!(abort.notified());
    std::future::poll_fn(|cx| {
        if let Poll::Ready(value) = work.as_mut().poll(cx) {
            return Poll::Ready(Some(value));
        }
        if aborted.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

/// The fetcher's task: while a client has a layer on, keep every provider
/// fresh and publish the list when it changes; otherwise sleep until one
/// turns a layer on. S47: while a provider has not answered (the first
/// round, or after a reset) the list goes out after each provider, with
/// `status` `loading` and its `percent`; a line a busy client could not
/// take is offered again every `RETRY` until it is; a reset aborts the
/// round in flight.
pub async fn run() {
    let hub = &*HUB;
    let mut fetcher = match Fetcher::new() {
        Ok(fetcher) => fetcher,
        Err(e) => {
            // Review N2: say so to the clients rather than "loading" forever.
            eprintln!("{} Obs: disabled: {e}", stamp());
            let lists = blank_lists("failed", &e);
            loop {
                if hub.wanted() {
                    hub.publish(line(&lists, now_ms()));
                }
                hub.wake.notified().await;
            }
        }
    };
    let mut first = true;
    loop {
        if hub.take_reset() {
            eprintln!("{} Obs: reset", stamp());
            fetcher.reset();
            first = true;
        }
        if !hub.wanted() {
            hub.wake.notified().await;
            continue;
        }
        let now = now_ms();
        let round = async {
            let progress = fetcher.loading();
            if progress {
                hub.publish(line(&fetcher.lists, now_ms()));
            }
            for index in 0..PROVIDERS.len() {
                fetcher.update(index, now, first).await;
                if progress && index + 1 < PROVIDERS.len() {
                    hub.publish(line(&fetcher.lists, now_ms()));
                }
            }
        };
        if or_abort(round, &hub.abort).await.is_none() {
            continue;
        }
        first = false;
        let owed = hub.publish(line(&fetcher.lists, now_ms()));
        // A client still owed the newest line is offered it again soon (the
        // next round publishes it; nothing is fetched that is not due).
        let _ = tokio::time::timeout(if owed { RETRY } else { TICK }, hub.wake.notified()).await;
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
    fn a_failed_daily_list_is_retried_after_ten_minutes() {
        let now = parse_iso("2026-09-22T12:00:00Z").unwrap();
        let min = 60 * 1000;
        let daily = &dmi::parts()[1];
        assert!(daily.max_age > MIN_AGE);
        // Nothing good cached, failed 5 min ago: wait; 11 min ago: retry.
        assert!(!Id::Dmi.due(daily, None, Some(now - 5 * min), now));
        assert!(Id::Dmi.due(daily, None, Some(now - 11 * min), now));
        // A good copy from an hour ago is kept for the day.
        assert!(!Id::Dmi.due(daily, Some(now - 60 * min), None, now));
        // A 200 with a bad body is not a station list; an empty one neither.
        assert!(!Id::Dmi.usable(daily, b"{\"features\":[]}"));
        assert!(!Id::Dmi.usable(daily, b"<html>busy</html>"));
        assert!(!Id::Frost.usable(daily, b""));
        assert!(Id::Dmi.usable(daily, &fixture("dmi_stations_20260922.json.gz")));
        assert!(Id::Dmi.optional(1) && !Id::Dmi.optional(0) && !Id::Frost.optional(0));
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
        // S47: the layer's own status, with the newest observation's age.
        assert_eq!(value["status"], "ok");
        assert_eq!(value["percent"], 100);
        assert_eq!(value["ageSeconds"], 60);
        // Stale by the time the line is built: dropped from the list.
        let later = line(&lists, now + 2 * STALE_MS);
        let value: serde_json::Value = serde_json::from_str(&later).unwrap();
        assert_eq!(value["stations"].as_array().unwrap().len(), 0);
        assert_eq!(value["attribution"], "");
    }

    #[test]
    fn the_layer_is_loading_until_every_provider_answered() {
        let now = parse_iso("2026-09-23T10:12:44Z").unwrap();
        let mut lists = blank_lists("loading", "");
        let value = |lists: &[(ProviderInfo, Vec<Station>)]| -> serde_json::Value {
            serde_json::from_str(&line(lists, now)).unwrap()
        };
        let v = value(&lists);
        assert_eq!((v["status"].as_str(), v["percent"].as_u64()), (Some("loading"), Some(0)));
        assert!(v.get("ageSeconds").is_none(), "no station, no age");
        lists[0].0.status = "ok";
        lists[0].1.push(station(now - 600_000, Some(9.0)));
        lists[1].0.status = "failed";
        let v = value(&lists);
        assert_eq!((v["status"].as_str(), v["percent"].as_u64()), (Some("loading"), Some(50)));
        assert_eq!(v["stations"].as_array().unwrap().len(), 1, "stations show as they come");
        lists[2].0.status = "failed";
        lists[3].0.status = "skipped";
        assert_eq!(value(&lists)["status"], "ok");
        lists[0].0.status = "failed";
        assert_eq!(value(&lists)["status"], "error");
    }

    /// S47, the stations bug (docs/protocol.md, Weather layers): the
    /// client's queue is full (tile replies, states) when the list is
    /// published; the line must not be lost for good. Before S47 it was
    /// dropped and recorded as sent, and a client asking again with the
    /// layer already on was not answered.
    #[test]
    fn a_line_a_full_queue_refused_is_offered_again() {
        let hub = Hub::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tx.try_send("busy\n".to_owned()).unwrap();
        let temp = Layers {
            temp: true,
            wind: false,
        };
        hub.set(1, temp, &tx);
        hub.publish("stations\n".into());
        assert_eq!(rx.try_recv().unwrap(), "busy\n");
        assert!(rx.try_recv().is_err());
        // The fetcher's next round: the same list, which it still owes.
        hub.publish("stations\n".into());
        assert_eq!(rx.try_recv().unwrap(), "stations\n");
        hub.publish("stations\n".into());
        assert!(rx.try_recv().is_err(), "delivered once, not repeated");
        // A client that lost it anyway asks again with the layer on.
        hub.set(1, temp, &tx);
        assert_eq!(rx.try_recv().unwrap(), "stations\n");
        // A reset forgets the list: nothing is held until the next fetch.
        hub.reset();
        hub.set(1, temp, &tx);
        assert!(rx.try_recv().is_err());
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
