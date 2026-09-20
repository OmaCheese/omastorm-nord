import QtQuick
import QtQuick.Layouts
import "Location.js" as Location
import "Mosaic.js" as Mosaic

// My mosaic's checklist (S25, docs/protocol.md "My mosaic"): every radar by
// country, the country nearest the map centre first and the nearest radars
// first inside it, a tick each (at most hello.mosaic.maxSites), and the
// combine rule. SHOW sends set_mosaic and hands the set to `shown`, which
// selects My mosaic. The card sits at the map's right with no scrim, so the
// map keeps showing the ticked radars' reach circles (`circles`, drawn by
// RadarMap). Up and Down move, Space ticks, R switches the rule, Return
// shows, Escape or a click beside the card closes.
// S37 (the human, 2026-09-20: "remove the range tuning") took the per-radar
// reach steps out: every ticked radar reaches as far as it reaches, the row
// shows that range, and the engine ignores any reachKm a set carries. The
// wheel over the card scrolls the list and never reaches the map.
Item {
    id: picker
    property var engine
    property var theme
    property var store              // MosaicStore: the last set
    property real centerLat: 0
    property real centerLon: 0
    property bool compact: false
    property real cardTop: 20
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
    /// The radars in list order, fixed when the checklist opens.
    property var radars: []
    property int cursor: 0
    signal shown(var set)
    readonly property var info: engine ? engine.mosaic : null
    readonly property int maxSites: info ? info.maxSites : 12
    /// The ticked radars' reach circles, for RadarMap.
    readonly property var circles: !open ? [] : draft.sites.map(s => {
        var site = siteOf(s.id);
        return site ? { id: s.id, lat: site.lat, lon: site.lon, km: Mosaic.reachOf(s, site) } : null;
    }).filter(c => c !== null)
    component Word: Text {
        color: picker.theme.foreground
        font.family: picker.theme.font
        font.pixelSize: 12
        elide: Text.ElideRight
        verticalAlignment: Text.AlignVCenter
    }
    component Step: Rectangle {
        id: step
        property string label
        signal activated()
        implicitWidth: Math.max(22, stepText.implicitWidth + 10)
        implicitHeight: 18
        color: stepArea.containsMouse && enabled ? Qt.alpha(picker.theme.accent, .18) : "transparent"
        border.width: 1
        border.color: Qt.alpha(picker.theme.foreground, enabled ? .3 : .12)
        opacity: enabled ? 1 : .4
        Word { id: stepText; anchors.centerIn: parent; text: step.label; font.pixelSize: 10 }
        MouseArea { id: stepArea; anchors.fill: parent; hoverEnabled: true; enabled: step.enabled; onClicked: step.activated() }
    }
    visible: open
    focus: open
    function siteOf(id) { return engine ? engine.sites.find(s => s.id === id) || null : null; }
    function picked(id) { return draft.sites.find(s => s.id === id) || null; }
    function show() {
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
        radars = list;
        cursor = 0;
        open = true;
        forceActiveFocus();
    }
    function close() { open = false; }
    function toggle(id) {
        var sites = draft.sites.slice();
        var at = sites.findIndex(s => s.id === id);
        if (at >= 0) sites.splice(at, 1);
        // S37: no reach to carry — a ticked radar reaches as far as it
        // reaches, and the engine ignores any reachKm sent.
        else if (sites.length < maxSites) sites.push({ id: id });
        draft = withSites(sites);
    }
    /// The draft with other radars, the rule and height kept.
    function withSites(sites) { return { sites: sites, rule: draft.rule, heightM: draft.heightM, above: draft.above }; }
    // Every radar to one reach: a step from the longest set now.
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
        close();
        if (store) store.keep(set);
        engine.send(Mosaic.command(set, engine.sites, info ? info.rules : null));
        shown(set);
    }
    onCursorChanged: if (open) list.positionViewAtIndex(cursor, ListView.Contain)
    Keys.onPressed: event => {
        if (!open) return;
        event.accepted = true;
        var row = radars[cursor];
        if (event.key === Qt.Key_Escape) close();
        else if (event.key === Qt.Key_Up) cursor = Math.max(0, cursor - 1);
        else if (event.key === Qt.Key_Down) cursor = Math.min(radars.length - 1, cursor + 1);
        else if (event.key === Qt.Key_Space && row) toggle(row.site.id);
        else if (event.key === Qt.Key_R) nextRule();
        else if ((event.key === Qt.Key_BracketLeft || event.key === Qt.Key_BracketRight) && draft.rule === "height") stepHeight(event.key === Qt.Key_BracketLeft ? -1 : 1);
        else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) accept();
        else event.accepted = false;
    }
    // No scrim: the map stays in view; a click beside the card closes.
    MouseArea { anchors.fill: parent; onClicked: picker.close() }
    Rectangle {
        id: card
        width: Math.min(400, picker.width - 40)
        x: Math.round(picker.width - width - 20)
        y: Math.round(Math.max(20, picker.cardTop))
        height: Math.min(column.implicitHeight + 24, picker.height - y - 20)
        color: Qt.alpha(picker.theme.background, .95)
        border.width: 1
        border.color: picker.theme.foreground
        MouseArea { anchors.fill: parent } // a click on the card stays on the card
        // S37 (the human, 2026-09-20: "i dont want to zoom when i scroll"):
        // the card has no scrim, so a wheel over it used to reach the map's
        // WheelHandler and zoom at the pointer. This takes every wheel
        // event over the card and gives it to the list, which scrolls if it
        // has anywhere to go and swallows it either way.
        WheelHandler {
            acceptedDevices: PointerDevice.Mouse | PointerDevice.TouchPad
            onWheel: event => {
                list.flick(0, 0);
                var step = event.angleDelta.y !== 0 ? event.angleDelta.y : event.angleDelta.x;
                if (step !== 0 && list.contentHeight > list.height) {
                    var top = Math.max(0, Math.min(list.contentHeight - list.height,
                                                   list.contentY - step * 26 / 120));
                    list.contentY = top;
                }
                event.accepted = true;
            }
        }
        ColumnLayout {
            id: column
            anchors.fill: parent
            anchors.margins: 12
            spacing: 8
            RowLayout {
                Layout.fillWidth: true
                spacing: 10
                Word { text: "MY MOSAIC"; font.bold: true; font.letterSpacing: 1 }
                Word { text: picker.draft.sites.length + " OF " + picker.maxSites; font.pixelSize: 10; opacity: .6; Layout.fillWidth: true }
                Repeater {
                    model: picker.info ? picker.info.rules : []
                    Rectangle {
                        id: ruleChip
                        required property var modelData
                        readonly property bool on: picker.draft.rule === modelData.id
                        implicitWidth: ruleText.implicitWidth + 14
                        implicitHeight: 20
                        color: ruleArea.containsMouse || on ? Qt.alpha(picker.theme.accent, .18) : "transparent"
                        border.width: 1
                        border.color: on ? picker.theme.accent : Qt.alpha(picker.theme.foreground, .3)
                        Word { id: ruleText; anchors.centerIn: parent; text: ruleChip.modelData.name.toUpperCase(); font.pixelSize: 10; color: ruleChip.on ? picker.theme.accent : picker.theme.foreground }
                        MouseArea { id: ruleArea; anchors.fill: parent; hoverEnabled: true; onClicked: picker.setRule(ruleChip.modelData.id) }
                    }
                }
            }
            Word {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                font.pixelSize: 10; opacity: .6
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
                Word { text: "HEIGHT"; font.pixelSize: 10; opacity: .6 }
                Word { text: (picker.draft.heightM || 2000) / 1000 + " KM"; color: picker.theme.accent; font.pixelSize: 10; Layout.fillWidth: true }
                Step { label: "−"; enabled: (picker.draft.heightM || 2000) > 500; onActivated: picker.stepHeight(-1) }
                Step { label: "+"; enabled: (picker.draft.heightM || 2000) < 12000; onActivated: picker.stepHeight(1) }
                Step { visible: picker.groundOffered; label: "SEA"; enabled: picker.draft.above === "ground"; onActivated: picker.setAbove("sea") }
                Step { visible: picker.groundOffered; label: "GROUND"; enabled: picker.draft.above !== "ground"; onActivated: picker.setAbove("ground") }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(picker.theme.foreground, .17) }
            ListView {
                id: list
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.preferredHeight: Math.min(contentHeight, 26 * 14)
                Layout.minimumHeight: 26 * 4
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                model: picker.radars
                delegate: Rectangle {
                    id: row
                    required property var modelData
                    required property int index
                    readonly property var entry: picker.picked(modelData.site.id)
                    readonly property bool hot: rowArea.containsMouse || picker.cursor === index
                    readonly property color ink: entry ? picker.theme.accent : picker.theme.foreground
                    width: ListView.view.width
                    height: 26
                    color: hot ? Qt.alpha(picker.theme.foreground, .08) : "transparent"
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
                        onPositionChanged: picker.cursor = row.index
                        onClicked: { picker.cursor = row.index; picker.toggle(row.modelData.site.id); }
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
                        Word { text: row.modelData.site.id; font.bold: true; color: row.ink; Layout.preferredWidth: 84 }
                        Word { text: row.modelData.site.name.toUpperCase(); color: row.ink; opacity: .9; Layout.fillWidth: true }
                        Word { text: row.modelData.site.country; font.pixelSize: 10; opacity: .55 }
                        Word { text: Math.round(Mosaic.fullKm(row.modelData.site)) + " KM"; font.pixelSize: 10; opacity: row.entry ? .8 : .45; color: row.ink; horizontalAlignment: Text.AlignRight; Layout.preferredWidth: 50 }
                    }
                }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(picker.theme.foreground, .17) }
            RowLayout {
                Layout.fillWidth: true
                spacing: 12
                Word { text: picker.draft.rule === "height" ? "SPACE TICK · [ ] HEIGHT · R RULE · ↵ SHOW" : "SPACE TICK · R RULE · ↵ SHOW"; font.pixelSize: 10; opacity: .55; Layout.fillWidth: true; visible: !picker.compact }
                Item { Layout.fillWidth: true; visible: picker.compact }
                Rectangle {
                    implicitWidth: showText.implicitWidth + 18
                    implicitHeight: 22
                    enabled: picker.draft.sites.length > 0
                    opacity: enabled ? 1 : .4
                    color: showArea.containsMouse && enabled ? Qt.alpha(picker.theme.accent, .3) : Qt.alpha(picker.theme.accent, .18)
                    border.width: 1
                    border.color: picker.theme.accent
                    Word { id: showText; anchors.centerIn: parent; text: "SHOW"; color: picker.theme.accent; font.pixelSize: 11; font.bold: true }
                    MouseArea { id: showArea; anchors.fill: parent; hoverEnabled: true; enabled: parent.enabled; onClicked: picker.accept() }
                }
            }
        }
    }
}
