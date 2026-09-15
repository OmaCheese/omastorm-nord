"""Golden answer key for the Iberian crop of the OPERA composite (stream S33, DEC-17).

The same contract as golden/nordic-20260914/produce.py (S16, DEC-14), over the
`iberia` box: the file and the reprojection rules are the same, only the box,
the fixture and the projection spot checks differ.

Produced with h5py and pyproj, never with the engine's reader or its own
projection code, so `engine/src/composite.rs` is checked against an
independent implementation:

    uv run --no-project --with h5py --with numpy --with pyproj \
        python golden/iberia-20260915/produce.py data/raw/opera_iberia_202609151700.h5

The contract it encodes (docs/protocol.md, grid texture; DEC-14):

- The source grid is ODIM `/where`: `projdef` (`+proj=laea`), `xsize` ×
  `ysize` pixels of `xscale` × `yscale` metres, row 0 north. `UL_lon`/`UL_lat`
  is the outer corner of the upper-left pixel.
- The texture covers the Iberian box (`BOX`) with whole `PIXEL_M` Web Mercator
  texels counted from its west and north edges, so `east` and `south` move
  outward to the last texel's edge.
- Texel (c, r) takes the source pixel containing its centre (nearest
  neighbour); off the grid is nodata.
- Codes follow the polar convention: undetect 0, nodata or off the grid 1,
  measured round(dBZ * 2 + 66) clamped to 2..255.
- Only the chunks some texel falls in are needed. The fixture allocates a
  few of them (scripts/crop-opera.py); a chunk it leaves out reads as the
  dataset's fill value, which the fixture sets to nodata, and the engine
  reads a needed chunk the file does not allocate as nodata too.

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
BOX = {"west": -10.5, "east": 4.5, "south": 35.0, "north": 44.5}


def text(value):
    return value.decode() if isinstance(value, bytes) else str(value)


def main(path):
    raw_bytes = Path(path).read_bytes()
    f = h5py.File(path, "r")
    where = f["where"].attrs
    projdef = text(where["projdef"])
    xsize, ysize = int(where["xsize"]), int(where["ysize"])
    xscale, yscale = float(where["xscale"]), float(where["yscale"])
    laea = Proj(projdef)

    data = None
    for name in sorted(k for k in f["dataset1"] if k.startswith("data")):
        if text(f[f"dataset1/{name}/what"].attrs["quantity"]) == "DBZH":
            data = f"dataset1/{name}"
            break
    what = f[f"{data}/what"].attrs
    gain, offset = float(what["gain"]), float(what["offset"])
    nodata, undetect = float(what["nodata"]), float(what["undetect"])
    dset = f[f"{data}/data"]
    assert dset.shape == (ysize, xsize), dset.shape
    crow, ccol = dset.chunks

    x0, y0 = laea(float(where["UL_lon"]), float(where["UL_lat"]))
    lrx, lry = laea(float(where["LR_lon"]), float(where["LR_lat"]))

    merc_y = lambda lat: R * math.log(math.tan(math.pi / 4 + math.radians(lat) / 2))
    mxw, mxe = R * math.radians(BOX["west"]), R * math.radians(BOX["east"])
    myn, mys = merc_y(BOX["north"]), merc_y(BOX["south"])
    width = math.ceil((mxe - mxw) / PIXEL_M)
    height = math.ceil((myn - mys) / PIXEL_M)
    east = math.degrees((mxw + width * PIXEL_M) / R)
    south = math.degrees(math.atan(math.sinh((myn - height * PIXEL_M) / R)))

    # Every texel centre, back to lon/lat, forward to the grid.
    cx = mxw + (np.arange(width) + 0.5) * PIXEL_M
    cy = myn - (np.arange(height) + 0.5) * PIXEL_M
    lon = np.degrees(cx / R)
    lat = np.degrees(np.arctan(np.sinh(cy / R)))
    LON, LAT = np.meshgrid(lon, lat)
    X, Y = laea(LON, LAT)
    fi = (X - x0) / xscale
    fj = (y0 - Y) / yscale
    i = np.floor(fi).astype(np.int64)
    j = np.floor(fj).astype(np.int64)
    inside = (i >= 0) & (i < xsize) & (j >= 0) & (j < ysize)

    needed = sorted({(int(a), int(b)) for a, b in zip((j[inside] // crow).ravel(), (i[inside] // ccol).ravel())})

    # Only the window the texels reach is read; h5py fills what the file
    # does not allocate with the dataset's fill value.
    r0, r1 = int(j[inside].min()), int(j[inside].max()) + 1
    c0, c1 = int(i[inside].min()), int(i[inside].max()) + 1
    values = dset[r0:r1, c0:c1]

    def codes_of(v):
        dbz = v.astype(np.float64) * gain + offset
        measured = np.clip(np.round(dbz * 2 + 66), 2, 255).astype(np.uint8)
        return np.where(v == undetect, 0, np.where(v == nodata, 1, measured)).astype(np.uint8)

    window = codes_of(values)
    codes = np.ones((height, width), np.uint8)
    codes[inside] = window[j[inside] - r0, i[inside] - c0]

    # The chunks the file allocates, and what each needed one holds.
    allocated = []
    for k in range(dset.id.get_num_chunks()):
        info = dset.id.get_chunk_info(k)
        row, col = info.chunk_offset[0] // crow, info.chunk_offset[1] // ccol
        entry = {"row": int(row), "col": int(col), "address": int(info.byte_offset), "size": int(info.size),
                 "needed": (int(row), int(col)) in needed}
        if entry["needed"]:
            block = codes_of(dset[row * crow:min((row + 1) * crow, ysize), col * ccol:min((col + 1) * ccol, xsize)])
            entry["counts"] = {"measured": int((block >= 2).sum()), "undetect": int((block == 0).sum()),
                               "nodata": int((block == 1).sum())}
        allocated.append(entry)
    allocated.sort(key=lambda e: (e["row"], e["col"]))

    # A centre within a millimetre of a source pixel edge could land on
    # either side in another float implementation; samples avoid those.
    edge_mm = 1e-3 / xscale
    near = (np.abs(fi - np.round(fi)) < edge_mm) | (np.abs(fj - np.round(fj)) < edge_mm)

    rng = np.random.default_rng(20260915)
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

    # Projection spot checks: lon/lat -> LAEA metres, and pyproj's inverse
    # of those metres.
    points = [(float(where[f"{k}_lon"]), float(where[f"{k}_lat"])) for k in ("UL", "UR", "LL", "LR")]
    # The box's corners, Lisbon, Porto, Madrid, Barcelona, Palma, Seville,
    # Portugal's three mainland radars and Menorca's east tip.
    points += [(-10.5, 44.5), (4.5, 44.5), (-10.5, 35.0), (4.5, 35.0), (-9.14, 38.72), (-8.61, 41.15),
               (-3.70, 40.42), (2.17, 41.39), (2.65, 39.57), (-5.98, 37.39), (-8.2797, 40.845),
               (-8.4001, 39.0714), (-7.953, 37.3041), (4.33, 39.9)]
    projection = []
    for lo, la in points:
        px, py = laea(lo, la)
        ilo, ila = laea(px, py, inverse=True)
        projection.append({"lon": lo, "lat": la, "x": round(px, 4), "y": round(py, 4),
                           "inverseLon": round(ilo, 10), "inverseLat": round(ila, 10)})

    what_root = f["what"].attrs
    dwhat = f["dataset1/what"].attrs
    out = {
        "fixture": "iberia-20260915",
        "source": Path(path).name,
        "sha256": hashlib.sha256(raw_bytes).hexdigest(),
        "bytes": len(raw_bytes),
        "object": text(what_root["object"]),
        "odimSource": text(what_root["source"]),
        "date": text(what_root["date"]),
        "time": text(what_root["time"]),
        "startDate": text(dwhat["startdate"]),
        "startTime": text(dwhat["starttime"]),
        "endDate": text(dwhat["enddate"]),
        "endTime": text(dwhat["endtime"]),
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
        "chunkRows": int(crow),
        "chunkCols": int(ccol),
        "ulX": round(x0, 4),
        "ulY": round(y0, 4),
        "lrX": round(lrx, 4),
        "lrY": round(lry, 4),
        "box": BOX,
        "neededChunks": [{"row": a, "col": b} for a, b in needed],
        "allocatedChunks": allocated,
        "pixelM": PIXEL_M,
        "sphereR": R,
        "width": width,
        "height": height,
        "west": round(BOX["west"], 9),
        "east": round(east, 9),
        "north": round(BOX["north"], 9),
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
            "command": " ".join(["uv run --no-project --with h5py --with numpy --with pyproj python"] + sys.argv),
            "h5py": h5py.__version__,
            "numpy": np.__version__,
            "pyproj": pyproj.__version__,
            "proj": pyproj.proj_version_str,
        },
    }
    dest = Path(__file__).with_name("grid.json")
    dest.write_text(json.dumps(out, indent=1) + "\n")
    print(f"{dest}: {width} x {height}, bbox W{BOX['west']} E{east:.4f} S{south:.4f} N{BOX['north']}, "
          f"needed chunks {needed}, {out['counts']}, near-edge {out['nearEdge']}")


if __name__ == "__main__":
    main(sys.argv[1])
