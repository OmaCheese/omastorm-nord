#!/usr/bin/env bash
# Installer and pin (DESIGN.md, distribution): hash verify, refuse a
# mismatch, install under a scratch XDG_DATA_HOME, skip a current dest,
# bound a download in time and bytes (a local server, no internet), replace
# only an engine this installer put there, select architecture-specific
# assets, reject an unsupported machine, keep a checkout --ensure off the
# installer. Uses a scratch pin and the debug engine so check.sh does not
# need a release rebuild. The committed pin is then installed for real and
# its asset must hash to the pin, speak the protocol the UI accepts, and
# report the version its tag names.
set -euo pipefail
# Ubuntu CI verifies installation and checksums for the published Arch binary,
# whose newer glibc requirements prevent execution there. Native candidates
# are executed separately; normal desktop checks always run the pinned one.
published_runtime=true
if [[ ${1:-} == --published-install-only ]]; then
  published_runtime=false
  shift
fi
[[ $# == 0 ]] || { echo 'Usage: test-engine-pin.sh [--published-install-only]' >&2; exit 2; }
cd "$(dirname "$0")/.."

fail() { printf '%s\n' "$@" >&2; exit 1; }
die() { fail "$@"; }
source scripts/engine-pin.sh
[[ -x target/debug/omastorm-engine ]] || fail 'Need target/debug/omastorm-engine (check.sh builds it).'

# Several copies of the debug engine and a tree of HEAD: under target/, and
# gone on exit, pass or fail.
scratch=$PWD/target/test-engine-pin
rm -rf "$scratch"
mkdir -p "$scratch/tmp"
server=
trap '[[ -z $server ]] || kill "$server" 2>/dev/null; rm -rf "$scratch"' EXIT
export XDG_DATA_HOME="$scratch/data" XDG_CACHE_HOME="$scratch/cache" XDG_RUNTIME_DIR="$scratch/runtime"
# Any temp file the installer made would land here, so a leftover is seen.
export TMPDIR=$scratch/tmp
mkdir -p "$XDG_RUNTIME_DIR"
# The debug build keeps its symbols (about 200 MB, over the installer's
# 64 MiB cap); stripped it is a working engine of about release size.
debug=$scratch/omastorm-engine
strip -o "$debug" target/debug/omastorm-engine
sum=$(sha256sum -- "$debug" | awk '{print $1}')
native=$(engine_machine "$(uname -m)")
export OMASTORM_ENGINE_MACHINE=$native
other=x86_64
[[ $native == x86_64 ]] && other=aarch64
# Distinct bytes catch a selector that uses the host's checksum for both CPUs.
printf 'other architecture fixture\n' > "$scratch/other"
other_sum=$(sha256sum -- "$scratch/other" | awk '{print $1}')
pin=$scratch/release.pin
cat > "$pin" <<PIN
tag=engine-test
repo=OmaCheese/omastorm-nord
asset_$native=omastorm-engine-$native-unknown-linux-gnu
sha256_$native=$sum
asset_$other=omastorm-engine-$other-unknown-linux-gnu
sha256_$other=$other_sum
PIN
export OMASTORM_ENGINE_PIN=$pin
dest=$XDG_DATA_HOME/omastorm-nord/bin/omastorm-engine
install_cmd=(bash scripts/fetch-engine.sh)
leftover() { # a temp file or a staged engine the installer left, if any
  { find "$TMPDIR" -maxdepth 1 -name 'omastorm-engine.*'; find "$(dirname "$dest")" -maxdepth 1 -name '.omastorm-engine.*'; } 2>/dev/null | head -n1
}
refused() { # label, expected message, command...: it fails, says why, leaves no temp
  local label=$1 want=$2
  shift 2
  if "$@" 2> "$scratch/refused.err"; then fail "$label: the installer accepted it"; fi
  rg -qF -- "$want" "$scratch/refused.err" || fail "$label: unclear error: $(cat "$scratch/refused.err")"
  [[ -z $(leftover) ]] || fail "$label left $(leftover)"
}

# A substituted file is refused and leaves no dest.
printf 'not-the-engine' > "$scratch/bogus"
if OMASTORM_ENGINE_ASSET="$scratch/bogus" "${install_cmd[@]}" 2>"$scratch/mismatch.err"; then
  fail 'Installer accepted a sha256 mismatch'
fi
rg -q 'sha256 mismatch' "$scratch/mismatch.err" || fail "Mismatch error was unclear: $(cat "$scratch/mismatch.err")"
# The popover shows the log's last line: the diagnosis is that line.
[[ $(tail -n1 "$scratch/mismatch.err") == 'Engine sha256 mismatch'* ]] || fail 'The mismatch is not the last line'
[[ ! -e $dest ]] || fail 'Mismatch wrote a dest'

# Matching asset installs, is executable, and hashes to the pin.
OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}"
[[ -x $dest ]] || fail 'Installer did not write an executable dest'
[[ $(sha256sum -- "$dest" | awk '{print $1}') == "$sum" ]] || fail 'Installed dest does not match the pin'
path=$(OMASTORM_ENGINE_ASSET="$debug" bash scripts/fetch-engine.sh --print-path)
[[ $path == "$dest" ]] || fail "--print-path: $path"

# A dest that already matches is left alone; no asset and no download.
unset OMASTORM_ENGINE_ASSET
bash scripts/fetch-engine.sh

# The curl path (file://, no GitHub) verifies and installs too.
rm -f "$dest"
OMASTORM_ENGINE_URL="file://$debug" bash scripts/fetch-engine.sh
[[ -x $dest && $(sha256sum -- "$dest" | awk '{print $1}') == "$sum" ]] || fail 'file:// install did not match the pin'

# Download bounds, against a local server (scripts/fake-download.py): a stall
# is given up at the deadline, and a reply over the 64 MiB cap is cut
# whether or not it names its length; curl's own refusal (exit 63) names
# the URL, so a chunked reply shows that curl cut it mid-stream. Nothing is
# installed, and the download (a hidden file beside dest) goes on each
# failure and when the installer is killed.
rm -f "$dest" "$dest.sha256"
python3 scripts/fake-download.py "$scratch/port" & server=$!
for _ in {1..50}; do [[ -s $scratch/port ]] && break; sleep .1; done
base=http://127.0.0.1:$(cat "$scratch/port")
started=$SECONDS
refused 'A stalled server' 'curl exit 28' env OMASTORM_ENGINE_MAX_TIME=2 OMASTORM_ENGINE_URL="$base/stall" "${install_cmd[@]}"
(( SECONDS - started <= 10 )) || fail "A stalled server took $((SECONDS - started)) s against a 2 s deadline"
[[ $(tail -n1 "$scratch/refused.err") == 'Could not download '*'curl exit 28'* ]] || fail 'The download failure is not the last line'
big=$((70 << 20))
refused 'A chunked reply over the cap' "from $base/chunked/$big: it is larger than the 64 MiB cap" \
  env OMASTORM_ENGINE_URL="$base/chunked/$big" "${install_cmd[@]}"
refused 'A Content-Length over the cap' "from $base/sized/$big: it is larger than the 64 MiB cap" \
  env OMASTORM_ENGINE_URL="$base/sized/$big" "${install_cmd[@]}"
truncate -s 65M "$scratch/huge"
refused 'A local asset over the cap' 'larger than the 64 MiB cap' env OMASTORM_ENGINE_ASSET="$scratch/huge" "${install_cmd[@]}"
# One installer at a time: while a stalled one holds the download, a second
# refuses at once. Killed (a logout), the stalled one still removes its
# download; setsid gives it and its curl one process group to signal.
setsid env OMASTORM_ENGINE_MAX_TIME=60 OMASTORM_ENGINE_URL="$base/stall" "${install_cmd[@]}" 2>/dev/null & stalled=$!
for _ in {1..50}; do [[ -n $(leftover) ]] && break; sleep .1; done
[[ -n $(leftover) ]] || fail 'The stalled installer staged no download'
started=$SECONDS
if OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}" 2> "$scratch/lock.err"; then fail 'A second installer ran beside the first'; fi
rg -qF 'already running' "$scratch/lock.err" || fail "Second installer error was unclear: $(cat "$scratch/lock.err")"
(( SECONDS - started <= 2 )) || fail 'The second installer waited for the first'
kill -TERM -- -"$stalled"
wait "$stalled" || true
[[ -z $(leftover) ]] || fail "A killed installer left $(leftover)"
[[ ! -e $dest ]] || fail 'A bounded download wrote a dest'
# SIGKILL runs no trap: the staged download stays until the next install,
# which removes it under the lock.
setsid env OMASTORM_ENGINE_MAX_TIME=60 OMASTORM_ENGINE_URL="$base/stall" "${install_cmd[@]}" 2>/dev/null & stalled=$!
for _ in {1..50}; do [[ -n $(leftover) ]] && break; sleep .1; done
kill -KILL -- -"$stalled"
wait "$stalled" || true
[[ -n $(leftover) ]] || fail 'SIGKILL left no staged download to clean up'
OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}"
[[ -z $(leftover) ]] || fail "The next install did not remove $(leftover)"
rm -f "$dest" "$dest.sha256"

# Provenance: a regular file at dest is replaced only while it has the hash
# this installer recorded beside it, or the pin's. A symlink (even to the
# pinned engine), a directory, a file this installer did not write, or one
# of another user is refused and left as it is.
OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}"
[[ $(cat "$dest.sha256") == "$sum" ]] || fail 'An install did not record its sha256'
# An engine installed before the record existed is adopted by a launch.
rm -f "$dest.sha256"
"${install_cmd[@]}"
[[ $(cat "$dest.sha256") == "$sum" ]] || fail 'A launch did not adopt the pinned engine'
# A symlink planted at the record is replaced, never written through.
printf 'victim\n' > "$scratch/victim"
rm -f "$dest.sha256"
ln -s "$scratch/victim" "$dest.sha256"
"${install_cmd[@]}"
[[ ! -L $dest.sha256 && $(cat "$dest.sha256") == "$sum" ]] || fail 'The record was not rewritten'
[[ $(cat "$scratch/victim") == victim ]] || fail 'The record was written through a symlink'
# A previously installed engine is replaced by a new pinned one, and back.
printf 'the next engine\n' > "$scratch/next"
next_sum=$(sha256sum -- "$scratch/next" | awk '{print $1}')
sed "s/^sha256_$native=.*/sha256_$native=$next_sum/" "$pin" > "$scratch/next.pin"
OMASTORM_ENGINE_PIN=$scratch/next.pin OMASTORM_ENGINE_ASSET=$scratch/next "${install_cmd[@]}"
[[ $(sha256sum -- "$dest" | awk '{print $1}') == "$next_sum" && $(cat "$dest.sha256") == "$next_sum" ]] \
  || fail 'A previous install was not replaced by the new pin'
OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}"
[[ $(sha256sum -- "$dest" | awk '{print $1}') == "$sum" ]] || fail 'The debug engine did not replace the next one'
# An engine an older plugin installed before the record existed, first
# launched after a pin bump: replaced when the new pin lists it as a
# previous_ hash (pin-engine-release.sh carries them forward), refused
# without that line. Two previous_ lines: the key repeats.
printf 'engine v1\n' > "$scratch/v1"
v1_sum=$(sha256sum -- "$scratch/v1" | awk '{print $1}')
rm -f "$dest" "$dest.sha256"
install -m 755 -- "$scratch/v1" "$dest"
refused 'An unrecorded older engine, not listed' "Refusing to replace $dest: it is not the engine this installer put there" \
  env OMASTORM_ENGINE_PIN="$scratch/next.pin" OMASTORM_ENGINE_ASSET="$scratch/next" "${install_cmd[@]}"
[[ $(cat "$dest") == 'engine v1' ]] || fail 'An unlisted older engine was replaced'
{ cat "$scratch/next.pin"; printf 'previous_sha256_%s=%s\n' "$native" "$other_sum" "$native" "$v1_sum"; } > "$scratch/previous.pin"
OMASTORM_ENGINE_PIN=$scratch/previous.pin OMASTORM_ENGINE_ASSET=$scratch/next "${install_cmd[@]}"
[[ $(sha256sum -- "$dest" | awk '{print $1}') == "$next_sum" && $(cat "$dest.sha256") == "$next_sum" ]] \
  || fail 'A listed older engine was not replaced by the new pin'
OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}"
# Someone's own file there.
rm -f "$dest"
printf 'my own tool\n' > "$dest"
chmod 755 -- "$dest"
refused 'A file this installer did not write' "Refusing to replace $dest: it is not the engine this installer put there" \
  env OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}"
[[ $(cat "$dest") == 'my own tool' ]] || fail 'A file this installer did not write was replaced'
rg -qF "rm -- $(printf %q "$dest")" "$scratch/refused.err" || fail 'The refusal does not say how to clear the path'
# The command it gives works for a path with a quote and a space in it.
quoted_home="$scratch/it's here"
mkdir -p "$quoted_home/omastorm-nord/bin"
printf 'mine\n' > "$quoted_home/omastorm-nord/bin/omastorm-engine"
if XDG_DATA_HOME=$quoted_home OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}" 2> "$scratch/quoted.err"; then fail 'A quoted path was replaced'; fi
clear=$(sed -n 's/.*(\(rm -- .*\)); Omastorm.*/\1/p' "$scratch/quoted.err")
bash -c "$clear" && [[ ! -e $quoted_home/omastorm-nord/bin/omastorm-engine ]] || fail "The given command did not clear the path: $clear"
# A symlink, to the pinned engine or to someone's file.
rm -f "$dest"
cp -- "$debug" "$scratch/linked"
ln -s "$scratch/linked" "$dest"
refused 'A symlink to the pinned engine' "Refusing to replace $dest: it is a symlink" "${install_cmd[@]}"
ln -sfn "$scratch/victim" "$dest"
refused 'A symlink to a file' "Refusing to replace $dest: it is a symlink" env OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}"
[[ -L $dest && $(cat "$scratch/victim") == victim ]] || fail 'The installer followed a symlink at dest'
rm -f "$dest"
mkdir "$dest"
refused 'A directory at dest' "Refusing to replace $dest: it is not a regular file" env OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}"
rmdir "$dest"
# Another user's file: in a user namespace a root-owned file is unmapped
# (nobody), so one bind-mounted over dest stands in. Some CI hosts allow
# no user namespaces; the refusal is then not exercised.
: > "$dest"
if unshare -rm true 2>/dev/null; then
  # shellcheck disable=SC2016 # $1 and $2 belong to the inner shell
  refused 'Another user'"'"'s file' "Refusing to replace $dest: it is not owned by you" \
    unshare -rm bash -c 'mount --bind /etc/passwd "$1" && OMASTORM_ENGINE_ASSET=$2 bash scripts/fetch-engine.sh' _ "$dest" "$debug"
else
  echo 'No user namespace here: the foreign-owner refusal was not exercised.' >&2
fi
rm -f "$dest"
OMASTORM_ENGINE_ASSET="$debug" "${install_cmd[@]}"

# Both architectures select their own asset and checksum, including arm64 alias.
for machine in x86_64 aarch64 arm64; do
  arch=$(engine_machine "$machine")
  fixture=$debug expected=$sum
  if [[ $arch != "$native" ]]; then fixture=$scratch/other; expected=$other_sum; fi
  OMASTORM_ENGINE_MACHINE=$machine OMASTORM_ENGINE_ASSET=$fixture "${install_cmd[@]}"
  [[ $(sha256sum -- "$dest" | awk '{print $1}') == "$expected" ]] || fail "Wrong asset for $machine"
done
# A host binary cannot pass verification for the other architecture.
rm -f "$dest"
if OMASTORM_ENGINE_MACHINE=$other OMASTORM_ENGINE_ASSET=$debug "${install_cmd[@]}" 2>"$scratch/arch.err"; then
  fail 'Installer accepted the other architecture checksum'
fi
rg -q 'sha256 mismatch' "$scratch/arch.err" || fail 'Wrong architecture did not fail checksum verification'
[[ ! -e $dest ]] || fail 'Wrong architecture wrote a dest'

# The normal download path must construct the architecture-specific release URL.
mkdir -p "$scratch/bin" "$scratch/downloads"
cp "$debug" "$scratch/downloads/omastorm-engine-$native-unknown-linux-gnu"
cp "$scratch/other" "$scratch/downloads/omastorm-engine-$other-unknown-linux-gnu"
cat > "$scratch/bin/curl" <<'CURL'
#!/usr/bin/env bash
set -euo pipefail
while [[ $# -gt 0 ]]; do
  case $1 in
    -o) out=$2; shift 2 ;;
    --) url=$2; break ;;
    *) shift ;;
  esac
done
printf '%s\n' "$url" > "$DOWNLOAD_FIXTURES/url"
cp "$DOWNLOAD_FIXTURES/${url##*/}" "$out"
CURL
chmod +x "$scratch/bin/curl"
for arch in x86_64 aarch64; do
  rm -f "$dest"
  PATH="$scratch/bin:$PATH" DOWNLOAD_FIXTURES=$scratch/downloads OMASTORM_ENGINE_MACHINE=$arch "${install_cmd[@]}"
  [[ $(cat "$scratch/downloads/url") == "https://github.com/OmaCheese/omastorm-nord/releases/download/engine-test/omastorm-engine-$arch-unknown-linux-gnu" ]] \
    || fail "Wrong download URL for $arch"
done
rm -f "$dest"

# An unsupported CPU and a supported CPU without a published pin never fetch.
if OMASTORM_ENGINE_MACHINE=armv7l "${install_cmd[@]}" 2>"$scratch/arch.err"; then
  fail 'Installer accepted armv7l'
fi
rg -q 'Unsupported engine architecture: armv7l' "$scratch/arch.err" || fail "Arch error was unclear: $(cat "$scratch/arch.err")"
sed "/^asset_$other=/d; /^sha256_$other=/d" "$pin" > "$scratch/missing.pin"
if OMASTORM_ENGINE_MACHINE=$other OMASTORM_ENGINE_PIN=$scratch/missing.pin "${install_cmd[@]}" 2>"$scratch/arch.err"; then
  fail 'Installer accepted an unpinned architecture'
fi
rg -q "No pinned $other engine" "$scratch/arch.err" || fail 'Missing architecture error was unclear'

# An incomplete or duplicated pin is refused before touching the destination.
sed "/^sha256_$native=/d" "$pin" > "$scratch/incomplete.pin"
cp "$pin" "$scratch/duplicate.pin"
printf 'sha256_%s=%s\n' "$native" "$sum" >> "$scratch/duplicate.pin"
cp "$pin" "$scratch/previous-bad.pin"
printf 'previous_sha256_%s=not-a-hash\n' "$native" >> "$scratch/previous-bad.pin"
for bad in incomplete duplicate previous-bad; do
  if OMASTORM_ENGINE_PIN=$scratch/$bad.pin "${install_cmd[@]}" 2>"$scratch/pin.err"; then
    fail "Installer accepted $bad pin"
  fi
done

# Checkout --ensure uses the debug engine and does not write the data home.
rm -rf "$XDG_DATA_HOME"
bash run.sh --ensure
[[ ! -e $dest ]] || fail 'Checkout --ensure wrote the release dest'
timeout 2 socat -t0.2 - "UNIX-CONNECT:$XDG_RUNTIME_DIR/omastorm-nord/engine.sock" < /dev/null | rg -q '"type":"hello"' \
  || fail 'Checkout --ensure did not produce a hello'
target/debug/omastorm-engine stop >/dev/null

# A tree without target/debug installs from the asset and ensures.
clone=$scratch/clone
mkdir -p "$clone"
git archive HEAD | tar -x -C "$clone"
# The launcher and installer come from the working tree so the check covers
# uncommitted changes to them; everything else is HEAD, as a clone would be.
mkdir -p "$clone/scripts" "$clone/engine"
cp -- run.sh "$clone/run.sh"
cp -- scripts/fetch-engine.sh "$clone/scripts/fetch-engine.sh"
cp -- scripts/engine-pin.sh "$clone/scripts/engine-pin.sh"
install -D -m 644 "$pin" "$clone/engine/release.pin"
rm -rf "$clone/target"
export OMASTORM_ENGINE_ASSET=$debug OMASTORM_ENGINE_PIN=$clone/engine/release.pin
(cd "$clone" && bash run.sh --ensure)
[[ -x $dest ]] || fail 'Clone --ensure did not install the engine'
timeout 2 socat -t0.2 - "UNIX-CONNECT:$XDG_RUNTIME_DIR/omastorm-nord/engine.sock" < /dev/null | rg -q '"type":"hello"' \
  || fail 'Clone --ensure did not produce a hello'
"$dest" stop >/dev/null

# The committed pin names what users get. Install from it for real: the
# asset the pin names must exist on GitHub, hash to the pin, speak the
# protocol version the UI accepts, and report the version its tag names.
# The asset is fetched once into target/pinned/<sha256> and reused. When
# GitHub is unreachable the step says so and passes; a checkout is correct
# without the network, and the fetch is retried on the next run.
read_engine_pin engine/release.pin
committed=${hashes[$native]:-}
if [[ -z $committed ]]; then
  echo "Engine install fixtures PASS; no published $native pin yet, native release check pending."
  exit 0
fi
[[ $committed =~ ^[a-f0-9]{64}$ ]] || fail 'Committed pin sha256 is not 64 lowercase hex digits'
[[ $tag =~ ^engine-([0-9]+\.[0-9]+\.[0-9]+)$ ]] || fail "Committed pin tag is not engine-<version>: $tag"
pinned_version=${BASH_REMATCH[1]}
ui_protocol=$(rg -o 'message\.v !== ([0-9]+)' -r '$1' ui/Engine.qml)
[[ -n $ui_protocol ]] || fail 'Could not read the protocol version ui/Engine.qml accepts'
cache=target/pinned/$committed
unset OMASTORM_ENGINE_ASSET
export OMASTORM_ENGINE_PIN=$PWD/engine/release.pin
rm -f "$dest"
if [[ -f $cache ]]; then
  OMASTORM_ENGINE_ASSET=$cache bash scripts/fetch-engine.sh
elif curl -fsI --max-time 5 https://github.com > /dev/null 2>&1; then
  bash scripts/fetch-engine.sh
  install -D -m 755 "$dest" "$cache"
else
  echo 'Committed pin: GitHub unreachable, the published asset was not verified this run.' >&2
fi
if [[ -x $dest ]]; then
  [[ $(sha256sum -- "$dest" | awk '{print $1}') == "$committed" ]] || fail 'Pinned asset install did not match the pin'
  if [[ $published_runtime == false ]]; then
    echo 'Engine install fixtures and published asset checksum PASS; published runtime explicitly omitted (host libc compatibility).'
    exit 0
  fi
  "$dest" ensure
  hello=$(timeout 2 socat -t0.2 - "UNIX-CONNECT:$XDG_RUNTIME_DIR/omastorm-nord/engine.sock" < /dev/null | head -n1 || true)
  "$dest" stop >/dev/null
  rg -q '"type":"hello"' <<< "$hello" || fail 'Pinned asset did not produce a hello'
  [[ $(jq -r .v <<< "$hello") == "$ui_protocol" ]] \
    || fail "Pinned asset speaks protocol v$(jq -r .v <<< "$hello"); ui/Engine.qml accepts v$ui_protocol"
  [[ $(jq -r .engine <<< "$hello") == "$pinned_version" ]] \
    || fail "Pinned asset reports engine $(jq -r .engine <<< "$hello"); the pin names $tag"
fi

echo 'Engine install: pin verify, mismatch refuse, dest install, skip current, download bounds, one installer, provenance, arch, checkout --ensure, clone --ensure, pinned asset PASS'
