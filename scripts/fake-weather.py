#!/usr/bin/env python3
"""A local stand-in for the weather layers' providers (S47), for checks and
captures that must not touch SMHI, FMI, DMI, Frost or MET Norway.

The engine is pointed here with OMASTORM_OBS_BASE=http://127.0.0.1:<port>;
the path and query of each provider's URL are kept (obs/mod.rs `rebase`).
The station answers are the recorded fixtures in data/fixtures/obs/ with
their times moved to a few minutes ago, so the engine's 90-minute stale
rule keeps them; the MET Nordic grid is synthetic (the engine's 3/24
stride, as engine/tests/grid.rs builds it), valid on the current hour.

  fake-weather.py PORTFILE [--fail dmi,fmi] [--slow smhi=4,grid=6] [--log FILE]

PORTFILE receives the port once the server listens. --fail answers a
provider with HTTP 503; --slow holds its answers that many seconds. Every
request is appended to --log as "<provider> <status>", so a check can count
requests per provider. SIGUSR1 re-reads PORTFILE + ".fail" (a provider
list like --fail) and PORTFILE + ".slow" (like --slow), so a run can make
a provider fail, hang or recover.
"""
import gzip
import json
import os
import re
import signal
import struct
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "data", "fixtures", "obs")


def fixture(name):
    with gzip.open(os.path.join(ROOT, name), "rb") as f:
        return f.read()


def iso_shift(text, delta_s):
    def repl(m):
        t = time.strptime(m.group(1), "%Y-%m-%dT%H:%M:%S")
        s = int(time.mktime(t) - time.timezone) + delta_s
        return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(s)) + (m.group(2) or "") + "Z"
    return re.sub(r"(20\d\d-\d\d-\d\dT\d\d:\d\d:\d\d)(\.\d+)?Z", repl, text)


def smhi(parameter, now):
    body = json.loads(fixture(f"smhi_p{parameter}_202609221800.json.gz"))
    # The newest report ten minutes ago.
    delta = int(now * 1000) - 600_000 - body["period"]["to"]
    for station in body.get("station", []):
        for v in station.get("value") or []:
            v["date"] += delta
    body["period"]["to"] += delta
    body["period"]["from"] += delta
    return json.dumps(body).encode()


def fmi(now):
    text = fixture("fmi_202609221833.xml.gz").decode()
    delta = int(now) - 300 - 1790101980  # 18:33Z → five minutes ago
    text = re.sub(r"(\s)(17\d{8})(\s)", lambda m: f"{m.group(1)}{int(m.group(2)) + delta}{m.group(3)}", text)
    return iso_shift(text, delta).encode()


def dmi_obs(now):
    text = fixture("dmi_obs_202609221830.json.gz").decode()
    delta = int(now) - 300 - 1790101800  # 18:30Z → five minutes ago
    return iso_shift(text, delta).encode()


NX, NY, WX, WY = 599, 774, 75, 97


def dods_array(values, fmt):
    n = len(values)
    return struct.pack(">II", n, n) + struct.pack(f">{n}{fmt}", *values)


def grid_subset(seconds):
    dds = (
        "Dataset {\n    Float32 x[x = %d];\n    Float32 y[y = %d];\n    Float64 time[time = 1];\n"
        "    Structure {\n        Float32 air_temperature_2m[time = 1][y = %d][x = %d];\n    } air_temperature_2m;\n"
        "    Structure {\n        Float32 wind_speed_10m[time = 1][y = %d][x = %d];\n    } wind_speed_10m;\n"
        "    Structure {\n        Float32 wind_direction_10m[time = 1][y = %d][x = %d];\n    } wind_direction_10m;\n"
        "} metpplatest/met_analysis_1_0km_nordic_latest.nc;\n\nData:\n"
    ) % (NX, NY, NY, NX, WY, WX, WY, WX)
    out = bytearray(dds.encode())
    out += dods_array([-897442.2 + 3000.0 * i for i in range(NX)], "f")
    out += dods_array([-1104322.0 + 3000.0 * j for j in range(NY)], "f")
    out += struct.pack(">II", 1, 1) + struct.pack(">d", seconds)
    out += dods_array([293.15 - 30.0 * (k // NX) / NY for k in range(NX * NY)], "f")
    # A wind that varies, so the numbers differ across the map.
    out += dods_array([2.0 + 14.0 * ((k // WX) / WY) for k in range(WX * WY)], "f")
    out += dods_array([200.0 + 140.0 * ((k % WX) / WX) for k in range(WX * WY)], "f")
    return bytes(out)


def grid_probe(seconds):
    head = b"Dataset {\n    Float64 time[time = 1];\n} metpplatest/met_analysis_1_0km_nordic_latest.nc;\n\nData:\n"
    return head + struct.pack(">II", 1, 1) + struct.pack(">d", seconds)


class State:
    fail = set()
    slow = {}
    log = None
    lock = threading.Lock()
    subset_cache = {}


def provider_of(path):
    if "/metobs" in path or "/parameter/" in path:
        return "smhi"
    if path.startswith("/wfs"):
        return "fmi"
    if "/metObs/" in path:
        return "dmi"
    if ".nc.dods" in path:
        return "grid"
    if "frost" in path or "/observations/v0" in path or "/sources/v0" in path:
        return "frost"
    return "other"


def answer(path, now):
    if "/parameter/" in path:
        p = int(re.search(r"/parameter/(\d+)/", path).group(1))
        return smhi(p, now), "application/json"
    if path.startswith("/wfs"):
        return fmi(now), "application/xml"
    if "/metObs/collections/observation/" in path:
        return dmi_obs(now), "application/geo+json"
    if "/metObs/collections/station/" in path:
        return fixture("dmi_stations_20260922.json.gz"), "application/geo+json"
    if ".nc.dods" in path:
        hour = float(int(now) - int(now) % 3600)
        if path.endswith(".nc.dods?time"):
            return grid_probe(hour), "application/octet-stream"
        with State.lock:
            body = State.subset_cache.get(hour)
            if body is None:
                body = State.subset_cache[hour] = grid_subset(hour)
        return body, "application/octet-stream"
    return None, None


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_GET(self):
        who = provider_of(self.path)
        delay = State.slow.get(who, 0)
        if delay:
            time.sleep(delay)
        if who in State.fail:
            body, kind, status = b"busy", "text/plain", 503
        else:
            body, kind = answer(self.path, time.time())
            status = 200 if body is not None else 404
            if body is None:
                body, kind = b"", "text/plain"
        if State.log:
            with State.lock, open(State.log, "a") as f:
                f.write(f"{who} {status}\n")
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


def parse_list(text):
    return {x.strip() for x in text.split(",") if x.strip()}


def main():
    args = sys.argv[1:]
    portfile = args.pop(0)
    while args:
        flag = args.pop(0)
        value = args.pop(0)
        if flag == "--fail":
            State.fail = parse_list(value)
        elif flag == "--slow":
            State.slow = {k: float(v) for k, v in (x.split("=") for x in parse_list(value))}
        elif flag == "--log":
            State.log = value
    failfile = portfile + ".fail"

    def reread(*_):
        try:
            with open(failfile) as f:
                State.fail = parse_list(f.read())
        except OSError:
            State.fail = set()
        try:
            with open(portfile + ".slow") as f:
                State.slow = {k: float(v) for k, v in (x.split("=") for x in parse_list(f.read()))}
        except OSError:
            State.slow = {}

    signal.signal(signal.SIGUSR1, reread)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    with open(portfile + ".tmp", "w") as f:
        f.write(str(server.server_address[1]))
    os.replace(portfile + ".tmp", portfile)
    server.serve_forever()


if __name__ == "__main__":
    main()
