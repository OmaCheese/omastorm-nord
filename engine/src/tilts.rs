//! The tilt store (S27): the decoded reflectivity tilts of every volume a
//! poller reads, kept on disk, so every product of that volume (and later a
//! mosaic or a section, S24a/b, S25) is composed without fetching the
//! volume again.
//!
//! **Unit.** One tilt is one scan's `Sweep` exactly as `odim::sweep_at`
//! makes it: the requantized u8 codes (rays × gates), each ray's azimuth,
//! elevation and time, the gate geometry and the coding. It is written
//! zlib-deflated, byte-exact (`encode`, `decode`). A volume's **angle
//! table** (`TiltInfo` per dataset, in the file's order, what
//! `products::needed` takes) is kept once per volume, when a read has seen
//! every scan's `where` group; SMHI's lowest-scan read (`/dataset1` alone,
//! DEC-2) stores its tilt without one.
//!
//! **Key.** Station id, the provider's nominal time (SMHI's `valid`, ORD's
//! file time: what the poller knows before any request), and the dataset
//! index, with the angle in tenths of a degree beside it. The volume row
//! names its source file; another file for the same station and time
//! replaces everything kept for it, so tilts of two files never mix.
//!
//! **Where.** `$XDG_CACHE_HOME/omastorm-se/tilts/`: `index.sqlite` (its own
//! WAL database, next to `frames/catalog.sqlite`, which stays as it is), and
//! `<station>/<YYYYMMDDTHHMMSSZ>-<tenths>-<dataset>.u8z` per tilt.
//!
//! **Cap.** `OMASTORM_TILTS_MB` (default 256; 0 turns the store off), in
//! 10^6 bytes of `.u8z` files. After each save the least recently used
//! tilts go first, oldest volume first on a tie, whatever the station.
//! Nothing is ever fetched to fill the store: it holds what the pollers
//! read anyway.
//!
//! **Reads.** A poller names its volume with a `Slot` and asks
//! `Slot::compose` first: when the angle table is stored and every tilt
//! `needed` names is too, the product is composed from disk with no request
//! at all (provenance `FROM_STORE`). Otherwise it reads the file with
//! `decode`, which decodes only the needed tilts the store lacks
//! (`odim::decode_tilts`'s `select`), stores them and the angle table, and
//! composes from both. Every answer is the one `products::decode_volume`
//! gives for the same file (tests below).
//!
//! **For S24a, S24b and S25**: `Store::volumes` lists a station's stored
//! volumes, `Store::volume` its angle table and which tilts are stored,
//! `Store::tilts` / `Store::tilt` load them.

use crate::odim::Tilt as Which;
use crate::products::{Tilt, TiltInfo, Want, compose, needed};
use crate::smhi_live::Scan;
use crate::sweep::{Ray, Sweep};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use rusqlite::{Connection, OptionalExtension, params};
use std::fs;
use std::io::{self, Read, Seek, Write};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// The store's size cap when `OMASTORM_TILTS_MB` is unset, in MB (10^6
/// bytes): twelve SMHI radars' full hour loops, with room to spare.
pub const DEFAULT_CAP_MB: u64 = 256;
/// The tail of a frame's provenance when the store made it: the same
/// fields a fetched frame's has, so `soak-report.sh` sums it as nothing.
pub const FROM_STORE: &str = "from the tilt store, 0 range requests, 0 of 0 bytes";
/// A `.u8z` file's first bytes, once inflated.
const MAGIC: &[u8; 4] = b"OMT1";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn format_ms(ms: i64, format: &str) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format(format).to_string())
        .unwrap_or_default()
}

fn log(what: impl std::fmt::Display) {
    eprintln!("{} Tilts {what}", format_ms(now_ms(), "%Y-%m-%dT%H:%M:%SZ"));
}

fn sql(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}

/// An angle in tenths of a degree, as the store names it.
pub fn tenths(elangle: f64) -> i64 {
    (elangle * 10.0).round() as i64
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

/// A station id as a directory name: anything but ASCII letters, digits,
/// `-` and `_` becomes `_`.
fn dir_name(station: &str) -> String {
    station
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The file format
// ---------------------------------------------------------------------------

/// A tilt as a `.u8z` file: zlib of `MAGIC`, the sweep's fields and
/// `elangle` (little-endian), each ray's azimuth, elevation and time, then
/// the codes ray by ray.
pub fn encode(tilt: &Tilt) -> io::Result<Vec<u8>> {
    let s = &tilt.sweep;
    let gates = usize::from(s.gates);
    let mut raw = Vec::with_capacity(64 + s.rays.len() * (16 + gates));
    raw.extend_from_slice(MAGIC);
    let rays = u32::try_from(s.rays.len()).map_err(io::Error::other)?;
    raw.extend_from_slice(&rays.to_le_bytes());
    raw.extend_from_slice(&s.gates.to_le_bytes());
    raw.extend_from_slice(&s.first_gate_m.to_le_bytes());
    raw.extend_from_slice(&s.gate_spacing_m.to_le_bytes());
    raw.extend_from_slice(&s.scale.to_le_bytes());
    raw.extend_from_slice(&s.offset.to_le_bytes());
    raw.push(s.code1_status);
    raw.extend_from_slice(&s.start_ms.to_le_bytes());
    raw.extend_from_slice(&s.end_ms.to_le_bytes());
    raw.extend_from_slice(&tilt.elangle.to_le_bytes());
    for ray in &s.rays {
        raw.extend_from_slice(&ray.azimuth_deg.to_le_bytes());
        raw.extend_from_slice(&ray.elevation_deg.to_le_bytes());
        raw.extend_from_slice(&ray.time_ms.to_le_bytes());
    }
    for ray in &s.rays {
        if ray.codes.len() != gates {
            return Err(io::Error::other(format!(
                "a ray of {} codes in a sweep of {gates} gates",
                ray.codes.len()
            )));
        }
        raw.extend_from_slice(&ray.codes);
    }
    let mut z = ZlibEncoder::new(Vec::with_capacity(raw.len() / 4), Compression::default());
    z.write_all(&raw)?;
    z.finish()
}

/// Reads a `.u8z` file's fields in order.
struct Fields<'a>(&'a [u8]);

impl Fields<'_> {
    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let (head, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "a short tilt file"))?;
        self.0 = rest;
        Ok(*head)
    }
}

/// `encode`'s inverse.
pub fn decode_file(bytes: &[u8]) -> io::Result<Tilt> {
    let mut raw = Vec::new();
    ZlibDecoder::new(bytes).read_to_end(&mut raw)?;
    let mut f = Fields(&raw);
    if &f.take::<4>()? != MAGIC {
        return Err(io::Error::other("not a tilt file"));
    }
    let rays = u32::from_le_bytes(f.take()?) as usize;
    let gates = u16::from_le_bytes(f.take()?);
    let first_gate_m = u32::from_le_bytes(f.take()?);
    let gate_spacing_m = u32::from_le_bytes(f.take()?);
    let scale = f32::from_le_bytes(f.take()?);
    let offset = f32::from_le_bytes(f.take()?);
    let [code1_status] = f.take()?;
    let start_ms = i64::from_le_bytes(f.take()?);
    let end_ms = i64::from_le_bytes(f.take()?);
    let elangle = f64::from_le_bytes(f.take()?);
    let mut heads = Vec::with_capacity(rays);
    for _ in 0..rays {
        heads.push((
            f32::from_le_bytes(f.take()?),
            f32::from_le_bytes(f.take()?),
            i64::from_le_bytes(f.take()?),
        ));
    }
    let n = usize::from(gates);
    if f.0.len() != rays * n {
        return Err(io::Error::other(format!(
            "{} code bytes for {rays} rays of {n} gates",
            f.0.len()
        )));
    }
    let rays = heads
        .into_iter()
        .zip(f.0.chunks_exact(n.max(1)))
        .map(|((azimuth_deg, elevation_deg, time_ms), codes)| Ray {
            azimuth_deg,
            elevation_deg,
            time_ms,
            codes: codes.to_vec(),
        })
        .collect();
    Ok(Tilt {
        elangle,
        sweep: Sweep {
            rays,
            start_ms,
            end_ms,
            gates,
            first_gate_m,
            gate_spacing_m,
            scale,
            offset,
            code1_status,
        },
    })
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// One stored tilt, as `Store::volume` lists it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stored {
    /// The dataset's index in the file (`/dataset1` is 0).
    pub dataset: usize,
    pub tenths: i64,
    pub info: TiltInfo,
    /// Its `.u8z` file's size.
    pub bytes: u64,
}

/// What the store holds of one volume.
#[derive(Clone, PartialEq, Debug)]
pub struct Volume {
    pub time_ms: i64,
    /// The file the tilts came from (SMHI's volume key, ORD's object key).
    pub source: String,
    /// Every scan's geometry, in the file's order; `None` until a read has
    /// seen them all.
    pub angles: Option<Vec<TiltInfo>>,
    /// The stored tilts, by dataset.
    pub tilts: Vec<Stored>,
}

/// Tilts and bytes on disk.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Usage {
    pub tilts: u64,
    pub bytes: u64,
}

pub struct Store {
    dir: PathBuf,
    conn: Mutex<Connection>,
    cap: u64,
}

fn angles_json(infos: &[TiltInfo]) -> String {
    let rows: Vec<[f64; 4]> = infos
        .iter()
        .map(|i| {
            [
                i.elangle,
                f64::from(i.first_gate_m),
                f64::from(i.gate_spacing_m),
                f64::from(i.gates),
            ]
        })
        .collect();
    serde_json::to_string(&rows).unwrap_or_default()
}

fn angles_of(json: &str) -> Option<Vec<TiltInfo>> {
    let rows: Vec<[f64; 4]> = serde_json::from_str(json).ok()?;
    Some(
        rows.into_iter()
            .map(|[elangle, first, spacing, gates]| TiltInfo {
                elangle,
                first_gate_m: first as u32,
                gate_spacing_m: spacing as u32,
                gates: gates as u16,
            })
            .collect(),
    )
}

static SHARED: LazyLock<Option<Arc<Store>>> = LazyLock::new(|| {
    let mb = match std::env::var("OMASTORM_TILTS_MB") {
        Ok(v) => v.trim().parse::<u64>().unwrap_or_else(|_| {
            log(format_args!(
                "store: OMASTORM_TILTS_MB={v:?} is not a number; {DEFAULT_CAP_MB} MB"
            ));
            DEFAULT_CAP_MB
        }),
        Err(_) => DEFAULT_CAP_MB,
    };
    if mb == 0 {
        log("store: off (OMASTORM_TILTS_MB=0)");
        return None;
    }
    match crate::osm::cache_root().and_then(|root| Store::open(root.join("tilts"), mb * 1_000_000))
    {
        Ok(store) => Some(Arc::new(store)),
        Err(e) => {
            log(format_args!("store: off: {e}"));
            None
        }
    }
});

/// The engine's store, under its cache root, opened on first use; `None`
/// when `OMASTORM_TILTS_MB` is 0 or it cannot be opened (the pollers then
/// read every volume as before S27).
pub fn shared() -> Option<Arc<Store>> {
    SHARED.clone()
}

impl Store {
    /// Open or create the store in `dir`, capped at `cap` bytes.
    pub fn open(dir: PathBuf, cap: u64) -> io::Result<Store> {
        fs::create_dir_all(&dir)?;
        let conn = Connection::open(dir.join("index.sqlite")).map_err(sql)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 2000;
             CREATE TABLE IF NOT EXISTS volumes (
                 station TEXT NOT NULL,
                 time_ms INTEGER NOT NULL,
                 source TEXT NOT NULL,
                 angles TEXT,
                 PRIMARY KEY (station, time_ms)
             );
             CREATE TABLE IF NOT EXISTS tilts (
                 station TEXT NOT NULL,
                 time_ms INTEGER NOT NULL,
                 dataset INTEGER NOT NULL,
                 tenths INTEGER NOT NULL,
                 elangle REAL NOT NULL,
                 first_gate_m INTEGER NOT NULL,
                 gate_spacing_m INTEGER NOT NULL,
                 gates INTEGER NOT NULL,
                 file TEXT NOT NULL,
                 bytes INTEGER NOT NULL,
                 used_ms INTEGER NOT NULL,
                 PRIMARY KEY (station, time_ms, dataset)
             );
             CREATE INDEX IF NOT EXISTS tilts_used ON tilts (used_ms, time_ms);",
        )
        .map_err(sql)?;
        Ok(Store {
            dir,
            conn: Mutex::new(conn),
            cap,
        })
    }

    /// A station's stored volumes, newest first: their times and source
    /// files. Only volumes with at least one tilt.
    pub fn volumes(&self, station: &str) -> io::Result<Vec<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT v.time_ms, v.source FROM volumes v
                 WHERE v.station = ?1 AND EXISTS (
                     SELECT 1 FROM tilts t WHERE t.station = v.station AND t.time_ms = v.time_ms)
                 ORDER BY v.time_ms DESC",
            )
            .map_err(sql)?;
        let rows = stmt
            .query_map(params![station], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// What the store holds of one volume: its source, angle table and
    /// stored tilts; `None` when it holds nothing.
    pub fn volume(&self, station: &str, time_ms: i64) -> io::Result<Option<Volume>> {
        let conn = self.conn.lock().unwrap();
        let Some((source, angles)) = conn
            .query_row(
                "SELECT source, angles FROM volumes WHERE station = ?1 AND time_ms = ?2",
                params![station, time_ms],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(sql)?
        else {
            return Ok(None);
        };
        let mut stmt = conn
            .prepare(
                "SELECT dataset, tenths, elangle, first_gate_m, gate_spacing_m, gates, bytes
                 FROM tilts WHERE station = ?1 AND time_ms = ?2 ORDER BY dataset",
            )
            .map_err(sql)?;
        let tilts = stmt
            .query_map(params![station, time_ms], |r| {
                Ok(Stored {
                    dataset: r.get::<_, i64>(0)? as usize,
                    tenths: r.get(1)?,
                    info: TiltInfo {
                        elangle: r.get(2)?,
                        first_gate_m: r.get(3)?,
                        gate_spacing_m: r.get(4)?,
                        gates: r.get(5)?,
                    },
                    bytes: r.get::<_, i64>(6)? as u64,
                })
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        Ok(Some(Volume {
            time_ms,
            source,
            angles: angles.as_deref().and_then(angles_of),
            tilts,
        }))
    }

    /// One stored tilt, by dataset index; `None` when it is not stored (or
    /// its file has gone, which forgets it). Marks it used.
    pub fn tilt(&self, station: &str, time_ms: i64, dataset: usize) -> io::Result<Option<Tilt>> {
        let file: Option<String> = {
            let conn = self.conn.lock().unwrap();
            let file = conn
                .query_row(
                    "SELECT file FROM tilts WHERE station = ?1 AND time_ms = ?2 AND dataset = ?3",
                    params![station, time_ms, dataset as i64],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql)?;
            if file.is_some() {
                conn.execute(
                    "UPDATE tilts SET used_ms = ?4
                     WHERE station = ?1 AND time_ms = ?2 AND dataset = ?3",
                    params![station, time_ms, dataset as i64, now_ms()],
                )
                .map_err(sql)?;
            }
            file
        };
        let Some(file) = file else { return Ok(None) };
        match fs::read(self.dir.join(&file)) {
            Ok(bytes) => decode_file(&bytes).map(Some),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.conn
                    .lock()
                    .unwrap()
                    .execute(
                        "DELETE FROM tilts WHERE station = ?1 AND time_ms = ?2 AND dataset = ?3",
                        params![station, time_ms, dataset as i64],
                    )
                    .map_err(sql)?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    #[allow(
        dead_code,
        reason = "the API S24a, S24b and S25 compose from (engine/README.md)"
    )]
    /// Every stored tilt of a volume, by dataset, with its index.
    pub fn tilts(&self, station: &str, time_ms: i64) -> io::Result<Vec<(usize, Tilt)>> {
        let Some(volume) = self.volume(station, time_ms)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for stored in volume.tilts {
            if let Some(tilt) = self.tilt(station, time_ms, stored.dataset)? {
                out.push((stored.dataset, tilt));
            }
        }
        Ok(out)
    }

    /// Tilts and bytes stored for `station`, or in all when `None`.
    pub fn usage(&self, station: Option<&str>) -> io::Result<Usage> {
        let conn = self.conn.lock().unwrap();
        let (tilts, bytes): (i64, i64) = conn
            .query_row(
                "SELECT count(*), coalesce(sum(bytes), 0) FROM tilts
                 WHERE ?1 IS NULL OR station = ?1",
                params![station],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sql)?;
        Ok(Usage {
            tilts: tilts as u64,
            bytes: bytes as u64,
        })
    }

    /// Keep `tilts` (dataset index, tilt) of `station`'s volume at
    /// `time_ms` read from `source`, and its angle table when given; then
    /// evict past the cap and log the station's usage. Another source for
    /// the same station and time replaces what was kept.
    pub fn save(
        &self,
        station: &str,
        time_ms: i64,
        source: &str,
        angles: Option<&[TiltInfo]>,
        tilts: &[(usize, &Tilt)],
    ) -> io::Result<()> {
        let blobs = tilts
            .iter()
            .map(|(k, t)| Ok((*k, *t, encode(t)?)))
            .collect::<io::Result<Vec<_>>>()?;
        let sub = dir_name(station);
        fs::create_dir_all(self.dir.join(&sub))?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(sql)?;
        let held: Option<String> = tx
            .query_row(
                "SELECT source FROM volumes WHERE station = ?1 AND time_ms = ?2",
                params![station, time_ms],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        let mut stale = Vec::new();
        if held.as_deref().is_some_and(|held| held != source) {
            let mut stmt = tx
                .prepare("SELECT file FROM tilts WHERE station = ?1 AND time_ms = ?2")
                .map_err(sql)?;
            stale = stmt
                .query_map(params![station, time_ms], |r| r.get::<_, String>(0))
                .map_err(sql)?
                .collect::<Result<_, _>>()
                .map_err(sql)?;
            drop(stmt);
            tx.execute(
                "DELETE FROM tilts WHERE station = ?1 AND time_ms = ?2",
                params![station, time_ms],
            )
            .map_err(sql)?;
            tx.execute(
                "UPDATE volumes SET angles = NULL WHERE station = ?1 AND time_ms = ?2",
                params![station, time_ms],
            )
            .map_err(sql)?;
        }
        tx.execute(
            "INSERT INTO volumes (station, time_ms, source, angles) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (station, time_ms) DO UPDATE SET
                 source = excluded.source,
                 angles = coalesce(excluded.angles, volumes.angles)",
            params![station, time_ms, source, angles.map(angles_json)],
        )
        .map_err(sql)?;
        let used = now_ms();
        let stamp = format_ms(time_ms, "%Y%m%dT%H%M%SZ");
        for (dataset, tilt, blob) in &blobs {
            let t = tenths(tilt.elangle);
            let file = format!("{sub}/{stamp}-{t}-{dataset}.u8z");
            let path = self.dir.join(&file);
            let temp = path.with_extension("tmp");
            fs::write(&temp, blob)?;
            fs::rename(&temp, &path)?;
            stale.retain(|f| f != &file);
            let s = &tilt.sweep;
            tx.execute(
                "INSERT OR REPLACE INTO tilts (station, time_ms, dataset, tenths, elangle,
                     first_gate_m, gate_spacing_m, gates, file, bytes, used_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    station,
                    time_ms,
                    *dataset as i64,
                    t,
                    tilt.elangle,
                    s.first_gate_m,
                    s.gate_spacing_m,
                    s.gates,
                    file,
                    blob.len() as i64,
                    used
                ],
            )
            .map_err(sql)?;
        }
        tx.commit().map_err(sql)?;
        for file in stale {
            let _ = fs::remove_file(self.dir.join(file));
        }
        let evicted = self.evict(&conn)?;
        drop(conn);
        if !blobs.is_empty() {
            let here = self.usage(Some(station))?;
            log(format_args!(
                "{station}: {} tilts, {}, cap {}",
                here.tilts,
                mb(here.bytes),
                mb(self.cap)
            ));
        }
        if evicted.tilts > 0 {
            let all = self.usage(None)?;
            log(format_args!(
                "store: evicted {} tilts, {}; now {} tilts, {}, cap {}",
                evicted.tilts,
                mb(evicted.bytes),
                all.tilts,
                mb(all.bytes),
                mb(self.cap)
            ));
        }
        Ok(())
    }

    /// Drop the least recently used tilts (oldest volume first on a tie),
    /// whatever the station, until the store fits its cap; then volumes
    /// left with no tilt. Returns what went.
    fn evict(&self, conn: &Connection) -> io::Result<Usage> {
        let total: i64 = conn
            .query_row("SELECT coalesce(sum(bytes), 0) FROM tilts", [], |r| {
                r.get(0)
            })
            .map_err(sql)?;
        let mut over = total - i64::try_from(self.cap).unwrap_or(i64::MAX);
        if over <= 0 {
            return Ok(Usage::default());
        }
        let mut victims: Vec<(String, i64, i64, String, i64)> = Vec::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT station, time_ms, dataset, file, bytes FROM tilts
                     ORDER BY used_ms, time_ms, station, dataset",
                )
                .map_err(sql)?;
            let mut rows = stmt.query([]).map_err(sql)?;
            while over > 0
                && let Some(r) = rows.next().map_err(sql)?
            {
                let bytes: i64 = r.get(4).map_err(sql)?;
                victims.push((
                    r.get(0).map_err(sql)?,
                    r.get(1).map_err(sql)?,
                    r.get(2).map_err(sql)?,
                    r.get(3).map_err(sql)?,
                    bytes,
                ));
                over -= bytes;
            }
        }
        let mut gone = Usage::default();
        for (station, time_ms, dataset, file, bytes) in &victims {
            conn.execute(
                "DELETE FROM tilts WHERE station = ?1 AND time_ms = ?2 AND dataset = ?3",
                params![station, time_ms, dataset],
            )
            .map_err(sql)?;
            let _ = fs::remove_file(self.dir.join(file));
            gone.tilts += 1;
            gone.bytes += *bytes as u64;
        }
        conn.execute(
            "DELETE FROM volumes WHERE NOT EXISTS (
                 SELECT 1 FROM tilts t
                 WHERE t.station = volumes.station AND t.time_ms = volumes.time_ms)",
            [],
        )
        .map_err(sql)?;
        Ok(gone)
    }
}

// ---------------------------------------------------------------------------
// Reading a volume through the store
// ---------------------------------------------------------------------------

/// One volume, as a poller names it before any request: the store, the
/// station, the provider's nominal time and the source file.
#[derive(Clone)]
pub struct Slot {
    pub store: Arc<Store>,
    pub station: String,
    pub time_ms: i64,
    pub source: String,
}

impl Slot {
    /// `want` composed from the store alone, when it holds the volume's
    /// angle table (not needed for SMHI's lowest scan, `/dataset1`) and
    /// every tilt `needed` names; `None` otherwise. No request.
    pub fn compose(&self, want: Want, which: Which) -> io::Result<Option<Scan>> {
        let Some(volume) = self.store.volume(&self.station, self.time_ms)? else {
            return Ok(None);
        };
        if volume.source != self.source {
            return Ok(None);
        }
        let picked = match &volume.angles {
            _ if want.is_lowest() && which == Which::First => vec![0],
            Some(infos) => needed(want, infos),
            None => return Ok(None),
        };
        if picked.is_empty() {
            return Ok(None);
        }
        let mut tilts = Vec::with_capacity(picked.len());
        for k in picked {
            match self.store.tilt(&self.station, self.time_ms, k)? {
                Some(tilt) => tilts.push((k, tilt)),
                None => return Ok(None),
            }
        }
        finish(want, which, volume.angles.as_deref(), tilts)
            .map(Some)
            .map_err(io::Error::other)
    }

    /// The stored tilt for dataset `k` of this source, when its geometry is
    /// `info`.
    fn stored(&self, k: usize, info: &TiltInfo) -> Option<Tilt> {
        let volume = self.store.volume(&self.station, self.time_ms).ok()??;
        let held = volume.tilts.iter().find(|s| s.dataset == k)?;
        if volume.source != self.source || held.info != *info {
            return None;
        }
        self.store
            .tilt(&self.station, self.time_ms, k)
            .ok()?
            .filter(|t| t.elangle == info.elangle)
    }

    /// `Store::save`, logged instead of failing: a frame never depends on
    /// the store.
    fn save(&self, angles: Option<&[TiltInfo]>, tilts: &[(usize, &Tilt)]) {
        if let Err(e) = self
            .store
            .save(&self.station, self.time_ms, &self.source, angles, tilts)
        {
            log(format_args!("{}: not stored: {e}", self.station));
        }
    }
}

/// A field-by-field copy (`Sweep` is not `Clone`).
fn copy_sweep(s: &Sweep) -> Sweep {
    Sweep {
        rays: s
            .rays
            .iter()
            .map(|r| Ray {
                azimuth_deg: r.azimuth_deg,
                elevation_deg: r.elevation_deg,
                time_ms: r.time_ms,
                codes: r.codes.clone(),
            })
            .collect(),
        start_ms: s.start_ms,
        end_ms: s.end_ms,
        gates: s.gates,
        first_gate_m: s.first_gate_m,
        gate_spacing_m: s.gate_spacing_m,
        scale: s.scale,
        offset: s.offset,
        code1_status: s.code1_status,
    }
}

/// What `products::decode_volume` makes of these tilts (dataset index,
/// tilt): the lowest scan as it is, or the product with the lowest scan it
/// read riding along (S26), under the same rules.
fn finish(
    want: Want,
    which: Which,
    angles: Option<&[TiltInfo]>,
    mut tilts: Vec<(usize, Tilt)>,
) -> Result<Scan, String> {
    // The decoder's order, so `compose`'s ties fall as they did.
    tilts.sort_by_key(|(k, _)| *k);
    let mut tilts: Vec<Tilt> = tilts.into_iter().map(|(_, t)| t).collect();
    if want.is_lowest() {
        return tilts
            .drain(..)
            .next()
            .map(|t| Scan::Polar(t.sweep))
            .ok_or_else(|| "the volume holds no reflectivity scans".to_owned());
    }
    // SMHI's lowest scan is `/dataset1`; its free copy is only that scan
    // when `/dataset1` is the lowest angle.
    let first_is_lowest = angles.is_some_and(|a| needed(Want::Lowest, a) == [0]);
    let free = match want {
        Want::Lowest | Want::Angle(_) => None,
        _ if which == Which::First && !first_is_lowest => None,
        _ => tilts
            .iter()
            .min_by(|a, b| a.elangle.total_cmp(&b.elangle))
            .map(|t| Box::new(copy_sweep(&t.sweep))),
    };
    compose(want, tilts).map(|sweep| Scan::Product(sweep, want, free))
}

/// `products::decode_volume` through the store: read from the file only the
/// tilts `want` needs that `slot` lacks, keep them (and the angle table),
/// and compose from both. The same answer as `decode_volume`, byte for
/// byte; with no slot, `decode_volume` itself.
pub fn decode<R: Read + Seek + Send + 'static>(
    reader: R,
    want: Want,
    which: Which,
    slot: Option<&Slot>,
) -> Result<Scan, String> {
    let Some(slot) = slot else {
        return crate::products::decode_volume(reader, want, which);
    };
    if want.is_lowest() && which == Which::First {
        // DEC-2: `/dataset1` alone; no angle table.
        let tilt = crate::odim::decode_first(reader).map_err(|e| e.to_string())?;
        slot.save(None, &[(0, &tilt)]);
        return Ok(Scan::Polar(tilt.sweep));
    }
    let mut table: Vec<TiltInfo> = Vec::new();
    let mut kept: Vec<(usize, Tilt)> = Vec::new();
    let mut read: Vec<usize> = Vec::new();
    let decoded = crate::odim::decode_tilts(reader, |infos| {
        table = infos.to_vec();
        for k in needed(want, infos) {
            match slot.stored(k, &infos[k]) {
                Some(tilt) => kept.push((k, tilt)),
                None => read.push(k),
            }
        }
        read.clone()
    })
    .map_err(|e| e.to_string())?;
    let fetched: Vec<(usize, Tilt)> = read.into_iter().zip(decoded).collect();
    let refs: Vec<(usize, &Tilt)> = fetched.iter().map(|(k, t)| (*k, t)).collect();
    slot.save(Some(&table), &refs);
    kept.extend(fetched);
    finish(want, which, Some(&table), kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::products::Blockage;

    const RAW: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../data/raw/");
    const FIXTURES: [(&str, Which, &str); 3] = [
        (
            "radar_vara_qcvol_202609131055_tilts.h5",
            Which::First,
            "vara",
        ),
        ("ord_nohur_202609140930_tilts.h5", Which::Lowest, "nohur"),
        ("ord_dksin_202609140940_tilts.h5", Which::Lowest, "dksin"),
    ];

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("omastorm-tilts-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn open(fixture: &str) -> fs::File {
        fs::File::open(format!("{RAW}{fixture}")).unwrap()
    }

    fn all_tilts(fixture: &str) -> Vec<Tilt> {
        crate::odim::decode_tilts(open(fixture), |infos| (0..infos.len()).collect()).unwrap()
    }

    /// Every field of two sweeps, codes included.
    fn same(a: &Sweep, b: &Sweep) -> bool {
        (
            a.start_ms,
            a.end_ms,
            a.gates,
            a.first_gate_m,
            a.gate_spacing_m,
            a.scale.to_bits(),
            a.offset.to_bits(),
            a.code1_status,
            a.rays.len(),
        ) == (
            b.start_ms,
            b.end_ms,
            b.gates,
            b.first_gate_m,
            b.gate_spacing_m,
            b.scale.to_bits(),
            b.offset.to_bits(),
            b.code1_status,
            b.rays.len(),
        ) && a.rays.iter().zip(&b.rays).all(|(x, y)| {
            x.azimuth_deg.to_bits() == y.azimuth_deg.to_bits()
                && x.elevation_deg.to_bits() == y.elevation_deg.to_bits()
                && x.time_ms == y.time_ms
                && x.codes == y.codes
        })
    }

    fn scans_match(a: &Scan, b: &Scan) -> bool {
        match (a, b) {
            (Scan::Polar(x), Scan::Polar(y)) => same(x, y),
            (Scan::Product(x, wx, fx), Scan::Product(y, wy, fy)) => {
                same(x, y)
                    && wx == wy
                    && match (fx, fy) {
                        (None, None) => true,
                        (Some(p), Some(q)) => same(p, q),
                        _ => false,
                    }
            }
            _ => false,
        }
    }

    /// The S20 `_tilts` fixtures, every tilt, through a `.u8z` file and the
    /// store and back, byte for byte; the sizes are the plan's §S27 table.
    #[test]
    fn tilts_round_trip_byte_exact() {
        let dir = scratch("round");
        let store = Store::open(dir.clone(), u64::MAX).unwrap();
        for (fixture, _, station) in FIXTURES {
            let tilts = all_tilts(fixture);
            assert!(tilts.len() >= 3, "{fixture}");
            let refs: Vec<(usize, &Tilt)> = tilts.iter().enumerate().collect();
            store.save(station, 1000, fixture, None, &refs).unwrap();
            let mut sizes = Vec::new();
            for (k, tilt) in tilts.iter().enumerate() {
                let blob = encode(tilt).unwrap();
                let back = decode_file(&blob).unwrap();
                assert_eq!(back.elangle.to_bits(), tilt.elangle.to_bits());
                assert!(same(&back.sweep, &tilt.sweep), "{fixture} tilt {k}");
                let stored = store.tilt(station, 1000, k).unwrap().unwrap();
                assert!(same(&stored.sweep, &tilt.sweep), "{fixture} stored {k}");
                sizes.push(format!(
                    "{:.1}°:{}x{}={}KB",
                    tilt.elangle,
                    tilt.sweep.rays.len(),
                    tilt.sweep.gates,
                    blob.len() / 1000
                ));
            }
            let used = store.usage(Some(station)).unwrap();
            assert_eq!(used.tilts, tilts.len() as u64);
            eprintln!(
                "{station}: {} bytes stored: {}",
                used.bytes,
                sizes.join(" ")
            );
        }
        assert!(decode_file(b"not zlib").is_err());
        let _ = fs::remove_dir_all(dir);
    }

    fn golden_table(station: &str) -> &'static Blockage {
        let golden = match station {
            "vara" => "vara-20260913",
            "nohur" => "nohur-20260914",
            _ => "dksin-20260914",
        };
        let key: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(format!("{RAW}../../golden/{golden}/products.json")).unwrap(),
        )
        .unwrap();
        let tenths: Vec<u8> = key["blockageTenths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        Box::leak(Box::new(Blockage {
            tenths: tenths.try_into().unwrap(),
        }))
    }

    /// Composing from the store gives what composing from the file gives
    /// (`products::decode_volume`, held to the S20 answer keys): cappi1,
    /// cappi2, cmax, clear, two angles and the lowest scan, the free lowest
    /// scan included, on all three providers' files. After one CMAX read
    /// the store answers everything else with no read at all.
    #[test]
    fn compose_from_store_equals_compose_from_file() {
        let dir = scratch("compose");
        let store = Arc::new(Store::open(dir.clone(), u64::MAX).unwrap());
        for (fixture, which, station) in FIXTURES {
            let slot = Slot {
                store: store.clone(),
                station: station.into(),
                time_ms: 1_000,
                source: fixture.into(),
            };
            assert!(slot.compose(Want::ColMax, which).unwrap().is_none());
            let first = decode(open(fixture), Want::ColMax, which, Some(&slot)).unwrap();
            let file = crate::products::decode_volume(open(fixture), Want::ColMax, which).unwrap();
            assert!(scans_match(&first, &file), "{station} cmax read");
            for want in [
                Want::ColMax,
                Want::Cappi(1000.0, 1000),
                Want::Cappi(2000.0, 2000),
                Want::Hybrid(golden_table(station)),
                Want::Angle(4.0),
                Want::Angle(1.0),
                Want::Lowest,
            ] {
                let from_file = crate::products::decode_volume(open(fixture), want, which).unwrap();
                let from_store = slot
                    .compose(want, which)
                    .unwrap()
                    .unwrap_or_else(|| panic!("{station} {}: not in the store", want.variant()));
                assert!(
                    scans_match(&from_store, &from_file),
                    "{station} {}",
                    want.variant()
                );
            }
            // Another file for the same station and time is not this one.
            let other = Slot {
                source: "another".into(),
                ..slot.clone()
            };
            assert!(other.compose(Want::ColMax, which).unwrap().is_none());
            let volume = store.volume(station, 1_000).unwrap().unwrap();
            assert_eq!(
                volume.angles.as_ref().map(Vec::len),
                Some(volume.tilts.len())
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// SMHI's lowest scan (`/dataset1`, DEC-2) stores one tilt and no
    /// angle table, so a product still reads the file, but only the tilts
    /// the store lacks, and answers as the file does.
    #[test]
    fn a_partial_volume_reads_only_what_is_missing() {
        let dir = scratch("partial");
        let store = Arc::new(Store::open(dir.clone(), u64::MAX).unwrap());
        let (fixture, which, station) = FIXTURES[0];
        let slot = Slot {
            store: store.clone(),
            station: station.into(),
            time_ms: 5_000,
            source: fixture.into(),
        };
        let lowest = decode(open(fixture), Want::Lowest, which, Some(&slot)).unwrap();
        let file = crate::products::decode_volume(open(fixture), Want::Lowest, which).unwrap();
        assert!(scans_match(&lowest, &file));
        let volume = store.volume(station, 5_000).unwrap().unwrap();
        assert_eq!((volume.angles, volume.tilts.len()), (None, 1));
        assert!(scans_match(
            &slot.compose(Want::Lowest, which).unwrap().unwrap(),
            &file
        ));
        assert!(
            slot.compose(Want::Cappi(1000.0, 1000), which)
                .unwrap()
                .is_none()
        );
        let cappi = decode(open(fixture), Want::Cappi(1000.0, 1000), which, Some(&slot)).unwrap();
        let file = crate::products::decode_volume(open(fixture), Want::Cappi(1000.0, 1000), which)
            .unwrap();
        assert!(scans_match(&cappi, &file));
        let needed_now = needed(
            Want::Cappi(1000.0, 1000),
            store
                .volume(station, 5_000)
                .unwrap()
                .unwrap()
                .angles
                .as_ref()
                .unwrap(),
        );
        assert_eq!(
            store.volume(station, 5_000).unwrap().unwrap().tilts.len(),
            needed_now.len()
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// Past the cap the least recently used tilts go, whatever the station,
    /// oldest volume first on a tie, files and all; a read counts as a use.
    #[test]
    fn eviction_drops_the_least_recently_used() {
        // Five-minute volumes, as the pollers key them.
        const M: i64 = 300_000;
        let dir = scratch("evict");
        let tilts = all_tilts(FIXTURES[1].0);
        let one = encode(&tilts[0]).unwrap().len() as u64;
        // Room for five of this tilt.
        let store = Store::open(dir.clone(), one * 5 + one / 2).unwrap();
        let save = |station: &str, t: i64| {
            store
                .save(station, t * M, "src", None, &[(0, &tilts[0])])
                .unwrap()
        };
        for t in 1..=5 {
            save(if t % 2 == 0 { "a" } else { "b" }, t);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(store.usage(None).unwrap().tilts, 5);
        // Reading the oldest makes it the newest in use.
        assert!(store.tilt("b", M, 0).unwrap().is_some());
        std::thread::sleep(std::time::Duration::from_millis(2));
        save("a", 6);
        assert_eq!(store.usage(None).unwrap().tilts, 5);
        assert!(
            store.volume("a", 2 * M).unwrap().is_none(),
            "the least recently used"
        );
        assert!(store.volume("b", M).unwrap().is_some(), "read, so kept");
        assert_eq!(
            store
                .volumes("a")
                .unwrap()
                .iter()
                .map(|v| v.0)
                .collect::<Vec<_>>(),
            [6 * M, 4 * M]
        );
        let files = fs::read_dir(dir.join("a")).unwrap().count()
            + fs::read_dir(dir.join("b")).unwrap().count();
        assert_eq!(files, 5, "evicted files are removed");
        // Another source for the same time replaces the volume.
        store
            .save("a", 6 * M, "other", None, &[(1, &tilts[1])])
            .unwrap();
        let volume = store.volume("a", 6 * M).unwrap().unwrap();
        assert_eq!(volume.source, "other");
        assert_eq!(
            volume.tilts.iter().map(|s| s.dataset).collect::<Vec<_>>(),
            [1]
        );
        let _ = fs::remove_dir_all(dir);
    }
}
