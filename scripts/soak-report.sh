#!/usr/bin/env bash
# S18 soak report: what the two home engines hold and what they asked for.
# READ-ONLY. It reads each engine's engine.log, its frame catalog (SQLite,
# opened mode=ro) and cache directories, /proc, and the omastorm-web journal.
# It never stops, signals or `ensure`s an engine and writes nothing.
#
#   scripts/soak-report.sh                  # everything in the logs, per day
#   scripts/soak-report.sh --by hour        # per UTC hour
#   scripts/soak-report.sh --since 2026-09-14T12   # lines from then (UTC prefix)
#   scripts/soak-report.sh --engine web     # web | bar | both (default)
#
# Engines (override for a private run):
#   web: WEB_RUNTIME=/run/user/1000/omastorm-web/omastorm-nord  WEB_CACHE=~/.cache/omastorm-web/omastorm-nord
#   bar: BAR_RUNTIME=/run/user/1000/omastorm-nord               BAR_CACHE=~/.cache/omastorm-nord
#   WEB_UNIT=omastorm-web (the gateway whose journal holds the /tex lines; empty: skip)
#
# Sources of the request numbers:
#   - "Net <provider>: requests=… bytes=… http429=… http5xx=… http4xx=… failed=…"
#     lines (engine since S18, every 15 min): every request, listings included.
#   - "backfilled … from <SMHI|ORD|EUMETNET OPERA> …: N range requests, B of T bytes"
#     lines: per-file cost of backfilled frames (live frames are not logged
#     with their cost), the only numbers an engine older than S18 has.
#     Frames the tilt store made (S27, "from the tilt store") cost nothing
#     and are counted apart, in the tilt store section.
# The tilt store (S27): <cache>/tilts/index.sqlite, opened mode=ro, and the
# "Tilts …" lines of engine.log.
# engine.log lives in /run (tmpfs): a reboot empties it, a restart appends.
set -euo pipefail

by=day since="" which=both
while (($#)); do
  case $1 in
    --by) by=$2; shift 2 ;;
    --since) since=$2; shift 2 ;;
    --engine) which=$2; shift 2 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
case $by in day) width=10 ;; hour) width=13 ;; *) echo "--by day|hour" >&2; exit 2 ;; esac

uid=$(id -u)
WEB_RUNTIME=${WEB_RUNTIME:-/run/user/$uid/omastorm-web/omastorm-nord}
WEB_CACHE=${WEB_CACHE:-$HOME/.cache/omastorm-web/omastorm-nord}
BAR_RUNTIME=${BAR_RUNTIME:-/run/user/$uid/omastorm-nord}
BAR_CACHE=${BAR_CACHE:-$HOME/.cache/omastorm-nord}
WEB_UNIT=${WEB_UNIT-omastorm-web}

mb() { awk -v b="$1" 'BEGIN { printf "%.1f MB", b / 1e6 }'; }

# Timestamped log lines at or after --since (a UTC prefix compares as text).
log_lines() {
  [[ -r $1 ]] || return 0
  awk -v since="$since" 'since == "" || ($1 ~ /^20[0-9][0-9]-/ && $1 >= since)' "$1"
}

process() { # runtime dir -> the serving engine whose stderr is its engine.log
  local log=$1/engine.log pid
  for pid in $(pgrep -f '^[^ ]*omastorm-engine serve' || true); do
    if [[ $(readlink "/proc/$pid/fd/2" 2>/dev/null) == "$log" ]]; then
      printf 'PID %s  %s\n' "$pid" "$(tr '\0' ' ' </proc/"$pid"/cmdline)"
      printf '  started %s, RSS %s MB\n' "$(ps -o lstart= -p "$pid")" \
        "$(ps -o rss= -p "$pid" | awk '{ printf "%.0f", $1 / 1024 }')"
      return
    fi
  done
  echo "no serving engine writes $log"
}

catalog() { # cache dir
  local frames=$1/frames db=$1/frames/catalog.sqlite
  if [[ ! -r $db ]]; then echo "  no catalog at $db"; return; fi
  printf '  %-13s %-6s %6s %6s %10s  %-20s  %-20s\n' station source frames codes bytes oldest newest
  local total=0 site n oldest newest src files codes bytes
  while IFS='|' read -r site n oldest newest src; do
    files=$(find "$frames/$site" -maxdepth 1 -name '*-sweep.png' 2>/dev/null | wc -l)
    codes=$(find "$frames/$site" -maxdepth 1 -name '*-codes.png' 2>/dev/null | wc -l)
    bytes=$(du -sb "$frames/$site" 2>/dev/null | cut -f1)
    total=$((total + ${bytes:-0}))
    printf '  %-13s %-6s %6s %6s %10s  %-20s  %-20s\n' "$site" "$src" "$n" "$codes" \
      "$(mb "${bytes:-0}")" "$oldest" "$newest"
    [[ $files == "$n" ]] || echo "    ($files sweep files on disk for $n rows)"
  done < <(sqlite3 -readonly "file:$db?mode=ro" "
      SELECT f.site, count(*), min(f.scan_time), max(f.scan_time),
             CASE WHEN n.provenance LIKE 'SMHI%' THEN 'smhi'
                  WHEN n.provenance LIKE 'ORD%' THEN 'ord'
                  WHEN n.provenance LIKE 'EUMETNET OPERA%' THEN 'opera'
                  ELSE '?' END
      FROM frames f JOIN frames n ON n.id = (
        SELECT id FROM frames WHERE site = f.site ORDER BY start_ms DESC LIMIT 1)
      GROUP BY f.site ORDER BY f.site")
  local sweeps vt_n vt_b db_b
  sweeps=$(find "$frames" -mindepth 2 -name '*-sweep.png' | wc -l)
  vt_n=$(find "$1/vt" -type f 2>/dev/null | wc -l)
  vt_b=$(du -sb "$1/vt" 2>/dev/null | cut -f1)
  db_b=$(du -cb "$db"* 2>/dev/null | tail -1 | cut -f1)
  printf '  cache: %s frames, %s in station dirs; catalog db %s; vt %s tiles, %s\n' \
    "$sweeps" "$(mb $total)" "$(mb "${db_b:-0}")" "$vt_n" "$(mb "${vt_b:-0}")"
}

net() { # log
  local lines
  lines=$(log_lines "$1" | grep ' Net [a-z]*: requests=' || true)
  if [[ -z $lines ]]; then
    echo "  no Net lines (engine older than S18, or no traffic yet)"
    return
  fi
  awk -v w="$width" '
    { t = substr($1, 1, w); p = $3; sub(":", "", p)
      if (!((t, p, substr($1, 1, 13)) in hours)) { hours[t, p, substr($1, 1, 13)] = 1; active[t, p]++ }
      for (i = 4; i <= NF; i++) { split($i, kv, "="); v[t, p, kv[1]] += kv[2] }
      if (!((t, p) in seen)) { seen[t, p] = 1; keys[++n] = t SUBSEP p } }
    END {
      printf "  %-13s %-6s %9s %9s %11s %5s %5s %5s %6s\n", "period", "prov", "requests", "req/act.h", "bytes", "429", "5xx", "4xx", "failed"
      for (i = 1; i <= n; i++) { split(keys[i], k, SUBSEP); t = k[1]; p = k[2]
        h = active[t, p]  # hours that logged traffic; an idle hour logs nothing
        printf "  %-13s %-6s %9d %9.0f %8.1f MB %5d %5d %5d %6d\n", t, p, v[t, p, "requests"], v[t, p, "requests"] / h,
          v[t, p, "bytes"] / 1e6, v[t, p, "http429"], v[t, p, "http5xx"], v[t, p, "http4xx"], v[t, p, "failed"] } }' <<<"$lines"
}

files() { # log: per-file cost of backfilled frames, by provider
  local lines
  lines=$(log_lines "$1" | grep -E ' backfilled [^ ]+ from ' | grep -v 'from the tilt store' || true)
  [[ -n $lines ]] || { echo "  no backfilled frames"; return; }
  awk -v w="$width" '
    { t = substr($1, 1, w); site = $3; sub(":", "", site)
      p = ($7 == "SMHI") ? "smhi-" $8 : ($7 == "ORD") ? "ord-" substr(site, 1, 2) : ($7 == "EUMETNET") ? "opera" : $7
      for (i = 1; i <= NF; i++) if ($i == "range") { r = $(i - 1); b = $(i + 2); tot = $(i + 4) }
      k = t SUBSEP p; if (!(k in f)) keys[++n] = k
      f[k]++; req[k] += r; by[k] += b; all[k] += tot }
    END {
      printf "  %-13s %-10s %6s %9s %10s %11s %6s\n", "period", "prov", "files", "req/file", "KB/file", "file KB", "read"
      for (i = 1; i <= n; i++) { k = keys[i]; split(k, kk, SUBSEP)
        printf "  %-13s %-10s %6d %9.1f %10.0f %11.0f %5.0f%%\n", kk[1], kk[2], f[k], req[k] / f[k], by[k] / f[k] / 1e3,
          all[k] / f[k] / 1e3, all[k] ? 100 * by[k] / all[k] : 0 } }' <<<"$lines"
}

tilts() { # cache dir, log: the tilt store (S27) per station, then its log lines
  local db=$1/tilts/index.sqlite log=$2
  if [[ -r $db ]]; then
    printf '  %-13s %7s %6s %10s  %-17s  %-17s\n' station volumes tilts bytes oldest newest
    local st vols n b old new
    while IFS='|' read -r st vols n b old new; do
      printf '  %-13s %7s %6s %10s  %-17s  %-17s\n' "$st" "$vols" "$n" "$(mb "$b")" "$old" "$new"
    done < <(sqlite3 -readonly "file:$db?mode=ro" "
        SELECT station, count(DISTINCT time_ms), count(*), sum(bytes),
               strftime('%Y-%m-%dT%H:%MZ', min(time_ms) / 1000, 'unixepoch'),
               strftime('%Y-%m-%dT%H:%MZ', max(time_ms) / 1000, 'unixepoch')
        FROM tilts GROUP BY station ORDER BY station")
    local all db_b
    all=$(sqlite3 -readonly "file:$db?mode=ro" "SELECT count(*) || ' ' || coalesce(sum(bytes), 0) FROM tilts")
    db_b=$(du -cb "$db"* 2>/dev/null | tail -1 | cut -f1)
    printf '  tilts: %s tilts, %s of .u8z files; index db %s\n' "${all% *}" "$(mb "${all#* }")" "$(mb "${db_b:-0}")"
  else
    echo "  no tilt store at $db (engine older than S27, or OMASTORM_TILTS_MB=0)"
  fi
  [[ -r $log ]] || return 0
  local cap evictions made
  cap=$(log_lines "$log" | grep -E ' Tilts [^ ]+: .*cap ' | tail -1 | sed -E 's/.*cap ([0-9.]+ MB).*/\1/' || true)
  evictions=$(log_lines "$log" | grep -c ' Tilts store: evicted' || true)
  made=$(log_lines "$log" | grep -c 'from the tilt store' || true)
  printf '  log: cap %s, %s evictions, %s backfilled frames made from the store (0 requests)\n' \
    "${cap:-?}" "${evictions:-0}" "${made:-0}"
}

episodes() { # log: condition changes (S18 Status lines) and the pollers' complaints
  local log=$1
  [[ -r $log ]] || return 0
  local status
  status=$(log_lines "$log" | grep -E '^[^ ]+ Status [^ ]+: ' || true)
  if [[ -n $status ]]; then
    echo "  episodes (stale / unavailable / offline, from Status lines):"
    awk -v now="$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
      function secs(t,   c) { c = "date -u -d " t " +%s"; c | getline s; close(c); return s }
      function close_ep(t) { if (cur != "" && cur != "ok" && cur != "loading")
          printf "    %-13s %-12s %s .. %s  %5.0f min\n", site, cur, from, t, (secs(t) - secs(from)) / 60 }
      { s = $3; sub(":", "", s); st = $4
        close_ep($1); site = s; cur = st; from = $1; n[st]++ }
      END { close_ep(now)
        printf "    transitions:"; for (k in n) printf " %s=%d", k, n[k]; print "" }' <<<"$status"
  else
    echo "  no Status lines (engine older than S18)"
  fi
  echo "  poller complaints (silent, retrying, offline, failed files):"
  grep -E 'published nothing|holds no|retrying|offline|HTTP [0-9]{3}|stopping|\(try [0-9]|range budget' "$log" |
    sed -E 's/^[0-9T:-]+Z //' | sort | uniq -c | sort -rn | head -15 | sed 's/^/   /' || true
  printf '  engine starts in this log: %s\n' "$(grep -c '^Ready with\|Archive ready' "$log" || true)"
}

report() { # name runtime cache
  local name=$1 rt=$2 cache=$3
  echo "=== $name engine: runtime $rt, cache $cache"
  echo "--- process"; process "$rt" | sed 's/^/  /'
  echo "--- catalog (frames per station)"; catalog "$cache"
  echo "--- requests per provider per $by (Net lines)"; net "$rt/engine.log"
  echo "--- backfilled files per provider per $by"; files "$rt/engine.log"
  echo "--- tilt store"; tilts "$cache" "$rt/engine.log"
  echo "--- staleness"; episodes "$rt/engine.log"
  echo
}

tex() {
  [[ -n $WEB_UNIT ]] || return 0
  echo "=== /tex requests (journalctl --user -u $WEB_UNIT, UTC)"
  local j from=()
  if [[ -n $since ]]; then # 2026-09-14 or 2026-09-14T12 (UTC) -> journalctl's form
    local s=${since/T/ }
    ((${#s} == 13)) && s+=":00"
    from=(--since "$s UTC")
  fi
  j=$(journalctl --user -u "$WEB_UNIT" --utc -o short-iso --no-pager "${from[@]}" 2>/dev/null |
      grep '"GET /tex/' || true)
  [[ -n $j ]] || { echo "  none"; return; }
  awk -v w="$width" '
    { t = substr($1, 1, w); k = "other"
      if ($0 ~ /GET \/tex\/sweep-/) k = "sweep"; else if ($0 ~ /GET \/tex\/azlut-/) k = "azlut"; else if ($0 ~ /GET \/tex\/codes-/) k = "codes"
      if (!(t in all)) keys[++n] = t
      all[t]++; c[t, k]++; if ($0 !~ /" 200 /) bad[t]++ }
    END { printf "  %-13s %7s %7s %7s %7s %7s\n", "period", "tex", "sweep", "azlut", "codes", "non200"
      for (i = 1; i <= n; i++) { t = keys[i]
        printf "  %-13s %7d %7d %7d %7d %7d\n", t, all[t], c[t, "sweep"], c[t, "azlut"], c[t, "codes"], bad[t] } }' <<<"$j"
}

echo "omastorm soak report $(date -u +%Y-%m-%dT%H:%MZ)${since:+ since $since} (per $by, UTC)"
echo
[[ $which == bar ]] || report web "$WEB_RUNTIME" "$WEB_CACHE"
[[ $which == web ]] || report bar "$BAR_RUNTIME" "$BAR_CACHE"
[[ $which == bar ]] || tex
