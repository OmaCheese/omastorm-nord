import QtQuick
import QtQuick.Layouts
import "Keys.js" as KeyMap

// The `?` sheet (DESIGN.md, picker and keys; keyboard map as built) after
// the KeyboardHints canvas: a scrim over the window and a card with KEYS,
// where the bindings live, two columns of key caps and what they do from
// the bindings in force, and a footer. `?` or Escape, or a click on the
// scrim, closes it; the window's own shortcuts stand down while it is open.
Item {
    id: sheet
    property var theme
    property var bindings: ({})       // action id -> canonical sequences
    property bool compact: false
    property real cardTop: 20         // the map's top edge; the card sits below it
    property bool open: false
    /// S46: the card, for the host's checks and captures.
    readonly property Item cardItem: card
    readonly property var columns: KeyMap.sheet(bindings)
    // What MosaicPicker.qml's keys do; not rebindable.
    readonly property var mosaicKeys: [
        { caps: ["↑", "↓", "j", "k"], label: "move" },
        { caps: ["space", "x"], label: "tick" },
        { caps: ["/"], label: "filter (esc back)" },
        { caps: ["r"], label: "rule" },
        { caps: ["[", "]"], label: "height" },
        { caps: ["↵"], label: "show" },
        { caps: ["esc"], label: "cancel" }
    ]
    readonly property string closeKeys: (bindings.close || []).map(KeyMap.pretty).join(" or ")
    component Word: Text {
        color: sheet.theme.foreground
        font.family: sheet.theme.font
        font.pixelSize: sheet.theme.size.body
        elide: Text.ElideRight
        verticalAlignment: Text.AlignVCenter
    }
    visible: open
    focus: open
    function show() { open = true; forceActiveFocus(); }
    function close() { open = false; }
    Keys.onPressed: event => {
        if (!open) return;
        if (event.key === Qt.Key_Escape || event.text === "?") { close(); event.accepted = true; }
    }
    Rectangle {
        anchors.fill: parent
        color: Qt.alpha(sheet.theme.background, .5)
        MouseArea { anchors.fill: parent; acceptedButtons: Qt.AllButtons; onClicked: sheet.close() }
    }
    Rectangle {
        id: card
        width: Math.min(Math.round(660 * Math.max(1, sheet.theme.size.k)), sheet.width - 40)
        x: Math.round((sheet.width - width) / 2)
        y: Math.round(Math.max(20, Math.min(sheet.cardTop + 20, sheet.height - height - 20)))
        height: column.implicitHeight + 36
        color: Qt.alpha(sheet.theme.background, .97)
        border.width: 1
        border.color: sheet.theme.foreground
        MouseArea { anchors.fill: parent; acceptedButtons: Qt.AllButtons } // any click on the card stays on the card
        ColumnLayout {
            id: column
            anchors.fill: parent
            anchors.margins: 18
            spacing: 12
            RowLayout {
                Layout.fillWidth: true
                spacing: 10
                Word { text: "KEYS"; font.bold: true; font.pixelSize: sheet.theme.size.title; font.letterSpacing: 2 }
                Item { Layout.fillWidth: true }
                Word { text: "rebindable in ~/.config/omastorm-nord/config.toml"; font.pixelSize: sheet.theme.size.small; opacity: .55 }
            }
            GridLayout {
                Layout.fillWidth: true
                columns: sheet.compact ? 1 : 2
                columnSpacing: 28
                rowSpacing: 0
                Repeater {
                    model: sheet.columns
                    ColumnLayout {
                        id: keyColumn
                        required property var modelData
                        Layout.fillWidth: true
                        Layout.alignment: Qt.AlignTop
                        spacing: 0
                        Repeater {
                            model: keyColumn.modelData
                            RowLayout {
                                id: row
                                required property var modelData
                                Layout.fillWidth: true
                                Layout.preferredHeight: Math.round(26 * Math.max(1, sheet.theme.size.k))
                                spacing: 8
                                Row {
                                    Layout.preferredWidth: Math.round(124 * Math.max(1, sheet.theme.size.k))
                                    Layout.minimumWidth: Math.round(124 * Math.max(1, sheet.theme.size.k))
                                    spacing: 4
                                    Repeater {
                                        model: row.modelData.caps
                                        Rectangle {
                                            required property string modelData
                                            width: Math.max(18, cap.implicitWidth + 10)
                                            height: 18
                                            color: "transparent"
                                            border.width: 1
                                            border.color: Qt.alpha(sheet.theme.foreground, .4)
                                            Word { id: cap; anchors.centerIn: parent; text: parent.modelData; font.pixelSize: sheet.theme.size.caption }
                                        }
                                    }
                                }
                                Word { text: row.modelData.label; opacity: .85; Layout.fillWidth: true }
                            }
                        }
                    }
                }
            }
            // S40: My mosaic's panel has its own keys, fixed, in force while
            // it is open (the window's stand down then).
            Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(sheet.theme.foreground, .17) }
            Word { text: "MY MOSAIC PANEL"; font.pixelSize: sheet.theme.size.small; font.bold: true; font.letterSpacing: 1; opacity: .6 }
            Flow {
                Layout.fillWidth: true
                spacing: 14
                Repeater {
                    model: sheet.mosaicKeys
                    Row {
                        id: mosaicKey
                        required property var modelData
                        spacing: 4
                        Repeater {
                            model: mosaicKey.modelData.caps
                            Rectangle {
                                required property string modelData
                                width: Math.max(18, mosaicCap.implicitWidth + 10)
                                height: 18
                                color: "transparent"
                                border.width: 1
                                border.color: Qt.alpha(sheet.theme.foreground, .4)
                                Word { id: mosaicCap; anchors.centerIn: parent; text: parent.modelData; font.pixelSize: sheet.theme.size.caption }
                            }
                        }
                        Word { text: mosaicKey.modelData.label; height: 18; opacity: .85; leftPadding: 2 }
                    }
                }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(sheet.theme.foreground, .17) }
            Word {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                elide: Text.ElideNone
                font.pixelSize: sheet.theme.size.small
                opacity: .55
                lineHeight: 1.4
                text: sheet.closeKeys
                    ? "With nothing else open, " + sheet.closeKeys + " closes the window."
                    : "No key closes the window."
            }
        }
    }
}
