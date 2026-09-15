import QtQuick
import Quickshell
import Quickshell.Io

// The reach (S29): per radar, how far from it its data is drawn, in km,
// chosen in the window's product menu and remembered in reach.json beside
// state.json. The window and the popover each hold one; the FileView watches
// for the other's writes. No entry (or 0) draws the whole sweep. The clip is
// the shader's (RadarMap.reachKm); the engine never hears of it.
// OMASTORM_STATE puts the file beside that one; OMASTORM_CONFIG without
// OMASTORM_STATE keeps it in memory, as Remembered does for state.json.
QtObject {
    id: root
    readonly property string path: {
        var state = Quickshell.env("OMASTORM_STATE");
        if (state) return state.slice(0, state.lastIndexOf("/") + 1) + "reach.json";
        if (Quickshell.env("OMASTORM_CONFIG")) return "";
        return (Quickshell.env("XDG_STATE_HOME") || Quickshell.env("HOME") + "/.local/state") + "/omastorm-se/reach.json";
    }
    property var kms: ({})
    property string error: ""
    // The reach of radar `id` in km; 0 for the whole sweep.
    function km(id) {
        var value = kms[id];
        return typeof value === "number" && value > 0 ? value : 0;
    }
    // Remember `value` km for `id`; 0 forgets it (the whole sweep).
    function set(id, value) {
        if (!id) return;
        var next = Object.assign({}, kms);
        if (value > 0) next[id] = Math.round(value);
        else delete next[id];
        kms = next;
        if (!path) return;
        pending = JSON.stringify(next);
        if (!writer.running) flush();
    }
    // The next write, held while one still runs (Quickshell ignores a new
    // command on a running Process); the writer's exit sends it.
    property string pending: ""
    function flush() {
        if (!pending) return;
        var slash = path.lastIndexOf("/");
        var dir = slash >= 0 ? path.slice(0, slash) : ".";
        writer.command = ["sh", "-c",
            "mkdir -p -- \"$1\" && printf '%s\\n' \"$3\" > \"$2\" && mv -f -- \"$2\" \"$4\"",
            "omastorm-reach", dir, path + ".tmp", pending, path];
        pending = "";
        writer.running = true;
    }
    property FileView file: FileView {
        path: root.path
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: {
            // Our own write still on its way: kms is already newer.
            if (root.pending || root.writer.running) return;
            try {
                var value = JSON.parse(text());
                root.kms = value && typeof value === "object" && !Array.isArray(value) ? value : {};
            } catch (e) {
                root.kms = {};
            }
        }
    }
    property Process writer: Process {
        command: ["true"]
        onExited: function (exitCode) {
            root.error = exitCode === 0 ? "" : "Could not write reach.json";
            if (root.pending) root.flush();
        }
    }
}
