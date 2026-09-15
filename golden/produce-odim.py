# Produces golden/<fixture>/sweep0.u8 + sweep0.json, the ODIM decoder's
# answer key for a Norwegian, Finnish or Danish radar file from EUMETNET Open
# Radar Data's 24-hour S3 cache (S15, DEC-13), independently of the engine:
# h5py (libhdf5) reads the file, not hdf5-pure. The shape is
# golden/vara-20260913/'s, so engine/src/odim.rs checks every fixture alike.
# Every float step mirrors engine/src/odim.rs so the comparison is exact.
#
#   uv run --no-project --with h5py --with numpy python golden/produce-odim.py \
#     data/raw/ord_nohur_202609140930.h5 golden/nohur-20260914 SOURCE_URL ORIGINAL.h5
#
# ORIGINAL.h5 is the file as the cache served it; the fixture is its
# lowest-tilt extract (scripts/trim-odim.py), and the answer key is the same
# from both apart from the source's sha256, size and date (EXTRACT_OF).
#
# What it reads, per country (the decoder quirks S15 found):
# - the lowest tilt by /datasetN/where elangle (ties: the lower N), never
#   /dataset1 by assumption;
# - DBZH by dataN/what/quantity, else TH (FI stores TH first, DBZH second);
# - any integer or float storage, requantized through gain/offset, with
#   undetect -> code 0 and nodata -> code 1 (all three are uint8, 0.5/-32);
# - ray centres from how/startazA + stopazA (FI, SMHI), else ODIM 2.0's
#   how/azangles "start:stop,..." string, which DMI writes as "azangels"
#   (DK), else equal sectors clockwise from north (NO: no angles at all);
# - ray times from how/startazT + stopazT, else the dataset's nominal
#   startdate/starttime and enddate/endtime (NO, FI, DK);
# - the first gate at rstart (km) + rscale / 2 (DK's rstart is 0.5 km);
#   S32: AEMET writes rstart in metres (200). Decided here from the pulse
#   rate, not from the engine's threshold: read as km, the ray would end
#   beyond the unambiguous range c / (2 x lowprf) of the root how/lowprf,
#   so it is metres (esahr: 560 Hz -> 267.7 km; 200 km + 250 km would be 450).
# - ES (S32): float64 TH then DBZH, gain 1, offset 0, undetect -32, nodata 95.5.
import datetime
import hashlib
import json
import math
import os
import sys

import h5py
import numpy as np


def text(v):
    if isinstance(v, bytes):
        v = v.decode("ascii")
    return str(v).rstrip("\0").strip()


def rnd(x):
    """Rust's f64::round (half away from zero)."""
    fl = math.floor(x)
    return fl + (1 if x - fl >= 0.5 else 0) if x >= 0 else -rnd(-x)


def four(x):
    """A Rust f32 formatted with four decimals, read back as a JSON number."""
    return float(f"{float(np.float32(x)):.4f}")


def nominal_ms(what, date, time):
    stamp = datetime.datetime.strptime(text(what[date]) + text(what[time]), "%Y%m%d%H%M%S")
    return int(stamp.replace(tzinfo=datetime.UTC).timestamp() * 1000)


def angle_pairs(how, rays):
    """ODIM 2.0 how/azangles (or DMI's azangels): 'start:stop,...' per ray."""
    for key in ("azangles", "azangels"):
        if key in how and isinstance(how[key], (bytes, str)):
            pairs = [p.split(":") for p in text(how[key]).split(",")]
            if len(pairs) == rays and all(len(p) == 2 for p in pairs):
                return [(float(a), float(b)) for a, b in pairs], key
    return None, None


def main():
    src, out, source_url, original = sys.argv[1:5]
    raw_bytes = open(src, "rb").read()
    orig_bytes = open(original, "rb").read()
    f = h5py.File(src, "r")
    g = h5py.File(original, "r")

    def tilts(h):
        return sorted(
            (float(h[k]["where"].attrs["elangle"]), int(k[len("dataset"):]), k)
            for k in h if k.startswith("dataset")
        )

    # The extract keeps the original's lowest tilt: hold it to that.
    assert tilts(g)[0][0] == tilts(f)[0][0], (tilts(g)[0], tilts(f)[0])
    name = tilts(f)[0][2]
    ds = f[name]

    moments = sorted((k for k in ds if k.startswith("data")), key=lambda k: int(k[4:]))
    quantity = {k: text(ds[k]["what"].attrs["quantity"]) for k in moments}
    m = next((k for k in moments if quantity[k] == "DBZH"), None) or next(
        k for k in moments if quantity[k] == "TH"
    )
    what = ds[m]["what"].attrs
    dwhat = ds["what"].attrs
    coding = lambda key: float(what[key]) if key in what else float(dwhat[key])
    gain, offset = coding("gain"), coding("offset")
    nodata, undetect = coding("nodata"), coding("undetect")
    where = ds["where"].attrs
    how = ds["how"].attrs if "how" in ds else {}
    nrays, nbins = int(where["nrays"]), int(where["nbins"])
    rscale, rstart = float(where["rscale"]), float(where["rstart"])
    rstart_unit = "km (ODIM)"
    root_how = f["how"].attrs if "how" in f else {}
    if "lowprf" in root_how and float(root_how["lowprf"]) > 0:
        unambiguous_km = 299_792.458 / (2.0 * float(root_how["lowprf"]))
        ray_km = nbins * rscale / 1000.0
        if rstart + ray_km > unambiguous_km >= rstart / 1000.0 + ray_km:
            rstart, rstart_unit = rstart / 1000.0, (
                f"metres: as km the ray would end at {rstart + nbins * rscale / 1000.0:g} km,"
                f" past c/(2 lowprf) = {unambiguous_km:.1f} km"
            )
    elangle = float(where["elangle"])
    data = ds[m]["data"][...]
    assert data.shape == (nrays, nbins), data.shape

    per_ray = lambda key: (
        [float(x) for x in how[key]]
        if key in how and np.ndim(how[key]) == 1 and len(how[key]) == nrays
        and np.issubdtype(np.asarray(how[key]).dtype, np.floating)
        else None
    )
    start, stop = per_ray("startazA"), per_ray("stopazA")
    if start and stop:
        pairs, angles = list(zip(start, stop)), "how/startazA + stopazA"
    else:
        pairs, key = angle_pairs(how, nrays)
        angles = f"how/{key} (ODIM 2.0 start:stop string)" if pairs else "equal sectors from north"
    if pairs:
        centers = [(a + ((b - a) % 360.0) / 2.0) % 360.0 for a, b in pairs]
    else:
        centers = [(i + 0.5) * 360.0 / nrays for i in range(nrays)]
    order = sorted(range(nrays), key=lambda r: centers[r])  # stable, like sort_by
    ray_el = per_ray("elangles") or [elangle] * nrays
    t0, t1 = per_ray("startazT"), per_ray("stopazT")
    if t0 and t1:
        t_start = [int(rnd(t * 1000.0)) for t in t0]
        base_ms, end_ms = min(t_start), max(int(rnd(t * 1000.0)) for t in t1)
        times = "how/startazT + stopazT"
    else:
        base_ms = nominal_ms(dwhat, "startdate", "starttime")
        end_ms = nominal_ms(dwhat, "enddate", "endtime") if "enddate" in dwhat else base_ms
        t_start = [base_ms] * nrays
        times = "the dataset's startdate/starttime and enddate/endtime"

    codes = bytearray()
    counts = {"undetect": 0, "nodata": 0, "measured": 0}
    for r in order:
        for gate in range(nbins):
            v = float(data[r, gate])
            if v == undetect:
                counts["undetect"] += 1
                codes.append(0)
            elif v == nodata:
                counts["nodata"] += 1
                codes.append(1)
            else:
                counts["measured"] += 1
                dbz = v * gain + offset
                codes.append(int(min(max(rnd(dbz * 2.0 + 66.0), 2.0), 255.0)))
    os.makedirs(out, exist_ok=True)
    open(os.path.join(out, "sweep0.u8"), "wb").write(bytes(codes))

    rwhere, rwhat = f["where"].attrs, f["what"].attrs
    iso = lambda ms: datetime.datetime.fromtimestamp(ms // 1000, datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
    golden = {
        "fixture": os.path.basename(os.path.normpath(out)),
        "station": text(rwhat["source"]),
        "object": text(rwhat["object"]),
        "conventions": text(f.attrs.get("Conventions", "")),
        "moment": quantity[m],
        "sweep": 0,
        "odimDataset": f"/{name}/{m}",
        "storage": str(data.dtype),
        "rays": nrays,
        "gates": nbins,
        "scale": 2.0,
        "offset": 66.0,
        "codeMeaning": {
            "0": "undetect (below threshold, G bit 2)",
            "1": "nodata (outside coverage, G bit 4)",
            "2-255": "measured; dBZ=(code-offset)/scale",
        },
        "firstGateM": rnd(rstart * 1000.0 + rscale / 2.0),
        "gateSpacingM": rnd(rscale),
        "rangeKm": rstart + nbins * rscale / 1000.0,
        "rstartUnit": rstart_unit,
        "elangle": elangle,
        "a1gate": int(where["a1gate"]) if "a1gate" in where else None,
        "odimGain": gain,
        "odimOffset": offset,
        "odimNodata": nodata,
        "odimUndetect": undetect,
        "azimuthsFrom": angles,
        "timesFrom": times,
        "azimuthDeg": [four(centers[r]) for r in order],
        "elevationDeg": [four(ray_el[r]) for r in order],
        "rayTimeBase": iso(base_ms),
        "rayTimeBaseMs": base_ms,
        "rayTimeMs": [t_start[r] - base_ms for r in order],
        "sweepEndMs": end_ms,
        "scanStart": f"{text(dwhat['startdate'])}T{text(dwhat['starttime'])}Z",
        "scanEnd": f"{text(dwhat['enddate'])}T{text(dwhat['endtime'])}Z",
        "siteLat": float(rwhere["lat"]),
        "siteLon": float(rwhere["lon"]),
        "siteAltM": float(rwhere["height"]),
        "counts": counts,
        "sourceFile": os.path.basename(src),
        "sourceUrl": source_url,
        "sourceSha256": hashlib.sha256(raw_bytes).hexdigest(),
        "sourceBytes": len(raw_bytes),
        "extractOf": {
            "file": source_url.rsplit("/", 1)[1],
            "bytes": len(orig_bytes),
            "sha256": hashlib.sha256(orig_bytes).hexdigest(),
            "by": "scripts/trim-odim.py --keep " + ",".join(
                text(ds[k]["what"].attrs["quantity"]) for k in moments
            ),
        },
        "rowOrder": "ascending azimuth (stable sort of ODIM row order by ray centre)",
        "rayTime": "ms from the earliest ray start; sweepEndMs is the latest ray stop, or the nominal end",
        "byteOrder": "row-major, rays x gates, uint8",
        "producedBy": f"golden/produce-odim.py (h5py {h5py.__version__}, numpy {np.__version__})",
        "producedOn": datetime.date.today().isoformat(),
    }
    with open(os.path.join(out, "sweep0.json"), "w") as fh:
        json.dump(golden, fh, separators=(",", ":"))
        fh.write("\n")
    print(json.dumps({k: golden[k] for k in (
        "fixture", "moment", "odimDataset", "storage", "rays", "gates", "firstGateM", "gateSpacingM",
        "azimuthsFrom", "timesFrom", "counts", "rayTimeBase")}))


main()
