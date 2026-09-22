#!/usr/bin/env bash
# The harness shell for captures of states the real feed cannot be asked
# for: the real UI files, and an Engine that lays OMASTORM_STATE_OVERRIDE (a
# JSON object) over every state it receives, one level deep, so a `frame`
# or `connection` field can change while the rest stays real. Prints the
# shell's path for OMASTORM_QML.
#
# `--input` (S40) also gives the window an `input` IPC target that sends real
# key, wheel and pointer events through Qt Quick's own delivery (QtTest's
# TestEvent, the window its target), so a check can press Down with the
# pointer resting over a list, or turn the wheel over a panel, and see what
# a person would: `key Down`, `text fi`, `wheel x y dy`, `move x y`,
# `click x y` in surface pixels, `where <map|frame|mosaic|chip>` for a
# rectangle, `mark <id>` for a radar's point on the map, `grab <png>`.
set -euo pipefail
cd "$(dirname "$0")/.."
input=0
[[ ${1:-} == --input ]] && input=1
# Harness outside the checkout: Omarchy rejects a shaders symlink in the plugin folder.
# Every UI file, so a file added or removed (S37's Reach.qml) cannot break it.
harness=$(mktemp -d "${TMPDIR:-/tmp}/omastorm-capture-harness.XXXXXX")
cp ui/*.qml ui/*.js ui/qmldir "$harness/"
ln -sfn "$PWD/ui/shaders" "$harness/shaders"
perl -pe 's/^(\s+)state = message;$/$1var override = JSON.parse(Quickshell.env("OMASTORM_STATE_OVERRIDE") || "{}");\n$1for (var key in override) message[key] = override[key] && typeof override[key] === "object" && !Array.isArray(override[key]) && message[key] ? Object.assign(message[key], override[key]) : override[key];\n$1state = message;/' ui/Engine.qml > "$harness/Engine.qml"
grep -q OMASTORM_STATE_OVERRIDE "$harness/Engine.qml"
if (( input )); then
  perl -0pi -e '
    s/^import QtQuick\n/import QtQuick\nimport QtTest\n/m;
    s/(\n(\s+)id: surface\n)/$1$2TestEvent { id: inputEvents }\n/;
    s/(\n    IpcHandler \{\n        target: "mosaic")/\n    IpcHandler {\n        target: "input"\n        function key(name: string): void { inputEvents.keyClick(Qt["Key_" + name], Qt.NoModifier, -1); }\n        function text(chars: string): void { for (var c of chars) inputEvents.keyClickChar(c, Qt.NoModifier, -1); }\n        function wheel(x: real, y: real, dy: int): void { inputEvents.mouseWheel(surface, x, y, Qt.NoButton, Qt.NoModifier, 0, dy, -1); }\n        function move(x: real, y: real): void { inputEvents.mouseMove(surface, x, y, -1, Qt.NoButton, Qt.NoModifier); }\n        function click(x: real, y: real): void { inputEvents.mouseClick(surface, x, y, Qt.LeftButton, Qt.NoModifier, -1); }\n        function where(name: string): string { var it = ({map: map, frame: mapFrame, mosaic: mosaicPicker, chip: radarsChip})[name]; var p = it.mapToItem(surface, 0, 0); return JSON.stringify({x: Math.round(p.x), y: Math.round(p.y), w: Math.round(it.width), h: Math.round(it.height), visible: it.visible}); }\n        function mark(id: string): string { var s = engine.sites.find(x => x.id === id); var p = map.mapToItem(surface, map.sx(map.mercatorX(s.lon)), map.sy(map.mercatorY(s.lat))); return JSON.stringify({x: Math.round(p.x), y: Math.round(p.y)}); }\n        function grab(path: string): void { surface.grabToImage(r => r.saveToFile(path)); }\n    }$1/;
  ' "$harness/RadarWindow.qml"
  grep -q 'target: "input"' "$harness/RadarWindow.qml"
  grep -q 'TestEvent { id: inputEvents }' "$harness/RadarWindow.qml"
fi
echo "$harness/shell.qml"
