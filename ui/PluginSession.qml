pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io
import "Location.js" as Location
import "Keys.js" as KeyMap
import "Mosaic.js" as Mosaic

QtObject {
    id: session
    // One non-map connection keeps the bar current, including on multiple
    // outputs. Visible maps have separate sockets for their tile rectangles.
    property Engine engine: Engine {}
    property Config config: Config {}
    property Remembered remembered: Remembered {}
    /// My mosaic's last set (S25), sent again to an engine that has none.
    property MosaicStore mosaicStore: MosaicStore {}
    /// The weather layers' switches (S42), shared by the window and popover.
    property LayersStore layers: LayersStore {}
    property bool mosaicResent: false
    // S48: no `theme` IPC target from the plugin: the Omarchy shell hosts
    // other plugins that register it (upstream Omastorm), and the hook only
    // ever calls the standalone window's (Theme.qml).
    property Theme theme: Theme { registerIpc: false }
    property bool windowOpen: false
    // The station whose loop this process's surfaces play from their own
    // buffers (Engine.qml, loop buffer), "" for none: play in the popover
    // plays the window too, and expand keeps playing, without the engine.
    property string loopSite: ""
    property bool initialized: false
    property string treatment: Quickshell.env("OMASTORM_STYLE") || "GLYPHS"
    // S24d: Relief (a storm height lit as a surface), shared by the window and
    // the popover; OMASTORM_RELIEF=1 turns it on for checks and captures.
    property bool relief: Quickshell.env("OMASTORM_RELIEF") === "1"
    // The weak-return floor in dBZ, or null for every measured return
    // (DESIGN.md, weak-return floor); config.toml's weak_floor and the `w`
    // key change it, OMASTORM_WEAK outranks the file for captures.
    property var weakFloor: KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) !== undefined ? KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) : KeyMap.DEFAULT_FLOOR
    property string startupError: ""
    readonly property bool ready: config.ready && remembered.ready
    readonly property string persistError: remembered.error ? remembered.error.toUpperCase() : ""
    property bool hasView: false
    property bool needsLocation: false
    property string placeName: ""
    property string locationSource: ""
    property bool ipLocationDismissed: true
    property bool locationPending: false
    property string locationError: ""
    property int locateAttempt: 0
    property int activeAttempt: 0
    readonly property bool locating: needsLocation && !ipLocationDismissed && locationPending
    property real centerLat: 0
    property real centerLon: 0
    property real span: Location.DEFAULT_SPAN
    property string lockId: ""
    property bool lockWanted: false
    property string lockSource: ""
    property string lastConfigLock: ""
    property bool pendingLocationPicker: false
    property var appliedExplicit: null
    signal viewChanged()
    signal locationPickerRequested()

    function requestLocationPicker() {
        cancelIpLocation();
        pendingLocationPicker = true;
        locationPickerRequested();
    }

    function cancelIpLocation() {
        ipLocationDismissed = true;
        locationPending = false;
        locateAttempt += 1;
        locator.queued = false;
        if (locator.running) locator.running = false;
    }

    function userNavigated(lat, lon, spanKm) {
        if (!Location.validPair(lat, lon)) return;
        if (needsLocation) {
            centerLat = lat;
            centerLon = lon;
            span = Location.clampSpan(spanKm);
            locationSource = "state";
            hasView = true;
            needsLocation = false;
            persist();
            applyRadar();
        }
        cancelIpLocation();
    }

    // One-shot wttr.in estimate (DESIGN.md). UI curl — not the engine.
    function requestIpLocation() {
        if (!initialized || !ready || hasView || !needsLocation
            || locationPending || !engine.state || engine.state.source !== "live") return;
        locateAttempt += 1;
        activeAttempt = locateAttempt;
        ipLocationDismissed = false;
        locationError = "";
        locationPending = true;
        var url = Quickshell.env("OMASTORM_LOCATION_URL") || "https://wttr.in/?format=j2";
        // The reply is capped before QML holds it: curl stops past the cap
        // (exit 63) and head cuts the stream for a curl that would not, so a
        // longer reply takes the network-error path, never a partial parse.
        // The URL, agent and cap are arguments, never shell text.
        locator.command = ["bash", "-c",
            'set -o pipefail; curl -fsS --max-time 10 --max-filesize "$3" -A "$2" -- "$1" | head -c "$(($3 + 1))"',
            "omastorm-locate", url, "omastorm-nord (fork of https://omastorm.com)", String(locateCap)];
        // Bind the attempt to this launch. If a prior curl is still dying after
        // cancel, queue one restart instead of overwriting its exit attribution.
        locator.attempt = locateAttempt;
        if (locator.running) {
            locator.queued = true;
            return;
        }
        locator.queued = false;
        locator.running = true;
    }

    function acceptIpLocation(place) {
        if (!ready || hasView || ipLocationDismissed || !place
            || !engine.state || engine.state.source !== "live") {
            locationPending = false;
            return;
        }
        // Recheck sources that may have arrived while the lookup was pending.
        resolve();
        if (hasView) {
            locationPending = false;
            return;
        }
        centerLat = place.lat;
        centerLon = place.lon;
        placeName = place.name || "";
        locationSource = "ip";
        span = Location.clampSpan(remembered.span);
        hasView = true;
        needsLocation = false;
        locationPending = false;
        persist();
        viewChanged();
        applyRadar();
    }

    function finishIpLocation(exitCode, raw, attempt) {
        // A cancelled or superseded curl can still report; ignore it.
        if (attempt !== undefined && attempt !== locateAttempt) return;
        if (ipLocationDismissed || hasView || !needsLocation) {
            locationPending = false;
            return;
        }
        if (exitCode !== 0) {
            locationError = "Couldn’t find your location. Try again or choose manually.";
            locationPending = false;
            viewChanged();
            return;
        }
        var place = Location.parseWttrHome(raw);
        if (!place) {
            locationError = "Couldn’t find your location. Try again or choose manually.";
            locationPending = false;
            viewChanged();
            return;
        }
        acceptIpLocation(place);
    }

    // A format=j2 reply is a few KB (no hourly forecast); 64 KiB leaves room
    // and still bounds what the bar process holds.
    readonly property int locateCap: 65536
    property Process locator: Process {
        property int attempt: 0
        property bool queued: false
        command: ["true"]
        stdout: StdioCollector { waitForEnd: true }
        onExited: function (exitCode) {
            // A terminate-then-retry left the old curl running; start the queued
            // launch and ignore this exit's stdout/code.
            if (queued) {
                queued = false;
                if (!session.ipLocationDismissed && session.locationPending
                    && attempt === session.locateAttempt) {
                    running = true;
                    return;
                }
                return;
            }
            // More than the cap got through only past a curl that ignores
            // --max-filesize; fail it as curl would (63).
            var over = !!stdout.data && stdout.data.byteLength > session.locateCap;
            session.finishIpLocation(over ? 63 : exitCode, over ? "" : String(stdout.text || ""), attempt);
        }
    }

    function resolve() {
        if (!config.ready || !remembered.ready) return;
        var env = Location.envView(Quickshell.env("OMASTORM_VIEW"));
        var explicit = Location.configCenter(config.values);
        var rememberedView = remembered.parsed;
        var place = Location.resolvePlace(explicit, rememberedView, config.location, env);
        if (!hasView) {
            if (place) {
                needsLocation = false;
                centerLat = place.lat;
                centerLon = place.lon;
                span = Location.clampSpan(place.span);
                hasView = true;
                locationSource = place.source;
                placeName = place.name || "";
            } else {
                needsLocation = true;
                locationSource = "";
                placeName = "";
            }
            applyLaunchLock(rememberedView);
        }
        if (explicit) {
            var same = appliedExplicit && appliedExplicit.lat === explicit.lat && appliedExplicit.lon === explicit.lon;
            appliedExplicit = explicit;
            if (hasView && !same) {
                placeName = "";
                locationSource = "config";
                centerLat = explicit.lat;
                centerLon = explicit.lon;
                needsLocation = false;
            }
        } else {
            appliedExplicit = null;
        }
        applyConfigLockChange();
        viewChanged();
    }

    function applyLaunchLock(rememberedView) {
        var cfg = Location.configLock(config.values);
        lastConfigLock = cfg;
        if (cfg) {
            lockId = cfg;
            lockWanted = true;
            lockSource = "config";
        } else if (rememberedView && rememberedView.lock) {
            lockId = rememberedView.lock;
            lockWanted = true;
            lockSource = "state";
        } else {
            lockId = "";
            lockWanted = false;
            lockSource = "nearest";
        }
    }

    function applyConfigLockChange() {
        var cfg = Location.configLock(config.values);
        if (cfg === lastConfigLock) return;
        lastConfigLock = cfg;
        if (cfg) {
            lockId = cfg;
            lockWanted = true;
            lockSource = "config";
        } else {
            lockId = remembered.lock || "";
            lockWanted = !!lockId;
            lockSource = lockWanted ? "state" : "nearest";
        }
    }

    function persist() {
        if (!hasView) return;
        remembered.snapshot(centerLat, centerLon, span, lockWanted ? lockId : "", placeName);
    }

    function rememberView(lat, lon, spanKm) {
        if (needsLocation) return;
        if (!Location.validPair(lat, lon)) return;
        var next = Location.clampSpan(spanKm);
        // Mercator round-trip after applyView can report 35.39999999999999
        // for a pick of 35.4 (#32). Keep the stored centre; only take span.
        var sameCenter = hasView
            && Math.abs(centerLat - lat) < 1e-6
            && Math.abs(centerLon - lon) < 1e-6;
        if (sameCenter && span === next) return;
        if (!sameCenter) {
            centerLat = lat;
            centerLon = lon;
        }
        span = next;
        hasView = true;
        persistTimer.restart();
    }

    function setPlace(lat, lon, name) {
        if (!Location.validPair(lat, lon)) return;
        cancelIpLocation();
        placeName = name || "";
        locationSource = "state";
        needsLocation = false;
        pendingLocationPicker = false;
        centerLat = lat;
        centerLon = lon;
        span = Location.DEFAULT_SPAN;
        hasView = true;
        var cfg = Location.configLock(config.values);
        if (cfg && lockSource === "config") {
            lockId = cfg;
            lockWanted = true;
            lockSource = "config";
        } else {
            lockId = "";
            lockWanted = false;
            lockSource = "nearest";
        }
        persist();
        viewChanged();
        applyRadar();
    }

    function resetView() {
        var target = Location.resolveReset(Location.configCenter(config.values), config.location);
        if (target) {
            centerLat = target.lat;
            centerLon = target.lon;
            locationSource = target.source;
            placeName = target.name || "";
        } else if (!hasView) {
            requestLocationPicker();
            return;
        }
        span = Location.DEFAULT_SPAN;
        hasView = true;
        persist();
        viewChanged();
        applyRadar();
    }

    function chooseRadar(id, lat, lon, name) {
        lat = Number(lat);
        lon = Number(lon);
        if (!id || !Location.validPair(lat, lon)) return;
        cancelIpLocation();
        placeName = name || id;
        locationSource = "state";
        needsLocation = false;
        pendingLocationPicker = false;
        centerLat = lat;
        centerLon = lon;
        hasView = true;
        lockId = id;
        lockWanted = true;
        lockSource = "state";
        persist();
        viewChanged();
        applyRadar();
    }

    function setLock(id, on) {
        if (on && id) {
            lockId = id;
            lockWanted = true;
            lockSource = "state";
        } else {
            lockId = "";
            lockWanted = false;
            lockSource = "nearest";
        }
        persist();
        applyRadar();
    }

    function followNearest(id) {
        lockId = "";
        lockWanted = false;
        lockSource = "nearest";
        persist();
        if (!engine.state) return;
        if (engine.state.site.locked) engine.send({type: "lock", enabled: false});
        if (!engine.state.site.follow) engine.send({type: "follow", enabled: true});
        if (id && (engine.state.site.id !== id || engine.state.source !== "live"))
            engine.send({type: "select_site", id: id});
    }

    // Whether the engine's station is the one `id` names. A configured or
    // remembered lock may be an alias or another case (`sevax` is Vara,
    // DEC-12); the engine answers with the canonical id, so a plain compare
    // would re-send select_site on every navigation.
    function isStation(id) {
        var current = engine.state.site.id, want = String(id).toLowerCase();
        if (id === current) return true;
        var s = engine.sites.find(s => s.id === current);
        return !!s && (s.id.toLowerCase() === want || (s.aliases || []).some(a => a.toLowerCase() === want));
    }

    function applyRadar() {
        if (!engine.state || !ready) return;
        if (needsLocation) return;
        if (lockWanted && lockId) {
            if (!isStation(lockId) || engine.state.source !== "live")
                engine.send({type: "select_site", id: lockId});
            if (!engine.state.site.locked) engine.send({type: "lock", enabled: true});
            if (!engine.state.site.follow) engine.send({type: "follow", enabled: true});
        } else {
            if (engine.state.site.locked) engine.send({type: "lock", enabled: false});
            if (!engine.state.site.follow) engine.send({type: "follow", enabled: true});
            if (hasView) engine.send({type: "view_center", lat: centerLat, lon: centerLon});
        }
    }

    function initialize() {
        if (initialized || !engine.state || !ready) return;
        initialized = true;
        resolve();
        applyRadar();
        persist();
    }

    function applyTreatment() {
        var errors = [], wanted = KeyMap.treatment(config.treatment, errors);
        if (!Quickshell.env("OMASTORM_STYLE") && wanted) treatment = wanted;
        if (KeyMap.envFloor(Quickshell.env("OMASTORM_WEAK")) === undefined) weakFloor = KeyMap.weakFloor(config.weakFloor, errors);
    }

    // My mosaic (S25): keep the engine's set; give an engine with none (a
    // restart) the one kept, once per connection. An engine older than S25
    // sends no hello.mosaic, and nothing happens.
    function keepMosaic() {
        var st = engine.state;
        if (!st || !engine.mosaic || !st.mosaic || !Array.isArray(st.mosaic.sites)) return;
        if (st.mosaic.sites.length) { mosaicStore.keep(st.mosaic); return; }
        if (mosaicResent || !Mosaic.valid(mosaicStore.set) || !engine.sites.length) return;
        var command = Mosaic.command(mosaicStore.set, engine.sites);
        if (!command.sites.length) return;
        mosaicResent = true;
        engine.send(command);
    }
    // Review S3 (S24b): a composite's product (sweden, nordic) makes the
    // engine read every radar's volumes, and this session's connection alone
    // would keep it polled with nothing on screen. Once the popover and the
    // window have both been closed for a few seconds, the composite goes back
    // to REF; the product is chosen again when a surface opens on that
    // station. A popover counts itself (Popover.qml); the window sets
    // windowOpen.
    property int popovers: 0
    readonly property bool surfacesOpen: popovers > 0 || windowOpen
    property var heldProduct: null
    onSurfacesOpenChanged: surfaceSettle.restart()
    property Timer surfaceSettle: Timer { interval: 3000; repeat: true; running: true; onTriggered: session.settleGridProduct() }
    function settleGridProduct() {
        var st = engine.state;
        if (!st || st.source !== "live" || !st.product) return;
        var site = (engine.sites || []).find(s => s.id === st.site.id);
        var composite = !!site && site.kind === "grid" && site.provider !== "mosaic";
        if (!surfacesOpen) {
            if (!composite || st.product.id === "REF") return;
            var command = {type: "set_product", product: st.product.id};
            if (st.product.heightM > 0) command.heightM = st.product.heightM;
            if (st.product.above) command.above = st.product.above;
            heldProduct = {site: site.id, command: command};
            engine.send({type: "set_product", product: "REF"});
        } else if (heldProduct) {
            var held = heldProduct;
            heldProduct = null;
            if (composite && held.site === site.id && st.product.id === "REF") engine.send(held.command);
        }
    }
    property Timer persistTimer: Timer { interval: 400; onTriggered: session.persist() }
    onLocatingChanged: viewChanged()
    property Connections engineEvents: Connections {
        target: session.engine
        function onStateChanged() {
            if (!session.engine.state) { session.initialized = false; session.mosaicResent = false; }
            else { session.startupError = ""; session.initialize(); session.keepMosaic(); }
        }
    }
    property Connections mosaicEvents: Connections {
        target: session.mosaicStore
        function onSetChanged() { session.keepMosaic(); }
    }
    property Connections configEvents: Connections {
        target: session.config
        function onReadyChanged() { session.resolve(); session.initialize(); }
        function onValuesChanged() { if (session.initialized) { session.resolve(); session.applyRadar(); } }
        function onLocationChanged() { if (!session.hasView) session.resolve(); if (session.initialized) session.applyRadar(); }
        function onTreatmentChanged() { session.applyTreatment(); }
        function onWeakFloorChanged() { session.applyTreatment(); }
    }
    property Connections rememberedEvents: Connections {
        target: session.remembered
        function onReadyChanged() { session.resolve(); session.initialize(); }
    }
    // The engine bootstrap (run.sh --ensure: install the pinned engine if
    // needed, start or replace the daemon) runs detached, so a plugin reload
    // mid-install cannot kill it: `omarchy plugin add` clones many files and
    // the registry reloads the plugin on each one. While the engine stays
    // unreachable it is retried every 20 s, so a killed or failed attempt
    // recovers on its own. argv, never shell text, since checkout paths may
    // contain spaces; bash will not start in Quickshell's cwd
    // (qrc:/qs-blackhole), so env -C moves it home. Its stderr lands in
    // bootstrap.log beside the socket (run.sh keeps it private), and the
    // popover shows the last line while there is no engine. The engine
    // refuses to run without an absolute XDG_RUNTIME_DIR (runtime_path in
    // engine/src/main.rs), and the log has no other private place, so
    // without one there is no bootstrap and the popover says why.
    readonly property string root: Quickshell.env("OMASTORM_ROOT") || Quickshell.env("HOME") + "/.config/omarchy/plugins/omacheese.omastorm-nord"
    readonly property string runtimeDir: Quickshell.env("XDG_RUNTIME_DIR") || ""
    readonly property bool hasRuntime: runtimeDir.startsWith("/")
    readonly property string bootstrapLog: hasRuntime ? runtimeDir + "/omastorm-nord/bootstrap.log" : ""
    readonly property string noRuntime: "The engine needs XDG_RUNTIME_DIR set to an absolute path (a login session sets it), and this session has none."
    function bootstrap() {
        if (!hasRuntime) { startupError = noRuntime; return; }
        Quickshell.execDetached(["env", "-C", Quickshell.env("HOME"), "OMASTORM_BOOTSTRAP_LOG=" + bootstrapLog, "bash", root + "/run.sh", "--ensure"]);
    }
    property Timer bootstrapRetry: Timer { interval: 20000; repeat: true; running: !session.engine.state && session.hasRuntime; onTriggered: session.bootstrap() }
    // run.sh --bootstrap-log prints only the log's last 4 KiB, and only
    // from a plain file of this user's in a private dir (the same rules
    // --ensure writes it under): a planted symlink (to /dev/zero, or
    // someone's file) or a FIFO is refused, its reason shown instead. Polled
    // while a popover is open with no engine: a watch would miss the log
    // when the runtime dir has no omastorm-nord yet, as after every boot.
    property Process bootstrapLogReader: Process {
        command: ["env", "-C", Quickshell.env("HOME"), "OMASTORM_BOOTSTRAP_LOG=" + session.bootstrapLog, "bash", session.root + "/run.sh", "--bootstrap-log"]
        stdout: StdioCollector { waitForEnd: true }
        onExited: function (exitCode) {
            var text = String(stdout.text || "").trim();
            if (session.engine.state || (!text && exitCode !== 0)) return;
            var lines = text.split("\n");
            session.startupError = lines[lines.length - 1];
        }
    }
    property Timer bootstrapLogPoll: Timer {
        interval: 2000; repeat: true; triggeredOnStart: true
        running: session.popovers > 0 && !session.engine.state && session.hasRuntime
        onTriggered: if (!session.bootstrapLogReader.running) session.bootstrapLogReader.running = true
    }
    Component.onCompleted: { applyTreatment(); resolve(); bootstrap(); }
}
