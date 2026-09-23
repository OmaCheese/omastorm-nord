// S41: the load while nothing is drawn yet, centred over the map: what is
// loading, one overall percentage, the segmented bar (LoadingBar), and the
// steps by name with ✓ done, ● running and ○ still to come. Once a frame is
// on screen the host hides it and the timeline's bar says the rest
// (Engine.qml, `steps`, `percent`, `drawn`).
import QtQuick

Rectangle {
    id: card

    /// The surface's Engine.
    property var engine
    /// The host's palette (Theme.qml).
    property var theme
    /// The popover's narrower card: smaller type, no step details.
    property bool compact: false

    readonly property var loading: engine ? engine.loading : null
    readonly property var steps: engine ? engine.steps : []
    /// An overall number exists once the engine counts (S31 and later).
    readonly property bool counted: !!loading

    implicitWidth: compact ? 248 : 320
    implicitHeight: column.implicitHeight + 2 * pad
    readonly property int pad: compact ? 12 : 16
    color: Qt.alpha(theme.background, 0.94)
    border.width: 1
    border.color: Qt.alpha(theme.foreground, 0.22)

    Column {
        id: column
        x: card.pad
        y: card.pad
        width: card.width - 2 * card.pad
        spacing: card.compact ? 8 : 10

        Item {
            width: parent.width
            height: Math.max(title.implicitHeight, big.implicitHeight)
            Column {
                anchors.left: parent.left
                anchors.right: big.left
                anchors.rightMargin: 8
                anchors.verticalCenter: parent.verticalCenter
                spacing: 2
                Text {
                    id: title
                    width: parent.width
                    text: "LOADING"
                    font.family: card.theme.font
                    font.pixelSize: card.compact ? card.theme.size.small : card.theme.size.caption
                    font.letterSpacing: 1
                    color: card.theme.foreground
                    opacity: 0.6
                }
                Text {
                    width: parent.width
                    text: card.engine ? card.engine.loadName : ""
                    visible: text !== ""
                    elide: Text.ElideRight
                    font.family: card.theme.font
                    font.pixelSize: card.compact ? card.theme.size.caption : card.theme.size.label
                    color: card.theme.foreground
                }
            }
            Text {
                id: big
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                // Before the engine counts there is no honest number.
                text: card.counted ? card.engine.percent + " %" : "…"
                font.family: card.theme.font
                font.pixelSize: card.compact ? card.theme.size.body * 2 : Math.round(card.theme.size.body * 8 / 3)
                font.bold: true
                color: card.theme.accent
            }
        }

        LoadingBar {
            width: parent.width
            // No count yet: an empty track, so the card keeps its shape.
            loading: card.loading || ({ "stage": "first", "percent": 0, "label": "", "stages": [{ "stage": "first", "share": 100, "percent": 0, "state": "waiting" }] })
            theme: card.theme
            thickness: card.compact ? 4 : 6
            gap: 3
        }

        // S47: the weather layers that are on, loading (●), in (✓) or
        // failed (✗), with what failed.
        Column {
            width: parent.width
            spacing: 2
            visible: card.engine && card.engine.layerStates && card.engine.layerStates.length > 0
            Repeater {
                model: card.engine ? card.engine.layerStates : []
                Text {
                    required property var modelData
                    width: parent.width
                    text: (modelData.state === "loading" ? "● " : modelData.state === "error" ? "✗ " : "✓ ") + modelData.text
                    elide: Text.ElideRight
                    font.family: card.theme.font
                    font.pixelSize: card.compact ? card.theme.size.small : card.theme.size.caption
                    color: modelData.state === "loading" ? card.theme.accent : modelData.state === "error" && card.theme.red ? card.theme.red : card.theme.foreground
                    opacity: modelData.state === "ok" ? 0.7 : 1
                }
            }
        }

        Column {
            width: parent.width
            spacing: card.compact ? 3 : 5
            Repeater {
                model: card.steps
                Column {
                    required property var modelData
                    readonly property bool active: modelData.state === "active"
                    readonly property bool done: modelData.state === "done"
                    width: parent.width
                    spacing: 1
                    Row {
                        spacing: 8
                        Text {
                            id: glyph
                            width: card.compact ? 12 : 14
                            text: parent.parent.done ? "✓" : parent.parent.active ? "●" : "○"
                            font.family: card.theme.font
                            font.pixelSize: card.compact ? card.theme.size.caption : card.theme.size.label
                            color: parent.parent.active || parent.parent.done ? card.theme.accent : card.theme.foreground
                            opacity: parent.parent.active || parent.parent.done ? 1 : 0.45
                            SequentialAnimation on opacity {
                                running: glyph.parent.parent.active
                                loops: Animation.Infinite
                                alwaysRunToEnd: true
                                NumberAnimation { to: 0.35; duration: 600 }
                                NumberAnimation { to: 1; duration: 600 }
                            }
                        }
                        Text {
                            text: modelData.name
                            font.family: card.theme.font
                            font.pixelSize: card.compact ? card.theme.size.caption : card.theme.size.label
                            font.bold: parent.parent.active
                            color: parent.parent.active ? card.theme.accent : card.theme.foreground
                            opacity: parent.parent.active ? 1 : parent.parent.done ? 0.75 : 0.45
                        }
                    }
                    Text {
                        x: (card.compact ? 12 : 14) + 8
                        width: parent.width - x
                        visible: parent.active && modelData.detail !== ""
                        text: modelData.detail
                        wrapMode: Text.Wrap
                        maximumLineCount: card.compact ? 2 : 3
                        elide: Text.ElideRight
                        font.family: card.theme.font
                        font.pixelSize: card.compact ? card.theme.size.small : card.theme.size.caption
                        color: card.theme.foreground
                        opacity: 0.8
                    }
                }
            }
        }
    }
}
