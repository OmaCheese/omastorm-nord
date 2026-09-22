import QtQuick
import Quickshell
import Quickshell.Io

// The weather layers' switches (S42): Radar, Temperature, Wind, remembered
// across restarts in layers.json beside state.json (as mosaic.json is).
// Radar is on and the station layers off until someone says otherwise.
// OMASTORM_LAYERS ("radar,temp,wind", any subset, "none" for none) outranks
// the file for checks and captures and is never written back; OMASTORM_STATE
// puts the file beside that one; OMASTORM_CONFIG without OMASTORM_STATE keeps
// it in memory.
QtObject {
    id: root
    readonly property string forced: Quickshell.env("OMASTORM_LAYERS") || ""
    readonly property string path: {
        if (forced) return "";
        var state = Quickshell.env("OMASTORM_STATE");
        if (state) return state.slice(0, state.lastIndexOf("/") + 1) + "layers.json";
        if (Quickshell.env("OMASTORM_CONFIG")) return "";
        return (Quickshell.env("XDG_STATE_HOME") || Quickshell.env("HOME") + "/.local/state") + "/omastorm-se/layers.json";
    }
    property bool radar: true
    property bool temp: false
    property bool wind: false
    readonly property bool any: temp || wind
    property string error: ""
    // Review N7: the file this store just wrote reads back unchanged.
    property string written: ""
    function parse(text) {
        if (text.trim() === written) return;
        try {
            var v = JSON.parse(text);
            if (!v || typeof v !== "object") return;
            radar = v.radar !== false;
            temp = v.temp === true;
            wind = v.wind === true;
        } catch (e) { /* a bad file keeps the defaults */ }
    }
    function set(name, on) {
        if (name !== "radar" && name !== "temp" && name !== "wind") return;
        if (root[name] === on) return;
        root[name] = on;
        save();
    }
    function toggle(name) { set(name, !root[name]); }
    function save() {
        if (!path) return;
        var text = JSON.stringify({ radar: radar, temp: temp, wind: wind });
        written = text;
        var slash = path.lastIndexOf("/");
        var dir = slash >= 0 ? path.slice(0, slash) : ".";
        writer.command = ["sh", "-c",
            "mkdir -p -- \"$1\" && printf '%s\\n' \"$3\" > \"$2\" && mv -f -- \"$2\" \"$4\"",
            "omastorm-layers", dir, path + ".tmp", text, path];
        writer.running = true;
    }
    property Process writer: Process {
        command: ["true"]
        onExited: function (exitCode) { root.error = exitCode === 0 ? "" : "Could not write layers.json"; }
    }
    property FileView file: FileView {
        path: root.path
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: root.parse(text())
    }
    Component.onCompleted: {
        if (!forced) return;
        var parts = forced.split(",").map(s => s.trim());
        radar = parts.indexOf("radar") >= 0;
        temp = parts.indexOf("temp") >= 0;
        wind = parts.indexOf("wind") >= 0;
    }
}
