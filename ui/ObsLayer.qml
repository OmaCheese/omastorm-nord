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
Item {
    id: stationLayer
    property var map
    property var obs: null
    property bool temp: false
    property bool wind: false
    property var theme
    property bool compact: false
    /// The scale and the wind key; off, only the credit line shows.
    property bool showLegend: true
    /// Room kept free at the bottom right for the map credit.
    property real creditHeight: 30
    readonly property bool light: !!(theme && theme.light)
    readonly property var stations: obs && Array.isArray(obs.stations) ? obs.stations : []
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
        return temp ? p95 + 34 : p95 + 10;
    }
    function hash(text) {
        var h = 2166136261;
        for (var i = 0; i < text.length; i++) { h ^= text.charCodeAt(i); h = Math.imul(h, 16777619) >>> 0; }
        return h;
    }
    function useful(s) { return (temp && hasTemp(s)) || (wind && hasWind(s)); }
    function rethin() {
        if (!map || !on) { if (shown.length) shown = []; return; }
        var world = map.worldPixels, grid = {}, out = [];
        thinnedAt = world;
        thinCount++;
        var list = stations.filter(useful).map(s => ({
            s: s, mx: map.mercatorX(s.lon), my: map.mercatorY(s.lat),
            rank: (hasTemp(s) && hasWind(s) ? 0 : 1) * 4294967296 + hash(s.id)
        }));
        var cell = cellFor(list);
        list.sort((a, b) => a.rank - b.rank);
        for (var p of list) {
            var x = p.mx * world, y = p.my * world;
            var gx = Math.floor(x / cell), gy = Math.floor(y / cell), clear = true;
            for (var dx = -1; dx <= 1 && clear; dx++)
                for (var dy = -1; dy <= 1 && clear; dy++)
                    for (var q of grid[(gx + dx) + ":" + (gy + dy)] || [])
                        if (Math.hypot(q.x - x, q.y - y) < cell) { clear = false; break; }
            if (!clear) continue;
            var key = gx + ":" + gy;
            (grid[key] = grid[key] || []).push({x: x, y: y});
            out.push(p);
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
    onStationsChanged: rethin()
    onTempChanged: rethin()
    onWindChanged: rethin()
    Connections {
        target: stationLayer.map
        function onWorldPixelsChanged() { stationLayer.scheduleThin(); }
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
            // The arrow: its tail at the station, pointing downwind.
            Image {
                readonly property int bandIndex: stationLayer.band(mark.s.windMs || 0)
                visible: mark.drawWind && !mark.calm
                source: visible ? stationLayer.arrowUrls[bandIndex] || "" : ""
                width: 16; height: stationLayer.bands[bandIndex][1] + 2
                x: -width / 2; y: -height + 1
                transformOrigin: Item.Bottom
                rotation: mark.drawWind ? (mark.s.windDirDeg + 180) % 360 : 0
                smooth: true
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
                    font.pixelSize: stationLayer.compact ? 10 : 11
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
        var lines = [s.name, (providerNames[s.provider] || s.provider) + " · " + hhmm(s.time)];
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
                    font.pixelSize: 11
                    font.bold: index === 0
                    opacity: index === 1 ? .7 : 1
                }
            }
        }
    }

    // The legend: the temperature scale and the wind key, with the credit.
    readonly property var skipped: obs && Array.isArray(obs.providers) ? obs.providers.filter(p => p.status !== "ok") : []
    readonly property string credit: (obs && obs.attribution ? obs.attribution : "")
        + (skipped.some(p => p.id === "frost" && p.status === "skipped") ? " · Norway: no stations" : "")
    Rectangle {
        id: legendBox
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.rightMargin: 10
        anchors.bottomMargin: stationLayer.creditHeight
        width: legendColumn.implicitWidth + 16
        height: legendColumn.implicitHeight + 10
        // Review N4: under the stations, so it never hides one.
        z: -1
        color: Qt.alpha(stationLayer.theme ? stationLayer.theme.background : "#1a1b26", .8)
        Column {
            id: legendColumn
            x: 8; y: 5
            spacing: 4
            Item {
                visible: stationLayer.temp && stationLayer.showLegend
                width: 170; height: 24
                Rectangle {
                    id: bar
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
                        font.pixelSize: 9
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
                    font.pixelSize: 12
                    font.bold: true
                }
                Text {
                    text: "wind blows to · longer = stronger · ○ calm"
                    color: stationLayer.theme ? stationLayer.theme.foreground : "#a9b1d6"
                    font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"
                    font.pixelSize: 9
                    opacity: .8
                    anchors.verticalCenter: parent.verticalCenter
                }
            }
            Text {
                text: stationLayer.stations.length ? stationLayer.credit : "Stations loading…"
                color: stationLayer.theme ? stationLayer.theme.foreground : "#a9b1d6"
                font.family: stationLayer.theme ? stationLayer.theme.font : "monospace"
                font.pixelSize: 9
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
