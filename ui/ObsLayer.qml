import QtQuick

// The weather station layers over the map (S42, docs/protocol.md "Weather
// layers"): each station's air temperature as a small rounded label ("12°")
// on one cold-to-warm scale, and its wind as an arrow pointing where the
// wind blows TO, longer and heavier with speed, a ring when calm (under
// 0.5 m/s). Stations are thinned by zoom so nothing overlaps: a greedy
// pick in world pixels, recomputed when the zoom changes, never on a pan,
// so a label does not flicker while the map moves. Hover names the station
// with its provider, time, temperature, wind and gust. A legend sits in the
// bottom-right corner above the map credit, with the providers' credit.
//
// It is an overlay the size of the map (`map` is a RadarMap): the map's own
// mouse handling stays underneath; hover is a passive HoverHandler.
//
// S47: each arrow has its speed beside it in m/s, rounded ("7"), with the
// gust when the station reports one ("7 (12)"): past the arrow's head, so
// the temperature keeps the upwind side; the thinning cell grows by the
// number's room. The grid's arrows carry numbers on a sparser lattice
// (every other lattice point both ways). The legend lists the speed bands
// in m/s under an arrow of each length.
//
// S43: `source` stations, grid or both. From the MET Nordic grid, wind is
// the same arrows on the grid's points (thinned with the stations, which
// win their cells, so with both the grid fills the gaps between stations
// and draws under them, fainter), and temperature is the field under the
// radar: `underlay`, which the map draws (RadarMap.underlay).
Item {
    id: stationLayer
    property var map
    property var obs: null
    /// S43: the engine's `obs` with source grid, and where it comes from.
    property var grid: null
    property string source: "stations"
    property string runtime: ""
    readonly property bool stationsOn: source !== "grid"
    readonly property bool gridOn: source === "grid" || source === "both"
    property bool temp: false
    property bool wind: false
    property var theme
    property bool compact: false
    /// S46: the theme's type scale (a fallback for a bare host).
    readonly property var sizes: theme && theme.size ? theme.size : ({small: 10, caption: 11, body: 12, label: 13, title: 14, k: 1})
    /// S46: a click on the legend asks the host to enlarge it (legend
    /// zoom); the host says whether it is enlarged.
    property bool legendZoomed: false
    signal legendClicked()
    /// The legend's box and the hover card, for the host's wheel guard.
    readonly property Item legendItem: legendBox
    readonly property Item tipItem: tip
    /// The scale and the wind key; off, only the credit line shows.
    property bool showLegend: true
    /// Room kept free at the bottom right for the map credit.
    property real creditHeight: 30
    readonly property bool light: !!(theme && theme.light)
    readonly property var stations: stationsOn && obs && Array.isArray(obs.stations) ? obs.stations : []
    readonly property var gridPoints: gridOn && wind && grid && grid.wind && Array.isArray(grid.wind.points) ? grid.wind.points : []
    /// The grid's temperature for the map to draw under the radar, or null.
    readonly property var underlay: temp && gridOn && grid && grid.temperature && runtime
        ? { source: "file://" + runtime + grid.temperature.texture, bounds: grid.temperature.bounds } : null
    readonly property bool on: temp || wind
    visible: on

    // One scale for every provider, °C: colour stops cold to warm.
    readonly property var scale: [
        [-30, "#6a3d9a"], [-20, "#3f5fbf"], [-10, "#3f94d6"], [-2, "#8fd0ee"],
        [2, "#b8e3a8"], [8, "#e9e98a"], [14, "#f7c95c"], [20, "#f39a45"],
        [26, "#e5603a"], [32, "#c2233a"]
    ]
    function mix(a, b, t) {
        var ca = Qt.color(a), cb = Qt.color(b);
        return Qt.rgba(ca.r + (cb.r - ca.r) * t, ca.g + (cb.g - ca.g) * t, ca.b + (cb.b - ca.b) * t, 1);
    }
    function tempColor(c) {
        if (c <= scale[0][0]) return Qt.color(scale[0][1]);
        for (var i = 1; i < scale.length; i++) {
            if (c <= scale[i][0]) {
                var t = (c - scale[i - 1][0]) / (scale[i][0] - scale[i - 1][0]);
                return mix(scale[i - 1][1], scale[i][1], t);
            }
        }
        return Qt.color(scale[scale.length - 1][1]);
    }
    function inkOn(fill) {
        var c = Qt.color(fill);
        return 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b > 0.55 ? "#1b1d24" : "#ffffff";
    }
    function degrees(c) {
        var n = Math.round(c);
        return (n < 0 ? "−" + (-n) : String(n)) + "°";
    }
    readonly property color arrowInk: light ? "#1d2433" : "#f3f5fa"
    readonly property color arrowHalo: light ? Qt.rgba(1, 1, 1, .8) : Qt.rgba(0.05, 0.06, 0.09, .75)

    function hasWind(s) { return s.windMs !== null && s.windMs !== undefined && s.windDirDeg !== null && s.windDirDeg !== undefined; }
    function hasTemp(s) { return s.tempC !== null && s.tempC !== undefined; }
    // Arrows by speed band, as on the web (app.js OBS_ARROWS): [upper m/s,
    // length px, stroke px]. Each band is drawn once per theme into an
    // image (review S2); a station's arrow is that image, rotated.
    readonly property var bands: [[2, 14, 1.6], [4, 18, 1.8], [7, 23, 2.1], [11, 29, 2.5], [16, 35, 2.9], [1e9, 42, 3.3]]
    function band(ms) { for (var i = 0; i < bands.length; i++) if (ms < bands[i][0]) return i; return bands.length - 1; }
    /// S47: the bands in words, m/s: "<2", "2–4", … "16+".
    function bandName(i) {
        if (i === 0) return "<" + bands[0][0];
        if (i === bands.length - 1) return bands[i - 1][0] + "+";
        return bands[i - 1][0] + "–" + bands[i][0];
    }
    /// S47: the speed beside an arrow, m/s rounded, with the gust in
    /// brackets when there is one worth saying (above the mean).
    function speedText(s) {
        var text = String(Math.round(s.windMs));
        if (s.gustMs !== null && s.gustMs !== undefined && Math.round(s.gustMs) > Math.round(s.windMs))
            text += " (" + Math.round(s.gustMs) + ")";
        return text;
    }
    /// Review S5: the room a number takes past an arrow's head, for the
    /// thinning cell: the 95th percentile label's real width ("10 (15)" is
    /// about 40 px), measured with the label's own font.
    TextMetrics { id: numberMetrics; font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"; font.pixelSize: stationLayer.compact ? 9 : 10; font.bold: true }
    function textWidth(t) { numberMetrics.text = t; return numberMetrics.advanceWidth; }
    function numberRoomFor(samples) {
        if (!wind || !samples.length) return 0;
        var widths = samples.map(s => textWidth(speedText(s))).sort((a, b) => a - b);
        return Math.ceil(widths[Math.min(widths.length - 1, Math.floor(widths.length * .95))]) + 6;
    }
    function arrowLength(ms) { return bands[band(ms)][1]; }
    property var arrowUrls: []
    Repeater {
        model: stationLayer.bands.length
        Canvas {
            id: painter
            required property int index
            readonly property real len: stationLayer.bands[index][1]
            readonly property real weight: stationLayer.bands[index][2]
            // Off the layer's edge: painted, never seen.
            x: -200; y: -200
            width: 16; height: len + 2
            onPaint: {
                var ctx = getContext("2d");
                ctx.reset();
                var cx = width / 2, top = 1, bottom = height - 1, head = 4 + weight * 1.6;
                for (var pass = 0; pass < 2; pass++) {
                    ctx.strokeStyle = pass === 0 ? stationLayer.arrowHalo : stationLayer.arrowInk;
                    ctx.fillStyle = pass === 0 ? stationLayer.arrowHalo : stationLayer.arrowInk;
                    ctx.lineWidth = pass === 0 ? weight + 2.4 : weight;
                    ctx.lineCap = "round";
                    ctx.beginPath();
                    ctx.moveTo(cx, bottom);
                    ctx.lineTo(cx, top + head * .8);
                    ctx.stroke();
                    ctx.beginPath();
                    ctx.moveTo(cx, top);
                    ctx.lineTo(cx - head * .62, top + head);
                    ctx.lineTo(cx + head * .62, top + head);
                    ctx.closePath();
                    if (pass === 0) { ctx.lineWidth = 2.4; ctx.stroke(); }
                    ctx.fill();
                }
            }
            // toDataURL paints again, so the grab runs once per ink, later.
            property string grabbed: ""
            function grab() {
                var key = String(stationLayer.arrowInk);
                if (grabbed === key) return;
                grabbed = key;
                var urls = stationLayer.arrowUrls.slice();
                urls[index] = toDataURL("image/png");
                stationLayer.arrowUrls = urls;
            }
            onPainted: if (grabbed !== String(stationLayer.arrowInk)) Qt.callLater(grab)
            Connections {
                target: stationLayer
                function onArrowInkChanged() { painter.requestPaint(); }
            }
        }
    }

    // Thinning: the stations shown, [{s, mx, my}], at least a cell apart in
    // world pixels. A station with both values goes first, then by a stable
    // hash of its id, so the same stations win at the same zoom every time.
    // The cell grows with the wind (review S1): the 95th percentile arrow
    // among the candidates, plus the pill beside it.
    property var shown: []
    function cellFor(list) {
        if (!wind) return 40;
        var lengths = list.filter(p => hasWind(p.s)).map(p => arrowLength(p.s.windMs)).sort((a, b) => a - b);
        var p95 = lengths.length ? lengths[Math.min(lengths.length - 1, Math.floor(lengths.length * .95))] : 14;
        return (temp ? p95 + 34 : p95 + 10) + numberRoomFor(list.filter(p => hasWind(p.s)).map(p => p.s));
    }
    function hash(text) {
        var h = 2166136261;
        for (var i = 0; i < text.length; i++) { h ^= text.charCodeAt(i); h = Math.imul(h, 16777619) >>> 0; }
        return h;
    }
    function useful(s) { return (temp && hasTemp(s)) || (wind && hasWind(s)); }
    function rethin() {
        if (!map || !on) { if (shown.length) shown = []; return; }
        var world = map.worldPixels, cells = {}, out = [];
        thinnedAt = world;
        thinCount++;
        var list = stations.filter(useful).map(s => ({
            s: s, mx: map.mercatorX(s.lon), my: map.mercatorY(s.lat),
            rank: (hasTemp(s) && hasWind(s) ? 0 : 1) * 4294967296 + hash(s.id)
        }));
        var cell = cellFor(list);
        list.sort((a, b) => a.rank - b.rank);
        function clearAt(x, y) {
            var gx = Math.floor(x / cell), gy = Math.floor(y / cell);
            for (var dx = -1; dx <= 1; dx++)
                for (var dy = -1; dy <= 1; dy++)
                    for (var q of cells[(gx + dx) + ":" + (gy + dy)] || [])
                        if (Math.hypot(q.x - x, q.y - y) < cell) return false;
            return true;
        }
        for (var p of list) {
            var x = p.mx * world, y = p.my * world;
            if (!clearAt(x, y)) continue;
            var key = Math.floor(x / cell) + ":" + Math.floor(y / cell);
            (cells[key] = cells[key] || []).push({x: x, y: y});
            out.push(p);
        }
        // S43: the grid's arrows on a lattice, every k-th row and column so
        // they sit an arrow apart; the lattice does not move with the map,
        // so only the points near the view are made (again after a pan),
        // and a point a station's cell already holds gives way to it.
        var points = gridPoints;
        if (points.length) {
            var cols = grid.wind.cols > 0 ? grid.wind.cols : points.length;
            var spacingPx = (grid.wind.spacingKm || 24) * world / (2 * Math.PI * 6371 * Math.cos(map.centerLat * Math.PI / 180));
            // Numbers sit on every other lattice point, so half their room.
            var room = numberRoomFor(points.filter(g => g[2] !== null).slice(0, 400).map(g => ({windMs: g[2], gustMs: null})));
            var k = Math.max(1, Math.ceil((gridP95 + 10 + room / 2) / Math.max(1, spacingPx)));
            var margin = 80, gridTime = grid.time;
            for (var i = 0; i < points.length; i++) {
                var row = Math.floor(i / cols), col = i % cols, g = points[i];
                if (row % k || col % k || g[2] === null || g[3] === null) continue;
                var mx = map.mercatorX(g[1]), my = map.mercatorY(g[0]);
                var sx = map.sx(mx), sy = map.sy(my);
                if (sx < -margin || sy < -margin || sx > width + margin || sy > height + margin) continue;
                if (!clearAt(mx * world, my * world)) continue;
                // S47: numbers on every other lattice point both ways.
                out.push({ s: { id: "grid:" + i, grid: true, lat: g[0], lon: g[1], windMs: g[2], windDirDeg: g[3], time: gridTime },
                           mx: mx, my: my, numbered: (row / k) % 2 === 0 && (col / k) % 2 === 0 });
            }
        }
        shown = out;
    }
    // Review S2: a zoom thins again once it settles (a trailing debounce),
    // and only when the scale moved by more than a tenth since the last.
    property real thinnedAt: 0
    property int thinCount: 0          // for checks: how often it thinned
    Timer { id: thin; interval: 150; onTriggered: stationLayer.rethin() }
    function scheduleThin() {
        if (map && thinnedAt > 0 && Math.abs(map.worldPixels / thinnedAt - 1) < .1) return;
        thin.restart();
    }
    // S43: a pan brings other grid points into view (the stations stay).
    function schedulePan() { if (gridPoints.length) thin.restart(); }
    onStationsChanged: rethin()
    // Review N5: the grid's 95th percentile arrow, once per grid message.
    readonly property real gridP95: {
        var lengths = gridPoints.filter(g => g[2] !== null).map(g => arrowLength(g[2])).sort((a, b) => a - b);
        return lengths.length ? lengths[Math.min(lengths.length - 1, Math.floor(lengths.length * .95))] : 14;
    }
    onGridPointsChanged: rethin()
    onWidthChanged: schedulePan()
    onHeightChanged: schedulePan()
    onTempChanged: rethin()
    onWindChanged: rethin()
    Connections {
        target: stationLayer.map
        function onWorldPixelsChanged() { stationLayer.scheduleThin(); }
        function onViewCenterXChanged() { stationLayer.schedulePan(); }
        function onViewCenterYChanged() { stationLayer.schedulePan(); }
    }

    Repeater {
        model: stationLayer.shown
        delegate: Item {
            id: mark
            required property var modelData
            readonly property var s: modelData.s
            readonly property bool drawWind: stationLayer.wind && stationLayer.hasWind(s)
            readonly property bool calm: drawWind && s.windMs < 0.5
            readonly property bool drawTemp: stationLayer.temp && stationLayer.hasTemp(s)
            // Where the wind comes from, as a unit vector on screen.
            readonly property real fromX: drawWind ? Math.sin(s.windDirDeg * Math.PI / 180) : 0
            readonly property real fromY: drawWind ? -Math.cos(s.windDirDeg * Math.PI / 180) : 0
            x: stationLayer.map ? stationLayer.map.sx(modelData.mx) : 0
            y: stationLayer.map ? stationLayer.map.sy(modelData.my) : 0
            visible: x > -60 && y > -60 && x < stationLayer.width + 60 && y < stationLayer.height + 60
            // S43: stations over the grid's arrows.
            z: s.grid ? 0 : 1
            // The arrow: its tail at the station, pointing downwind.
            Image {
                readonly property int bandIndex: stationLayer.band(mark.s.windMs || 0)
                visible: mark.drawWind && !mark.calm
                source: visible ? stationLayer.arrowUrls[bandIndex] || "" : ""
                width: 16; height: stationLayer.bands[bandIndex][1] + 2
                x: -width / 2; y: -height + 1
                transformOrigin: Item.Bottom
                rotation: mark.drawWind ? (mark.s.windDirDeg + 180) % 360 : 0
                // S43: with both, the grid's arrows step back.
                opacity: mark.s.grid && stationLayer.stationsOn ? .55 : 1
                smooth: true
            }
            // S47: the speed past the arrow's head, m/s (and the gust).
            Text {
                // Past the head by the label's own half extent along the
                // wind, so a wide "10 (15)" never sits on its arrow.
                readonly property real reach: stationLayer.arrowLength(mark.s.windMs || 0) + 4
                    + Math.abs(mark.fromX) * implicitWidth / 2 + Math.abs(mark.fromY) * implicitHeight / 2
                visible: mark.drawWind && !mark.calm && (!mark.s.grid || mark.modelData.numbered === true)
                text: visible ? stationLayer.speedText(mark.s) : ""
                // Downwind of the station, where the arrow points.
                x: Math.round(-mark.fromX * reach - implicitWidth / 2)
                y: Math.round(-mark.fromY * reach - implicitHeight / 2)
                color: stationLayer.arrowInk
                style: Text.Outline
                styleColor: stationLayer.arrowHalo
                font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"
                font.pixelSize: stationLayer.compact ? 9 : 10
                font.bold: true
                opacity: mark.s.grid && stationLayer.stationsOn ? .6 : 1
            }
            // Calm: a small ring at the station.
            Rectangle {
                visible: mark.calm && !mark.drawTemp
                width: 7; height: 7; radius: 3.5
                x: -3.5; y: -3.5
                color: "transparent"
                border.width: 1.6
                border.color: stationLayer.arrowInk
            }
            // The temperature: upwind of the arrow's tail when there is an
            // arrow, else on the station.
            Rectangle {
                visible: mark.drawTemp
                readonly property real away: mark.drawWind && !mark.calm ? 14 : 0
                readonly property color fill: mark.drawTemp ? stationLayer.tempColor(mark.s.tempC) : "transparent"
                width: tempText.implicitWidth + 8
                height: tempText.implicitHeight + 2
                radius: height / 2
                x: Math.round(mark.fromX * away - width / 2)
                y: Math.round(mark.fromY * away - height / 2)
                // Review N8: the radar shows through the pill.
                color: Qt.alpha(fill, .85)
                border.width: 1
                border.color: Qt.alpha(stationLayer.light ? "#1b1d24" : "#000000", .45)
                Text {
                    id: tempText
                    anchors.centerIn: parent
                    text: mark.drawTemp ? stationLayer.degrees(mark.s.tempC) : ""
                    color: stationLayer.inkOn(parent.fill)
                    font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"
                    font.pixelSize: stationLayer.compact ? stationLayer.sizes.small : stationLayer.sizes.caption
                    font.bold: true
                }
            }
        }
    }

    // Hover: the nearest shown station within 16 px names itself.
    property var hot: null
    HoverHandler {
        id: hover
        onPointChanged: stationLayer.pick(point.position.x, point.position.y)
        onHoveredChanged: if (!hovered) stationLayer.hot = null
    }
    function pick(px, py) {
        var best = null, bestD = 16;
        for (var p of shown) {
            var d = Math.hypot(map.sx(p.mx) - px, map.sy(p.my) - py);
            if (d < bestD) { bestD = d; best = p; }
        }
        hot = best;
    }
    /// For checks: the station nearest a map point, or null.
    function stationAt(px, py) { pick(px, py); return hot ? hot.s : null; }
    function compass(deg) {
        var names = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
        return names[Math.round(((deg % 360) + 360) % 360 / 45) % 8];
    }
    // Review S3: local time, as the rest of the window shows it.
    function hhmm(iso) {
        var d = new Date(iso || "");
        return isNaN(d.getTime()) ? "" : Qt.formatTime(d, Qt.locale().timeFormat(Locale.ShortFormat));
    }
    function one(v) { return (Math.round(v * 10) / 10).toFixed(1).replace("-", "−"); }
    readonly property var providerNames: ({smhi: "SMHI", fmi: "FMI", dmi: "DMI", frost: "MET Norway"})
    function tipLines(s) {
        var lines = s.grid ? ["MET Nordic grid", "MET Norway analysis · " + hhmm(s.time)]
            : [s.name, (providerNames[s.provider] || s.provider) + " · " + hhmm(s.time)];
        if (hasTemp(s)) lines.push(one(s.tempC) + " °C");
        if (hasWind(s)) lines.push(s.windMs < 0.5 ? "calm" : "wind " + one(s.windMs) + " m/s from " + compass(s.windDirDeg) + " (" + Math.round(s.windDirDeg) + "°)");
        else if (s.windMs !== null && s.windMs !== undefined) lines.push("wind " + one(s.windMs) + " m/s");
        if (s.gustMs !== null && s.gustMs !== undefined) lines.push("gust " + one(s.gustMs) + " m/s");
        return lines;
    }
    Rectangle {
        id: tip
        visible: !!stationLayer.hot
        readonly property real px: stationLayer.hot ? stationLayer.map.sx(stationLayer.hot.mx) : 0
        readonly property real py: stationLayer.hot ? stationLayer.map.sy(stationLayer.hot.my) : 0
        x: Math.round(Math.min(stationLayer.width - width - 6, Math.max(6, px + 14)))
        y: Math.round(Math.min(stationLayer.height - height - 6, Math.max(6, py - height - 10)))
        width: tipColumn.implicitWidth + 16
        height: tipColumn.implicitHeight + 10
        color: Qt.alpha(stationLayer.theme ? stationLayer.theme.background : "#1a1b26", .96)
        border.width: 1
        border.color: stationLayer.theme ? stationLayer.theme.foreground : "#a9b1d6"
        z: 10
        Column {
            id: tipColumn
            x: 8; y: 5
            Repeater {
                model: stationLayer.hot ? stationLayer.tipLines(stationLayer.hot.s) : []
                Text {
                    required property string modelData
                    required property int index
                    text: modelData
                    color: stationLayer.theme ? stationLayer.theme.foreground : "#a9b1d6"
                    font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"
                    font.pixelSize: stationLayer.sizes.caption
                    font.bold: index === 0
                    opacity: index === 1 ? .7 : 1
                }
            }
        }
    }

    // The legend: the temperature scale and the wind key, with the credit.
    readonly property var skipped: obs && Array.isArray(obs.providers) ? obs.providers.filter(p => p.status !== "ok") : []
    // S47: loading says how far, and no stations says why (it used to say
    // "loading" for ever when every provider had failed).
    readonly property int providersDone: obs ? (obs.providers || []).filter(p => p.status !== "loading").length : 0
    readonly property string stationCredit: !stationsOn ? ""
        : !obs || (obs.status === "loading" && !stations.length) ? "Stations loading…" + (obs && obs.providers ? " (" + providersDone + " of " + obs.providers.length + " providers)" : "")
        : !stations.length ? "No stations: " + (skipped.filter(p => p.status !== "loading").map(p => p.name + " " + p.status).join(", ") || "none reported")
        : (obs && obs.attribution ? obs.attribution : "")
          + (obs.status === "loading" ? " · loading " + providersDone + " of " + (obs.providers || []).length : "")
          + (skipped.some(p => p.id === "frost" && p.status === "skipped") ? " · Norway: no stations" : "")
    // S43: the grid's credit with its hour.
    readonly property string gridCredit: !gridOn ? ""
        : !grid || grid.status === "loading" ? "Grid loading…"
        : !grid.time ? "MET Nordic grid: " + (grid.provider && grid.provider.note ? grid.provider.note : "none")
        : "MET Nordic analysis " + hhmm(grid.time) + ", " + grid.attribution
    readonly property string credit: [stationCredit, gridCredit].filter(x => x).join("\n")
    Rectangle {
        id: legendBox
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.rightMargin: 10
        anchors.bottomMargin: stationLayer.creditHeight
        width: legendColumn.implicitWidth + 16
        height: legendColumn.implicitHeight + 10
        // S46: twice the size while zoomed, grown from the corner it sits
        // in, over everything on the map; the host shrinks it again.
        transformOrigin: Item.BottomRight
        scale: stationLayer.legendZoomed ? 2 : 1
        border.width: stationLayer.legendZoomed ? .5 : 0
        border.color: stationLayer.theme ? stationLayer.theme.foreground : "#a9b1d6"
        TapHandler { onTapped: stationLayer.legendClicked() }
        // Review N4: under the stations, so it never hides one; S43: over
        // the grid's arrows, which cover the whole map.
        z: stationLayer.legendZoomed ? 20 : .5
        color: Qt.alpha(stationLayer.theme ? stationLayer.theme.background : "#1a1b26", .8)
        Column {
            id: legendColumn
            x: 8; y: 5
            spacing: 4
            // S46: the scale's name and unit, on the enlarged card.
            Text {
                visible: stationLayer.legendZoomed && stationLayer.showLegend
                text: [stationLayer.temp ? "TEMPERATURE °C" : "", stationLayer.wind ? "WIND m/s" : ""].filter(x => x).join(" · ")
                color: stationLayer.theme ? stationLayer.theme.foreground : "#a9b1d6"
                font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"
                font.pixelSize: stationLayer.sizes.small
                font.bold: true
            }
            Item {
                visible: stationLayer.temp && stationLayer.showLegend
                // S46: wide and tall enough for the ticks at any text size.
                width: Math.round(170 * Math.max(1, stationLayer.sizes.k)); height: 13 + Math.ceil(stationLayer.sizes.small * 1.3)
                // Review S3: with the grid's field on, the ramp is drawn as
                // the field is, at its opacity over the theme's background.
                Rectangle {
                    visible: !!stationLayer.underlay
                    width: parent.width; height: 8; radius: 4
                    color: stationLayer.theme ? stationLayer.theme.background : "#1a1b26"
                }
                Rectangle {
                    id: bar
                    opacity: stationLayer.underlay && stationLayer.map ? stationLayer.map.underlayOpacity : 1
                    width: parent.width; height: 8; radius: 4
                    gradient: Gradient {
                        orientation: Gradient.Horizontal
                        GradientStop { position: stationLayer.stopPosition(stationLayer.scale[0][0]); color: stationLayer.scale[0][1] }
                        GradientStop { position: stationLayer.stopPosition(stationLayer.scale[1][0]); color: stationLayer.scale[1][1] }
                        GradientStop { position: stationLayer.stopPosition(stationLayer.scale[2][0]); color: stationLayer.scale[2][1] }
                        GradientStop { position: stationLayer.stopPosition(stationLayer.scale[3][0]); color: stationLayer.scale[3][1] }
                        GradientStop { position: stationLayer.stopPosition(stationLayer.scale[4][0]); color: stationLayer.scale[4][1] }
                        GradientStop { position: stationLayer.stopPosition(stationLayer.scale[5][0]); color: stationLayer.scale[5][1] }
                        GradientStop { position: stationLayer.stopPosition(stationLayer.scale[6][0]); color: stationLayer.scale[6][1] }
                        GradientStop { position: stationLayer.stopPosition(stationLayer.scale[7][0]); color: stationLayer.scale[7][1] }
                        GradientStop { position: stationLayer.stopPosition(stationLayer.scale[8][0]); color: stationLayer.scale[8][1] }
                        GradientStop { position: stationLayer.stopPosition(stationLayer.scale[9][0]); color: stationLayer.scale[9][1] }
                    }
                }
                Repeater {
                    // −30 … 32 °C across the bar, linear.
                    model: [-20, -10, 0, 10, 20, 30]
                    Text {
                        required property int modelData
                        readonly property real at: (stationLayer.stopPosition(modelData))
                        x: Math.round(at * bar.width - implicitWidth / 2)
                        y: 11
                        text: stationLayer.degrees(modelData)
                        color: stationLayer.theme ? stationLayer.theme.foreground : "#a9b1d6"
                        font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"
                        font.pixelSize: stationLayer.sizes.small
                        opacity: .8
                    }
                }
            }
            Row {
                visible: stationLayer.wind && stationLayer.showLegend
                spacing: 6
                Text {
                    text: "→"
                    color: stationLayer.arrowInk
                    font.pixelSize: stationLayer.sizes.body
                    font.bold: true
                }
                Text {
                    text: "wind blows to · 7 (12) = m/s (gust) · ○ calm"
                    color: stationLayer.theme ? stationLayer.theme.foreground : "#a9b1d6"
                    font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"
                    font.pixelSize: stationLayer.sizes.small
                    opacity: .8
                    anchors.verticalCenter: parent.verticalCenter
                }
            }
            // S47: the speed bands, an arrow of each length over its m/s.
            Row {
                id: bandKey
                visible: stationLayer.wind && stationLayer.showLegend
                spacing: 2
                Repeater {
                    model: stationLayer.bands.length
                    Column {
                        required property int index
                        width: 26
                        spacing: 1
                        Item {
                            width: parent.width
                            height: stationLayer.bands[stationLayer.bands.length - 1][1] * .6 + 2
                            Image {
                                source: stationLayer.arrowUrls[index] || ""
                                width: 16 * .6
                                height: (stationLayer.bands[index][1] + 2) * .6
                                anchors.horizontalCenter: parent.horizontalCenter
                                anchors.bottom: parent.bottom
                                smooth: true
                            }
                        }
                        Text {
                            width: parent.width
                            horizontalAlignment: Text.AlignHCenter
                            text: stationLayer.bandName(index)
                            color: stationLayer.theme ? stationLayer.theme.foreground : "#a9b1d6"
                            font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"
                            font.pixelSize: 8
                            opacity: .8
                        }
                    }
                }
                Text {
                    text: "m/s"
                    anchors.bottom: parent.bottom
                    color: stationLayer.theme ? stationLayer.theme.foreground : "#a9b1d6"
                    font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"
                    font.pixelSize: 8
                    opacity: .8
                }
            }
            Text {
                text: stationLayer.credit
                color: stationLayer.theme ? stationLayer.theme.foreground : "#a9b1d6"
                font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"
                font.pixelSize: stationLayer.sizes.small
                opacity: .6
            }
        }
    }
    // Where a temperature sits on the legend bar: linear over the scale.
    function stopPosition(c) {
        var lo = scale[0][0], hi = scale[scale.length - 1][0];
        return Math.max(0, Math.min(1, (c - lo) / (hi - lo)));
    }
}
