import QtQuick
import QtQuick.Layouts

// The map's right-click menu (S46): a compact card at the pointer, in the
// treatment menu's row style, over a clear scrim. Its rows come from the
// host (RadarWindow), one object each:
//   {kind: "header", label}                 a dim section name
//   {kind: "sep"}                           a hairline
//   {label, note, key, checked, radio, current, enabled, sub, run}
// `checked` (true/false) makes a check row, `radio` draws it as a dot;
// both flip in place and leave the menu open. `sub` is a list of rows
// shown in a second card beside this row (the product menu, FROM); `run`
// is called on a choice, after which a plain row closes the menu.
//
// Keys: Up and Down (j, k) move over the rows that act, Right (l) or
// Return opens a row's submenu, Left (h) or Escape closes it, Return or
// Space chooses, Escape closes. A click outside, or a right-click
// anywhere outside the card, closes it.
Item {
    id: menu
    property var theme
    property var rows: []
    property bool opened: false
    /// Where the pointer was, in this item's coordinates.
    property point at: Qt.point(0, 0)
    property int cursor: -1
    /// The row whose submenu is open, -1 for none; `inSub` while the keys
    /// move in it.
    property int subRow: -1
    property int subCursor: -1
    property bool inSub: false
    readonly property var sizes: theme && theme.size ? theme.size : ({small: 10, caption: 11, body: 12, label: 13, title: 14, k: 1})
    readonly property real grow: Math.max(1, sizes.k)
    readonly property real rowHeight: Math.round(24 * grow)
    readonly property color fg: theme ? theme.foreground : "#a9b1d6"
    readonly property color bg: theme ? theme.background : "#1a1b26"
    readonly property color accent: theme ? theme.accent : "#7aa2f7"
    readonly property var subRows: subRow >= 0 && subRow < rows.length && rows[subRow].sub ? rows[subRow].sub : []
    visible: opened
    focus: opened
    /// The card, for the host's checks and captures.
    readonly property Item cardItem: card
    readonly property Item subItem: subCard

    function acts(row) { return !!row && row.kind !== "header" && row.kind !== "sep" && row.enabled !== false; }
    function first(list) { for (var i = 0; i < list.length; i++) if (acts(list[i])) return i; return -1; }
    function step(list, from, delta) {
        var n = list.length;
        if (!n) return -1;
        for (var i = 1; i <= n; i++) {
            var j = from < 0 ? (delta > 0 ? i - 1 : n - i) : (from + delta * i + n * n) % n;
            if (acts(list[j])) return j;
        }
        return from;
    }
    function show(x, y) {
        at = Qt.point(x, y);
        cursor = -1; subRow = -1; subCursor = -1; inSub = false;
        opened = true;
        forceActiveFocus();
    }
    function close() { opened = false; subRow = -1; inSub = false; }
    function openSub(index) {
        if (index < 0 || !rows[index] || !rows[index].sub || !acts(rows[index])) { subRow = -1; return; }
        subRow = index;
        subCursor = -1;
    }
    /// A choice: a check or radio row flips and stays; any other closes.
    function choose(row) {
        if (!acts(row)) return;
        if (row.sub) return;
        var stays = row.checked === true || row.checked === false;
        if (!stays) close();
        if (typeof row.run === "function") row.run();
    }
    Keys.onPressed: event => {
        if (!opened) return;
        event.accepted = true;
        var k = event.key;
        if (inSub) {
            if (k === Qt.Key_Escape || k === Qt.Key_Left || k === Qt.Key_H) { inSub = false; subCursor = -1; }
            else if (k === Qt.Key_Up || k === Qt.Key_K) subCursor = step(subRows, subCursor, -1);
            else if (k === Qt.Key_Down || k === Qt.Key_J) subCursor = step(subRows, subCursor, 1);
            else if ((k === Qt.Key_Return || k === Qt.Key_Enter || k === Qt.Key_Space) && subCursor >= 0) choose(subRows[subCursor]);
            else event.accepted = false;
            return;
        }
        if (k === Qt.Key_Escape) close();
        else if (k === Qt.Key_Up || k === Qt.Key_K) { cursor = step(rows, cursor, -1); subRow = -1; }
        else if (k === Qt.Key_Down || k === Qt.Key_J) { cursor = step(rows, cursor, 1); subRow = -1; }
        else if ((k === Qt.Key_Right || k === Qt.Key_L || k === Qt.Key_Return || k === Qt.Key_Enter) && cursor >= 0 && rows[cursor] && rows[cursor].sub) {
            openSub(cursor);
            inSub = true;
            subCursor = first(subRows);
        }
        else if ((k === Qt.Key_Return || k === Qt.Key_Enter || k === Qt.Key_Space) && cursor >= 0) choose(rows[cursor]);
        else event.accepted = false;
    }
    // Outside the cards: any button closes.
    MouseArea {
        anchors.fill: parent
        acceptedButtons: Qt.AllButtons
        onPressed: menu.close()
    }

    component MenuRow: Rectangle {
        id: row
        property var item: ({})
        property bool hot: false
        property bool open: false
        signal hovered()
        signal clicked()
        readonly property bool header: item.kind === "header"
        readonly property bool sep: item.kind === "sep"
        readonly property bool check: item.checked === true || item.checked === false
        // The current choice (the region or product shown) in accent too.
        readonly property color ink: !menu.acts(item) ? Qt.alpha(menu.fg, .4) : hot || open || item.current ? menu.accent : menu.fg
        Layout.fillWidth: true
        implicitHeight: sep ? 9 : header ? Math.round(menu.sizes.small * 1.6) + 4 : menu.rowHeight
        color: (hot || open) && menu.acts(item) ? Qt.alpha(menu.fg, .08) : "transparent"
        Rectangle { visible: row.sep; anchors.verticalCenter: parent.verticalCenter; x: 8; width: parent.width - 16; height: 1; color: Qt.alpha(menu.fg, .17) }
        Text {
            visible: row.header
            anchors.left: parent.left; anchors.leftMargin: 10; anchors.bottom: parent.bottom; anchors.bottomMargin: 2
            text: row.item.label || ""
            color: menu.fg; opacity: .55
            font.family: menu.theme ? menu.theme.font : "monospace"
            font.pixelSize: menu.sizes.small; font.letterSpacing: 1
        }
        RowLayout {
            visible: !row.header && !row.sep
            anchors.fill: parent
            anchors.leftMargin: 10; anchors.rightMargin: 10
            spacing: 8
            // The check: a box (a dot for a radio row), accent when on.
            Rectangle {
                visible: row.check
                implicitWidth: Math.round(10 * menu.grow); implicitHeight: implicitWidth
                radius: row.item.radio ? width / 2 : 1
                color: row.item.checked ? menu.accent : "transparent"
                border.width: 1
                border.color: row.item.checked ? menu.accent : Qt.alpha(menu.fg, .5)
            }
            Text {
                Layout.fillWidth: true
                text: row.item.label || ""
                color: row.ink
                elide: Text.ElideRight
                font.family: menu.theme ? menu.theme.font : "monospace"
                font.pixelSize: menu.sizes.body
            }
            Text {
                visible: text !== ""
                text: row.item.sub ? "▸" : row.item.note || row.item.key || ""
                color: row.ink; opacity: .6
                font.family: menu.theme ? menu.theme.font : "monospace"
                font.pixelSize: menu.sizes.small
            }
        }
        MouseArea {
            anchors.fill: parent
            enabled: !row.header && !row.sep
            hoverEnabled: true
            acceptedButtons: Qt.LeftButton | Qt.RightButton
            onEntered: row.hovered()
            onClicked: row.clicked()
        }
    }

    Rectangle {
        id: card
        width: Math.round(236 * menu.grow)
        height: column.implicitHeight + 12
        x: Math.round(Math.max(4, Math.min(menu.width - width - 4, menu.at.x)))
        y: Math.round(Math.max(4, Math.min(menu.height - height - 4, menu.at.y)))
        color: Qt.alpha(menu.bg, .96)
        border.width: 1
        border.color: menu.fg
        MouseArea { anchors.fill: parent; acceptedButtons: Qt.AllButtons } // a click on the card stays on the card
        ColumnLayout {
            id: column
            anchors.fill: parent
            anchors.margins: 6
            spacing: 0
            Repeater {
                id: mainRows
                model: menu.opened ? menu.rows : []
                MenuRow {
                    required property var modelData
                    required property int index
                    item: modelData
                    hot: menu.cursor === index && !menu.inSub
                    open: menu.subRow === index
                    onHovered: { menu.cursor = index; menu.inSub = false; if (modelData.sub) menu.openSub(index); else menu.subRow = -1; }
                    onClicked: { if (modelData.sub) { menu.openSub(index); menu.inSub = true; menu.subCursor = menu.first(menu.subRows); } else menu.choose(modelData); }
                }
            }
        }
    }
    // The submenu, beside its row: right of the card when it fits, else left.
    Rectangle {
        id: subCard
        visible: menu.subRows.length > 0
        readonly property Item anchorRow: menu.subRow >= 0 ? mainRows.itemAt(menu.subRow) : null
        readonly property real rowY: anchorRow ? anchorRow.mapToItem(menu, 0, 0).y : card.y
        width: Math.round(222 * menu.grow)
        height: subColumn.implicitHeight + 12
        x: card.x + card.width + width - 2 <= menu.width ? card.x + card.width - 2 : Math.max(4, card.x - width + 2)
        y: Math.round(Math.max(4, Math.min(menu.height - height - 4, rowY - 6)))
        color: Qt.alpha(menu.bg, .96)
        border.width: 1
        border.color: menu.fg
        MouseArea { anchors.fill: parent; acceptedButtons: Qt.AllButtons }
        ColumnLayout {
            id: subColumn
            anchors.fill: parent
            anchors.margins: 6
            spacing: 0
            Repeater {
                model: menu.subRows
                MenuRow {
                    required property var modelData
                    required property int index
                    item: modelData
                    hot: menu.inSub && menu.subCursor === index
                    onHovered: { menu.inSub = true; menu.subCursor = index; }
                    onClicked: menu.choose(modelData)
                }
            }
        }
    }
}
