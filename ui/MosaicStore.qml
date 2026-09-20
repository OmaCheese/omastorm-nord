import QtQuick
import Quickshell
import Quickshell.Io
import "Mosaic.js" as Mosaic

// My mosaic's last set (S25): the radars and the rule, as
// state.mosaic last named them, in mosaic.json beside state.json. The
// session sends it again to an engine whose state.mosaic is empty (a
// restarted engine), once per connection, and the window's checklist starts
// from it. OMASTORM_STATE puts the file beside that one; OMASTORM_CONFIG
// without OMASTORM_STATE keeps it in memory.
QtObject {
    id: root
    readonly property string path: {
        var state = Quickshell.env("OMASTORM_STATE");
        if (state) return state.slice(0, state.lastIndexOf("/") + 1) + "mosaic.json";
        if (Quickshell.env("OMASTORM_CONFIG")) return "";
        return (Quickshell.env("XDG_STATE_HOME") || Quickshell.env("HOME") + "/.local/state") + "/omastorm-se/mosaic.json";
    }
    /// {sites: [{id}], rule}, and for a height set heightM and
    /// above (S30), or null before any.
    property var set: null
    property string error: ""
    function text(value) {
        // S37: no reach is kept; a set written by an older build still
        // loads, its reachKm simply ignored here and by the engine.
        var kept = { sites: value.sites.map(s => ({ id: s.id })), rule: value.rule || "lowest" };
        if (kept.rule === "height") {
            kept.heightM = Mosaic.validHeight(value.heightM) ? value.heightM : 2000;
            kept.above = value.above === "ground" ? "ground" : "sea";
        }
        return JSON.stringify(kept);
    }
    /// Remember `value` when it is a set and differs from the one kept.
    function keep(value) {
        if (!Mosaic.valid(value)) return;
        var next = text(value);
        if (Mosaic.valid(set) && text(set) === next) return;
        set = JSON.parse(next);
        if (!path) return;
        pending = next;
        if (!writer.running) flush();
    }
    // The next write, held while one still runs.
    property string pending: ""
    function flush() {
        if (!pending) return;
        var slash = path.lastIndexOf("/");
        var dir = slash >= 0 ? path.slice(0, slash) : ".";
        writer.command = ["sh", "-c",
            "mkdir -p -- \"$1\" && printf '%s\\n' \"$3\" > \"$2\" && mv -f -- \"$2\" \"$4\"",
            "omastorm-mosaic", dir, path + ".tmp", pending, path];
        pending = "";
        writer.running = true;
    }
    property FileView file: FileView {
        path: root.path
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: {
            if (root.pending || root.writer.running) return;
            try {
                var value = JSON.parse(text());
                if (Mosaic.valid(value)) root.set = value;
            } catch (e) {
                // A broken file forgets nothing already kept.
            }
        }
    }
    property Process writer: Process {
        command: ["true"]
        onExited: function (exitCode) {
            root.error = exitCode === 0 ? "" : "Could not write mosaic.json";
            if (root.pending) root.flush();
        }
    }
}
