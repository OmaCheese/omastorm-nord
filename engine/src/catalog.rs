//! The per-station frame ring buffer (DESIGN.md, frame storage): a SQLite
//! catalog of station, time, elevation, product, and provenance under
//! `$XDG_CACHE_HOME/omastorm-se/frames/`, with the sweep and lookup PNGs as
//! files beside it. Storage is a catalog, not a transport: the UI never reads
//! it directly. The engine writes each complete live frame here and keeps the
//! newest `RING` per station; the timeline lists the ring, and each listed
//! frame's files are linked into the runtime directory under names derived
//! from their content (`docs/protocol.md`, texture files), so a client sees
//! one name per frame for as long as the ring holds it.

use crate::protocol::{Frame, FrameKind};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// Frames kept per station: about five hours of volume scans.
pub const RING: usize = 60;

pub struct Catalog {
    conn: Mutex<Connection>,
    dir: PathBuf,
}

/// One frame as the timeline lists it.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Entry {
    pub id: String,
    pub scan_time: String,
    /// `scanTime` in milliseconds since the epoch.
    pub start_ms: i64,
    /// Where its textures are, for a frame the catalog holds; `None` for
    /// one it does not (the archived frame).
    pub record: Option<Record>,
}

/// A catalogued frame's metadata and files.
#[derive(Clone, PartialEq, Debug)]
pub struct Record {
    /// The frame as broadcast, with empty texture paths.
    pub frame: Frame,
    /// The sweep (or grid) texture and the azimuth lookup, absolute paths.
    /// A grid's lookup file is empty and never published.
    pub texture: PathBuf,
    pub azimuth_lut: PathBuf,
    /// 8 hex digits naming this version of the files: from the content hash
    /// in the file name, or for a pre-S19 file from its size and time. The
    /// files under a tag never change (`store` writes a new version under a
    /// new name), so neither does anything published under it.
    pub tag: String,
    /// A grid frame's one-channel code texture (`docs/protocol.md`, code
    /// texture), beside its texture; `None` for polar frames and grids
    /// catalogued before S19.
    pub codes: Option<PathBuf>,
}

/// A frame's encoded textures for `store_with_codes`: the sweep or grid
/// texture, the azimuth lookup (empty for a grid), and a grid's code
/// texture (empty for none).
pub struct Pngs<'a> {
    pub texture: &'a [u8],
    pub azimuth_lut: &'a [u8],
    pub codes: &'a [u8],
}

/// The code texture's file beside a texture file: `<stem>-codes.png` for
/// `<stem>-sweep.png`.
fn codes_of(texture: &str) -> Option<String> {
    texture
        .strip_suffix("-sweep.png")
        .map(|stem| format!("{stem}-codes.png"))
}

/// A frame's files to remove: its texture, lookup, and any code texture.
fn with_codes(texture: String, lut: String) -> Vec<String> {
    let codes = codes_of(&texture);
    [Some(texture), Some(lut), codes]
        .into_iter()
        .flatten()
        .collect()
}

/// A stored frame with its texture bytes.
#[cfg(test)]
pub struct Stored {
    /// The frame as broadcast, with empty texture paths.
    pub frame: Frame,
    /// `scanTime` in milliseconds since the epoch, for `ageSeconds`.
    pub start_ms: i64,
    pub texture: Vec<u8>,
    pub azimuth_lut: Vec<u8>,
}

fn sql(e: rusqlite::Error) -> io::Error {
    io::Error::other(format!("frame catalog: {e}"))
}

/// 64-bit FNV-1a over `parts`, each prefixed by its length. Not
/// cryptographic: it only has to tell two versions of one frame apart.
fn fnv1a(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for part in parts {
        eat(&(part.len() as u64).to_le_bytes());
        eat(part);
    }
    hash
}

/// The tag of the texture file at `relative` (under `dir`): the content hash
/// a post-S19 name carries (`<id>-<16 hex>-sweep.png`), else a hash of the
/// pre-S19 file's name, size, and modification time.
fn tag_of(dir: &Path, relative: &str) -> String {
    let hashed = relative
        .strip_suffix("-sweep.png")
        .and_then(|stem| stem.rsplit_once('-'))
        .map(|(_, hex)| hex)
        .filter(|hex| hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
    if let Some(hex) = hashed {
        return hex[..8].to_owned();
    }
    let (len, nanos) = fs::metadata(dir.join(relative))
        .map(|m| {
            let nanos = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            (m.len(), nanos)
        })
        .unwrap_or_default();
    let hash = fnv1a(&[
        relative.as_bytes(),
        &len.to_le_bytes(),
        &nanos.to_le_bytes(),
    ]);
    format!("{hash:016x}")[..8].to_owned()
}

impl Catalog {
    /// Open or create the catalog in `dir`.
    pub fn open(dir: PathBuf) -> io::Result<Catalog> {
        fs::create_dir_all(&dir)?;
        let conn = Connection::open(dir.join("catalog.sqlite")).map_err(sql)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS frames (
                 id TEXT PRIMARY KEY,
                 site TEXT NOT NULL,
                 product TEXT NOT NULL,
                 elevation_deg REAL NOT NULL,
                 start_ms INTEGER NOT NULL,
                 scan_time TEXT NOT NULL,
                 sweep_end TEXT NOT NULL,
                 provenance TEXT NOT NULL,
                 stored_ms INTEGER NOT NULL,
                 frame TEXT NOT NULL,
                 texture TEXT NOT NULL,
                 azimuth_lut TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS frames_site_time ON frames (site, start_ms);",
        )
        .map_err(sql)?;
        Ok(Catalog {
            conn: Mutex::new(conn),
            dir,
        })
    }

    /// Record a complete frame and its textures, then drop the station's
    /// frames past the ring, files included. Replaces a frame of the same id.
    /// The files are named by their content, so the same bytes again write
    /// nothing, and other bytes for the same frame go under a new name while
    /// the old version's files are removed: a published name never sees its
    /// bytes change. Returns the frame as the timeline lists it.
    #[cfg(test)]
    pub fn store(
        &self,
        site: &str,
        frame: &Frame,
        start_ms: i64,
        texture: &[u8],
        azimuth_lut: &[u8],
        provenance: &str,
    ) -> io::Result<Entry> {
        let pngs = Pngs {
            texture,
            azimuth_lut,
            codes: &[],
        };
        self.store_with_codes(site, frame, start_ms, pngs, provenance)
    }

    /// `store`, with a grid frame's one-channel code texture beside its grid
    /// texture (`docs/protocol.md`, code texture); empty `codes` for none.
    /// The code file shares the texture's version and goes with it.
    pub fn store_with_codes(
        &self,
        site: &str,
        frame: &Frame,
        start_ms: i64,
        pngs: Pngs,
        provenance: &str,
    ) -> io::Result<Entry> {
        let Pngs {
            texture,
            azimuth_lut,
            codes,
        } = pngs;
        let mut record = frame.clone();
        record.texture.clear();
        record.azimuth_lut.clear();
        let hash = format!("{:016x}", fnv1a(&[texture, azimuth_lut]));
        let texture_path = format!("{site}/{}-{hash}-sweep.png", frame.id);
        let lut_path = format!("{site}/{}-{hash}-azlut.png", frame.id);
        let codes_path = codes_of(&texture_path).unwrap_or_default();
        let mut files = vec![(&texture_path, texture), (&lut_path, azimuth_lut)];
        if !codes.is_empty() {
            files.push((&codes_path, codes));
        }
        for (path, bytes) in files {
            if !self.dir.join(path).is_file() {
                write(&self.dir.join(path), bytes)?;
            }
        }
        let stored_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let conn = self.conn.lock().unwrap();
        let previous: Option<(String, String)> = conn
            .query_row(
                "SELECT texture, azimuth_lut FROM frames WHERE id = ?1",
                params![frame.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        conn.execute(
            "INSERT OR REPLACE INTO frames (id, site, product, elevation_deg, start_ms, scan_time,
                 sweep_end, provenance, stored_ms, frame, texture, azimuth_lut)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                frame.id,
                site,
                frame.product,
                frame.elevation_deg,
                start_ms,
                frame.scan_time,
                frame.sweep_end,
                provenance,
                stored_ms,
                serde_json::to_string(&record)?,
                texture_path,
                lut_path,
            ],
        )
        .map_err(sql)?;
        let mut stale: Vec<String> = previous
            .map(|(t, l)| with_codes(t, l))
            .unwrap_or_default()
            .into_iter()
            .filter(|p| *p != texture_path && *p != lut_path && *p != codes_path)
            .collect();
        // The ring: everything past the newest RING for this station goes.
        let expired: Vec<(String, String, String)> = conn
            .prepare(
                "SELECT id, texture, azimuth_lut FROM frames WHERE site = ?1
                 ORDER BY start_ms DESC, id DESC LIMIT -1 OFFSET ?2",
            )
            .map_err(sql)?
            .query_map(params![site, RING as i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        for (id, texture, lut) in expired {
            conn.execute("DELETE FROM frames WHERE id = ?1", params![id])
                .map_err(sql)?;
            stale.extend(with_codes(texture, lut));
        }
        for path in stale {
            match fs::remove_file(self.dir.join(path)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Entry {
            id: frame.id.clone(),
            scan_time: frame.scan_time.clone(),
            start_ms,
            record: Some(Record {
                codes: (!codes.is_empty()).then(|| self.dir.join(&codes_path)),
                frame: record,
                tag: tag_of(&self.dir, &texture_path),
                texture: self.dir.join(texture_path),
                azimuth_lut: self.dir.join(lut_path),
            }),
        })
    }

    /// The station's frames, oldest first: the ring's contents, at most
    /// `RING`, as `state.timeline` lists them, each with its record.
    pub fn list(&self, site: &str) -> io::Result<Vec<Entry>> {
        type Row = (String, String, i64, String, String, String);
        let rows: Vec<Row> = self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT id, scan_time, start_ms, frame, texture, azimuth_lut FROM frames
                 WHERE site = ?1 ORDER BY start_ms DESC, id DESC LIMIT ?2",
            )
            .map_err(sql)?
            .query_map(params![site, RING as i64], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        let mut entries: Vec<Entry> = rows
            .into_iter()
            .map(|(id, scan_time, start_ms, frame, texture, lut)| {
                let record = match serde_json::from_str::<Frame>(&frame) {
                    Ok(frame) => Some(Record {
                        codes: codes_of(&texture)
                            .filter(|_| frame.kind == FrameKind::Grid)
                            .map(|c| self.dir.join(c))
                            .filter(|p| p.is_file()),
                        frame,
                        tag: tag_of(&self.dir, &texture),
                        texture: self.dir.join(texture),
                        azimuth_lut: self.dir.join(lut),
                    }),
                    Err(e) => {
                        eprintln!("Frame catalog: {id}: {e}");
                        None
                    }
                };
                Entry {
                    id,
                    scan_time,
                    start_ms,
                    record,
                }
            })
            .collect();
        entries.reverse();
        Ok(entries)
    }

    /// The stored frame `id` with its textures, if the ring still has it.
    #[cfg(test)]
    pub fn load(&self, id: &str) -> io::Result<Option<Stored>> {
        let row: Option<(String, i64, String, String)> = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT frame, start_ms, texture, azimuth_lut FROM frames WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(sql)?;
        let Some((frame, start_ms, texture, lut)) = row else {
            return Ok(None);
        };
        Ok(Some(Stored {
            frame: serde_json::from_str(&frame)?,
            start_ms,
            texture: fs::read(self.dir.join(texture))?,
            azimuth_lut: fs::read(self.dir.join(lut))?,
        }))
    }

    /// How many frames the station has.
    #[cfg(test)]
    pub fn count(&self, site: &str) -> io::Result<usize> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM frames WHERE site = ?1",
                params![site],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .map_err(sql)
    }
}

/// Write by temp-and-rename so a crash leaves no half file under a real name.
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{FrameKind, FrameStatus, Geometry};

    fn frame(site: &str, minute: u32) -> Frame {
        Frame {
            id: format!("{site}-20260906T12{minute:02}00Z-e0"),
            kind: FrameKind::Polar,
            product: "REF".into(),
            product_name: "Reflectivity".into(),
            units: "dBZ".into(),
            elevation_deg: 0.5,
            scan_time: format!("2026-09-06T12:{minute:02}:00Z"),
            sweep_end: format!("2026-09-06T12:{minute:02}:20Z"),
            status: FrameStatus::Complete,
            texture: "tex/runtime-path.png".into(),
            azimuth_lut: "tex/runtime-lut.png".into(),
            rays: 720,
            gates: 1832,
            first_gate_m: 2125,
            gate_spacing_m: 250,
            scale: 2.0,
            offset: 66.0,
            site: Geometry {
                lat: 35.0,
                lon: -97.0,
                alt_m: 380.0,
            },
            palette: vec!["#000000".into()],
            bounds: vec![0, 10],
            attribution: "SMHI, CC BY 4.0".into(),
            grid: None,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("omastorm-catalog-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_ring_keeps_the_newest_frames_and_their_files() {
        let dir = scratch("ring");
        let catalog = Catalog::open(dir.clone()).unwrap();
        assert!(catalog.list("KTLX").unwrap().is_empty());
        for minute in 0..(RING as u32 + 3) {
            let f = frame("KTLX", minute);
            catalog
                .store(
                    "KTLX",
                    &f,
                    1_757_160_000_000 + i64::from(minute) * 60_000,
                    &[minute as u8; 16],
                    &[1, 2, 3],
                    "unidata-nexrad-level2-chunks/KTLX/001/a..b",
                )
                .unwrap();
        }
        // Another station's ring is its own.
        catalog
            .store("KAMX", &frame("KAMX", 5), 1, &[9], &[9], "p")
            .unwrap();
        assert_eq!(catalog.count("KTLX").unwrap(), RING);
        assert_eq!(catalog.count("KAMX").unwrap(), 1);
        let listed = catalog.list("KTLX").unwrap();
        let newest = catalog.load(&listed[RING - 1].id).unwrap().unwrap();
        assert_eq!(newest.frame.id, frame("KTLX", RING as u32 + 2).id);
        // Runtime paths are not stored; the caller publishes.
        assert_eq!(newest.frame.texture, "");
        assert_eq!(newest.frame.azimuth_lut, "");
        assert_eq!(newest.texture, [(RING as u8 + 2); 16]);
        assert_eq!(newest.azimuth_lut, [1, 2, 3]);
        assert_eq!(
            newest.start_ms,
            1_757_160_000_000 + (RING as i64 + 2) * 60_000
        );
        let files = files(&dir.join("KTLX"));
        assert_eq!(
            files.len(),
            RING * 2,
            "one sweep and one lookup per kept frame"
        );
        assert!(
            !files
                .iter()
                .any(|f| f.contains("T120000Z") || f.contains("T120200Z"))
        );
        assert!(files.iter().any(|f| f.contains("T120300Z")));
        // The listing is the ring oldest first; a frame loads by id until it
        // falls off the ring.
        assert_eq!(listed.len(), RING);
        assert_eq!(listed[0].id, frame("KTLX", 3).id);
        assert_eq!(listed[0].scan_time, "2026-09-06T12:03:00Z");
        assert_eq!(listed[0].start_ms, 1_757_160_000_000 + 3 * 60_000);
        assert_eq!(listed[RING - 1].id, newest.frame.id);
        assert!(listed.windows(2).all(|w| w[0].start_ms < w[1].start_ms));
        let loaded = catalog.load(&listed[1].id).unwrap().unwrap();
        assert_eq!(loaded.frame.id, frame("KTLX", 4).id);
        assert_eq!(loaded.texture, [4; 16]);
        assert!(catalog.load(&frame("KTLX", 1).id).unwrap().is_none());
        assert_eq!(catalog.list("KAMX").unwrap().len(), 1);
        assert!(catalog.list("KOUN").unwrap().is_empty());
        // Every listed frame names its files, which hold its bytes.
        let record = listed[1].record.as_ref().unwrap();
        assert_eq!(fs::read(&record.texture).unwrap(), [4; 16]);
        assert_eq!(fs::read(&record.azimuth_lut).unwrap(), [1, 2, 3]);
        assert_eq!(record.frame.id, listed[1].id);
        assert_eq!(record.tag.len(), 8);
        // Reopening sees the same rows and the same tags.
        drop(catalog);
        let again = Catalog::open(dir.clone()).unwrap();
        assert_eq!(again.count("KTLX").unwrap(), RING);
        assert_eq!(again.list("KTLX").unwrap(), listed);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_frame_stored_again_keeps_its_name_unless_its_bytes_change() {
        let dir = scratch("again");
        let catalog = Catalog::open(dir.clone()).unwrap();
        let f = frame("vara", 5);
        let first = catalog.store("vara", &f, 5, &[7; 32], &[1], "a").unwrap();
        let before = files(&dir.join("vara"));
        let modified = fs::metadata(&first.record.as_ref().unwrap().texture)
            .unwrap()
            .modified()
            .unwrap();
        // The same volume again (a catalogued replay): same name, nothing written.
        let replay = catalog.store("vara", &f, 5, &[7; 32], &[1], "b").unwrap();
        assert_eq!(replay.record, first.record);
        assert_eq!(files(&dir.join("vara")), before);
        assert_eq!(
            fs::metadata(&replay.record.as_ref().unwrap().texture)
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
        // Other bytes for the same frame: a new tag, and the old files go.
        let changed = catalog.store("vara", &f, 5, &[8; 32], &[1], "c").unwrap();
        let (old, new) = (first.record.unwrap(), changed.record.unwrap());
        assert_ne!(old.tag, new.tag);
        assert!(!old.texture.exists() && !old.azimuth_lut.exists());
        assert_eq!(fs::read(&new.texture).unwrap(), [8; 32]);
        assert_eq!(files(&dir.join("vara")).len(), 2);
        assert_eq!(catalog.list("vara").unwrap()[0].record.as_ref(), Some(&new));
        let _ = fs::remove_dir_all(&dir);
    }

    fn grid_pngs(codes: &[u8]) -> Pngs<'_> {
        Pngs {
            texture: &[5; 8],
            azimuth_lut: &[],
            codes,
        }
    }

    #[test]
    fn a_grid_keeps_its_code_texture_beside_its_texture() {
        let dir = scratch("codes");
        let catalog = Catalog::open(dir.clone()).unwrap();
        let mut grid = frame("sweden", 5);
        grid.kind = FrameKind::Grid;
        let stored = catalog
            .store_with_codes("sweden", &grid, 5, grid_pngs(&[2, 3]), "a")
            .unwrap();
        let codes = stored.record.as_ref().unwrap().codes.clone().unwrap();
        assert_eq!(fs::read(&codes).unwrap(), [2, 3]);
        assert!(codes.to_str().unwrap().ends_with("-codes.png"));
        assert_eq!(catalog.list("sweden").unwrap()[0].record, stored.record);
        // A polar frame lists none.
        let polar = catalog
            .store("vara", &frame("vara", 5), 5, &[1], &[2], "a")
            .unwrap();
        assert_eq!(polar.record.unwrap().codes, None);
        assert_eq!(
            catalog.list("vara").unwrap()[0]
                .record
                .as_ref()
                .unwrap()
                .codes,
            None
        );
        // The ring takes the code texture with its frame.
        for minute in 6..(6 + RING as u32) {
            let mut g = frame("sweden", minute);
            g.kind = FrameKind::Grid;
            catalog
                .store_with_codes("sweden", &g, i64::from(minute), grid_pngs(&[7]), "b")
                .unwrap();
        }
        assert!(!codes.exists());
        assert_eq!(files(&dir.join("sweden")).len(), RING * 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pre_s19_file_gets_a_tag_from_its_size_and_time() {
        let dir = scratch("legacy");
        let catalog = Catalog::open(dir.clone()).unwrap();
        let f = frame("vara", 5);
        let stored = catalog.store("vara", &f, 5, &[7; 32], &[1], "a").unwrap();
        let record = stored.record.unwrap();
        // Rename the files to the pre-S19 scheme, as an older engine left them.
        let legacy = |suffix: &str| format!("vara/{}-{suffix}.png", f.id);
        fs::rename(&record.texture, dir.join(legacy("sweep"))).unwrap();
        fs::rename(&record.azimuth_lut, dir.join(legacy("azlut"))).unwrap();
        catalog
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE frames SET texture = ?1, azimuth_lut = ?2",
                params![legacy("sweep"), legacy("azlut")],
            )
            .unwrap();
        let tag = catalog.list("vara").unwrap()[0].record.clone().unwrap().tag;
        assert_eq!(tag.len(), 8);
        assert_ne!(tag, record.tag);
        assert_eq!(
            catalog.list("vara").unwrap()[0]
                .record
                .as_ref()
                .unwrap()
                .tag,
            tag,
            "stable while the file is unchanged"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
