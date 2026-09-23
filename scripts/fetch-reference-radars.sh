#!/usr/bin/env bash
# One-off data script (development only): writes
# engine/data/reference-radars.json, the other European weather radars the
# map shows as faint reference marks (S23, docs/protocol.md
# `hello.referenceSites`). They are not stations: the engine cannot show
# their data, and nothing selects them.
#
# Two requests, each made once:
#   1. EUMETNET Open Radar Data's locations list (one ORD API request; the
#      anonymous limit is 200 an hour, and nothing else at run time uses the
#      API). Every radar it lists whose ODIM node code is not already a
#      station in engine/data/sites.json (id, nod or alias) becomes a row,
#      at ORD's position. ORD's OPERA composite entry is not a radar and is
#      skipped.
#   2. EUMETNET OPERA's radar database (its JSON export on eumetnet.eu, not
#      the ORD API). It supplies the names ORD leaves blank (it lists some
#      radars as "[deess]" alone), and the operational radars of the
#      countries that feed the OPERA composite but not ORD: the United
#      Kingdom, Hungary, Portugal and Slovenia. The database states no
#      licence; the rows credit it as their source.
#
# ORD_LOCATIONS=<file> and OPERA_DB=<file> read a saved response instead of
# fetching (a rerun that must not spend an ORD request). Launch, setup and
# the checks never call this; rerun it only to take a new snapshot, then
# review the diff.
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - <<'PY'
import datetime, json, os, re, urllib.request

ORD = "https://api.meteogate.eu/eu-eumetnet-weather-radar/collections/observations/locations"
OPERA = ("https://www.eumetnet.eu/wp-content/themes/aeron-child/observations-programme/"
         "current-activities/opera/database/OPERA_Database/OPERA_RADARS_DB.json")
SRC_ORD, SRC_OPERA = "EUMETNET ORD", "EUMETNET OPERA database"
# The OPERA countries that are not in ORD's list, by the database's name.
OPERA_ONLY = {"United Kingdom": "GB", "Hungary": "HU", "Portugal": "PT", "Slovenia": "SI"}
# ISO 3166-1 numeric (the WIGOS issuer in ORD's ids, 0-<n>-0-<nod>) to alpha-2.
NUMERIC = {"56": "BE", "191": "HR", "203": "CZ", "208": "DK", "233": "EE", "246": "FI",
           "250": "FR", "276": "DE", "352": "IS", "372": "IE", "440": "LT", "470": "MT",
           "528": "NL", "578": "NO", "616": "PL", "642": "RO", "703": "SK", "724": "ES",
           "752": "SE", "756": "CH"}
NODE = re.compile(r"^[a-z]{5}$")
requests = 0


def load(env, url):
    global requests
    path = os.environ.get(env)
    if path:
        with open(path, "rb") as f:
            return json.load(f)
    requests += 1
    req = urllib.request.Request(url, headers={"User-Agent": "omastorm-nord fetch-reference-radars"})
    with urllib.request.urlopen(req, timeout=60) as r:
        left = r.headers.get("X-RateLimit-Remaining")
        if left is not None:
            print(f"{url}: X-RateLimit-Remaining {left}")
        return json.load(r)


today = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
with open("engine/data/sites.json", encoding="utf-8") as f:
    stations = json.load(f)["sites"]
known = set()
for s in stations:
    known |= {s["id"].lower(), s.get("nod", "").lower(), *(a.lower() for a in s.get("aliases", []))}
known.discard("")

ord_list = load("ORD_LOCATIONS", ORD)["features"]
opera_db = load("OPERA_DB", OPERA)
opera = {x["odimcode"].strip().lower(): x for x in opera_db if x.get("odimcode")}


def tidy(name):
    return re.sub(r"\s*/\s*", "/", " ".join(name.split()))


rows, ord_nodes, named_from_opera = [], set(), 0
for f in ord_list:
    wigos = f["id"].split("-")
    node = wigos[-1]
    if not NODE.match(node):
        continue  # 0-20010-0-OPERA: the OPERA composite, not a radar
    ord_nodes.add(node)
    if node in known:
        continue
    m = re.match(r"^\[(\w+)\]\s*(.*)$", f["properties"].get("name", ""))
    name = (m.group(2) if m else f["properties"].get("name", "")).strip()
    if not name and node in opera:
        name, named_from_opera = opera[node]["location"], named_from_opera + 1
    country = NUMERIC.get(wigos[1]) or node[:2].upper()
    assert country == node[:2].upper(), (node, country)
    lon, lat = f["geometry"]["coordinates"][:2]
    rows.append({"id": node, "name": tidy(name or node), "country": country,
                 "lat": round(lat, 5), "lon": round(lon, 5), "source": SRC_ORD, "retrieved": today})

added, skipped = {}, []
for node, x in sorted(opera.items()):
    if x.get("status") != "1" or node in ord_nodes or node in known:
        continue
    cc = OPERA_ONLY.get(x["country"])
    if not cc:
        skipped.append(f"{node} ({x['country']})")
        continue
    added[cc] = added.get(cc, 0) + 1
    rows.append({"id": node, "name": tidy(x["location"]), "country": cc,
                 "lat": round(float(x["latitude"]), 5), "lon": round(float(x["longitude"]), 5),
                 "source": SRC_OPERA, "retrieved": today})

rows.sort(key=lambda r: (r["country"], r["name"]))
ids = [r["id"] for r in rows]
assert len(ids) == len(set(ids)), "duplicate reference ids"
assert all(-90 <= r["lat"] <= 90 and -180 <= r["lon"] <= 180 for r in rows)
out = {
    "source": f"{ORD} ; {OPERA}",
    "retrieved": today,
    "notes": ("Reference radars (S23): positions only, drawn as faint marks; not stations. "
              f"{sum(r['source'] == SRC_ORD for r in rows)} rows are the radars in EUMETNET Open Radar "
              "Data's locations list that engine/data/sites.json does not hold (matched by ODIM node "
              f"code, the Swedish radars' nod included), at ORD's position; {named_from_opera} of them "
              "are unnamed in ORD and take their name from EUMETNET OPERA's radar database. "
              f"{sum(r['source'] == SRC_OPERA for r in rows)} rows are the operational radars "
              "(status 1) of the OPERA database for countries ORD does not list: "
              + ", ".join(f"{k} {v}" for k, v in sorted(added.items()))
              + ". The OPERA database states no licence; its rows are credited by source. "
              "Written by scripts/fetch-reference-radars.sh."),
    "radars": rows,
}
with open("engine/data/reference-radars.json", "w", encoding="utf-8") as f:
    json.dump(out, f, ensure_ascii=False, indent=1)
    f.write("\n")
print(f"wrote engine/data/reference-radars.json: {len(rows)} radars "
      f"({sum(r['source'] == SRC_ORD for r in rows)} ORD, {named_from_opera} named from OPERA; "
      f"OPERA-only {added}); requests made: {requests}")
if skipped:
    print("operational in the OPERA database but in neither ORD nor the countries above (not added): "
          + ", ".join(skipped))
PY
