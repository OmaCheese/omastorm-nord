#!/usr/bin/env bash
# One-off fixture script (development only): writes the `ord` rows of
# engine/data/sites.json, the 29 radars of Norway (12), Finland (12) and
# Denmark (5), from EUMETNET Open Radar Data's public 24-hour S3 cache
# (DEC-13). The SMHI rows are kept as they are.
#
# For each radar it lists today's files under YYYY/MM/DD/<CC>/<nod>/, takes
# the newest file the engine would read (quantities holding DBZH, else TH;
# per nominal time the lowest elevation), and opens it over HTTP range reads
# (64 KB blocks, not the whole file). The position comes from the root
# /where (lat, lon, height) and the identifiers from /what source; rangeKm
# is the lowest tilt's far edge, rstart + nbins x rscale. The display name
# is the radar's own (MET Norway's files carry no PLC, so the names are
# written here from ORD's location list and WMO OSCAR), and the region is the
# GeoNames admin-1 name of the nearest cities5000 place in the same country,
# so it reads like the location picker's regions (data/raw, run
# scripts/extract-fixtures.sh first).
#
# Every S3 request is counted and printed. The cache has no rate limit
# (no X-RateLimit headers); the ORD API is not called. Launch, setup and the
# checks never call this; rerun it only to take a new snapshot, then review
# the diff.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v uv >/dev/null 2>&1 || {
  printf 'uv is required (it supplies h5py for this one run).\n' >&2
  exit 1
}
[ -f data/raw/cities5000.txt ] || {
  printf 'data/raw/cities5000.txt is missing: run scripts/extract-fixtures.sh first.\n' >&2
  exit 1
}
uv run --quiet --no-project --with h5py python - <<'PY'
import datetime, io, json, math, re, statistics, urllib.parse, urllib.request
import xml.etree.ElementTree as ET

import h5py

CACHE = "https://s3.waw3-1.cloudferro.com/openradar-24h"
S3 = "{http://s3.amazonaws.com/doc/2006-03-01/}"
ATTRIBUTION = {"NO": "MET Norway, CC BY 4.0", "FI": "FMI, CC BY 4.0", "DK": "DMI, CC BY 4.0"}
# node code -> display name. FI and DK from ORD's location list
# (collections/observations/locations, 2026-09-14), NO from WMO OSCAR's
# station names (ORD lists the Norwegian radars without one).
SITES = {
    "NO": {
        "noand": "Andøya", "nober": "Berlevåg", "nobml": "Bømlo", "nohas": "Hasvik",
        "nohfj": "Hafjell", "nohgb": "Hægebostad", "nohur": "Hurum", "norsa": "Rissa",
        "norsg": "Rássegálvárri", "norst": "Røst", "nosmn": "Sømna", "nosta": "Stad",
    },
    "FI": {
        "fianj": "Anjalankoski", "fikan": "Kankaanpää", "fikau": "Kaunispää",
        "fikes": "Kesälahti", "fikor": "Korppoo", "fikuo": "Kuopio", "filuo": "Luosto",
        "finur": "Nurmes", "fipet": "Petäjävesi", "fiuta": "Utajärvi", "fivih": "Vihti",
        "fivim": "Vimpeli",
    },
    "DK": {
        "dkbor": "Bornholm", "dkrom": "Rømø", "dksam": "Samsø", "dksin": "Sindal",
        "dkste": "Stevns",
    },
}
REQUESTS = {"list": 0, "range": 0}
# The engine's file pattern (engine/src/providers/ord.rs `Listed::parse`).
NAME = re.compile(r"^(?P<nod>[a-z]{5})@(?P<t>\d{8}T\d{4})@(?P<el>[-\d._]+)@(?P<q>[A-Z0-9_]+)\.h5$")


def text(v):
    if isinstance(v, bytes):
        v = v.decode("ascii", "replace")
    return str(v).rstrip("\0").strip()


def list_keys(prefix, delimiter=None):
    """Every key (or common prefix, with a delimiter) under prefix."""
    out, token = [], None
    while True:
        query = {"list-type": "2", "prefix": prefix}
        if delimiter:
            query["delimiter"] = delimiter
        if token:
            query["continuation-token"] = token
        with urllib.request.urlopen(f"{CACHE}/?{urllib.parse.urlencode(query)}", timeout=30) as reply:
            root = ET.fromstring(reply.read())
        REQUESTS["list"] += 1
        tag = "CommonPrefixes/" + S3 + "Prefix" if delimiter else "Contents/" + S3 + "Key"
        out += [e.text for e in root.findall(S3 + tag.replace("/", "/", 1))]
        if root.findtext(S3 + "IsTruncated") != "true":
            return out
        token = root.findtext(S3 + "NextContinuationToken")


class Ranged(io.RawIOBase):
    """A read-only, seekable view of a URL through 64 KB Range requests."""

    BLOCK = 65536

    def __init__(self, url):
        self.url, self.pos, self.blocks = url, 0, {}
        self.size = None
        self.block(0)

    def readable(self):
        return True

    def seekable(self):
        return True

    def tell(self):
        return self.pos

    def seek(self, offset, whence=io.SEEK_SET):
        base = {io.SEEK_SET: 0, io.SEEK_CUR: self.pos, io.SEEK_END: self.size}[whence]
        self.pos = base + offset
        return self.pos

    def block(self, index):
        if index not in self.blocks:
            start = index * self.BLOCK
            end = start + self.BLOCK - 1 if self.size is None else min(start + self.BLOCK, self.size) - 1
            request = urllib.request.Request(self.url, headers={"Range": f"bytes={start}-{end}"})
            with urllib.request.urlopen(request, timeout=30) as reply:
                if reply.status != 206:
                    raise SystemExit(f"{self.url}: expected 206 for a Range request, got {reply.status}")
                self.size = int(reply.headers["content-range"].rsplit("/", 1)[1])
                self.blocks[index] = reply.read()
            REQUESTS["range"] += 1
        return self.blocks[index]

    def readinto(self, buffer):
        want = min(len(buffer), max(0, self.size - self.pos))
        got = 0
        while got < want:
            index, skip = divmod(self.pos, self.BLOCK)
            chunk = self.block(index)[skip:skip + want - got]
            buffer[got:got + len(chunk)] = chunk
            got += len(chunk)
            self.pos += len(chunk)
        return got


def newest_readable(keys):
    """(key, nominal time) of the newest file the engine would read."""
    by_time = {}
    for key in keys:
        m = NAME.match(key.rsplit("/", 1)[1])
        if not m:
            continue
        quantities = m["q"].split("_")
        rank = 0 if "DBZH" in quantities else 1 if "TH" in quantities else None
        if rank is None:
            continue
        lowest = min(float(e) for e in m["el"].split("_"))
        by_time.setdefault(m["t"], []).append((rank, lowest, key))
    times = sorted(by_time)
    # The newest time may still be missing its lowest scan (FI publishes
    # one file per elevation): take the newest complete one.
    for t in reversed(times[:-1] or times):
        return min(by_time[t])[2], t, times
    raise SystemExit("no readable file")


def regions():
    """Admin-1 names by 'CC.code', and the cities5000 places of NO/FI/DK."""
    names = {}
    for line in open("data/raw/admin1CodesASCII.txt", encoding="utf-8"):
        code, name = line.split("\t")[:2]
        names[code] = name
    places = []
    for line in open("data/raw/cities5000.txt", encoding="utf-8"):
        f = line.rstrip("\n").split("\t")
        if f[8] in SITES:
            places.append((float(f[4]), float(f[5]), f[8], f"{f[8]}.{f[10]}", f[1]))
    return names, places


def nearest(places, cc, lat, lon):
    def km(p):
        dlat = math.radians(p[0] - lat)
        dlon = math.radians(p[1] - lon) * math.cos(math.radians(lat))
        return 6371 * math.hypot(dlat, dlon)
    return min((p for p in places if p[2] == cc), key=km)


admin1, places = regions()
today = datetime.datetime.now(datetime.UTC)
day = today.strftime("%Y/%m/%d")
rows = []
for cc, sites in SITES.items():
    listed = sorted(p.rstrip("/").rsplit("/", 1)[1] for p in list_keys(f"{day}/{cc}/", "/"))
    if listed != sorted(sites):
        raise SystemExit(f"{cc}: the cache lists {listed}, this script {sorted(sites)}")
    for nod, name in sites.items():
        kinds = [p.rstrip("/").rsplit("/", 1)[1] for p in list_keys(f"{day}/{cc}/{nod}/", "/")]
        if len(kinds) != 1:
            raise SystemExit(f"{nod}: expected one object type, found {kinds}")
        source_id = f"{cc}/{nod}/{kinds[0]}"
        keys = list_keys(f"{day}/{source_id}/")
        key, nominal, times = newest_readable(keys)
        stamps = [datetime.datetime.strptime(t, "%Y%m%dT%H%M") for t in times]
        gaps = [(b - a).total_seconds() / 60 for a, b in zip(stamps, stamps[1:])]
        cadence = statistics.median(gaps) if gaps else None
        with h5py.File(Ranged(f"{CACHE}/{key}"), "r") as f:
            where = f["where"].attrs
            ids = dict(p.split(":", 1) for p in text(f["what"].attrs["source"]).split(",") if ":" in p)
            tilts = sorted(
                (float(f[k]["where"].attrs["elangle"]), int(k[7:]), k)
                for k in f if k.startswith("dataset")
            )
            elangle, number, lowest = tilts[0]
            w = f[lowest]["where"].attrs
            nbins, rscale, rstart = int(w["nbins"]), float(w["rscale"]), float(w["rstart"])
            quantities = [text(f[lowest][d]["what"].attrs["quantity"]) for d in f[lowest] if d.startswith("data")]
            q = "DBZH" if "DBZH" in quantities else "TH"
            dtype = str(f[lowest][f"data{quantities.index(q) + 1}"]["data"].dtype)
            lat, lon, alt = float(where["lat"]), float(where["lon"]), float(where["height"])
            conventions = text(f.attrs.get("Conventions", ""))
            obj = text(f["what"].attrs["object"])
        if ids.get("NOD", nod) != nod:
            raise SystemExit(f"{key}: NOD {ids.get('NOD')} is not {nod}")
        place = nearest(places, cc, lat, lon)
        range_km = round(rstart + nbins * rscale / 1000.0, 3)
        rows.append({
            "id": nod,
            "name": name,
            "state": admin1.get(place[3], ""),
            "lat": round(lat, 6),
            "lon": round(lon, 6),
            "altM": round(alt, 1),
            "provider": "ord",
            "country": cc,
            "rangeKm": range_km,
            "attribution": ATTRIBUTION[cc],
            "nod": nod,
            "sourceId": source_id,
            "wmo": ids.get("WMO", ""),
            "rad": ids.get("RAD", ""),
            "odim": f"{conventions} {obj}",
            "lowestTilt": {
                "elangle": round(elangle, 3), "dataset": number, "quantity": q, "dtype": dtype,
                "nbins": nbins, "rscaleM": rscale, "rstartKm": rstart,
            },
            "cadenceMin": cadence,
            "newest": key.rsplit("/", 1)[1],
        })
        print(f"{nod} {cc} {name:14} {obj:4} {conventions:12} /{lowest} el {elangle:.2f} {q} {dtype:6}"
              f" {nbins} x {rscale:g} m + {rstart:g} km = {range_km:g} km  cadence {cadence} min"
              f"  near {place[4]} ({admin1.get(place[3], '?')})")

path = "engine/data/sites.json"
table = json.load(open(path, encoding="utf-8"))
table["sites"] = [s for s in table["sites"] if s.get("provider", "smhi") == "smhi"] + rows
table["notes"] = re.sub(r" The ord rows.*$", "", table["notes"]) + (
    " The ord rows (Norway, Finland, Denmark, DEC-13) come from EUMETNET Open Radar Data's"
    " 24-hour S3 cache: id and nod are the ODIM node code, sourceId the cache path"
    " <CC>/<nod>/<PVOL|SCAN>, lat/lon/altM the newest file's root /where, rangeKm the lowest"
    " tilt's rstart + nbins x rscale, state the GeoNames admin-1 region of the nearest"
    f" cities5000 place; retrieved {today.date().isoformat()} by scripts/fetch-ord-sites.sh."
)
with open(path, "w", encoding="utf-8") as out:
    json.dump(table, out, ensure_ascii=False, indent=2)
    out.write("\n")
print(f"Wrote {path}: {len(rows)} ord rows; S3 requests: {REQUESTS['list']} listings, {REQUESTS['range']} ranges.")
PY
