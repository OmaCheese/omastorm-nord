#!/usr/bin/env python3
"""S24c answer key: a 10 km tower seen by two synthetic radars, its sections
and profiles computed independently of the engine (numpy), from the rule in
docs/protocol.md "Sections and profiles".

Writes, under engine/tests/sections/:
  tower-volumes.zz  zlib of the two radars' volumes, u8 codes, ordered
                    radar (A, B) x scan x ray (360, azimuth k + 0.5) x gate
  tower-key.json    the scenario and the expected cuts and profiles

The engine's test (grid3d::tests::a_tower_seen_by_two_radars_matches_the_numpy_key)
reads both, places the radars with its own Layout, fills its own grid and
compares. Run from the repository root:

  python3 scripts/section-key.py
"""
from __future__ import annotations

import json
import math
import zlib
from pathlib import Path

import numpy as np

OUT = Path(__file__).resolve().parent.parent / "engine" / "tests" / "sections"

EARTH_M = 6_371_000.0 * 4.0 / 3.0   # the 4/3 earth of the lookup rule
SPHERE_M = 6_371_000.0             # the great-circle sphere
MERC_R = 6_378_137.0
PIXEL_M = 2000.0                   # the products' lattice
FACTOR = 2                         # a column is 2 x 2 of those texels
LEVELS, LEVEL_M = 24, 500.0
COLUMN_M, MAX_COLUMNS = 2000.0, 300
UNIT_M = 10.0
HALF_BEAM = math.radians(0.5)

ANGLES = [0.5, 1.0, 1.5, 2.0, 2.5, 4.0, 8.0, 14.0, 24.0, 40.0]
GATES, FIRST_M, SPACING_M = 300, 250, 500
RADARS = [
    {"id": "twra", "lat": 59.0, "lon": 13.0, "altM": 100.0, "rangeKm": 150.0},
    {"id": "twrb", "lat": 59.0, "lon": 15.0, "altM": 300.0, "rangeKm": 150.0},
]
TOWER = {"lat": 59.25, "lon": 14.2, "radiusM": 6000.0, "topM": 10000.0, "code": 166}
# Radar B's rays 200-209 are no data (code 1), as a blocked sector.
BLOCKED = {"radar": "twrb", "rays": [200, 209]}

LINES = [
    {"name": "west-east through the tower", "from": {"lat": 59.25, "lon": 13.6}, "to": {"lat": 59.25, "lon": 14.8}},
    {"name": "south-north through the tower", "from": {"lat": 58.9, "lon": 14.2}, "to": {"lat": 59.6, "lon": 14.2}},
    {"name": "B's blocked sector", "from": {"lat": 58.7, "lon": 14.3}, "to": {"lat": 58.3, "lon": 14.0}},
    {"name": "over 600 km, mostly outside", "from": {"lat": 57.0, "lon": 10.0}, "to": {"lat": 62.5, "lon": 18.0}},
]
POINTS = [
    {"name": "tower centre", "lat": 59.25, "lon": 14.2},
    {"name": "tower edge", "lat": 59.25, "lon": 14.29},
    {"name": "clear air between the radars", "lat": 59.0, "lon": 14.0},
    {"name": "under B's blocked sector", "lat": 58.6, "lon": 14.25},
    {"name": "outside the grid", "lat": 65.0, "lon": 20.0},
]


def rust_round(x):
    """f64::round: half away from zero."""
    return np.where(x < 0, -np.floor(-x + 0.5), np.floor(x + 0.5))


def destination(lat, lon, bearing, d):
    phi, lam, th, dl = np.radians(lat), np.radians(lon), np.radians(bearing), d / SPHERE_M
    phi2 = np.arcsin(np.sin(phi) * np.cos(dl) + np.cos(phi) * np.sin(dl) * np.cos(th))
    lam2 = lam + np.arctan2(np.sin(th) * np.sin(dl) * np.cos(phi), np.cos(dl) - np.sin(phi) * np.sin(phi2))
    return np.degrees(phi2), np.degrees(lam2)


def haversine(lat1, lon1, lat2, lon2):
    p1, p2 = np.radians(lat1), np.radians(lat2)
    dp = (p2 - p1) / 2.0
    dl = np.radians(lon2 - lon1) / 2.0
    h = np.sin(dp) ** 2 + np.cos(p1) * np.cos(p2) * np.sin(dl) ** 2
    return 2.0 * SPHERE_M * np.arcsin(np.sqrt(np.clip(h, 0.0, 1.0)))


def bearing(lat1, lon1, lat2, lon2):
    p1, p2 = np.radians(lat1), np.radians(lat2)
    dl = np.radians(lon2 - lon1)
    return np.degrees(np.arctan2(np.sin(dl) * np.cos(p2), np.cos(p1) * np.sin(p2) - np.sin(p1) * np.cos(p2) * np.cos(dl)))


def merc_x(lon):
    return MERC_R * np.radians(lon)


def merc_y(lat):
    return MERC_R * np.log(np.tan(math.pi / 4 + np.radians(lat) / 2.0))


def merc_lat(y):
    return np.degrees(np.arctan(np.sinh(y / MERC_R)))


# ---------------------------------------------------------------------------
# The volumes: the tower, measured by each gate's beam centre
# ---------------------------------------------------------------------------

def volume(radar):
    codes = np.zeros((len(ANGLES), 360, GATES), dtype=np.uint8)
    az = np.arange(360) + 0.5
    r = FIRST_M + SPACING_M * np.arange(GATES, dtype=np.float64)
    for t, deg in enumerate(ANGLES):
        e = math.radians(deg)
        s = EARTH_M * np.arctan2(r * math.cos(e), EARTH_M + r * math.sin(e))
        h = radar["altM"] + EARTH_M * math.cos(e) / np.cos(e + s / EARTH_M) - EARTH_M
        lat2, lon2 = destination(radar["lat"], radar["lon"], az[:, None], s[None, :])
        inside = haversine(lat2, lon2, TOWER["lat"], TOWER["lon"]) <= TOWER["radiusM"]
        codes[t] = np.where(inside & (h[None, :] <= TOWER["topM"]), TOWER["code"], 0)
        if radar["id"] == BLOCKED["radar"]:
            a, b = BLOCKED["rays"]
            codes[t, a:b + 1, :] = 1
    return codes


# ---------------------------------------------------------------------------
# Placement (the layout's own arithmetic) and the fill
# ---------------------------------------------------------------------------

def layout_edges():
    """The west and north lattice edges of the two reach circles' box."""
    xs, ys = [], []
    for radar in RADARS:
        steps = np.arange(360, dtype=np.float64)
        la, lo = destination(radar["lat"], radar["lon"], steps, radar["rangeKm"] * 1000.0)
        xs.append(merc_x(lo))
        ys.append(merc_y(la))
    x0 = min(x.min() for x in xs)
    y1 = max(y.max() for y in ys)
    return math.floor(x0 / PIXEL_M) * PIXEL_M, math.ceil(y1 / PIXEL_M) * PIXEL_M


def texels(radar, mx_west, my_north):
    """Every 2,000 m texel within the radar's reach: global lattice (i, j),
    ground distance units and bearing tenths, as the layout computes them."""
    reach = radar["rangeKm"] * 1000.0
    box = 1.0 + reach / 111_000.0 / math.cos(math.radians(radar["lat"] + 2))
    i = np.arange(math.floor(merc_x(radar["lon"] - box) / PIXEL_M), math.ceil(merc_x(radar["lon"] + box) / PIXEL_M))
    j = np.arange(math.floor(merc_y(radar["lat"] - 2) / PIXEL_M), math.ceil(merc_y(radar["lat"] + 2) / PIXEL_M))
    c = (i - round(mx_west / PIXEL_M)).astype(np.float64)
    rr = (round(my_north / PIXEL_M) - 1 - j).astype(np.float64)
    lam = (mx_west + (c + 0.5) * PIXEL_M) / MERC_R
    phi2 = np.radians(merc_lat(my_north - (rr + 0.5) * PIXEL_M))
    phi1, lam1 = math.radians(radar["lat"]), math.radians(radar["lon"])
    dl = lam[None, :] - lam1
    half = np.sin((phi2[:, None] - phi1) / 2.0)
    hav = np.clip(half * half + math.cos(phi1) * np.cos(phi2)[:, None] * np.sin(dl / 2.0) ** 2, 0.0, 1.0)
    s = 2.0 * SPHERE_M * np.arcsin(np.sqrt(hav))
    brg = np.degrees(np.arctan2(np.sin(dl) * np.cos(phi2)[:, None],
                                math.cos(phi1) * np.sin(phi2)[:, None] - math.sin(phi1) * np.cos(phi2)[:, None] * np.cos(dl)))
    brg = np.mod(brg, 360.0)
    ok = s <= reach
    jj, ii = np.meshgrid(j, i, indexing="ij")
    d = rust_round(s / UNIT_M).astype(np.int64)
    az = (np.floor(brg * 10.0).astype(np.int64)) % 3600
    return ii[ok], jj[ok], d[ok], az[ok]


def beam(deg, reach, alt):
    """Per distance unit: gate (-1 none), and the levels the beam covers."""
    units = int(rust_round(np.array(reach / UNIT_M))) + 1
    e = math.radians(deg)
    d = np.arange(units, dtype=np.float64)
    theta = d * UNIT_M / EARTH_M
    c = np.cos(e + theta)
    r = EARTH_M * np.sin(theta) / c
    g = rust_round((r - FIRST_M) / SPACING_M)
    gate = np.where((g >= 0) & (g < GATES), g, -1).astype(np.int64)
    centre = alt + (EARTH_M * math.cos(e) / c - EARTH_M)
    w = r * math.tan(HALF_BEAM)
    lo_m, hi_m = centre - w, centre + w
    lo = np.maximum(np.floor(lo_m / LEVEL_M), 0).astype(np.int64)
    hi = np.minimum(np.ceil(hi_m / LEVEL_M).astype(np.int64) - 1, LEVELS - 1)
    valid = (gate >= 0) & (hi_m > 0) & (lo_m < LEVELS * LEVEL_M) & (lo <= hi)
    return gate, lo, hi, valid


def fill(volumes):
    mx_west, my_north = layout_edges()
    placed = [texels(r, mx_west, my_north) for r in RADARS]
    gi = np.concatenate([p[0] for p in placed]) // FACTOR
    gj = np.concatenate([p[1] for p in placed]) // FACTOR
    ci0, cj0 = gi.min(), gj.min()
    shape = (gj.max() - cj0 + 1, gi.max() - ci0 + 1, LEVELS)
    codes = np.zeros(shape, dtype=np.int64)
    counts = np.zeros(shape, dtype=np.int64)
    fed = np.zeros(shape, dtype=np.int64)
    masks = np.zeros(shape[:2], dtype=np.int64)
    for index, (radar, (i, j, d, az)) in enumerate(zip(RADARS, placed)):
        reach = radar["rangeKm"] * 1000.0
        ci, cj = i // FACTOR - ci0, j // FACTOR - cj0
        row = az // 10  # rays at k + 0.5: the nearest to entry az is az // 10
        reached = np.zeros(shape[:2], dtype=bool)
        samples = []
        for t, deg in enumerate(ANGLES):
            gate, lo, hi, valid = beam(deg, reach, radar["altM"])
            ok = valid[d]
            code = np.where(ok, volumes[index][t, row, np.maximum(gate[d], 0)], 1)
            ok &= code != 1
            samples.append((ci[ok], cj[ok], lo[d][ok], hi[d][ok], code[ok]))
            reached[cj[ok], ci[ok]] = True
        # This radar's bit among the column's radars: how many earlier radars reached it.
        local = np.zeros(shape[:2], dtype=np.int64)
        for k in range(index):
            local += (masks >> k) & 1
        bit = np.where(local < 8, 1 << np.minimum(local, 7), 0)
        for ci_, cj_, lo_, hi_, code_ in samples:
            for k in range(LEVELS):
                m = (lo_ <= k) & (k <= hi_)
                np.maximum.at(codes[:, :, k], (cj_[m], ci_[m]), code_[m])
                np.add.at(counts[:, :, k], (cj_[m], ci_[m]), 1)
                np.bitwise_or.at(fed[:, :, k], (cj_[m], ci_[m]), bit[cj_[m], ci_[m]])
        masks |= reached.astype(np.int64) << index
    return codes, np.minimum(counts, 255), fed, masks, (ci0, cj0)


def column_at(lat, lon, shape, origin):
    i = math.floor(float(merc_x(lon)) / PIXEL_M)
    j = math.floor(float(merc_y(lat)) / PIXEL_M)
    c, r = i // FACTOR - origin[0], j // FACTOR - origin[1]
    if 0 <= c < shape[1] and 0 <= r < shape[0]:
        return r, c
    return None


def main():
    volumes = [volume(r) for r in RADARS]
    codes, counts, fed, masks, origin = fill(volumes)
    ids = [r["id"] for r in RADARS]

    def names(mask):
        return [ids[k] for k in range(len(ids)) if (mask >> k) & 1]

    cuts = []
    for line in LINES:
        a, b = line["from"], line["to"]
        length = float(haversine(a["lat"], a["lon"], b["lat"], b["lon"]))
        n = min(max(math.ceil(length / COLUMN_M), 1), MAX_COLUMNS)
        step = length / n
        brg = float(bearing(a["lat"], a["lon"], b["lat"], b["lon"]))
        out = np.ones((LEVELS, n), dtype=np.int64)
        mask = 0
        for i in range(n):
            la, lo = destination(a["lat"], a["lon"], brg, (i + 0.5) * step)
            at = column_at(float(la), float(lo), codes.shape, origin)
            if at is None:
                continue
            r, c = at
            mask |= int(masks[r, c])
            for k in range(LEVELS):
                if counts[r, c, k] > 0:
                    out[LEVELS - 1 - k, i] = codes[r, c, k]
        cuts.append({**line, "columns": n, "lengthM": length, "codes": out.flatten().tolist(), "radars": names(mask)})

    profiles = []
    for p in POINTS:
        at = column_at(p["lat"], p["lon"], codes.shape, origin)
        if at is None:
            profiles.append({**p, "status": "outside", "levels": [], "radars": []})
            continue
        r, c = at
        column = names(int(masks[r, c]))
        levels = []
        for k in range(LEVELS):
            bits = int(fed[r, c, k])
            levels.append({"code": int(codes[r, c, k]), "samples": int(counts[r, c, k]),
                           "radars": [rid for b, rid in enumerate(column[:8]) if (bits >> b) & 1]})
        profiles.append({**p, "status": "ready", "levels": levels, "radars": column})

    OUT.mkdir(parents=True, exist_ok=True)
    blob = b"".join(v.tobytes() for v in volumes)
    (OUT / "tower-volumes.zz").write_bytes(zlib.compress(blob, 9))
    key = {
        "about": "scripts/section-key.py: a 10 km tower seen by two radars (S24c answer key)",
        "radars": RADARS, "angles": ANGLES, "gates": GATES, "firstGateM": FIRST_M, "gateSpacingM": SPACING_M,
        "tower": TOWER, "blocked": BLOCKED, "cuts": cuts, "profiles": profiles,
    }
    (OUT / "tower-key.json").write_text(json.dumps(key, separators=(",", ":")) + "\n")
    centre = next(p for p in profiles if p["name"] == "tower centre")
    top = max((k for k, lv in enumerate(centre["levels"]) if lv["code"] >= 2), default=None)
    print(f"volumes {len(blob)} bytes -> {(OUT / 'tower-volumes.zz').stat().st_size} zz; "
          f"grid {codes.shape}; tower centre echo to level {top} ({(top + 1) * 500 if top is not None else 0} m), "
          f"radars {centre['radars']}; cuts {[c['columns'] for c in cuts]}")


if __name__ == "__main__":
    main()
