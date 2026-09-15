# The terrain grid for heights "above ground" (S30, docs/protocol.md
# Terrain): one mean terrain height per Web Mercator texel of 2,000 m (the
# composites' and My mosaic's lattice, counted from Mercator x = 0 and
# y = 0), over the box of every Nordic radar's reach, written to
# engine/data/terrain-nordic-2km.bin. Computed once, offline; the engine
# carries only the grid.
#
#   uv run --no-project --with numpy --with pillow python scripts/terrain-grid.py \
#     --cache /some/cache/dir
#
# Terrain: the Terrarium tiles scripts/blockage-tables.py uses (Mapzen's
# Terrain Tiles on AWS, open data, https://registry.opendata.aws/terrain-tiles/),
# zoom 8 (611.5 Mercator metres a pixel, about 300 m at 60 N), height =
# R*256 + G + B/256 - 32768 m. Their sources and licences are listed at
# https://github.com/tilezen/joerd/blob/master/docs/attribution.md; see
# data/README.md. Tiles are fetched once into --cache, one request each,
# and reused (the blockage tables' cache works as it is).
#
# The rule:
# - the box: every radar of engine/data/sites.json with a REACH_KM circle
#   (at least any radar's rangeKm) around it, on the 6,371 km sphere, out
#   to whole texels of the lattice;
# - only tiles within REACH_KM + a tile's diagonal of some radar are read;
#   texels no tile covers stay 0;
# - a texel's height is the mean of the zoom-8 pixels whose centres fall in
#   it (about 3.3 x 3.3 of them), sea and land below sea level as 0;
# - stored in steps of STEP_M (rounded, at most 255 steps), row 0 north.
#
# File (little-endian): b"OMTERR1\0", i32 col0 (the west edge / 2000 m),
# i32 north0 (the north edge / 2000 m), u32 width, u32 height, u32 texel
# metres (2000), u32 step in decimetres (100), then zlib of width x height
# bytes, each row delta-coded (byte minus the byte west of it, mod 256; the
# first of a row as it is), which deflates terrain better.
import argparse
import io
import json
import math
import os
import struct
import sys
import urllib.request
import zlib

import numpy as np
from PIL import Image

ZOOM = 8
URL = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png"
UA = "omastorm-se/S30 terrain grid (fork of https://omastorm.com)"
MERCATOR_R = 6_378_137.0
PIXEL_M = 2000.0
SPHERE_M = 6_371_000.0
REACH_KM = 250.0
STEP_M = 10.0
WORLD = 2.0 * math.pi * MERCATOR_R
HERE = os.path.dirname(os.path.abspath(__file__))
SITES = os.path.join(HERE, "..", "engine", "data", "sites.json")
OUT = os.path.join(HERE, "..", "engine", "data", "terrain-nordic-2km.bin")


def mercator_y(lat):
    return MERCATOR_R * math.log(math.tan(math.pi / 4.0 + math.radians(lat) / 2.0))


def mercator_x(lon):
    return MERCATOR_R * math.radians(lon)


def destination(lat, lon, bearing, d):
    phi, lam, th, de = math.radians(lat), math.radians(lon), math.radians(bearing), d / SPHERE_M
    phi2 = math.asin(math.sin(phi) * math.cos(de) + math.cos(phi) * math.sin(de) * math.cos(th))
    lam2 = lam + math.atan2(math.sin(th) * math.sin(de) * math.cos(phi),
                            math.cos(de) - math.sin(phi) * math.sin(phi2))
    return math.degrees(phi2), math.degrees(lam2)


def ground_m(lat1, lon1, lat2, lon2):
    p1, p2 = math.radians(lat1), math.radians(lat2)
    h = math.sin((p2 - p1) / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(math.radians(lon2 - lon1) / 2) ** 2
    return 2.0 * SPHERE_M * math.asin(min(1.0, math.sqrt(h)))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cache", required=True)
    ap.add_argument("--out", default=OUT)
    args = ap.parse_args()
    os.makedirs(args.cache, exist_ok=True)
    radars = [(s["lat"], s["lon"]) for s in json.load(open(SITES))["sites"]]
    reach = REACH_KM * 1000.0

    # The box of every reach circle, out to whole texels.
    xs, ys = [], []
    for lat, lon in radars:
        for b in range(0, 360, 2):
            la, lo = destination(lat, lon, b, reach)
            xs.append(mercator_x(lo))
            ys.append(mercator_y(la))
    col0 = math.floor(min(xs) / PIXEL_M)
    col1 = math.ceil(max(xs) / PIXEL_M)
    north0 = math.ceil(max(ys) / PIXEL_M)
    south0 = math.floor(min(ys) / PIXEL_M)
    width, height = col1 - col0, north0 - south0
    print(f"box: cols {col0}..{col1}, north {north0} south {south0}: {width} x {height} texels", file=sys.stderr)

    # Zoom-8 tiles over the box, near some radar.
    n = 2 ** ZOOM
    tile_m = WORLD / n
    tx0 = int((col0 * PIXEL_M + WORLD / 2) // tile_m)
    tx1 = int((col1 * PIXEL_M + WORLD / 2) // tile_m)
    ty0 = int((WORLD / 2 - north0 * PIXEL_M) // tile_m)
    ty1 = int((WORLD / 2 - south0 * PIXEL_M) // tile_m)

    def tile_lat(ty):
        y = WORLD / 2 - ty * tile_m
        return math.degrees(math.atan(math.sinh(y / MERCATOR_R)))

    def near(tx, ty):
        north, south = tile_lat(ty), tile_lat(ty + 1)
        west = math.degrees((tx * tile_m - WORLD / 2) / MERCATOR_R)
        east = math.degrees(((tx + 1) * tile_m - WORLD / 2) / MERCATOR_R)
        diag = ground_m(north, west, south, east)
        for lat, lon in radars:
            cl = min(max(lat, south), north)
            co = min(max(lon, west), east)
            if ground_m(lat, lon, cl, co) <= reach + diag:
                return True
        return False

    total = np.zeros(width * height)
    count = np.zeros(width * height)
    used = fetched = fetched_bytes = 0
    px = np.arange(256) + 0.5
    for ty in range(ty0, ty1 + 1):
        for tx in range(tx0, tx1 + 1):
            if not near(tx, ty):
                continue
            path = os.path.join(args.cache, f"{ZOOM}-{tx}-{ty}.png")
            if not os.path.exists(path):
                req = urllib.request.Request(URL.format(z=ZOOM, x=tx, y=ty), headers={"User-Agent": UA})
                with urllib.request.urlopen(req, timeout=60) as r:
                    data = r.read()
                with open(path + ".part", "wb") as f:
                    f.write(data)
                os.replace(path + ".part", path)
                fetched += 1
                fetched_bytes += len(data)
            used += 1
            rgb = np.asarray(Image.open(path).convert("RGB"), dtype=np.float64)
            h = rgb[:, :, 0] * 256.0 + rgb[:, :, 1] + rgb[:, :, 2] / 256.0 - 32768.0
            h = np.maximum(h, 0.0)
            # Pixel centres in Mercator metres, then the texel they fall in.
            x = (tx * 256 + px) / (256 * n) * WORLD - WORLD / 2
            y = WORLD / 2 - (ty * 256 + px) / (256 * n) * WORLD
            c = np.floor(x / PIXEL_M).astype(np.int64) - col0
            r = north0 - 1 - np.floor(y / PIXEL_M).astype(np.int64)
            okc, okr = (c >= 0) & (c < width), (r >= 0) & (r < height)
            if not okc.any() or not okr.any():
                continue
            idx = (r[okr][:, None] * width + c[okc][None, :]).ravel()
            vals = h[np.ix_(okr, okc)].ravel()
            total += np.bincount(idx, weights=vals, minlength=width * height)
            count += np.bincount(idx, minlength=width * height)
        print(f"  tile row {ty}: {used} tiles used, {fetched} fetched ({fetched_bytes:,} bytes)", file=sys.stderr)

    mean = np.where(count > 0, total / np.maximum(count, 1), 0.0)
    steps = np.clip(np.rint(mean / STEP_M), 0, 255).astype(np.uint8).reshape(height, width)
    delta = steps.copy()
    delta[:, 1:] = (steps[:, 1:].astype(np.int16) - steps[:, :-1].astype(np.int16)) % 256
    body = zlib.compress(delta.tobytes(), 9)
    head = b"OMTERR1\0" + struct.pack("<iiIIII", col0, north0, width, height, int(PIXEL_M), int(STEP_M * 10))
    with open(args.out + ".part", "wb") as f:
        f.write(head + body)
    os.replace(args.out + ".part", args.out)
    covered = int((count > 0).sum())
    print(json.dumps({
        "tilesUsed": used, "tilesFetched": fetched, "bytesFetched": fetched_bytes,
        "width": width, "height": height, "col0": col0, "north0": north0,
        "texelsCovered": covered, "rawBytes": width * height, "fileBytes": len(head) + len(body),
        "maxM": float(steps.max()) * STEP_M, "saturated": int((mean >= 255 * STEP_M).sum()),
    }))


if __name__ == "__main__":
    main()
