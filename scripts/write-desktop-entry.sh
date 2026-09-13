#!/usr/bin/env bash
# Opt-in app-launcher entry (DESIGN.md, launcher entry). Writes
# $XDG_DATA_HOME/applications/omastorm-se.desktop whose Exec is the shell
# toggle. Ordinary install and launch never call this; the user runs it.
set -euo pipefail
cd "$(dirname "$0")/.."

die() { printf '%s\n' "$@" >&2; exit 1; }

case ${1:-} in
  ''|--print-path) ;;
  *) die "usage: write-desktop-entry.sh [--print-path]" ;;
esac

root=$PWD
mark=$root/branding/mark/omastorm-mark-app.svg
[[ -f $mark ]] || die "Omastorm mark missing: $mark"

data_home=${XDG_DATA_HOME:-${HOME:?}/.local/share}
apps=$data_home/applications
desktop=$apps/omastorm-se.desktop

mkdir -p -- "$apps"
tmp=$(mktemp -- "$apps/omastorm-se.desktop.XXXXXX")
trap 'rm -f -- "$tmp"' EXIT
cat > "$tmp" <<EOF
[Desktop Entry]
Type=Application
Name=Omastorm SE
GenericName=Weather radar
Comment=Live SMHI radar for the Omarchy desktop
Exec=omarchy shell shell toggle rb.omastorm-se "{}"
TryExec=omarchy
Icon=$mark
Terminal=false
StartupNotify=false
Categories=Science;
Keywords=radar;SMHI;weather;
EOF
chmod 644 -- "$tmp"
mv -f -- "$tmp" "$desktop"
trap - EXIT

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$apps" >/dev/null 2>&1 || true
fi

if [[ ${1:-} == --print-path ]]; then
  printf '%s\n' "$desktop"
fi
exit 0
