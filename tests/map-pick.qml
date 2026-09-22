import QtQuick
import QtTest
import Quickshell

// S39: My mosaic's pick mode on the radar map. Hit-testing (radarAt), a
// click against a drag, the signals, and a capture of the marks in a dark
// and a light theme. Real mouse events through QtTest's TestEvent, so the
// map's own MouseArea is what is under test.
ShellRoot {
    Engine { id: engine; onTileReady: tile => map.tileReady(tile) }
    FloatingWindow {
        implicitWidth: 960; implicitHeight: 680; color: "#101820"
        Rectangle {
            id: capture
            anchors.fill: parent
            color: map.theme.background
            RadarMap {
                id: map
                anchors.fill: parent
                theme: dark
                scan: engine.state ? engine.state.frame : null
                texture: engine.texture
                azimuthLut: engine.azimuthLut
                sites: engine.sites
                siteId: engine.state ? engine.state.site.id : ""
                tileRoot: "file://" + engine.runtime
                onTilesNeeded: (z,x0,y0,x1,y1) => engine.send({type:"tiles_needed",z:z,x0:x0,y0:y0,x1:x1,y1:y1})
                onResetRequested: resets++
            }
        }
    }
    TestEvent { id: ev }
    SignalSpy { id: picked; target: map; signalName: "radarPicked" }
    SignalSpy { id: hovered; target: map; signalName: "radarHovered" }
    // Omarchy-like themes (Theme.qml's snapshot): Tokyo Night and a light one.
    readonly property var dark: ({font:"monospace", foreground:"#c0caf5", accent:"#7aa2f7",
                                  background:"#1a1b26", antenna:"#7dcfff", light:false})
    readonly property var light: ({font:"monospace", foreground:"#4c4f69", accent:"#1e66f5",
                                   background:"#eff1f5", antenna:"#179299", light:true})
    property int resets: 0
    property int stage: 0
    property var lone        // a mark with no other within 40 px
    property var pair        // the two closest marks
    property var nordic: ({lat: 62, lon: 16})
    function check(ok, message) { if (!ok) throw new Error(message); }
    function at(s) { return Qt.point(map.sx(map.mercatorX(s.lon)), map.sy(map.mercatorY(s.lat))); }
    function onScreen(p) { return p.x > 40 && p.x < map.width-40 && p.y > 40 && p.y < map.height-40; }
    function press(p) { check(ev.mousePress(map, p.x, p.y, Qt.LeftButton, Qt.NoModifier, -1), "window not shown"); }
    function move(p, buttons) { check(ev.mouseMove(map, p.x, p.y, -1, buttons, Qt.NoModifier), "window not shown"); }
    function release(p) { check(ev.mouseRelease(map, p.x, p.y, Qt.LeftButton, Qt.NoModifier, -1), "window not shown"); }
    // A Nordic view about 2000 km across at 62° N, whatever the site's latitude.
    function nordicView() {
        map.lookAt(nordic.lat, nordic.lon);
        map.zoom(2000 * Math.cos(map.siteLat*Math.PI/180) / Math.cos(nordic.lat*Math.PI/180));
    }
    Timer {
        interval: 400; repeat: true; running: true
        onTriggered: {
            try {
                if (stage === 0) {
                    if (!engine.state || map.sites.length === 0) return;   // wait for hello and a frame
                    check(map.pickSites.length === 41, "Pick mode should offer every polar radar: " + map.pickSites.length);
                    nordicView();
                    // Outside pick mode radarAt still answers, but the mouse never picks.
                    check(!map.pickMode && map.pickLabels.length === 0, "Pick mode on by default");
                } else if (stage === 1) {
                    var marks = map.pickSites.map(s => ({s: s, p: at(s)})).filter(m => onScreen(m.p));
                    check(marks.length > 20, "Nordic view shows too few radars: " + marks.length);
                    var best = Infinity;
                    for (var a of marks) {
                        var nearest = Infinity;
                        for (var b of marks) if (a !== b) {
                            var d = Math.hypot(a.p.x-b.p.x, a.p.y-b.p.y);
                            nearest = Math.min(nearest, d);
                            if (d < best) { best = d; pair = [a.s, b.s]; }
                        }
                        if (!lone && nearest > 40) lone = a.s;
                    }
                    check(!!lone && !!pair, "No lone mark or pair in view");
                    // Outside pick mode: a click on a mark picks nothing, a drag pans as ever.
                    var p = at(lone), cx = map.viewCenterX;
                    press(p); release(p);
                    check(picked.count === 0, "Picked outside pick mode");
                    press(p); move(Qt.point(p.x+2, p.y), Qt.LeftButton); release(Qt.point(p.x+2, p.y));
                    check(Math.abs((cx-map.viewCenterX)*map.worldPixels-2) < 1e-6, "A 2 px drag outside pick mode must pan 2 px");
                    check(hovered.count === 0, "Hover reported outside pick mode");
                    nordicView();
                    map.pickMode = true;
                } else if (stage === 2) {
                    check(map.pickLabels.length === 41, "Every radar should have a label in pick mode: " + map.pickLabels.length);
                    // radarAt: the mark itself, inside and outside the 12 px radius.
                    var p = at(lone);
                    check(map.radarAt(p.x, p.y) === lone.id, "radarAt missed the mark itself");
                    check(map.radarAt(p.x+11, p.y) === lone.id, "radarAt missed inside the radius");
                    check(map.radarAt(p.x+13, p.y) === "", "radarAt hit beyond the radius");
                    check(map.radarAt(p.x, p.y-13) === "", "radarAt hit beyond the radius (y)");
                    // Two close marks: the nearer wins.
                    var a = at(pair[0]), b = at(pair[1]), d = Math.hypot(a.x-b.x, a.y-b.y);
                    check(d < 24, "The closest pair is too far apart to test: " + d);
                    check(map.radarAt(a.x+(b.x-a.x)*.4, a.y+(b.y-a.y)*.4) === pair[0].id, "Nearer mark lost (a)");
                    check(map.radarAt(a.x+(b.x-a.x)*.6, a.y+(b.y-a.y)*.6) === pair[1].id, "Nearer mark lost (b)");
                    // Hover: the mark under the pointer, "" off it.
                    move(p, Qt.NoButton);
                    check(hovered.count === 1 && hovered.signalArguments[0][0] === lone.id, "Hover over a mark not reported");
                    move(Qt.point(p.x+30, p.y+30), Qt.NoButton);
                    check(map.radarAt(p.x+30, p.y+30) === "" && hovered.count === 2
                          && hovered.signalArguments[1][0] === "", "Leaving a mark not reported");
                    // A click on a mark picks it; a little jitter is still a click, and the map holds still.
                    var cx = map.viewCenterX, cy = map.viewCenterY;
                    press(p); release(p);
                    check(picked.count === 1 && picked.signalArguments[0][0] === lone.id, "A click on a mark did not pick it");
                    press(p); move(Qt.point(p.x+2, p.y+1), Qt.LeftButton); release(Qt.point(p.x+2, p.y+1));
                    check(picked.count === 2 && picked.signalArguments[1][0] === lone.id, "A 2 px jitter should still pick");
                    check(map.viewCenterX === cx && map.viewCenterY === cy, "A click moved the map");
                    // A 10 px drag from a mark pans the whole 10 px and picks nothing.
                    press(p); move(Qt.point(p.x+5, p.y), Qt.LeftButton); move(Qt.point(p.x+10, p.y), Qt.LeftButton);
                    release(Qt.point(p.x+10, p.y));
                    check(picked.count === 2, "A 10 px drag picked a radar");
                    check(Math.abs((cx-map.viewCenterX)*map.worldPixels-10) < 1e-6 && Math.abs(map.viewCenterY-cy) < 1e-12,
                          "A 10 px drag in pick mode must pan 10 px: " + (cx-map.viewCenterX)*map.worldPixels);
                    // A click on empty map does nothing.
                    var empty = Qt.point(p.x+40, p.y+40);
                    check(map.radarAt(empty.x, empty.y) === "", "Test point not empty");
                    cx = map.viewCenterX;
                    press(empty); release(empty);
                    check(picked.count === 2 && map.viewCenterX === cx && resets === 0, "A click on empty map did something");
                    // Double-click: on empty map it still resets; on a mark it is one pick.
                    check(ev.mouseDoubleClickSequence(map, empty.x, empty.y, Qt.LeftButton, Qt.NoModifier, -1), "window not shown");
                    check(resets === 1 && picked.count === 2, "A double-click on empty map must reset and pick nothing");
                    var q = at(lone);
                    check(ev.mouseDoubleClickSequence(map, q.x, q.y, Qt.LeftButton, Qt.NoModifier, -1), "window not shown");
                    check(resets === 1 && picked.count === 3, "A double-click on a mark must be one pick, not a reset: " + picked.count + "/" + resets);
                    // The wheel still zooms about the pointer.
                    var span = map.span;
                    check(ev.mouseWheel(map, q.x, q.y, 0, 120, Qt.NoButton, Qt.NoModifier, -1), "window not shown");
                    check(map.span < span, "The wheel did not zoom in pick mode");
                    var q2 = at(lone);
                    check(Math.hypot(q2.x-q.x, q2.y-q.y) < 1e-6, "Wheel zoom moved the ground under the pointer");
                    // Leaving pick mode clears the hover.
                    move(q2, Qt.NoButton);
                    var n = hovered.count;
                    map.pickMode = false;
                    check(hovered.count === n+1 && hovered.signalArguments[n][0] === "", "Leaving pick mode kept a hover");
                    check(map.radarAt(q2.x, q2.y) === lone.id, "radarAt should stay geometric");
                    // The capture: two ticked, one hot, the rest plain, at the Nordic view.
                    map.pickMode = true;
                    nordicView();
                    var ticks = map.pickSites.filter(s => ["vara", "hudiksvall", "fikor", "nohur"].indexOf(s.id) >= 0);
                    check(ticks.length === 4, "Sample radars missing from the table");
                    map.pickedIds = ticks.map(s => s.id);
                    map.mosaicCircles = ticks.map(s => ({id: s.id, lat: s.lat, lon: s.lon, km: 240}));
                    var hot = map.pickSites.find(s => map.pickedIds.indexOf(s.id) < 0 && onScreen(at(s)));
                    map.hotId = hot.id;
                } else if (stage === 5) {
                    capture.grabToImage(result => {
                        check(result.saveToFile(Quickshell.env("OMASTORM_REVIEW")+"/map-pick-dark.png"), "Capture failed");
                        map.theme = light;
                    });
                } else if (stage === 8) {
                    capture.grabToImage(result => {
                        check(result.saveToFile(Quickshell.env("OMASTORM_REVIEW")+"/map-pick-light.png"), "Capture failed");
                        console.log("MAP_PICK_PASSED"); Qt.quit();
                    });
                }
                stage++;
            } catch (e) { console.error(e); Qt.quit(); }
        }
    }
    Timer { interval: 20000; running: true; onTriggered: { console.error("map-pick timed out at stage " + stage); Qt.quit(); } }
}
