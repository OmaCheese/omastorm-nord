import QtQuick
import QtQuick.Shapes
import Quickshell

// The radar map: camera, GPU radar shader, basemap tile layer, camera-translated
// overlay, and pointer handling. Surfaces place it, feed it state,
// and drive the camera through center, span,
// reset(), zoom(), and maxSpan. It draws no chrome and holds no station or
// product strings, except the caption of its own rings (`ringNote`, S29);
// every input arrives through its properties. Tiles come and
// go through tilesNeeded and tileReady(); the surface connects them to the
// engine so the map knows nothing of sockets.
Item {
    id: map
    clip: true

    // Inputs from the surface.
    property var scan: null          // socket state frame, or null
    property string texture: ""      // engine-written sweep texture (gates × rays)
    property string azimuthLut: ""   // engine-written azimuth lookup (3600 × 1)
    // True when `texture` is a grid's one-channel code texture (docs/
    // protocol.md, code texture) instead of its RGBA grid texture: the
    // shader then rebuilds each texel's class from the frame's bounds, scale
    // and offset through `classTexture`, with the engine's rule.
    property bool codes: false
    property string siteId: ""
    property var sites: []           // hello.sites; locations, not live availability
    property var referenceSites: []  // hello.referenceSites: other radars, faint marks only (S23)
    // How far the sweep reaches on the ground, from the frame's own geometry:
    // the far edge of the last gate (radar.frag draws up to half a gate past
    // it), slant range turned into ground distance on the shader's 4/3
    // effective-radius earth. SMHI volumes reach 240 km, NEXRAD reflectivity
    // 460 km. With no frame (or the gateless loading placeholder), the
    // station's own rangeKm from hello, else SMHI's 240.
    readonly property var activeSite: sites.find(s => s.id === siteId) || null
    readonly property real nominalCoverageKm: activeSite && activeSite.rangeKm > 0 ? activeSite.rangeKm : 240
    readonly property real coverageKm: {
        var s = scan;
        // A composite reaches its farthest corner from the site it is placed by.
        if (placement) {
            var g = placement, far = 0;
            for (var c of [[g.north, g.west], [g.north, g.east], [g.south, g.west], [g.south, g.east]])
                far = Math.max(far, distanceKm(s.site.lat, s.site.lon, c[0], c[1]));
            return far;
        }
        if (!s || !(s.gates > 0) || !(s.gateSpacingM > 0)) return nominalCoverageKm;
        var slantM = (s.firstGateM || 0) + (s.gates - .5) * s.gateSpacingM;
        var e = (s.elevationDeg || 0) * Math.PI / 180, earthM = 6371000 * 4 / 3;
        return earthM * Math.atan2(slantM * Math.cos(e), earthM + slantM * Math.sin(e)) / 1000;
    }
    // S29: the active radar's rings follow what is shown. `product` is the
    // engine's state.product (the height of a CAPPI frame); `reachKm` the
    // reach the surface remembers for this radar (Reach.qml), 0 for the
    // whole sweep: the shader draws nothing past it and the rings follow.
    property var product: null
    property real reachKm: 0
    // S24d: Relief: a storm height (ETOP) drawn lit as a surface (radar.frag).
    property bool relief: false
    readonly property real reachShownKm: reachKm > 0 && reachKm < coverageKm ? reachKm : 0
    readonly property real weatherTopM: 12000
    readonly property real lowBeamM: 2000
    // Where the beam centre at `deg` is `m` metres above the antenna, in
    // ground km on the 4/3 earth: cos(e + s/R) = R cos(e) / (R + m). 0 when
    // it never gets there.
    function groundKmAt(deg, m) {
        var earthM = 6371000 * 4 / 3, e = deg * Math.PI / 180, c = earthM * Math.cos(e) / (earthM + m);
        if (!(m > 0) || c >= 1) return 0;
        return Math.max(0, earthM * (Math.acos(c) - e) / 1000);
    }
    // Each ring: its radius, whether it is the solid (strong) one, and what
    // it marks. The lowest scan, Clear view and Column max: the data's edge
    // and a faint ring where the lowest beam is 2 km up (inside it the scan
    // is rain near the ground, outside it rain aloft). One angle: where its
    // beam passes 12 km, if that comes before the data's edge. A height:
    // solid where the lowest beam's centre reaches it, faint at the data's
    // edge. A reach drops the rings beyond it and becomes the circle.
    readonly property var rings: {
        if (!drawable || grid || !scan) return [];
        var data = coverageKm, e = scan.elevationDeg || 0, id = scan.product || "REF", out = [];
        var here = activeSite;
        var lowest = here && here.elevations && here.elevations.length ? here.elevations[0].deg : e;
        if (id === "CAPPI" || id === "CAPPI1" || id === "CAPPI2") {
            // CAPPI (S29) is above sea level; an older engine's CAPPI1/2 were
            // 1 and 2 km above the antenna.
            var sea = id === "CAPPI";
            var asl = product && product.id === "CAPPI" && product.heightM > 0 ? product.heightM : 2000;
            var above = sea ? asl - (here && here.altM ? here.altM : 0) : id === "CAPPI1" ? 1000 : 2000;
            // S30: above the ground the lowest beam meets the height at no
            // one distance (the ground varies): only the data's edge.
            var at = sea && product && product.above === "ground" ? 0 : groundKmAt(e, above);
            if (at > 0 && at < data) {
                out.push({km: at, strong: true, what: "height", heightM: sea ? asl : above, sea: sea});
                out.push({km: data, strong: false, what: "data"});
            } else out.push({km: data, strong: true, what: "data"});
        } else if (id === "REF" && e > lowest + .25) {
            var top = groundKmAt(e, weatherTopM);
            out.push(top > 0 && top < data ? {km: top, strong: true, what: "top", deg: e} : {km: data, strong: true, what: "data"});
        } else {
            out.push({km: data, strong: true, what: "data"});
            // Storm height and rain mass use every beam, so the lowest
            // beam's 2 km ring means nothing on them (S24a review #6).
            var low = id === "ETOP" || id === "VIL" ? 0 : groundKmAt(e, lowBeamM);
            if (low > 0 && low < data) out.push({km: low, strong: false, what: "low", deg: e});
        }
        if (reachShownKm > 0) {
            out = out.filter(r => r.km < reachShownKm);
            out.unshift({km: reachShownKm, strong: true, what: "reach"});
        }
        return out;
    }
    // What each ring means, in the order drawn; the surface shows it.
    readonly property string ringNote: {
        var parts = [], km = v => Math.round(v) + " km";
        for (var r of rings) {
            if (r.what === "reach") parts.push("circle " + km(r.km) + ": the reach you set");
            else if (r.what === "data") parts.push((r.strong ? "circle " : "faint ring ") + km(r.km) + ": edge of the data");
            else if (r.what === "top") parts.push("circle " + km(r.km) + ": the " + r.deg.toFixed(1) + "° beam passes 12 km");
            else if (r.what === "low") parts.push("faint ring " + km(r.km) + ": the " + r.deg.toFixed(1) + "° beam 2 km up, rain aloft beyond");
            else if (r.what === "height") parts.push("circle " + km(r.km) + ": the lowest beam at " + (r.heightM / 1000) + " km" + (r.sea ? " above sea" : " up"));
        }
        if (drawable && scan && scan.product === "CAPPI") parts.push("hatched: no radar at this height");
        // S24a: a storm height is a lower bound where the highest beam that
        // reaches it still holds 18 dBZ (G bit 8, hatched over its colour).
        // Which beams reach a column changes with the scan pattern (review #5).
        if (drawable && scan && scan.product === "ETOP") parts.push("hatched: at least this high (the hatch moves with the radar's scan pattern)");
        return parts.join(" · ");
    }
    property string tileRoot: ""     // file URL of the runtime directory, for tile paths
    property var theme
    property string treatment: "GLYPHS"
    // The weak-return floor in dBZ, or null to draw every measured return
    // (DESIGN.md, weak-return floor). The shader compares raw moment codes,
    // so the frame's scale and offset place the floor in code units; a frame
    // without them (the loading placeholder) has no floor.
    property var weakFloor: null
    // The floor is in dBZ (S24a): a frame in other units (Storm height,
    // Rain mass) has none.
    readonly property int weakBelow: scan && weakFloor !== null && scan.scale > 0 && scan.units === "dBZ" ? Math.max(0, Math.min(256, Math.ceil(weakFloor * scan.scale + scan.offset))) : 0
    property int labelSize: 12
    property real radarOpacity: 1    // the radar layer alone; the basemap keeps its strength
    property bool locked: false      // the accent frame on the active marker and tag (DESIGN.md, markers)
    property bool interactive: true  // false while location prompt/picker owns the surface
    // A frame with a scan time is radar to draw. The loading placeholder
    // (docs/protocol.md, frame.status: no scan time, one blank row) draws no
    // radar; tiles, labels, markers, and coverage still show, so a station
    // waiting for its first sweep is the map without radar (DESIGN.md, lean
    // startup as built). With no station selected yet the rings, crosshair,
    // and tag stay away too.
    readonly property bool drawable: !!scan && scan.scanTime !== ""
    readonly property int bands: scan ? scan.palette.length : 0
    // A grid frame (docs/protocol.md, frame.kind): the national composite,
    // reprojected by the engine to Web Mercator and placed by `scan.grid`.
    // It has no antenna, so no rings, crosshair, footprint, or tag.
    // Bindings that read the placement test `placement` itself, never
    // `grid`: on a frame change one can update before the other.
    readonly property var placement: scan && scan.kind === "grid" && scan.grid ? scan.grid : null
    readonly property bool grid: placement !== null
    property string error: ""
    signal tilesNeeded(int z, int x0, int y0, int x1, int y1)
    // The view centre once a pan or zoom settles, when it moved since the last
    // report; the surface sends it as `view_center` and the engine decides the
    // hand-off. The camera is never moved from here in answer.
    signal viewSettled(real lat, real lon)

    // The frame is Web Mercator: the unit square is the world, x east, y
    // south. Defined once here in double and handed to the shaders as the
    // view centre's offset from the site plus the scale.
    readonly property real maxLatitude: 85.05112878
    function mercatorX(lon) { return (lon + 180) / 360; }
    function mercatorY(lat) {
        var phi = Math.max(-maxLatitude, Math.min(maxLatitude, lat)) * Math.PI / 180;
        return (1 - Math.asinh(Math.tan(phi)) / Math.PI) / 2;
    }
    function longitude(mx) { return mx * 360 - 180; }
    function latitude(my) { return Math.atan(Math.sinh(Math.PI * (1 - 2 * my))) * 180 / Math.PI; }
    readonly property var site: scan ? scan.site : null
    readonly property real siteLat: site ? site.lat : 0
    readonly property real siteMx: site ? mercatorX(site.lon) : 0.5
    readonly property real siteMy: site ? mercatorY(site.lat) : 0.5
    // Ground kilometres per Mercator unit at the site's latitude, on the
    // shader's 6371 km sphere; span and the range rings are measured there.
    readonly property real kmPerUnit: 2 * Math.PI * 6371 * Math.cos(siteLat * Math.PI / 180)

    // Camera. `center` is a longitude/latitude point, or null for the home
    // view: the site offset by `home` kilometres east and north. `span` is
    // the ground distance across the shorter viewport side at the scan site,
    // never under 25 km. The longer side shows at most one world, and the view
    // stays inside the tile pyramid on both axes; the whole network fits one
    // Mercator world, so nothing wraps at the date line.
    property var center: null
    readonly property point home: Qt.point(-5, 15)
    property real span: 210
    readonly property real maxSpan: kmPerUnit * Math.min(width, height) / Math.max(1, width, height)
    readonly property real pixelsPerKm: Math.max(Math.min(width, height) / Math.max(25, span), Math.max(width, height) / kmPerUnit)
    readonly property real worldPixels: pixelsPerKm * kmPerUnit
    readonly property real unitsPerPixel: 1 / worldPixels
    readonly property real wantedX: center ? mercatorX(center.x) : siteMx + home.x / kmPerUnit
    readonly property real wantedY: center ? mercatorY(center.y) : siteMy - home.y / kmPerUnit
    readonly property real viewCenterX: Math.max(width/2*unitsPerPixel, Math.min(1-width/2*unitsPerPixel, wantedX))
    readonly property real viewCenterY: Math.max(height/2*unitsPerPixel, Math.min(1-height/2*unitsPerPixel, wantedY))
    // The home span: a radar's neighbourhood, or the whole country for the
    // composite.
    readonly property real homeSpan: grid ? 1500 : 210
    function reset() { center = null; span = Math.min(homeSpan, maxSpan); }
    signal navigated(real lat, real lon, real spanKm)
    function zoom(value, notify) {
        if (!interactive) return;
        span = Math.max(25, Math.min(maxSpan, value));
        if (notify !== false) navigated(centerLat, centerLon, span);
    }
    function look(mx, my) { center = Qt.point(longitude(mx), latitude(my)); }
    // Centre exactly on a place. Loading frames and radar hand-offs must
    // not call this; the camera is the user's (DESIGN.md, location).
    function lookAt(lat, lon) {
        // A fresh object: assigning Qt.point onto `var` can no-op when Qt
        // treats the previous point as equal, so the camera never moves.
        center = { x: Number(lon), y: Number(lat) };
    }
    signal resetRequested()
    // The keyboard pan (DESIGN.md, keyboard map): one step is an eighth of
    // the viewport's shorter side, in the given screen direction.
    function pan(dx, dy) {
        if (!interactive) return;
        var stepPixels = Math.max(1, Math.round(Math.min(width, height) / 8)) * unitsPerPixel;
        look(viewCenterX + dx * stepPixels, viewCenterY + dy * stepPixels);
        navigated(centerLat, centerLon, span);
    }
    readonly property real centerLat: latitude(viewCenterY)
    readonly property real centerLon: longitude(viewCenterX)
    // Centre the home view on a station before its frame arrives, so the
    // camera lands once where the state's site will put it.
    function jumpTo(lat, lon) {
        var k = 2 * Math.PI * 6371 * Math.cos(lat * Math.PI / 180);
        center = Qt.point(longitude(mercatorX(lon) + home.x / k), latitude(mercatorY(lat) - home.y / k));
    }
    // A hand-off changes the site under a camera the user placed. The span
    // is measured at the site's latitude, so it is rescaled to keep the
    // ground scale on screen exactly where it was.
    property real heldKmPerUnit: 0
    // Restoring a remembered view sets span itself; a site change under that
    // restore must not rescale it.
    property bool holdSpan: false
    onKmPerUnitChanged: {
        if (center && heldKmPerUnit > 0 && !holdSpan) span *= kmPerUnit / heldKmPerUnit;
        heldKmPerUnit = kmPerUnit;
    }
    function distanceKm(lat1, lon1, lat2, lon2) {
        var r = Math.PI / 180, dp = (lat2 - lat1) * r, dl = (lon2 - lon1) * r;
        var h = Math.sin(dp / 2) ** 2 + Math.cos(lat1 * r) * Math.cos(lat2 * r) * Math.sin(dl / 2) ** 2;
        return 2 * 6371 * Math.asin(Math.sqrt(Math.max(0, Math.min(1, h))));
    }
    // The table station nearest the view centre, for `n`; null before hello.
    function nearest() {
        var best = null, bestKm = Infinity;
        for (var s of sites) {
            if (s.kind === "grid") continue;  // the composite has no antenna
            var km = distanceKm(centerLat, centerLon, s.lat, s.lon);
            if (km < bestKm) { bestKm = km; best = s; }
        }
        return best;
    }
    // Mercator units to viewport pixels.
    function sx(mx) { return width / 2 + (mx - viewCenterX) * worldPixels; }
    function sy(my) { return height / 2 + (my - viewCenterY) * worldPixels; }

    // Tile layer (DESIGN.md, basemap tiles). The zoom whose 512 px tiles land
    // nearest 1:1 on screen is requested for the visible rectangle when the
    // camera settles; every announced tile inside the last request is drawn,
    // placed by the camera so a pan needs no new tiles until it settles.
    readonly property int tileZoom: Math.max(0, Math.min(22, Math.round(Math.log2(worldPixels / 512))))
    property var tiles: ({})         // "z/x/y" -> {set, path, labels}
    property int displayedLevel: -1
    property var request: null       // the last tiles_needed rectangle
    // Whether any drawn tile came from the osm set; the surface shows the
    // engine's attribution while it is true (docs/protocol.md, basemap).
    property bool osmOnScreen: false
    onViewCenterXChanged: { settle.restart(); Qt.callLater(refreshOverlay); }
    onViewCenterYChanged: { settle.restart(); Qt.callLater(refreshOverlay); }
    onUnitsPerPixelChanged: settle.restart()
    // A state change re-asks even for an unchanged rectangle: a restarted
    // engine publishes under a new generation.
    onScanChanged: { scheduleLayout(); if (scan) { settle.reask = true; settle.restart(); } else { reportedLat = NaN; reportedSpan = NaN; } }
    Timer { id: settle; interval: 120; property bool reask: true; onTriggered: { map.requestTiles(); map.reportCenter(); } }
    // Forgotten when the engine goes away, so a reconnect reports the centre
    // the camera is at rather than the one the old daemon knew.
    property real reportedLat: NaN
    property real reportedLon: NaN
    property real reportedSpan: NaN
    function reportCenter() {
        if (!scan || (centerLat === reportedLat && centerLon === reportedLon && span === reportedSpan)) return;
        reportedLat = centerLat; reportedLon = centerLon; reportedSpan = span;
        viewSettled(centerLat, centerLon);
    }
    function tileRect(z) {
        var n = Math.pow(2, z);
        var tile = function(units) { return Math.max(0, Math.min(n - 1, Math.floor(units * n))); };
        return { z: z,
                 x0: tile(viewCenterX - width / 2 * unitsPerPixel), y0: tile(viewCenterY - height / 2 * unitsPerPixel),
                 x1: tile(viewCenterX + width / 2 * unitsPerPixel), y1: tile(viewCenterY + height / 2 * unitsPerPixel) };
    }
    function requestTiles() {
        if (!scan || width <= 0 || height <= 0) return;
        // At most 64 tiles per request (docs/protocol.md): a huge viewport
        // steps out a level rather than asking for a rectangle it cannot have.
        var rect = tileRect(tileZoom);
        while (rect.z > 0 && (rect.x1 - rect.x0 + 1) * (rect.y1 - rect.y0 + 1) > 64) rect = tileRect(rect.z - 1);
        var unchanged = request && ["z", "x0", "y0", "x1", "y1"].every(k => request[k] === rect[k]);
        request = rect;
        if (!unchanged || settle.reask) tilesNeeded(rect.z, rect.x0, rect.y0, rect.x1, rect.y1);
        settle.reask = false;
        rebuildTileModel();
    }
    function tileReady(tile) {
        // Late replies from superseded requests must not grow a hidden cache.
        if (!inside(tile.z, tile.x, tile.y, request)) return;
        var key = tile.z + "/" + tile.x + "/" + tile.y;
        var old = tiles[key];
        tiles[key] = { set: tile.set, path: tile.path, labels: tile.labels,
                       ready: !!old && old.path === tile.path && old.ready };
        rebuildTileModel();
    }
    // Keep delegates for tiles that stay, so a pan or a re-announcement never
    // recreates an Image that is already on screen.
    ListModel { id: tileModel }
    function inside(z, x, y, rect) {
        return rect && z === rect.z && x >= rect.x0 && x <= rect.x1 && y >= rect.y0 && y <= rect.y1;
    }
    function imageReady(key, path, ready) {
        if (!tiles[key] || tiles[key].path !== path) return;
        tiles[key].ready = ready;
        Qt.callLater(rebuildTileModel);
    }
    function rebuildTileModel() {
        if (!request) return;
        if (displayedLevel < 0) displayedLevel = request.z;
        var complete = true;
        for (var y = request.y0; y <= request.y1; y++)
            for (var x = request.x0; x <= request.x1; x++) {
                var tile = tiles[request.z + "/" + x + "/" + y];
                if (!tile || !tile.ready) complete = false;
            }
        // Switch atomically: overlapping translucent masks would double the
        // line weight if both levels painted at once. Images preload hidden.
        if (complete) displayedLevel = request.z;
        var heldRect = tileRect(displayedLevel), wanted = {}, retained = {};
        var osm = false, names = {}, candidates = [];
        for (var key in tiles) {
            var parts = key.split("/"), z = Number(parts[0]), col = Number(parts[1]), row = Number(parts[2]);
            var shown = inside(z, col, row, heldRect);
            if (!shown && !inside(z, col, row, request)) continue;
            var t = tiles[key];
            retained[key] = t;
            wanted[key] = { key: key, level: z, column: col, row: row, path: t.path, set: t.set };
            if (!shown || !t.ready) continue;
            if (t.set === "osm") osm = true;
            for (var place of t.labels) candidates.push(place);
        }
        tiles = retained;
        osmOnScreen = osm;
        // Rank is meaningful within a source; class breaks equal ranks, then
        // name and position make order independent of tile arrival order.
        var classes = {capital: 0, city: 1, town: 2, village: 3};
        candidates.sort((a, b) => a.rank - b.rank || classes[a.class] - classes[b.class]
                        || a.name.localeCompare(b.name) || a.lat - b.lat || a.lon - b.lon);
        var nextPlaces = [];
        for (var p of candidates) {
            var id = p.name + "/" + p.lat.toFixed(5) + "/" + p.lon.toFixed(5);
            if (!names[id]) { names[id] = true; nextPlaces.push(p); }
        }
        if (JSON.stringify(places) !== JSON.stringify(nextPlaces)) places = nextPlaces;
        for (var i = tileModel.count - 1; i >= 0; i--) {
            var entry = tileModel.get(i);
            if (!wanted[entry.key]) { tileModel.remove(i); continue; }
            if (wanted[entry.key].path !== entry.path) tileModel.setProperty(i, "path", wanted[entry.key].path);
            delete wanted[entry.key];
        }
        for (var key2 in wanted) tileModel.append(wanted[key2]);
    }
    Repeater {
        model: tileModel
        ShaderEffect {
            // Roles avoid Item's own x, y, and z.
            required property string key
            visible: level === map.displayedLevel
            required property int level
            required property int column
            required property int row
            required property string path
            readonly property real n: Math.pow(2, level)
            // Edges round to whole pixels so neighbouring tiles meet without
            // a seam, at the cost of up to half a pixel of drift.
            readonly property real edgeX: map.sx(column / n)
            readonly property real edgeY: map.sy(row / n)
            x: Math.round(edgeX)
            y: Math.round(edgeY)
            width: Math.round(map.sx((column + 1) / n)) - Math.round(edgeX)
            height: Math.round(map.sy((row + 1) / n)) - Math.round(edgeY)
            property var mask: Image {
                source: map.tileRoot + path
                visible: false
                smooth: true
                mipmap: false
                cache: false
                asynchronous: true
                onStatusChanged: map.imageReady(key, path, status === Image.Ready)
                Component.onCompleted: map.imageReady(key, path, status === Image.Ready)
            }
            property color boundaries: Qt.alpha(map.theme.foreground, .32)
            property color water: Qt.alpha(map.theme.accent, .55)
            property color minorRoads: Qt.alpha(map.theme.foreground, .24)
            property color majorRoads: Qt.alpha(map.theme.foreground, .48)
            fragmentShader: "shaders/tile.frag.qsb"
            onStatusChanged: if (status === ShaderEffect.Error) map.error = "Basemap GPU shader failed: " + log
        }
    }

    // Place labels from the displayed tiles, ranked before collision layout.
    // Laid out in site-relative pixels. Pan translates the parent; collision
    // runs on zoom, resize, theme, data change, or leaving the padded region.
    property var places: []
    property var labels: []
    property var siteLabels: []
    // Station names show from this scale up (see rebuildLabels).
    readonly property real stationLabelMinPxPerKm: .2
    onSitesChanged: scheduleLayout()
    onSiteIdChanged: scheduleLayout()
    onWidthChanged: { scheduleLayout(); settle.restart(); }
    onHeightChanged: { scheduleLayout(); settle.restart(); }
    onWorldPixelsChanged: { scheduleLayout(); Qt.callLater(refreshOverlay); }
    onLabelSizeChanged: scheduleLayout()
    onPlacesChanged: scheduleLayout()
    onThemeChanged: scheduleLayout()
    Component.onCompleted: scheduleLayout()
    function scheduleLayout() { Qt.callLater(rebuildLabels); }
    // A padded viewport bounds text and dashed-path work at every zoom.
    // Small pans only translate the scene; replenish before the padding runs
    // out. Keep the anchor fixed between replenishments.
    property real overlayX: siteMx
    property real overlayY: siteMy
    property real overlayScale: 1
    readonly property real overlayHalfX: (width/2 + 256) / overlayScale
    readonly property real overlayHalfY: (height/2 + 256) / overlayScale
    function refreshOverlay() {
        if (Math.abs(viewCenterX-overlayX)*worldPixels > 128
            || Math.abs(viewCenterY-overlayY)*worldPixels > 128
            || worldPixels/overlayScale > 2
            || (width/2+128)*unitsPerPixel > overlayHalfX
            || (height/2+128)*unitsPerPixel > overlayHalfY) {
            overlayX = viewCenterX; overlayY = viewCenterY; overlayScale = worldPixels;
            scheduleLayout();
        }
    }
    TextMetrics {
        id: labelMetrics
        font.family: map.theme.font
        font.pixelSize: map.labelSize
    }
    // A station's map label: NEXRAD ids (KTLX) are the label; SMHI ids are
    // the town folded to lowercase ASCII, so show the town, capitalised
    // like an id ("ÅTVIDABERG", not "atvidaberg"). Inline, not Sites.js:
    // the map harness loads this file alone.
    function stationLabel(s) {
        return s && s.name && s.id === s.id.toLowerCase() ? s.name.toUpperCase() : s ? s.id : "";
    }
    readonly property string activeLabel: stationLabel(sites.find(s => s.id === siteId) || {id: siteId})
    function rebuildLabels() {
        var started = Date.now();
        if (!scan) { labels = []; siteLabels = []; return; }
        labelMetrics.text = activeLabel;
        var occupied = [{x:-7, y:-7, w:14, h:14},
                        {x:7, y:4, w:labelMetrics.advanceWidth+6, h:16}], result = [], stations = [];
        // Station IDs take priority over place names. Reserve every marker
        // first; co-located archived/test stations must not cover each other.
        // Names nearest the view centre are placed first, so at network zoom
        // the ones that collide drop from the edges, never at random.
        var cx = overlayX, cy = overlayY;
        var candidates = sites.filter(s => s.id !== siteId && s.kind !== "grid"
            && Math.abs(mercatorX(s.lon)-overlayX) <= overlayHalfX
            && Math.abs(mercatorY(s.lat)-overlayY) <= overlayHalfY)
            .sort((a, b) => Math.hypot(mercatorX(a.lon)-cx, mercatorY(a.lat)-cy) - Math.hypot(mercatorX(b.lon)-cx, mercatorY(b.lat)-cy)
                            || a.id.localeCompare(b.id));
        for (var s of candidates) {
            var mx = (mercatorX(s.lon) - siteMx) * worldPixels;
            var my = (mercatorY(s.lat) - siteMy) * worldPixels;
            occupied.push({x:mx-4, y:my-4, w:8, h:8});
        }
        // Wider than a few countries (the web client's zoom 3, about 5 km a
        // pixel at 60° N) the markers stay and the names go, as on the phone.
        if (pixelsPerKm < stationLabelMinPxPerKm) candidates = [];
        for (var s of candidates) {
            var tx = (mercatorX(s.lon) - siteMx) * worldPixels;
            var ty = (mercatorY(s.lat) - siteMy) * worldPixels;
            labelMetrics.text = stationLabel(s);
            var tw = labelMetrics.advanceWidth, chosen = null;
            for (var q of [{x:tx+10,y:ty+4}, {x:tx-tw-16,y:ty+4},
                           {x:tx+10,y:ty-20}, {x:tx-tw-16,y:ty-20}]) {
                if (occupied.some(o => q.x<o.x+o.w+5 && q.x+tw+11>o.x && q.y<o.y+o.h+4 && q.y+20>o.y)) continue;
                chosen = q; break;
            }
            if (!chosen) continue;
            occupied.push({x:chosen.x,y:chosen.y,w:tw+6,h:16});
            stations.push({name:stationLabel(s), x:chosen.x, y:chosen.y, width:tw+6});
        }
        siteLabels = stations;
        for (var p of places) {
            labelMetrics.text = p.name;
            var tx = (mercatorX(p.lon) - siteMx) * worldPixels, ty = (mercatorY(p.lat) - siteMy) * worldPixels;
            var tw = labelMetrics.advanceWidth;
            var candidates = [{x:tx+7,y:ty-8},{x:tx-tw-12,y:ty-8},
                              {x:tx-tw/2,y:ty+7},{x:tx-tw/2,y:ty-23}];
            var chosen = null;
            for (var q of candidates) {
                if(occupied.some(o => q.x<o.x+o.w+5 && q.x+tw+11>o.x && q.y<o.y+o.h+4 && q.y+20>o.y)) continue;
                chosen=q; break;
            }
            if (!chosen) continue;
            occupied.push({x:chosen.x,y:chosen.y,w:tw+6,h:16});
            result.push({name:p.name, x:chosen.x, y:chosen.y,
                         width:tw+6, markerX:tx, markerY:ty});
        }
        labels = result;
        if (Quickshell.env("OMASTORM_PROFILE")) console.log("OVERLAY_MS", Date.now()-started);
    }

    // Great-circle destinations on the radar's 6371 km sphere, projected to
    // Mercator. A screen-space ellipse is wrong at high latitudes. Geometry
    // stays fixed inside the padded region; zoom scales points and pan
    // translates the parent. Unwrap longitude about each station.
    // A ring `s.km` from the station (S29), else the data's edge.
    function ringKm(s) { return s.km !== undefined ? s.km : coverageKm; }
    function coveragePoints(s) {
        var phi = s.lat * Math.PI / 180, lambda = s.lon * Math.PI / 180;
        var d = ringKm(s) / 6371, points = [];
        for (var i = 0; i <= 360; i++) {
            var bearing = (i % 360) * Math.PI / 180;
            var lat = Math.asin(Math.sin(phi)*Math.cos(d) + Math.cos(phi)*Math.sin(d)*Math.cos(bearing));
            var dl = Math.atan2(Math.sin(bearing)*Math.sin(d)*Math.cos(phi), Math.cos(d)-Math.sin(phi)*Math.sin(lat));
            points.push(Qt.point(mercatorX((lambda+dl)*180/Math.PI)-siteMx, mercatorY(lat*180/Math.PI)-siteMy));
        }
        return points;
    }
    function coverageInReach(s) {
        var distance = distanceKm(latitude(overlayY), longitude(overlayX), s.lat, s.lon);
        // Mercator ground scale never exceeds its equatorial scale. This
        // conservative padded-view radius cannot cull an arc crossing the view.
        var radius = Math.hypot(overlayHalfX, overlayHalfY)*2*Math.PI*6371;
        return Math.abs(distance-ringKm(s)) <= radius;
    }
    // Clip before Qt tessellates dashes, including circles whose centres are
    // outside the view. Coordinates remain relative to the active-site copy.
    function clippedCoverage(points) {
        var lines = [], line = [], previous = null;
        for (var i=1; i<points.length; i++) {
            var a = points[i-1], b = points[i], lo = 0, hi = 1;
            for (var axis=0; axis<2; axis++) {
                var start = axis ? a.y : a.x, delta = axis ? b.y-a.y : b.x-a.x;
                var middle = axis ? overlayY-siteMy : overlayX-siteMx;
                var reach = axis ? overlayHalfY : overlayHalfX;
                if (Math.abs(delta)<1e-15) {
                    if (Math.abs(start-middle)>reach) hi = -1;
                } else {
                    var t0 = (middle-reach-start)/delta, t1 = (middle+reach-start)/delta;
                    lo = Math.max(lo, Math.min(t0,t1)); hi = Math.min(hi, Math.max(t0,t1));
                }
            }
            if (lo>hi) { previous = null; continue; }
            var p = Qt.point((a.x+(b.x-a.x)*lo)*100000, (a.y+(b.y-a.y)*lo)*100000);
            var q = Qt.point((a.x+(b.x-a.x)*hi)*100000, (a.y+(b.y-a.y)*hi)*100000);
            if (!previous || Math.hypot(p.x-previous.x,p.y-previous.y)>1e-7) {
                line = [p]; lines.push(line);
            }
            line.push(q); previous = q;
        }
        return lines;
    }
    readonly property var coverageSites: {
        if (!drawable || !site || grid) return [];
        // Only the active radar gets a footprint; overlapping network circles
        // obscure geography at continental zoom. Use the measured scan site.
        // Its solid rings (S29: one, or the reach and a height inside it).
        return rings.filter(r => r.strong).map(r => ({id:siteId, lat:site.lat, lon:site.lon, km:r.km}));
    }
    // The faint rings (S29): the data's edge under a height, the lowest
    // beam 2 km up.
    readonly property var faintSites: !drawable || !site || grid ? []
        : rings.filter(r => !r.strong).map(r => ({id:siteId, lat:site.lat, lon:site.lon, km:r.km}))
    // S25: My mosaic's chosen radars at their reach, [{id, lat, lon, km}]:
    // the checklist's while it is open, the set's while My mosaic shows.
    property var mosaicCircles: []
    // S30: where a My mosaic height frame's holes are hatched: its chosen
    // radars' reach circles ([{lat, lon, km}], the window's blue circles by
    // default), as the shader wants them: latitude and longitude less the
    // site's (radians) and reach (metres), at most 12. Empty: hatched
    // everywhere, as before.
    property var hatchSites: mosaicCircles
    // S24b: a provider composite's product (sweden, nordic), made by the
    // engine from all its radars. Its Height is not hatched: 41 radars'
    // circles do not fit the shader's 12, and its box has no radar in the
    // corners; nor are My mosaic's circles its business.
    readonly property bool compositeProduct: grid && sites.some(s => s.id === siteId && s.kind === "grid" && s.provider !== "mosaic")
    readonly property var hatchCircles: !placement || !scan || scan.product !== "CAPPI" || !hatchSites ? []
        : hatchSites.slice(0, 12).map(c => Qt.vector4d(c.lat * Math.PI / 180, (mercatorX(c.lon) - siteMx) * 2 * Math.PI, c.km * 1000, 0))
    function circleAt(i) { return i < hatchCircles.length ? hatchCircles[i] : Qt.vector4d(0, 0, 0, 0); }

    // Upload the immutable sweep and its azimuth lookup once. Pan/zoom updates
    // shader uniforms; the polar-to-screen lookup runs in the shader and no
    // JavaScript visits radar cells.
    Image {
        id: sweepTexture
        source: map.texture
        visible: false
        smooth: false
        mipmap: false
    }
    Image {
        id: azimuthTexture
        source: map.azimuthLut
        visible: false
        smooth: false
        mipmap: false
    }
    // The frame's palette as a bands x 1 strip; the shader samples
    // texel centers, so radar and legend share the socket palette.
    // The strip stays visible so its children get scene-graph nodes
    // (a hidden Item's children never render into a layer); the
    // source hides it on screen while rendering it into the texture.
    Item {
        id: paletteStrip
        width: Math.max(1, map.bands); height: 1
        Repeater {
            model: map.scan ? map.scan.palette : []
            Rectangle {
                required property string modelData
                required property int index
                x: index; width: 1; height: 1; color: modelData
            }
        }
    }
    ShaderEffectSource {
        id: paletteTexture
        sourceItem: paletteStrip
        hideSource: true
        visible: false
        smooth: false
        mipmap: false
    }
    // For a code texture: each code's class + 1 as a 256 x 1 strip (R),
    // with the engine's rule (docs/protocol.md, code texture): codes 0 and 1
    // draw nothing; otherwise value = (code - offset) / scale in f32, the
    // class is the number of bounds at or below it, minus one, kept inside
    // the palette. Drawn as runs of equal class (a dozen rectangles) the way
    // the palette strip is, and rebuilt only when the station's encoding
    // changes: never per frame, and never when a loop moves between frames
    // with and without a code texture (rebuilding it there stalled the loop).
    readonly property string codeKey: scan && scan.scale > 0 && scan.bounds && scan.palette
        ? [scan.offset, scan.scale, scan.palette.length].concat(scan.bounds).join(",") : ""
    property var codeRuns: []
    onCodeKeyChanged: {
        var runs = [];
        if (codeKey !== "") {
            var bounds = scan.bounds, classes = scan.palette.length;
            var offset = Math.fround(scan.offset), scale = Math.fround(scan.scale);
            for (var code = 0; code < 256; code++) {
                var cls = 0;
                if (code >= 2) {
                    var value = Math.fround(Math.fround(code - offset) / scale), above = 0;
                    for (var b of bounds) if (Math.fround(b) <= value) above++;
                    cls = Math.max(0, Math.min(classes - 1, above - 1)) + 1;
                }
                var last = runs[runs.length - 1];
                if (last && last.value === cls) last.width++;
                else runs.push({x: code, width: 1, value: cls});
            }
        }
        codeRuns = runs;
    }
    Item {
        id: classStrip
        width: 256; height: 1
        Repeater {
            model: map.codeRuns
            Rectangle {
                required property var modelData
                x: modelData.x; width: modelData.width; height: 1
                color: Qt.rgba(modelData.value / 255, 0, 0, 1)
            }
        }
    }
    ShaderEffectSource {
        id: classTexture
        sourceItem: classStrip
        hideSource: true
        visible: false
        smooth: false
        mipmap: false
    }
    ShaderEffect {
        id: radarEffect
        visible: map.drawable
        // The radar alone, not the basemap: .6 under UNAVAILABLE.
        opacity: map.radarOpacity
        anchors.fill: parent
        onStatusChanged: if (status === ShaderEffect.Error) map.error = "Radar GPU shader failed: " + log
        Component.onCompleted: if (GraphicsInfo.api === GraphicsInfo.Software) map.error = "Radar requires GPU rendering (OpenGL/Vulkan)."
        property var sweep: sweepTexture
        property var azimuthLut: azimuthTexture
        property var swatches: paletteTexture
        property var classes: classTexture
        property int useCodes: map.codes && map.placement ? 1 : 0
        property int bands: map.bands
        // Sweep geometry travels as uniforms; the frame's numbers are the
        // only radar values QML ever touches, and they are geometry, not data.
        property int rays: map.scan ? map.scan.rays : 0
        property int gates: map.scan ? map.scan.gates : 0
        property real firstGateM: map.scan ? map.scan.firstGateM : 0
        property real gateSpacingM: map.scan ? map.scan.gateSpacingM : 1
        property real elevationDeg: map.scan ? map.scan.elevationDeg : 0
        property int weakBelow: map.weakBelow
        // A grid frame's rectangle: its north-west corner's offset from the
        // site and its size, in Mercator units, and its size in texels.
        property int kind: map.placement ? 1 : 0
        property vector2d gridOrigin: map.placement ? Qt.vector2d(map.mercatorX(map.placement.west) - map.siteMx, map.mercatorY(map.placement.north) - map.siteMy) : Qt.vector2d(0, 0)
        property vector2d gridSize: map.placement ? Qt.vector2d(map.mercatorX(map.placement.east) - map.mercatorX(map.placement.west), map.mercatorY(map.placement.south) - map.mercatorY(map.placement.north)) : Qt.vector2d(1, 1)
        property vector2d gridTexels: map.placement ? Qt.vector2d(map.placement.xsize, map.placement.ysize) : Qt.vector2d(0, 0)
        property vector2d viewport: Qt.vector2d(width, height)
        // The camera as the shader wants it: the view centre relative to the
        // site in Mercator units, the scale, and the site's latitude.
        property vector2d centerOffset: Qt.vector2d(map.viewCenterX - map.siteMx, map.viewCenterY - map.siteMy)
        property real unitsPerPixel: map.unitsPerPixel
        property real siteLatDeg: map.siteLat
        property int treatment: map.treatment === "PIXELS" ? 0 : map.treatment === "GLYPHS" ? 1 : 2
        // S29: a height slice's no-data texels hatched, and the reach.
        property int hatchNodata: map.scan && map.scan.product === "CAPPI" && !map.compositeProduct ? 1 : 0
        // S24a: a storm height's "at least" texels (G bit 8) hatched over
        // their colour.
        property int hatchAtLeast: map.scan && map.scan.product === "ETOP" ? 1 : 0
        property real reachM: map.reachShownKm * 1000
        // S24d: a storm height lit as a surface while Relief is on; the
        // frame's scale and offset give each texel's height.
        property int relief: map.relief && map.scan && map.scan.product === "ETOP" ? 1 : 0
        property real codeScale: map.scan && map.scan.scale > 0 ? map.scan.scale : 1
        property real codeOffset: map.scan && map.scan.offset ? map.scan.offset : 0
        property color hatchInk: map.theme ? map.theme.foreground : "white"
        property vector4d hatchColor: Qt.vector4d(hatchInk.r, hatchInk.g, hatchInk.b, .45)
        // S30: hatched only inside the chosen radars' reach (radar.frag).
        property int circleCount: map.hatchCircles.length
        property vector4d circle0: map.circleAt(0)
        property vector4d circle1: map.circleAt(1)
        property vector4d circle2: map.circleAt(2)
        property vector4d circle3: map.circleAt(3)
        property vector4d circle4: map.circleAt(4)
        property vector4d circle5: map.circleAt(5)
        property vector4d circle6: map.circleAt(6)
        property vector4d circle7: map.circleAt(7)
        property vector4d circle8: map.circleAt(8)
        property vector4d circle9: map.circleAt(9)
        property vector4d circle10: map.circleAt(10)
        property vector4d circle11: map.circleAt(11)
        fragmentShader: "shaders/radar.frag.qsb"
    }
    Item {
        id: overlayCamera
        x: map.sx(map.siteMx)
        y: map.sy(map.siteMy)
        visible: !!map.scan
        Repeater {
            id: coverageRepeater
            model: map.coverageSites
            Loader {
                id: footprint
                required property var modelData
                active: map.coverageInReach(modelData)
                // Shape geometry is vector-backed; no map-sized texture or
                // canvas is allocated for a coverage circle at close zoom.
                sourceComponent: Shape {
                    readonly property var vertices: map.coveragePoints(footprint.modelData)
                    // Keep the path fixed while zooming. Scale scene-graph
                    // geometry, not hundreds of JS points on every tick.
                    transform: Scale { xScale: map.worldPixels/100000; yScale: xScale }
                    ShapePath {
                        fillColor: "transparent"
                        // The circle of what is shown (S29), in accent.
                        strokeColor: Qt.alpha(map.theme.accent, .6)
                        strokeWidth: 100000/map.worldPixels
                        strokeStyle: ShapePath.DashLine
                        dashPattern: [3, 5]
                        PathMultiline {
                            paths: map.clippedCoverage(vertices)
                        }
                    }
                }
            }
        }
        // My mosaic's radars (S25), solid in accent.
        Repeater {
            model: map.mosaicCircles
            Loader {
                id: mosaicRing
                required property var modelData
                active: map.coverageInReach(modelData)
                sourceComponent: Shape {
                    readonly property var vertices: map.coveragePoints(mosaicRing.modelData)
                    transform: Scale { xScale: map.worldPixels/100000; yScale: xScale }
                    ShapePath {
                        fillColor: "transparent"
                        strokeColor: Qt.alpha(map.theme.accent, .8)
                        strokeWidth: 1.5*100000/map.worldPixels
                        PathMultiline {
                            paths: map.clippedCoverage(vertices)
                        }
                    }
                }
            }
        }
        // The faint rings (S29), finer dashes in the foreground.
        Repeater {
            model: map.faintSites
            Loader {
                id: faintRing
                required property var modelData
                active: map.coverageInReach(modelData)
                sourceComponent: Shape {
                    readonly property var vertices: map.coveragePoints(faintRing.modelData)
                    transform: Scale { xScale: map.worldPixels/100000; yScale: xScale }
                    ShapePath {
                        fillColor: "transparent"
                        strokeColor: Qt.alpha(map.theme.foreground, .3)
                        strokeWidth: 100000/map.worldPixels
                        strokeStyle: ShapePath.DashLine
                        dashPattern: [1, 4]
                        PathMultiline {
                            paths: map.clippedCoverage(vertices)
                        }
                    }
                }
            }
        }
        // Range rings at the site's Mercator scale; over 200 km the scale
        // drifts by a couple of percent, which a ring drawn as a circle hides.
        // Every 50 km inside the sweep; the dashed footprint marks its edge
        // (240 km for SMHI), so a short-range radar never shows a ring past it.
        Repeater {
            model: map.grid ? [] : [50, 100, 150, 200].filter(r => r < (map.reachShownKm || map.coverageKm))
            Rectangle {
                required property int modelData
                visible: map.siteId !== ""
                width: 2 * modelData * map.pixelsPerKm
                height: width
                x: -width / 2; y: -height / 2
                radius: width / 2
                color: "transparent"
                border.width: 1
                border.color: Qt.alpha(map.theme.foreground, .18)
                antialiasing: true
            }
        }
        // Reference radars (S23): positions only, under the station marks,
        // no label, no ring; not stations, so nothing here selects them.
        Repeater {
            model: map.referenceSites
            Rectangle {
                required property var modelData
                x: (map.mercatorX(modelData.lon)-map.siteMx)*map.worldPixels-2
                y: (map.mercatorY(modelData.lat)-map.siteMy)*map.worldPixels-2
                width: 4; height: 4; radius: 2
                color: Qt.alpha(map.theme.foreground, .35)
                antialiasing: true
            }
        }
        Repeater {
            model: map.sites.filter(s => s.id !== map.siteId && s.kind !== "grid")
            Rectangle {
                required property var modelData
                x: (map.mercatorX(modelData.lon)-map.siteMx)*map.worldPixels-3
                y: (map.mercatorY(modelData.lat)-map.siteMy)*map.worldPixels-3
                width: 6; height: 6
                color: map.theme.background
                border.width: 1; border.color: Qt.alpha(map.theme.foreground, .7)
            }
        }
        Repeater {
            model: map.siteLabels
            Rectangle {
                required property var modelData
                x: modelData.x; y: modelData.y
                width: modelData.width; height: 16
                visible: overlayCamera.x+x >= 8 && overlayCamera.x+x+width <= map.width-8
                    && overlayCamera.y+y >= 26 && overlayCamera.y+y+height <= map.height-30
                color: Qt.alpha(map.theme.background, .88)
                Text {
                    x: 3; anchors.verticalCenter: parent.verticalCenter
                    text: modelData.name; color: Qt.alpha(map.theme.foreground, .75)
                    font.family: map.theme.font; font.pixelSize: map.labelSize
                }
            }
        }
        Repeater {
            model: map.labels
            Item {
                required property var modelData
                // Keep labels clear of the surface's corner annotations.
                visible: overlayCamera.x + modelData.x >= 8
                    && overlayCamera.x + modelData.x + modelData.width <= map.width - 8
                    && overlayCamera.y + modelData.y >= 26
                    && overlayCamera.y + modelData.y + 16 <= map.height - 30
                Rectangle {
                    x: modelData.markerX - 1; y: modelData.markerY - 1
                    width: 2; height: 2; color: map.theme.foreground
                }
                Rectangle {
                    x: modelData.x; y: modelData.y
                    width: modelData.width; height: 16
                    color: Qt.alpha(map.theme.background, .88)
                    Text {
                        x: 3; anchors.verticalCenter: parent.verticalCenter
                        text: modelData.name; color: map.theme.foreground
                        font.family: map.theme.font; font.pixelSize: map.labelSize
                    }
                }
            }
        }
        Rectangle { x: -7; y: -.5; width: 14; height: 1; color: map.theme.foreground; visible: map.siteId !== "" && !map.grid }
        Rectangle { x: -.5; y: -7; width: 1; height: 14; color: map.theme.foreground; visible: map.siteId !== "" && !map.grid }
        // The lock: an accent 1 px frame on the marker and the tag.
        Rectangle {
            x: -6; y: -6; width: 12; height: 12; color: "transparent"
            visible: map.locked && !map.grid
            border.width: 1; border.color: map.theme.accent
        }
        Rectangle {
            x: 7; y: 4; width: Math.floor(siteTag.implicitWidth) + 6; height: 16; color: map.theme.background
            visible: map.siteId !== "" && !map.grid
            border.width: map.locked ? 1 : 0; border.color: map.theme.accent
            Text {
                id: siteTag
                x: 3; anchors.verticalCenter: parent.verticalCenter
                text: map.activeLabel; color: map.theme.foreground
                font.family: map.theme.font; font.pixelSize: map.labelSize
            }
        }
    }
    MouseArea {
        anchors.fill: parent
        enabled: map.interactive
        cursorShape: pressed ? Qt.ClosedHandCursor : Qt.OpenHandCursor
        property real lastX
        property real lastY
        onPressed: mouse => { lastX=mouse.x; lastY=mouse.y; }
        onPositionChanged: mouse => {
            if(pressed && (mouse.x !== lastX || mouse.y !== lastY)) {
                map.look(map.viewCenterX - (mouse.x-lastX)*map.unitsPerPixel, map.viewCenterY - (mouse.y-lastY)*map.unitsPerPixel);
                lastX=mouse.x; lastY=mouse.y;
                map.navigated(map.centerLat, map.centerLon, map.span);
            }
        }
        onWheel: wheel => {
            // Zoom about the pointer: the ground under it stays put.
            var mx=map.viewCenterX+(wheel.x-width/2)*map.unitsPerPixel;
            var my=map.viewCenterY+(wheel.y-height/2)*map.unitsPerPixel;
            map.zoom(Math.min(map.span,map.maxSpan)*(wheel.angleDelta.y>0?.85:1/.85), false);
            map.look(mx-(wheel.x-width/2)*map.unitsPerPixel, my-(wheel.y-height/2)*map.unitsPerPixel);
            map.navigated(map.centerLat, map.centerLon, map.span);
        }
        onDoubleClicked: map.resetRequested()
    }
}
