#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
check_dir="$PWD/target/check-engine-ui"
mkdir -p "$check_dir"
cp ui/Engine.qml "$check_dir/Engine.qml"
cat > "$check_dir/shell.qml" <<'QML'
import QtQuick
import Quickshell
ShellRoot {
    Engine { id: engine }
    function check(ok, message) { if (!ok) throw new Error(message); }
    property string good: ""
    property var tiles: []
    Connections { target: engine; function onTileReady(tile) { tiles.push(tile); } }
    Timer {
        interval: 1000; running: true
        onTriggered: {
            check(!!engine.state, "No socket state received");
            // The 12 SMHI radars, ORD's 29 (NO, FI, DK: S15) and 11 (ES: S32), the national composite (protocol v2), OPERA's Nordic one (S16) and My mosaic (S25).
            check(engine.sites.filter(s => s.kind === "polar").length === 52
                  && engine.sites.filter(s => s.kind === "grid").map(s => s.id).join() === "sweden,nordic,mymosaic", "No site table received");
            check(engine.texture.indexOf("file://") === 0, "No engine texture");
            check(engine.azimuthLut.indexOf("file://") === 0 && engine.azimuthLut !== engine.texture, "No engine azimuth lookup");
            check(engine.state.frame.rays > 0 && engine.state.frame.gates > 0 && engine.state.frame.gateSpacingM > 0, "No sweep geometry in state");
            var basemap = engine.state.basemap;
            check(!!basemap && basemap.ne.version.length > 0 && ["ok", "offline", "unavailable"].indexOf(basemap.osm.status) >= 0
                  && basemap.osm.source.length > 0 && typeof basemap.osm.version === "string" && basemap.osm.attribution.indexOf("OpenStreetMap") >= 0,
                  "No basemap sources in state: " + JSON.stringify(basemap));
            good = JSON.stringify(engine.state);
            engine.receive("bad json");
            check(engine.state === null && engine.texture === "" && engine.azimuthLut === "", "Malformed state still renders");
            engine.receive(good);
            check(!!engine.state && engine.error === "", "Valid state did not recover");
            var traversal = JSON.parse(good);
            traversal.frame.texture = "tex/../x.png";
            engine.receive(JSON.stringify(traversal));
            check(engine.state === null && engine.texture === "", "Parent segment in texture path still renders");
            var lutTraversal = JSON.parse(good);
            lutTraversal.frame.azimuthLut = "tex/a/b.png";
            engine.receive(JSON.stringify(lutTraversal));
            check(engine.state === null && engine.azimuthLut === "", "Extra segment in azimuth lookup path still renders");
            var renamed = JSON.parse(good);
            renamed.frame.texture = "tex/sweep-TEST-r1.png";
            renamed.frame.azimuthLut = "tex/azlut-TEST-r1.png";
            engine.receive(JSON.stringify(renamed));
            check(!!engine.state && engine.texture.indexOf("/omastorm-se/tex/sweep-TEST-r1.png") > 0 && engine.azimuthLut.indexOf("/omastorm-se/tex/azlut-TEST-r1.png") > 0, "Free one-segment texture names were rejected");
            engine.receive(good);
            check(!!engine.state && engine.error === "", "Fixture state did not recover after renamed textures");
            // Version 2: a grid frame has no azimuth lookup (its texture stands
            // in for the sampler) and is placed by frame.grid; a lookup on a
            // grid, a grid with no placement, or any other kind is rejected.
            var gridState = JSON.parse(good);
            gridState.frame.kind = "grid";
            gridState.frame.azimuthLut = "";
            gridState.frame.rays = 0;
            gridState.frame.gates = 0;
            gridState.frame.grid = {projection: "EPSG:3857", xsize: 1364, ysize: 1983, xscale: 2000, yscale: 2000,
                                    west: 5.324, east: 29.83, north: 70.034, south: 53.701, sourceProjdef: "+proj=stere"};
            engine.receive(JSON.stringify(gridState));
            check(!!engine.state && engine.error === "" && engine.azimuthLut === engine.texture, "Grid frame was rejected: " + engine.error);
            var gridLut = JSON.parse(JSON.stringify(gridState));
            gridLut.frame.azimuthLut = "tex/azlut-TEST-r1.png";
            engine.receive(JSON.stringify(gridLut));
            check(engine.state === null && engine.error.indexOf("Invalid engine message") === 0, "Grid frame with an azimuth lookup was accepted");
            var unplaced = JSON.parse(JSON.stringify(gridState));
            delete unplaced.frame.grid;
            engine.receive(JSON.stringify(unplaced));
            check(engine.state === null, "Grid frame without a placement was accepted");
            var unknownKind = JSON.parse(good);
            unknownKind.frame.kind = "hex";
            engine.receive(JSON.stringify(unknownKind));
            check(engine.state === null, "Unknown frame kind was accepted");
            engine.receive(good);
            check(!!engine.state && engine.error === "" && engine.state.frame.kind === "polar", "Fixture state did not recover after grid frames");
            // A rejection is this client's own: it sits beside state, survives
            // a state broadcast, and clears when this client sends again.
            engine.receive('{"type":"error","v":2,"command":"select_site","message":"Not here"}');
            check(!!engine.state && engine.rejection === "Not here" && engine.error === "", "Rejection did not sit beside state");
            engine.receive(good);
            check(!!engine.state && engine.rejection === "Not here", "A state broadcast cleared this client's rejection");
            engine.send({type: "pause"});
            check(engine.rejection === "", "Sending a command did not clear the previous rejection");
            // The tile path rule: a parent segment or a fifth level is transport
            // trouble like a bad texture path; a well-formed reply reaches the map.
            engine.receive('{"type":"tile_ready","v":2,"set":"ne","z":5,"x":7,"y":12,"path":"tiles/ne/5/../12-a.png","labels":[]}');
            check(engine.state === null && engine.error.indexOf("Invalid engine message") === 0 && tiles.length === 0, "Parent segment in tile path was accepted");
            engine.receive(good);
            engine.receive('{"type":"tile_ready","v":2,"set":"ne","z":5,"x":7,"y":12,"path":"tiles/ne/5/7/12/a.png","labels":[]}');
            check(engine.state === null && tiles.length === 0, "Extra segment in tile path was accepted");
            engine.receive(good);
            engine.receive('{"type":"tile_ready","v":2,"set":"foo","z":5,"x":7,"y":12,"path":"tiles/foo/5/7/12-a.png","labels":[]}');
            check(engine.state === null && tiles.length === 0, "Unknown tile set was accepted");
            engine.receive(good);
            engine.receive('{"type":"tile_ready","v":2,"set":"osm","z":11,"x":470,"y":808,"path":"tiles/osm/11/470/808-3f9a1c2e.png","labels":[]}');
            check(!!engine.state && engine.error === "" && tiles.length === 1 && tiles[0].path === "tiles/osm/11/470/808-3f9a1c2e.png", "Valid tile_ready did not reach the map");
            // Round trips through the real daemon: a station outside the
            // table is rejected (a table station would go live and reach the
            // network), and the four z5 tiles around KTLX come back as ne masks.
            engine.send({type: "select_site", id: "XXXX"});
            engine.send({type: "tiles_needed", z: 5, x0: 7, y0: 12, x1: 8, y1: 13});
        }
    }
    Timer {
        interval: 2000; running: true
        onTriggered: {
            check(!!engine.state && engine.state.site.id === "KTLX", "Rejected select_site changed state");
            check(engine.rejection.indexOf("XXXX") >= 0, "Daemon did not answer the rejected command to this client: " + JSON.stringify(engine.rejection));
            var served = tiles.filter(t => t.set === "ne" && t.z === 5 && t.path.indexOf("tiles/ne/5/") === 0);
            check(served.length === 4, "Daemon did not answer tiles_needed with four ne tiles: " + tiles.length);
            // A version 1 engine (or anything else) latches: this UI speaks v2.
            engine.receive('{"type":"state","v":1}');
            check(engine.incompatible && engine.state === null && engine.texture === "", "Version mismatch still renders");
            engine.receive(good);
            check(engine.state === null, "Version mismatch was not latched");
            console.log("ENGINE_UI_PASSED");
            Qt.quit();
        }
    }
    Timer { interval: 5000; running: true; onTriggered: Qt.quit() }
}
QML
OMASTORM_QML="$check_dir/shell.qml" QT_QPA_PLATFORM=offscreen QT_QPA_PLATFORMTHEME=basic \
  bash run.sh > "$check_dir/result.log" 2>&1
cat "$check_dir/result.log"
rg -q ENGINE_UI_PASSED "$check_dir/result.log"
