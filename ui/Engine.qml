import QtQuick
import Quickshell
import Quickshell.Io

QtObject {
    id: engine
    property var state: null
    property var sites: []
    /// hello.referenceSites (S23): other radars' positions, marks only.
    property var referenceSites: []
    /// hello.products (S20): the product vocabulary, in chooser order;
    /// empty from an older engine, which hides the chooser.
    property var products: []
    /// hello.mosaic (S25): what My mosaic accepts, or null from an engine
    /// older than S25, which hides it.
    property var mosaic: null
    /// Transport and parsing trouble: disconnected, unreadable message,
    /// unknown protocol version. Cleared by the next valid state.
    property string error: ""
    /// The engine's answer to this client's last command when it could not be
    /// carried out. Only this client hears it, and a later state does not
    /// clear it; the next command from here does, except a tile request,
    /// which is the map's housekeeping rather than something the user did.
    property string rejection: ""
    property bool incompatible: false
    /// One tile answering this client's `tiles_needed`; a reply, not state.
    signal tileReady(var tile)
    // S42: the weather layers this connection shows, {temp, wind}; sent as
    // set_layers on every connect once a layer has been on (an older
    // engine just logs the command), and the engine's `obs` for them.
    // S43: `source` (stations, grid or both) says where they come from;
    // `grid` is the engine's `obs` with source grid (the MET Nordic
    // analysis), held apart from the stations' `obs`.
    property var layers: ({temp: false, wind: false, source: "stations"})
    property bool layersSent: false
    property bool lightningSent: false
    property string layersLine: ""
    property var obs: null
    property var grid: null
    /// Review S5: an S42 engine answered `source` with an error; the layers
    /// go again without it, and the grid says why it is missing.
    property bool gridRefused: false
    readonly property var refusedGrid: ({time: "", provider: {id: "metnordic", status: "failed", note: "grid needs a newer engine"}, attribution: ""})
    readonly property string layerSource: layers && (layers.source === "grid" || layers.source === "both") ? layers.source : "stations"
    readonly property bool stationsWanted: !!(layers && (layers.temp || layers.wind)) && layerSource !== "grid"
    readonly property bool gridWanted: !!(layers && (layers.temp || layers.wind)) && layerSource !== "stations"
    // S44: `layers.lightning` turns the strikes on for this connection;
    // `lightning` is the engine's `lightning` message (strikes files under
    // tex/), `lightningInfo` hello.lightning (null from an older engine).
    property var lightning: null
    property var lightningInfo: null
    readonly property bool lightningWanted: !!(layers && layers.lightning)
    onLayersChanged: sendLayers()
    function sendLayers() {
        var on = !!(layers && (layers.temp || layers.wind || layers.lightning));
        if (!stationsWanted) obs = null;
        if (!gridWanted) grid = null;
        if (!lightningWanted) lightning = null;
        if (!on && !layersSent) return;
        if (!socket || !socket.connected) return;
        // Review N7: the same layers again (a store reload) are not re-sent.
        var command = {type: "set_layers", temp: !!layers.temp, wind: !!layers.wind};
        // An S42 engine knows only stations; `source` goes out when it is not
        // and the engine has not refused it (review S5).
        if (layerSource !== "stations" && !gridRefused) command.source = layerSource;
        // An engine older than S44 ignores it; sent only once it has been on.
        if (lightningWanted || lightningSent) command.lightning = lightningWanted;
        if (lightningWanted) lightningSent = true;
        var line = JSON.stringify(command);
        if (layersSent && line === layersLine) return;
        layersSent = true;
        layersLine = line;
        socket.write(line + "\n");
    }
    // ---- S47: the weather layers' own loading, and Reset ----
    /// A provider that let the stations down, in a few words ("DMI error
    /// 503", "Frost: no client ID"), or "".
    function providerProblem(p) {
        if (!p) return "";
        if (p.status === "skipped" && p.id === "frost") return "Frost: no client ID";
        if (p.status === "skipped") return (p.name || p.id) + ": off";
        if (p.status !== "failed") return "";
        var m = /HTTP (\d{3})/.exec(p.note || "");
        return (p.name || p.id) + (m ? " error " + m[1] : /timed out|timeout/i.test(p.note || "") ? " timed out" : " failed");
    }
    /// Each weather layer as the surfaces say it: {id, state (off, loading,
    /// ok, error), percent, text, problems}. From the engine's `status`
    /// and `percent` (S47); an older engine sends neither, and its layers
    /// read `loading` until their first line and `ok` after it.
    readonly property var stationsLayer: {
        if (!stationsWanted) return {id: "stations", state: "off", percent: 0, text: "", problems: []};
        var o = obs;
        if (!o) return {id: "stations", state: "loading", percent: 0, text: "Fetching weather stations", problems: []};
        var providers = Array.isArray(o.providers) ? o.providers : [];
        var problems = providers.map(providerProblem).filter(x => x !== "");
        var n = Array.isArray(o.stations) ? o.stations.length : 0;
        if (o.status === "loading") {
            var total = providers.length || 4, done = providers.filter(p => p.status !== "loading").length;
            return {id: "stations", state: "loading", percent: Number(o.percent) || 0, problems: problems,
                    text: "Fetching weather stations (" + done + " of " + total + " providers)"};
        }
        if (o.status === "error" || (n === 0 && problems.length > 0 && problems.length === providers.length))
            return {id: "stations", state: "error", percent: 100, problems: problems,
                    text: "Weather stations: " + (problems.length ? problems.join(", ") : "none")};
        return {id: "stations", state: "ok", percent: 100, problems: problems, text: n + " stations"};
    }
    readonly property var gridLayer: {
        var what = layers && layers.temp ? "the temperature grid" : "the wind grid";
        if (!gridWanted) return {id: "grid", state: "off", percent: 0, text: "", problems: []};
        var g = grid;
        if (!g || g.status === "loading" || (g.provider && g.provider.status === "loading"))
            return {id: "grid", state: "loading", percent: 0, text: "Fetching " + what, problems: []};
        var p = g.provider || {};
        var problem = p.status === "failed" ? providerProblem({status: "failed", name: "MET Nordic grid", note: p.note}) : "";
        if (!g.time) return {id: "grid", state: "error", percent: 100, problems: problem ? [problem] : [],
                             text: problem || ("MET Nordic grid: " + (p.note || "none"))};
        return {id: "grid", state: "ok", percent: 100, problems: problem ? [problem + " (update)"] : [],
                text: "Grid " + Qt.formatTime(new Date(g.time), Qt.locale().timeFormat(Locale.ShortFormat))};
    }
    readonly property var lightningLayer: {
        if (!lightningWanted) return {id: "lightning", state: "off", percent: 0, text: "", problems: []};
        if (state && !lightningInfo) return {id: "lightning", state: "error", percent: 100, text: "Lightning: engine too old", problems: ["engine too old"]};
        var m = lightning;
        if (!m) return {id: "lightning", state: "loading", percent: 0, text: "Fetching lightning", problems: []};
        if (m.status === "failed") {
            var problem = providerProblem({status: "failed", name: "FMI lightning", note: m.note});
            return {id: "lightning", state: m.path ? "ok" : "error", percent: 100, text: problem, problems: [problem]};
        }
        return {id: "lightning", state: "ok", percent: 100, text: (Number(m.count) || 0) + " strikes", problems: []};
    }
    /// The layers still loading, for the loading bar and card.
    readonly property var layerLoads: [stationsLayer, gridLayer, lightningLayer].filter(l => l.state === "loading")
    readonly property var layerStates: [stationsLayer, gridLayer, lightningLayer].filter(l => l.state !== "off")
    /// Review of the stations bug: a layer that is on and has had no line
    /// for a while is asked for again (an S47 engine answers every
    /// `set_layers`; an older one ignores the repeat).
    property Timer layersWatch: Timer {
        interval: 15000
        repeat: true
        running: !!engine.socket && engine.socket.connected && engine.layersSent && engine.reasked < 4
            && ((engine.stationsWanted && !engine.obs) || (engine.gridWanted && !engine.grid) || (engine.lightningWanted && !engine.lightning && !!engine.lightningInfo))
        // Review S3: all off, then the layers again: an engine older than
        // S47 answers only a layer turning on, never the same ask twice.
        onTriggered: {
            engine.reasked++;
            var off = {type: "set_layers", temp: false, wind: false};
            if (engine.lightningSent) off.lightning = false;
            engine.socket.write(JSON.stringify(off) + "\n");
            engine.layersLine = "";
            engine.sendLayers();
        }
    }
    // Review N7: a new set of wanted layers starts the count again.
    readonly property string wantedKey: [stationsWanted, gridWanted, lightningWanted].join()
    onWantedKeyChanged: reasked = 0
    /// Re-asks on this connection; an engine with no weather layers at all
    /// (older than S42) is not asked for ever.
    property int reasked: 0
    /// S47: a Reset is under way: the connection is being replaced, and the
    /// new one sends `reset` after `hello`.
    property bool resetting: false
    signal resetDone()
    /// S47 (docs/protocol.md, Reset): drop everything this client holds
    /// (state, frames, layer lines, the load's progress), reconnect, and
    /// ask the engine to load the current choices afresh. The station, the
    /// product, the layers and the view stay as they are.
    function resetAll() {
        // Review S2: one at a time (Shift+R autorepeat reconnected per key).
        if (incompatible || resetting) return;
        resetting = true;
        resetTimeout.restart();
        state = null;
        obs = null;
        grid = null;
        lightning = null;
        heldLoading = null;
        lastLoading = null;
        holdTimer.stop();
        pendingId = "";
        rejection = "";
        error = "";
        layersSent = false;
        layersLine = "";
        // Review S1: the old connection stays open until the new one's
        // hello, so the engine never sees this client gone (a composite's
        // product would go back to the composite).
        if (retiring) retiring.destroy();
        retiring = socket;
        socket = socketFactory.createObject(engine);
    }
    property var retiring: null
    /// End a Reset: `sent` when the new connection's hello came and the
    /// `reset` went; otherwise (no hello within 5 s, or the new connection
    /// failed) the engine is down, and it says so.
    function endReset(sent) {
        resetting = false;
        resetTimeout.stop();
        if (retiring) { retiring.destroy(); retiring = null; }
        if (!sent && !state && !incompatible) error = disconnectedText;
    }
    property Timer resetTimeout: Timer { interval: 5000; onTriggered: engine.endReset(false) }
    /// Places answering this client's `search_places`; a reply, not state.
    signal placesReady(var message)
    readonly property string runtime: Quickshell.env("XDG_RUNTIME_DIR") + "/omastorm-se/"
    /// `state.timeline`, or none (a harness state may leave it out).
    readonly property var timeline: state && state.timeline ? state.timeline : []
    /// `state.loading` (S31): how far the load this client waits for has
    /// come, or null (nothing loads, or an engine older than S31, whose
    /// `connection.status` still says `loading`).
    readonly property var wireLoading: state && state.source === "live" && state.loading
        && typeof state.loading.label === "string" && isFinite(state.loading.percent) ? state.loading : null
    /// What the surfaces show: `wireLoading`, or (S41 review SF1) the last
    /// one held for `holdMs` after it went null while a stage of it was
    /// still waiting. A radar's load goes null after its first stage's
    /// linger and its history starts BACKFILL_DELAY (3 s) later; without
    /// the hold the bar and the percentage blinked out in between. A new
    /// loading, another station or the timer ends the hold.
    readonly property var loading: wireLoading
        || (heldLoading && state && heldLoading.site === state.site.id ? heldLoading.loading : null)
    property var heldLoading: null
    property var lastLoading: null
    readonly property int holdMs: 4000
    property Timer holdTimer: Timer { interval: engine.holdMs; onTriggered: engine.heldLoading = null }
    onWireLoadingChanged: {
        var l = wireLoading;
        if (l) {
            holdTimer.stop();
            heldLoading = null;
            lastLoading = {site: state.site.id, loading: l};
            return;
        }
        var last = lastLoading;
        lastLoading = null;
        var waiting = !!last && Array.isArray(last.loading.stages) && last.loading.stages.some(s => s && s.state === "waiting");
        if (waiting && state && state.site.id === last.site) {
            heldLoading = last;
            holdTimer.restart();
        } else {
            heldLoading = null;
        }
    }
    /// S31: while a composite's product has no frame (the placeholder), the
    /// composite's own newest frame the engine names to draw in its place.
    readonly property var under: {
        var u = loading && state && !state.frame.scanTime ? loading.under : null;
        if (!u || !u.scanTime || !validTexturePath(u.texture)) return null;
        if (u.kind === "grid") return u.azimuthLut === "" && u.grid ? u : null;
        return u.kind === "polar" && validTexturePath(u.azimuthLut) ? u : null;
    }

    // ---- S41: the load as named steps and one overall percentage ----
    // The engine's stages are named by the work, not by their ids, and two
    // steps only the client knows go round them: starting the engine (the
    // bar's `ensure` and the socket, before any state) and drawing (the
    // frame received, its texture not loaded yet). The one number a surface
    // shows is the whole load's: Σ share × percent over the stages (an
    // engine older than S35: its top-level percent), and it never goes
    // back within a load.
    /// A step's name, by the work it does.
    function stepName(id) {
        return id === "engine" ? "Starting engine" : id === "first" ? "Fetching radar data"
            : id === "build" ? "Engine: building the frame" : id === "draw" ? "Drawing"
            : id === "history" ? "Fetching history" : id || "";
    }
    /// What the engine's label says past the load's name ("Vara
    /// Reflectivity 0.5°: 24 of 58 frames" → "24 of 58 frames"), minus the
    /// words the step's name already says.
    function stepDetail(l) {
        if (!l || typeof l.label !== "string") return "";
        var at = l.label.indexOf(": ");
        var d = at >= 0 ? l.label.slice(at + 2) : l.label;
        if (l.stage === "history") d = d.replace(/^history /, "");
        if (l.stage === "first" && d === "first frame") d = "the newest scan";
        return d;
    }
    /// The load's name: the label before its colon ("My mosaic, Lowest
    /// beam"), else the station's.
    readonly property string loadName: {
        var l = loading;
        if (l && l.label.indexOf(": ") > 0) return l.label.slice(0, l.label.indexOf(": "));
        return site ? site.name : "";
    }
    /// The whole load's fill, 0 to 100, from `loading` alone.
    function overallOf(l) {
        if (!l) return 0;
        var clamp = p => Math.max(0, Math.min(100, Number(p) || 0));
        var st = Array.isArray(l.stages) ? l.stages.filter(s => !!s) : [];
        if (!st.length) return Math.floor(clamp(l.percent));
        var sum = 0, shares = 0;
        for (var s of st) {
            var share = Math.max(1, Number(s.share) || 0);
            shares += share;
            sum += share * (s.state === "done" ? 100 : s.state === "waiting" ? 0 : clamp(s.percent));
        }
        return Math.floor(sum / shares);
    }
    /// The overall percentage a surface shows: `overallOf(loading)`, held
    /// at its highest within one load. Another station, or a load back on
    /// its first stage (or lower in it), starts again from its own value.
    property int percent: 0
    property string loadKey: ""
    property real firstSeen: -1
    onLoadingChanged: {
        var l = loading;
        if (!l) { percent = 0; loadKey = ""; loadStage = ""; firstSeen = -1; return; }
        var first = l.stage === "first" ? Number(l.percent) || 0 : 101;
        // Review NIT7: without `stages` (an engine older than S35) the
        // percentage is the stage's own, so a new stage starts again.
        var staged = Array.isArray(l.stages) && l.stages.length > 0;
        var again = state.site.id !== loadKey || firstSeen > first || (!staged && l.stage !== loadStage);
        var value = overallOf(l);
        percent = again ? value : Math.max(percent, value);
        loadKey = state.site.id;
        loadStage = l.stage;
        firstSeen = first;
    }
    property string loadStage: ""
    /// Whether the texture of the frame on screen is loaded: a hidden Image
    /// on the same URL, which Qt's cache shares with the map's sampler.
    property Image drawProbe: Image {
        // Only while something loads (review NIT4): idle, it would hold
        // one more image for nothing.
        source: engine.busy ? engine.texture : ""
        asynchronous: true
        cache: true
        smooth: false
        mipmap: false
        visible: false
    }
    readonly property bool drawn: !!frame && !!frame.scanTime && texture !== "" && drawProbe.status === Image.Ready
    /// Something is loading that a surface should say: no engine yet, a
    /// load in `loading`, or (an engine older than S31) `loading` status.
    readonly property bool busy: starting || (!!state && (!!loading
        || (state.source === "live" && !!state.connection && state.connection.status === "loading")))
    /// No state yet and nothing wrong but the socket (review SF3): the
    /// engine is starting. Any other error (an unreadable message, an
    /// unknown protocol version) is shown as itself, not as a start.
    readonly property string disconnectedText: "Radar engine disconnected. Reconnecting…"
    readonly property string unavailableText: "Radar engine unavailable. Reconnecting…"
    readonly property bool starting: !state && !incompatible
        && (error === "" || error === disconnectedText || error === unavailableText)
    /// The steps in order, each {id, name, state (done|active|waiting),
    /// detail}. The engine's stages are the segments of the bar; the two
    /// client steps have none. `steps` is reassigned only when this changes
    /// (review SF2): every state rebuilt the array, and a Repeater over it
    /// recreated its delegates once a second, restarting the ● pulse.
    property var steps: []
    property string stepsKey: ""
    onStepsNowChanged: {
        var key = JSON.stringify(stepsNow);
        if (key === stepsKey) return;
        stepsKey = key;
        steps = stepsNow;
    }
    readonly property var stepsNow: {
        var step = (id, st, detail) => ({id: id, name: stepName(id), state: st, detail: detail || ""});
        var out = [step("engine", state ? "done" : "active", state ? "" : error ? "waiting for its socket" : "connecting")];
        if (!state) return out.concat([step("first", "waiting"), step("draw", "waiting")]);
        var l = loading;
        var stages = l && Array.isArray(l.stages) && l.stages.length ? l.stages.filter(s => !!s)
            : l ? [{stage: l.stage, state: "active"}]
            : [{stage: "first", state: drawn ? "done" : "active"}];
        // The frame is drawn after the stage that makes it.
        var after = stages.some(s => s.stage === "build") ? "build" : "first";
        var scanned = !!frame && !!frame.scanTime;
        var draw = null;
        for (var s of stages) {
            var st = s.state === "done" || s.state === "active" ? s.state : "waiting";
            out.push(step(s.stage, st, l && st === "active" && s.stage === l.stage ? stepDetail(l) : ""));
            if (s.stage === after) {
                draw = step("draw", drawn ? "done" : st === "done" && scanned ? "active" : "waiting");
                out.push(draw);
            }
        }
        // A load with no first stage (the station opened on a frame it had).
        if (!draw) out.splice(1, 0, step("draw", drawn ? "done" : scanned ? "active" : "waiting"));
        return out;
    }
    /// The step to name: drawing while it runs, else the one the engine is
    /// on. Between two (the linger at a stage's 100, before the next one
    /// starts) the next one, "up next"; when every step is done, "Loaded".
    readonly property var activeStep: {
        var s = steps.find(s => s.id === "draw" && s.state === "active") || steps.find(s => s.state === "active");
        if (s) return s;
        var next = steps.find(s => s.state === "waiting" && s.id !== "draw");
        if (next) return Object.assign({}, next, {detail: "up next"});
        return loading ? {id: "loaded", name: "Loaded", state: "done", detail: ""} : null;
    }

    // The frame on screen (docs/protocol.md, timeline textures). While this
    // client plays its own loop, or has just stepped or scrubbed and waits
    // for the engine to confirm, it is a timeline entry drawn from its own
    // stable textures; otherwise the engine's `frame`. Surfaces draw and
    // label this, never `state.frame`.
    readonly property var frame: {
        if (!state) return null;
        var id = shownId !== "" ? shownId : pendingId;
        if (id !== "" && id !== state.frame.id) {
            var e = timeline.find(t => t.id === id);
            if (e && bufferable(e)) return entryFrame(e);
        }
        // The engine's frame, through its timeline entry when that names the
        // same files: a grid entry's code texture is the smaller one to read,
        // under the loop's rule (loopCodes) so pausing never switches kind.
        var own = timeline.find(t => t.id === state.frame.id && t.texture === state.frame.texture);
        if (own && own.codes && loopCodes && state.frame.kind === "grid") return Object.assign({}, state.frame, {codes: own.codes});
        // S31: the composite under its loading product, not the placeholder.
        if (under) return under;
        return state.frame;
    }
    readonly property bool playing: looping || (!!state && !!state.playing)
    /// True when `texture` is a grid's one-channel code texture rather than
    /// its RGBA grid texture (docs/protocol.md, code texture); RadarMap then
    /// rebuilds each texel's class from the frame's bounds.
    readonly property bool codes: !!frame && frame.kind === "grid" && !!frame.codes
    /// The files to draw: none once a surface that showed this client has
    /// closed (`active` back to false), so a closed window lets the frame on
    /// screen go too, not only the loop's. A client never made active (the
    /// map checks and tests) draws as it always did.
    readonly property bool drawing: active || !everActive
    property bool everActive: false
    readonly property string texture: frame && drawing ? "file://" + runtime + (codes ? frame.codes : frame.texture) : ""
    /// A grid frame has no azimuth lookup (docs/protocol.md, frame.kind). The
    /// shader never reads one for a grid, but its sampler still wants an
    /// image, so the grid texture stands in.
    readonly property string azimuthLut: frame && drawing ? (frame.kind === "grid" ? texture : "file://" + runtime + frame.azimuthLut) : ""
    /// The selected station's row from `hello`, or null before it arrives.
    readonly property var site: state ? (sites.find(s => s.id === state.site.id) || null) : null

    // ---- The loop buffer and local playback (S21, after the web's S19) ----
    // Each complete timeline entry names stable textures, so this client
    // keeps the newest ones loaded and plays them itself: a hidden Image per
    // file holds the decoded pixmap in Qt's cache, and RadarMap's sampler,
    // asking for the same URL, gets it (and its GPU texture, once shown)
    // without reading or decoding the file again and without waiting for
    // the engine. Play and pause are shared by this process's surfaces
    // through `loopSite`; pause, step and scrub `seek`, so the engine and
    // every other client follow. Entries without textures (an engine older
    // than S19) leave all of it off and the engine plays, as before.
    /// Whether this client shows frames: a visible window or popover. The
    /// session's status connection keeps it off and holds no textures.
    property bool active: false
    /// Decoded texture memory the buffer may hold, in bytes (DESIGN.md, loop
    /// buffer): a quarter of the machine's MemAvailable, read each time this
    /// client opens, at least `bufferFloor` and at most `bufferCeiling`. Qt
    /// keeps each image as 4 bytes a texel (a one-channel code texture is
    /// widened on load) in the shell's own memory and again on the GPU, so
    /// the ceiling sits below the web's 512 MB: Sweden's two hours (24
    /// frames, about 250 MB) fit, Nordic's 15 MB frames give 18 or so.
    property real bufferCeiling: 280 * 1024 * 1024
    readonly property real bufferFloor: 96 * 1024 * 1024
    /// MemAvailable in bytes when this client last opened, 0 when unknown.
    property real memAvailable: 0
    readonly property real bufferCap: memAvailable > 0
        ? Math.max(bufferFloor, Math.min(bufferCeiling, memAvailable / 4)) : bufferCeiling
    property FileView meminfo: FileView { path: "/proc/meminfo"; blockLoading: true }
    function readMemory() {
        meminfo.reload();
        var m = /MemAvailable:\s+(\d+) kB/.exec(meminfo.text());
        memAvailable = m ? Number(m[1]) * 1024 : 0;
    }
    // Closing drops the loop's Images and the frame on screen (`texture`
    // empties); a collection then frees the JS garbage of the states that
    // arrived while it was open (once a second while live).
    onActiveChanged: {
        if (active) { everActive = true; readMemory(); }
        else Qt.callLater(gc);
    }
    Component.onCompleted: {
        if (active) { everActive = true; readMemory(); }
        stepsKey = JSON.stringify(stepsNow);
        steps = stepsNow;
    }
    readonly property int bufferLimit: 24     // two hours of 5-minute scans, as on the web
    readonly property int minStart: 6         // the loop starts with this many ready, or all there are
    readonly property int stepMs: 250         // a frame's time on screen
    readonly property int dwellMs: 1000       // and the newest frame's
    /// The station whose loop this process plays, "" for none; a surface
    /// binds it to PluginSession.loopSite and answers `loopRequested`. An
    /// Engine nobody shares (`loopShared` false) keeps it itself.
    property string loopSite: ""
    property bool loopShared: false
    signal loopRequested(string site)
    function requestLoop(site) {
        loopRequested(site);
        if (!loopShared) loopSite = site;
    }

    function kindOf(e) { return e.placement ? e.placement.kind : state.frame.kind; }
    /// A complete entry with the stable files its kind needs.
    function bufferable(e) {
        if (!state || e.status !== "complete" || !e.texture) return false;
        var k = kindOf(e);
        if (k === "polar") return !!e.azimuthLut;
        return k === "grid" && !!(e.placement ? e.placement.grid : state.frame.grid);
    }
    /// Whether the newest `bufferLimit` complete entries all name a code
    /// texture. A loop that mixes the two (a composite's rows catalogued
    /// before S19 beside newer ones) draws every frame from RGBA instead:
    /// each switch of the sampler from an RGBA to a code texture stalled the
    /// loop about 50 ms in the harness. Such rows leave the ring within hours.
    readonly property bool loopCodes: {
        if (!state) return false;
        var n = 0, tl = timeline;
        for (var i = tl.length - 1; i >= 0 && n < bufferLimit; i--) {
            if (tl[i].status !== "complete" || !tl[i].texture) continue;
            if (!tl[i].codes) return false;
            n++;
        }
        return n > 0;
    }
    /// An entry as a frame: the station's product, palette and bounds from
    /// `frame`, its placement from its own `placement` when it has one.
    function entryFrame(e) {
        var f = Object.assign({}, state.frame, e.placement || {});
        if (f.kind !== "grid") delete f.grid;
        f.id = e.id;
        f.scanTime = e.scanTime;
        f.sweepEnd = e.scanTime;
        f.status = e.status;
        f.texture = e.texture;
        f.azimuthLut = e.azimuthLut;
        f.codes = f.kind === "grid" && loopCodes && e.codes ? e.codes : "";
        return f;
    }
    function frameBytes(f) {
        return f.kind === "grid" ? f.grid.xsize * f.grid.ysize * 4 : f.rays * f.gates * 4 + 3600 * 4;
    }
    /// The files a frame draws from, as the URLs RadarMap asks for.
    function urlsOf(f) {
        if (f.kind === "grid") return ["file://" + runtime + (f.codes || f.texture)];
        return ["file://" + runtime + f.texture, "file://" + runtime + f.azimuthLut];
    }
    /// The loop: the newest bufferable entries, oldest first, at most
    /// `bufferLimit` and within `bufferCap`.
    readonly property var loopFrames: {
        var out = [], bytes = 0;
        if (!active || !state) return out;
        var tl = timeline;
        for (var i = tl.length - 1; i >= 0 && out.length < bufferLimit; i--) {
            if (!bufferable(tl[i])) continue;
            var f = entryFrame(tl[i]), b = frameBytes(f);
            if (bytes + b > bufferCap) break;
            bytes += b;
            out.unshift(f);
        }
        return out;
    }
    readonly property bool canLoop: loopFrames.length >= 2
    /// Loop frames whose files are loaded.
    readonly property var readyFrames: {
        readyRevision;
        return loopFrames.filter(f => urlsOf(f).every(u => readyUrls[u]));
    }
    readonly property int bufferReady: readyFrames.length
    readonly property int bufferTarget: loopFrames.length
    readonly property real bufferMB: Math.round(loopFrames.reduce((n, f) => n + frameBytes(f), 0) / 1048576)
    readonly property int bufferCapMB: Math.round(bufferCap / 1048576)
    /// Bufferable entries within `bufferLimit` that the cap left out.
    readonly property int bufferDropped: {
        if (!active || !state) return 0;
        var n = 0, tl = timeline;
        for (var i = tl.length - 1; i >= 0 && n < bufferLimit; i--) if (bufferable(tl[i])) n++;
        return Math.max(0, n - loopFrames.length);
    }
    /// What the buffer holds and costs, as the web's Loop note says it:
    /// "19/19 frames · 278 MB of 280 MB · 5 older left out".
    readonly property string bufferNote: bufferTarget > 0
        ? bufferReady + "/" + bufferTarget + " frames · " + bufferMB + " MB of " + bufferCapMB + " MB"
          + (bufferDropped > 0 ? " · " + bufferDropped + " older left out" : "")
        : ""
    readonly property bool looping: active && canLoop && !!state && loopSite !== "" && loopSite === state.site.id

    // One hidden Image per file of the loop, newest first, kept while the
    // entry stays in the loop; rows are diffed so a state (once a second
    // while live) never reloads what is already there.
    property var readyUrls: ({})
    property int readyRevision: 0
    property ListModel residentModel: ListModel {}
    property Instantiator residents: Instantiator {
        model: engine.residentModel
        delegate: Image {
            property string url: model.url
            source: url
            asynchronous: true
            cache: true
            smooth: false
            mipmap: false
            visible: false
            onStatusChanged: engine.residentStatus(url, status)
            Component.onCompleted: engine.residentStatus(url, status)
        }
    }
    function residentStatus(url, status) {
        var ready = status === Image.Ready;
        if (!!readyUrls[url] === ready) return;
        if (ready) readyUrls[url] = true;
        else delete readyUrls[url];
        readyRevision++;
    }
    onLoopFramesChanged: {
        var want = {}, order = [];
        for (var i = loopFrames.length - 1; i >= 0; i--)
            for (var u of urlsOf(loopFrames[i]))
                if (!want[u]) { want[u] = true; order.push(u); }
        var dropped = false;
        for (var j = residentModel.count - 1; j >= 0; j--) {
            var have = residentModel.get(j).url;
            if (want[have]) { delete want[have]; continue; }
            residentModel.remove(j);
            delete readyUrls[have];
            dropped = true;
        }
        for (var n of order) if (want[n]) residentModel.append({url: n});
        if (dropped) readyRevision++;
    }

    // The local loop: a fixed schedule (next += step), checked every few
    // milliseconds, so frame times stay even whatever the tick; it waits
    // for `minStart` frames and only ever shows loaded ones.
    property string shownId: ""
    property real nextMs: 0
    property bool waiting: false
    property Timer ticker: Timer {
        interval: 8
        repeat: true
        running: engine.looping
        onRunningChanged: {
            if (running) {
                engine.shownId = engine.frame ? engine.frame.id : "";
                engine.waiting = true;
                engine.nextMs = Date.now();
            } else {
                engine.shownId = "";
            }
        }
        onTriggered: engine.tick()
    }
    function tick() {
        var ring = readyFrames, now = Date.now();
        if (waiting) {
            if (ring.length < 2 || ring.length < Math.min(minStart, loopFrames.length)) return;
            waiting = false;
            nextMs = now;
        }
        if (now < nextMs || ring.length < 2) return;
        var at = ring.findIndex(f => f.id === shownId);
        var next = ring[(at + 1) % ring.length];
        var newest = next === ring[ring.length - 1];
        shownId = next.id;
        nextMs += newest ? dwellMs : stepMs;
        if (nextMs < now) nextMs = now + (newest ? dwellMs : stepMs);
    }

    // A frame this client already shows while its `seek` is on the way:
    // kept until the engine's state names it, the seek is refused, or 5 s.
    property string pendingId: ""
    property Timer pendingExpiry: Timer { interval: 5000; onTriggered: engine.pendingId = "" }
    function pin(id) {
        pendingId = state && id !== state.frame.id ? id : "";
        if (pendingId !== "") pendingExpiry.restart();
    }

    // Transport, for both surfaces.
    function togglePlay() {
        if (!state) return;
        if (looping) {
            // Pause where the loop is, and tell the engine so every other
            // client shows the same frame.
            var id = shownId;
            requestLoop("");
            if (id !== "" && id !== state.frame.id && timeline.some(t => t.id === id)) {
                pin(id);
                send({type: "seek", id: id});
            }
        } else if (state.playing) {
            send({type: "pause"});  // the engine's own loop, started by another client
        } else if (canLoop) {
            requestLoop(state.site.id);
        } else if (timeline.filter(f => f.status === "complete").length > 1) {
            send({type: "play"});
        }
    }
    /// Show the entry now when this client has its textures, and `seek` so
    /// the engine and other clients follow; ends a loop either way.
    function seekTo(id) {
        if (!state || !id) return;
        if (looping) requestLoop("");
        var e = timeline.find(t => t.id === id);
        if (e && bufferable(e)) pin(id);
        if (id !== state.frame.id || state.playing) send({type: "seek", id: id});
    }
    /// Move `delta` entries from the frame on screen; without stable
    /// textures the engine steps from its own position, as before.
    function stepBy(delta) {
        if (!state || timeline.length < 2) return;
        if (!canLoop) { send({type: "step", delta: delta}); return; }
        var tl = timeline, at = tl.findIndex(t => t.id === (frame ? frame.id : ""));
        seekTo(tl[Math.max(0, Math.min(tl.length - 1, (at < 0 ? tl.length - 1 : at) + delta))].id);
    }

    /// The protocol's one rule for texture paths (`docs/protocol.md`): the
    /// literal `tex/` prefix and exactly one further segment that is not empty,
    /// `.`, or `..` and holds no `/`, backslash, or NUL. The engine applies the
    /// same rule before publishing.
    function validTexturePath(path) {
        if (typeof path !== "string" || path.indexOf("tex/") !== 0) return false;
        var name = path.slice(4);
        return name !== "" && name !== "." && name !== ".." && !/[\/\\\0]/.test(name);
    }
    /// The tile path rule (`docs/protocol.md`): the literal `tiles/` prefix,
    /// a set (`ne` or `osm`), a zoom and a column as decimal integers, and
    /// one further segment under the texture rule. The engine applies the
    /// same rule before publishing.
    function validTilePath(path) {
        if (typeof path !== "string") return false;
        var parts = path.split("/");
        if (parts.length !== 5 || parts[0] !== "tiles" || (parts[1] !== "ne" && parts[1] !== "osm")) return false;
        if (!/^[0-9]+$/.test(parts[2]) || !/^[0-9]+$/.test(parts[3])) return false;
        var name = parts[4];
        return name !== "" && name !== "." && name !== ".." && !/[\\\0]/.test(name);
    }
    function receive(data) {
        if (incompatible) return;
        try {
            var message = JSON.parse(data);
            if (message.v !== 2) {
                incompatible = true;
                state = null;
                error = "Unsupported engine protocol version: " + message.v;
                socket.connected = false;
                return;
            }
            if (message.type === "hello") {
                sites = message.sites;
                referenceSites = message.referenceSites || [];
                products = message.products || [];
                mosaic = message.mosaic && Array.isArray(message.mosaic.rules) ? message.mosaic : null;
                lightningInfo = message.lightning && typeof message.lightning === "object" ? message.lightning : null;
                layersSent = false;
                gridRefused = false;
                lightningSent = false;
                reasked = 0;
                // S47: a Reset asks the engine before the layers go again,
                // so they come back fetched afresh.
                if (resetting) {
                    socket.write(JSON.stringify({type: "reset"}) + "\n");
                    endReset(true);
                    resetDone();
                }
                sendLayers();
            }
            else if (message.type === "state") {
                var frame = message.frame;
                if (!frame || !validTexturePath(frame.texture))
                    throw new Error("Invalid texture path: " + JSON.stringify(frame && frame.texture));
                // Version 2: a polar sweep with its azimuth lookup, or a grid
                // texture placed by frame.grid with no lookup.
                if (frame.kind === "polar") {
                    if (!validTexturePath(frame.azimuthLut))
                        throw new Error("Invalid azimuth lookup path: " + JSON.stringify(frame.azimuthLut));
                } else if (frame.kind === "grid") {
                    var g = frame.grid;
                    if (frame.azimuthLut !== "")
                        throw new Error("A grid frame has no azimuth lookup: " + JSON.stringify(frame.azimuthLut));
                    if (!g || !(g.xsize > 0) || !(g.ysize > 0) || !(g.east > g.west) || !(g.north > g.south))
                        throw new Error("Invalid grid placement: " + JSON.stringify(g));
                } else {
                    throw new Error("Unknown frame kind: " + JSON.stringify(frame.kind));
                }
                // Timeline entries' files (S19) obey the same rule; "" or
                // absent means the entry has none.
                for (var e of message.timeline || [])
                    for (var key of ["texture", "azimuthLut", "codes"])
                        if (e[key] && !validTexturePath(e[key]))
                            throw new Error("Invalid timeline texture path: " + JSON.stringify(e[key]));
                state = message;
                error = "";
                if (pendingId !== "" && (frame.id === pendingId || !(message.timeline || []).some(t => t.id === pendingId))) pendingId = "";
                // A loop belongs to its station; another station ends it.
                if (loopSite !== "" && message.site.id !== loopSite) requestLoop("");
            } else if (message.type === "error" && message.command === "set_layers" && layerSource !== "stations" && !gridRefused) {
                gridRefused = true;
                layersLine = "";
                sendLayers();
                grid = gridWanted ? refusedGrid : null;
            } else if (message.type === "error") {
                rejection = message.message;
                if (message.command === "seek") pendingId = "";
            }
            else if (message.type === "tile_ready") {
                if (!validTilePath(message.path))
                    throw new Error("Invalid tile path: " + JSON.stringify(message.path));
                tileReady(message);
            } else if (message.type === "places") {
                placesReady(message);
            } else if (message.type === "obs" && message.source === "grid") {
                if (message.temperature && !validTexturePath(message.temperature.texture))
                    throw new Error("Invalid grid texture path: " + JSON.stringify(message.temperature.texture));
                grid = gridWanted ? message : null;
            } else if (message.type === "obs") {
                obs = stationsWanted && Array.isArray(message.stations) ? message : null;
            } else if (message.type === "lightning") {
                if (message.path !== "" && !validTexturePath(message.path))
                    throw new Error("Invalid strikes path: " + JSON.stringify(message.path));
                lightning = lightningWanted ? message : null;
            }
        } catch (e) { state = null; error = "Invalid engine message: " + e; }
    }
    function send(command) {
        if (command.type !== "tiles_needed" && command.type !== "search_places") rejection = "";
        socket.write(JSON.stringify(command) + "\n");
    }
    property var socket: socketFactory.createObject(engine)
    property Component socketFactory: Component {
        Socket {
            id: sock
            path: engine.runtime + "engine.sock"
            connected: true
            // S47: a connection a Reset replaced (kept open until the new
            // one's hello, review S1) says nothing and is not listened to.
            parser: SplitParser { onRead: data => { if (engine.socket === sock) engine.receive(data); } }
            onConnectedChanged: {
                if (engine.socket !== sock) return;
                if (!connected && !engine.incompatible) {
                    engine.state = null;
                    engine.obs = null;
                    engine.grid = null;
                    engine.lightning = null;
                    engine.error = engine.disconnectedText;
                }
            }
            onError: {
                if (engine.socket === sock && !engine.incompatible) engine.error = engine.unavailableText;
            }
        }
    }
    property Timer reconnect: Timer {
        interval: 1000
        repeat: true
        running: engine.socket && !engine.socket.connected && !engine.incompatible
        // A failed initial connect leaves Quickshell's underlying socket
        // allocated; toggling connected cannot retry it. Replace the object.
        onTriggered: {
            // S47 review S2: a Reset whose new connection did not come up
            // is over (the engine is down): no stale `reset` later.
            if (engine.resetting) engine.endReset(false);
            var previous = engine.socket;
            engine.socket = engine.socketFactory.createObject(engine);
            previous.destroy();
        }
    }
}
