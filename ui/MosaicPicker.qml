import QtQuick
import QtQuick.Layouts
import "Location.js" as Location
import "Mosaic.js" as Mosaic
import "Sites.js" as Sites

// My mosaic's panel (S25; docked since S40, docs/protocol.md "My mosaic").
// While open it takes the right side of the map stage at the stage's full
// height and the map shrinks to its left (RadarWindow sets the geometry;
// a `compact` window gets it as an overlay over the map instead). The map
// beside it is where radars are picked: RadarMap's pick mode (S39) draws
// every radar, `pickedIds` is the draft, `hotId` the cursor row, and a
// click on a mark comes back as toggle(). The panel lists CHOSEN (the
// ticked radars, each with a remove ✕; a click centres the map on it), a
// filter over id, name and country, and ALL RADARS by country, the country
// nearest the map centre first and the nearest radars first inside it (the
// order is fixed when the panel opens, the filter only narrows it). At most
// hello.mosaic.maxSites ticks, and the combine rule. SHOW sends set_mosaic
// and hands the set to `shown`; CANCEL, ✕ or Escape discard the draft.
//
// S40, the human 2026-09-22 ("the list doesnt change with arrow keys"):
// the rows no longer move the cursor on hover. Qt re-sends hover every
// frame the content moves under a still pointer, so the old
// onPositionChanged put the cursor back on the row under the pointer each
// time the list scrolled, and the arrows never got past a screenful. The
// keys live in a FocusScope that show() focuses again once its caller is
// done (a closing site picker cannot take them back), and a press anywhere
// in the panel takes them back from wherever they went. The wheel over the
// panel only ever scrolls the panel.
FocusScope {
    id: picker
    property var engine
    property var theme
    property var store              // MosaicStore: the last set
    property real centerLat: 0
    property real centerLon: 0
    property bool compact: false
    /// The map stage's whole width, the panel's included.
    property real stageWidth: 0
    /// The width RadarWindow gives the docked panel.
    readonly property real panelWidth: 380
    /// Docked beside the map while the map keeps 320 px, else over it.
    readonly property bool docked: !compact && stageWidth >= panelWidth + 320
    property bool open: false
    /// The working copy, {sites: [{id, reachKm}], rule, heightM, above}
    /// (the height and what it is above count only for the height rule, S30).
    property var draft: ({ sites: [], rule: "lowest", heightM: 2000, above: "sea" })
    /// S30: heights above the ground; an older engine's CAPPI entry in
    /// hello.products says nothing about them. S32: only when every chosen
    /// radar's hello.sites[].above has it. Since S36 every shipped radar is
    /// on the Nordic terrain grid, so it always does; the check stays for
    /// any radar beyond it.
    readonly property bool groundOffered: {
        var p = engine ? engine.products.find(x => x.id === "CAPPI") : null;
        if (!p || !Array.isArray(p.above) || p.above.indexOf("ground") < 0) return false;
        return draft.sites.every(d => {
            var s = engine.sites.find(x => x.id === d.id);
            return !(s && Array.isArray(s.above) && s.above.indexOf("ground") < 0);
        });
    }
    /// Every radar in list order, fixed when the panel opens.
    property var radars: []
    /// The filter's text, and the radars it leaves: ALL RADARS' rows.
    property alias filter: filterField.text
    readonly property var rows: {
        var q = Sites.fold(filter.trim());
        if (q === "") return radars;
        return radars.filter(r => [r.site.id, r.site.name, r.site.country || "", Sites.countryName(r.site.country)]
                             .some(t => Sites.fold(String(t)).indexOf(q) >= 0));
    }
    /// The cursor, an index into `rows`.
    property int cursor: 0
    /// The cursor's radar, so a filter that keeps it keeps the cursor on it.
    property string cursorId: ""
    /// The radar under the cursor: RadarMap's hotId.
    readonly property string hotId: open && rows.length ? rows[Math.min(cursor, rows.length - 1)].site.id : ""
    /// Whether the list keys or the filter hold the keyboard (IPC status).
    readonly property bool listFocused: keys.activeFocus
    readonly property bool filterFocused: filterField.activeFocus
    signal shown(var set)
    /// Closed without SHOW: the draft is gone.
    signal cancelled()
    /// A CHOSEN row was clicked: centre the map on that radar.
    signal centre(var site)
    /// The keys or a click in the list moved the cursor: the map pans to it
    /// if it is off screen. Hover and map clicks move the cursor without it.
    signal cursorMoved(string id)
    /// `?` in the panel: the keys sheet.
    signal helpRequested()
    /// A tick the panel had no room for, for a moment (S40 review 6).
    property string warning: ""
    Timer { id: warningTimer; interval: 2500; onTriggered: picker.warning = "" }
    readonly property var info: engine ? engine.mosaic : null
    readonly property int maxSites: info ? info.maxSites : 12
    /// The ticked radars' ids, for RadarMap's pickedIds.
    readonly property var pickedIds: open ? draft.sites.map(s => s.id) : []
    /// The ticked radars' reach circles, for RadarMap.
    readonly property var circles: !open ? [] : draft.sites.map(s => {
        var site = siteOf(s.id);
        return site ? { id: s.id, lat: site.lat, lon: site.lon, km: Mosaic.reachOf(s, site) } : null;
    }).filter(c => c !== null)
    component Word: Text {
        color: picker.theme.foreground
        font.family: picker.theme.font
        font.pixelSize: picker.theme.size.body
        elide: Text.ElideRight
        verticalAlignment: Text.AlignVCenter
    }
    component Step: Rectangle {
        id: step
        property string label
        property bool accent: false
        signal activated()
        implicitWidth: Math.max(22, stepText.implicitWidth + 14)
        implicitHeight: 20
        color: stepArea.containsMouse && enabled ? Qt.alpha(picker.theme.accent, accent ? .3 : .18)
            : accent ? Qt.alpha(picker.theme.accent, .18) : "transparent"
        border.width: 1
        border.color: accent ? picker.theme.accent : Qt.alpha(picker.theme.foreground, enabled ? .3 : .12)
        opacity: enabled ? 1 : .4
        Word {
            id: stepText
            anchors.centerIn: parent
            text: step.label
            font.pixelSize: step.accent ? picker.theme.size.caption : picker.theme.size.small; font.bold: step.accent
            color: step.accent ? picker.theme.accent : picker.theme.foreground
        }
        MouseArea { id: stepArea; anchors.fill: parent; hoverEnabled: true; enabled: step.enabled; cursorShape: Qt.PointingHandCursor; onClicked: step.activated() }
    }
    component Heading: Word {
        font.pixelSize: picker.theme.size.small; font.bold: true; font.letterSpacing: 1; opacity: .6
    }
    visible: open
    function siteOf(id) { return engine ? engine.sites.find(s => s.id === id) || null : null; }
    function picked(id) { return draft.sites.find(s => s.id === id) || null; }
    function show() {
        // Open already (the site picker's My mosaic, the chip): keep the
        // draft, take the keys.
        if (open) { takeKeys(); return; }
        if (!info || !engine) return;
        var st = engine.state;
        var from = st && st.mosaic && st.mosaic.sites && st.mosaic.sites.length ? st.mosaic : store ? store.set : null;
        var heightM = from && Mosaic.validHeight(from.heightM) ? from.heightM : 2000;
        var above = from && from.above === "ground" ? "ground" : "sea";
        draft = Mosaic.valid(from)
            ? { sites: from.sites.filter(s => siteOf(s.id)).map(s => ({ id: s.id, reachKm: Mosaic.reachOf(s, siteOf(s.id)) })), rule: from.rule || "lowest", heightM: heightM, above: above }
            : { sites: [], rule: "lowest", heightM: heightM, above: above };
        if (draft.rule === "height" && !(info.rules || []).some(r => r.id === "height")) setRule("lowest");
        var list = engine.sites.filter(s => s.kind !== "grid")
            .map(s => ({ site: s, km: Location.distanceKm(centerLat, centerLon, s.lat, s.lon) }));
        var nearest = {};
        for (var r of list) {
            var g = r.site.country || "?";
            if (!(nearest[g] <= r.km)) nearest[g] = r.km;
        }
        list.sort((a, b) => nearest[a.site.country || "?"] - nearest[b.site.country || "?"]
                  || (a.site.country < b.site.country ? -1 : a.site.country > b.site.country ? 1 : 0) || a.km - b.km);
        for (var i = 0; i < list.length; i++) list[i].groupStart = i > 0 && list[i].site.country !== list[i - 1].site.country;
        filterField.text = "";
        radars = list;
        cursor = 0;
        cursorId = list.length ? list[0].site.id : "";
        warning = "";
        open = true;
        allList.positionViewAtBeginning();
        takeKeys();
        // Again once the caller is done: the site picker's accept() opens
        // this from inside its own key handler and closes itself around it.
        Qt.callLater(() => { if (picker.open && !filterField.activeFocus) picker.takeKeys(); });
    }
    /// Closes and discards the draft (CANCEL, ✕, Escape, the chip).
    function close() {
        if (!open) return;
        shut();
        cancelled();
    }
    function shut() {
        open = false;
        filterField.text = "";
        filterField.focus = false;
        keys.focus = true;
    }
    /// The list keys take the keyboard.
    function takeKeys() { keys.forceActiveFocus(); }
    function toFilter() { filterField.forceActiveFocus(); filterField.selectAll(); }
    function move(delta) {
        if (!rows.length) return;
        cursor = Math.max(0, Math.min(rows.length - 1, cursor + delta));
        cursorMoved(hotId);
    }
    /// The cursor onto a radar's row, if the filter shows it.
    function point(id) {
        var at = rows.findIndex(r => r.site.id === id);
        if (at >= 0) cursor = at;
        return at >= 0;
    }
    function toggle(id) {
        var sites = draft.sites.slice();
        var at = sites.findIndex(s => s.id === id);
        if (at >= 0) sites.splice(at, 1);
        // S37: no reach to carry — a ticked radar reaches as far as it
        // reaches, and the engine ignores any reachKm sent.
        else if (sites.length < maxSites) sites.push({ id: id });
        else { warning = maxSites + " OF " + maxSites + " · UNTICK ONE FIRST"; warningTimer.restart(); return; }
        draft = withSites(sites);
    }
    /// The draft with other radars, the rule and height kept.
    function withSites(sites) { return { sites: sites, rule: draft.rule, heightM: draft.heightM, above: draft.above }; }
    function setRule(id) { draft = { sites: draft.sites, rule: id, heightM: draft.heightM, above: draft.above }; }
    // S30: the height rule's height, 500 m steps, and what it is above.
    function stepHeight(delta) {
        var m = Math.max(500, Math.min(12000, (draft.heightM || 2000) + 500 * delta));
        draft = { sites: draft.sites, rule: draft.rule, heightM: m, above: draft.above };
    }
    function setHeight(m) {
        if (Mosaic.validHeight(m)) draft = { sites: draft.sites, rule: draft.rule, heightM: m, above: draft.above };
    }
    function setAbove(above) {
        draft = { sites: draft.sites, rule: draft.rule, heightM: draft.heightM, above: above === "ground" ? "ground" : "sea" };
    }
    function nextRule() {
        var rules = info ? info.rules : [];
        var at = rules.findIndex(r => r.id === draft.rule);
        if (rules.length) setRule(rules[(at + 1) % rules.length].id);
    }
    function accept() {
        if (!engine || !draft.sites.length) return;
        var set = { sites: draft.sites.slice(), rule: draft.rule };
        if (set.rule === "height") {
            set.heightM = draft.heightM;
            set.above = groundOffered ? draft.above : "sea";
        }
        shut();
        if (store) store.keep(set);
        engine.send(Mosaic.command(set, engine.sites, info ? info.rules : null));
        shown(set);
    }
    onCursorChanged: {
        if (rows[cursor]) cursorId = rows[cursor].site.id;
        if (open) allList.positionViewAtIndex(cursor, ListView.Contain);
    }
    // A filter that keeps the cursor's radar keeps the cursor on it.
    onRowsChanged: {
        var at = rows.findIndex(r => r.site.id === cursorId);
        cursor = Math.max(0, at);
        cursorId = rows.length ? rows[cursor].site.id : "";
        if (open) allList.positionViewAtIndex(cursor, ListView.Contain);
    }

    Rectangle {
        anchors.fill: parent
        color: picker.docked ? picker.theme.background : Qt.alpha(picker.theme.background, .97)
        border.width: 1
        border.color: Qt.alpha(picker.theme.foreground, picker.docked ? .17 : 1)
    }
    // Under everything: a press on the panel's empty parts stays on the
    // panel, and every wheel event nothing above took (a list at its end)
    // stops here, so the compact overlay never zooms the map under it.
    MouseArea {
        anchors.fill: parent
        acceptedButtons: Qt.AllButtons
        onWheel: wheel => wheel.accepted = true
    }
    // The list keys. Window shortcuts stand down while the panel is open
    // (RadarWindow's overlayOpen), so these are the only keys in force.
    Item {
        id: keys
        focus: true
        Keys.onPressed: event => {
            if (!picker.open) return;
            var row = picker.rows[picker.cursor];
            event.accepted = true;
            if (event.modifiers & (Qt.ControlModifier | Qt.AltModifier | Qt.MetaModifier)) event.accepted = false;
            else if (event.key === Qt.Key_Escape) picker.close();
            else if (event.key === Qt.Key_Up || event.key === Qt.Key_K) picker.move(-1);
            else if (event.key === Qt.Key_Down || event.key === Qt.Key_J) picker.move(1);
            else if (event.key === Qt.Key_PageUp) picker.move(-10);
            else if (event.key === Qt.Key_PageDown) picker.move(10);
            else if (event.key === Qt.Key_Home) picker.move(-picker.rows.length);
            else if (event.key === Qt.Key_End) picker.move(picker.rows.length);
            else if ((event.key === Qt.Key_Space || event.key === Qt.Key_X) && row) picker.toggle(row.site.id);
            else if (event.key === Qt.Key_Slash) picker.toFilter();
            else if (event.text === "?") picker.helpRequested();
            else if (event.key === Qt.Key_R) picker.nextRule();
            else if ((event.key === Qt.Key_BracketLeft || event.key === Qt.Key_BracketRight) && picker.draft.rule === "height") picker.stepHeight(event.key === Qt.Key_BracketLeft ? -1 : 1);
            else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) picker.accept();
            else event.accepted = false;
        }
    }
    ColumnLayout {
        id: column
        anchors.fill: parent
        anchors.margins: 12
        spacing: 7
        RowLayout {
            Layout.fillWidth: true
            spacing: 10
            Word { text: "MY MOSAIC"; font.bold: true; font.letterSpacing: 1 }
            Word {
                text: picker.warning || picker.draft.sites.length + " OF " + picker.maxSites
                color: picker.warning ? picker.theme.accent : picker.theme.foreground
                font.pixelSize: picker.theme.size.small; font.bold: picker.warning !== ""; opacity: picker.warning ? 1 : .6
                Layout.fillWidth: true
            }
            Rectangle {
                implicitWidth: 22; implicitHeight: 22
                color: closeArea.containsMouse ? Qt.alpha(picker.theme.accent, .18) : "transparent"
                border.width: 1
                border.color: Qt.alpha(picker.theme.foreground, .3)
                Word { anchors.centerIn: parent; text: "✕"; font.pixelSize: picker.theme.size.caption }
                MouseArea { id: closeArea; anchors.fill: parent; hoverEnabled: true; cursorShape: Qt.PointingHandCursor; onClicked: picker.close() }
            }
        }
        Flow {
            Layout.fillWidth: true
            spacing: 6
            Repeater {
                model: picker.info ? picker.info.rules : []
                Rectangle {
                    id: ruleChip
                    required property var modelData
                    readonly property bool on: picker.draft.rule === modelData.id
                    width: ruleText.implicitWidth + 14
                    height: 20
                    color: ruleArea.containsMouse || on ? Qt.alpha(picker.theme.accent, .18) : "transparent"
                    border.width: 1
                    border.color: on ? picker.theme.accent : Qt.alpha(picker.theme.foreground, .3)
                    Word { id: ruleText; anchors.centerIn: parent; text: ruleChip.modelData.name.toUpperCase(); font.pixelSize: picker.theme.size.small; color: ruleChip.on ? picker.theme.accent : picker.theme.foreground }
                    MouseArea { id: ruleArea; anchors.fill: parent; hoverEnabled: true; cursorShape: Qt.PointingHandCursor; onClicked: picker.setRule(ruleChip.modelData.id) }
                }
            }
        }
        Word {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            elide: Text.ElideNone
            font.pixelSize: picker.theme.size.small; opacity: .6
            text: picker.draft.rule === "strongest"
                ? "EACH SPOT: THE STRONGEST ECHO ANY TICKED RADAR SEES THERE"
                : picker.draft.rule === "height"
                ? "EACH SPOT: THE RADAR WHOSE BEAM PASSES NEAREST THE HEIGHT, THE NEARER ON A TIE; HATCHED WHERE NONE REACHES IT. READS EACH RADAR'S WHOLE VOLUME"
                : "EACH SPOT: THE RADAR WHOSE BEAM IS LOWEST THERE, THE NEARER ON A TIE"
        }
        // S30: the height rule's height and what it is measured from.
        RowLayout {
            Layout.fillWidth: true
            spacing: 4
            visible: picker.draft.rule === "height"
            Word { text: "HEIGHT"; font.pixelSize: picker.theme.size.small; opacity: .6 }
            Word { text: (picker.draft.heightM || 2000) / 1000 + " KM"; color: picker.theme.accent; font.pixelSize: picker.theme.size.small; Layout.fillWidth: true }
            Step { label: "−"; enabled: (picker.draft.heightM || 2000) > 500; onActivated: picker.stepHeight(-1) }
            Step { label: "+"; enabled: (picker.draft.heightM || 2000) < 12000; onActivated: picker.stepHeight(1) }
            Step { visible: picker.groundOffered; label: "SEA"; enabled: picker.draft.above === "ground"; onActivated: picker.setAbove("sea") }
            Step { visible: picker.groundOffered; label: "GROUND"; enabled: picker.draft.above !== "ground"; onActivated: picker.setAbove("ground") }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(picker.theme.foreground, .17) }
        // CHOSEN: the ticked radars in the order they were ticked, one chip
        // each: the name centres the map on it, the ✕ unticks it.
        RowLayout {
            Layout.fillWidth: true
            Heading { text: "CHOSEN"; Layout.fillWidth: true }
            Word { text: picker.draft.sites.length ? "CLICK ONE TO CENTRE IT" : ""; font.pixelSize: picker.theme.size.small; opacity: .45 }
        }
        Word {
            visible: !picker.draft.sites.length
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            elide: Text.ElideNone
            font.pixelSize: picker.theme.size.small; opacity: .5
            text: picker.docked ? "NONE YET. CLICK A RADAR ON THE MAP, OR TICK ONE BELOW." : "NONE YET. TICK ONE BELOW."
        }
        Flow {
            id: chosenFlow
            visible: picker.draft.sites.length > 0
            Layout.fillWidth: true
            spacing: 4
            Repeater {
                model: picker.draft.sites
                Rectangle {
                    id: chosen
                    required property var modelData
                    readonly property var site: picker.siteOf(modelData.id)
                    readonly property bool hot: picker.hotId === modelData.id
                    width: chosenRow.implicitWidth + 2
                    height: 22
                    color: Qt.alpha(picker.theme.accent, chosenArea.containsMouse ? .26 : .14)
                    border.width: 1
                    border.color: hot ? picker.theme.foreground : picker.theme.accent
                    Row {
                        id: chosenRow
                        x: 1
                        height: parent.height
                        Item {
                            width: chosenName.implicitWidth + 14; height: parent.height
                            Word { id: chosenName; anchors.centerIn: parent; text: chosen.site ? chosen.site.name.toUpperCase() : chosen.modelData.id; color: picker.theme.accent; font.pixelSize: picker.theme.size.small; font.bold: true }
                            MouseArea {
                                id: chosenArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    picker.point(chosen.modelData.id);
                                    if (chosen.site) picker.centre(chosen.site);
                                }
                            }
                        }
                        Rectangle {
                            width: 20; height: parent.height
                            color: removeArea.containsMouse ? Qt.alpha(picker.theme.accent, .3) : "transparent"
                            Word { anchors.centerIn: parent; text: "✕"; font.pixelSize: picker.theme.size.small; color: picker.theme.accent; opacity: removeArea.containsMouse ? 1 : .7 }
                            MouseArea {
                                id: removeArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: picker.toggle(chosen.modelData.id)
                            }
                        }
                    }
                }
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(picker.theme.foreground, .17) }
        RowLayout {
            Layout.fillWidth: true
            Heading { text: "ALL RADARS"; Layout.fillWidth: true }
            Word { text: picker.filter !== "" ? picker.rows.length + " OF " + picker.radars.length : picker.radars.length; font.pixelSize: picker.theme.size.small; opacity: .5 }
        }
        // The filter: id, name or country. `/` comes here; Escape, Return,
        // Tab or Down go back to the list with the filter kept.
        Rectangle {
            Layout.fillWidth: true
            implicitHeight: 28
            color: Qt.alpha(picker.theme.foreground, .04)
            border.width: 1
            border.color: filterField.activeFocus ? picker.theme.accent : Qt.alpha(picker.theme.foreground, .3)
            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 8
                anchors.rightMargin: 8
                spacing: 8
                Word { text: "/"; font.pixelSize: picker.theme.size.caption; opacity: .55 }
                TextInput {
                    id: filterField
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    color: picker.theme.foreground
                    selectionColor: Qt.alpha(picker.theme.accent, .4)
                    selectedTextColor: picker.theme.foreground
                    font.family: picker.theme.font
                    font.pixelSize: picker.theme.size.body
                    verticalAlignment: TextInput.AlignVCenter
                    clip: true
                    Word {
                        anchors.fill: parent
                        visible: !filterField.text
                        text: "FILTER · ID, NAME, COUNTRY"
                        font.pixelSize: picker.theme.size.small; opacity: .45
                    }
                    Keys.onPressed: event => {
                        if (event.key === Qt.Key_Escape || event.key === Qt.Key_Down || event.key === Qt.Key_Return
                                || event.key === Qt.Key_Enter || event.key === Qt.Key_Tab) {
                            picker.takeKeys();
                            event.accepted = true;
                        } else if (event.key === Qt.Key_U && event.modifiers === Qt.ControlModifier) {
                            filterField.text = "";
                            event.accepted = true;
                        }
                    }
                }
            }
        }
        ListView {
            id: allList
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.minimumHeight: 26 * 3
            clip: true
            boundsBehavior: Flickable.StopAtBounds
            model: picker.rows
            delegate: Rectangle {
                id: row
                required property var modelData
                required property int index
                readonly property var entry: picker.picked(modelData.site.id)
                readonly property bool current: picker.cursor === index
                readonly property color ink: entry ? picker.theme.accent : picker.theme.foreground
                width: ListView.view.width
                height: 26
                // The cursor is the keyboard's; the pointer only tints.
                color: current ? Qt.alpha(picker.theme.foreground, picker.listFocused ? .12 : .07)
                    : rowArea.containsMouse ? Qt.alpha(picker.theme.foreground, .05) : "transparent"
                Rectangle {
                    visible: row.current
                    anchors.left: parent.left; anchors.top: parent.top; anchors.bottom: parent.bottom
                    width: 2; color: picker.theme.accent
                }
                // A hairline where the next country's radars begin.
                Rectangle {
                    visible: row.modelData.groupStart
                    anchors.left: parent.left; anchors.right: parent.right; anchors.top: parent.top
                    anchors.leftMargin: 10; anchors.rightMargin: 10
                    height: 1; color: Qt.alpha(picker.theme.foreground, .17)
                }
                MouseArea {
                    id: rowArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: { picker.cursor = row.index; picker.cursorMoved(picker.hotId); picker.toggle(row.modelData.site.id); }
                }
                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 10
                    anchors.rightMargin: 10
                    spacing: 8
                    Rectangle {
                        implicitWidth: 11; implicitHeight: 11
                        border.width: 1
                        border.color: row.ink
                        color: row.entry ? picker.theme.accent : "transparent"
                        opacity: row.entry || picker.draft.sites.length < picker.maxSites ? 1 : .35
                    }
                    Word { text: row.modelData.site.id; font.bold: true; color: row.ink; Layout.preferredWidth: 72 }
                    Word { text: row.modelData.site.name.toUpperCase(); color: row.ink; opacity: .9; Layout.fillWidth: true }
                    Word { text: row.modelData.site.country; font.pixelSize: picker.theme.size.small; opacity: .55 }
                    Word { text: Math.round(Mosaic.fullKm(row.modelData.site)) + " KM"; font.pixelSize: picker.theme.size.small; opacity: row.entry ? .8 : .45; color: row.ink; horizontalAlignment: Text.AlignRight; Layout.preferredWidth: 46 }
                }
            }
            Word {
                anchors.centerIn: parent
                visible: !picker.rows.length
                text: "NO RADAR MATCHES"
                font.pixelSize: picker.theme.size.small; opacity: .5; font.letterSpacing: 1
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(picker.theme.foreground, .17) }
        RowLayout {
            Layout.fillWidth: true
            spacing: 8
            Word {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                elide: Text.ElideNone
                font.pixelSize: picker.theme.size.small; opacity: .55
                text: "↑↓ MOVE · SPACE TICK\n/ FILTER · R RULE" + (picker.draft.rule === "height" ? " · [ ] HEIGHT" : "")
            }
            Step { label: "CANCEL"; implicitHeight: 24; onActivated: picker.close() }
            Step {
                label: "SHOW ↵"
                accent: true
                implicitHeight: 24
                enabled: picker.draft.sites.length > 0
                onActivated: picker.accept()
            }
        }
    }
    // Over everything, taking nothing: a press anywhere in the panel hands
    // the list its keys back first (unless it lands in the filter, which
    // then takes them itself), and the press goes on to what is under it.
    MouseArea {
        anchors.fill: parent
        acceptedButtons: Qt.AllButtons
        onPressed: mouse => {
            if (!filterField.contains(mapToItem(filterField, mouse.x, mouse.y))) picker.takeKeys();
            mouse.accepted = false;
        }
    }
}
