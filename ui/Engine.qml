import QtQuick
import Quickshell
import Quickshell.Io

QtObject {
    id: engine
    property var state: null
    property var sites: []
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
    readonly property string texture: state && state.frame ? "file://" + runtime + state.frame.texture : ""
    /// A grid frame has no azimuth lookup (docs/protocol.md, frame.kind). The
    /// shader never reads one for a grid, but its sampler still wants an
    /// image, so the grid texture stands in.
    readonly property string azimuthLut: state && state.frame ? "file://" + runtime + (state.frame.kind === "grid" ? state.frame.texture : state.frame.azimuthLut) : ""
    /// The selected station's row from `hello`, or null before it arrives.
    readonly property var site: state ? (sites.find(s => s.id === state.site.id) || null) : null
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
            if (message.type === "hello") sites = message.sites;
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
                state = message;
                error = "";
            } else if (message.type === "error") rejection = message.message;
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
