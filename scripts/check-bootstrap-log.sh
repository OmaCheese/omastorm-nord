#!/usr/bin/env bash
# The plugin bootstrap's log: run.sh --ensure makes its dir owner-only and
# the log afresh (600) on each bootstrap; a symlinked, non-regular or
# foreign dir or log is refused, for writing and for the popover's read
# (run.sh --bootstrap-log), and never followed; the read is at most the last
# 4 KiB. Then ui/PluginSession.qml: the popover shows the log's last line or
# the refusal, and without an absolute XDG_RUNTIME_DIR it says why and never
# bootstraps. A copy of run.sh runs a stand-in engine, so no daemon, network
# or personal file is involved.
set -euo pipefail
cd "$(dirname "$0")/.."
fail() { printf '%s\n' "$@" >&2; exit 1; }
# Short: Quickshell's socket goes under the runtime dir (107-byte limit).
scratch=$PWD/target/cbl
rm -rf "$scratch"
mkdir -p "$scratch/root/target/debug" "$scratch/rt" "$scratch/elsewhere"
chmod 700 "$scratch/rt"
trap 'rm -rf "$scratch"' EXIT
root=$scratch/root
cp -- run.sh "$root/run.sh"
# The stand-in engine says it ran: to stderr, which is the log, and a marker.
cat > "$root/target/debug/omastorm-engine" <<'ENGINE'
#!/bin/sh
echo "stand-in engine: $1" >&2
: > "$MARKER"
ENGINE
chmod +x "$root/target/debug/omastorm-engine"
export MARKER=$scratch/ran
rt=$scratch/rt
dir=$rt/omastorm-nord
log=$dir/bootstrap.log
ensure() { OMASTORM_BOOTSTRAP_LOG=$log bash "$root/run.sh" --ensure; }
read_log() { OMASTORM_BOOTSTRAP_LOG=$log timeout 5 bash "$root/run.sh" --bootstrap-log; }
refused() { # label, expected message, command...: it fails, says why, no engine runs
  local label=$1 want=$2
  shift 2
  rm -f "$MARKER"
  if "$@" > "$scratch/out" 2>&1; then fail "$label: accepted"; fi
  rg -qF -- "$want" "$scratch/out" || fail "$label: unclear: $(cat "$scratch/out")"
  [[ ! -e $MARKER ]] || fail "$label: the engine ran"
}
printf 'victim\n' > "$scratch/victim"

# A first bootstrap makes the dir 700 and the log 600, and the engine's
# stderr lands in the log; the next bootstrap starts a fresh one.
ensure
[[ $(stat -c %a "$dir") == 700 ]] || fail "The log dir is mode $(stat -c %a "$dir"), not 700"
[[ $(stat -c %a "$log") == 600 ]] || fail "The log is mode $(stat -c %a "$log"), not 600"
[[ $(cat "$log") == 'stand-in engine: ensure' ]] || fail "The log holds: $(cat "$log")"
[[ -e $MARKER ]] || fail 'The engine did not run'
printf 'an old line\n' > "$log"
ensure
[[ $(cat "$log") == 'stand-in engine: ensure' ]] || fail 'The next bootstrap did not start a fresh log'

# Planted at the log: a symlink (to a file, or to nothing yet), a FIFO.
rm -f "$log"
ln -s "$scratch/victim" "$log"
refused 'A symlinked log' "Refusing the bootstrap log $log: it is a symlink" ensure
[[ $(cat "$scratch/victim") == victim ]] || fail 'The bootstrap wrote through a symlinked log'
refused 'Reading a symlinked log' "Refusing the bootstrap log $log: it is a symlink" read_log
rm -f "$log"
ln -s "$scratch/nowhere" "$log"
refused 'A dangling symlinked log' "Refusing the bootstrap log $log: it is a symlink" ensure
[[ ! -e $scratch/nowhere ]] || fail 'The bootstrap created the target of a dangling symlink'
rm -f "$log"
ln -s /dev/zero "$log"
refused 'Reading a symlink to /dev/zero' "Refusing the bootstrap log $log: it is a symlink" read_log
rm -f "$log"
mkfifo "$log"
refused 'A FIFO log' "Refusing the bootstrap log $log: it is not a regular file" ensure
refused 'Reading a FIFO log' "Refusing the bootstrap log $log: it is not a regular file" read_log
# Planted at the dir: a symlink, or a file.
rm -rf "$dir"
ln -s "$scratch/elsewhere" "$dir"
refused 'A symlinked log dir' "Refusing the bootstrap log $dir: it is a symlink" ensure
[[ -z $(ls -A "$scratch/elsewhere") ]] || fail 'The bootstrap wrote into a symlinked dir'
rm -f "$dir"
: > "$dir"
refused 'A file for the log dir' "Refusing the bootstrap log $dir: it is not a directory" ensure
rm -f "$dir"
# Another user's dir or log: in a user namespace a root-owned path is
# unmapped (nobody), so one bind-mounted in place stands in. Some CI hosts
# allow no user namespaces; those refusals are then not exercised.
if unshare -rm true 2>/dev/null; then
  mkdir -m 700 "$dir"
  # shellcheck disable=SC2016 # $1..$3 belong to the inner shell
  refused "Another user's log dir" "Refusing the bootstrap log $dir: it is not owned by you" \
    unshare -rm bash -c 'mount --bind /usr/share "$1" && OMASTORM_BOOTSTRAP_LOG=$2 bash "$3" --ensure' _ "$dir" "$log" "$root/run.sh"
  : > "$log"
  # shellcheck disable=SC2016
  refused "Another user's log" "Refusing the bootstrap log $log: it is not owned by you" \
    unshare -rm bash -c 'mount --bind /etc/passwd "$1" && OMASTORM_BOOTSTRAP_LOG=$1 bash "$2" --ensure' _ "$log" "$root/run.sh"
  rm -rf "$dir"
else
  echo 'No user namespace here: the foreign-owner refusals were not exercised.' >&2
fi

# The read: nothing before a first bootstrap, then at most the last 4 KiB.
[[ -z $(read_log) ]] || fail 'A missing log read as something'
ensure
{ head -c $((1 << 20)) /dev/zero | tr '\0' x; printf '\nthe last line\n'; } > "$log"
read_log > "$scratch/read"
(( $(stat -c %s "$scratch/read") <= 4096 )) || fail "The read printed $(stat -c %s "$scratch/read") bytes"
[[ $(tail -n1 "$scratch/read") == 'the last line' ]] || fail 'The read lost the last line'
rm -rf "$dir"
echo 'run.sh bootstrap log: private dir and file, fresh log, symlink/FIFO/dir/foreign refused and not followed, bounded read PASS'

# The popover's side, through the shared session and the copy of run.sh.
cp -r ui "$scratch/ui"
cat > "$scratch/ui/Test.qml" <<'QML'
import QtQuick
import Quickshell
import Quickshell.Io
ShellRoot {
    id: test
    property var s: PluginSession
    property var steps: []
    function assertThat(ok, why) { if (!ok) { console.error("FAIL: " + why); Qt.quit(); } }
    // Run a shell step on the log, then wait for the popover's text.
    Process { id: shell; property var next; onExited: next() }
    function step(script, want, why, next) {
        shell.command = ["bash", "-c", script, "step", test.s.bootstrapLog];
        shell.next = function () { waitFor(want, why, next); };
        shell.running = true;
    }
    function waitFor(want, why, next) { waiter.want = want; waiter.why = why; waiter.next = next; waiter.ticks = 0; waiter.start(); }
    Timer {
        id: waiter
        property var want
        property string why
        property var next
        property int ticks: 0
        interval: 50; repeat: true
        onTriggered: {
            ticks++;
            if (!want(test.s.startupError) && ticks < 160) return;
            stop();
            test.assertThat(want(test.s.startupError), why + " (popover says: " + test.s.startupError + ")");
            next();
        }
    }
    Component.onCompleted: {
        s.popovers = 1;
        if (Quickshell.env("CHECK_CASE") === "noruntime") {
            waitFor(e => e === s.noRuntime, "no runtime dir: the reason", function () {
                assertThat(s.bootstrapLog === "" && !s.bootstrapRetry.running && !s.bootstrapLogPoll.running, "no runtime dir: no log, no retry, no read");
                console.log("BOOTSTRAP_LOG_PASSED noruntime");
                Qt.quit();
            });
            return;
        }
        waitFor(e => e === "stand-in engine: ensure", "the bootstrap's last line", function () {
            step('{ head -c 1048576 /dev/zero | tr "\\0" x; printf "\\nthe last line\\n"; } > "$1"',
                e => e === "the last line", "a 1 MiB log: its last line", function () {
                assertThat(s.bootstrapLogReader.stdout.data.byteLength <= 4096, "a 1 MiB log: read at most 4 KiB");
                step('rm -f "$1" && ln -s /dev/zero "$1"',
                    e => e.startsWith("Refusing the bootstrap log") && e.includes("it is a symlink"), "a symlink to /dev/zero: refused", function () {
                    step('rm -f "$1" && mkfifo "$1"',
                        e => e.startsWith("Refusing the bootstrap log") && e.includes("it is not a regular file"), "a FIFO: refused", function () {
                        console.log("BOOTSTRAP_LOG_PASSED log");
                        Qt.quit();
                    });
                });
            });
        });
    }
}
QML
popover() { # case, XDG_RUNTIME_DIR: the session reads the log as the popover does
  (cd "$scratch" && CHECK_CASE=$1 XDG_RUNTIME_DIR=$2 OMASTORM_ROOT=$root \
    OMASTORM_CONFIG="$scratch/empty.toml" OMASTORM_STATE="$scratch/state.json" QT_QPA_PLATFORM=offscreen \
    timeout 40 quickshell -p "$scratch/ui/Test.qml") > "$scratch/qml.log" 2>&1 || { cat "$scratch/qml.log"; fail "Popover case $1 did not finish"; }
  rg -q "BOOTSTRAP_LOG_PASSED $1" "$scratch/qml.log" || { cat "$scratch/qml.log"; fail "Popover case $1 failed"; }
  if rg -q 'ReferenceError|TypeError|Binding loop|Unable to assign' "$scratch/qml.log"; then cat "$scratch/qml.log"; fail "Popover case $1: QML errors"; fi
}
rm -f "$MARKER"
popover log "$rt"
[[ -e $MARKER ]] || fail 'The popover did not bootstrap'
# A relative runtime dir, as the engine refuses: the reason, and no bootstrap.
mkdir -m 700 "$scratch/relative"
rm -f "$MARKER"
popover noruntime relative
[[ ! -e $MARKER ]] || fail 'The popover bootstrapped without a runtime dir'
echo 'Popover bootstrap log: last line, 4 KiB read, symlink and FIFO refused, no runtime dir PASS'
