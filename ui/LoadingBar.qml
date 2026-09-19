// S35: the load as one segmented bar. `state.loading.stages` is the whole
// load in order; each segment is as wide as its `share` (the shares sum to
// 100, so the layout comes from the engine and this file invents nothing)
// and fills by its own `percent`. A segment already done stays full and a
// segment still to come is dim, so the bar reads as one thing while the
// stages change under it, instead of a number that starts again three
// times. An engine older than S35 sends no `stages`: the bar is then the
// single one S31 drew, from the top-level `percent`
// (docs/protocol.md, Loading progress).
import QtQuick

Item {
    id: bar

    /// `state.loading`, or null while nothing is loading.
    property var loading: null
    /// The host's palette (Theme.qml): `accent` and `foreground`.
    property var theme
    /// Name the stages under the segments. The window has the room for it;
    /// the popover's strip does not.
    property bool names: false
    /// The bar's own thickness, the thin rule S31 drew.
    property int thickness: 2
    /// Between two segments, so the divisions read at 2 px.
    property int gap: 2

    /// The segments to draw: the engine's stages, or one segment holding
    /// the whole bar when the engine is older than S35.
    readonly property var segments: {
        const l = bar.loading;
        if (!l)
            return [];
        const stages = l.stages;
        if (stages && stages.length > 0)
            return stages;
        return [{ "stage": l.stage, "share": 100, "percent": l.percent, "state": "active" }];
    }

    /// Each segment's width in pixels. The last one takes what rounding
    /// left over, so the segments always fill the bar exactly.
    readonly property var widths: {
        const n = bar.segments.length;
        if (n < 1 || bar.width <= 0)
            return [];
        const room = Math.max(0, bar.width - bar.gap * (n - 1));
        let out = [];
        let used = 0;
        for (let i = 0; i < n; i++) {
            const share = Math.max(0, Math.min(100, bar.segments[i].share || 0));
            const w = i === n - 1 ? room - used : Math.round(room * share / 100);
            out.push(Math.max(0, w));
            used += out[i];
        }
        return out;
    }

    /// A segment's left edge.
    function edge(index) {
        let x = 0;
        for (let i = 0; i < index; i++)
            x += (bar.widths[i] || 0) + bar.gap;
        return x;
    }

    /// The stage in words, under its segment.
    function stageName(stage) {
        return stage === "first" ? "first frame" : stage === "build" ? "building" : stage === "history" ? "history" : stage || "";
    }

    visible: !!bar.loading
    implicitHeight: bar.thickness + (bar.names ? nameRow.implicitHeight + 3 : 0)
    // Review NIT4: an Item anchored left/right/bottom keeps its default
    // height of 0 unless it is given one, and its children then hang below
    // the anchor line. The rule is drawn at the top of this item, so a
    // host anchors the bar's bottom at `-thickness - implicitHeight` from
    // where it wants the rule, names or no names.
    height: bar.implicitHeight

    Repeater {
        model: bar.segments
        Rectangle {
            required property var modelData
            required property int index
            readonly property bool waiting: modelData.state === "waiting"

            x: bar.edge(index)
            y: 0
            width: bar.widths[index] || 0
            height: bar.thickness
            radius: Math.floor(bar.thickness / 2)
            // The track: dimmer for a stage that has not started, so the
            // bar says at a glance how much of the load is still to come.
            color: Qt.alpha(bar.theme.accent, waiting ? 0.14 : 0.22)

            Rectangle {
                height: parent.height
                radius: parent.radius
                // A finished stage stays full whatever its last count was.
                width: modelData.state === "done" ? parent.width : Math.round(parent.width * Math.max(0, Math.min(100, modelData.percent || 0)) / 100)
                color: bar.theme.accent
                opacity: parent.waiting ? 0 : 1
                Behavior on width {
                    NumberAnimation {
                        duration: 300
                    }
                }
            }
        }
    }

    Item {
        id: nameRow
        visible: bar.names
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.topMargin: bar.thickness + 3
        implicitHeight: 11
        height: visible ? implicitHeight : 0

        Repeater {
            model: bar.names ? bar.segments : []
            Text {
                required property var modelData
                required property int index

                x: bar.edge(index)
                width: bar.widths[index] || 0
                height: nameRow.implicitHeight
                text: bar.stageName(modelData.stage)
                font.pixelSize: 9
                elide: Text.ElideRight
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
                color: bar.theme.accent
                // The stage running is the one to read; the others are
                // there for the shape of the load, not for attention.
                opacity: modelData.state === "active" ? 0.85 : modelData.state === "done" ? 0.45 : 0.30
            }
        }
    }
}
