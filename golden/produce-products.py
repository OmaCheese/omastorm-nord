# Produces golden/<fixture>/products.json and one <variant>.u8.gz per
# product: the answer key for engine/src/products.rs (S20), independently of
# the engine. h5py (libhdf5) reads the multi-angle fixture, not hdf5-pure;
# each tilt is decoded as golden/produce-odim.py decodes the lowest one, and
# every product is composed from ALL the fixture's tilts by the rules in
# docs/protocol.md (Products, how each product is made). The engine reads
# only the tilts `products::needed` names, so a match also shows that its
# choice of tilts loses nothing.
#
#   uv run --no-project --with h5py --with numpy python golden/produce-products.py \
#     data/fixtures/radar_vara_qcvol_202609131055_tilts.h5 golden/vara-20260913 SOURCE_URL ORIGINAL.h5
#
# SOURCE_URL and ORIGINAL.h5 may be "-" to keep what the existing
# products.json says of them (S29 re-ran the keys for the new height rule
# without the original volumes).
#
# The float steps mirror the contract's formulas in the engine's order, with
# Python's scalar math (libm, like Rust's f64) rather than numpy's vector
# kernels, so the comparison is exact:
# - R = 6,371,000 m x 4/3; an output gate g of the lowest tilt (placed at
#   its nominal angle rounded to 0.01 deg) lies at ground distance
#   s = R atan2(r cos e0, R + r sin e0), r = firstGateM + g gateSpacingM;
# - a tilt at e covers it with gate round((r_e - first) / spacing) when that
#   is one of its gates, r_e = R sin(s/R) / cos(e + s/R) (no hit where
#   cos(e + s/R) <= 1e-9), at beam height R cos(e) / cos(e + s/R) - R;
# - a tilt's value for output ray i is that gate on its ray nearest ray i's
#   azimuth (both as float32 degrees, as the engine stores them), within
#   0.75 deg, else code 1;
# - cappi1/2 (S29's height rule, at 1000 and 2000 m above the antenna):
#   the point at ground distance s and height H is seen at
#   eH = atan2((R + H) cos(s/R) - R, (R + H) sin(s/R)); of the covering
#   tilts with |eH - e| <= 0.5 deg (radians compared), the one whose height is
#   nearest H, the lower angle on a tie; none: code 1; CMAX: the highest measured code
#   (>= 2), else 0 if any tilt is below threshold, else 1; HYBRID ("clear"):
#   per output ray, floor(azimuth) picks a blockage entry, and the lowest
#   tilt with round(elangle x 10) >= it (else the highest) gives the value
#   (code 1 where it does not cover). The blockage table here is synthetic
#   and written into products.json, so the engine's test uses the same one.
# - a single angle ("a<tenths>") is that tilt as decoded, nothing composed.
import datetime
import gzip
import hashlib
import json
import math
import os
import sys

import h5py
import numpy as np

R = 6_371_000.0 * 4.0 / 3.0
GAP_DEG = 0.75
BEAM_HALF_DEG = 0.5


def text(v):
    if isinstance(v, bytes):
        v = v.decode("ascii")
    return str(v).rstrip("\0").strip()


def rnd(x):
    """Rust's f64::round (half away from zero)."""
    fl = math.floor(x)
    return fl + (1 if x - fl >= 0.5 else 0) if x >= 0 else -rnd(-x)


def f32(x):
    return float(np.float32(x))


def angle_pairs(how, rays):
    """ODIM 2.0 how/azangles (or DMI's azangels): 'start:stop,...' per ray."""
    for key in ("azangles", "azangels"):
        if key in how and isinstance(how[key], (bytes, str)):
            pairs = [p.split(":") for p in text(how[key]).split(",")]
            if len(pairs) == rays and all(len(p) == 2 for p in pairs):
                return [(float(a), float(b)) for a, b in pairs]
    return None


def decode(ds):
    """One tilt as produce-odim.py decodes the lowest: rays in ascending
    azimuth, codes 0 undetect / 1 nodata / 2-255 measured."""
    moments = sorted((k for k in ds if k.startswith("data")), key=lambda k: int(k[4:]))
    quantity = {k: text(ds[k]["what"].attrs["quantity"]) for k in moments}
    m = next((k for k in moments if quantity[k] == "DBZH"), None) or next(
        k for k in moments if quantity[k] == "TH"
    )
    what, dwhat = ds[m]["what"].attrs, ds["what"].attrs
    coding = lambda key: float(what[key]) if key in what else float(dwhat[key])
    gain, offset = coding("gain"), coding("offset")
    nodata, undetect = coding("nodata"), coding("undetect")
    where = ds["where"].attrs
    how = ds["how"].attrs if "how" in ds else {}
    nrays, nbins = int(where["nrays"]), int(where["nbins"])
    rscale, rstart = float(where["rscale"]), float(where["rstart"])
    data = ds[m]["data"][...]
    assert data.shape == (nrays, nbins), data.shape
    per_ray = lambda key: (
        [float(x) for x in how[key]]
        if key in how and np.ndim(how[key]) == 1 and len(how[key]) == nrays
        and np.issubdtype(np.asarray(how[key]).dtype, np.floating)
        else None
    )
    start, stop = per_ray("startazA"), per_ray("stopazA")
    pairs = list(zip(start, stop)) if start and stop else angle_pairs(how, nrays)
    if pairs:
        centers = [(a + ((b - a) % 360.0) / 2.0) % 360.0 for a, b in pairs]
    else:
        centers = [(i + 0.5) * 360.0 / nrays for i in range(nrays)]
    order = sorted(range(nrays), key=lambda r: centers[r])
    rows = []
    for r in order:
        row = bytearray()
        for v in data[r].tolist():
            v = float(v)
            if v == undetect:
                row.append(0)
            elif v == nodata:
                row.append(1)
            else:
                row.append(int(min(max(rnd((v * gain + offset) * 2.0 + 66.0), 2.0), 255.0)))
        rows.append(bytes(row))
    return {
        "dataset": ds.name,
        "elangle": float(where["elangle"]),
        "first": int(rnd(rstart * 1000.0 + rscale / 2.0)),
        "spacing": int(rnd(rscale)),
        "gates": nbins,
        "azimuth": [f32(centers[r]) for r in order],
        "rows": rows,
    }


def ground_list(tilts):
    """The ground distance of each output gate (the lowest tilt's gates)."""
    base = tilts[0]
    e0 = math.radians(rnd(base["elangle"] * 100.0) / 100.0)
    grounds = []
    for g in range(base["gates"]):
        r = float(base["first"]) + g * float(base["spacing"])
        grounds.append(R * math.atan2(r * math.cos(e0), R + r * math.sin(e0)))
    return grounds


def hits(tilts):
    """Per tilt, per output gate: (gate, height) where it covers it, else None."""
    grounds = ground_list(tilts)
    out = []
    for t in tilts:
        e = math.radians(t["elangle"])
        per = []
        for s in grounds:
            theta = s / R
            c = math.cos(e + theta)
            if c <= 1e-9:
                per.append(None)
                continue
            r = R * math.sin(theta) / c
            gate = rnd((r - float(t["first"])) / float(t["spacing"]))
            per.append((int(gate), R * math.cos(e) / c - R) if 0 <= gate < t["gates"] else None)
        out.append(per)
    return out


def ray_map(out_az, scan_az):
    """Per output ray, the scan ray nearest its azimuth within GAP_DEG."""
    result = []
    for a in out_az:
        best, held = None, None
        for j, b in enumerate(scan_az):
            d = abs(b - a)
            d = min(d, 360.0 - d)
            if held is None or d < held:
                best, held = j, d
        result.append(best if held is not None and held <= GAP_DEG else None)
    return result


def etop_code(column, alt):
    """S24a Storm height of one output gate. `column`: the covering tilts,
    ascending by angle, as (code, beam height above the antenna in m)."""
    top = None
    for k, (v, _) in enumerate(column):
        if v >= 102:  # 18 dBZ
            top = k
    if top is None:
        return 0 if any(v != 1 for v, _ in column) else 1
    t = (column[top][1] + alt) / 1000.0
    code = min(2 + 2 * int(rnd(5.0 * t)), 254)
    at_least = all(v == 1 for v, _ in column[top + 1:])
    return code + (1 if at_least else 0)


def vil_code(column):
    """S24a Rain mass of one output gate, same `column`: Marshall-Palmer
    water over the gaps between consecutive beam centres, dBZ capped at 56."""
    readings = [(v, h) for v, h in column if v != 1]
    if not readings:
        return 1
    if all(v == 0 for v, _ in readings):
        return 0
    zs = []
    for v, h in readings:
        if v == 0:
            zs.append((0.0, h))
        else:
            dbz = min((float(v) - 66.0) / 2.0, 56.0)
            zs.append((math.pow(10.0, dbz / 10.0), h))
    total = 0.0
    for (za, ha), (zb, hb) in zip(zs, zs[1:]):
        total += 3.44e-6 * math.pow((za + zb) / 2.0, 4.0 / 7.0) * (hb - ha)
    return min(2 + int(rnd(2.0 * total)), 255)


def compose(tilts, product, height=None, table=None, alt=None):
    tilts = sorted(tilts, key=lambda t: t["elangle"])  # stable, like sort_by
    h = hits(tilts)
    base = tilts[0]
    gates = base["gates"]
    maps = [ray_map(base["azimuth"], t["azimuth"]) for t in tilts]

    def value(k, i, g):
        hit = h[k][g]
        if hit is None:
            return None
        j = maps[k][i]
        return tilts[k]["rows"][j][hit[0]] if j is not None else 1

    picks = []
    if product == "cappi":
        half = math.radians(BEAM_HALF_DEG)
        for g, s in enumerate(ground_list(tilts)):
            theta, rh = s / R, R + height
            seen = math.atan2(rh * math.cos(theta) - R, rh * math.sin(theta))
            best, held = None, None
            for k in range(len(tilts)):
                if h[k][g] is None:
                    continue
                if abs(seen - math.radians(tilts[k]["elangle"])) > half:
                    continue
                d = abs(h[k][g][1] - height)
                if held is None or d < held or (d == held and tilts[k]["elangle"] < tilts[best]["elangle"]):
                    best, held = k, d
            picks.append(best)
    ascending = sorted(range(len(tilts)), key=lambda k: tilts[k]["elangle"])
    codes = bytearray()
    for i, az in enumerate(base["azimuth"]):
        if product == "clear":
            tenths = table[int(math.floor(az)) % 360]
            k_clear = next((k for k in ascending if rnd(tilts[k]["elangle"] * 10.0) >= tenths), ascending[-1])
        for g in range(gates):
            if product == "cappi":
                v = value(picks[g], i, g) if picks[g] is not None else None
                codes.append(1 if v is None else v)
            elif product == "clear":
                v = value(k_clear, i, g)
                codes.append(1 if v is None else v)
            elif product in ("etop", "vil"):
                column = [(value(k, i, g), h[k][g][1]) for k in range(len(tilts)) if h[k][g] is not None]
                codes.append(etop_code(column, alt) if product == "etop" else vil_code(column))
            else:  # cmax
                top, below = None, False
                for k in range(len(tilts)):
                    v = value(k, i, g)
                    if v is not None and v >= 2:
                        top = v if top is None else max(top, v)
                    elif v == 0:
                        below = True
                codes.append(top if top is not None else (0 if below else 1))
    return bytes(codes), base


def synthetic_table(tilts):
    """A blockage table that exercises every branch: blocked up to the
    second angle in the north-east quadrant, above every angle due south
    (the highest is used), clear elsewhere."""
    angles = sorted(t["elangle"] for t in tilts)
    second = int(rnd(angles[1] * 10.0))
    return [second if 30 <= d < 120 else (255 if 170 <= d < 190 else 0) for d in range(360)]


def extract_of(source_url, original, tilts):
    """What the fixture was trimmed from: the original volume's name, size,
    digest, and the trim that made the fixture."""
    orig = open(original, "rb").read()
    angles = sorted(h5py.File(original, "r")[k]["where"].attrs["elangle"] for k in h5py.File(original, "r")
                    if k.startswith("dataset"))
    return {
        "file": source_url.rsplit("/", 1)[1],
        "bytes": len(orig),
        "sha256": hashlib.sha256(orig).hexdigest(),
        "by": "scripts/trim-odim.py --keep DBZH --tilts " + ",".join(str(angles.index(t["elangle"])) for t in tilts),
    }


def write_product(out, products, variant, codes, base, what):
    blob = gzip.compress(codes, mtime=0)
    name = f"{variant}.u8.gz"
    open(os.path.join(out, name), "wb").write(blob)
    counts = {"undetect": codes.count(0), "nodata": codes.count(1)}
    counts["measured"] = len(codes) - counts["undetect"] - counts["nodata"]
    products[variant] = {
        "what": what,
        "file": name,
        "rays": len(base["azimuth"]),
        "gates": base["gates"],
        "firstGateM": base["first"],
        "gateSpacingM": base["spacing"],
        "u8Sha256": hashlib.sha256(codes).hexdigest(),
        "counts": counts,
    }
    if variant == "etop":
        # "At least" tops are the odd measured codes (S24a).
        products[variant]["atLeast"] = sum(1 for c in codes if c >= 2 and c % 2 == 1)


def vertical(out, products, tilts, alt):
    """S24a: the storm height and the rain mass, from every tilt."""
    codes, base = compose(tilts, "etop", alt=alt)
    write_product(out, products, "etop", codes, base,
                  f"storm height: highest beam >= 18 dBZ, beam centre + altM {alt} m, 0.2 km, +1 at least")
    codes, base = compose(tilts, "vil")
    write_product(out, products, "vil", codes, base, "rain mass: Marshall-Palmer VIL, dBZ capped at 56, 0.5 kg/m2")


def parts_main(args):
    """S24a: FMI's one-angle SCAN files of one nominal time, assembled into
    one volume (every dataset of every file, ascending by angle), and its
    cmax, etop and vil.

      produce-products.py --parts OUT SOURCE_PREFIX FIXTURE=ORIGINAL ...
    """
    out, prefix, pairs = args[0], args[1], [a.split("=", 1) for a in args[2:]]
    os.makedirs(out, exist_ok=True)
    tilts, parts, alt, station = [], [], None, None
    for fixture, original in pairs:
        f = h5py.File(fixture, "r")
        names = sorted((k for k in f if k.startswith("dataset")), key=lambda k: int(k[len("dataset"):]))
        tilts.extend(decode(f[k]) for k in names)
        if alt is None:
            alt = float(f["where"].attrs["height"])
            station = text(f["what"].attrs["source"])
        raw, orig = open(fixture, "rb").read(), open(original, "rb").read()
        parts.append({
            "fixture": os.path.basename(fixture),
            "elangle": float(f[names[0]]["where"].attrs["elangle"]),
            "bytes": len(raw),
            "sha256": hashlib.sha256(raw).hexdigest(),
            "original": {
                "file": os.path.basename(original),
                "url": prefix + os.path.basename(original),
                "bytes": len(orig),
                "sha256": hashlib.sha256(orig).hexdigest(),
                "by": "scripts/trim-odim.py --keep DBZH",
            },
        })
    ascending = sorted(tilts, key=lambda t: t["elangle"])
    products = {}
    codes, base = compose(tilts, "cmax")
    write_product(out, products, "cmax", codes, base, "column maximum of the assembled volume")
    vertical(out, products, tilts, alt)
    golden = {
        "fixture": os.path.basename(os.path.normpath(out)),
        "station": station,
        "parts": parts,
        "tilts": [
            {"dataset": t["dataset"], "elangle": t["elangle"], "rays": len(t["azimuth"]), "gates": t["gates"],
             "firstGateM": t["first"], "gateSpacingM": t["spacing"]}
            for t in ascending
        ],
        "placementDeg": rnd(ascending[0]["elangle"] * 100.0) / 100.0,
        "altM": alt,
        "products": products,
        "byteOrder": "gzip of row-major rays x gates uint8; rays in ascending azimuth of the lowest tilt",
        "producedBy": f"golden/produce-products.py --parts (h5py {h5py.__version__}, numpy {np.__version__})",
        "producedOn": datetime.date.today().isoformat(),
    }
    with open(os.path.join(out, "products.json"), "w") as fh:
        json.dump(golden, fh, indent=1)
        fh.write("\n")
    print(json.dumps({v: (p["rays"], p["gates"], p["counts"], p.get("atLeast")) for v, p in products.items()}))


def main():
    if sys.argv[1] == "--parts":
        return parts_main(sys.argv[2:])
    src, out, source_url, original = sys.argv[1:5]
    raw = open(src, "rb").read()
    previous = None
    if original == "-" or source_url == "-":
        with open(os.path.join(out, "products.json")) as fh:
            previous = json.load(fh)
        if source_url == "-":
            source_url = previous["sourceUrl"]
    f = h5py.File(src, "r")
    names = sorted((k for k in f if k.startswith("dataset")), key=lambda k: int(k[len("dataset"):]))
    tilts = [decode(f[k]) for k in names]
    ascending = sorted(tilts, key=lambda t: t["elangle"])
    table = synthetic_table(tilts)
    os.makedirs(out, exist_ok=True)

    products = {}

    def write(variant, codes, base, what):
        blob = gzip.compress(codes, mtime=0)
        name = f"{variant}.u8.gz"
        open(os.path.join(out, name), "wb").write(blob)
        counts = {"undetect": codes.count(0), "nodata": codes.count(1)}
        counts["measured"] = len(codes) - counts["undetect"] - counts["nodata"]
        products[variant] = {
            "what": what,
            "file": name,
            "rays": len(base["azimuth"]),
            "gates": base["gates"],
            "firstGateM": base["first"],
            "gateSpacingM": base["spacing"],
            "u8Sha256": hashlib.sha256(codes).hexdigest(),
            "counts": counts,
        }

    for variant, height in (("cappi1", 1000.0), ("cappi2", 2000.0)):
        codes, base = compose(tilts, "cappi", height=height)
        write(variant, codes, base, f"height slice at {int(height)} m above the antenna, beam +-0.5 deg")
    codes, base = compose(tilts, "cmax")
    write("cmax", codes, base, "column maximum")
    codes, base = compose(tilts, "clear", table=table)
    write("clear", codes, base, "clear view under blockageTenths")
    for t in (ascending[1], ascending[-1]):
        deg = t["elangle"]
        write(f"a{int(rnd(deg * 10.0))}", b"".join(t["rows"]), t, f"the {deg} deg tilt ({t['dataset']}) as decoded")
    # S24a: the antenna's height from the file's own /where, not the
    # engine's site table.
    alt = float(f["where"].attrs["height"])
    vertical(out, products, tilts, alt)

    golden = {
        "fixture": os.path.basename(os.path.normpath(out)),
        "station": text(f["what"].attrs["source"]),
        "tilts": [
            {"dataset": t["dataset"], "elangle": t["elangle"], "rays": len(t["azimuth"]), "gates": t["gates"],
             "firstGateM": t["first"], "gateSpacingM": t["spacing"]}
            for t in tilts
        ],
        "placementDeg": rnd(ascending[0]["elangle"] * 100.0) / 100.0,
        "altM": alt,
        "blockageTenths": table,
        "products": products,
        "byteOrder": "gzip of row-major rays x gates uint8; rays in ascending azimuth of the lowest tilt (a single angle: its own)",
        "sourceFile": os.path.basename(src),
        "sourceUrl": source_url,
        "sourceSha256": hashlib.sha256(raw).hexdigest(),
        "sourceBytes": len(raw),
        "extractOf": previous["extractOf"] if original == "-" else extract_of(source_url, original, tilts),
        "producedBy": f"golden/produce-products.py (h5py {h5py.__version__}, numpy {np.__version__})",
        "producedOn": datetime.date.today().isoformat(),
    }
    with open(os.path.join(out, "products.json"), "w") as fh:
        json.dump(golden, fh, indent=1)
        fh.write("\n")
    print(json.dumps({v: (p["rays"], p["gates"], p["counts"]) for v, p in products.items()}))


main()
