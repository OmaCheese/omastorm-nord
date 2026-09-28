#!/usr/bin/env bash
# Consent IP location through the shared session: stub curl, no network,
# personal configuration, shell bootstrap, or shared daemon. Then the reply
# cap, through the real curl against a local server.
set -euo pipefail
cd "$(dirname "$0")/.."
scratch=$(mktemp -d /tmp/omastorm-ip-check.XXXXXX)
server=
trap '[[ -z $server ]] || kill "$server" 2>/dev/null; rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/runtime"
cp -r ui "$scratch/ui"
sed -i '/Quickshell.execDetached(/c\        return;' "$scratch/ui/PluginSession.qml"
sed -i 's/connected: true/connected: false/; /running: engine.socket/c\        running: false' "$scratch/ui/Engine.qml"

fixture='{"nearest_area":[{"areaName":[{"value":"Stamford"}],"latitude":"41.05","longitude":"-73.54"}]}'
printf '%s\n' "$fixture" > "$scratch/ok.json"
cat > "$scratch/bin/curl" <<EOF
#!/bin/bash
echo "\$*" >> "$scratch/curl.log"
# Last non-flag argument is the URL (OMASTORM_LOCATION_URL or wttr.in).
url=\${@: -1}
if [[ \$url == file://* ]]; then
  cat -- "\${url#file://}"
else
  cat -- "$scratch/ok.json"
fi
EOF
chmod +x "$scratch/bin/curl"

cat > "$scratch/ui/Test.qml" <<'QML'
import QtQuick
import Quickshell
import Quickshell.Io
import "Location.js" as Location
ShellRoot {
    id: test
    Engine {
        id: fake
        property var sent: []
        function send(command) { sent = sent.concat([command]); }
    }
    FloatingWindow {
        visible: true
        implicitWidth: 308
        implicitHeight: 260
        LocationPrompt {
            id: consent
            anchors.fill: parent
            session: PluginSession
            theme: PluginSession.theme.snapshot
            onManualChosen: PluginSession.requestLocationPicker()
        }
    }
    property int step: 0
    function action(name) {
        for (var child of consent.children)
            if (child.objectName === name) return child;
        throw new Error("Missing consent action: " + name);
    }
    function assertThat(ok, why) { if (!ok) throw new Error(why); }
    function fresh(values, saved, weather, source) {
        var s = PluginSession;
        if (s.locator.running) s.locator.running = false;
        fake.state = null;
        s.initialized = false;
        s.hasView = false;
        s.needsLocation = false;
        s.ipLocationDismissed = true;
        s.locationPending = false;
        s.locationError = "";
        s.appliedExplicit = null;
        s.config.values = values;
        s.remembered.parsed = Location.parseState(saved);
        s.config.location = weather;
        fake.sent = [];
        fake.state = {source: source || "live", site: {id: "", locked: false, follow: true}};
        s.initialize();
    }
    function waitSettled(next) {
        waiter.next = next;
        waiter.ticks = 0;
        waiter.start();
    }
    Timer {
        id: waiter
        property var next
        property int ticks: 0
        interval: 20; repeat: true
        onTriggered: {
            ticks++;
            var s = PluginSession;
            // Up to 10 s: mise check runs this beside the Rust build, and a
            // lookup is a few processes (bash, curl, head).
            if (s.locationPending && ticks < 500) return;
            stop();
            try { next(); } catch (e) { console.error(e); Qt.quit(); }
        }
    }
    Timer {
        interval: 10; running: true; repeat: true
        onTriggered: {
            var s = PluginSession;
            if (!s.ready) return;
            stop();
            try {
                s.engine = fake;
                var cap = Quickshell.env("OMASTORM_LOCATE_CASE");
                if (cap) { capCase(cap); return; }
                var good = Location.parseWttrHome('{"nearest_area":[{"areaName":[{"value":"Stamford"}],"latitude":"41.05","longitude":"-73.54"}]}');
                assertThat(good && good.lat === 41.05 && good.name === "Stamford", "parse wttr home");
                assertThat(Location.parseWttrHome("{}") === null, "reject empty wttr");
                assertThat(Location.parseWttrHome('{"nearest_area":[{"latitude":"91","longitude":"0"}]}') === null, "reject bad wttr coords");
                assertThat(Location.parseWttrHome('{"nearest_area":[{"latitude":null,"longitude":null}]}') === null, "reject null wttr coords");
                assertThat(Location.parseWttrHome('{"nearest_area":[{"latitude":"","longitude":""}]}') === null, "reject blank wttr coords");

                fresh({}, "", null);
                assertThat(!s.locating && s.needsLocation && !s.locationPending, "startup requires consent");
                s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"X"}],"latitude":"1","longitude":"2"}]}');
                assertThat(!s.hasView, "unsolicited finish ignored while dismissed");

                action("approximateLocation").clicked();
                assertThat(s.locating && s.needsLocation && s.locationPending, "fresh lookup");
                s.requestIpLocation();
                assertThat(s.locationPending, "duplicate lookup ignored while pending");
                waitSettled(afterFirstLookup);
            } catch (e) { console.error(e); Qt.quit(); }
        }
    }
    // The reply cap: every reply here is valid JSON (ok.json padded with
    // spaces), so only the cap can refuse it, and a cut at the cap would
    // still parse.
    function capCase(expect) {
        var s = PluginSession;
        fresh({}, "", null);
        action("approximateLocation").clicked();
        waitSettled(function () {
            if (expect === "found")
                assertThat(s.hasView && s.locationSource === "ip" && s.placeName === "Stamford", "a reply under the cap is used");
            else
                assertThat(!s.hasView && s.needsLocation && !s.locationPending && !!s.locationError, "a reply over the cap fails like a network error");
            console.log("IP_CAP_PASSED " + expect);
            Qt.quit();
        });
    }
    function afterFirstLookup() {
        var s = PluginSession;
        assertThat(s.hasView && !s.needsLocation && s.locationSource === "ip", "IP view");
        assertThat(s.centerLat === 41.05 && s.centerLon === -73.54, "IP coordinates");
        assertThat(s.remembered.lat === 41.05 && s.remembered.lon === -73.54, "remember IP view");
        assertThat(fake.sent.some(c => c.type === "view_center" && c.lat === 41.05), "nearest radar follows IP center");

        fresh({}, JSON.stringify(s.remembered.parsed), null);
        assertThat(s.locationSource === "state" && !s.locationPending, "reopen remembered IP without lookup");
        fresh({center_lat:30, center_lon:-81}, '{"lat":35,"lon":-97}', {lat:36,lon:-79});
        assertThat(s.locationSource === "config" && s.centerLat === 30, "explicit precedence");
        fresh({}, '{"lat":35,"lon":-97}', {lat:36,lon:-79});
        assertThat(s.locationSource === "state" && s.centerLat === 35, "state precedence");
        fresh({}, "", {lat:36,lon:-79});
        assertThat(s.locationSource === "weather" && s.centerLat === 36, "weather precedence");

        fresh({locked_radar:"KTLX"}, "", null);
        s.requestIpLocation();
        waitSettled(afterLock);
    }
    function afterLock() {
        var s = PluginSession;
        assertThat(s.lockId === "KTLX" && s.lockWanted && s.centerLat === 41.05, "configured lock independent of IP center");

        fresh({}, "", null);
        s.requestIpLocation();
        s.setPlace(30,-81,"Picked");
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Late"}],"latitude":"41.05","longitude":"-73.54"}]}');
        assertThat(s.centerLat === 30 && s.placeName === "Picked", "late reply after picker choice");

        fresh({}, "", null);
        s.requestIpLocation();
        s.chooseRadar("KTLX",35,-97,"Radar");
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Late"}],"latitude":"41.05","longitude":"-73.54"}]}');
        assertThat(s.centerLat === 35 && s.lockId === "KTLX", "late reply after radar choice");

        fresh({}, "", null);
        s.requestIpLocation();
        s.userNavigated(36,-98,170);
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Late"}],"latitude":"41.05","longitude":"-73.54"}]}');
        assertThat(s.centerLat === 36 && s.span === 170 && !s.needsLocation, "late reply after navigation");

        fresh({}, "", null);
        s.requestIpLocation();
        action("manualLocation").clicked();
        assertThat(s.needsLocation && !s.hasView && !s.locating, "manual picker interrupts lookup");

        fresh({}, "", null);
        s.locateAttempt += 1;
        s.activeAttempt = s.locateAttempt;
        s.ipLocationDismissed = false;
        s.locationPending = true;
        s.finishIpLocation(22, "", s.locateAttempt);
        assertThat(s.needsLocation && !s.locating && !!s.locationError, "failure leaves onboarding available");
        s.locateAttempt += 1;
        s.activeAttempt = s.locateAttempt;
        s.ipLocationDismissed = false;
        s.locationPending = true;
        s.locationError = "";
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Stamford"}],"latitude":"41.05","longitude":"-73.54"}]}', s.locateAttempt);
        assertThat(s.hasView && s.locationSource === "ip", "retry accepts successful reply");

        fresh({}, "", null);
        s.locateAttempt += 1;
        s.activeAttempt = s.locateAttempt;
        s.ipLocationDismissed = false;
        s.locationPending = true;
        s.config.location = {lat: 36, lon: -79, name: "Stokesdale"};
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Late"}],"latitude":"41.05","longitude":"-73.54"}]}', s.locateAttempt);
        assertThat(s.locationSource === "weather" && s.centerLat === 36 && !s.locationPending, "resolve mid-lookup clears pending");

        fresh({}, "", null);
        s.locateAttempt += 1;
        s.activeAttempt = s.locateAttempt;
        s.ipLocationDismissed = false;
        s.locationPending = true;
        fake.state = {source: "archived", site: {id: "", locked: false, follow: true}};
        s.finishIpLocation(0, '{"nearest_area":[{"areaName":[{"value":"Late"}],"latitude":"41.05","longitude":"-73.54"}]}', s.locateAttempt);
        assertThat(s.needsLocation && !s.hasView && !s.locationPending, "archive mid-lookup clears pending");

        fresh({}, "", null);
        assertThat(s.needsLocation && !s.locationPending, "no automatic lookup");
        fresh({}, "", null, "archived");
        s.requestIpLocation();
        assertThat(s.needsLocation && !s.locationPending, "archive never locates");
        assertThat(s.config.parseLocation('{"latitude":null,"longitude":null}') === null, "null weather is not zero");
        console.log("IP_LOCATION_PASSED");
        Qt.quit();
    }
}
QML

OMASTORM_CONFIG="$scratch/empty.toml" OMASTORM_STATE="$scratch/state.json" \
  OMASTORM_LOCATION_URL="file://$scratch/ok.json" \
  PATH="$scratch/bin:$PATH" \
  XDG_RUNTIME_DIR="$scratch/runtime" QT_QPA_PLATFORM=offscreen \
  timeout 20 quickshell -p "$scratch/ui/Test.qml" > "$scratch/log" 2>&1 || { cat "$scratch/log"; exit 1; }
cat "$scratch/log"
rg -q IP_LOCATION_PASSED "$scratch/log"
if rg -q 'ReferenceError|TypeError|Binding loop|Unable to assign' "$scratch/log"; then exit 1; fi

# The reply cap (64 KiB). The real curl against scripts/fake-download.py:
# under the cap the place is found; one byte over it, or megabytes over,
# chunked with no Content-Length, the lookup fails. Then the stub, a curl
# that ignores --max-filesize: head and the length check refuse it too.
python3 scripts/fake-download.py "$scratch/port" "$scratch/ok.json" & server=$!
# Up to 10 s for the port, then a clear failure rather than a bad URL.
for _ in {1..100}; do [[ -s $scratch/port ]] && break; sleep .1; done
[[ -s $scratch/port ]] || { echo 'The fake server did not start' >&2; exit 1; }
base=http://127.0.0.1:$(cat "$scratch/port")
printf '%s%*s' "$fixture" 100000 '' > "$scratch/padded.json"
cap_case() { # expect, url, PATH
  OMASTORM_LOCATE_CASE=$1 OMASTORM_LOCATION_URL=$2 PATH=$3 \
    OMASTORM_CONFIG="$scratch/empty.toml" OMASTORM_STATE="$scratch/cap-state.json" \
    XDG_RUNTIME_DIR="$scratch/runtime" QT_QPA_PLATFORM=offscreen \
    timeout 20 quickshell -p "$scratch/ui/Test.qml" > "$scratch/cap.log" 2>&1 || { cat "$scratch/cap.log"; exit 1; }
  rg -q "IP_CAP_PASSED $1" "$scratch/cap.log" || { cat "$scratch/cap.log"; echo "Reply cap: $2 was not $1" >&2; exit 1; }
  if rg -q 'ReferenceError|TypeError|Binding loop|Unable to assign' "$scratch/cap.log"; then cat "$scratch/cap.log"; exit 1; fi
  rm -f "$scratch/cap-state.json"
}
cap_case found "$base/padded/60000/ok.json" "$PATH"
cap_case failed "$base/padded/65537/ok.json" "$PATH"
cap_case failed "$base/padded/$((10 << 20))/ok.json" "$PATH"
cap_case failed "file://$scratch/padded.json" "$scratch/bin:$PATH"
echo 'IP location reply cap PASS'
