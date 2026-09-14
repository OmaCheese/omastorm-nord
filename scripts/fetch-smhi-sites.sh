#!/usr/bin/env bash
# One-off fixture script (development only): rewrites the SMHI rows of
# engine/data/sites.json from SMHI's open radar API (other providers' rows
# are kept). For each qcvol area it opens the newest volume
# over HTTP range reads (a few 64 KB blocks, not the 15 MB file) and takes
# the radar's position from the root /where group (lat, lon, height) and its
# identifiers from /what source (RAD, NOD, WMO). Display names and counties
# are written here: the files carry ASCII place codes only. The county is the
# GeoNames admin-1 name, so it reads like the location picker's regions.
# Launch, setup and the checks never call this; rerun it only to take a new
# snapshot, then review the diff.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v uv >/dev/null 2>&1 || {
  printf 'uv is required (it supplies h5py and fsspec for this one run).\n' >&2
  exit 1
}
uv run --quiet --no-project --with h5py python - <<'PY'
import datetime, io, json, urllib.request

import h5py

API = "https://opendata-download-radar.smhi.se/api/version/latest"
# area key -> (display name, county). The area key is the station id.
SITES = {
    "angelholm": ("Ängelholm", "Skåne"),
    "atvidaberg": ("Åtvidaberg", "Östergötland"),
    "balsta": ("Bålsta", "Uppsala"),
    "hemse": ("Hemse", "Gotland"),
    "hudiksvall": ("Hudiksvall", "Gävleborg"),
    "karlskrona": ("Karlskrona", "Blekinge"),
    "kiruna": ("Kiruna", "Norrbotten"),
    "leksand": ("Leksand", "Dalarna"),
    "lulea": ("Luleå", "Norrbotten"),
    "ornskoldsvik": ("Örnsköldsvik", "Västernorrland"),
    "ostersund": ("Östersund", "Jämtland"),
    "vara": ("Vara", "Västra Götaland"),
}


def get(url):
    with urllib.request.urlopen(url, timeout=30) as reply:
        return json.load(reply)


class Ranged(io.RawIOBase):
    """A read-only, seekable view of a URL through 64 KB Range requests."""

    BLOCK = 65536

    def __init__(self, url):
        self.url, self.pos, self.blocks, self.requests = url, 0, {}, 0
        # The size from a one-byte range's Content-Range: a HEAD sometimes
        # comes back without a Content-Length.
        first = urllib.request.Request(url, headers={"Range": "bytes=0-0"})
        with urllib.request.urlopen(first, timeout=30) as reply:
            self.size = int(reply.headers["content-range"].rsplit("/", 1)[1])
            reply.read()
        self.requests += 1

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
            end = min(start + self.BLOCK, self.size) - 1
            request = urllib.request.Request(self.url, headers={"Range": f"bytes={start}-{end}"})
            with urllib.request.urlopen(request, timeout=30) as reply:
                if reply.status != 206:
                    raise SystemExit(f"{self.url}: expected 206 for a Range request, got {reply.status}")
                self.blocks[index] = reply.read()
            self.requests += 1
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


areas = sorted(a["key"] for a in get(f"{API}/area.json")["areas"] if a["key"] != "sweden")
missing = set(areas) ^ set(SITES)
if missing:
    raise SystemExit(f"SMHI's area list and this script disagree: {sorted(missing)}")

sites = []
for key in areas:
    listing = get(f"{API}/area/{key}/product/qcvol.json")
    newest = listing["lastFiles"][-1]
    link = next(f["link"] for f in newest["formats"] if f["key"] == "h5")
    stream = Ranged(link)
    with h5py.File(stream, "r") as volume:
        where = volume["where"].attrs
        source = volume["what"].attrs["source"]
        source = source.decode() if isinstance(source, bytes) else str(source)
        ids = dict(part.split(":", 1) for part in source.split(","))
        name, county = SITES[key]
        sites.append({
            "id": key,
            "name": name,
            "state": county,
            "lat": round(float(where["lat"]), 6),
            "lon": round(float(where["lon"]), 6),
            "altM": round(float(where["height"]), 1),
            "rad": ids.get("RAD", ""),
            "nod": ids.get("NOD", ""),
            "wmo": ids.get("WMO", ""),
            "newest": newest["valid"],
        })
        print(f"{key:13} {ids.get('RAD', ''):5} {ids.get('NOD', ''):6} newest {newest['valid']}"
              f"  ({stream.requests} requests, {sum(map(len, stream.blocks.values()))} bytes)")

table = {
    "source": f"{API}/area/{{id}}/product/qcvol.json",
    "layout": "ODIM_H5 2.2 PVOL: root /where lat, lon, height; /what source",
    "retrieved": datetime.date.today().isoformat(),
    "notes": (
        "The 12 SMHI qcvol radars; id is the API area key. lat/lon/altM are each "
        "volume's root /where (height above sea level, metres); rad, nod and wmo "
        "come from /what source (e.g. vara is RAD:SE49, NOD:sevax). state holds "
        "the county (GeoNames admin-1). newest is the listing's latest valid time "
        "when retrieved: a site far behind (leksand) is listed, and shows "
        "unavailable. Written by scripts/fetch-smhi-sites.sh."
    ),
    "sites": sites,
}
# Rows of other providers (scripts/fetch-ord-sites.sh) stay as they are.
try:
    previous = json.load(open("engine/data/sites.json", encoding="utf-8"))
except FileNotFoundError:
    previous = {"sites": [], "notes": ""}
others = [s for s in previous["sites"] if s.get("provider", "smhi") != "smhi"]
table["sites"] += others
if others and " The ord rows" in previous["notes"]:
    table["notes"] += previous["notes"][previous["notes"].index(" The ord rows"):]
with open("engine/data/sites.json", "w", encoding="utf-8") as out:
    json.dump(table, out, ensure_ascii=False, indent=2)
    out.write("\n")
print(f"Wrote engine/data/sites.json: {len(sites)} SMHI sites, {len(others)} others kept.")
PY
