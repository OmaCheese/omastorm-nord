import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import "Sites.js" as Sites
import "Keys.js" as KeyMap
import "Location.js" as Location
import "Mosaic.js" as Mosaic

Item {
    id: app
    // A standalone launcher owns its process; a plugin never does.
    property var session: null
    property var shell: null
    property var manifest: null
    readonly property var store: PluginSession
    property bool opened: session === null
    // A standalone window has no plugin session but reads the PluginSession
    // singleton, whose settle timer puts a composite's product back on REF
    // while no surface is open (S24b S3): this window is one (fixed by S24d).
    Binding { when: app.session === null; target: app.store; property: "windowOpen"; value: app.opened }
    function open(payload) {
        opened = true;
        if (session) session.windowOpen = true;
        applyView();
        if (store.pendingLocationPicker) Qt.callLater(() => locationPicker.show(""));
        else maybeOfferLocation();
    }
    function close() {
        if (!opened) return;
        opened = false;
        store.persist();
        if (session) session.windowOpen = false;
    }
    function dismiss() {
        if (!session) Qt.quit();
        else if (shell) shell.hide("rb.omastorm-se");
        else close();
    }
    readonly property var state: engine.state
    // The frame on screen: the engine's, or one from this window's own loop
    // buffer while it plays or waits on its seek (Engine.qml).
    readonly property var scan: engine.frame
    // Every station, product, and source string on screen comes from the engine.
    readonly property string siteId: state ? state.site.id : ""
    readonly property string siteName: engine.site ? engine.site.name.toUpperCase() : ""
    // True when the id is only the name folded to ASCII (every SMHI key and
    // region word: vara, sweden); an ODIM node code (nohur) is not.
    readonly property bool idIsName: !!engine.site && !!siteId && Sites.fold(engine.site.name) === siteId
    // The credit for the frame on screen, verbatim (docs/protocol.md,
    // frame.attribution); the station's own before a frame names one.
    readonly property string attribution: scan && scan.attribution ? scan.attribution
        : engine.site && engine.site.attribution ? engine.site.attribution : ""
    readonly property string sourceBadge: state ? state.source.toUpperCase() : ""
    // The timeline (DESIGN.md): the station's frames oldest first with the
    // sweep in progress last. The engine owns the paused position; a loop
    // plays from this window's buffer (Engine.qml).
    readonly property var frames: state ? state.timeline : []
    readonly property int frameIndex: scan ? frames.findIndex(f => f.id === scan.id) : -1
    readonly property bool playing: engine.playing
    readonly property var newestComplete: { var done = frames.filter(f => f.status === "complete"); return done.length ? done[done.length - 1] : null; }
    // The connection condition while live (DESIGN.md):
    // LIVE / ARCHIVED is the badge; a light beside it carries health.
    // Age under the product line is how stale the frame on screen is.
    // Prose is reserved for rejections, config mistakes, and notices.
    readonly property string condition: state && state.source === "live" ? state.connection.status : ""
    readonly property bool alert: condition !== "" && condition !== "ok"
    readonly property bool scanning: !!scan && scan.status === "partial" && !!scan.scanTime
    readonly property color conditionColor: condition === "stale" ? theme.yellow
        : condition === "loading" ? theme.accent
        : condition === "unavailable" || condition === "offline" ? theme.red : theme.foreground
    // Light beside LIVE: accent when healthy (or archived), yellow stale,
    // red when the feed is down; pulses while loading or a sweep is painting.
    readonly property color statusLightColor: {
        if (!state) return theme.foreground;
        if (state.source === "archived") return theme.accent;
        if (condition === "stale") return theme.yellow;
        if (condition === "unavailable" || condition === "offline") return theme.red;
        return theme.accent;
    }
    readonly property bool statusLightPulse: condition === "loading" || (condition === "ok" && scanning)
    // Age of the frame on screen: newest complete age, plus how far the
    // playhead sits behind that sweep. No local clock.
    readonly property int shownAge: !state || !scan || !scan.scanTime || !newestComplete ? -1
        : Math.max(0, state.connection.ageSeconds + Math.round((Date.parse(newestComplete.scanTime) - Date.parse(scan.scanTime)) / 1000))
    readonly property string ageText: condition && shownAge >= 0 ? ago(shownAge) : ""
    function ago(seconds) {
        var m = Math.floor(seconds / 60);
        if (m < 1) return "Now";
        if (m < 60) return m + " min ago";
        var h = Math.floor(m / 60);
        return h < 24 ? h + "h " + (m % 60) + "m ago" : Math.floor(h / 24) + "d " + (h % 24) + "h ago";
    }
    // Stamp above the tick strip: locale picks date order and 12/24h only.
    readonly property bool stamp12h: {
        var fmt = Qt.locale().timeFormat(Locale.ShortFormat);
        return fmt.indexOf("A") >= 0 || fmt.indexOf("a") >= 0;
    }
    readonly property string stampDateOrder: {
        var fmt = Qt.locale().dateFormat(Locale.ShortFormat);
        var y = fmt.indexOf("y"), m = fmt.indexOf("M"), d = fmt.indexOf("d");
        if (y >= 0 && (m < 0 || y < m) && (d < 0 || y < d)) return "ymd";
        if (d >= 0 && m >= 0 && d < m) return "dmy";
        return "mdy";
    }
    function pad2(n) { return (n < 10 ? "0" : "") + n; }
    function stamp(iso) {
        if (!iso) return "";
        var d = new Date(iso), y = d.getFullYear(), mo = d.getMonth() + 1, day = d.getDate();
        var dateStr = stampDateOrder === "ymd" ? y + "-" + pad2(mo) + "-" + pad2(day)
            : stampDateOrder === "dmy" ? pad2(day) + "/" + pad2(mo) + "/" + String(y).slice(2)
            : pad2(mo) + "/" + pad2(day) + "/" + String(y).slice(2);
        var timeStr = stamp12h ? Qt.formatTime(d, "h:mm AP") : Qt.formatTime(d, "HH:mm");
        return dateStr + " · " + timeStr + " " + Qt.formatTime(d, "t");
    }
    // History fills 60 positions from the left when the strip is wide enough.
    // Compact widths drop the empty pads — at ~5 px/slot they read as a
    // dotted cliff after the playhead instead of "room to fill."
    readonly property var slots: {
        var result = [];
        for (var j = 0; j < frames.length; j++)
            result.push({id: frames[j].id, partial: frames[j].status === "partial", empty: false});
        if (!win.compact) {
            for (var i = frames.length; i < 60; i++) result.push({empty: true, partial: false});
        }
        return result;
    }
    readonly property int currentSlot: scan ? slots.findIndex(s => !s.empty && s.id === scan.id) : -1
    // Play loops the buffered frames here (Engine.qml, loop buffer); pause,
    // step and jumps show the frame at once and seek, so others follow.
    function togglePlay() { if (frames.length > 1) engine.togglePlay(); }
    function step(delta) { if (frames.length > 1) engine.stepBy(delta); }
    function jump(toNewest) { if (frames.length > 1) engine.seekTo(frames[toNewest ? frames.length - 1 : 0].id); }
    readonly property int bands: scan ? scan.palette.length : 0
    function legendLabel(index) {
        var bounds = scan.bounds;
        return index === 0 ? "<" + bounds[1] : index === bands - 1 ? bounds[index] + "+" : String(bounds[index]);
    }
    // Where the weak-return floor cuts the legend strip, as a fraction of its
    // width: the bands are equal columns, so the floor interpolates inside
    // the band it falls in. 0 when the floor is off, under the scale, or not
    // in force because the frame carries no scale (an older engine).
    // The floor is in dBZ (S24a): a Storm height or Rain mass legend hides
    // nothing.
    readonly property bool floorActive: !!scan && weakFloor !== null && scan.scale > 0 && scan.units === "dBZ"
    readonly property real floorFraction: {
        if (!floorActive || weakFloor <= scan.bounds[0]) return 0;
        for (var i = 0; i < bands; i++)
            if (weakFloor < scan.bounds[i + 1]) return (i + (weakFloor - scan.bounds[i]) / (scan.bounds[i + 1] - scan.bounds[i])) / bands;
        return 1;
    }
    // Which legend numbers fit: the last always shows; each earlier one shows
    // only if it clears the previous shown number and the last one. This keeps
    // spacing even at any width and band count instead of hiding by parity.
    TextMetrics { id: legendMetrics; font.family: app.theme.font; font.pixelSize: app.theme.size.small; text: "0" }
    readonly property var legendShown: legendFit(legendRow.width, legendMetrics.advanceWidth, 8)
    readonly property var legendZoomShown: legendZoom === "radar" ? legendFit(zoomRow.width, zoomMetrics.advanceWidth, 16) : []
    // S46: the same fit for any strip width and digit width (the enlarged
    // legend's too).
    function legendFit(width, advance, gap) {
        var n = bands, shown = [];
        if (!n) return shown;
        var column = width / n, last = (n - 1) * column, cursor = 0;
        for (var i = 0; i < n - 1; i++) {
            var x = i * column, right = x + legendLabel(i).length * advance + gap;
            shown[i] = x >= cursor && right <= last;
            if (shown[i]) cursor = right;
        }
        shown[n - 1] = true;
        return shown;
    }
    // Buffers while the window is open; play and pause are shared with the
    // popover through the session (Engine.qml, loop buffer).
    Engine {
        id: engine
        active: app.opened
        loopShared: true
        loopSite: app.store.loopSite
        onLoopRequested: site => app.store.loopSite = site
        // S42: the station layers this window shows, only while it is open
        // (the engine fetches nothing while no client shows one).
        layers: ({temp: app.opened && app.store.layers.temp, wind: app.opened && app.store.layers.wind, source: app.store.layers.source,
                  lightning: app.opened && app.store.layers.lightning})
    }
    // Deliberate preferences and remembered view (DESIGN.md, location).
    // PluginSession owns config.toml, state.json, and the camera; this
    // window applies the view to its map and sends map-local tile requests.
    readonly property var config: store.config
    property bool applyingView: false
    Timer { id: applyingViewClear; interval: 250; onTriggered: app.applyingView = false }
    function applyView() {
        if (!store.hasView) return;
        applyingView = true;
        applyingViewClear.restart();
        map.holdSpan = true;
        map.lookAt(store.centerLat, store.centerLon);
        map.span = store.span;
        if (engine.state)
            engine.send({type: "view_center", lat: store.centerLat, lon: store.centerLon});
        Qt.callLater(() => { map.holdSpan = false; });
    }
    property bool viewApplied: false
    onStateChanged: {
        if (!state) viewApplied = false;
        else {
            store.initialize();
            if (!viewApplied) { applyView(); viewApplied = true; }
            maybeOfferLocation();
        }
    }
    function maybeOfferLocation() {
        if (!opened || !store.initialized || !store.needsLocation || store.locating || locationPicker.open) return;
        Qt.callLater(() => {
            if (app.opened && app.store.initialized && app.store.needsLocation && !app.store.locating && !locationPicker.open) locationPicker.show("", true);
        });
    }
    Connections {
        target: store
        function onViewChanged() {
            if (locationPicker.open && locationPicker.onboarding && !app.store.needsLocation) locationPicker.close();
            if (app.opened) app.applyView();
            app.maybeOfferLocation();
        }
        function onLocationPickerRequested() { if (app.opened) locationPicker.show(""); }
    }
    Connections {
        target: config
        function onReadyChanged() { app.applyView(); }
        function onKeysChanged() { app.applySettings(); }
        function onTreatmentChanged() { app.applySettings(); }
        function onWeakFloorChanged() { app.applySettings(); }
        function onValuesChanged() { app.applySettings(); }
    }
    // The keyboard map (DESIGN.md, keyboard map as built): Keys.js lays the
    // `[keys]` table over the defaults, asking Qt whether each sequence
    // parses; a bad value or a key bound twice keeps the default and is
    // named in the status slot, like a rejection, until the file is fixed.
    // The treatment and weak_floor settings are judged the same way.
    // OMASTORM_STYLE and OMASTORM_WEAK, set by the capture scripts, outrank
    // the file.
    property var bindings: ({})
    property var configErrors: []
    readonly property string configError: !configErrors.length ? "" : configErrors[0].toUpperCase() + (configErrors.length > 1 ? " (+" + (configErrors.length - 1) + " MORE)" : "")
    Shortcut { id: probe; enabled: false }
    function canon(sequence) { probe.sequence = sequence; return probe.portableText; }
    function applySettings() {
        var errors = Location.configErrors(config.values);
        var wanted = KeyMap.treatment(config.treatment, errors), floor = KeyMap.weakFloor(config.weakFloor, errors);
        var resolved = KeyMap.resolve(config.keys, canon);
        bindings = resolved.bindings;
        configErrors = errors.concat(resolved.errors);
        if (!session && !Quickshell.env("OMASTORM_STYLE") && wanted) treatment = wanted;
        if (!session && KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) === undefined) weakFloor = floor;
    }
    Component.onCompleted: applySettings()
    readonly property bool overlayOpen: picker.open || locationPicker.open || sheet.open || mosaicPicker.open || mapMenu.opened || legendZoom !== ""
    // S46: legend zoom. Which legend is enlarged: "radar" (the colour scale
    // under the map), "obs" (temperature and wind), "lightning" (the
    // strikes' key), "" none. A click on a legend enlarges it; a click
    // anywhere (on it again or outside) or Escape shrinks it. Not
    // remembered.
    property string legendZoom: ""
    // S46: the wheel over an overlay never zooms the map (the human
    // 2026-09-23: "scrolling above this box still zooms the map"). One
    // test for every overlay, asked by the map's own wheel handler: while
    // a scrimmed overlay is open (pickers, sheet, LAYERS, menus, an
    // enlarged legend) the scrim covers the map and no wheel zooms; else a
    // wheel over a floating card or key on the map (below) is the card's.
    readonly property bool mapCovered: picker.open || locationPicker.open || sheet.open || layersPanel.opened
        || treatmentMenu.opened || productMenu.opened || mapMenu.opened || legendZoom !== ""
        || (mosaicPicker.open && !mosaicPicker.docked)
    function wheelSinks() {
        return [helpChip, followChip, scaleBar, ringCaptionBox, loadingCard, mapCredit, northMark,
                obsLayer.legendItem, obsLayer.tipItem, lightningLayer.keyItem, mosaicPicker];
    }
    function wheelBlockedAt(mx, my) {
        if (mapCovered) return true;
        for (var it of wheelSinks()) {
            if (!it || !it.visible || it.width <= 0 || it.height <= 0) continue;
            var p = map.mapToItem(it, mx, my);   // through any scale (an enlarged legend)
            if (p.x >= 0 && p.y >= 0 && p.x < it.width && p.y < it.height) return true;
        }
        return false;
    }
    // S46: the right-click menu's regions: every composite the picker lists
    // (Sweden, Nordic, Iberia), My mosaic, the single radars shown lately
    // (this window's, newest first, not remembered), and More… (the picker).
    property var recentSites: []
    onSiteIdChanged: {
        var s = engine.sites.find(x => x.id === siteId);
        if (!s || s.kind === "grid" || s.provider === "mosaic") return;
        recentSites = [siteId].concat(recentSites.filter(id => id !== siteId)).slice(0, 4);
    }
    // Review S1: what the menu reads from the engine's state, as strings
    // and numbers that change only when their value does, so the stream of
    // state messages does not rebuild the rows (and move the submenu).
    readonly property string shownProductKey: state && state.product ? state.product.id + "/" + state.product.elevationIndex : ""
    readonly property string productRowsKey: JSON.stringify(productRows)
    readonly property int mosaicCount: mosaicSet ? mosaicSet.sites.length : 0
    readonly property var mapMenuRows: {
        if (!mapMenu.opened) return [];
        var rows = [{kind: "header", label: "REGION"}];
        var composites = engine.sites.filter(s => s.kind === "grid" && s.provider !== "mosaic");
        for (let s of composites)
            rows.push({label: (s.name || s.id).toUpperCase(), note: "COMPOSITE", current: s.id === siteId, run: () => app.choose(s)});
        var mine = engine.sites.find(s => s.provider === "mosaic");
        if (mine) rows.push({label: "MY MOSAIC", note: mosaicCount ? mosaicCount + " RADARS" : "", current: mine.id === siteId, run: () => app.choose(mine)});
        for (let id of recentSites) {
            let r = engine.sites.find(x => x.id === id);
            // Review S3: these go first when the card would not fit.
            if (r) rows.push({label: (r.name || r.id).toUpperCase(), note: r.country || "", current: r.id === siteId, droppable: true, run: () => app.choose(r)});
        }
        rows.push({label: "MORE…", run: () => picker.show("")});
        rows.push({kind: "sep"});
        var products = JSON.parse(productRowsKey);
        rows.push({label: "PRODUCT · " + (productLabel || "—"), enabled: products.length > 0,
                   sub: products.map(r => ({label: r.label, note: r.note,
                                               current: shownProductKey === r.product + "/" + r.index,
                                               run: () => app.chooseProduct(r)}))});
        rows.push({kind: "header", label: "LAYERS"});
        var l = store.layers;
        for (let row of [["radar", "RADAR"], ["temp", "TEMPERATURE"], ["wind", "WIND"], ["lightning", "LIGHTNING"]])
            rows.push({label: row[1], checked: !!l[row[0]], run: () => app.store.layers.toggle(row[0])});
        rows.push({label: "FROM · " + String(l.source).toUpperCase(),
                   sub: [["stations", "STATIONS"], ["grid", "GRID"], ["both", "BOTH"]].map(row => ({
                       label: row[1], radio: true, checked: l.source === row[0], run: () => app.store.layers.setSource(row[0])}))});
        rows.push({kind: "sep"});
        rows.push({label: "LOCATION…", run: () => locationPicker.show("")});
        rows.push({label: "KEYS", run: () => sheet.show()});
        // S47's universal reset (guarded for a host without it).
        if (typeof app.resetAll === "function") {
            rows.push({kind: "sep"});
            rows.push({label: "RESET (RELOAD EVERYTHING)", run: () => app.resetAll()});
        }
        return rows;
    }
    function openMapMenu(mx, my) {
        treatmentMenu.close();
        productMenu.close();
        // Review M1: never over an open overlay (it keeps the keyboard).
        if (mapCovered) return;
        var p = map.mapToItem(mapMenu, mx, my);
        mapMenu.show(p.x, p.y);
    }
    // S46: the chrome for checks and captures: the right-click menu, the
    // legend zoom, and where each overlay is (surface pixels).
    // quickshell ipc --pid <pid> call chrome menu 300 200
    IpcHandler {
        target: "chrome"
        function menu(x: real, y: real): void { app.openMapMenu(x, y); }
        function closeMenu(): void { mapMenu.close(); }
        // The recent radars, set for checks (a real visit selects a radar).
        function recent(ids: string): void { app.recentSites = ids.split(",").filter(x => x); }
        function shownRows(): string { return JSON.stringify(mapMenu.shownRows.map(r => r.kind || r.label)); }
        function legend(name: string): void { app.legendZoom = name; }
        function rect(name: string): string {
            var it = ({map: map, picker: picker.cardItem, chip: siteTitle, menu: mapMenu.cardItem, submenu: mapMenu.subItem,
                       layers: layersPanel.cardItem, sheet: sheet.cardItem, location: locationPicker.cardItem, mosaic: mosaicPicker, treatment: treatmentMenu, product: productMenu,
                       help: helpChip, scale: scaleBar, rings: ringCaptionBox, loading: loadingCard, credit: mapCredit, north: northMark,
                       obsLegend: obsLayer.legendItem, obsTip: obsLayer.tipItem, lightningKey: lightningLayer.keyItem,
                       legend: legend, legendCard: legendZoomCard})[name];
            if (!it) return "";
            var p = it.mapToItem(surface, 0, 0), q = it.mapToItem(surface, it.width, it.height);
            return JSON.stringify({x: Math.round(p.x), y: Math.round(p.y), w: Math.round(q.x - p.x), h: Math.round(q.y - p.y), visible: it.visible});
        }
        function status(): string {
            return JSON.stringify({menu: mapMenu.opened, rows: app.mapMenuRows.map(r => r.kind || (r.label + (r.checked === true ? " [x]" : r.checked === false ? " [ ]" : "") + (r.current ? " *" : "") + (r.sub ? " >" : ""))),
                                   cursor: mapMenu.cursor, sub: mapMenu.subRow, inSub: mapMenu.inSub, legendZoom: app.legendZoom,
                                   span: Math.round(map.span * 1000) / 1000, lat: Math.round(map.centerLat * 10000) / 10000, lon: Math.round(map.centerLon * 10000) / 10000,
                                   covered: app.mapCovered, base: app.theme.baseSize, size: app.theme.size, picker: picker.open, recent: app.recentSites,
                                   resetAll: typeof app.resetAll === "function"});
        }
    }
    // My mosaic (S25): whether it is the station shown, and the chosen
    // radars' circles for the map (the checklist's while it is open).
    readonly property bool mosaicShown: !!engine.site && engine.site.provider === "mosaic"
    readonly property var mosaicSet: state && state.mosaic && state.mosaic.sites ? state.mosaic : null
    readonly property var mosaicCircles: mosaicPicker.open ? mosaicPicker.circles
        : !mosaicShown || !mosaicSet ? [] : mosaicSet.sites.map(s => {
            var site = engine.sites.find(x => x.id === s.id);
            return site ? {id: s.id, lat: site.lat, lon: site.lon, km: Mosaic.reachOf(s, site)} : null;
        }).filter(c => c !== null)
    readonly property string mosaicLabel: !mosaicShown ? "" : !mosaicSet || !mosaicSet.sites.length ? "CHOOSE RADARS"
        : Mosaic.setName(engine.mosaic ? engine.mosaic.rules : [], mosaicSet).toUpperCase() + " / " + mosaicSet.sites.length + (mosaicSet.sites.length === 1 ? " RADAR" : " RADARS")
    // S40: the panel's cursor radar on the map beside it. After a key or a
    // list click (cursorMoved) the map centres on it when it is off screen
    // or within 60 px of an edge (100 px on the right, where its label
    // goes); at open and when the dock resizes the map, only when it is
    // off screen. Hover and map clicks never move the camera.
    function keepInView(id, band) {
        var s = engine.sites.find(x => x.id === id);
        if (!s || !mosaicPicker.open || !mosaicPicker.docked) return;
        var x = map.sx(map.mercatorX(s.lon)), y = map.sy(map.mercatorY(s.lat)), m = band === undefined ? 60 : band;
        if (x < m || x > map.width - m - (m ? 40 : 0) || y < m || y > map.height - m) map.lookAt(s.lat, s.lon);
    }
    // S40 review 3: the camera when the panel opened, and whether the user
    // moved it since; a cancel puts back a camera only the panel moved, so
    // arrowing through the list and Escape never hands the station off.
    property var mosaicView: null
    property bool mosaicNavigated: false
    // SHOW in the checklist: My mosaic, centred on the chosen radars.
    function showMosaic(set) {
        if (!engine.mosaic) return;
        var s = 90, n = -90, w = 180, e = -180;
        for (var x of set.sites) {
            var site = engine.sites.find(r => r.id === x.id);
            if (!site) continue;
            var km = Mosaic.reachOf(x, site), dLat = km / 111.2, dLon = km / (111.2 * Math.cos(site.lat * Math.PI / 180));
            s = Math.min(s, site.lat - dLat); n = Math.max(n, site.lat + dLat);
            w = Math.min(w, site.lon - dLon); e = Math.max(e, site.lon + dLon);
        }
        if (s > n) return;
        store.chooseRadar(engine.mosaic.station, (s + n) / 2, (w + e) / 2, "My mosaic");
        applyView();
        map.zoom(Math.max((n - s) * 111.2, (e - w) * 111.2 * Math.cos((s + n) / 2 * Math.PI / 180)) * 1.05);
    }
    // The product chooser (S20, docs/protocol.md products): the selected
    // radar's products (hello.sites[].products, named by hello.products),
    // then its angles with the beam centre's height at 50 and 100 km. Empty
    // for My mosaic (its set's rule), an archive, or an engine older than
    // S20, which hides the chip. S24b: the composites (sweden, nordic) list
    // products the engine makes from their radars; there REF is the
    // provider's own composite. The choice is the engine's, shared with
    // every client.
    readonly property var productRows: {
        var st = engine.state;
        var site = engine.sites.find(s => s.id === app.siteId);
        if (!st || !st.product || st.source !== "live" || !site || !(site.products || []).length || !engine.products.length) return [];
        var names = {};
        for (var p of engine.products) names[p.id] = p.name;
        var composite = site.kind === "grid";
        var rows = site.products.map(id => ({product: id, index: 0, label: composite && id === "REF" ? "COMPOSITE" : (names[id] || id).toUpperCase(),
                                             note: composite && id !== "REF" ? "ALL RADARS" : ""}));
        if (site.products.indexOf("REF") >= 0)
            (site.elevations || []).forEach((e, i) => { if (i > 0) rows.push({product: "REF", index: i, label: e.deg.toFixed(1) + "°", note: e.beamKm50 + " / " + e.beamKm100 + " KM"}); });
        return rows;
    }
    readonly property string productLabel: {
        var st = engine.state;
        if (!st || !st.product) return "";
        var row = productRows.find(r => r.product === st.product.id && r.index === st.product.elevationIndex);
        return (row ? row.label : st.product.id) + (heightM > 0 ? " " + heightM / 1000 + " KM" + (chosenAbove === "ground" ? " ABOVE GROUND" : "") : "");
    }
    function chooseProduct(row) {
        productMenu.close();
        var command = {type: "set_product", product: row.product, elevationIndex: row.index};
        // Height again keeps the height on screen (S29), and what it is
        // measured from (S30); else the engine's default, 2 km above sea.
        if (row.product === "CAPPI" && heightM > 0) command.heightM = heightM;
        if (row.product === "CAPPI" && groundOffered && chosenAbove === "ground") command.above = "ground";
        engine.send(command);
    }
    // S30: a height may be above the ground (hello.products' CAPPI entry
    // says so; an older engine's does not, and the choice stays hidden).
    // S32: and the station's own hello.sites[].above has it (the terrain
    // grid is Nordic, and since S36 every radar sits on it); an engine
    // before S32 sends no per-station list.
    readonly property bool groundOffered: {
        var p = engine.products.find(x => x.id === "CAPPI");
        if (!p || !Array.isArray(p.above) || p.above.indexOf("ground") < 0) return false;
        var id = engine.state ? engine.state.site.id : "";
        var s = engine.sites.find(x => x.id === id);
        return !(s && Array.isArray(s.above) && s.above.indexOf("ground") < 0);
    }
    readonly property string chosenAbove: {
        var p = engine.state && engine.state.product;
        return p && p.id === "CAPPI" && p.above === "ground" ? "ground" : "sea";
    }
    function setAbove(above) {
        if (!heightM || above === chosenAbove) return;
        engine.send({type: "set_product", product: "CAPPI", heightM: heightM, above: above});
    }
    // S29: the Height product's height above sea level in 500 m steps, the
    // − and + of the product menu. A step shows at once and is sent when the
    // clicking pauses, so a run of steps restarts the engine's poller once.
    readonly property int chosenHeight: {
        var p = engine.state && engine.state.product;
        return p && p.id === "CAPPI" && p.heightM > 0 ? p.heightM : 0;
    }
    property int pendingHeight: 0
    readonly property int heightM: chosenHeight > 0 ? (pendingHeight || chosenHeight) : 0
    onChosenHeightChanged: pendingHeight = 0
    function stepHeight(delta) {
        if (!heightM) return;
        pendingHeight = Math.max(500, Math.min(12000, heightM + 500 * delta));
        heightSend.restart();
    }
    Timer {
        id: heightSend
        interval: 350
        onTriggered: if (app.pendingHeight > 0 && app.pendingHeight !== app.chosenHeight)
            engine.send(app.groundOffered
                ? {type: "set_product", product: "CAPPI", heightM: app.pendingHeight, above: app.chosenAbove}
                : {type: "set_product", product: "CAPPI", heightM: app.pendingHeight})
    }
    Connections { target: engine; function onRejectionChanged() { if (engine.rejection) app.pendingHeight = 0; } }
    /// S47 (docs/protocol.md, Reset): forget what is transient and load the
    /// current choices afresh: this window's engine connection drops its
    /// state, frames, layer lines and load and reconnects, and asks the
    /// engine to abort its loads and fetch again. The station, product,
    /// layers and view stay. S46's right-click menu calls this.
    function resetAll() {
        treatmentMenu.close();
        productMenu.close();
        if (layersPanel.opened) layersPanel.close();
        pendingHeight = 0;
        engine.resetAll();
        notice = "RESET · RELOADING RADAR AND LAYERS";
        noticeTimer.restart();
    }
    function run(action) {
        switch (action) {
        case "search": treatmentMenu.close(); picker.show(""); break;
        case "nearest": nearest(); break;
        case "lock": toggleLock(); break;
        case "home": locationPicker.show(""); break;
        case "pan_left": map.pan(-1, 0); break;
        case "pan_right": map.pan(1, 0); break;
        case "pan_up": map.pan(0, -1); break;
        case "pan_down": map.pan(0, 1); break;
        case "zoom_in": map.zoom(Math.min(map.span, map.maxSpan) / 1.25); break;
        case "zoom_out": map.zoom(Math.min(map.span, map.maxSpan) * 1.25); break;
        case "reset": resetView(); break;
        case "previous_frame": step(-1); break;
        case "next_frame": step(1); break;
        case "play": togglePlay(); break;
        case "oldest": jump(false); break;
        case "newest": jump(true); break;
        case "pixels": case "glyphs": case "stipple": treatment = action.toUpperCase(); treatmentMenu.close(); break;
        case "weak": weakFloor = weakFloor === null ? configuredFloor : null; break;
        case "relief": relief = !relief; break;
        case "help": treatmentMenu.close(); if (sheet.open) sheet.close(); else sheet.show(); break;
        case "layers": treatmentMenu.close(); productMenu.close(); if (layersPanel.opened) layersPanel.close(); else layersPanel.show(); break;
        case "reset_all": resetAll(); break;
        case "close": dismiss(); break;
        }
    }
    // Drives the keyboard map from outside for checks and captures:
    // quickshell ipc --pid <pid> call keys run pan_left
    IpcHandler {
        target: "keys"
        function run(action: string): void { app.run(action); }
        function bindings(): string { return JSON.stringify(app.bindings); }
        function errors(): string { return JSON.stringify(app.configErrors); }
        function menu(open: bool): void { if (open) treatmentMenu.show(); else treatmentMenu.close(); }
        function productChooser(open: bool): void { if (open) productMenu.show(); else productMenu.close(); }
        function products(): string {
            return JSON.stringify({label: app.productLabel, rows: app.productRows, menu: productMenu.opened,
                                   heightM: app.heightM, coverageKm: map.coverageKm, rings: map.rings, ringNote: map.ringNote});
        }
        // S29: the menu's height steps, for checks and captures (S37 took
        // the reach steps out).
        function setHeight(m: int): void { if (app.heightM > 0) { app.pendingHeight = m; heightSend.restart(); } }
        function field(name: string): string { var value = JSON.parse(status())[name]; return value === undefined ? "" : String(value); }
        function status(): string {
            return JSON.stringify({sheet: sheet.open, menu: treatmentMenu.opened, treatment: app.treatment, weakFloor: app.weakFloor === null ? "off" : app.weakFloor, error: app.configError,
                                   span: Math.round(map.span * 10) / 10, lat: Math.round(map.centerLat * 1000) / 1000, lon: Math.round(map.centerLon * 1000) / 1000,
                                   locationSource: app.store.locationSource, needsLocation: app.store.needsLocation, locating: app.store.locating,
                                   site: app.siteId, locked: app.locked, lockSource: app.store.lockSource, outsideCoverage: app.outsideCoverage});
        }
    }
    // S41: the load as the surfaces show it, for checks and captures.
    IpcHandler {
        target: "loading"
        function status(): string {
            return JSON.stringify({busy: engine.busy, drawn: engine.drawn, card: loadingCard.visible, bar: loadingBar.visible,
                                   percent: engine.percent, raw: engine.overallOf(engine.loading), segments: loadingBar.segments.length,
                                   stage: engine.loading ? engine.loading.stage : "", step: engine.activeStep ? engine.activeStep.name : "",
                                   detail: engine.activeStep ? engine.activeStep.detail : "", name: engine.loadName, error: engine.error,
                                   steps: engine.steps.map(s => s.id + ":" + s.state)});
        }
    }
    // S42: the layers for checks and captures:
    // quickshell ipc --pid <pid> call layers set temp true
    IpcHandler {
        target: "layers"
        function set(name: string, on: bool): void { app.store.layers.set(name, on); }
        // S47: Reset, and each layer's state as the panel says it.
        function reset(): void { app.resetAll(); }
        function states(): string {
            return JSON.stringify({stations: engine.stationsLayer, grid: engine.gridLayer, lightning: engine.lightningLayer,
                                   loads: engine.layerLoads.map(l => l.id), resetting: engine.resetting,
                                   radar: engine.loading ? engine.loading.label : "", percent: engine.percent,
                                   frame: engine.frame ? engine.frame.scanTime : "", timeline: engine.timeline.length});
        }
        function panel(open: bool): void { if (open) layersPanel.show(); else layersPanel.close(); }
        // S43: stations, grid or both.
        function source(name: string): void { app.store.layers.setSource(name); }
        function status(): string {
            var l = app.store.layers, o = engine.obs, g = engine.grid;
            return JSON.stringify({radar: l.radar, temp: l.temp, wind: l.wind, source: l.source, panel: layersPanel.opened,
                                   stations: o ? o.stations.length : -1, shown: obsLayer.shown.length, thins: obsLayer.thinCount,
                                   gridTime: g ? g.time : "", gridPoints: g && g.wind ? g.wind.points.length : -1,
                                   underlay: map.underlay ? map.underlay.source : "",
                                   attribution: o ? o.attribution : "", status: layersPanel.status});
        }
        function hover(x: real, y: real): string { var s = obsLayer.stationAt(x, y); return s ? JSON.stringify(s) : ""; }
        function mark(id: string): string {
            var p = obsLayer.shown.find(q => q.s.id === id);
            return p ? JSON.stringify({x: Math.round(map.sx(p.mx)), y: Math.round(map.sy(p.my))}) : "";
        }
        function shownIds(): string { return JSON.stringify(obsLayer.shown.map(p => p.s.id)); }
        // S44: what the lightning layer holds and draws.
        function lightning(): string {
            var m = engine.lightning;
            return JSON.stringify({on: app.store.layers.lightning, status: m ? m.status : "", count: lightningLayer.count,
                                   shown: lightningLayer.shownCounts, drawn: lightningLayer.drawn, paints: lightningLayer.paints, paintMs: lightningLayer.paintMs,
                                   live: lightningLayer.live, ref: new Date(lightningLayer.refMs).toISOString(),
                                   replay: m ? m.replay : null, attribution: m ? m.attribution : "", hello: !!engine.lightningInfo,
                                   frames: app.frames.filter(f => f.status === "complete").length, frame: app.scan ? app.scan.scanTime : ""});
        }
    }
    // Site navigation (DESIGN.md, location): the lock pins the radar against
    // hand-offs; `n` releases it and selects the nearest radar without moving
    // the camera. The site picker locks and centres on that station.
    readonly property bool locked: state ? state.site.locked : false
    readonly property bool following: state ? state.site.follow && !state.site.locked : false
    readonly property var resetTarget: Location.resolveReset(Location.configCenter(config.values), config.location)
    readonly property bool outsideCoverage: {
        var s = engine.site;
        return !!(locked && s && Location.distanceKm(map.centerLat, map.centerLon, s.lat, s.lon) > map.coverageKm);
    }
    function toggleLock() {
        if (!state || !siteId) return;
        store.setLock(locked ? "" : siteId, !locked);
    }
    // MOCK: what the place chip says. OMASTORM_MOCK_GPS stands in for a
    // receiver so the GPS states can be captured without one.
    readonly property string mockGps: Quickshell.env("OMASTORM_MOCK_GPS") || ""
    readonly property string placeState: mockGps === "following" || mockGps === "home" ? "FOLLOWING" : mockGps === "nofix" ? "NO FIX" : ""
    readonly property string placeLabel: {
        if (mockGps === "home") return "GÖTEBORG";
        if (mockGps && mockGps !== "paused") return "GPS";
        var t = app.resetTarget;
        if (t && Location.distanceKm(map.centerLat, map.centerLon, t.lat, t.lon) < 2) return (t.name || "OMARCHY'S LOCATION").toUpperCase();
        if (app.store.placeName && Location.distanceKm(map.centerLat, map.centerLon, app.store.centerLat, app.store.centerLon) < 2) {
            var name = app.store.placeName.toUpperCase();
            return app.store.locationSource === "ip" ? "IP NEAR " + name : name;
        }
        if (app.store.locationSource === "ip" && Location.distanceKm(map.centerLat, map.centerLon, app.store.centerLat, app.store.centerLon) < 2)
            return "IP NEAR YOU";
        var lat = map.centerLat, lon = map.centerLon;
        return Math.abs(lat).toFixed(2) + "° " + (lat < 0 ? "S" : "N") + "  " + Math.abs(lon).toFixed(2) + "° " + (lon < 0 ? "W" : "E");
    }
    property string notice: ""
    Timer { id: noticeTimer; interval: 3000; onTriggered: app.notice = "" }
    function resetView() {
        store.resetView();
        applyView();
    }
    function nearest() {
        var s = map.nearest();
        if (!state || !s) return;
        store.followNearest(s.id);
    }
    function choose(s) {
        if (!state || !s) return;
        // My mosaic with no set, here or remembered: the checklist first.
        if (s.provider === "mosaic" && !(mosaicSet && mosaicSet.sites.length) && !Mosaic.valid(store.mosaicStore.set)) {
            mosaicPicker.show();
            return;
        }
        if (s.provider === "mosaic") {
            showMosaic(mosaicSet && mosaicSet.sites.length ? mosaicSet : store.mosaicStore.set);
            return;
        }
        store.chooseRadar(s.id, Number(s.lat), Number(s.lon), s.name || s.id);
        applyView();
    }
    // Drives the picker from outside for checks and captures:
    // quickshell ipc --pid <pid> call picker open tul
    IpcHandler {
        target: "picker"
        function open(query: string): void { picker.show(query); }
        function accept(): void { picker.accept(); }
        function close(): void { picker.close(); }
        function move(delta: int): void { picker.move(delta); }
        function matches(): string { return JSON.stringify(picker.rows.map(r => r.site.id)); }
        function status(): string { return JSON.stringify({open: picker.open, query: picker.query, selected: picker.selected, total: picker.ranked.total, focused: picker.fieldFocused}); }
    }
    // My mosaic's checklist from outside, for checks and captures (S25):
    // quickshell ipc --pid <pid> call mosaic toggle vara
    IpcHandler {
        target: "mosaic"
        function open(): void { mosaicPicker.show(); }
        function close(): void { mosaicPicker.close(); }
        function accept(): void { mosaicPicker.accept(); }
        function toggle(id: string): void { mosaicPicker.toggle(id); }
        function rule(id: string): void { mosaicPicker.setRule(id); }
        // S30: the height rule's height (metres) and what it is above.
        function height(m: int): void { mosaicPicker.setHeight(m); }
        function above(id: string): void { mosaicPicker.setAbove(id); }
        // S40: the cursor and the filter, as the keys would move them.
        function move(delta: int): void { mosaicPicker.move(delta); }
        function filter(text: string): void { mosaicPicker.filter = text; }
        function status(): string {
            // S40: where the cursor is, what holds the keyboard, the
            // filter and how many rows it leaves, and the map beside it.
            return JSON.stringify({open: mosaicPicker.open, draft: mosaicPicker.draft, rows: mosaicPicker.radars.length,
                                   cursor: mosaicPicker.cursor, hot: mosaicPicker.hotId, focused: mosaicPicker.listFocused,
                                   filterFocused: mosaicPicker.filterFocused, filter: mosaicPicker.filter, shownRows: mosaicPicker.rows.length,
                                   docked: mosaicPicker.open && mosaicPicker.docked, warning: mosaicPicker.warning, lat: Math.round(map.centerLat * 1000) / 1000, lon: Math.round(map.centerLon * 1000) / 1000, mapWidth: Math.round(map.width), span: Math.round(map.span * 10) / 10,
                                   pickMode: map.pickMode, mapHot: map.hotId, mapPicked: map.pickedIds,
                                   circles: app.mosaicCircles.length, label: app.mosaicLabel, shown: app.mosaicShown,
                                   command: engine.sites.length ? Mosaic.command(mosaicPicker.draft, engine.sites, engine.mosaic ? engine.mosaic.rules : null) : null});
        }
    }
    IpcHandler {
        target: "location"
        function open(query: string): void { locationPicker.show(query); }
        function accept(): void { locationPicker.accept(); }
        function close(): void { locationPicker.close(); }
        function move(delta: int): void { locationPicker.move(delta); }
        function go(lat: string, lon: string, name: string): void { locationPicker.go(Number(lat), Number(lon), name); }
        function setLat(text: string): void { locationPicker.latText = text; }
        function setLon(text: string): void { locationPicker.lonText = text; }
        function matches(): string { return JSON.stringify(locationPicker.rows.map(r => r.where ? r.name + ", " + r.where : r.name)); }
        function status(): string { return JSON.stringify({open: locationPicker.open, query: locationPicker.query, selected: locationPicker.selected, focused: locationPicker.fieldFocused, count: locationPicker.rows.length, lat: locationPicker.latText, lon: locationPicker.lonText, error: locationPicker.coordError}); }
    }
    readonly property var theme: session ? session.theme.snapshot : themeInputs.snapshot
    // S46: fixed chrome (controls, menu cards, rows) grows with Omarchy's
    // text size above the default base of 12 and never shrinks below it.
    readonly property real grow: Math.max(1, theme.size.k)
    Theme { id: themeInputs; registerIpc: !app.session }
    // Glyphs is the default; config.toml's `treatment` and the keys change it.
    property string treatment: session ? session.treatment : Quickshell.env("OMASTORM_STYLE") || "GLYPHS"
    onTreatmentChanged: if (session) session.treatment = treatment
    // S24d: Relief, a storm height lit as a surface (the product menu or the
    // `relief` action); the popover follows through the session.
    // OMASTORM_RELIEF=1 turns it on for checks and captures.
    property bool relief: session ? session.relief : Quickshell.env("OMASTORM_RELIEF") === "1"
    onReliefChanged: if (session) session.relief = relief
    // The weak-return floor (DESIGN.md, weak-return floor): measured returns
    // under this many dBZ draw nothing, null draws them all. `w` toggles
    // between off and the configured floor (the default when config.toml
    // has none or says false); the popover inherits it through the session.
    property var weakFloor: session ? session.weakFloor : KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) !== undefined ? KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) : KeyMap.DEFAULT_FLOOR
    onWeakFloorChanged: if (session) session.weakFloor = weakFloor
    readonly property real configuredFloor: { var floor = KeyMap.weakFloor(config.weakFloor, []); return floor === null ? KeyMap.DEFAULT_FLOOR : floor; }
    Connections {
        target: app.session
        function onTreatmentChanged() { app.treatment = app.session.treatment; }
        function onWeakFloorChanged() { app.weakFloor = app.session.weakFloor; }
        function onLocationPickerRequested() { if (app.opened) locationPicker.show(""); }
    }
    // Quickshell keeps the process alive after its last window closes, which
    // left 400-600 MB orphans behind every close. Quit with the window; the
    // engine daemon is separate and stays up.
    Connections { target: Quickshell; function onLastWindowClosed() { if (!app.session) Qt.quit(); } }
    FloatingWindow {
        id: win
        title: "Omastorm SE"
        visible: app.opened
        onVisibleChanged: if (!visible && app.opened) app.dismiss()
        implicitWidth: Number(Quickshell.env("OMASTORM_WIDTH")) || 960
        implicitHeight: Number(Quickshell.env("OMASTORM_HEIGHT")) || 680
        minimumSize: Qt.size(360 * Math.max(1, app.theme.baseSize/12), 360 * Math.max(1, app.theme.baseSize/12))
        color: app.theme.background
        property bool compact: width < 560

        component LabelText: Text {
            color: app.theme.foreground
            font.family: app.theme.font
            font.pixelSize: app.theme.size.body
            elide: Text.ElideRight
        }
        component Control: Button {
            id: button
            property bool selected: false
            implicitHeight: Math.round(30 * app.grow)
            implicitWidth: Math.max(Math.round(30 * app.grow), contentItem.implicitWidth + 18)
            padding: 6
            contentItem: LabelText {
                text: button.text
                color: button.selected ? app.theme.background : app.theme.foreground
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
            }
            background: Rectangle {
                color: button.selected ? app.theme.accent : button.hovered || button.activeFocus ? Qt.alpha(app.theme.accent, .18) : "transparent"
                border.width: 1
                border.color: button.selected || button.activeFocus ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
            }
        }
        // Chrome icons as Nerd Font glyphs (same Material Design Icons set
        // Omarchy's shell uses for media / panels). The theme's monospace
        // alias resolves to JetBrainsMono Nerd Font on Omarchy.
        component Glyph: Item {
            id: glyphRoot
            property string glyph: "play"
            property color ink: app.theme.foreground
            property real fade: 1
            implicitWidth: 16
            implicitHeight: 16
            // Codepoints match Omarchy media (play/pause/prev/next) and common
            // MDI lock / keyboard / crosshair / search / chevron glyphs.
            readonly property var icons: ({
                "play": "󰐊", "pause": "󰏤", "back": "󰒮", "fwd": "󰒭",
                "first": "󰒫", "last": "󰒬", "lock": "󰌾", "unlock": "󰌿",
                "keys": "󰌌", "follow": "󰆣", "search": "󰍉", "chevron": "󰅀",
                "radar": "󰐷"
            })
            Text {
                anchors.centerIn: parent
                text: glyphRoot.icons[glyphRoot.glyph] || ""
                color: glyphRoot.ink
                opacity: glyphRoot.fade
                font.family: app.theme.font
                font.pixelSize: app.theme.size.title
                renderType: Text.NativeRendering
            }
        }
        // A 30 px control showing one glyph. Never takes keyboard focus: the
        // keys stay global.
        component GlyphButton: Button {
            id: transport
            property string glyph: "play"
            property bool selected: false
            implicitHeight: Math.round(30 * app.grow)
            implicitWidth: Math.round(30 * app.grow)
            padding: 0
            focusPolicy: Qt.NoFocus
            contentItem: Glyph {
                glyph: transport.glyph
                ink: transport.selected ? app.theme.background : app.theme.foreground
                fade: transport.enabled ? 1 : .35
            }
            background: Rectangle {
                color: transport.selected ? app.theme.accent : transport.hovered ? Qt.alpha(app.theme.accent, .18) : "transparent"
                border.width: 1
                border.color: transport.selected ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
            }
        }
        // MOCK: one chip per idea, in two parts. The name opens the picker;
        // the glyph to its right is the toggle (crosshair = follow place,
        // padlock = pin radar), filled with the accent while on.
        component Chip: RowLayout {
            id: chip
            property string glyph: "follow"
            property string label: ""
            property string tag: ""
            property bool on: false
            property bool tagAccent: false
            property bool enabled: true
            signal toggled()
            signal opened()
            spacing: 0
            Button {
                id: name
                implicitHeight: Math.round(30 * app.grow)
                implicitWidth: contentItem.implicitWidth + (win.compact ? 14 : 22)
                padding: 0
                focusPolicy: Qt.NoFocus
                enabled: chip.enabled
                visible: !win.compact
                onClicked: chip.opened()
                contentItem: RowLayout {
                    spacing: 7
                    Item { Layout.fillWidth: true }
                    LabelText { text: chip.label; opacity: chip.enabled ? 1 : .35 }
                    LabelText {
                        text: chip.tag; visible: chip.tag !== ""
                        color: chip.tagAccent ? app.theme.accent : app.theme.foreground
                        opacity: chip.tagAccent ? .9 : .55
                        font.pixelSize: app.theme.size.small; font.letterSpacing: 1
                    }
                    Glyph { glyph: "chevron"; implicitWidth: 12; fade: .6 }
                    Item { Layout.fillWidth: true }
                }
                background: Rectangle {
                    color: name.hovered ? Qt.alpha(app.theme.accent, .18) : "transparent"
                    border.width: 1
                    border.color: Qt.alpha(app.theme.foreground, .22)
                }
            }
            GlyphButton {
                glyph: chip.glyph; selected: chip.on; enabled: chip.enabled
                Layout.leftMargin: -1
                onClicked: chip.toggled()
            }
        }
        Rectangle {
          id: surface
          anchors.fill: parent
          color: app.theme.background
          // One Shortcut per action with the bindings in force (Keys.js has
          // the defaults). While the picker or the sheet is open its card
          // has the keyboard and every window shortcut stands down; while
          // the treatment menu is open the treatment keys still choose and
          // the menu itself takes Escape.
          Instantiator {
            model: KeyMap.ACTIONS.map(a => a.id)
            delegate: Shortcut {
                sequences: app.bindings[modelData] || []
                enabled: app.opened && !app.overlayOpen && !layersPanel.opened && (!treatmentMenu.opened || modelData === "pixels" || modelData === "glyphs" || modelData === "stipple")
                onActivated: app.run(modelData)
            }
          }
          ColumnLayout {
            id: layout
            anchors.fill: parent
            anchors.margins: win.compact ? 12 : 20
            // Review S2: room for the loading bar's names, which hang below
            // the tick strip and grow with the text.
            anchors.bottomMargin: Math.max(win.compact ? 12 : 20, loadingBar.implicitHeight + 2)
            spacing: 10
            // Chrome names (use these when tweaking):
            //   brand row     — mark, OMASTORM, status light, LIVE/ARCHIVED
            //   site row      — station title, radar lock (yellow when outside coverage)
            //   product stack — product line + meta line (right of site row)
            //   product line  — REFLECTIVITY / tilt + SMHI
            //   meta line     — age, right-aligned under the product line
            //   map stage     — radar map frame
            //   follow chip   — crosshair (place follow); hidden until GPS
            //   help chip     — ? keys on the map
            //   scale bar     — ground distance, bottom-left of the map
            //   legend        — dBZ scale under the map
            //   transport     — playback buttons
            //   tick strip    — frame ticks
            //   strip stamp   — date/time/zone above the tick strip
            //   frame index   — N / available frames above the strip
            RowLayout {
                id: brandRow
                Layout.fillWidth: true
                RadarMark { ink: app.theme.accent; size: 20; Layout.rightMargin: 8 }
                LabelText { text: "OMASTORM SE"; font.bold: true; font.letterSpacing: 2.5; font.pixelSize: app.theme.size.heading }
                Item { Layout.fillWidth: true }
                // LIVE / ARCHIVED as text; the light carries feed health.
                RowLayout {
                    spacing: 8
                    Rectangle {
                        id: statusLight
                        width: 8; height: 8; radius: 4
                        Layout.alignment: Qt.AlignVCenter
                        visible: !!app.state
                        color: app.statusLightColor
                        SequentialAnimation on opacity {
                            running: app.statusLightPulse
                            loops: Animation.Infinite
                            NumberAnimation { from: 1; to: .25; duration: 700; easing.type: Easing.InOutSine }
                            NumberAnimation { from: .25; to: 1; duration: 700; easing.type: Easing.InOutSine }
                            onRunningChanged: if (!running) statusLight.opacity = 1
                        }
                    }
                    LabelText { text: app.sourceBadge; color: app.theme.accent; font.letterSpacing: 1.5 }
                }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(app.theme.foreground, .25) }
            RowLayout {
                id: siteRow
                Layout.fillWidth: true
                // MOCK: the station title is the radar control. Click it to
                // pick a station; the padlock beside it pins that radar (not
                // the map — the crosshair on the map is place-follow).
                // No border or hover fill on the title — it reads as text.
                Button {
                    id: siteTitle
                    implicitHeight: Math.round(30 * app.grow)
                    padding: 0
                    focusPolicy: Qt.NoFocus
                    // S40 review 4: nothing while My mosaic's panel is open.
                    enabled: !!app.state && !mosaicPicker.open
                    onClicked: picker.show("")
                    Layout.alignment: Qt.AlignTop
                    contentItem: RowLayout {
                        spacing: 8
                        // A table station's name is the title. SMHI ids are the
                        // town folded to ASCII ("angelholm" for Ängelholm), so
                        // never "VARA vara"; an ODIM node code shows dimmed
                        // beside it ("HURUM nohur"). A station outside the
                        // table (archived KTLX) is its id.
                        LabelText { text: engine.site ? app.siteName : app.siteId || "—"; font.pixelSize: app.theme.size.display; font.bold: true }
                        LabelText { text: app.siteId; visible: !win.compact && !!engine.site && !app.idIsName; opacity: .65 }
                        Glyph { glyph: "chevron"; implicitWidth: 12; fade: .5 }
                    }
                    background: Item {}
                }
                // Pins the radar on screen; chip outline so it reads as a toggle.
                // Yellow (same stale cue as the status light) when the camera
                // sits outside that radar's rings — no banner.
                Rectangle {
                    id: lockButton
                    implicitWidth: Math.round(30 * app.grow); implicitHeight: Math.round(30 * app.grow)
                    radius: 2
                    Layout.alignment: Qt.AlignTop
                    opacity: !!app.state ? 1 : .35
                    readonly property color lockColor: !app.locked ? app.theme.foreground
                        : app.outsideCoverage ? app.theme.yellow : app.theme.accent
                    color: lockArea.containsMouse && !!app.state ? Qt.alpha(lockColor, .18) : "transparent"
                    border.width: 1
                    border.color: app.locked ? lockColor : Qt.alpha(app.theme.foreground, .22)
                    Glyph {
                        anchors.centerIn: parent
                        glyph: app.locked ? "lock" : "unlock"
                        ink: app.locked ? lockButton.lockColor : app.theme.foreground
                        fade: app.locked ? 1 : .45
                    }
                    MouseArea {
                        id: lockArea
                        anchors.fill: parent
                        hoverEnabled: true
                        enabled: !!app.state
                        onClicked: app.toggleLock()
                    }
                }
                // The radars chip (S37, the human 2026-09-20: "i cant see
                // the list until i click on the product"; S40 moved it out
                // of the hidden bar, where nobody could reach it): while My
                // mosaic is shown it opens and closes My mosaic's panel. It
                // never takes the keyboard, which is the panel's.
                Button {
                    id: radarsChip
                    visible: app.mosaicShown || mosaicPicker.open
                    implicitHeight: Math.round(30 * app.grow)
                    implicitWidth: contentItem.implicitWidth + 18
                    padding: 0
                    focusPolicy: Qt.NoFocus
                    Layout.alignment: Qt.AlignTop
                    Layout.leftMargin: 4
                    opacity: mosaicPicker.open || hovered ? 1 : .8
                    onClicked: mosaicPicker.open ? mosaicPicker.close() : mosaicPicker.show()
                    contentItem: RowLayout {
                        spacing: 6
                        Item { Layout.fillWidth: true }
                        Glyph { glyph: "radar"; implicitWidth: 14; ink: mosaicPicker.open ? app.theme.accent : app.theme.foreground }
                        LabelText {
                            text: (win.compact ? "" : "RADARS · ") + (mosaicPicker.open ? mosaicPicker.draft.sites.length : (app.mosaicSet && app.mosaicSet.sites.length) || 0)
                            color: mosaicPicker.open ? app.theme.accent : app.theme.foreground
                        }
                        Glyph { glyph: "chevron"; implicitWidth: 12; fade: .6; visible: !win.compact }
                        Item { Layout.fillWidth: true }
                    }
                    background: Rectangle {
                        color: mosaicPicker.open || radarsChip.hovered ? Qt.alpha(app.theme.accent, .18) : "transparent"
                        border.width: 1
                        border.color: mosaicPicker.open ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
                    }
                }
                // S42: the LAYERS chip beside the product line opens the
                // layers panel (radar, temperature, wind) and names what is
                // on besides the radar. Like RADARS it never takes the keys.
                Button {
                    id: layersChip
                    implicitHeight: Math.round(30 * app.grow)
                    implicitWidth: contentItem.implicitWidth + 18
                    padding: 0
                    focusPolicy: Qt.NoFocus
                    Layout.alignment: Qt.AlignTop
                    Layout.leftMargin: 4
                    readonly property var store: app.store.layers
                    // S43: where they come from, when it is not the stations.
                    readonly property string extra: [store.temp ? "°C" : "", store.wind ? "WIND" : "", store.lightning ? "⚡" : ""].filter(x => x).join(" + ")
                        + ((store.temp || store.wind) && store.source !== "stations" ? " · " + store.source.toUpperCase() : "")
                    readonly property bool lit: layersPanel.opened || store.temp || store.wind || store.lightning || !store.radar
                    opacity: lit || hovered ? 1 : .8
                    onClicked: app.run("layers")
                    contentItem: RowLayout {
                        spacing: 6
                        Item { Layout.fillWidth: true }
                        LabelText {
                            text: (win.compact && layersChip.extra ? "" : "LAYERS")
                                + (layersChip.extra ? (win.compact ? "" : " · ") + layersChip.extra : "")
                                + (layersChip.store.radar ? "" : " · NO RADAR")
                            color: layersChip.lit ? app.theme.accent : app.theme.foreground
                        }
                        Glyph { glyph: "chevron"; implicitWidth: 12; fade: .6; visible: !win.compact }
                        Item { Layout.fillWidth: true }
                    }
                    background: Rectangle {
                        color: layersPanel.opened || layersChip.hovered ? Qt.alpha(app.theme.accent, .18) : "transparent"
                        border.width: 1
                        border.color: layersChip.lit ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
                    }
                }
                Item { Layout.fillWidth: true }
                // product stack: compact product line; age right-aligned under it.
                ColumnLayout {
                    id: productStack
                    spacing: 2
                    Layout.alignment: Qt.AlignTop | Qt.AlignRight
                    // S40: it gives way (the product line elides) when the
                    // RADARS chip joins a narrow site row, instead of pushing
                    // the whole window's layout past its right edge.
                    Layout.fillWidth: true
                    Layout.maximumWidth: implicitWidth
                    Layout.minimumWidth: 60
                    RowLayout {
                        id: productLine
                        spacing: 8
                        visible: !!app.scan
                        LabelText {
                            id: productText
                            Layout.fillWidth: true
                            // S44 review SF5: the credit gives way first.
                            Layout.minimumWidth: Math.min(120, implicitWidth)
                            horizontalAlignment: Text.AlignRight
                            // An angle only for one scan angle (REF, S20); a
                            // product built from several has none to show.
                            // My mosaic (S25) names its rule and radars, and
                            // opens its checklist from here.
                            text: app.mosaicShown ? app.mosaicLabel
                                : !app.scan ? "" : app.scan.productName.toUpperCase() + (!app.scan.scanTime ? "" : app.scan.kind === "grid" ? " / COMPOSITE" : (app.scan.product || "REF") === "REF" ? " / " + app.scan.elevationDeg.toFixed(1) + "°" : "")
                            font.underline: productTextArea.containsMouse && productTextArea.enabled
                            // The product menu opens from here (S29): the
                            // chip row that held its chip is hidden.
                            MouseArea {
                                id: productTextArea
                                anchors.fill: parent
                                enabled: app.productRows.length > 0 && !mosaicPicker.open
                                hoverEnabled: true
                                cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                                // S37: My mosaic has products of its own now
                                // (REF and Rain mass), so this opens the
                                // product menu for it too; its radar list is
                                // on the RADARS chip.
                                onClicked: productMenu.opened ? productMenu.close() : productMenu.show()
                            }
                        }
                        // The source's credit, verbatim (SMHI, MET Norway, FMI, DMI, OPERA).
                        LabelText {
                            // S44: the strikes' credit beside the frame's
                            // (compact: in the lightning key on the map). It
                            // shrinks, eliding, before the product name does
                            // (review SF5).
                            text: app.attribution + (!win.compact && app.store.layers.lightning && engine.lightning && engine.lightning.attribution
                                ? (app.attribution ? " · " : "") + engine.lightning.attribution : "")
                            visible: text !== ""
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                            Layout.maximumWidth: implicitWidth
                            Layout.minimumWidth: Math.min(40, implicitWidth)
                            font.letterSpacing: 1; opacity: .55
                        }
                    }
                    LabelText {
                        id: metaLine
                        Layout.alignment: Qt.AlignRight
                        visible: app.ageText !== ""
                        text: app.ageText
                        color: app.alert && app.condition !== "loading" ? app.conditionColor : app.theme.foreground
                        opacity: app.alert && app.condition !== "loading" ? 1 : .75
                    }
                }
            }
            // Rejections, config mistakes, and notices only — feed health is
            // the light beside LIVE, not a prose status row.
            LabelText {
                Layout.fillWidth: true
                Layout.topMargin: -4
                text: engine.rejection || app.configError || store.persistError || app.notice
                color: app.theme.accent
                opacity: 1
                visible: text !== ""
                horizontalAlignment: Text.AlignRight
            }
            Rectangle {
                id: mapFrame
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.minimumHeight: 100
                Layout.topMargin: -4
                // S40: My mosaic's panel docks at the stage's right and the
                // map shrinks beside it (a compact window gets an overlay).
                Layout.rightMargin: mosaicPicker.open && mosaicPicker.docked ? mosaicPicker.panelWidth + 10 : 0
                color: app.theme.background
                border.color: Qt.alpha(app.theme.foreground, .17)
                clip: true
                RadarMap {
                    id: map
                    anchors.fill: parent
                    scan: app.scan
                    texture: engine.texture
                    azimuthLut: engine.azimuthLut
                    codes: engine.codes
                    mosaicCircles: app.mosaicCircles
                    // S40 × S39: while My mosaic's panel is open the map is in
                    // pick mode: every radar a mark, the draft's ticked, the
                    // panel's cursor ringed; a click on a mark ticks or
                    // unticks it, and the mark under the pointer puts the
                    // cursor on its row.
                    pickMode: mosaicPicker.open
                    pickedIds: mosaicPicker.pickedIds
                    hotId: mosaicPicker.hotId
                    onRadarPicked: id => { if (mosaicPicker.open) { mosaicPicker.toggle(id); mosaicPicker.point(id); } }
                    onRadarHovered: id => { if (mosaicPicker.open && id !== "") mosaicPicker.point(id); }
                    siteId: app.siteId
                    sites: engine.sites
                    referenceSites: engine.referenceSites
                    tileRoot: "file://" + engine.runtime
                    theme: app.theme
                    treatment: app.treatment
                    weakFloor: app.weakFloor
                    radarOpacity: !app.store.layers.radar ? 0 : app.condition === "unavailable" ? .6 : 1
                    // S43: the MET Nordic temperature field under the radar.
                    underlay: obsLayer.underlay
                    labelSize: win.compact ? app.theme.size.small : app.theme.size.body
                    locked: app.locked
                    product: app.state ? app.state.product : null
                    relief: app.relief
                    interactive: !app.store.needsLocation && !locationPicker.open
                    onNavigated: (lat, lon, spanKm) => { if (mosaicPicker.open) app.mosaicNavigated = true; app.store.userNavigated(lat, lon, spanKm); }
                    // S40: measured again once the dock has resized the map.
                    onWidthChanged: if (mosaicPicker.open) Qt.callLater(app.keepInView, mosaicPicker.hotId, 0)
                    // A settled pan hands the centre to the engine, which switches
                    // station while following and unlocked; the camera stays.
                    onViewSettled: (lat, lon) => {
                        if (!app.opened || app.store.needsLocation) return;
                        // S40: the panel's cursor moves the camera from radar
                        // to radar; none of that may hand the station off.
                        // A cancel reports where the camera ended up.
                        if (mosaicPicker.open) return;
                        // A pick or restore already set the store; a settle
                        // still queued from the previous camera must not
                        // write that centre back (radar jumps, map stays).
                        if (app.applyingView
                            && (Math.abs(lat - app.store.centerLat) > 0.05
                                || Math.abs(lon - app.store.centerLon) > 0.05))
                            return;
                        engine.send({type: "view_center", lat: lat, lon: lon});
                        app.store.rememberView(lat, lon, map.span);
                    }
                    onResetRequested: app.resetView()
                    // S46: the right-click menu, and the one wheel guard.
                    onContextRequested: (x, y) => app.openMapMenu(x, y)
                    wheelBlocked: (x, y) => app.wheelBlockedAt(x, y)
                    Component.onCompleted: app.applyView()
                    // The map asks for tiles when its camera settles and the
                    // engine answers this window alone, tile by tile.
                    onTilesNeeded: (z, x0, y0, x1, y1) => engine.send({type: "tiles_needed", z: z, x0: x0, y0: y0, x1: x1, y1: y1})
                }
                Connections { target: engine; function onTileReady(tile) { map.tileReady(tile); } }
                // S44: lightning strikes over the radar, under the stations.
                LightningLayer {
                    id: lightningLayer
                    anchors.fill: parent
                    map: map
                    message: engine.lightning
                    runtime: engine.runtime
                    on: app.store.layers.lightning
                    theme: app.theme
                    compact: win.compact
                    frame: app.scan
                    // The newest frame, not looping: the trail runs to now.
                    live: !app.playing && !!app.state && app.state.source === "live"
                        && (!app.newestComplete || !app.scan || app.frameIndex >= app.frames.length - 1
                            || app.scan.id === app.newestComplete.id)
                    clockMs: Number(Quickshell.env("OMASTORM_LIGHTNING_CLOCK_MS") || 0)
                    creditInKey: win.compact
                    keyZoomed: app.legendZoom === "lightning"
                    onKeyClicked: app.legendZoom = "lightning"
                }
                // S42: the weather stations over the radar.
                ObsLayer {
                    id: obsLayer
                    anchors.fill: parent
                    map: map
                    obs: engine.obs
                    grid: engine.grid
                    source: app.store.layers.source
                    runtime: engine.runtime
                    temp: app.store.layers.temp
                    wind: app.store.layers.wind
                    theme: app.theme
                    compact: win.compact
                    creditHeight: 30
                    legendZoomed: app.legendZoom === "obs"
                    onLegendClicked: app.legendZoom = "obs"
                }
                // Place-follow (crosshair) stays out of the release until GPS
                // is wired; keep the mock chip for captures via OMASTORM_MOCK_GPS.
                // N ↑ is map orientation only — not a control.
                Rectangle {
                    id: followChip
                    anchors.top: parent.top; anchors.left: parent.left; anchors.margins: 10
                    width: 26; height: 26
                    readonly property bool on: app.mockGps === "following" || app.mockGps === "home"
                    color: on ? app.theme.accent : followArea.containsMouse ? Qt.alpha(app.theme.accent, .18) : Qt.alpha(app.theme.background, .9)
                    border.width: 1; border.color: on ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
                    visible: false
                    Glyph { anchors.centerIn: parent; glyph: "follow"; ink: followChip.on ? app.theme.background : app.theme.foreground }
                    MouseArea { id: followArea; anchors.fill: parent; hoverEnabled: true }
                }
                LabelText {
                    id: northMark
                    anchors.top: parent.top; anchors.left: parent.left; anchors.margins: 10
                    text: "N ↑"; opacity: .75
                    visible: !!app.state
                }
                // The `?` chip in the map's top-right corner (DESIGN.md, window
                // chrome) opens the keys sheet, as does the key itself.
                Rectangle {
                    id: helpChip
                    anchors.top: parent.top; anchors.right: parent.right; anchors.margins: 10
                    width: helpRow.implicitWidth + 12; height: Math.round(22 * app.grow)
                    color: Qt.alpha(app.theme.background, .9)
                    opacity: helpArea.containsMouse ? 1 : .7
                    visible: !!app.state
                    RowLayout {
                        id: helpRow
                        anchors.centerIn: parent
                        spacing: 5
                        Glyph { glyph: "keys" }
                        LabelText { text: "?"; font.pixelSize: app.theme.size.small }
                    }
                    MouseArea { id: helpArea; anchors.fill: parent; hoverEnabled: true; onClicked: app.run("help") }
                }
                // Scale bar (DESIGN.md): fixed-length tick; the label is the
                // round distance that length currently spans. Locale picks
                // kilometres or miles (same measurementSystem as the OS).
                Rectangle {
                    id: scaleBar
                    anchors.bottom: parent.bottom; anchors.left: parent.left; anchors.margins: 10
                    visible: !!app.scan && map.pixelsPerKm > 0
                    readonly property bool metric: Qt.locale().measurementSystem === Locale.MetricSystem
                    readonly property real kmPerMile: 1.609344
                    readonly property var steps: metric
                        ? [1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000]
                        : [0.5, 1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000]
                    readonly property real barPx: 72
                    property real nice: metric ? 25 : 10
                    property bool wasMetric: metric
                    width: barPx + 16
                    height: Math.round(28 * app.grow)
                    color: Qt.alpha(app.theme.background, .9)
                    function nearest(raw) {
                        var best = steps[0], err = Math.abs(steps[0] - raw);
                        for (var i = 1; i < steps.length; i++) {
                            var e = Math.abs(steps[i] - raw);
                            if (e < err) { err = e; best = steps[i]; }
                        }
                        return best;
                    }
                    function stabilize() {
                        if (map.pixelsPerKm <= 0) return;
                        if (wasMetric !== metric) {
                            wasMetric = metric;
                            nice = metric ? 25 : 10;
                        }
                        var exactKm = barPx / map.pixelsPerKm;
                        var exact = metric ? exactKm : exactKm / kmPerMile;
                        // Stay on the current step while exact is closer to it
                        // than to its neighbours (wide band around each step).
                        if (Math.abs(exact - nice) <= nice * 0.35) return;
                        nice = nearest(exact);
                    }
                    Connections {
                        target: map
                        function onPixelsPerKmChanged() { scaleBar.stabilize() }
                        function onSpanChanged() { scaleBar.stabilize() }
                    }
                    onMetricChanged: stabilize()
                    Component.onCompleted: stabilize()
                    onVisibleChanged: if (visible) stabilize()
                    Item {
                        anchors.horizontalCenter: parent.horizontalCenter
                        anchors.verticalCenter: parent.verticalCenter
                        width: scaleBar.barPx
                        height: 16
                        LabelText {
                            anchors.horizontalCenter: parent.horizontalCenter
                            anchors.top: parent.top
                            text: {
                                var u = scaleBar.metric ? "km" : "mi";
                                var n = scaleBar.nice;
                                if (scaleBar.metric && n >= 1000) return (n / 1000) + "k " + u;
                                if (!scaleBar.metric && n < 1) return n + " " + u;
                                return n + " " + u;
                            }
                            font.pixelSize: app.theme.size.small
                            opacity: .75
                        }
                        Rectangle {
                            anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom
                            height: 1
                            color: Qt.alpha(app.theme.foreground, .65)
                        }
                        Rectangle {
                            anchors.left: parent.left; anchors.bottom: parent.bottom
                            width: 1; height: 5
                            color: Qt.alpha(app.theme.foreground, .65)
                        }
                        Rectangle {
                            anchors.right: parent.right; anchors.bottom: parent.bottom
                            width: 1; height: 5
                            color: Qt.alpha(app.theme.foreground, .65)
                        }
                    }
                }
                // What the rings mean (S29), above the scale bar.
                Rectangle {
                    id: ringCaptionBox
                    anchors.bottom: scaleBar.top; anchors.left: parent.left
                    anchors.leftMargin: 10; anchors.bottomMargin: 6
                    visible: map.ringNote !== ""
                    width: ringCaption.contentWidth + 10
                    height: ringCaption.contentHeight + 6
                    color: Qt.alpha(app.theme.background, .9)
                    LabelText {
                        id: ringCaption
                        x: 5; y: 3
                        width: Math.min(mapFrame.width * .6, 460)
                        wrapMode: Text.Wrap
                        text: map.ringNote.toUpperCase()
                        font.pixelSize: app.theme.size.small; opacity: .75
                    }
                }
                // OSM ODbL safe harbour: short credit in a map corner. Full
                // catalogue (NOAA, Natural Earth, GeoNames, …) stays in README.
                LabelText {
                    id: mapCredit
                    anchors.bottom: parent.bottom; anchors.right: parent.right; anchors.margins: 12
                    // The grey marks' positions credit their source (S23).
                    text: "© OpenStreetMap" + (engine.referenceSites.length ? " · other radars: EUMETNET" : "")
                    visible: !!app.scan
                    font.pixelSize: app.theme.size.small; opacity: .55
                }
                LabelText { anchors.centerIn: parent; width: parent.width-24; wrapMode: Text.Wrap; horizontalAlignment: Text.AlignHCenter; text: map.error || engine.error; visible: text.length > 0 && !(loadingCard.visible && !map.error) }
                // S41: while nothing is drawn yet, the load in steps over the
                // map; once a frame is on screen the timeline's bar says it.
                LoadingCard {
                    id: loadingCard
                    anchors.centerIn: parent
                    width: Math.min(implicitWidth, parent.width - 24)
                    engine: engine
                    theme: app.theme
                    compact: win.compact
                    visible: engine.busy && !engine.drawn
                }
            }
            // legend — colors for the map above
            ColumnLayout {
                id: legend
                Layout.fillWidth: true
                spacing: 4
                visible: !!app.scan && app.store.layers.radar
                // S46: a click enlarges it (legend zoom).
                TapHandler { onTapped: app.legendZoom = "radar" }
                HoverHandler { cursorShape: Qt.PointingHandCursor }
                Item {
                    Layout.fillWidth: true
                    implicitHeight: legendRow.implicitHeight
                    RowLayout {
                        id: legendRow
                        anchors.fill: parent; spacing: 0
                        Repeater {
                            model: app.scan ? app.scan.palette : []
                            ColumnLayout {
                                required property string modelData
                                required property int index
                                Layout.fillWidth: true; Layout.preferredWidth: 1; spacing: 4
                                Rectangle { Layout.fillWidth: true; height: 6; color: modelData }
                                // Number at the swatch's left edge; the unit sits at the far
                                // right of the last column so the strip spans the full width.
                                RowLayout {
                                    id: labelRow
                                    Layout.fillWidth: true; spacing: 0
                                    LabelText {
                                        id: number
                                        text: app.legendLabel(index)
                                        opacity: app.legendShown[index] ? 1 : 0
                                        font.pixelSize: app.theme.size.small
                                    }
                                    Item { Layout.fillWidth: true }
                                    LabelText {
                                        text: app.scan ? app.scan.units : ""
                                        font.pixelSize: app.theme.size.small
                                        visible: index===app.bands-1 && labelRow.width >= number.implicitWidth + implicitWidth + 8
                                    }
                                }
                            }
                        }
                    }
                    // The hidden part of the scale (DESIGN.md, weak-return floor):
                    // the swatches under the floor sink into the background with a
                    // tick at the floor, so the legend shows what the map leaves out.
                    Rectangle {
                        visible: app.floorFraction > 0
                        height: 6
                        width: Math.round(legendRow.width * app.floorFraction)
                        color: Qt.alpha(app.theme.background, .8)
                        Rectangle { anchors.right: parent.right; width: 1; height: parent.height; color: app.theme.foreground; opacity: .7 }
                    }
                }
            }
            // Timestamp and frame index above the ticks; transport alongside.
            RowLayout {
                Layout.fillWidth: true
                spacing: 12
                visible: !!app.scan
                RowLayout {
                    id: transport
                    Layout.alignment: Qt.AlignBottom
                    spacing: 5
                    GlyphButton { glyph: "first"; visible: !win.compact; enabled: app.frames.length > 1; onClicked: app.jump(false) }
                    GlyphButton { glyph: "back"; enabled: app.frames.length > 1; onClicked: app.step(-1) }
                    GlyphButton { glyph: app.playing ? "pause" : "play"; selected: app.playing; enabled: app.frames.length > 1; onClicked: app.togglePlay() }
                    GlyphButton { glyph: "fwd"; enabled: app.frames.length > 1; onClicked: app.step(1) }
                    GlyphButton { glyph: "last"; visible: !win.compact; enabled: app.frames.length > 1; onClicked: app.jump(true) }
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 4
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 8
                        LabelText {
                            id: stripStamp
                            Layout.fillWidth: !engine.loading
                            visible: !!app.scan && !!app.scan.scanTime
                            text: app.scan ? app.stamp(app.scan.scanTime) : ""
                            font.pixelSize: app.theme.size.small
                            opacity: .65
                            horizontalAlignment: Text.AlignLeft
                            elide: Text.ElideRight
                        }
                        // S31: what is loading and how far (state.loading).
                        // S41: the whole load's percentage, big enough to
                        // read, then the step by name and its count.
                        LabelText {
                            visible: !!engine.loading
                            text: engine.percent + " %"
                            font.pixelSize: app.theme.size.label
                            font.bold: true
                            color: app.theme.accent
                        }
                        LabelText {
                            Layout.fillWidth: true
                            visible: !!engine.loading
                            text: engine.activeStep ? engine.activeStep.name + (engine.activeStep.detail ? " · " + engine.activeStep.detail : "") : ""
                            font.pixelSize: app.theme.size.caption
                            color: app.theme.accent
                            horizontalAlignment: Text.AlignLeft
                            elide: Text.ElideRight
                        }
                        LabelText {
                            visible: !win.compact && app.frameIndex >= 0
                            horizontalAlignment: Text.AlignRight
                            text: (app.frameIndex + 1) + " / " + app.frames.length
                            font.pixelSize: app.theme.size.small
                            opacity: .65
                        }
                    }
                    Item {
                        id: strip
                        Layout.fillWidth: true
                        implicitHeight: 14
                        // S31: the load's progress, a thin bar under the
                        // ticks. S35: one segment per stage, the earlier
                        // ones staying full, named under the segments
                        // (the window has the room the popover has not).
                        LoadingBar {
                            id: loadingBar
                            loading: engine.loading
                            // S47: the weather layers' fetches when the
                            // radar has nothing loading.
                            layers: engine.layerLoads
                            theme: app.theme
                            names: !win.compact
                            // S41: 4 px, not S31's 2 px rule.
                            thickness: 4
                            anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom
                            // The bar starts 1 px below the strip, whatever
                            // the names add below it, so bar and names fit
                            // the window's 20 px margin.
                            anchors.bottomMargin: -1 - implicitHeight
                        }
                        Repeater {
                            model: app.slots
                            Rectangle {
                                required property var modelData
                                required property int index
                                readonly property bool current: !modelData.empty && index === app.currentSlot
                                readonly property bool tall: current || modelData.partial
                                x: app.slots.length > 1 ? Math.round(index * (strip.width - width) / (app.slots.length - 1)) : Math.round((strip.width - width) / 2)
                                y: Math.round((strip.height - height) / 2)
                                width: tall ? 3 : 2
                                height: modelData.empty ? 3 : tall ? 14 : 8
                                // Compact (no empty pads): even weight so a mid-loop
                                // playhead does not cliff into dimmer stubs.
                                color: current ? app.theme.accent : modelData.partial ? "transparent"
                                    : Qt.alpha(app.theme.foreground, modelData.empty ? .10
                                        : win.compact ? .40
                                        : index > app.currentSlot ? .28 : .42)
                                border.width: modelData.partial && !current ? 1 : 0
                                border.color: app.theme.accent
                            }
                        }
                        // Dragging scrubs: the nearest frame under the pointer shows at
                        // once (from the loop buffer when it holds it) and is sought once
                        // per frame change; a seek ends any loop.
                        MouseArea {
                            anchors.fill: parent
                            anchors.topMargin: -6
                            anchors.bottomMargin: -18
                            enabled: app.frames.length > 1
                            property string target: ""
                            function scrub(mx) {
                                var n = app.slots.length;
                                if (n < 2) return;
                                var i = Math.round(Math.max(0, Math.min(1, mx / strip.width)) * (n - 1));
                                if (app.slots[i].empty) return;
                                var id = app.slots[i].id;
                                if (id && id !== target) { target = id; engine.seekTo(id); }
                            }
                            onPressed: mouse => { target = ""; scrub(mouse.x); }
                            onPositionChanged: mouse => { if (pressed) scrub(mouse.x); }
                        }
                    }
                }
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: 5
                // MOCK: the bar is hidden. Radar moved to the header, the
                // place to the map, treatment and zoom to the keys and wheel.
                visible: false
                Chip {
                    glyph: "follow"
                    label: app.placeLabel
                    tag: app.placeState
                    on: app.mockGps === "following" || app.mockGps === "home"
                    tagAccent: on
                    onOpened: locationPicker.show("")
                }
                Chip {
                    glyph: "lock"
                    label: app.siteId || "—"
                    tag: app.locked ? "LOCKED" : "FOLLOWING"
                    on: app.locked
                    tagAccent: app.locked
                    enabled: !!app.state
                    onToggled: app.toggleLock()
                    onOpened: picker.show("")
                }
                Item { width: 6 }
                Item { Layout.fillWidth: true }
                // The product chip (S20): the engine's product for a live
                // radar; click opens the station's products and angles.
                Button {
                    id: productChip
                    visible: app.productRows.length > 0
                    implicitHeight: Math.round(30 * app.grow)
                    implicitWidth: contentItem.implicitWidth + 18
                    padding: 0
                    opacity: productMenu.opened || hovered || activeFocus ? 1 : .7
                    onClicked: productMenu.opened ? productMenu.close() : productMenu.show()
                    contentItem: RowLayout {
                        spacing: 6
                        Item { Layout.fillWidth: true }
                        LabelText { text: app.productLabel }
                        Glyph { glyph: "chevron"; implicitWidth: 12 }
                        Item { Layout.fillWidth: true }
                    }
                    background: Rectangle {
                        color: productMenu.opened || productChip.hovered || productChip.activeFocus ? Qt.alpha(app.theme.accent, .18) : "transparent"
                        border.width: 1
                        border.color: productMenu.opened || productChip.activeFocus ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
                    }
                }
                Item { width: 6; visible: productChip.visible }
                // The treatment chip (DESIGN.md, treatment control): one
                // low-emphasis control naming the treatment; click opens the
                // three in the picker's row style above it, 1 2 3 choose.
                Button {
                    id: treatmentChip
                    implicitHeight: Math.round(30 * app.grow)
                    implicitWidth: contentItem.implicitWidth + 18
                    padding: 0
                    opacity: treatmentMenu.opened || hovered || activeFocus ? 1 : .7
                    onClicked: treatmentMenu.opened ? treatmentMenu.close() : treatmentMenu.show()
                    contentItem: RowLayout {
                        spacing: 6
                        Item { Layout.fillWidth: true }
                        LabelText { text: app.treatment }
                        Glyph { glyph: "chevron"; implicitWidth: 12 }
                        Item { Layout.fillWidth: true }
                    }
                    background: Rectangle {
                        color: treatmentMenu.opened || treatmentChip.hovered || treatmentChip.activeFocus ? Qt.alpha(app.theme.accent, .18) : "transparent"
                        border.width: 1
                        border.color: treatmentMenu.opened || treatmentChip.activeFocus ? app.theme.accent : Qt.alpha(app.theme.foreground, .22)
                    }
                }
                Rectangle { width: 1; height: 18; color: Qt.alpha(app.theme.foreground, .22); Layout.leftMargin: 4; Layout.rightMargin: 4; visible: !win.compact }
                Control { text: "−"; visible: !win.compact; onClicked: map.zoom(Math.min(map.span,map.maxSpan)*1.25) }
                Control { text: "+"; visible: !win.compact; onClicked: map.zoom(Math.min(map.span,map.maxSpan)/1.25) }
            }
          }
          // The site picker over everything, its card's top on the map's.
          SitePicker {
            id: picker
            anchors.fill: parent
            sites: engine.sites
            theme: app.theme
            centerLat: map.centerLat
            centerLon: map.centerLon
            homeSite: ""
            compact: win.compact
            cardTop: layout.anchors.margins + mapFrame.y
            // S46: it drops down under the site name that opens it.
            anchorItem: siteTitle
            onChosen: site => app.choose(site)
          }
          LocationPicker {
            id: locationPicker
            session: app.store
            onManualStarted: app.store.cancelIpLocation()
            anchors.fill: parent
            theme: app.theme
            engine: engine
            closeOnScrim: !app.store.needsLocation
            centerLat: map.centerLat
            centerLon: map.centerLon
            compact: win.compact
            cardTop: layout.anchors.margins + mapFrame.y
            onChosen: (lat, lon, name) => {
                app.store.setPlace(lat, lon, name);
                app.applyView();
                app.notice = name ? "LOCATION · " + name.toUpperCase() : "LOCATION · " + lat.toFixed(4) + ", " + lon.toFixed(4);
                noticeTimer.restart();
            }
          }
          // My mosaic's panel (S25, docked since S40): the right side of the
          // map stage at its full height, the map shrunk to its left and in
          // pick mode (S39); over the map in a compact window.
          // S40 review 4: an overlay opened over My mosaic's panel (the keys
          // sheet from `?`) hands the keyboard back to it when it closes.
          Connections {
            target: sheet
            function onOpenChanged() { if (!sheet.open && mosaicPicker.open) mosaicPicker.takeKeys(); }
          }
          Connections {
            target: picker
            function onOpenChanged() { if (!picker.open && mosaicPicker.open) Qt.callLater(mosaicPicker.takeKeys); }
          }
          Connections {
            target: locationPicker
            function onOpenChanged() { if (!locationPicker.open && mosaicPicker.open) mosaicPicker.takeKeys(); }
          }
          Connections {
            target: productMenu
            function onOpenedChanged() { if (!productMenu.opened && mosaicPicker.open) mosaicPicker.takeKeys(); }
          }
          Connections {
            target: treatmentMenu
            function onOpenedChanged() { if (!treatmentMenu.opened && mosaicPicker.open) mosaicPicker.takeKeys(); }
          }
          Connections {
            target: mapMenu
            function onOpenedChanged() { if (!mapMenu.opened && mosaicPicker.open) Qt.callLater(mosaicPicker.takeKeys); }
          }
          Connections {
            target: app
            function onLegendZoomChanged() { if (app.legendZoom === "" && mosaicPicker.open) mosaicPicker.takeKeys(); }
          }
          MosaicPicker {
            id: mosaicPicker
            x: layout.x + mapFrame.x + (docked ? mapFrame.width + 10 : 0)
            y: layout.y + mapFrame.y
            width: docked ? panelWidth : mapFrame.width
            height: mapFrame.height
            engine: engine
            theme: app.theme
            store: app.store.mosaicStore
            centerLat: map.centerLat
            centerLon: map.centerLon
            compact: win.compact
            stageWidth: layout.width
            onShown: set => app.showMosaic(set)
            onCentre: site => { app.mosaicNavigated = true; map.lookAt(site.lat, site.lon); }
            onCancelled: {
                if (app.mosaicView && !app.mosaicNavigated) {
                    map.lookAt(app.mosaicView.lat, app.mosaicView.lon);
                    map.span = app.mosaicView.span;
                }
                app.mosaicView = null;
                map.reportedLat = NaN;
                Qt.callLater(map.reportCenter);
            }
            onOpenChanged: if (open) {
                app.mosaicView = {lat: map.centerLat, lon: map.centerLon, span: map.span};
                app.mosaicNavigated = false;
                Qt.callLater(app.keepInView, hotId, 0);
            }
            onCursorMoved: id => Qt.callLater(app.keepInView, id)
            onHelpRequested: sheet.show()
            // The cursor's radar stays on the map: off screen, the map
            // centres on it.
          }
          // The treatment menu over the surface (not a Popup, which the
          // window overlay would draw outside the captured surface): a card
          // above the chip's right edge in the picker's row style, the
          // current and the hovered row in accent, each row's key at the
          // right. A click outside, Escape, a treatment key, or a choice
          // closes it; Up, Down, and Return choose from the keyboard.
          // S42: the layers panel, a card below the LAYERS chip.
          LayersPanel {
            id: layersPanel
            anchors.fill: parent
            theme: app.theme
            store: app.store.layers
            chip: layersChip
            obs: engine.obs
            grid: engine.grid
            lightning: engine.lightning
            lightningInfo: engine.lightningInfo
            strikesShown: lightningLayer.shownCounts[0] + lightningLayer.shownCounts[1]
            keyText: (app.bindings.layers || []).map(KeyMap.pretty).join(" ")
            // S47: each layer's loading or failure, and Reset.
            stationsLayer: engine.stationsLayer
            gridLayer: engine.gridLayer
            lightningLayer: engine.lightningLayer
            onResetRequested: app.resetAll()
          }
          Item {
            id: treatmentMenu
            anchors.fill: parent
            property bool opened: false
            property int cursor: -1
            visible: opened
            focus: opened
            function show() { cursor = -1; opened = true; forceActiveFocus(); }
            function close() { opened = false; }
            Keys.onPressed: event => {
                if (!opened) return;
                event.accepted = true;
                if (event.key === Qt.Key_Escape) close();
                else if (event.key === Qt.Key_Up) cursor = Math.max(0, (cursor < 0 ? KeyMap.TREATMENTS.indexOf(app.treatment) : cursor) - 1);
                else if (event.key === Qt.Key_Down) cursor = Math.min(KeyMap.TREATMENTS.length - 1, (cursor < 0 ? KeyMap.TREATMENTS.indexOf(app.treatment) : cursor) + 1);
                else if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter) && cursor >= 0) app.run(KeyMap.TREATMENTS[cursor].toLowerCase());
                else event.accepted = false;
            }
            MouseArea { anchors.fill: parent; onClicked: treatmentMenu.close() }
            Rectangle {
                id: treatmentCard
                readonly property point anchor: treatmentMenu.opened ? treatmentChip.mapToItem(treatmentMenu, treatmentChip.width, 0) : Qt.point(0, 0)
                x: Math.round(anchor.x - width)
                y: Math.round(anchor.y - height - 6)
                width: Math.round(168 * app.grow)
                height: treatmentRows.implicitHeight + 12
                color: Qt.alpha(app.theme.background, .95)
                border.width: 1
                border.color: app.theme.foreground
                MouseArea { anchors.fill: parent } // a click on the card stays on the card
                ColumnLayout {
                    id: treatmentRows
                    anchors.fill: parent
                    anchors.margins: 6
                    spacing: 0
                    Repeater {
                        model: KeyMap.TREATMENTS
                        Rectangle {
                            id: treatmentRow
                            required property string modelData
                            required property int index
                            readonly property bool current: app.treatment === modelData
                            readonly property bool hot: rowArea.containsMouse || treatmentMenu.cursor === index
                            readonly property color ink: current || hot ? app.theme.accent : app.theme.foreground
                            Layout.fillWidth: true
                            implicitHeight: Math.round(28 * app.grow)
                            color: hot ? Qt.alpha(app.theme.foreground, .08) : "transparent"
                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 10
                                anchors.rightMargin: 10
                                LabelText { text: treatmentRow.modelData; color: treatmentRow.ink; Layout.fillWidth: true }
                                LabelText {
                                    text: (app.bindings[treatmentRow.modelData.toLowerCase()] || []).map(KeyMap.pretty).join(" ")
                                    color: treatmentRow.ink; font.pixelSize: app.theme.size.small; opacity: .6
                                }
                            }
                            MouseArea { id: rowArea; anchors.fill: parent; hoverEnabled: true; onClicked: app.run(treatmentRow.modelData.toLowerCase()) }
                        }
                    }
                }
            }
          }
          // The product menu (S20), like the treatment menu: a card above
          // the product chip, the current and the hovered row in accent; a
          // click outside or Escape closes it, Up, Down and Return choose.
          Item {
            id: productMenu
            // A − / + / FULL step in the menu (S29).
            component MenuStep: Rectangle {
                id: stepButton
                property string label
                signal activated()
                implicitWidth: Math.max(Math.round(22 * app.grow), stepLabel.implicitWidth + 10)
                implicitHeight: Math.round(20 * app.grow)
                color: stepArea.containsMouse && enabled ? Qt.alpha(app.theme.accent, .18) : "transparent"
                border.width: 1
                border.color: Qt.alpha(app.theme.foreground, enabled ? .3 : .12)
                opacity: enabled ? 1 : .4
                LabelText { id: stepLabel; anchors.centerIn: parent; text: stepButton.label; font.pixelSize: app.theme.size.small }
                MouseArea { id: stepArea; anchors.fill: parent; hoverEnabled: true; enabled: stepButton.enabled; onClicked: stepButton.activated() }
            }
            anchors.fill: parent
            property bool opened: false
            property int cursor: -1
            visible: opened && app.productRows.length > 0
            focus: opened
            function show() { cursor = -1; opened = true; forceActiveFocus(); }
            function close() { opened = false; }
            Keys.onPressed: event => {
                if (!opened) return;
                event.accepted = true;
                var n = app.productRows.length;
                if (event.key === Qt.Key_Escape) close();
                else if (event.key === Qt.Key_Up) cursor = Math.max(0, (cursor < 0 ? 0 : cursor) - 1);
                else if (event.key === Qt.Key_Down) cursor = Math.min(n - 1, cursor + 1);
                else if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter) && cursor >= 0 && cursor < n) app.chooseProduct(app.productRows[cursor]);
                else event.accepted = false;
            }
            MouseArea { anchors.fill: parent; onClicked: productMenu.close() }
            Rectangle {
                // Above the product chip when it shows; else, the chip row
                // being hidden, below the header's product line, which
                // opens it (S29).
                readonly property bool fromChip: productChip.visible
                readonly property point anchor: !productMenu.opened ? Qt.point(0, 0)
                    : fromChip ? productChip.mapToItem(productMenu, productChip.width, 0)
                    : productText.mapToItem(productMenu, productText.width, productText.height)
                x: Math.max(4, Math.round(anchor.x - width))
                y: Math.max(4, Math.round(fromChip ? anchor.y - height - 6 : anchor.y + 6))
                width: Math.round(214 * app.grow)
                height: productRowsColumn.implicitHeight + 12
                color: Qt.alpha(app.theme.background, .95)
                border.width: 1
                border.color: app.theme.foreground
                MouseArea { anchors.fill: parent } // a click on the card stays on the card
                ColumnLayout {
                    id: productRowsColumn
                    anchors.fill: parent
                    anchors.margins: 6
                    spacing: 0
                    Repeater {
                        model: app.productRows
                        Rectangle {
                            id: productRow
                            required property var modelData
                            required property int index
                            readonly property var st: engine.state
                            readonly property bool current: !!st && !!st.product && st.product.id === modelData.product && st.product.elevationIndex === modelData.index
                            readonly property bool hot: productArea.containsMouse || productMenu.cursor === index
                            readonly property color ink: current || hot ? app.theme.accent : app.theme.foreground
                            // The first angle row opens the advanced part.
                            readonly property bool firstAngle: modelData.note !== "" && (index === 0 || app.productRows[index - 1].note === "")
                            Layout.fillWidth: true
                            implicitHeight: Math.round((24 + (firstAngle ? 18 : 0)) * app.grow)
                            color: hot ? Qt.alpha(app.theme.foreground, .08) : "transparent"
                            LabelText {
                                visible: productRow.firstAngle
                                anchors.left: parent.left; anchors.leftMargin: 10; anchors.top: parent.top; anchors.topMargin: 4
                                text: "SCAN ANGLE · BEAM 50 / 100 KM"; font.pixelSize: app.theme.size.small; opacity: .55
                            }
                            RowLayout {
                                anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom
                                height: Math.round(24 * app.grow)
                                anchors.leftMargin: 10
                                anchors.rightMargin: 10
                                LabelText { text: productRow.modelData.label; color: productRow.ink; Layout.fillWidth: true }
                                LabelText { text: productRow.modelData.note; color: productRow.ink; font.pixelSize: app.theme.size.small; opacity: .6 }
                            }
                            MouseArea { id: productArea; anchors.fill: parent; hoverEnabled: true; onClicked: app.chooseProduct(productRow.modelData) }
                        }
                    }
                    // S29: Height's height above sea level, with its steps.
                    ColumnLayout {
                        Layout.fillWidth: true
                        Layout.topMargin: 4
                        Layout.leftMargin: 10; Layout.rightMargin: 10
                        visible: app.heightM > 0
                        spacing: 2
                        LabelText {
                            Layout.fillWidth: true
                            wrapMode: Text.Wrap
                            text: (app.chosenAbove === "ground" ? "HEIGHT ABOVE THE GROUND UNDER EACH POINT" : "HEIGHT ABOVE SEA")
                                + " · THE BEAM IS 1.7 KM THICK AT 100 KM, SO FINER STEPS REPEAT FAR OUT"
                            font.pixelSize: app.theme.size.small; opacity: .55
                        }
                        RowLayout {
                            Layout.fillWidth: true
                            Layout.preferredHeight: Math.round(24 * app.grow)
                            spacing: 4
                            LabelText { text: app.heightM / 1000 + " KM"; color: app.theme.accent; Layout.fillWidth: true }
                            MenuStep { label: "−"; enabled: app.heightM > 500; onActivated: app.stepHeight(-1) }
                            MenuStep { label: "+"; enabled: app.heightM < 12000; onActivated: app.stepHeight(1) }
                        }
                        // S30: measured from sea level or from the ground.
                        RowLayout {
                            Layout.fillWidth: true
                            Layout.preferredHeight: Math.round(24 * app.grow)
                            spacing: 4
                            visible: app.groundOffered
                            LabelText { text: "FROM"; opacity: .6; Layout.fillWidth: true }
                            MenuStep { label: "SEA"; enabled: app.chosenAbove !== "sea"; onActivated: app.setAbove("sea") }
                            MenuStep { label: "GROUND"; enabled: app.chosenAbove !== "ground"; onActivated: app.setAbove("ground") }
                        }
                    }
                    // S24d: Relief, while a storm height shows.
                    ColumnLayout {
                        Layout.fillWidth: true
                        Layout.topMargin: 4
                        Layout.leftMargin: 10; Layout.rightMargin: 10
                        visible: !!app.scan && app.scan.product === "ETOP"
                        spacing: 2
                        LabelText {
                            Layout.fillWidth: true
                            wrapMode: Text.Wrap
                            text: "RELIEF · THE TOPS AS A SURFACE LIT FROM THE NORTH-WEST"
                            font.pixelSize: app.theme.size.small; opacity: .55
                        }
                        RowLayout {
                            Layout.fillWidth: true
                            Layout.preferredHeight: Math.round(24 * app.grow)
                            spacing: 4
                            LabelText { text: app.relief ? "ON" : "OFF"; color: app.theme.accent; Layout.fillWidth: true }
                            MenuStep { label: "ON"; enabled: !app.relief; onActivated: app.relief = true }
                            MenuStep { label: "OFF"; enabled: app.relief; onActivated: app.relief = false }
                        }
                    }
                }
            }
          }
          // S46: an enlarged legend over a clear scrim: any click (on the
          // legend again or outside it) or Escape shrinks it. The radar's
          // scale is drawn here as a card over the map's foot; the
          // temperature/wind legend and the strikes' key grow in place.
          Item {
            id: legendZoomLayer
            anchors.fill: parent
            visible: app.legendZoom !== ""
            focus: visible
            onVisibleChanged: if (visible) forceActiveFocus()
            Keys.onPressed: event => { if (event.key === Qt.Key_Escape) { app.legendZoom = ""; event.accepted = true; } }
            MouseArea { anchors.fill: parent; acceptedButtons: Qt.AllButtons; onPressed: app.legendZoom = "" }
            TextMetrics { id: zoomMetrics; font.family: app.theme.font; font.pixelSize: app.theme.size.small * 2; text: "0" }
            Rectangle {
                id: legendZoomCard
                visible: app.legendZoom === "radar" && !!app.scan
                width: Math.min(mapFrame.width - 24, Math.round(760 * app.grow))
                height: zoomColumn.implicitHeight + 24
                x: Math.round(layout.x + mapFrame.x + (mapFrame.width - width) / 2)
                y: Math.round(layout.y + mapFrame.y + mapFrame.height - height - 12)
                color: Qt.alpha(app.theme.background, .96)
                border.width: 1
                border.color: app.theme.foreground
                ColumnLayout {
                    id: zoomColumn
                    x: 12; y: 12
                    width: parent.width - 24
                    spacing: 8
                    RowLayout {
                        Layout.fillWidth: true
                        LabelText {
                            Layout.fillWidth: true
                            text: app.scan ? app.scan.productName.toUpperCase() : ""
                            font.pixelSize: app.theme.size.title; font.bold: true; font.letterSpacing: 1
                        }
                        LabelText { text: app.scan ? app.scan.units : ""; font.pixelSize: app.theme.size.title; color: app.theme.accent }
                    }
                    Item {
                        Layout.fillWidth: true
                        implicitHeight: zoomRow.implicitHeight
                        RowLayout {
                            id: zoomRow
                            anchors.left: parent.left; anchors.right: parent.right
                            spacing: 0
                            Repeater {
                                model: app.legendZoom === "radar" && app.scan ? app.scan.palette : []
                                ColumnLayout {
                                    required property string modelData
                                    required property int index
                                    Layout.fillWidth: true; Layout.preferredWidth: 1; spacing: 6
                                    Rectangle { Layout.fillWidth: true; height: Math.round(16 * app.grow); color: modelData }
                                    LabelText {
                                        text: app.legendLabel(index)
                                        opacity: app.legendZoomShown[index] ? 1 : 0
                                        font.pixelSize: app.theme.size.small * 2
                                    }
                                }
                            }
                        }
                        // The weak-return floor, as on the strip.
                        Rectangle {
                            visible: app.floorFraction > 0
                            height: Math.round(16 * app.grow)
                            width: Math.round(zoomRow.width * app.floorFraction)
                            color: Qt.alpha(app.theme.background, .8)
                            Rectangle { anchors.right: parent.right; width: 2; height: parent.height; color: app.theme.foreground; opacity: .7 }
                        }
                    }
                    LabelText {
                        Layout.fillWidth: true
                        visible: app.floorFraction > 0
                        text: "SHADED: UNDER THE WEAK-RETURN FLOOR, NOT DRAWN (W)"
                        font.pixelSize: app.theme.size.small; opacity: .6
                    }
                }
            }
          }
          // S46: the map's right-click menu, over everything but the sheet.
          MapMenu {
            id: mapMenu
            anchors.fill: parent
            theme: app.theme
            rows: app.mapMenuRows
          }
          // The `?` sheet over everything, below the map's top edge.
          KeysSheet {
            id: sheet
            anchors.fill: parent
            theme: app.theme
            bindings: app.bindings
            compact: win.compact
            cardTop: layout.anchors.margins + mapFrame.y
          }
        }
        // Opt-in capture uses the actual QML scene at a fixed size, without a compositor.
        Timer {
            interval: Number(Quickshell.env("OMASTORM_CAPTURE_DELAY")) || 2500; running: !app.session && !!Quickshell.env("OMASTORM_CAPTURE"); repeat: false
            onTriggered: surface.grabToImage(result => { result.saveToFile(Quickshell.env("OMASTORM_CAPTURE")); Qt.quit(); })
        }
        // S47: a capture in the middle of a run (a load, then Reset), for
        // the captures that need several: quickshell ipc call capture save <png>.
        IpcHandler {
            target: "capture"
            enabled: !app.session
            function save(path: string): void { surface.grabToImage(result => result.saveToFile(path)); }
        }
    }
}
