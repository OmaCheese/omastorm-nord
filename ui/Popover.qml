import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import "Keys.js" as KeyMap

FocusScope {
    id: card
    required property var session
    // Review S3: the session counts open popovers (the bar drops the card
    // when it closes), to put a composite's product back to REF with none.
    QtObject {
        Component.onCompleted: card.session.popovers += 1
        Component.onDestruction: card.session.popovers = Math.max(0, card.session.popovers - 1)
    }
    property var theme: session.theme.snapshot
    property alias engine: connection
    readonly property var state: connection.state
    // The frame on screen, from the card's own loop buffer while it plays
    // or waits on its seek (Engine.qml), else the engine's.
    readonly property var scan: connection.frame
    readonly property var frames: state ? state.timeline : []
    // Popover is too narrow for the window's fixed 60 empties. One tick per
    // frame, no gap stubs, pixel-snapped — same language, denser strip.
    readonly property var slots: {
        var result = [];
        for (var j = 0; j < frames.length; j++)
            result.push({id: frames[j].id, partial: frames[j].status === "partial"});
        return result;
    }
    readonly property int currentSlot: scan ? slots.findIndex(s => s.id === scan.id) : -1
    readonly property string condition: state ? state.source === "archived" ? "archived" : state.connection.status : "offline"
    readonly property color statusColor: !state && connection.starting ? theme.accent
        : condition === "stale" ? theme.yellow
        : condition === "offline" || condition === "unavailable" ? theme.red : theme.accent
    readonly property string statusText: {
        // Review NIT5: an engine on its way up is not a feed that is down.
        if (!state) return connection.starting ? "STARTING" : "OFFLINE";
        if (condition === "archived") return "ARCHIVED";
        var label = condition === "ok" ? "LIVE" : condition.toUpperCase();
        // S31: the engine's loading progress beside the condition; S41:
        // the whole load's, which never goes back within it.
        if (connection.loading) return label + " · " + connection.percent + " %";
        var complete = frames.filter(f => f.status === "complete");
        if (!scan || !scan.scanTime || !complete.length) return label;
        var age = Math.max(0, state.connection.ageSeconds +
            Math.round((Date.parse(complete[complete.length - 1].scanTime) - Date.parse(scan.scanTime)) / 1000));
        var minutes = Math.floor(age / 60);
        return label + " · " + (minutes < 1 ? "just now" : minutes < 60 ? minutes + " min ago"
            : minutes < 1440 ? Math.floor(minutes / 60) + "h ago" : Math.floor(minutes / 1440) + "d ago");
    }
    signal expandRequested()
    signal closeRequested()
    implicitWidth: 308
    // Match RadarBar's fixed KeyboardPanel contentHeight (400 − 28 inset).
    // Do not track layout.implicitHeight — time labels and status text
    // settling after open made the panel shrink and grow.
    implicitHeight: 372
    // The card exists only while it is open, so its buffer does too; a
    // smaller one than the window's (the phone's cap).
    Engine {
        id: connection
        active: true
        bufferCeiling: 160 * 1024 * 1024
        loopShared: true
        loopSite: card.session.loopSite
        onLoopRequested: site => card.session.loopSite = site
        // S42: the station layers follow the window's switches.
        layers: ({temp: card.session.layers.temp, wind: card.session.layers.wind, source: card.session.layers.source,
                  lightning: card.session.layers.lightning})
    }
    // The reach the window's product menu set for each radar (S29).
    function step(delta) { connection.stepBy(delta); }
    function play() { connection.togglePlay(); }
    // Respect the same config keys as the window; Enter always expands.
    Shortcut { id: probe; enabled: false }
    function canon(sequence) { probe.sequence = sequence; return probe.portableText; }
    property var bindings: ({})
    function applyKeys() { bindings = KeyMap.resolve(session.config.keys, canon).bindings; }
    Component.onCompleted: applyKeys()
    Connections { target: card.session.config; function onKeysChanged() { card.applyKeys(); } }
    Instantiator {
        model: ["previous_frame", "next_frame", "play", "close"]
        delegate: Shortcut {
            required property string modelData
            sequences: card.bindings[modelData] || []
            enabled: card.visible
            onActivated: {
                if (modelData === "close") card.closeRequested();
                else if (modelData === "play") card.play();
                else card.step(modelData === "previous_frame" ? -1 : 1);
            }
        }
    }
    Keys.onReturnPressed: expandRequested()
    Keys.onEnterPressed: expandRequested()
    component Label: Text {
        color: card.theme.foreground
        font.family: card.theme.font
        font.pixelSize: 12
        elide: Text.ElideRight
    }
    component Control: Button {
        id: button
        implicitHeight: 28
        implicitWidth: Math.max(28, contentItem.implicitWidth + 12)
        padding: 5
        contentItem: Label {
            text: button.text
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
            opacity: button.enabled ? 1 : .35
        }
        background: Rectangle {
            color: button.hovered || button.activeFocus ? Qt.alpha(card.theme.accent, .16) : "transparent"
            border.width: 1
            border.color: button.activeFocus ? card.theme.accent : Qt.alpha(card.theme.foreground, .22)
        }
    }
    ColumnLayout {
        id: layout
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        height: card.implicitHeight
        spacing: 10
        RowLayout {
            Layout.fillWidth: true
            spacing: 6
            // SMHI ids are the town folded to lowercase ASCII: show the town
            // alone ("Vara"), never "vara Vara". NEXRAD ids keep id + name.
            readonly property string siteId: card.state ? card.state.site.id : ""
            readonly property string siteName: connection.site ? connection.site.name : ""
            readonly property bool idIsName: !!siteId && !!siteName && siteId === siteId.toLowerCase()
            Label { text: parent.idIsName ? parent.siteName : parent.siteId || "—"; font.bold: true; font.pixelSize: 14 }
            Label { Layout.fillWidth: true; text: parent.idIsName ? "" : parent.siteName; opacity: .65 }
            Rectangle { width: 5; height: 5; radius: 3; color: card.statusColor }
            Label { text: card.statusText; color: card.statusColor; font.pixelSize: 11 }
        }
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 280
            color: card.theme.background
            border.color: Qt.alpha(card.theme.foreground, .17)
            clip: true
            RadarMap {
                id: map
                anchors.fill: parent
                scan: card.scan
                texture: connection.texture
                azimuthLut: connection.azimuthLut
                codes: connection.codes
                siteId: card.state ? card.state.site.id : ""
                sites: connection.sites
                referenceSites: connection.referenceSites
                tileRoot: "file://" + connection.runtime
                theme: card.theme
                treatment: card.session.treatment
                weakFloor: card.session.weakFloor
                relief: card.session.relief
                labelSize: 10
                product: card.state ? card.state.product : null
                // S30: a My mosaic height frame's holes are hatched only
                // inside the chosen radars' reach (no circles drawn here).
                hatchSites: {
                    var st = card.state;
                    if (!st || !st.mosaic || !st.mosaic.sites || !(connection.site && connection.site.provider === "mosaic")) return [];
                    return st.mosaic.sites.map(s => {
                        var r = connection.sites.find(x => x.id === s.id);
                        if (!r) return null;
                        var full = r.rangeKm > 0 ? r.rangeKm : 240;
                        return { lat: r.lat, lon: r.lon, km: full };
                    }).filter(c => c !== null);
                }
                radarOpacity: !card.session.layers.radar ? 0 : card.condition === "unavailable" ? .6 : 1
                // S43: the MET Nordic temperature field under the radar.
                underlay: popoverObs.underlay
                interactive: !card.session.needsLocation
                onNavigated: (lat, lon, spanKm) => card.session.userNavigated(lat, lon, spanKm)
                onTilesNeeded: (z, x0, y0, x1, y1) => connection.send({type: "tiles_needed", z: z, x0: x0, y0: y0, x1: x1, y1: y1})
                function applyView() {
                    if (!card.session.hasView) return;
                    holdSpan = true;
                    lookAt(card.session.centerLat, card.session.centerLon);
                    span = card.session.span;
                    Qt.callLater(() => { holdSpan = false; });
                }
                Component.onCompleted: applyView()
            }
            Connections { target: connection; function onTileReady(tile) { map.tileReady(tile); } }
            // S44: lightning strikes, small, without the key.
            LightningLayer {
                anchors.fill: parent
                map: map
                message: connection.lightning
                runtime: connection.runtime
                on: card.session.layers.lightning
                theme: card.theme
                compact: true
                showKey: false
                frame: card.scan
                live: {
                    var st = card.state, tl = st && st.timeline ? st.timeline : [];
                    if (connection.playing || !st || st.source !== "live") return false;
                    var done = tl.filter(f => f.status === "complete");
                    return !card.scan || !done.length || card.scan.id === done[done.length - 1].id || card.scan.id === tl[tl.length - 1].id;
                }
            }
            // S42: the weather stations, small; only the credit of the legend.
            ObsLayer {
                id: popoverObs
                anchors.fill: parent
                map: map
                obs: connection.obs
                grid: connection.grid
                source: card.session.layers.source
                runtime: connection.runtime
                temp: card.session.layers.temp
                wind: card.session.layers.wind
                theme: card.theme
                compact: true
                showLegend: false
                creditHeight: 32
            }
            Connections {
                target: card.session
                function onViewChanged() { map.applyView(); }
            }
            Label { anchors.top: parent.top; anchors.right: parent.right; anchors.margins: 8; text: "⤢"; font.pixelSize: 20; opacity: .65 }
            // What the rings mean (S29).
            Rectangle {
                anchors.top: parent.top; anchors.left: parent.left; anchors.margins: 8
                width: ringCaption.contentWidth + 8
                height: ringCaption.contentHeight + 4
                visible: map.ringNote !== ""
                color: Qt.alpha(card.theme.background, .92)
                Label {
                    id: ringCaption
                    x: 4; y: 2
                    width: card.width - 64
                    wrapMode: Text.Wrap
                    font.pixelSize: 9; opacity: .75
                    text: map.ringNote.toUpperCase()
                }
            }
            RowLayout {
                anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom; anchors.margins: 8
                Rectangle {
                    implicitWidth: product.implicitWidth + 10; implicitHeight: 20
                    color: Qt.alpha(card.theme.background, .92)
                    Label { id: product; anchors.centerIn: parent; font.pixelSize: 10; opacity: .8
                        text: card.scan ? card.scan.productName.toUpperCase() + (card.scan.kind === "grid" ? " · COMPOSITE" : (card.scan.product || "REF") === "REF" ? " " + card.scan.elevationDeg.toFixed(1) + "°" : "") : "" }
                }
                Item { Layout.fillWidth: true }
                Rectangle {
                    implicitWidth: time.implicitWidth + 10; implicitHeight: 20
                    color: Qt.alpha(card.theme.background, .92)
                    Label { id: time; anchors.centerIn: parent; font.pixelSize: 10; opacity: .8
                        text: {
                            if (!card.scan || !card.scan.scanTime) return "";
                            var loc = Qt.locale(), d = new Date(card.scan.scanTime);
                            var timeFmt = loc.timeFormat(Locale.ShortFormat) + " t";
                            return card.condition === "archived"
                                ? Qt.formatDateTime(d, loc.dateFormat(Locale.ShortFormat) + " " + timeFmt)
                                : Qt.formatTime(d, timeFmt);
                        }
                    }
                }
            }
            MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: card.expandRequested() }
            Label {
                anchors.centerIn: parent; width: parent.width - 24; wrapMode: Text.Wrap
                horizontalAlignment: Text.AlignHCenter
                visible: !card.state && !(loadingCard.visible && !card.session.startupError)
                text: card.session.startupError || connection.error
            }
            // S41: the load in steps while nothing is drawn yet.
            LoadingCard {
                id: loadingCard
                anchors.centerIn: parent
                width: Math.min(implicitWidth, parent.width - 24)
                engine: connection
                theme: card.theme
                compact: true
                visible: connection.busy && !connection.drawn && !card.session.needsLocation && !card.session.startupError
            }
            Rectangle {
                anchors.fill: parent
                visible: card.session.needsLocation
                color: Qt.alpha(card.theme.background, .82)
                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: { card.session.requestLocationPicker(); card.expandRequested(); }
                    onWheel: wheel => { wheel.accepted = true }
                }
                LocationPrompt {
                    anchors.centerIn: parent
                    width: parent.width - 32
                    session: card.session
                    theme: card.theme
                    onManualChosen: { card.session.requestLocationPicker(); card.expandRequested(); }
                }
            }
        }
        Label {
            Layout.fillWidth: true
            visible: !!connection.rejection
            text: connection.rejection; color: card.theme.accent; wrapMode: Text.Wrap
        }
        RowLayout {
            Layout.fillWidth: true
            spacing: 6
            Control { text: "‹"; Accessible.name: "Previous frame"; enabled: card.frames.length > 1; onClicked: card.step(-1) }
            Control { text: connection.playing ? "Ⅱ" : "▷"; Accessible.name: "Play or pause"; enabled: card.frames.filter(f => f.status === "complete").length > 1; onClicked: card.play() }
            Control { text: "›"; Accessible.name: "Next frame"; enabled: card.frames.length > 1; onClicked: card.step(1) }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 3
                Item {
                    id: strip
                    Layout.fillWidth: true
                    implicitHeight: 14
                    // S31: the load's progress, a thin bar under the ticks.
                    // S35: one segment per stage of the load, the earlier
                    // ones staying full. The strip has no room for the
                    // stage names; the label under it names the one
                    // running, as it did.
                    LoadingBar {
                        loading: connection.loading
                        theme: card.theme
                        // S41: 3 px, readable, still under the ticks.
                        thickness: 3
                        anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom
                        anchors.bottomMargin: -1 - implicitHeight
                    }
                    Repeater {
                        model: card.slots
                        Rectangle {
                            required property var modelData
                            required property int index
                            readonly property bool current: index === card.currentSlot
                            x: card.slots.length > 1 ? Math.round(index * (strip.width - width) / (card.slots.length - 1)) : Math.round((strip.width - width) / 2)
                            y: Math.round((strip.height - height) / 2)
                            width: current || modelData.partial ? 3 : 2
                            height: current || modelData.partial ? 14 : 10
                            color: current ? card.theme.accent : modelData.partial ? "transparent"
                                : Qt.alpha(card.theme.foreground, .40)
                            border.width: modelData.partial && !current ? 1 : 0
                            border.color: card.theme.accent
                        }
                    }
                }
                RowLayout {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 12
                    // S41: while loading, the step has the whole row.
                    Label { font.pixelSize: 10; opacity: .55; visible: !connection.loading; text: card.frames.length ? Qt.formatTime(new Date(card.frames[0].scanTime), Qt.locale().timeFormat(Locale.ShortFormat)) : " " }
                    // While it plays: frames held of the loop and their memory
                    // against this card's cap (Engine.qml, bufferNote).
                    // S31: what is loading, from the engine (state.loading),
                    // beside the buffer line while the loop plays (review NIT6).
                    // S41: the step by name and the whole load's
                    // percentage; the engine's label is too long for the
                    // strip and elided its count away.
                    Label {
                        Layout.fillWidth: true
                        horizontalAlignment: Text.AlignHCenter
                        font.pixelSize: 10; font.bold: true; color: card.theme.accent
                        elide: Text.ElideRight
                        visible: !!connection.loading
                        text: connection.loading ? connection.percent + " % · " + (connection.activeStep ? connection.activeStep.name : "") : ""
                    }
                    Label {
                        Layout.fillWidth: true
                        horizontalAlignment: Text.AlignHCenter
                        font.pixelSize: 9; opacity: .45
                        visible: connection.looping && connection.bufferTarget > 0
                        text: connection.bufferReady + "/" + connection.bufferTarget + " · " + connection.bufferMB + " of " + connection.bufferCapMB + " MB"
                            + (connection.bufferDropped > 0 ? " · −" + connection.bufferDropped : "")
                    }
                    Item { Layout.fillWidth: true; visible: !connection.loading && !(connection.looping && connection.bufferTarget > 0) }
                    Label { font.pixelSize: 10; opacity: .55; visible: !connection.loading; text: card.condition === "ok" ? "now" : card.frames.length ? Qt.formatTime(new Date(card.frames[card.frames.length - 1].scanTime), Qt.locale().timeFormat(Locale.ShortFormat)) : " " }
                }
            }
        }
        Label {
            Layout.fillWidth: true
            font.pixelSize: 8
            opacity: .5
            elide: Text.ElideRight
            // The frame's own credit, verbatim (docs/protocol.md,
            // frame.attribution), then the basemap's.
            readonly property string radarCredit: card.scan && card.scan.attribution ? card.scan.attribution
                : connection.site && connection.site.attribution ? connection.site.attribution : ""
            text: (radarCredit ? radarCredit + " · " : "") + (map.osmOnScreen ? "© OpenStreetMap" : "Natural Earth")
                + (connection.referenceSites.length ? " · other radars: EUMETNET" : "")
                + (card.session.layers.lightning && connection.lightning && connection.lightning.attribution ? " · " + connection.lightning.attribution : "")
        }
    }
}
