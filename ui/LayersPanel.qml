import QtQuick
import QtQuick.Layouts

// The LAYERS panel (S42): a card below the LAYERS chip, like the treatment
// menu, with three switches: Radar (as before), Temperature and Wind from
// the weather stations. Each is on or off on its own and remembered
// (LayersStore); radar and the station layers show together. A click
// outside, Escape or the layers key closes it; Up, Down (or j, k) move,
// Space or Return flips the row, 1 2 3 flip a row directly. S43: a fourth
// row chooses where temperature and wind come from, STATIONS, GRID (the MET
// Nordic analysis) or BOTH; Left and Right (h, l) or Space move along it, 5
// steps it (review S4).
Item {
    id: panel
    property var theme
    property var store              // LayersStore
    property Item chip              // the card sits below this chip
    property string keyText: ""     // the layers key as the sheet writes it
    property var obs: null          // the engine's obs, for the status line
    property var grid: null         // the engine's grid obs (S43)
    property bool opened: false
    property int cursor: 0
    visible: opened
    focus: opened
    readonly property string source: store ? store.source : "stations"
    readonly property string fromWord: source === "grid" ? "MET Nordic grid" : source === "both" ? "grid + stations" : "stations"
    readonly property var rows: [
        { id: "radar", label: "RADAR", note: "precipitation" },
        { id: "temp", label: "TEMPERATURE", note: fromWord + ", °C" },
        { id: "wind", label: "WIND", note: fromWord + ", arrows" }
    ]
    readonly property var sourceNames: [["stations", "STATIONS"], ["grid", "GRID"], ["both", "BOTH"]]
    readonly property int sourceRow: rows.length
    function show() { cursor = 0; opened = true; forceActiveFocus(); }
    function close() { opened = false; }
    function flip(index) { if (store && index >= 0 && index < rows.length) store.toggle(rows[index].id); }
    Keys.onPressed: event => {
        if (!opened) return;
        event.accepted = true;
        if (event.key === Qt.Key_Escape || (event.key === Qt.Key_L && (event.modifiers & Qt.ControlModifier))) close();
        else if (event.key === Qt.Key_Up || event.key === Qt.Key_K) cursor = Math.max(0, cursor - 1);
        else if (event.key === Qt.Key_Down || event.key === Qt.Key_J) cursor = Math.min(sourceRow, cursor + 1);
        else if (cursor === sourceRow && (event.key === Qt.Key_Left || event.key === Qt.Key_H)) store.cycleSource(-1);
        else if (cursor === sourceRow && (event.key === Qt.Key_Right || event.key === Qt.Key_L)) store.cycleSource(1);
        else if (event.key === Qt.Key_Space || event.key === Qt.Key_Return || event.key === Qt.Key_Enter) { if (cursor === sourceRow) store.cycleSource(1); else flip(cursor); }
        else if (event.key >= Qt.Key_1 && event.key <= Qt.Key_3) { cursor = event.key - Qt.Key_1; flip(cursor); }
        else if (event.key === Qt.Key_5) { cursor = sourceRow; store.cycleSource(1); }
        else event.accepted = false;
    }
    // Norway's Frost needs a client ID; the panel says why there is no
    // Norwegian station rather than leaving a hole unexplained.
    readonly property string stationStatus: {
        if (!obs) return "STATIONS LOADING…";
        var out = [];
        for (var p of obs.providers || []) {
            if (p.status === "skipped" && p.id === "frost") out.push("NORWAY: NO FROST ID");
            else if (p.status === "failed") out.push(p.name.toUpperCase() + ": FAILED");
        }
        var n = (obs.stations || []).length;
        return n + " STATIONS" + (out.length ? " · " + out.join(" · ") : "");
    }
    // S43: the grid's hour in local time, or why there is none.
    readonly property string gridStatus: {
        if (!grid) return "GRID LOADING…";
        var failed = grid.provider && grid.provider.status === "failed";
        if (!grid.time) return "GRID: " + (failed ? "FAILED" : "NONE");
        var d = new Date(grid.time);
        return "GRID " + Qt.formatTime(d, Qt.locale().timeFormat(Locale.ShortFormat)) + (failed ? " · UPDATE FAILED" : "");
    }
    readonly property string status: {
        if (!store || !(store.temp || store.wind)) return "";
        var out = [];
        if (source !== "grid") out.push(stationStatus);
        if (source !== "stations") out.push(gridStatus);
        return out.join(" · ");
    }
    component Word: Text {
        color: panel.theme ? panel.theme.foreground : "#a9b1d6"
        font.family: panel.theme ? panel.theme.font : "monospace"
        font.pixelSize: panel.theme ? panel.theme.baseSize : 12
        elide: Text.ElideRight
    }
    MouseArea { anchors.fill: parent; onClicked: panel.close() }
    Rectangle {
        id: card
        // Below the chip, its left edge on the chip's (the chip sits in the
        // header); kept inside the surface.
        readonly property point anchor: panel.opened && panel.chip ? panel.chip.mapToItem(panel, 0, panel.chip.height) : Qt.point(0, 0)
        x: Math.round(Math.max(6, Math.min(panel.width - width - 6, anchor.x)))
        y: Math.round(Math.min(panel.height - height - 6, anchor.y + 6))
        width: 250
        height: column.implicitHeight + 12
        color: Qt.alpha(panel.theme ? panel.theme.background : "#1a1b26", .95)
        border.width: 1
        border.color: panel.theme ? panel.theme.foreground : "#a9b1d6"
        MouseArea { anchors.fill: parent } // a click on the card stays on the card
        ColumnLayout {
            id: column
            anchors.fill: parent
            anchors.margins: 6
            spacing: 0
            RowLayout {
                Layout.fillWidth: true
                Layout.leftMargin: 10; Layout.rightMargin: 10
                Layout.bottomMargin: 4
                Word { text: "LAYERS"; font.pixelSize: 10; opacity: .6; Layout.fillWidth: true }
                Word { text: panel.keyText; font.pixelSize: 10; opacity: .45 }
            }
            Repeater {
                model: panel.rows
                Rectangle {
                    id: row
                    required property var modelData
                    required property int index
                    readonly property bool on: !!panel.store && !!panel.store[modelData.id]
                    readonly property bool hot: rowArea.containsMouse || panel.cursor === index
                    readonly property color accent: panel.theme ? panel.theme.accent : "#7aa2f7"
                    readonly property color ink: hot ? accent : panel.theme ? panel.theme.foreground : "#a9b1d6"
                    Layout.fillWidth: true
                    implicitHeight: 36
                    color: hot ? Qt.alpha(panel.theme ? panel.theme.foreground : "#a9b1d6", .08) : "transparent"
                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 10
                        anchors.rightMargin: 10
                        spacing: 8
                        ColumnLayout {
                            spacing: 0
                            Layout.fillWidth: true
                            Word { text: row.modelData.label; color: row.ink; Layout.fillWidth: true }
                            Word { text: row.modelData.note; font.pixelSize: 9; opacity: .55; Layout.fillWidth: true }
                        }
                        Word { text: String(row.index + 1); font.pixelSize: 10; opacity: .45 }
                        // The switch: a track with a knob, accent when on.
                        Rectangle {
                            id: track
                            implicitWidth: 30; implicitHeight: 16
                            radius: 8
                            color: row.on ? row.accent : "transparent"
                            border.width: 1
                            border.color: row.on ? row.accent : Qt.alpha(panel.theme ? panel.theme.foreground : "#a9b1d6", .45)
                            Rectangle {
                                width: 10; height: 10; radius: 5
                                y: 3
                                x: row.on ? track.width - width - 3 : 3
                                color: row.on ? (panel.theme ? panel.theme.background : "#1a1b26") : Qt.alpha(panel.theme ? panel.theme.foreground : "#a9b1d6", .7)
                                Behavior on x { NumberAnimation { duration: 90 } }
                            }
                        }
                    }
                    MouseArea {
                        id: rowArea
                        anchors.fill: parent
                        hoverEnabled: true
                        onClicked: { panel.cursor = row.index; panel.flip(row.index); }
                    }
                }
            }
            // S43: where temperature and wind come from.
            Rectangle {
                id: sourceRowItem
                readonly property bool hot: sourceArea.containsMouse || panel.cursor === panel.sourceRow
                Layout.fillWidth: true
                implicitHeight: 36
                color: hot ? Qt.alpha(panel.theme ? panel.theme.foreground : "#a9b1d6", .08) : "transparent"
                MouseArea { id: sourceArea; anchors.fill: parent; hoverEnabled: true; onClicked: panel.cursor = panel.sourceRow }
                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 10
                    anchors.rightMargin: 10
                    spacing: 4
                    Word { text: "FROM"; font.pixelSize: 10; opacity: .6; Layout.preferredWidth: 38 }
                    Repeater {
                        model: panel.sourceNames
                        Rectangle {
                            id: seg
                            required property var modelData
                            readonly property bool on: panel.source === modelData[0]
                            readonly property color accent: panel.theme ? panel.theme.accent : "#7aa2f7"
                            Layout.fillWidth: true
                            implicitHeight: 20
                            radius: 3
                            color: on ? accent : segArea.containsMouse ? Qt.alpha(accent, .18) : "transparent"
                            border.width: 1
                            border.color: on ? accent : Qt.alpha(panel.theme ? panel.theme.foreground : "#a9b1d6", .35)
                            Word {
                                anchors.centerIn: parent
                                text: seg.modelData[1]
                                font.pixelSize: 9
                                color: seg.on ? (panel.theme ? panel.theme.background : "#1a1b26") : (panel.theme ? panel.theme.foreground : "#a9b1d6")
                            }
                            MouseArea {
                                id: segArea
                                anchors.fill: parent
                                hoverEnabled: true
                                onClicked: { panel.cursor = panel.sourceRow; panel.store.setSource(seg.modelData[0]); }
                            }
                        }
                    }
                    Word { text: "5"; font.pixelSize: 10; opacity: .45; Layout.leftMargin: 4 }
                }
            }
            Word {
                visible: text !== ""
                text: panel.status
                font.pixelSize: 9
                opacity: .55
                Layout.fillWidth: true
                Layout.leftMargin: 10; Layout.rightMargin: 10
                Layout.topMargin: 4
                wrapMode: Text.Wrap
            }
        }
    }
}
