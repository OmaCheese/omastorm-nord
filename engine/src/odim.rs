//! ODIM_H5 decoding for SMHI polar volumes (`qcvol`, ODIM_H5 2.2 `PVOL`):
//! the lowest tilt's DBZH as the same `Sweep` the Level II path produces, so
//! the texture, the azimuth lookup, the frame and the UI stay unchanged
//! (`docs/protocol.md`, sweep texture).
//!
//! Only `/dataset1` is read. SMHI stores its tilts in ascending elevation,
//! and touching no other dataset keeps a ranged read of a 15 MB volume to a
//! few requests and about 100 KB (DEC-2). DBZH is found by its
//! `dataN/what/quantity`, never by index.
//!
//! The int16 values are requantized to the NEXRAD byte convention, so the
//! weak-return floor and the palette bounds work as they do for Level II:
//! `code = round(dBZ × 2 + 66)` clamped to 2..255, `scale` 2, `offset` 66.
//! `undetect` becomes code 0 (below threshold) and `nodata` code 1, which
//! this sweep's texture marks as outside coverage rather than range folded.
//!
//! Rays are sorted by the midpoint of `how/startazA` and `how/stopazA`; the
//! file's rows are already in azimuth order, but `a1gate` shows the scan
//! started mid-circle, so row order says nothing about time.

use crate::sweep::{OUTSIDE_COVERAGE, Ray, Sweep};
use chrono::NaiveDateTime;
use hdf5_pure::{AttrValue, File, ReadSeekSource};
use std::collections::HashMap;
use std::fmt;
use std::io::{Read, Seek};

/// Measured value = (code - offset) / scale, the NEXRAD reflectivity encoding.
pub const SCALE: f32 = 2.0;
pub const OFFSET: f32 = 66.0;

/// The HDF5 format signature, at offset 0 in every SMHI volume.
const HDF5_SIGNATURE: &[u8; 8] = b"\x89HDF\r\n\x1a\n";

/// Why a volume could not be decoded, with the ODIM path involved.
#[derive(Debug)]
pub struct OdimError(String);

impl fmt::Display for OdimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OdimError {}

fn fail(what: impl fmt::Display) -> OdimError {
    OdimError(what.to_string())
}

/// The radar that wrote the volume: the root `/what` `source` string (for
/// example `WMO:02600,RAD:SE49,PLC:Vara,NOD:sevax,…`) and the root `/where`
/// position, height in metres above sea level.
#[derive(Debug, Clone, PartialEq)]
pub struct OdimSite {
    pub source: String,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: f64,
}

impl OdimSite {
    /// One `KEY:value` item of `source`, such as `PLC` (place) or `NOD`.
    pub fn source_item(&self, key: &str) -> Option<&str> {
        self.source.split(',').find_map(|item| {
            let (k, v) = item.split_once(':')?;
            (k.trim() == key).then(|| v.trim())
        })
    }
}

/// Whether `bytes` start like an HDF5 file (and so, here, an ODIM volume).
pub fn is_odim(bytes: &[u8]) -> bool {
    bytes.starts_with(HDF5_SIGNATURE)
}

/// Decode the lowest tilt's DBZH from an ODIM volume. `reader` may be a
/// file, a buffer, or a ranged HTTP reader: only the byte ranges holding the
/// root metadata and `/dataset1` are read.
#[allow(
    dead_code,
    reason = "the SMHI live poller (smhi_live.rs) calls it; archive mode needs the site too"
)]
pub fn decode_lowest_dbzh<R: Read + Seek + Send + 'static>(reader: R) -> Result<Sweep, OdimError> {
    lowest_dbzh(&open(reader)?)
}

/// `decode_lowest_dbzh`, plus the site the volume names.
pub fn decode_lowest_dbzh_with_site<R: Read + Seek + Send + 'static>(
    reader: R,
) -> Result<(Sweep, OdimSite), OdimError> {
    let file = open(reader)?;
    Ok((lowest_dbzh(&file)?, site(&file)?))
}

fn open<R: Read + Seek + Send + 'static>(reader: R) -> Result<File, OdimError> {
    let source =
        ReadSeekSource::new(reader).map_err(|e| fail(format!("reading the volume: {e}")))?;
    File::from_source(source).map_err(|e| fail(format!("opening the volume as HDF5: {e}")))
}

type Attrs = HashMap<String, AttrValue>;

fn attrs(file: &File, path: &str) -> Result<Attrs, OdimError> {
    let group = file.group(path).map_err(|e| fail(format!("{path}: {e}")))?;
    group
        .attrs()
        .map_err(|e| fail(format!("{path} attributes: {e}")))
}

fn num(attrs: &Attrs, key: &str) -> Option<f64> {
    Some(match attrs.get(key)? {
        AttrValue::F32(v) => f64::from(*v),
        AttrValue::F64(v) => *v,
        AttrValue::I8(v) => f64::from(*v),
        AttrValue::I16(v) => f64::from(*v),
        AttrValue::I32(v) => f64::from(*v),
        AttrValue::I64(v) => *v as f64,
        AttrValue::U8(v) => f64::from(*v),
        AttrValue::U16(v) => f64::from(*v),
        AttrValue::U32(v) => f64::from(*v),
        AttrValue::U64(v) => *v as f64,
        _ => return None,
    })
}

fn need(attrs: &Attrs, path: &str, key: &str) -> Result<f64, OdimError> {
    num(attrs, key).ok_or_else(|| fail(format!("{path}: no numeric {key}")))
}

/// A per-ray array of `how`, when present with one value per ray.
fn per_ray(attrs: &Attrs, key: &str, rays: usize) -> Option<Vec<f64>> {
    let values = match attrs.get(key)? {
        AttrValue::F64Array(v) => v.clone(),
        AttrValue::F32Array(v) => v.iter().map(|&x| f64::from(x)).collect(),
        _ => return None,
    };
    (values.len() == rays).then_some(values)
}

fn text(attrs: &Attrs, key: &str) -> Option<String> {
    let value = match attrs.get(key)? {
        AttrValue::String(s)
        | AttrValue::AsciiString(s)
        | AttrValue::VarLenAsciiString(s)
        | AttrValue::VarLenString(s)
        | AttrValue::AsciiStringSized { value: s, .. }
        | AttrValue::StringSized { value: s, .. } => s,
        _ => return None,
    };
    Some(value.trim_end_matches('\0').trim().to_owned())
}

/// A nominal `date` + `time` pair of a `what` group, in epoch milliseconds.
fn nominal_ms(what: &Attrs, date: &str, time: &str) -> Option<i64> {
    let stamp = format!("{}{}", text(what, date)?, text(what, time)?);
    let parsed = NaiveDateTime::parse_from_str(&stamp, "%Y%m%d%H%M%S").ok()?;
    Some(parsed.and_utc().timestamp_millis())
}

/// Seconds since the epoch as whole milliseconds.
fn millis(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

/// One ODIM value as a NEXRAD-convention reflectivity byte.
fn requantize(raw: i16, gain: f64, offset: f64, nodata: f64, undetect: f64) -> u8 {
    let value = f64::from(raw);
    if value == undetect {
        0
    } else if value == nodata {
        1
    } else {
        let dbz = value * gain + offset;
        (dbz * f64::from(SCALE) + f64::from(OFFSET))
            .round()
            .clamp(2.0, 255.0) as u8
    }
}

fn lowest_dbzh(file: &File) -> Result<Sweep, OdimError> {
    const DS: &str = "/dataset1";
    let where_ = attrs(file, &format!("{DS}/where"))?;
    let dwhat = attrs(file, &format!("{DS}/what")).unwrap_or_default();
    let how = attrs(file, &format!("{DS}/how")).unwrap_or_default();
    let (data, what) = (1..)
        .map_while(|m| {
            let path = format!("{DS}/data{m}");
            attrs(file, &format!("{path}/what")).ok().map(|a| (path, a))
        })
        .find(|(_, a)| text(a, "quantity").as_deref() == Some("DBZH"))
        .ok_or_else(|| fail(format!("{DS} has no DBZH")))?;

    let wpath = format!("{DS}/where");
    let rays = need(&where_, &wpath, "nrays")? as usize;
    let bins = need(&where_, &wpath, "nbins")? as usize;
    let rscale = need(&where_, &wpath, "rscale")?;
    let rstart = need(&where_, &wpath, "rstart")?;
    let elangle = need(&where_, &wpath, "elangle")?;
    // ODIM allows the encoding in the dataset's `what` when the data's lacks it.
    let coding = |key: &str| {
        num(&what, key)
            .or_else(|| num(&dwhat, key))
            .ok_or_else(|| fail(format!("{data}/what: no {key}")))
    };
    let (gain, offset) = (coding("gain")?, coding("offset")?);
    let (nodata, undetect) = (coding("nodata")?, coding("undetect")?);
    let gates = u16::try_from(bins).map_err(|_| fail(format!("{wpath}: {bins} bins")))?;
    if rays == 0 || gates == 0 {
        return Err(fail(format!("{wpath}: {rays} rays of {bins} bins")));
    }

    let values = file
        .dataset(&format!("{data}/data"))
        .and_then(|d| d.read_i16())
        .map_err(|e| fail(format!("{data}/data: {e}")))?;
    if values.len() != rays * bins {
        return Err(fail(format!(
            "{data}/data holds {} values, not {rays} × {bins}",
            values.len()
        )));
    }

    // Ray centres from the start and stop azimuths, across the 360° wrap;
    // without them ODIM's rows are equal sectors clockwise from north.
    let centers: Vec<f64> = match (
        per_ray(&how, "startazA", rays),
        per_ray(&how, "stopazA", rays),
    ) {
        (Some(start), Some(stop)) => start
            .iter()
            .zip(&stop)
            .map(|(a, b)| (a + (b - a).rem_euclid(360.0) / 2.0).rem_euclid(360.0))
            .collect(),
        _ => (0..rays)
            .map(|i| (i as f64 + 0.5) * 360.0 / rays as f64)
            .collect(),
    };
    let elevations = per_ray(&how, "elangles", rays).unwrap_or_else(|| vec![elangle; rays]);
    let (ray_ms, start_ms, end_ms) = match (
        per_ray(&how, "startazT", rays),
        per_ray(&how, "stopazT", rays),
    ) {
        (Some(start), Some(stop)) => {
            let ray_ms: Vec<i64> = start.iter().map(|&t| millis(t)).collect();
            let first = ray_ms.iter().copied().min().unwrap_or_default();
            let last = stop.iter().map(|&t| millis(t)).max().unwrap_or(first);
            (ray_ms, first, last)
        }
        _ => {
            let first = nominal_ms(&dwhat, "startdate", "starttime")
                .ok_or_else(|| fail(format!("{DS}: no ray times and no start time")))?;
            let last = nominal_ms(&dwhat, "enddate", "endtime").unwrap_or(first);
            (vec![first; rays], first, last)
        }
    };

    let mut order: Vec<usize> = (0..rays).collect();
    order.sort_by(|&a, &b| centers[a].total_cmp(&centers[b]));
    let rays = order
        .into_iter()
        .map(|r| Ray {
            azimuth_deg: centers[r] as f32,
            elevation_deg: elevations[r] as f32,
            time_ms: ray_ms[r],
            codes: values[r * bins..(r + 1) * bins]
                .iter()
                .map(|&v| requantize(v, gain, offset, nodata, undetect))
                .collect(),
        })
        .collect();
    Ok(Sweep {
        rays,
        start_ms,
        end_ms,
        gates,
        first_gate_m: (rstart * 1000.0 + rscale / 2.0).round() as u32,
        gate_spacing_m: rscale.round() as u32,
        scale: SCALE,
        offset: OFFSET,
        code1_status: OUTSIDE_COVERAGE,
    })
}

fn site(file: &File) -> Result<OdimSite, OdimError> {
    let where_ = attrs(file, "/where")?;
    let what = attrs(file, "/what")?;
    Ok(OdimSite {
        source: text(&what, "source").unwrap_or_default(),
        lat: need(&where_, "/where", "lat")?,
        lon: need(&where_, "/where", "lon")?,
        alt_m: need(&where_, "/where", "height")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sweep::BELOW_THRESHOLD;
    use serde::Deserialize;
    use sha2::{Digest, Sha256};
    use std::io::{self, Cursor, SeekFrom};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::{fs, time::Instant};

    const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../golden/vara-20260913/");
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../data/raw/radar_vara_qcvol_202609131055.h5"
    );

    /// `golden/vara-20260913/sweep0.json`, written by h5py (`produce.py`).
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Golden {
        rays: usize,
        gates: u16,
        scale: f32,
        offset: f32,
        first_gate_m: f64,
        gate_spacing_m: f64,
        azimuth_deg: Vec<f64>,
        elevation_deg: Vec<f64>,
        ray_time_base: String,
        ray_time_base_ms: i64,
        ray_time_ms: Vec<i64>,
        sweep_end_ms: i64,
        site_lat: f64,
        site_lon: f64,
        site_alt_m: f64,
        counts: Counts,
        source_sha256: String,
        source_bytes: usize,
    }

    #[derive(Deserialize)]
    struct Counts {
        undetect: usize,
        nodata: usize,
        measured: usize,
    }

    fn golden() -> (Golden, Vec<u8>) {
        let golden: Golden =
            serde_json::from_slice(&fs::read(format!("{GOLDEN}sweep0.json")).unwrap()).unwrap();
        (golden, fs::read(format!("{GOLDEN}sweep0.u8")).unwrap())
    }

    /// Four decimals, the way the golden angles were written.
    fn four(value: f64) -> String {
        format!("{value:.4}")
    }

    #[test]
    fn lowest_dbzh_matches_the_h5py_golden_files_exactly() {
        let (golden, codes) = golden();
        let volume = fs::read(FIXTURE).unwrap();
        assert_eq!(volume.len(), golden.source_bytes);
        assert_eq!(
            format!("{:x}", Sha256::digest(&volume)),
            golden.source_sha256
        );
        assert!(is_odim(&volume));
        let started = Instant::now();
        let (sweep, site) = decode_lowest_dbzh_with_site(fs::File::open(FIXTURE).unwrap()).unwrap();
        eprintln!("decoded lowest DBZH in {:?}", started.elapsed());

        assert_eq!(sweep.rays.len(), golden.rays);
        assert_eq!(sweep.gates, golden.gates);
        assert_eq!(sweep.scale, golden.scale);
        assert_eq!(sweep.offset, golden.offset);
        assert_eq!(f64::from(sweep.first_gate_m), golden.first_gate_m);
        assert_eq!(f64::from(sweep.gate_spacing_m), golden.gate_spacing_m);
        assert_eq!(sweep.code1_status, OUTSIDE_COVERAGE);
        assert_eq!(golden.ray_time_base, "2026-09-13T10:55:03Z");
        assert_eq!(sweep.start_ms, golden.ray_time_base_ms);
        assert_eq!(sweep.end_ms, golden.sweep_end_ms);
        let gates = usize::from(sweep.gates);
        for (row, ray) in sweep.rays.iter().enumerate() {
            assert_eq!(
                four(f64::from(ray.azimuth_deg)),
                four(golden.azimuth_deg[row]),
                "azimuth of row {row}"
            );
            assert_eq!(
                four(f64::from(ray.elevation_deg)),
                four(golden.elevation_deg[row]),
                "elevation of row {row}"
            );
            assert_eq!(
                ray.time_ms - sweep.start_ms,
                golden.ray_time_ms[row],
                "time of row {row}"
            );
            assert!(
                ray.codes == codes[row * gates..(row + 1) * gates],
                "DBZH bytes of row {row}"
            );
        }
        assert_eq!(codes.len(), golden.rays * gates);
        let count = |f: fn(u8) -> bool| codes.iter().filter(|&&c| f(c)).count();
        assert_eq!(count(|c| c == 0), golden.counts.undetect);
        assert_eq!(count(|c| c == 1), golden.counts.nodata);
        assert_eq!(count(|c| c >= 2), golden.counts.measured);

        // The file holds lon 12.826024055480957 exactly, and so does the
        // golden text, but serde_json's default float parser lands one ulp
        // off; compare the site to well below a millimetre instead.
        for (ours, theirs) in [
            (site.lat, golden.site_lat),
            (site.lon, golden.site_lon),
            (site.alt_m, golden.site_alt_m),
        ] {
            assert!((ours - theirs).abs() < 1e-12, "{ours} != {theirs}");
        }
        assert_eq!(site.source_item("PLC"), Some("Vara"));
        assert_eq!(site.source_item("NOD"), Some("sevax"));
        // The geometry the plan fixes for SMHI (`firstGateM` = rstart·1000 + rscale/2).
        assert_eq!((sweep.first_gate_m, sweep.gate_spacing_m), (250, 500));
        assert_eq!((sweep.elevation_deg() * 100.0).round() / 100.0, 0.5);
    }

    #[test]
    fn texture_marks_nodata_outside_coverage_and_needs_no_blank_row() {
        let mut sweep = decode_lowest_dbzh(fs::File::open(FIXTURE).unwrap()).unwrap();
        // The fixture has no nodata gates; the last gate of every ray stands in.
        let last = usize::from(sweep.gates) - 1;
        for ray in &mut sweep.rays {
            ray.codes[last] = 1;
        }
        let bounds = [-32, 0, 10, 20, 30, 40, 45, 50, 55, 60, 65, 70, 96];
        let pixels = sweep.texture(&bounds, 12);
        let gates = usize::from(sweep.gates);
        assert_eq!(sweep.rows(), 360, "a complete 1° cut needs no blank row");
        assert_eq!(pixels.len(), 360 * gates * 4);
        let mut measured = 0;
        for (i, px) in pixels.as_chunks::<4>().0.iter().enumerate() {
            let code = sweep.rays[i / gates].codes[i % gates];
            assert_eq!([px[2], px[3]], [code, 255]);
            match code {
                0 => assert_eq!(px[..2], [0, BELOW_THRESHOLD]),
                1 => assert_eq!(px[..2], [0, OUTSIDE_COVERAGE]),
                _ => {
                    measured += 1;
                    let value = (f32::from(code) - OFFSET) / SCALE;
                    let class = usize::from(px[0] - 1);
                    assert!(bounds[class] as f32 <= value && value < bounds[class + 1] as f32);
                }
            }
        }
        assert!(measured > 3000, "{measured} measured gates");
        let mut used = vec![false; 360];
        for (entry, px) in sweep.azimuth_lut().as_chunks::<4>().0.iter().enumerate() {
            let row = usize::from(u16::from_le_bytes([px[0], px[1]]));
            used[row] = true;
            let d = (sweep.rays[row].azimuth_deg - (entry as f32 + 0.5) / 10.0).abs();
            assert!(d.min(360.0 - d) <= 0.55, "entry {entry} maps to row {row}");
        }
        assert!(used.iter().all(|&u| u), "every ray owns lookup entries");
    }

    /// A `Read + Seek` that counts what the decoder pulls, standing in for
    /// the live poller's ranged HTTP reader.
    struct Counting {
        inner: Cursor<Vec<u8>>,
        bytes: Arc<AtomicU64>,
        reads: Arc<AtomicU64>,
    }

    impl Read for Counting {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.bytes.fetch_add(n as u64, Ordering::Relaxed);
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(n)
        }
    }

    impl Seek for Counting {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    /// DEC-2's budget holds for this decoder, not just the spike: the lowest
    /// DBZH of a 14.7 MB volume needs well under 300 KB of it.
    #[test]
    fn decoding_reads_a_small_fraction_of_the_volume() {
        let (bytes, reads) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let reader = Counting {
            inner: Cursor::new(fs::read(FIXTURE).unwrap()),
            bytes: bytes.clone(),
            reads: reads.clone(),
        };
        let sweep = decode_lowest_dbzh(reader).unwrap();
        assert_eq!(sweep.rays.len(), 360);
        let (bytes, reads) = (bytes.load(Ordering::Relaxed), reads.load(Ordering::Relaxed));
        eprintln!("read {bytes} bytes in {reads} reads");
        assert!(bytes < 300_000, "{bytes} bytes read");
    }

    #[test]
    fn other_input_is_an_error_not_a_panic() {
        let ktlx = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../data/raw/KTLX20130520_201643_V06.gz"
        );
        let head = fs::read(ktlx).unwrap()[..4096].to_vec();
        assert!(!is_odim(&head));
        assert!(decode_lowest_dbzh(Cursor::new(head)).is_err());
        assert!(decode_lowest_dbzh(Cursor::new(Vec::new())).is_err());
        let truncated = fs::read(FIXTURE).unwrap()[..65536].to_vec();
        assert!(decode_lowest_dbzh(Cursor::new(truncated)).is_err());
    }

    #[test]
    fn requantizes_like_the_nexrad_convention() {
        let (gain, offset, nodata, undetect) = (0.01, 0.0, -32768.0, -32767.0);
        let q = |raw| requantize(raw, gain, offset, nodata, undetect);
        assert_eq!(q(-32767), 0);
        assert_eq!(q(-32768), 1);
        assert_eq!(q(0), 66); // 0 dBZ
        assert_eq!(q(2000), 106); // 20 dBZ
        assert_eq!(q(-3200), 2); // -32 dBZ, the floor code
        assert_eq!(q(-5000), 2); // clamped up
        assert_eq!(q(9500), 255); // 95 dBZ -> 256, clamped down
    }
}
