import QtQuick
import Quickshell
import Quickshell.Io
import "Toml.js" as Toml

QtObject {
    id: root
    property bool registerIpc: true
    readonly property string themePath: Quickshell.env("OMASTORM_THEME_DIR") || (Quickshell.env("HOME") + "/.local/state/omarchy/current/theme")
    readonly property string userPath: Quickshell.env("OMASTORM_USER_SHELL") || (Quickshell.env("HOME") + "/.config/omarchy/shell.toml")
    property var colors: ({})
    property var shell: ({})
    property var user: ({})

    function parse(raw) { return Toml.parse(raw); }
    function setting(key, fallback) {
        return user[key] !== undefined ? user[key] : shell[key] !== undefined ? shell[key] : fallback;
    }
    function color(value, fallback) {
        var resolved = colors[value] !== undefined ? colors[value] : value;
        return typeof resolved === "string" && /^#[0-9a-fA-F]{6}$/.test(resolved) ? resolved : fallback;
    }
    /// S38: whether the active Omarchy theme is a light one. Omarchy's
    /// colors.toml says so itself (`mode = "light" | "dark"`); a theme
    /// written before that key, or a custom one, is judged by the
    /// background's luminance instead. Everything the map paints reads this
    /// one flag, so switching Omarchy themes switches the radar with it.
    function isLight(bg) {
        if (colors.mode === "light") return true;
        if (colors.mode === "dark") return false;
        var hex = /^#[0-9a-fA-F]{6}$/.test(bg) ? bg : "#1a1b26";
        var r = parseInt(hex.substr(1, 2), 16) / 255;
        var g = parseInt(hex.substr(3, 2), 16) / 255;
        var b = parseInt(hex.substr(5, 2), 16) / 255;
        // Rec. 709 luminance; halfway is the only sensible cut.
        return 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.5;
    }
    readonly property var snapshot: ({
        background: color(setting("popups.background", colors.background), "#1a1b26"),
        foreground: color(setting("popups.text", colors.foreground), "#a9b1d6"),
        accent: color(colors.accent, "#7aa2f7"),
        // Condition colours (DESIGN.md): stale is the
        // theme's yellow, unavailable and offline its red.
        yellow: color(colors.yellow, "#e0af68"),
        red: color(colors.red, "#f7768e"),
        // S38: a radar mark is not a city. Antennas take the theme's cyan
        // where it has one, its green next, and the accent as a last
        // resort; city dots and labels keep the foreground.
        antenna: color(colors.cyan, color(colors.green, color(colors.accent, "#7aa2f7"))),
        light: isLight(color(setting("popups.background", colors.background), "#1a1b26")),
        // Both Text and Canvas resolve the generic family through Qt/fontconfig.
        font: "monospace",
        baseSize: Number(setting("font.base-size", 12)) > 0 ? Number(setting("font.base-size", 12)) : 12
    })
    function reload() {
        colorsFile.reload();
        shellFile.reload();
        userFile.reload();
    }
    property FileView colorsFile: FileView {
        path: root.themePath + "/colors.toml"
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: root.colors = root.parse(text())
        onLoadFailed: root.colors = ({})
    }
    property FileView shellFile: FileView {
        path: root.themePath + "/shell.toml"
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: root.shell = root.parse(text())
        onLoadFailed: root.shell = ({})
    }
    property FileView userFile: FileView {
        path: root.userPath
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: root.user = root.parse(text())
        onLoadFailed: root.user = ({})
    }
    property IpcHandler ipc: IpcHandler {
        target: root.registerIpc ? "theme" : ""
        function reload(): void { root.reload(); }
    }
}
