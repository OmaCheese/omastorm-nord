import QtQuick
import Quickshell.Io

// Lightning strikes over the map (S44, docs/protocol.md "Lightning"): the
// NORDLIS network's located pulses from the engine's packed strikes file
// (tex/lightning-<hash>.bin, 16 bytes a strike), drawn by ONE Canvas, never
// a Repeater (a busy 15 minutes is ~2,000 strikes). Cloud-to-ground is a
// bold cross, in-cloud a small dot; both fade from new to old over the
// trail (30 minutes), from white-hot to deep orange.
//
// Which strikes: with the newest frame shown live, those of the last trail
// before now, faded by their age now (a tick moves the fade on); while
// looping or at an older frame, those of the trail before the frame's
// scanTime plus its 5 minutes, faded by their age then.
//
// Pans move the painted canvas (it is painted with a margin around the
// view) and repaint only past the margin or when the zoom changes, so a
// drag does not redraw thousands of marks every frame.
Item {
    id: root
    property var map
    /// The engine's `lightning` message, or null.
    property var message: null
    /// The runtime directory the message's path is under (Engine.runtime).
    property string runtime: ""
    property bool on: false
    property var theme
    property bool compact: false
    /// S46: the theme's type scale (the window's; a fallback for a bare host).
    readonly property var sizes: theme && theme.size ? theme.size : ({small: 10, caption: 11, body: 12, label: 13, title: 14, k: 1})
    /// The frame on screen (for its scanTime) and whether it is the live one.
    property var frame: null
    property bool live: true
    /// A fixed "now" for checks and captures (ms), 0 = the clock.
    property real clockMs: 0
    visible: on

    readonly property bool light: !!(theme && theme.light)
    readonly property real trailMs: (message && message.trailS > 0 ? message.trailS : 1800) * 1000
    readonly property real frameMs: 5 * 60 * 1000
    property real nowMs: clockMs > 0 ? clockMs : Date.now()
    Timer {
        // The fade moves on while live; a minute's step is invisible at 30.
        interval: 15000
        repeat: true
        running: root.on && root.live && root.clockMs <= 0
        onTriggered: root.nowMs = Date.now()
    }
    onClockMsChanged: nowMs = clockMs > 0 ? clockMs : Date.now()
    /// The time the fade is measured from.
    readonly property real refMs: {
        if (live || !frame || !frame.scanTime) return nowMs;
        var t = Date.parse(frame.scanTime);
        return isFinite(t) ? t + frameMs : nowMs;
    }

    // ---- the strikes, parsed once per file ---------------------------------
    property var times: null        // Float64Array, ms, oldest first
    property var mxs: null          // Float64Array, Web Mercator 0..1
    property var mys: null
    property var clouds: null       // Uint8Array, 1 = in-cloud
    property int count: 0
    // Review SF4: two files, `path` (the whole ring, rewritten at most every
    // 5 minutes) and `recent` (from `recentFrom` on, every minute); drawn
    // merged: `path`'s strikes before recentFrom, then all of `recent`.
    readonly property string path: on && message && typeof message.path === "string" ? message.path : ""
    readonly property string recentPath: on && message && typeof message.recent === "string" ? message.recent : ""
    readonly property real recentFrom: {
        var t = message && message.recentFrom ? Date.parse(message.recentFrom) : NaN;
        return isFinite(t) ? t : -Infinity;
    }
    property var full: null
    property var recent: null
    function decode(buffer) {
        if (!buffer || !buffer.byteLength || buffer.byteLength < 16) return null;
        var view = new DataView(buffer);
        if (view.getUint8(0) !== 0x4f || view.getUint8(1) !== 0x53 || view.getUint8(2) !== 0x4c || view.getUint8(3) !== 0x31) return null;
        var n = view.getUint32(4, true);
        if (buffer.byteLength !== 16 + 16 * n) return null;
        var base = view.getFloat64(8, true);
        var t = new Float64Array(n), x = new Float64Array(n), y = new Float64Array(n), c = new Uint8Array(n);
        for (var i = 0, o = 16; i < n; i++, o += 16) {
            t[i] = base + view.getUint32(o, true);
            var lat = view.getFloat32(o + 4, true), lon = view.getFloat32(o + 8, true);
            x[i] = (lon + 180) / 360;
            var s = Math.sin(Math.max(-85.05, Math.min(85.05, lat)) * Math.PI / 180);
            y[i] = 0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI);
            c[i] = view.getUint8(o + 14) & 1;
        }
        return { t: t, x: x, y: y, c: c, n: n };
    }
    /// The drawn arrays from the two files.
    function merge() {
        var f = path !== "" ? full : null, r = recentPath !== "" ? recent : null;
        var keep = 0;
        if (f) { keep = f.n; if (r) while (keep > 0 && f.t[keep - 1] >= recentFrom) keep--; }
        var rn = r ? r.n : 0, n = keep + rn;
        var t = new Float64Array(n), x = new Float64Array(n), y = new Float64Array(n), c = new Uint8Array(n);
        if (keep) { t.set(f.t.subarray(0, keep)); x.set(f.x.subarray(0, keep)); y.set(f.y.subarray(0, keep)); c.set(f.c.subarray(0, keep)); }
        if (rn) { t.set(r.t, keep); x.set(r.x, keep); y.set(r.y, keep); c.set(r.c, keep); }
        times = t; mxs = x; mys = y; clouds = c; count = n;
        canvas.requestPaint();
    }
    onPathChanged: if (path === "") { full = null; merge(); }
    onRecentPathChanged: if (recentPath === "") { recent = null; merge(); }
    onRecentFromChanged: merge()
    FileView {
        id: fullFile
        path: root.path !== "" ? root.runtime + root.path : ""
        printErrors: false
        onLoaded: { root.full = root.decode(fullFile.data()); root.merge(); }
    }
    FileView {
        id: recentFile
        path: root.recentPath !== "" ? root.runtime + root.recentPath : ""
        printErrors: false
        onLoaded: { root.recent = root.decode(recentFile.data()); root.merge(); }
    }
    /// The first index with time > `ms` (times sorted).
    function upper(ms) {
        var lo = 0, hi = count;
        while (lo < hi) { var mid = (lo + hi) >> 1; if (times[mid] > ms) hi = mid; else lo = mid + 1; }
        return lo;
    }
    /// Strikes in the trail at the reference time: [ground, cloud].
    readonly property var shownCounts: {
        if (!count) return [0, 0];
        var hi = upper(refMs), lo = upper(refMs - trailMs), g = 0;
        for (var i = lo; i < hi; i++) if (!clouds[i]) g++;
        return [g, hi - lo - g];
    }

    // ---- drawing -------------------------------------------------------------
    // Age buckets, new to old: ink and opacity.
    readonly property var inks: ["#ffffff", "#fff27a", "#ffd23f", "#ffab2e", "#ff8424", "#f0602a"]
    readonly property var alphas: [1, .92, .8, .66, .52, .38]
    readonly property color halo: light ? Qt.rgba(0.08, 0.09, 0.12, .85) : Qt.rgba(0, 0, 0, .8)
    readonly property real margin: 160
    property real paintedX: 0.5
    property real paintedY: 0.5
    property real paintedWorld: 0
    readonly property real viewX: map ? map.viewCenterX : 0.5
    readonly property real viewY: map ? map.viewCenterY : 0.5
    readonly property real world: map ? map.worldPixels : 0
    readonly property real shiftX: paintedWorld > 0 ? (paintedX - viewX) * world : 0
    readonly property real shiftY: paintedWorld > 0 ? (paintedY - viewY) * world : 0
    onWorldChanged: if (on) zoomPaint.restart()
    onShiftXChanged: if (on && Math.abs(shiftX) > margin * .8) canvas.requestPaint()
    onShiftYChanged: if (on && Math.abs(shiftY) > margin * .8) canvas.requestPaint()
    onRefMsChanged: if (on) canvas.requestPaint()
    onOnChanged: canvas.requestPaint()
    onLightChanged: canvas.requestPaint()
    onWidthChanged: canvas.requestPaint()
    onHeightChanged: canvas.requestPaint()
    Timer { id: zoomPaint; interval: 40; onTriggered: canvas.requestPaint() }
    /// Paints since start, for checks.
    property int paints: 0
    property int drawn: 0
    property int paintMs: 0

    Canvas {
        id: canvas
        x: -root.margin + root.shiftX
        y: -root.margin + root.shiftY
        width: root.width + 2 * root.margin
        height: root.height + 2 * root.margin
        // A zoom between paints: scale the old picture about the view's
        // centre until the debounced paint lands.
        transformOrigin: Item.Center
        scale: root.paintedWorld > 0 && root.world > 0 ? root.world / root.paintedWorld : 1
        onPaint: {
            var started = Date.now();
            var ctx = getContext("2d");
            ctx.reset();
            if (!root.on || !root.map || !root.count || root.world <= 0) {
                root.paintedWorld = root.world;
                root.paintedX = root.viewX;
                root.paintedY = root.viewY;
                root.drawn = 0;
                return;
            }
            var world = root.world, cx = root.viewX, cy = root.viewY;
            var ox = width / 2, oy = height / 2;
            var ref = root.refMs, trail = root.trailMs;
            var hi = root.upper(ref), lo = root.upper(ref - trail);
            var nb = root.inks.length;
            // One path per bucket and kind: 2 × 6 strokes and fills in all.
            var dots = [], crosses = [];
            for (var b = 0; b < nb; b++) { dots.push([]); crosses.push([]); }
            var t = root.times, xs = root.mxs, ys = root.mys, cl = root.clouds;
            var drawn = 0;
            for (var i = lo; i < hi; i++) {
                var px = ox + (xs[i] - cx) * world, py = oy + (ys[i] - cy) * world;
                if (px < -8 || py < -8 || px > width + 8 || py > height + 8) continue;
                var bucket = Math.min(nb - 1, Math.floor((ref - t[i]) / trail * nb));
                (cl[i] ? dots : crosses)[bucket].push(px, py);
                drawn++;
            }
            var r = root.compact ? 1.5 : 1.9;
            var arm = root.compact ? 3.5 : 4.5;
            // Oldest first, so the newest sit on top.
            for (b = nb - 1; b >= 0; b--) {
                var d = dots[b], c = crosses[b], k;
                ctx.globalAlpha = root.alphas[b];
                if (d.length) {
                    ctx.fillStyle = root.halo;
                    ctx.beginPath();
                    for (k = 0; k < d.length; k += 2) { ctx.moveTo(d[k] + r + 1, d[k + 1]); ctx.arc(d[k], d[k + 1], r + 1, 0, 2 * Math.PI); }
                    ctx.fill();
                    ctx.fillStyle = root.inks[b];
                    ctx.beginPath();
                    for (k = 0; k < d.length; k += 2) { ctx.moveTo(d[k] + r, d[k + 1]); ctx.arc(d[k], d[k + 1], r, 0, 2 * Math.PI); }
                    ctx.fill();
                }
                if (c.length) {
                    ctx.lineCap = "round";
                    ctx.beginPath();
                    for (k = 0; k < c.length; k += 2) {
                        ctx.moveTo(c[k] - arm, c[k + 1]); ctx.lineTo(c[k] + arm, c[k + 1]);
                        ctx.moveTo(c[k], c[k + 1] - arm); ctx.lineTo(c[k], c[k + 1] + arm);
                    }
                    ctx.strokeStyle = root.halo;
                    ctx.lineWidth = root.compact ? 3.6 : 4.4;
                    ctx.stroke();
                    ctx.strokeStyle = root.inks[b];
                    ctx.lineWidth = root.compact ? 1.7 : 2.2;
                    ctx.stroke();
                }
            }
            ctx.globalAlpha = 1;
            root.paintedWorld = world;
            root.paintedX = cx;
            root.paintedY = cy;
            root.drawn = drawn;
            root.paints++;
            root.paintMs = Date.now() - started;
        }
    }

    // ---- the key ---------------------------------------------------------------
    /// The key's words: honest about what NORDLIS sees.
    readonly property string credit: message && message.attribution ? message.attribution : "FMI NORDLIS, CC BY 4.0"
    property bool showKey: true
    /// The credit in the key (the window's compact header has no room).
    property bool creditInKey: false
    /// Where the key sits: top left, below the map's north mark.
    property real keyTop: 34
    /// S46: a click on the key asks the host to enlarge it (legend zoom);
    /// the host says whether it is enlarged.
    property bool keyZoomed: false
    signal keyClicked()
    /// The key's box in this layer, for the host's wheel guard.
    readonly property Item keyItem: key
    Rectangle {
        id: key
        visible: root.showKey && root.on
        // Twice the size while zoomed, grown from its top-left corner.
        transformOrigin: Item.TopLeft
        scale: root.keyZoomed ? 2 : 1
        z: root.keyZoomed ? 20 : 0
        border.width: root.keyZoomed ? .5 : 0
        border.color: root.theme ? root.theme.foreground : "#a9b1d6"
        MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: root.keyClicked() }
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.leftMargin: 10
        anchors.topMargin: root.keyTop
        width: keyRow.implicitWidth + 12
        height: keyRow.implicitHeight + 8
        radius: 3
        color: Qt.alpha(root.theme ? root.theme.background : "#1a1b26", .8)
        Row {
            id: keyRow
            x: 6; y: 4
            spacing: 8
            Text {
                text: "NORDLIS STRIKES"
                color: root.theme ? root.theme.foreground : "#a9b1d6"
                font.family: root.theme ? root.theme.font : "monospace"
                font.pixelSize: root.sizes.small
                opacity: .75
                anchors.verticalCenter: parent.verticalCenter
            }
            // The two marks as they are drawn.
            Canvas {
                width: 12; height: 12
                anchors.verticalCenter: parent.verticalCenter
                onPaint: {
                    var ctx = getContext("2d"); ctx.reset();
                    ctx.lineCap = "round";
                    ctx.beginPath(); ctx.moveTo(1.5, 6); ctx.lineTo(10.5, 6); ctx.moveTo(6, 1.5); ctx.lineTo(6, 10.5);
                    ctx.strokeStyle = root.halo; ctx.lineWidth = 4.4; ctx.stroke();
                    ctx.strokeStyle = root.inks[1]; ctx.lineWidth = 2.2; ctx.stroke();
                }
            }
            Text {
                text: "ground " + root.shownCounts[0]
                color: root.theme ? root.theme.foreground : "#a9b1d6"
                font.family: root.theme ? root.theme.font : "monospace"
                font.pixelSize: root.sizes.small
                anchors.verticalCenter: parent.verticalCenter
            }
            Canvas {
                width: 8; height: 12
                anchors.verticalCenter: parent.verticalCenter
                onPaint: {
                    var ctx = getContext("2d"); ctx.reset();
                    ctx.fillStyle = root.halo; ctx.beginPath(); ctx.arc(4, 6, 2.9, 0, 2 * Math.PI); ctx.fill();
                    ctx.fillStyle = root.inks[1]; ctx.beginPath(); ctx.arc(4, 6, 1.9, 0, 2 * Math.PI); ctx.fill();
                }
            }
            Text {
                text: "cloud " + root.shownCounts[1]
                color: root.theme ? root.theme.foreground : "#a9b1d6"
                font.family: root.theme ? root.theme.font : "monospace"
                font.pixelSize: root.sizes.small
                anchors.verticalCenter: parent.verticalCenter
            }
            // New to old over the trail.
            Row {
                spacing: 0
                anchors.verticalCenter: parent.verticalCenter
                Repeater {
                    model: root.inks.length
                    Rectangle {
                        required property int index
                        width: 6; height: 6
                        color: root.inks[index]
                        opacity: root.alphas[index]
                    }
                }
            }
            Text {
                text: Math.round(root.trailMs / 60000) + " min" + (root.creditInKey ? " · " + root.credit : "")
                color: root.theme ? root.theme.foreground : "#a9b1d6"
                font.family: root.theme ? root.theme.font : "monospace"
                font.pixelSize: root.sizes.small
                opacity: .75
                anchors.verticalCenter: parent.verticalCenter
            }
        }
    }
}
