# Produces golden/vara-20260913/sweep0.u8 + sweep0.json, the ODIM decoder's
# answer key, independently of the engine: h5py (libhdf5) reads the file, not
# hdf5-pure. Every float step mirrors engine/src/odim.rs so the comparison is
# exact rather than tolerant.
#
#   uv run --no-project --with h5py --with numpy python \
#     golden/vara-20260913/produce.py data/raw/radar_vara_qcvol_202609131055.h5 golden/vara-20260913
#
# Since S13 the vendored file is the lowest-tilt extract of SMHI's volume
# (scripts/trim-odim.py: root groups + /dataset1 with DBZH only). Run on the
# original volume and on the extract, this script writes the same sweep0.u8
# and the same sweep0.json apart from the source's sha256, size and date;
# EXTRACT_OF records the original.
import datetime
import hashlib
import json
import math
import os
import sys

import h5py
import numpy as np

SOURCE_URL = (
    "https://opendata-download-radar.smhi.se/api/version/latest/area/vara/"
    "product/qcvol/2026/09/13/radar_vara_qcvol_202609131055.h5"
)
EXTRACT_OF = {
    "file": "radar_vara_qcvol_202609131055.h5",
    "bytes": 14701179,
    "sha256": "af8a6cc1984b2bd863cf4be484a2959e5d6020629ab8a9aa7f5447f8b1d1e7d0",
    "by": "scripts/trim-odim.py --keep DBZH",
}


def text(v):
    if isinstance(v, bytes):
        v = v.decode("ascii")
    return str(v).rstrip("\0").strip()


def rnd(x):
    """Rust's f64::round for the non-negative values it is used on."""
    fl = math.floor(x)
    return fl + (1 if x - fl >= 0.5 else 0) if x >= 0 else -rnd(-x)


def four(x):
    """A Rust f32 formatted with four decimals, read back as a JSON number."""
    return float(f"{float(np.float32(x)):.4f}")


def main():
    src, out = sys.argv[1], sys.argv[2]
    raw_bytes = open(src, "rb").read()
    f = h5py.File(src, "r")

    elangles = {}
    n = 1
    while f"dataset{n}" in f:
        elangles[n] = float(f[f"dataset{n}/where"].attrs["elangle"])
        n += 1
    # The decoder reads /dataset1 only; hold the file to that assumption.
    assert elangles[1] == min(elangles.values()), elangles
    ds = f["dataset1"]

    m = 1
    while text(ds[f"data{m}/what"].attrs["quantity"]) != "DBZH":
        m += 1
    what = ds[f"data{m}/what"].attrs
    gain, offset = float(what["gain"]), float(what["offset"])
    nodata, undetect = float(what["nodata"]), float(what["undetect"])
    where = ds["where"].attrs
    how = ds["how"].attrs
    nrays, nbins = int(where["nrays"]), int(where["nbins"])
    rscale, rstart = float(where["rscale"]), float(where["rstart"])
    elangle = float(where["elangle"])
    data = ds[f"data{m}/data"][...]
    assert data.shape == (nrays, nbins), data.shape

    start = [float(x) for x in how["startazA"]]
    stop = [float(x) for x in how["stopazA"]]
    centers = [(a + ((b - a) % 360.0) / 2.0) % 360.0 for a, b in zip(start, stop)]
    order = sorted(range(nrays), key=lambda r: centers[r])  # stable, like sort_by
    ray_el = [float(x) for x in how["elangles"]] if "elangles" in how else [elangle] * nrays
    t_start = [int(rnd(float(t) * 1000.0)) for t in how["startazT"]]
    t_stop = [int(rnd(float(t) * 1000.0)) for t in how["stopazT"]]
    base_ms, end_ms = min(t_start), max(t_stop)

    codes = bytearray()
    counts = {"undetect": 0, "nodata": 0, "measured": 0}
    for r in order:
        for g in range(nbins):
            v = float(data[r, g])
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
    open(os.path.join(out, "sweep0.u8"), "wb").write(bytes(codes))

    rwhere, rwhat, dwhat = f["where"].attrs, f["what"].attrs, ds["what"].attrs
    iso = lambda ms: datetime.datetime.fromtimestamp(ms // 1000, datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
    golden = {
        "fixture": "vara-20260913",
        "station": text(rwhat["source"]),
        "moment": "DBZH",
        "sweep": 0,
        "odimDataset": f"/dataset1/data{m}",
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
        "elangle": elangle,
        "a1gate": int(where["a1gate"]),
        "odimGain": gain,
        "odimOffset": offset,
        "odimNodata": nodata,
        "odimUndetect": undetect,
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
        "sourceUrl": SOURCE_URL,
        "sourceSha256": hashlib.sha256(raw_bytes).hexdigest(),
        "sourceBytes": len(raw_bytes),
        "extractOf": EXTRACT_OF,
        "rowOrder": "ascending azimuth (stable sort of ODIM row order by the startazA/stopazA midpoint)",
        "rayTime": "how/startazT of each ray in ms (rounded), counted from the earliest; sweepEndMs is the latest stopazT",
        "byteOrder": "row-major, rays x gates, uint8",
        "producedBy": f"golden/vara-20260913/produce.py (h5py {h5py.__version__}, numpy {np.__version__})",
        "producedOn": datetime.date.today().isoformat(),
    }
    with open(os.path.join(out, "sweep0.json"), "w") as fh:
        json.dump(golden, fh, separators=(",", ":"))
        fh.write("\n")
    print(json.dumps({k: golden[k] for k in ("rays", "gates", "firstGateM", "gateSpacingM", "counts", "rayTimeBase", "sourceSha256")}))


main()
