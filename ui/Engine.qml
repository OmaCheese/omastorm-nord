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
    /// Places answering this client's `search_places`; a reply, not state.
    signal placesReady(var message)
    readonly property string runtime: Quickshell.env("XDG_RUNTIME_DIR") + "/omastorm-se/"
    /// `state.timeline`, or none (a harness state may leave it out).
    readonly property var timeline: state && state.timeline ? state.timeline : []

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
    Component.onCompleted: if (active) { everActive = true; readMemory(); }
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
            path: engine.runtime + "engine.sock"
            connected: true
            parser: SplitParser { onRead: data => engine.receive(data) }
            onConnectedChanged: {
                if (!connected && !engine.incompatible) {
                    engine.state = null;
                    engine.error = "Radar engine disconnected. Reconnecting…";
                }
            }
            onError: {
                if (!engine.incompatible) engine.error = "Radar engine unavailable. Reconnecting…";
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
            var previous = engine.socket;
            engine.socket = engine.socketFactory.createObject(engine);
            previous.destroy();
        }
    }
}
