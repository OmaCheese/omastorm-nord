#!/usr/bin/env python3
"""Per-pair reflectivity offsets between overlapping radars: a report,
nothing applied (S24b, plan §S24b step 6, docs/protocol.md "The composites'
products").

The composites' products take, per texel, one radar's value with no
averaging, so a radar that reads a few dB hotter than its neighbour shows
as a seam. This estimates each overlapping pair's offset from the volumes
in the engine's tilt store (S27: `<cache>/omastorm-se/tilts`, `index.sqlite`
and one `.u8z` file per tilt) and the station table
(`engine/data/sites.json`):

- For every nominal time (5-minute mark) at which two radars within reach
  of each other both have a volume, their overlap is sampled every `--step`
  km (inside both radars' range on the 6,371 km sphere).
- At each point each radar's scans are placed by the lookup rule (slant
  range and beam centre on the 4/3 earth, the gate nearest the slant range,
  the ray nearest the bearing within 0.75°), and the pair of scans, one per
  radar, whose beam centres above sea level are nearest each other is taken.
  The point counts when they are within `--max-dh` metres, both under
  `--max-h`, and both read at least `--min-dbz` (DBZH).
- Per pair: the number of points, the median, quartiles and mean of A − B
  in dB over all times, and each time's median; pairs with fewer than
  `--min-n` points are listed apart.

Output: `<out>.md` (the report) and `<out>.json`. The engine applies no
correction; the numbers are for the human to judge whether one is worth
asking for. A few frames are not a day: read small counts as indicative.

  python3 scripts/radar-bias.py --store ~/.cache/omastorm-se/tilts --out review/s24b/bias
"""
import argparse
import datetime as dt
import json
import math
import os
import sqlite3
import struct
import zlib
from pathlib import Path

import numpy as np

SPHERE_M = 6_371_000.0
EARTH_M = SPHERE_M * 4.0 / 3.0
GAP_DEG = 0.75
CADENCE_MS = 5 * 60 * 1000
HEAD = struct.Struct("<4sIHIIffBqqd")
RAY = struct.Struct("<ffq")
NAMED = [("vara", "nohur"), ("angelholm", "dkste"), ("angelholm", "dksin"), ("vara", "dksin")]


def args():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    p.add_argument("--store", type=Path, default=cache / "omastorm-se" / "tilts")
    p.add_argument("--sites", type=Path, default=Path(__file__).resolve().parent.parent / "engine/data/sites.json")
    p.add_argument("--out", type=Path, default=Path("radar-bias"))
    p.add_argument("--min-dbz", type=float, default=20.0)
    p.add_argument("--max-dh", type=float, default=300.0, help="metres between the two beam centres")
    p.add_argument("--max-h", type=float, default=4000.0, help="metres above sea level")
    p.add_argument("--step", type=float, default=3.0, help="km between sample points")
    p.add_argument("--min-n", type=int, default=50)
    return p.parse_args()


def stations(path):
    """id -> (lat, lon, altM, range m), every radar of the table."""
    out = {}
    for s in json.loads(path.read_text())["sites"]:
        out[s["id"]] = (s["lat"], s["lon"], s.get("altM", 0.0), s.get("rangeKm", 240.0) * 1000.0)
    return out


def tilt(path):
    """A `.u8z` file (engine/src/tilts.rs `encode`): the scan's angle, gate
    geometry, sorted azimuths and codes (rays × gates), or None when it is not
    DBZH-coded (scale 2, offset 66)."""
    raw = zlib.decompress(path.read_bytes())
    magic, rays, gates, first, spacing, scale, offset, _, _, _, elangle = HEAD.unpack_from(raw)
    if magic != b"OMT1" or scale != 2.0 or offset != 66.0 or rays == 0 or gates == 0:
        return None
    at = HEAD.size
    az = np.array([RAY.unpack_from(raw, at + i * RAY.size)[0] for i in range(rays)], dtype=np.float64)
    at += rays * RAY.size
    codes = np.frombuffer(raw, dtype=np.uint8, count=rays * gates, offset=at).reshape(rays, gates)
    order = np.argsort(az, kind="stable")
    return {"deg": elangle, "first": float(first), "spacing": float(max(spacing, 1)), "gates": gates,
            "az": az[order], "codes": codes[order]}


def rows(az):
    """The azimuth lookup (docs/protocol.md): per tenth of a degree, the ray
    nearest its centre within 0.75°, else -1."""
    centre = (np.arange(3600) + 0.5) / 10.0
    n = len(az)
    after = np.searchsorted(az, centre, side="right") % n
    before = (after - 1) % n
    d = lambda i: np.minimum(np.abs(az[i] - centre), 360.0 - np.abs(az[i] - centre))  # noqa: E731
    pick = np.where(d(before) <= d(after), before, after)
    return np.where(np.minimum(d(before), d(after)) <= GAP_DEG, pick, -1)


def ground_and_bearing(lat0, lon0, lat, lon):
    p1, p2 = math.radians(lat0), np.radians(lat)
    dl = np.radians(lon - lon0)
    h = np.sin((p2 - p1) / 2) ** 2 + math.cos(p1) * np.cos(p2) * np.sin(dl / 2) ** 2
    s = 2 * SPHERE_M * np.arcsin(np.sqrt(np.clip(h, 0, 1)))
    b = np.degrees(np.arctan2(np.sin(dl) * np.cos(p2), math.cos(p1) * np.sin(p2) - math.sin(p1) * np.cos(p2) * np.cos(dl)))
    return s, np.mod(b, 360.0)


def place(scans, alt, s, bearing):
    """Per point and scan: beam centre above sea level (inf where the scan
    does not cover the point) and code (1 there)."""
    entry = np.floor(bearing * 10).astype(int) % 3600
    theta = s / EARTH_M
    heights, codes = [], []
    for t in scans:
        e = math.radians(t["deg"])
        c = np.cos(e + theta)
        r = EARTH_M * np.sin(theta) / c
        gate = np.rint((r - t["first"]) / t["spacing"]).astype(int)
        ray = t["rows"][entry]
        ok = (gate >= 0) & (gate < t["gates"]) & (ray >= 0) & (c > 1e-9)
        code = np.ones(len(s), dtype=np.uint8)
        code[ok] = t["codes"][ray[ok], gate[ok]]
        h = np.where(ok, alt + EARTH_M * math.cos(e) / c - EARTH_M, np.inf)
        heights.append(h)
        codes.append(code)
    return np.stack(heights, axis=1), np.stack(codes, axis=1)


def lens(a, b, step_km):
    """Sample points (lat, lon arrays) inside both radars' range."""
    (la, loa, _, ra), (lb, lob, _, rb) = a, b
    south = max(la - ra / 111_200, lb - rb / 111_200)
    north = min(la + ra / 111_200, lb + rb / 111_200)
    if south >= north:
        return np.empty(0), np.empty(0)
    lat = np.arange(south, north, step_km / 111.2)
    pts = []
    for y in lat:
        w = min(loa - ra / (111_200 * math.cos(math.radians(y))), lob - rb / (111_200 * math.cos(math.radians(y))))
        e = max(loa + ra / (111_200 * math.cos(math.radians(y))), lob + rb / (111_200 * math.cos(math.radians(y))))
        lon = np.arange(w, e, step_km / (111.2 * math.cos(math.radians(y))))
        pts.append(np.stack([np.full_like(lon, y), lon], axis=1))
    p = np.concatenate(pts)
    sa, _ = ground_and_bearing(la, loa, p[:, 0], p[:, 1])
    sb, _ = ground_and_bearing(lb, lob, p[:, 0], p[:, 1])
    keep = (sa <= ra) & (sb <= rb)
    return p[keep, 0], p[keep, 1]


def main():
    o = args()
    sites = stations(o.sites)
    db = sqlite3.connect(f"file:{o.store / 'index.sqlite'}?mode=ro", uri=True)
    by_time = {}
    for station, time_ms, file in db.execute("SELECT station, time_ms, file FROM tilts"):
        if station not in sites:
            continue
        t = (time_ms + 60_000) // CADENCE_MS * CADENCE_MS
        by_time.setdefault(t, {}).setdefault(station, []).append(file)
    cache, pairs = {}, {}

    def volume(station, t):
        key = (station, t)
        if key not in cache:
            scans = [x for x in (tilt(o.store / f) for f in by_time[t][station]) if x is not None]
            for x in scans:
                x["rows"] = rows(x["az"])
            cache[key] = sorted(scans, key=lambda x: x["deg"])
        return cache[key]

    min_code = 2 * o.min_dbz + 66
    for t in sorted(by_time):
        present = sorted(by_time[t])
        for i, a in enumerate(present):
            for b in present[i + 1:]:
                sa, sb = sites[a], sites[b]
                d, _ = ground_and_bearing(sa[0], sa[1], np.array([sb[0]]), np.array([sb[1]]))
                if d[0] >= sa[3] + sb[3]:
                    continue
                lat, lon = lens(sa, sb, o.step)
                if len(lat) == 0:
                    continue
                va, vb = volume(a, t), volume(b, t)
                if not va or not vb:
                    continue
                ha, ca = place(va, sa[2], *ground_and_bearing(sa[0], sa[1], lat, lon))
                hb, cb = place(vb, sb[2], *ground_and_bearing(sb[0], sb[1], lat, lon))
                ha = np.where(ha <= o.max_h, ha, np.inf)
                hb = np.where(hb <= o.max_h, hb, np.inf)
                with np.errstate(invalid="ignore"):  # inf − inf: neither scan covers the point
                    dh = np.abs(ha[:, :, None] - hb[:, None, :])
                dh = np.where(np.isfinite(dh), dh, np.inf)
                flat = dh.reshape(len(lat), -1).argmin(axis=1)
                ia, ib = np.divmod(flat, hb.shape[1])
                rows_ = np.arange(len(lat))
                best = dh[rows_, ia, ib]
                code_a, code_b = ca[rows_, ia].astype(float), cb[rows_, ib].astype(float)
                ok = (best <= o.max_dh) & (code_a >= min_code) & (code_b >= min_code)
                if not ok.any():
                    pairs.setdefault((a, b), {"diffs": [], "times": {}})
                    continue
                diff = (code_a[ok] - code_b[ok]) / 2.0
                entry = pairs.setdefault((a, b), {"diffs": [], "times": {}})
                entry["diffs"].extend(diff.tolist())
                entry["times"][t] = (len(diff), float(np.median(diff)))
    results = []
    for (a, b), e in sorted(pairs.items()):
        x = np.array(e["diffs"])
        row = {"a": a, "b": b, "n": int(len(x))}
        if len(x):
            q1, med, q3 = np.percentile(x, [25, 50, 75])
            row.update(median=round(float(med), 2), q1=round(float(q1), 2), q3=round(float(q3), 2),
                       mean=round(float(x.mean()), 2))
        row["times"] = {dt.datetime.fromtimestamp(t / 1000, dt.UTC).strftime("%Y-%m-%dT%H:%MZ"): {"n": n, "median": round(m, 2)}
                        for t, (n, m) in sorted(e["times"].items())}
        results.append(row)
    times = sorted(by_time)
    fmt = lambda t: dt.datetime.fromtimestamp(t / 1000, dt.UTC).strftime("%Y-%m-%d %H:%MZ")  # noqa: E731
    meta = {"store": str(o.store), "times": [fmt(t) for t in times], "radarsPerTime": {fmt(t): len(by_time[t]) for t in times},
            "minDbz": o.min_dbz, "maxDhM": o.max_dh, "maxHM": o.max_h, "stepKm": o.step, "minN": o.min_n}
    o.out.parent.mkdir(parents=True, exist_ok=True)
    Path(f"{o.out}.json").write_text(json.dumps({"meta": meta, "pairs": results}, indent=1))
    lines = ["# Radar pair offsets (report only; nothing is applied)", "",
             f"Store `{o.store}`, {len(times)} nominal times ({meta['times'][0] if times else '-'} to "
             f"{meta['times'][-1] if times else '-'}), up to {max(meta['radarsPerTime'].values(), default=0)} radars a time.",
             f"Points every {o.step:g} km inside both ranges; per point the two scans whose beam centres are nearest "
             f"each other, within {o.max_dh:g} m and under {o.max_h:g} m above sea level, both at least {o.min_dbz:g} dBZ. "
             "Difference A − B in dB (positive: A reads hotter).", ""]
    enough = [r for r in results if r["n"] >= o.min_n]
    few = [r for r in results if 0 < r["n"] < o.min_n]
    lines += ["## Named seams", "", "| A | B | points | median | quartiles | mean |", "|---|---|---:|---:|---|---:|"]
    for a, b in NAMED:
        r = next((r for r in results if {r["a"], r["b"]} == {a, b}), None)
        if r is None:
            lines.append(f"| {a} | {b} | no shared time | | | |")
        elif r["n"] == 0:
            lines.append(f"| {r['a']} | {r['b']} | 0 (no echo at both) | | | |")
        else:
            lines.append(f"| {r['a']} | {r['b']} | {r['n']} | {r['median']:+.1f} | {r['q1']:+.1f} … {r['q3']:+.1f} | {r['mean']:+.1f} |")
    lines += ["", f"## Pairs with at least {o.min_n} points, largest offset first", "",
              "| A | B | points | median | quartiles | mean | times |", "|---|---|---:|---:|---|---:|---:|"]
    for r in sorted(enough, key=lambda r: -abs(r["median"])):
        lines.append(f"| {r['a']} | {r['b']} | {r['n']} | {r['median']:+.1f} | {r['q1']:+.1f} … {r['q3']:+.1f} | {r['mean']:+.1f} | {len(r['times'])} |")
    lines += ["", f"## Fewer than {o.min_n} points (indicative only)", "",
              ", ".join(f"{r['a']}–{r['b']} {r['n']} ({r['median']:+.1f})" for r in few) or "none", "",
              f"Overlapping pairs with no point at {o.min_dbz:g} dBZ on both: "
              + (", ".join(f"{r['a']}–{r['b']}" for r in results if r["n"] == 0) or "none"), ""]
    Path(f"{o.out}.md").write_text("\n".join(lines))
    print("\n".join(lines))


if __name__ == "__main__":
    main()
