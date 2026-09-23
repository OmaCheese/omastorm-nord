import QtQuick
import Quickshell
import qs.Ui
import qs.Commons

BarWidget {
    id: root
    moduleName: "omacheese.omastorm-nord"
    property var session: PluginSession
    property bool opened: false
    property bool popoutSwitchClosing: false
    readonly property var state: session.engine.state
    readonly property bool live: state && state.source === "live" && state.connection.status === "ok"
    readonly property bool down: !state || state.connection.status === "offline" || state.connection.status === "unavailable"
    function open() {
        popoutSwitchClosing = false;
        opened = true;
    }
    function close() { opened = false; }
    function closeForPopoutSwitch() { popoutSwitchClosing = true; close(); }
    function expand() {
        Quickshell.execDetached(["omarchy", "shell", "shell", session.windowOpen ? "summon" : "toggle", "omacheese.omastorm-nord", "{}"]);
        close();
    }
    implicitWidth: button.implicitWidth
    implicitHeight: button.implicitHeight
    BarIconButton {
        id: button
        anchors.fill: parent
        bar: root.bar
        slotSize: 27
        opticalSize: 16
        useActiveColor: false
        active: root.opened
        iconComponent: Component {
            Item {
                RadarMark { anchors.centerIn: parent; ink: button.foreground; opacity: root.live ? 1 : .6 }
                Rectangle { anchors.right: parent.right; anchors.bottom: parent.bottom; width: 5; height: 5; color: Color.urgent; visible: root.down }
            }
        }
        onPressed: b => { if (b === Qt.LeftButton) { if (root.opened) root.close(); else root.open(); } }
    }
    KeyboardPanel {
        id: popup
        anchorItem: button
        bar: root.bar
        owner: root
        open: root.opened
        // Fixed height: binding to the Loader item jumps from the 440
        // fallback to a settling layout on every open (shrink/grow jitter).
        // Card geometry is stable (308×372 content + 28 inset around it).
        padding: 12
        borderSpec: Border.flat(Color.accent, 2)
        // S46: the card's size at Omarchy's text size, plus the inset.
        readonly property real grow: Math.max(1, root.session.theme.snapshot.size.k)
        contentWidth: Math.round(308 * grow) + 28
        contentHeight: Math.round(372 * grow) + 28
        focusTarget: content.item
        Loader {
            id: content
            anchors.fill: parent
            active: root.opened
            sourceComponent: Popover {
                session: root.session
                onCloseRequested: root.close()
                onExpandRequested: root.expand()
            }
        }
    }
}
