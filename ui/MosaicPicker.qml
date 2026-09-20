import QtQuick
import QtQuick.Layouts
import "Location.js" as Location
import "Mosaic.js" as Mosaic

// My mosaic's checklist (S25, docs/protocol.md "My mosaic"): every radar by
// country, the country nearest the map centre first and the nearest radars
// first inside it, a tick each (at most hello.mosaic.maxSites), each ticked
// radar's reach in 25 km steps from 25 km to its full range, one step for
// all of them, and the combine rule. SHOW sends set_mosaic and hands the set
// to `shown`, which selects My mosaic. The card sits at the map's right with
// no scrim, so the map keeps showing the ticked radars' reach circles
// (`circles`, drawn by RadarMap). Up and Down move, Space ticks, Left and
// Right step the reach, R switches the rule, Return shows, Escape or a
// click beside the card closes.
// A radar's reach here is the engine's: past it the radar gives the mosaic
// nothing and its neighbours take over. The product menu's REACH (S29) is
// the same distance for one radar on screen; a radar ticked here starts at
// that reach when one is set.
Item {
    id: picker
    property var engine
    property var theme
    property var store              // MosaicStore: the last set
    property var reach              // Reach: S29's per-radar reach
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
        else if (sites.length < maxSites) {
            var full = Mosaic.fullKm(siteOf(id)), r = reach ? reach.km(id) : 0;
            sites.push({ id: id, reachKm: r > 0 && r < full ? r : full });
        }
        draft = withSites(sites);
    }
    /// The draft with other radars, the rule and height kept.
    function withSites(sites) { return { sites: sites, rule: draft.rule, heightM: draft.heightM, above: draft.above }; }
    // 25 km steps; past the radar's range is its full range.
    function stepped(now, delta) {
        return delta < 0 ? Math.max(25, Math.ceil(now / 25) * 25 - 25) : Math.floor(now / 25) * 25 + 25;
    }
    function stepReach(id, delta) {
        draft = withSites(draft.sites.map(s => {
            if (s.id !== id) return s;
            var site = siteOf(s.id), full = Mosaic.fullKm(site), next = stepped(Mosaic.reachOf(s, site), delta);
            return { id: s.id, reachKm: next >= full ? full : next };
        }));
    }
    // Every radar to one reach: a step from the longest set now.
    function stepAll(delta) {
        if (!draft.sites.length) return;
        var base = Math.max.apply(null, draft.sites.map(s => Mosaic.reachOf(s, siteOf(s.id))));
        var next = stepped(base, delta);
        draft = withSites(draft.sites.map(s => {
            var full = Mosaic.fullKm(siteOf(s.id));
            return { id: s.id, reachKm: next >= full ? full : next };
        }));
    }
    function fullAll() {
        draft = withSites(draft.sites.map(s => ({ id: s.id, reachKm: Mosaic.fullKm(siteOf(s.id)) })));
    }
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
    function reachText(entry) {
        var site = siteOf(entry.id), km = Mosaic.reachOf(entry, site);
        return km >= Mosaic.fullKm(site) ? "FULL" : Math.round(km) + " KM";
    }
    readonly property string allText: {
        var sites = draft.sites;
        if (!sites.length) return "";
        if (sites.every(s => Mosaic.reachOf(s, siteOf(s.id)) >= Mosaic.fullKm(siteOf(s.id)))) return "FULL";
        var first = Math.round(Mosaic.reachOf(sites[0], siteOf(sites[0].id)));
        return sites.every(s => Math.round(Mosaic.reachOf(s, siteOf(s.id))) === first) ? first + " KM" : "MIXED";
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
        else if ((event.key === Qt.Key_Left || event.key === Qt.Key_Right) && row && picked(row.site.id)) stepReach(row.site.id, event.key === Qt.Key_Left ? -1 : 1);
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
            RowLayout {
                Layout.fillWidth: true
                spacing: 4
                visible: picker.draft.sites.length > 1
                Word { text: "EVERY RADAR'S REACH"; font.pixelSize: 10; opacity: .6; Layout.fillWidth: true }
                Word { text: picker.allText; color: picker.theme.accent; font.pixelSize: 10 }
                Step { label: "−"; onActivated: picker.stepAll(-1) }
                Step { label: "+"; onActivated: picker.stepAll(1) }
                Step { label: "FULL"; enabled: picker.allText !== "FULL"; onActivated: picker.fullAll() }
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
                        Word { text: row.modelData.site.country; font.pixelSize: 10; opacity: .55; visible: !row.entry }
                        Step { visible: !!row.entry; label: "−"; enabled: !!row.entry && Mosaic.reachOf(row.entry, row.modelData.site) > 25; onActivated: picker.stepReach(row.modelData.site.id, -1) }
                        Word { visible: !!row.entry; text: row.entry ? picker.reachText(row.entry) : ""; color: picker.theme.accent; font.pixelSize: 10; horizontalAlignment: Text.AlignHCenter; Layout.preferredWidth: 50 }
                        Step { visible: !!row.entry; label: "+"; enabled: !!row.entry && Mosaic.reachOf(row.entry, row.modelData.site) < Mosaic.fullKm(row.modelData.site); onActivated: picker.stepReach(row.modelData.site.id, 1) }
                    }
                }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Qt.alpha(picker.theme.foreground, .17) }
            RowLayout {
                Layout.fillWidth: true
                spacing: 12
                Word { text: picker.draft.rule === "height" ? "SPACE TICK · ← → REACH · [ ] HEIGHT · ↵ SHOW" : "SPACE TICK · ← → REACH · R RULE · ↵ SHOW"; font.pixelSize: 10; opacity: .55; Layout.fillWidth: true; visible: !picker.compact }
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
