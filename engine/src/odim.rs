//! ODIM_H5 decoding for polar volumes and scans: SMHI's `qcvol` (ODIM_H5 2.2
//! `PVOL`, S2) and, since S15, the Norwegian, Finnish and Danish files of
//! EUMETNET Open Radar Data (DEC-13). The lowest tilt's reflectivity becomes
//! the same `Sweep` the Level II path produces, so the texture, the azimuth
//! lookup, the frame and the UI stay unchanged (`docs/protocol.md`, sweep
//! texture).
//!
//! What varies between the sources, and how it is read (each proven by a
//! golden file under `golden/`, produced by h5py):
//!
//! - **Which tilt.** `Tilt::First` reads `/dataset1` only. SMHI stores its
//!   tilts in ascending elevation, and touching no other dataset keeps a
//!   ranged read of a 15 MB volume to a few requests and about 100 KB
//!   (DEC-2). `Tilt::Lowest` reads every `/datasetN/where` and takes the
//!   lowest `elangle` (ties: the lower N); the ORD provider uses it. All 29
//!   ORD radars store ascending too (measured 2026-09-14), but the files do
//!   not promise it.
//! - **Which quantity.** `DBZH` by its `dataN/what/quantity`, never by
//!   index, else `TH` (`REFLECTIVITY`). FMI's scans store `TH` first and
//!   `DBZH` second.
//! - **Storage.** Any integer or float type (SMHI int16, ORD uint8), read as
//!   `f64` (exact for every integer width) and requantized through
//!   `gain`/`offset` to the NEXRAD byte convention, so the weak-return floor
//!   and the palette bounds work as they do for Level II: `code = round(dBZ ×
//!   2 + 66)` clamped to 2..255, `scale` 2, `offset` 66. `undetect` becomes
//!   code 0 (below threshold) and `nodata` code 1, which this sweep's texture
//!   marks as outside coverage rather than range folded.
//! - **Ray azimuths.** The midpoint of `how/startazA` and `how/stopazA`
//!   (SMHI, FMI); else ODIM 2.0's `how/azangles` string of `start:stop`
//!   pairs, which DMI writes as `azangels`; else equal sectors clockwise
//!   from north (MET Norway's files carry no angles). `a1gate` says where
//!   the scan started in time, not in azimuth, so it changes nothing here.
//!   Rays are sorted by centre.
//! - **Ray times.** `how/startazT`/`stopazT` (SMHI), else the dataset's
//!   nominal `startdate`/`starttime` and `enddate`/`endtime` (ORD).
//! - **Range.** The first gate's centre is `rstart` (km) + `rscale` / 2:
//!   250 m for SMHI and FMI, 125 m for MET Norway, 750 m for DMI.
//! - **Other angles (S20).** `decode_tilts` reads every scan's `where`, lets
//!   `products::needed` pick the scans a product needs, and decodes those
//!   alike (`products.rs`).

use crate::products::{Tilt as ProductTilt, TiltInfo};
use crate::sweep::{OUTSIDE_COVERAGE, Ray, Sweep};
use chrono::NaiveDateTime;
use hdf5_pure::{AttrValue, File, ReadSeekSource};
use std::collections::HashMap;
use std::fmt;
use std::io::{Read, Seek};

/// Measured value = (code - offset) / scale, the NEXRAD reflectivity encoding.
pub const SCALE: f32 = 2.0;
pub const OFFSET: f32 = 66.0;

/// The quantities read, in order of preference: corrected reflectivity,
/// else total (uncorrected) reflectivity.
pub const REFLECTIVITY: [&str; 2] = ["DBZH", "TH"];

/// The HDF5 format signature, at offset 0 in every volume read here.
const HDF5_SIGNATURE: &[u8; 8] = b"\x89HDF\r\n\x1a\n";

/// Which dataset holds the tilt to read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tilt {
    /// `/dataset1`, touching no other dataset (SMHI, DEC-2).
    First,
    /// The lowest `elangle` of all `/datasetN` (ORD).
    Lowest,
}

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

/// Decode `/dataset1`'s reflectivity from an SMHI volume. `reader` may be a
/// file, a buffer, or a ranged HTTP reader: only the byte ranges holding the
/// root metadata and `/dataset1` are read.
#[allow(
    dead_code,
    reason = "the SMHI live poller (smhi_live.rs) calls it; archive mode needs the site too"
)]
pub fn decode_lowest_dbzh<R: Read + Seek + Send + 'static>(reader: R) -> Result<Sweep, OdimError> {
    decode_lowest(reader, Tilt::First)
}

/// Decode the reflectivity of the tilt `tilt` names.
pub fn decode_lowest<R: Read + Seek + Send + 'static>(
    reader: R,
    tilt: Tilt,
) -> Result<Sweep, OdimError> {
    lowest_reflectivity(&open(reader)?, tilt)
}

/// `/dataset1` as `decode_lowest(reader, Tilt::First)` reads it, touching
/// no other dataset (DEC-2), with its `where/elangle`: the tilt store
/// (`tilts.rs`, S27) keeps it by angle.
pub fn decode_first<R: Read + Seek + Send + 'static>(reader: R) -> Result<ProductTilt, OdimError> {
    let file = open(reader)?;
    let wpath = "/dataset1/where";
    let elangle = need(&attrs(&file, wpath)?, wpath, "elangle")?;
    Ok(ProductTilt {
        elangle,
        sweep: sweep_at(&file, "/dataset1")?,
    })
}

/// The lowest tilt's reflectivity, plus the site the volume names. Archive
/// mode reads a local file, so it looks at every tilt.
pub fn decode_lowest_dbzh_with_site<R: Read + Seek + Send + 'static>(
    reader: R,
) -> Result<(Sweep, OdimSite), OdimError> {
    let file = open(reader)?;
    Ok((lowest_reflectivity(&file, Tilt::Lowest)?, site(&file)?))
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

/// ODIM 2.0's per-ray `start:stop` pairs, one comma-separated string:
/// `how/azangles`, or `how/azangels` as DMI spells it. `None` unless every
/// one of `rays` pairs parses.
fn angle_pairs(how: &Attrs, rays: usize) -> Option<Vec<(f64, f64)>> {
    ["azangles", "azangels"].iter().find_map(|key| {
        let pairs: Option<Vec<(f64, f64)>> = text(how, key)?
            .split(',')
            .map(|pair| {
                let (a, b) = pair.split_once(':')?;
                Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
            })
            .collect();
        pairs.filter(|p| p.len() == rays)
    })
}

/// The first of `REFLECTIVITY` among `quantities`, as its index there.
fn reflectivity<S: AsRef<str>>(quantities: &[S]) -> Option<usize> {
    REFLECTIVITY
        .iter()
        .find_map(|want| quantities.iter().position(|q| q.as_ref() == *want))
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

/// One stored ODIM value as a NEXRAD-convention reflectivity byte.
fn requantize(value: f64, gain: f64, offset: f64, nodata: f64, undetect: f64) -> u8 {
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

/// The dataset `tilt` names: `/dataset1`, or the lowest `elangle` (ties:
/// the lower number).
fn tilt_path(file: &File, tilt: Tilt) -> Result<String, OdimError> {
    if tilt == Tilt::First {
        return Ok("/dataset1".to_owned());
    }
    let mut lowest: Option<(f64, String)> = None;
    for n in 1.. {
        let path = format!("/dataset{n}");
        let Ok(where_) = attrs(file, &format!("{path}/where")) else {
            break;
        };
        let elangle = need(&where_, &format!("{path}/where"), "elangle")?;
        if lowest.as_ref().is_none_or(|(low, _)| elangle < *low) {
            lowest = Some((elangle, path));
        }
    }
    lowest
        .map(|(_, path)| path)
        .ok_or_else(|| fail("the volume has no /dataset1"))
}

fn lowest_reflectivity(file: &File, tilt: Tilt) -> Result<Sweep, OdimError> {
    sweep_at(file, &tilt_path(file, tilt)?)
}

/// Every scan's geometry, then the reflectivity of the scans `select`
/// names, each with its `where/elangle` (S20, `products::needed`). Only the
/// `where` groups of the other scans are read, so a ranged read stays small.
pub fn decode_tilts<R: Read + Seek + Send + 'static>(
    reader: R,
    select: impl FnOnce(&[TiltInfo]) -> Vec<usize>,
) -> Result<Vec<ProductTilt>, OdimError> {
    let file = open(reader)?;
    let (mut paths, mut infos) = (Vec::new(), Vec::new());
    for n in 1.. {
        let path = format!("/dataset{n}");
        let wpath = format!("{path}/where");
        let Ok(where_) = attrs(&file, &wpath) else {
            break;
        };
        let rscale = need(&where_, &wpath, "rscale")?;
        let rstart = need(&where_, &wpath, "rstart")?;
        let bins = need(&where_, &wpath, "nbins")? as usize;
        infos.push(TiltInfo {
            elangle: need(&where_, &wpath, "elangle")?,
            // As `sweep_at` writes them into the sweep.
            first_gate_m: (rstart * 1000.0 + rscale / 2.0).round() as u32,
            gate_spacing_m: rscale.round() as u32,
            gates: u16::try_from(bins).map_err(|_| fail(format!("{wpath}: {bins} bins")))?,
        });
        paths.push(path);
    }
    if infos.is_empty() {
        return Err(fail("the volume has no /dataset1"));
    }
    select(&infos)
        .into_iter()
        .map(|k| {
            let path = paths.get(k).ok_or_else(|| fail(format!("no scan {k}")))?;
            Ok(ProductTilt {
                elangle: infos[k].elangle,
                sweep: sweep_at(&file, path)?,
            })
        })
        .collect()
}

/// The reflectivity of dataset `ds` (`/datasetN`).
fn sweep_at(file: &File, ds: &str) -> Result<Sweep, OdimError> {
    let where_ = attrs(file, &format!("{ds}/where"))?;
    let dwhat = attrs(file, &format!("{ds}/what")).unwrap_or_default();
    let how = attrs(file, &format!("{ds}/how")).unwrap_or_default();
    // The moments up to the first DBZH, which nothing later can beat
    // (`REFLECTIVITY`): SMHI stores DBZH first of 15, so a ranged read
    // touches one moment's header instead of all of them.
    let mut moments: Vec<(String, Attrs)> = Vec::new();
    for m in 1.. {
        let path = format!("{ds}/data{m}");
        let Ok(what) = attrs(file, &format!("{path}/what")) else {
            break;
        };
        let first_choice = text(&what, "quantity").as_deref() == Some(REFLECTIVITY[0]);
        moments.push((path, what));
        if first_choice {
            break;
        }
    }
    let quantities: Vec<String> = moments
        .iter()
        .map(|(_, a)| text(a, "quantity").unwrap_or_default())
        .collect();
    let (data, what) = reflectivity(&quantities)
        .map(|i| &moments[i])
        .ok_or_else(|| fail(format!("{ds} has no DBZH or TH, only {quantities:?}")))?;

    let wpath = format!("{ds}/where");
    let rays = need(&where_, &wpath, "nrays")? as usize;
    let bins = need(&where_, &wpath, "nbins")? as usize;
    let rscale = need(&where_, &wpath, "rscale")?;
    let rstart = need(&where_, &wpath, "rstart")?;
    let elangle = need(&where_, &wpath, "elangle")?;
    // ODIM allows the encoding in the dataset's `what` when the data's lacks it.
    let coding = |key: &str| {
        num(what, key)
            .or_else(|| num(&dwhat, key))
            .ok_or_else(|| fail(format!("{data}/what: no {key}")))
    };
    let (gain, offset) = (coding("gain")?, coding("offset")?);
    let (nodata, undetect) = (coding("nodata")?, coding("undetect")?);
    let gates = u16::try_from(bins).map_err(|_| fail(format!("{wpath}: {bins} bins")))?;
    if rays == 0 || gates == 0 {
        return Err(fail(format!("{wpath}: {rays} rays of {bins} bins")));
    }

    // Any integer or float storage, exactly (an f64 holds every value of
    // SMHI's int16 and ORD's uint8).
    let values = file
        .dataset(&format!("{data}/data"))
        .and_then(|d| d.read_f64())
        .map_err(|e| fail(format!("{data}/data: {e}")))?;
    if values.len() != rays * bins {
        return Err(fail(format!(
            "{data}/data holds {} values, not {rays} × {bins}",
            values.len()
        )));
    }

    // Ray centres from the start and stop azimuths, across the 360° wrap;
    // without them ODIM's rows are equal sectors clockwise from north.
    let pairs = match (
        per_ray(&how, "startazA", rays),
        per_ray(&how, "stopazA", rays),
    ) {
        (Some(start), Some(stop)) => Some(start.into_iter().zip(stop).collect::<Vec<_>>()),
        _ => angle_pairs(&how, rays),
    };
    let centers: Vec<f64> = match pairs {
        Some(pairs) => pairs
            .iter()
            .map(|(a, b)| (a + (b - a).rem_euclid(360.0) / 2.0).rem_euclid(360.0))
            .collect(),
        None => (0..rays)
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
                .ok_or_else(|| fail(format!("{ds}: no ray times and no start time")))?;
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

    const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../");
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../data/raw/radar_vara_qcvol_202609131055.h5"
    );

    /// `golden/<fixture>/sweep0.json`, written by h5py (`produce.py`,
    /// `produce-odim.py`).
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
        #[serde(default)]
        odim_dataset: String,
    }

    #[derive(Deserialize)]
    struct Counts {
        undetect: usize,
        nodata: usize,
        measured: usize,
    }

    /// Four decimals, the way the golden angles were written.
    fn four(value: f64) -> String {
        format!("{value:.4}")
    }

    /// Decode `data/raw/<file>` reading `tilt`, and hold every ray, gate,
    /// angle and time to `golden/<dir>/`. Returns what was decoded and the
    /// answer key.
    fn matches_golden(file: &str, dir: &str, tilt: Tilt) -> (Sweep, OdimSite, Golden) {
        let path = format!("{ROOT}data/raw/{file}");
        let golden: Golden =
            serde_json::from_slice(&fs::read(format!("{ROOT}golden/{dir}/sweep0.json")).unwrap())
                .unwrap();
        let codes = fs::read(format!("{ROOT}golden/{dir}/sweep0.u8")).unwrap();
        let volume = fs::read(&path).unwrap();
        assert_eq!(volume.len(), golden.source_bytes, "{file}");
        assert_eq!(
            format!("{:x}", Sha256::digest(&volume)),
            golden.source_sha256,
            "{file}"
        );
        assert!(is_odim(&volume));
        let started = Instant::now();
        let file_ = fs::File::open(&path).unwrap();
        let sweep = decode_lowest(file_, tilt).unwrap();
        eprintln!("{file}: decoded in {:?}", started.elapsed());
        let (again, site) = decode_lowest_dbzh_with_site(fs::File::open(&path).unwrap()).unwrap();
        assert_eq!(again.rays.len(), sweep.rays.len());

        assert_eq!(sweep.rays.len(), golden.rays);
        assert_eq!(sweep.gates, golden.gates);
        assert_eq!(sweep.scale, golden.scale);
        assert_eq!(sweep.offset, golden.offset);
        assert_eq!(f64::from(sweep.first_gate_m), golden.first_gate_m);
        assert_eq!(f64::from(sweep.gate_spacing_m), golden.gate_spacing_m);
        assert_eq!(sweep.code1_status, OUTSIDE_COVERAGE);
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
                "reflectivity bytes of row {row}"
            );
            // The archive path (every tilt) reads the same rows.
            assert!(again.rays[row].codes == ray.codes, "archive row {row}");
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
        (sweep, site, golden)
    }

    #[test]
    fn lowest_dbzh_matches_the_h5py_golden_files_exactly() {
        let (sweep, site, golden) = matches_golden(
            "radar_vara_qcvol_202609131055.h5",
            "vara-20260913",
            Tilt::First,
        );
        assert_eq!(golden.ray_time_base, "2026-09-13T10:55:03Z");
        assert_eq!(site.source_item("PLC"), Some("Vara"));
        assert_eq!(site.source_item("NOD"), Some("sevax"));
        // The geometry the plan fixes for SMHI (`firstGateM` = rstart·1000 + rscale/2).
        assert_eq!((sweep.first_gate_m, sweep.gate_spacing_m), (250, 500));
        assert_eq!((sweep.elevation_deg() * 100.0).round() / 100.0, 0.5);
    }

    /// MET Norway (Hurum): a DBZH-only PVOL of uint8, 720 rays of 960 gates
    /// of 250 m, no `startazA`/`startazT` at all (equal half-degree sectors,
    /// nominal times), `a1gate` 501, and a sweep that starts before the
    /// file's nominal 09:30.
    #[test]
    fn met_norway_hurum_matches_its_golden() {
        let (sweep, site, golden) =
            matches_golden("ord_nohur_202609140930.h5", "nohur-20260914", Tilt::Lowest);
        assert_eq!(golden.odim_dataset, "/dataset1/data1");
        assert_eq!((sweep.rays.len(), sweep.gates), (720, 960));
        assert_eq!((sweep.first_gate_m, sweep.gate_spacing_m), (125, 250));
        assert_eq!(sweep.rays[0].azimuth_deg, 0.25);
        assert_eq!(sweep.rays[719].azimuth_deg, 359.75);
        assert_eq!(golden.ray_time_base, "2026-09-14T09:29:06Z");
        assert_eq!(sweep.end_ms - sweep.start_ms, 60_000);
        assert_eq!(site.source_item("NOD"), Some("nohur"));
        assert_eq!(site.source_item("PLC"), None, "MET Norway names no place");
        assert!(golden.counts.measured > 50_000, "the fixture has echoes");
    }

    /// FMI (Korppoo): a single-sweep SCAN (ODIM 2.3) holding TH as data1 and
    /// DBZH as data2; `startazA`/`stopazA` but no ray times.
    #[test]
    fn fmi_korppoo_reads_dbzh_not_the_th_stored_first() {
        let (sweep, site, golden) =
            matches_golden("ord_fikor_202609140940.h5", "fikor-20260914", Tilt::Lowest);
        assert_eq!(golden.odim_dataset, "/dataset1/data2");
        assert_eq!((sweep.rays.len(), sweep.gates), (360, 500));
        assert_eq!((sweep.first_gate_m, sweep.gate_spacing_m), (250, 500));
        assert_eq!(golden.ray_time_base, "2026-09-14T09:40:01Z");
        assert_eq!(site.source_item("PLC"), Some("Korpo"));
        assert_eq!(site.source_item("NOD"), Some("fikor"));
    }

    /// DMI (Sindal): an ODIM 2.0 PVOL of eight quantities in 90 × 119
    /// chunks, ray angles only in the `how/azangels` string, and `rstart`
    /// 0.5 km.
    #[test]
    fn dmi_sindal_takes_its_angles_from_the_azangels_string() {
        let (sweep, site, golden) =
            matches_golden("ord_dksin_202609140940.h5", "dksin-20260914", Tilt::Lowest);
        assert_eq!(golden.odim_dataset, "/dataset1/data1");
        assert_eq!((sweep.rays.len(), sweep.gates), (360, 475));
        assert_eq!((sweep.first_gate_m, sweep.gate_spacing_m), (750, 500));
        // Row 0 spans 359.561°..0.505°: its centre is just east of north,
        // where equal sectors would have put it at 0.5°.
        assert!(
            sweep.rays[0].azimuth_deg < 0.1,
            "{}",
            sweep.rays[0].azimuth_deg
        );
        assert_eq!(site.source_item("PLC"), Some("Sindal"));
        assert_eq!(site.source_item("NOD"), Some("dksin"));
    }

    #[test]
    fn dbzh_is_preferred_then_th() {
        assert_eq!(reflectivity(&["TH", "DBZH", "VRADH"]), Some(1));
        assert_eq!(reflectivity(&["VRADH", "TH"]), Some(1));
        assert_eq!(reflectivity(&["DBZH"]), Some(0));
        assert_eq!(reflectivity(&["VRADH", "ZDR"]), None);
        assert_eq!(reflectivity::<&str>(&[]), None);
    }

    #[test]
    fn odim_2_0_angle_strings_parse_or_are_ignored() {
        let how = |key: &str, value: &str| -> Attrs {
            HashMap::from([(key.to_owned(), AttrValue::String(value.to_owned()))])
        };
        assert_eq!(
            angle_pairs(&how("azangels", "359.5:0.5,0.5:1.5"), 2),
            Some(vec![(359.5, 0.5), (0.5, 1.5)])
        );
        assert_eq!(
            angle_pairs(&how("azangles", "0:1, 1:2 ,2:3"), 3).map(|p| p.len()),
            Some(3)
        );
        // The wrong count, a broken pair, or another key: none of it.
        assert_eq!(angle_pairs(&how("azangels", "0:1,1:2"), 3), None);
        assert_eq!(angle_pairs(&how("azangels", "0:1,x:2"), 2), None);
        assert_eq!(angle_pairs(&how("aztimes", "0:1,1:2"), 2), None);
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

    /// A half-degree cut (MET Norway) fills the texture and the lookup too.
    #[test]
    fn a_half_degree_cut_needs_no_blank_row_either() {
        let path = format!("{ROOT}data/raw/ord_nohur_202609140930.h5");
        let sweep = decode_lowest(fs::File::open(path).unwrap(), Tilt::Lowest).unwrap();
        assert_eq!(sweep.rows(), 720);
        let mut used = vec![false; 720];
        for px in sweep.azimuth_lut().as_chunks::<4>().0 {
            used[usize::from(u16::from_le_bytes([px[0], px[1]]))] = true;
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
    /// DBZH needs well under 300 KB. (Measured on the full 14.7 MB volume
    /// before S13; the vendored fixture is now its 67 KB lowest-tilt extract.)
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
        assert!(decode_lowest_dbzh(Cursor::new(truncated.clone())).is_err());
        assert!(decode_lowest(Cursor::new(truncated), Tilt::Lowest).is_err());
    }

    #[test]
    fn requantizes_like_the_nexrad_convention() {
        // SMHI's int16 coding.
        let (gain, offset, nodata, undetect) = (0.01, 0.0, -32768.0, -32767.0);
        let q = |raw| requantize(raw, gain, offset, nodata, undetect);
        assert_eq!(q(-32767.0), 0);
        assert_eq!(q(-32768.0), 1);
        assert_eq!(q(0.0), 66); // 0 dBZ
        assert_eq!(q(2000.0), 106); // 20 dBZ
        assert_eq!(q(-3200.0), 2); // -32 dBZ, the floor code
        assert_eq!(q(-5000.0), 2); // clamped up
        assert_eq!(q(9500.0), 255); // 95 dBZ -> 256, clamped down
        // ORD's uint8 coding (gain 0.5, offset -32, nodata 255, undetect 0):
        // the code is the stored byte plus 2.
        let u = |raw| requantize(raw, 0.5, -32.0, 255.0, 0.0);
        assert_eq!(u(0.0), 0);
        assert_eq!(u(255.0), 1);
        assert_eq!(u(1.0), 3); // -31.5 dBZ
        assert_eq!(u(104.0), 106); // 20 dBZ
        assert_eq!(u(254.0), 255); // 95 dBZ -> 256, clamped down
        // A float coding (gain 1, offset 0), with ODIM's float sentinels.
        let f = |raw| requantize(raw, 1.0, 0.0, -9_999_000.0, -8_888_000.0);
        assert_eq!(f(-8_888_000.0), 0);
        assert_eq!(f(-9_999_000.0), 1);
        assert_eq!(f(20.25), 107); // 106.5 rounds away from zero
    }
}
