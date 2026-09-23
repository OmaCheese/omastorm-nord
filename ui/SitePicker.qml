import QtQuick
import QtQuick.Layouts
import "Sites.js" as Sites

// The fuzzy site picker (DESIGN.md, picker and keys; picker as built) in the
// launcher's style: a scrim over the window, a card with an input row, every
// match (S48: a list that scrolls, as tall as the window lets it) with the
// matched letters in accent and each station's distance and bearing from the
// map centre, and a hint footer. Typing filters and goes back to the top, the
// wheel scrolls, up and down (page up and down, home and end) move and keep
// the selection in view, Enter hands the station to `chosen` (lock and
// centre), Escape or a click on the scrim closes. The keys are handled here
// the window's own shortcuts stand down.
Item {
    id: picker
    property var sites: []            // hello.sites
    property var theme
    property real centerLat: 0        // the map centre the distances are from
    property real centerLon: 0
    property string homeSite: ""      // the configured home, marked in its row
    property bool compact: false
    property real cardTop: 20         // where the card's top edge sits
    // S46: the chip that opens the picker (the header's site name). The
    // card drops down under it like a menu, left edges aligned, kept
    // inside the window; with none (or hidden) it sits centred at cardTop.
    property Item anchorItem: null
    /// The card, for the host's wheel guard, checks and captures.
    readonly property Item cardItem: card
    property bool open: false
    property alias query: field.text
    // Whether the field holds the keyboard; closed, it must not.
    readonly property bool fieldFocused: field.activeFocus
    property int selected: 0
    /// S48: the list, for checks (its scroll) and the wheel.
    readonly property Item listItem: list
    readonly property int rowHeight: Math.round(28 * Math.max(1, theme.size.k))
    /// What a check needs of the scroll: where the list is, which rows it
    /// shows whole, and whether the selection is one of them.
    function viewStatus() {
        var first = Math.ceil(list.contentY / rowHeight), last = Math.floor((list.contentY + list.height) / rowHeight) - 1;
        return { contentY: Math.round(list.contentY), height: Math.round(list.height), contentHeight: rows.length * rowHeight,
                 rowHeight: rowHeight, first: first, last: Math.min(last, rows.length - 1), selected: selected,
                 selectedVisible: selected >= first && selected <= last, overflow: list.overflows, footer: footer.text,
                 card: { y: Math.round(card.y), h: Math.round(card.height) }, window: Math.round(height) };
    }
    component Word: Text {
        color: picker.theme.foreground
        font.family: picker.theme.font
        font.pixelSize: picker.theme.size.body
        elide: Text.ElideRight
        verticalAlignment: Text.AlignVCenter
    }
    signal chosen(var site)
    // S48: every match, no row limit; the ranking and the group hairlines stay.
    readonly property var ranked: open ? Sites.rank(sites, query, centerLat, centerLon, Infinity, Qt.locale().measurementSystem === Locale.MetricSystem) : ({ rows: [], total: 0 })
    readonly property var rows: ranked.rows
    visible: open
    function show(text) {
        field.text = text || "";
        selected = 0;
        open = true;
        list.contentY = 0;
        Qt.callLater(() => { field.cursorPosition = field.length; field.forceActiveFocus(); });
    }
    // Closing hands the keyboard back: a hidden field that kept active focus
    // would swallow the next `/` as text instead of the search shortcut.
    function close() { open = false; field.text = ""; selected = 0; field.focus = false; }
    function accept() {
        if (!open || !rows.length) return;
        var s = rows[Math.min(selected, rows.length - 1)].site;
        close();
        chosen(s);
    }
    // Keyboard moves keep the selection in view; hover does not scroll.
    function move(delta) { select(selected + delta); }
    function select(index) {
        selected = Math.max(0, Math.min(rows.length - 1, index));
        if (rows.length) list.positionViewAtIndex(selected, ListView.Contain);
    }
    /// Rows a page key moves: the ones the list shows whole, less one.
    readonly property int pageRows: Math.max(1, Math.floor(list.height / rowHeight) - 1)
    onQueryChanged: { selected = 0; list.contentY = 0; }
    Rectangle {
        anchors.fill: parent
        color: Qt.alpha(picker.theme.background, .5)
        MouseArea { anchors.fill: parent; acceptedButtons: Qt.AllButtons; onClicked: picker.close() }
    }
    Rectangle {
        id: card
        width: Math.min(Math.round(520 * Math.max(1, picker.theme.size.k)), picker.width - 12)
        // Where the chip's bottom-left corner is, measured again at each
        // open and whenever the window changes size.
        readonly property point anchor: picker.open && picker.anchorItem && picker.anchorItem.visible && picker.width > 0 && picker.height > 0
            ? picker.anchorItem.mapToItem(picker, 0, picker.anchorItem.height) : Qt.point(NaN, NaN)
        readonly property bool anchored: !isNaN(anchor.x)
        x: Math.round(anchored ? Math.max(6, Math.min(picker.width - width - 6, anchor.x)) : (picker.width - width) / 2)
        // S48: the top stays put (under the chip, or at cardTop) and the
        // list takes the room down to 6 px above the window's bottom; only
        // when even one row does not fit does the card move up.
        readonly property real topEdge: anchored ? Math.max(6, anchor.y + 6) : Math.max(20, picker.cardTop)
        // Everything on the card but the list: margins, input row, hairline,
        // footer and the three gaps between them.
        readonly property real chrome: 2 * column.anchors.margins + inputRow.implicitHeight + 1 + footer.implicitHeight + 3 * column.spacing
        readonly property real listRoom: Math.max(picker.rowHeight, picker.height - 6 - topEdge - chrome)
        y: Math.round(Math.max(6, Math.min(topEdge, picker.height - height - 6)))
        height: column.implicitHeight + 2 * column.anchors.margins
        color: Qt.alpha(picker.theme.background, .95)
        border.width: 1
        border.color: picker.theme.foreground
        MouseArea { anchors.fill: parent; acceptedButtons: Qt.AllButtons } // any click on the card stays on the card
        ColumnLayout {
            id: column
            anchors.fill: parent
            anchors.margins: 14
            spacing: 10
            Rectangle {
                id: inputRow
                Layout.fillWidth: true
                implicitHeight: Math.round(36 * Math.max(1, picker.theme.size.k))
                color: Qt.alpha(picker.theme.foreground, .04)
                border.width: 1
                border.color: Qt.alpha(picker.theme.foreground, .4)
                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 10
                    anchors.rightMargin: 10
                    spacing: 10
                    Canvas {
                        implicitWidth: 16; implicitHeight: 16
                        property color ink: picker.theme.foreground
                        onInkChanged: requestPaint()
                        onPaint: {
                            var ctx = getContext("2d");
                            ctx.reset(); ctx.clearRect(0, 0, width, height);
                            ctx.strokeStyle = ink; ctx.lineWidth = 1.5;
                            ctx.beginPath(); ctx.arc(6.5, 6.5, 4.5, 0, 2 * Math.PI); ctx.stroke();
                            ctx.beginPath(); ctx.moveTo(10, 10); ctx.lineTo(14, 14); ctx.stroke();
                        }
                    }
                    // Real input so the block caret sits on the insertion point
                    // (the old Text + sibling Rectangle sat a layout gap away).
                    TextInput {
                        id: field
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        color: picker.theme.foreground
                        font.family: picker.theme.font
                        font.pixelSize: picker.theme.size.label
                        verticalAlignment: TextInput.AlignVCenter
                        clip: true
                        leftPadding: 0; rightPadding: 0
                        cursorVisible: false
                        cursorDelegate: Item {}
                        Keys.onPressed: event => {
                            if (event.key === Qt.Key_Escape) { picker.close(); event.accepted = true; }
                            else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) { picker.accept(); event.accepted = true; }
                            else if (event.key === Qt.Key_Up) { picker.move(-1); event.accepted = true; }
                            else if (event.key === Qt.Key_Down) { picker.move(1); event.accepted = true; }
                            else if (event.key === Qt.Key_PageUp) { picker.move(-picker.pageRows); event.accepted = true; }
                            else if (event.key === Qt.Key_PageDown) { picker.move(picker.pageRows); event.accepted = true; }
                            else if (event.key === Qt.Key_Home && event.modifiers === Qt.NoModifier) { picker.select(0); event.accepted = true; }
                            else if (event.key === Qt.Key_End && event.modifiers === Qt.NoModifier) { picker.select(picker.rows.length - 1); event.accepted = true; }
                            else if (event.key === Qt.Key_U && event.modifiers === Qt.ControlModifier) { field.text = ""; event.accepted = true; }
                        }
                        Rectangle {
                            width: 7; height: 15
                            x: field.cursorRectangle.x
                            y: Math.round(field.cursorRectangle.y + (field.cursorRectangle.height - height) / 2)
                            color: picker.theme.foreground
                            opacity: .9
                        }
                    }
                    Word { text: "town · county · country"; font.pixelSize: picker.theme.size.small; opacity: .45; visible: !picker.compact }
                }
            }
            // S48: every match in a clipped list, as tall as its rows up to
            // the room the window leaves; the wheel scrolls it by rows (and
            // stays here: the map under the scrim never sees it).
            Item {
                id: listBox
                Layout.fillWidth: true
                Layout.preferredHeight: picker.rows.length ? Math.min(picker.rows.length * picker.rowHeight, card.listRoom) : picker.rowHeight
                ListView {
                    id: list
                    anchors.fill: parent
                    clip: true
                    interactive: false
                    boundsBehavior: Flickable.StopAtBounds
                    model: picker.rows
                    // The rows' whole height (they are all one height), not
                    // ListView's estimate.
                    readonly property real rowsHeight: picker.rows.length * picker.rowHeight
                    readonly property bool overflows: rowsHeight > height + .5
                    function scrollBy(dy) { contentY = Math.max(0, Math.min(Math.max(0, rowsHeight - height), contentY + dy)); }
                    // A taller window (or fewer rows) never leaves it scrolled past the end.
                    onHeightChanged: scrollBy(0)
                    onRowsHeightChanged: scrollBy(0)
                    delegate: Rectangle {
                        id: row
                        required property var modelData
                        required property int index
                        readonly property bool current: index === picker.selected
                        readonly property color ink: current ? picker.theme.accent : picker.theme.foreground
                        width: ListView.view.width
                        height: picker.rowHeight
                        color: current ? Qt.alpha(picker.theme.foreground, .08) : "transparent"
                        // A hairline where the next country's radars (or the
                        // radars after the composites) begin.
                        Rectangle {
                            visible: row.modelData.groupStart
                            anchors.left: parent.left; anchors.right: parent.right; anchors.top: parent.top
                            anchors.leftMargin: 12; anchors.rightMargin: 12
                            height: 1; color: Qt.alpha(picker.theme.foreground, .17)
                        }
                        RowLayout {
                            anchors.fill: parent
                            anchors.leftMargin: 12
                            anchors.rightMargin: 12
                            spacing: 12
                            Word { text: Sites.mark(row.modelData.site.id, row.modelData.idHits, picker.theme.accent); textFormat: Text.StyledText; font.bold: true; color: row.ink; Layout.preferredWidth: Math.round(52 * Math.max(1, picker.theme.size.k)) }
                            Word { text: Sites.mark(row.modelData.place, row.modelData.placeHits, picker.theme.accent); textFormat: Text.StyledText; color: row.ink; opacity: .9; Layout.fillWidth: true }
                            Word { text: [row.modelData.site.id === picker.homeSite ? "home" : "", row.modelData.tag, row.modelData.where].filter(t => t).join(" · "); font.pixelSize: picker.theme.size.small; color: row.ink; opacity: .6 }
                        }
                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            onPositionChanged: picker.selected = row.index
                            onClicked: { picker.selected = row.index; picker.accept(); }
                        }
                    }
                }
                // The wheel, over the rows: three rows a notch (a touchpad's
                // pixels as they come). Buttons pass through to the rows.
                MouseArea {
                    anchors.fill: parent
                    acceptedButtons: Qt.NoButton
                    onWheel: wheel => {
                        var dy = wheel.pixelDelta.y !== 0 ? -wheel.pixelDelta.y : -wheel.angleDelta.y / 120 * 3 * picker.rowHeight;
                        list.scrollBy(dy);
                        wheel.accepted = true;
                    }
                }
                // A thin scrollbar at the right edge while the rows overflow;
                // it can be dragged.
                Rectangle {
                    id: scrollBar
                    visible: list.overflows
                    anchors.right: parent.right
                    anchors.rightMargin: 2
                    width: barArea.containsMouse || barArea.pressed ? 5 : 3
                    radius: width / 2
                    readonly property real track: list.height - 4
                    height: Math.max(16, track * list.height / Math.max(1, list.rowsHeight))
                    y: 2 + (track - height) * (list.rowsHeight > list.height ? list.contentY / (list.rowsHeight - list.height) : 0)
                    color: Qt.alpha(picker.theme.foreground, barArea.pressed ? .6 : .4)
                    MouseArea {
                        id: barArea
                        anchors.fill: parent
                        anchors.margins: -4
                        hoverEnabled: true
                        property real grabY: 0
                        onPressed: mouse => grabY = mouse.y
                        onPositionChanged: mouse => {
                            if (!pressed) return;
                            var span = scrollBar.track - scrollBar.height;
                            if (span > 0) list.scrollBy((mouse.y - grabY) / span * (list.rowsHeight - list.height));
                        }
                    }
                }
                Word { visible: !picker.rows.length; text: "NO STATION MATCHES"; anchors.fill: parent; anchors.leftMargin: 12; opacity: .55; font.letterSpacing: 1 }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(picker.theme.foreground, .17) }
            RowLayout {
                Layout.fillWidth: true
                spacing: 14
                Word { text: "↑ ↓ move"; font.pixelSize: picker.theme.size.small; opacity: .55 }
                Word { text: "↵ select and lock"; font.pixelSize: picker.theme.size.small; opacity: .55 }
                Word { text: "esc close"; font.pixelSize: picker.theme.size.small; opacity: .55; visible: !picker.compact }
                Item { Layout.fillWidth: true }
                // "44 radars" for the whole table, "5 of 44" once filtered.
                Word { id: footer; text: picker.query.trim() ? picker.rows.length + " of " + picker.sites.length : picker.sites.length + " radars"; font.pixelSize: picker.theme.size.small; opacity: .55 }
            }
        }
    }
}
