#!/usr/bin/env bash
# Point the Omarchy bar plugin at this checkout, or restore the clone.
# Ordinary install and launch never call this; mise plugin-link / plugin-unlink do.
# --rescan restarts the Omarchy shell when this checkout is linked, so the bar
# drops cached QML (inotify and rescanPlugins do not follow the symlink).
set -euo pipefail
cd "$(dirname "$0")/.."

die() { printf '%s\n' "$@" >&2; exit 1; }

id=rb.omastorm-se
root=$(readlink -f "$PWD")
plugins=${XDG_CONFIG_HOME:-${HOME:?}/.config}/omarchy/plugins
target=$plugins/$id
backup=$plugins/.$id.unlinked

[[ -f $root/manifest.json ]] || die "Not an Omastorm checkout: missing $root/manifest.json"
got=$(jq -r .id "$root/manifest.json")
[[ $got == "$id" ]] || die "manifest id is $got, expected $id"

linked() {
  [[ -L $target ]] || return 1
  [[ $(readlink -f "$target") == "$root" ]]
}

shell_up() {
  command -v omarchy-shell >/dev/null 2>&1 || return 1
  omarchy-shell shell ping >/dev/null 2>&1
}

rescan() {
  command -v omarchy >/dev/null 2>&1 || return 0
  shell_up || return 0
  # rescanPlugins leaves the popover on the QML loaded before the symlink.
  omarchy restart shell >/dev/null 2>&1 || true
  local i
  for (( i = 0; i < 50; i++ )); do
    shell_up && return 0
    sleep 0.1
  done
}

enable() {
  if ! shell_up; then
    printf 'Enable it when the shell is running: omarchy plugin enable %s\n' "$id"
    return 0
  fi
  local attempt result
  local discovered=0
  for (( attempt = 0; attempt < 40; attempt++ )); do
    if omarchy-shell shell listPlugins 2>/dev/null | jq -e --arg id "$id" 'any(.[]; .id == $id)' >/dev/null; then
      discovered=1
      break
    fi
    sleep 0.05
  done
  if (( ! discovered )); then
    die "The shell does not know $id yet. Is omarchy-shell running?"
  fi
  result=$(omarchy-shell shell enablePlugin "$id" '{}')
  [[ $result == ok ]] || die "Could not enable $id: ${result:-no reply}"
  printf 'Enabled %s\n' "$id"
}

link() {
  mkdir -p -- "$plugins"
  if linked; then
    printf 'Already linked: %s -> %s\n' "$target" "$root"
    rescan
    enable
    return 0
  fi
  if [[ -L $target ]]; then
    die "Plugin is a symlink to $(readlink -f "$target"), not this checkout. Unlink that first."
  fi
  if [[ -e $target ]]; then
    [[ -d $target ]] || die "Refusing to replace $target (not a directory)"
    if [[ -e $backup || -L $backup ]]; then
      die "Kept clone already at $backup. mise plugin-unlink, or remove that backup, then retry."
    fi
    mv -- "$target" "$backup" || die "Could not move $target aside. Check ownership, then retry."
    printf 'Kept installed clone at %s\n' "$backup"
  fi
  ln -s -- "$root" "$target" || {
    if [[ -d $backup ]]; then
      mv -- "$backup" "$target" || true
    fi
    die "Could not link $target -> $root. Check ownership, then retry."
  }
  printf 'Linked %s -> %s\n' "$target" "$root"
  rescan
  enable
}

unlink() {
  if [[ ! -e $target && ! -L $target ]]; then
    die "Plugin is not installed at $target"
  fi
  linked || die "Plugin at $target is not a symlink to this checkout."
  rm -f -- "$target"
  if [[ -d $backup ]]; then
    mv -- "$backup" "$target" || die "Removed the link but could not restore $backup"
    printf 'Restored installed clone at %s\n' "$target"
  else
    printf 'Unlinked %s. No clone to restore; omarchy plugin add https://github.com/OmaCheese/omastorm-se --enable\n' "$target"
  fi
  rescan
}

case ${1:-} in
  ''|--link) link ;;
  --unlink) unlink ;;
  --rescan)
    linked || exit 0
    rescan
    ;;
  --status)
    if linked; then printf 'this checkout (linked)\n'
    else printf 'installed copy\n'
    fi
    ;;
  --print-path) printf '%s\n' "$target" ;;
  *) die "usage: link-plugin.sh [--link|--unlink|--rescan|--status|--print-path]" ;;
esac
