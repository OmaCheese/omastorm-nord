"""Golden answer key for the Sweden composite reprojection (stream S8, DEC-11).

Produced with h5py and pyproj, never with the engine's reader or its own
projection code, so `engine/src/composite.rs` is checked against an
independent implementation:

    uv run --with h5py --with numpy --with pyproj \
        python golden/sweden-20260913/produce.py data/raw/radar_sweden_comp_202609132220.h5

The contract it encodes (docs/protocol.md, grid texture):

- The source grid is ODIM `/where`: `projdef`, `xsize` × `ysize` pixels of
  `xscale` × `yscale` metres, row 0 north. `UL_lon`/`UL_lat` is the outer
  corner of the upper-left pixel.
- The Web Mercator bounding box is the lon/lat box of the grid's outer
  boundary, sampled at every source pixel edge. The texture starts at its
  west and north edges and covers it with whole `PIXEL_M` Mercator pixels,
  so `east` and `south` move outward to the last pixel's edge.
- Output pixel (c, r) takes the source pixel containing its centre (nearest
  neighbour); outside the grid is nodata.
- Codes follow the polar convention: undetect 0, nodata or outside 1,
  measured round(dBZ * 2 + 66) clamped to 2..255.

Writes `grid.json` beside this script.
"""

import hashlib
import json
import math
import sys
from pathlib import Path

import h5py
import numpy as np
import pyproj
from pyproj import Proj

PIXEL_M = 2000.0
R = 6378137.0


def text(value):
    return value.decode() if isinstance(value, bytes) else str(value)


def main(path):
    raw_bytes = Path(path).read_bytes()
    f = h5py.File(path, "r")
    where = f["where"].attrs
    projdef = text(where["projdef"])
    xsize, ysize = int(where["xsize"]), int(where["ysize"])
    xscale, yscale = float(where["xscale"]), float(where["yscale"])
    stere = Proj(projdef)

    data = None
    for name in sorted(k for k in f["dataset1"] if k.startswith("data")):
        if text(f[f"dataset1/{name}/what"].attrs["quantity"]) == "DBZH":
            data = f"dataset1/{name}"
            break
    what = f[f"{data}/what"].attrs
    gain, offset = float(what["gain"]), float(what["offset"])
    nodata, undetect = float(what["nodata"]), float(what["undetect"])
    values = f[f"{data}/data"][...]
    assert values.shape == (ysize, xsize), values.shape

    x0, y0 = stere(float(where["UL_lon"]), float(where["UL_lat"]))
    lrx, lry = stere(float(where["LR_lon"]), float(where["LR_lat"]))

    # The outer boundary at every source pixel edge.
    xs = x0 + np.arange(xsize + 1) * xscale
    ys = y0 - np.arange(ysize + 1) * yscale
    bx = np.concatenate([xs, xs, np.full(ysize + 1, x0), np.full(ysize + 1, x0 + xsize * xscale)])
    by = np.concatenate([np.full(xsize + 1, y0), np.full(xsize + 1, y0 - ysize * yscale), ys, ys])
    blon, blat = stere(bx, by, inverse=True)
    west, east0 = float(blon.min()), float(blon.max())
    south0, north = float(blat.min()), float(blat.max())

    merc_y = lambda lat: R * math.log(math.tan(math.pi / 4 + math.radians(lat) / 2))
    mxw, mxe = R * math.radians(west), R * math.radians(east0)
    myn, mys = merc_y(north), merc_y(south0)
    width = math.ceil((mxe - mxw) / PIXEL_M)
    height = math.ceil((myn - mys) / PIXEL_M)
    east = math.degrees((mxw + width * PIXEL_M) / R)
    south = math.degrees(math.atan(math.sinh((myn - height * PIXEL_M) / R)))

    # Every output pixel centre, back to lon/lat, forward to the grid.
    cx = mxw + (np.arange(width) + 0.5) * PIXEL_M
    cy = myn - (np.arange(height) + 0.5) * PIXEL_M
    lon = np.degrees(cx / R)
    lat = np.degrees(np.arctan(np.sinh(cy / R)))
    LON, LAT = np.meshgrid(lon, lat)
    X, Y = stere(LON, LAT)
    fi = (X - x0) / xscale
    fj = (y0 - Y) / yscale
    i = np.floor(fi).astype(np.int64)
    j = np.floor(fj).astype(np.int64)
    inside = (i >= 0) & (i < xsize) & (j >= 0) & (j < ysize)

    dbz = values.astype(np.float64) * gain + offset
    measured = np.clip(np.round(dbz * 2 + 66), 2, 255).astype(np.uint8)
    source_codes = np.where(values == undetect, 0, np.where(values == nodata, 1, measured)).astype(np.uint8)
    codes = np.ones((height, width), np.uint8)
    codes[inside] = source_codes[j[inside], i[inside]]

    # A centre within a millimetre of a source pixel edge could land on
    # either side in another float implementation; samples avoid those.
    edge_mm = 1e-3 / xscale
    near = (np.abs(fi - np.round(fi)) < edge_mm) | (np.abs(fj - np.round(fj)) < edge_mm)

    rng = np.random.default_rng(20260913)
    safe = ~near
    pick = []
    for cls, n in [(codes >= 2, 120), (codes == 0, 40), (codes == 1, 40)]:
        rows, cols = np.nonzero(cls & safe)
        take = rng.choice(len(rows), size=min(n, len(rows)), replace=False)
        pick += [(int(cols[k]), int(rows[k])) for k in take]
    pick.sort(key=lambda p: (p[1], p[0]))
    samples = [
        {"col": c, "row": r, "lon": round(float(LON[r, c]), 9), "lat": round(float(LAT[r, c]), 9),
         "srcCol": int(i[r, c]) if inside[r, c] else None, "srcRow": int(j[r, c]) if inside[r, c] else None,
         "code": int(codes[r, c])}
        for c, r in pick
    ]

    # Projection spot checks: lon/lat -> stereographic metres, both ways.
    points = [(float(where[f"{k}_lon"]), float(where[f"{k}_lat"])) for k in ("UL", "UR", "LL", "LR")]
    points += [(11.97, 57.71), (18.07, 59.33), (20.23, 67.86), (14.0, 62.0), (24.0, 55.0), (5.5, 69.0)]
    projection = [
        {"lon": lo, "lat": la, "x": round(px, 4), "y": round(py, 4)}
        for (lo, la), (px, py) in ((p, stere(*p)) for p in points)
    ]

    what_root = f["what"].attrs
    dwhat = f["dataset1/what"].attrs
    out = {
        "fixture": "sweden-20260913",
        "source": Path(path).name,
        "sha256": hashlib.sha256(raw_bytes).hexdigest(),
        "bytes": len(raw_bytes),
        "object": text(what_root["object"]),
        "odimSource": text(what_root["source"]),
        "date": text(what_root["date"]),
        "time": text(what_root["time"]),
        "endDate": text(dwhat["enddate"]),
        "endTime": text(dwhat["endtime"]),
        "prodpar": float(dwhat["prodpar"]),
        "dataset": data,
        "quantity": "DBZH",
        "odimGain": gain,
        "odimOffset": offset,
        "odimNodata": nodata,
        "odimUndetect": undetect,
        "projdef": projdef,
        "xsize": xsize,
        "ysize": ysize,
        "xscale": xscale,
        "yscale": yscale,
        "ulX": round(x0, 4),
        "ulY": round(y0, 4),
        "lrX": round(lrx, 4),
        "lrY": round(lry, 4),
        "sourceCounts": {
            "measured": int((source_codes >= 2).sum()),
            "undetect": int((source_codes == 0).sum()),
            "nodata": int((source_codes == 1).sum()),
        },
        "pixelM": PIXEL_M,
        "sphereR": R,
        "width": width,
        "height": height,
        "west": round(west, 9),
        "east": round(east, 9),
        "north": round(north, 9),
        "south": round(south, 9),
        "counts": {
            "measured": int((codes >= 2).sum()),
            "undetect": int((codes == 0).sum()),
            "nodataOrOutside": int((codes == 1).sum()),
            "outside": int((~inside).sum()),
        },
        "nearEdge": int(near.sum()),
        "projection": projection,
        "samples": samples,
        "provenance": {
            "command": " ".join(["uv run --with h5py --with numpy --with pyproj python"] + sys.argv),
            "h5py": h5py.__version__,
            "numpy": np.__version__,
            "pyproj": pyproj.__version__,
            "proj": pyproj.proj_version_str,
        },
    }
    dest = Path(__file__).with_name("grid.json")
    dest.write_text(json.dumps(out, indent=1) + "\n")
    print(f"{dest}: {width} x {height}, bbox W{west:.4f} E{east:.4f} S{south:.4f} N{north:.4f}, "
          f"{out['counts']}, near-edge {out['nearEdge']}")


if __name__ == "__main__":
    main(sys.argv[1])
