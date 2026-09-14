# Blockage tables for "Clear view" (S20, docs/protocol.md Products,
# HYBRID): per radar and degree of azimuth, the lowest scan angle the
# terrain does not block, in tenths of a degree, written to
# engine/data/blockage.json. Computed once, offline, from a terrain model;
# the engine and the plugin carry only the 360 numbers per radar.
#
#   uv run --no-project --with numpy --with pillow python scripts/blockage-tables.py \
#     --cache /some/cache/dir vara hurum ...
#
# Terrain: Mapzen's Terrarium tiles on AWS (Terrain Tiles, open data,
# https://registry.opendata.aws/terrain-tiles/), zoom 8 (about 300 m a pixel
# at 60 N), height = R*256 + G + B/256 - 32768 m. Their sources are listed at
# https://github.com/tilezen/joerd/blob/master/docs/attribution.md (in the
# Nordic countries EU-DEM, SRTM, GMTED2010 and the national models they
# credit). Tiles are fetched once into --cache, one request each, and reused.
#
# The rule, per radar at antenna height altM (engine/data/sites.json):
# - five rays per degree of azimuth (d + 0.1, 0.3, ... 0.9), each sampled
#   every 250 m of ground distance from 2 km to 100 km (the nearest 2 km are
#   the site itself, finer than the terrain model);
# - at ground distance s the terrain's elevation angle is the scan angle
#   whose beam centre, under the lookup rule's 4/3 earth
#   (h = R cos e / cos(e + s/R) - R, R = 6371 km x 4/3), is at the terrain's
#   height above the antenna: tan e = ((h+R) cos(s/R) - R) / ((h+R) sin(s/R));
# - a degree's terrain angle is the highest over its rays and distances, and
#   its entry is ceil((angle + 0.2) x 10) tenths, at least 0: a scan is clear
#   where its beam centre passes 0.2 deg above everything in that direction
#   (about a fifth of a 1 deg beam), and the engine picks the lowest scan
#   whose angle, rounded to tenths, is at or above the entry.
import argparse
import datetime
import io
import json
import math
import os
import sys
import urllib.request

import numpy as np
from PIL import Image

R = 6_371_000.0 * 4.0 / 3.0
ZOOM = 8
URL = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png"
UA = "omastorm-se/S20 blockage tables (fork of https://omastorm.com)"
MARGIN_DEG = 0.2
NEAR_M, FAR_M, STEP_M = 2_000.0, 100_000.0, 250.0
EARTH_M = 6_371_000.0  # for moving along the ground


def tile_xy(lat, lon, z):
    n = 2 ** z
    x = (lon + 180.0) / 360.0 * n
    y = (1.0 - math.asinh(math.tan(math.radians(lat))) / math.pi) / 2.0 * n
    return x, y


class Terrain:
    def __init__(self, cache):
        self.cache, self.tiles, self.fetched = cache, {}, 0
        os.makedirs(cache, exist_ok=True)

    def tile(self, x, y):
        key = (x, y)
        if key not in self.tiles:
            path = os.path.join(self.cache, f"{ZOOM}-{x}-{y}.png")
            if not os.path.exists(path):
                req = urllib.request.Request(URL.format(z=ZOOM, x=x, y=y), headers={"User-Agent": UA})
                with urllib.request.urlopen(req, timeout=60) as r:
                    data = r.read()
                open(path, "wb").write(data)
                self.fetched += 1
            rgb = np.asarray(Image.open(path).convert("RGB"), dtype=np.float64)
            self.tiles[key] = rgb[:, :, 0] * 256.0 + rgb[:, :, 1] + rgb[:, :, 2] / 256.0 - 32768.0
        return self.tiles[key]

    def height(self, lat, lon):
        """Bilinear terrain height (m), sea and below clamped at 0."""
        fx, fy = tile_xy(lat, lon, ZOOM)
        px, py = fx * 256.0 - 0.5, fy * 256.0 - 0.5
        x0, y0 = math.floor(px), math.floor(py)
        tx, ty = px - x0, py - y0
        vals = []
        for dy in (0, 1):
            for dx in (0, 1):
                gx, gy = x0 + dx, y0 + dy
                t = self.tile(gx // 256, gy // 256)
                vals.append(t[gy % 256, gx % 256])
        top = vals[0] * (1 - tx) + vals[1] * tx
        bottom = vals[2] * (1 - tx) + vals[3] * tx
        return max(0.0, top * (1 - ty) + bottom * ty)


def destination(lat, lon, bearing_deg, dist_m):
    p1, l1, b, d = math.radians(lat), math.radians(lon), math.radians(bearing_deg), dist_m / EARTH_M
    p2 = math.asin(math.sin(p1) * math.cos(d) + math.cos(p1) * math.sin(d) * math.cos(b))
    l2 = l1 + math.atan2(math.sin(b) * math.sin(d) * math.cos(p1), math.cos(d) - math.sin(p1) * math.sin(p2))
    return math.degrees(p2), math.degrees(l2)


def terrain_angle(h, s):
    """The scan angle (deg) whose beam centre is h metres above the antenna
    at ground distance s, under the 4/3 earth."""
    t = s / R
    return math.degrees(math.atan(((h + R) * math.cos(t) - R) / ((h + R) * math.sin(t))))


def table(terrain, lat, lon, alt):
    out, highest = [], []
    for d in range(360):
        worst = -90.0
        for sub in (0.1, 0.3, 0.5, 0.7, 0.9):
            s = NEAR_M
            while s <= FAR_M:
                plat, plon = destination(lat, lon, d + sub, s)
                worst = max(worst, terrain_angle(terrain.height(plat, plon) - alt, s))
                s += STEP_M
        highest.append(worst)
        out.append(min(255, max(0, math.ceil((worst + MARGIN_DEG) * 10.0 - 1e-9))))
    return out, highest


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--cache", required=True)
    p.add_argument("--out", default="engine/data/blockage.json")
    p.add_argument("radars", nargs="+")
    args = p.parse_args()
    sites = {s["id"]: s for s in json.load(open("engine/data/sites.json"))["sites"]}
    terrain = Terrain(args.cache)
    radars, report = {}, {}
    for rid in args.radars:
        s = sites[rid]
        tenths, angles = table(terrain, s["lat"], s["lon"], s["altM"])
        radars[rid] = tenths
        report[rid] = {
            "blockedAbove0.5": sum(1 for t in tenths if t > 5),
            "maxTenths": max(tenths),
            "maxTerrainDeg": round(max(angles), 2),
            "medianTerrainDeg": round(sorted(angles)[180], 2),
        }
        print(rid, json.dumps(report[rid]), file=sys.stderr)
    doc = {
        "source": "Terrain Tiles on AWS (Mapzen Terrarium, zoom 8; sources: "
                  "https://github.com/tilezen/joerd/blob/master/docs/attribution.md)",
        "rule": f"per degree of azimuth, ceil((highest terrain elevation angle within {NEAR_M/1000:g}-{FAR_M/1000:g} km, "
                f"4/3 earth, from the antenna at altM) + {MARGIN_DEG} deg) in tenths of a degree; "
                "scripts/blockage-tables.py",
        "retrieved": datetime.date.today().isoformat(),
        "radars": {rid: radars[rid] for rid in sorted(radars)},
    }
    with open(args.out, "w") as fh:
        fh.write("{\n")
        for key in ("source", "rule", "retrieved"):
            fh.write(f"  {json.dumps(key)}: {json.dumps(doc[key])},\n")
        fh.write('  "radars": {\n')
        items = list(doc["radars"].items())
        for i, (rid, t) in enumerate(items):
            fh.write(f"    {json.dumps(rid)}: {json.dumps(t, separators=(',', ':'))}{',' if i + 1 < len(items) else ''}\n")
        fh.write("  }\n}\n")
    print(json.dumps({"tilesFetched": terrain.fetched, "tilesUsed": len(terrain.tiles), "report": report}))


main()
