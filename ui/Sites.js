.pragma library
// The site picker's matching and ranking (DESIGN.md, picker as built) over
// hello.sites. Pure functions so a check can drive them without a window.
//
// The table is SMHI's twelve radars: the ID is the API's ASCII area key
// (ornskoldsvik), the name its Swedish spelling (Örnsköldsvik), and the
// state column holds the county spelled out (Västernorrland).

function stateName(county) { return county || ""; }
// The row's place column: the name, then the county.
function place(site) { var s = stateName(site.state); return site.name.toUpperCase() + (s ? ", " + s.toUpperCase() : ""); }
// Lower case with the Nordic letters folded one for one (å ä → a, ö ø → o,
// æ → a, é → e), so "ostersund" finds Östersund and a hit's position in the
// folded text is its position in the shown one.
function fold(text) {
    return text.toLowerCase().replace(/[åäàáæ]/g, "a").replace(/[öøó]/g, "o").replace(/[éè]/g, "e").replace(/ü/g, "u");
}

function distanceKm(lat1, lon1, lat2, lon2) {
    var r = Math.PI / 180, dp = (lat2 - lat1) * r, dl = (lon2 - lon1) * r;
    var h = Math.sin(dp / 2) ** 2 + Math.cos(lat1 * r) * Math.cos(lat2 * r) * Math.sin(dl / 2) ** 2;
    return 2 * 6371 * Math.asin(Math.sqrt(Math.max(0, Math.min(1, h))));
}
// Initial great-circle bearing from the first point to the second, degrees clockwise from north.
function bearingDeg(lat1, lon1, lat2, lon2) {
    var r = Math.PI / 180, dl = (lon2 - lon1) * r;
    var y = Math.sin(dl) * Math.cos(lat2 * r);
    var x = Math.cos(lat1 * r) * Math.sin(lat2 * r) - Math.sin(lat1 * r) * Math.cos(lat2 * r) * Math.cos(dl);
    return (Math.atan2(y, x) * 180 / Math.PI + 360) % 360;
}
function compass(deg) { return ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][Math.round(deg / 45) % 8]; }
// `metric` from the caller's locale (Locale.MetricSystem); kilometres otherwise miles.
function where(km, deg, metric) {
    if (metric) return km < 1 ? "< 1 km" : Math.round(km) + " km " + compass(deg);
    var mi = km / 1.609344;
    return mi < 1 ? "< 1 mi" : Math.round(mi) + " mi " + compass(deg);
}

function range(from, count) { var out = []; for (var i = 0; i < count; i++) out.push(from + i); return out; }
// Index where `needle` starts a word of `text` (the start, or after a space or comma), or -1.
function wordStart(text, needle) {
    for (var i = text.indexOf(needle); i >= 0; i = text.indexOf(needle, i + 1))
        if (i === 0 || text[i - 1] === " " || text[i - 1] === ",") return i;
    return -1;
}
// The letters of `needle` in order through `text`, leftmost first; null if any is missing.
function subsequence(text, needle) {
    var hits = [], at = 0;
    for (var ch of needle) {
        if (ch === " ") continue;
        at = text.indexOf(ch, at);
        if (at < 0) return null;
        hits.push(at++);
    }
    return hits;
}

// How `query` matches one station: the tier it lands in and the matched
// letters in the ID and place columns, or null. Tiers, best first:
//   0  the query starts the ID
//   1  the query starts a word of the name
//   2  the query starts a word of the county
//   3  the query appears anywhere in the ID, name, or county
//   4  the query's letters appear in order across the ID and place
// All comparisons are on folded text (`fold`).
function match(site, query) {
    var q = fold(query.trim().replace(/\s+/g, " "));
    var id = fold(site.id), name = fold(site.name), placeText = fold(place(site));
    var state = fold(stateName(site.state));
    var stateAt = name.length + 2, i;
    if (!q) return { tier: 5, idHits: [], placeHits: [] };
    if (id.indexOf(q) === 0) return { tier: 0, idHits: range(0, q.length), placeHits: [] };
    if ((i = wordStart(name, q)) >= 0) return { tier: 1, idHits: [], placeHits: range(i, q.length) };
    if (state && (i = wordStart(state, q)) >= 0) return { tier: 2, idHits: [], placeHits: range(stateAt + i, q.length) };
    if ((i = id.indexOf(q)) >= 0) return { tier: 3, idHits: range(i, q.length), placeHits: [] };
    if ((i = placeText.indexOf(q)) >= 0) return { tier: 3, idHits: [], placeHits: range(i, q.length) };
    var hits = subsequence(id + " " + placeText, q);
    if (!hits) return null;
    return { tier: 4, idHits: hits.filter(h => h < id.length), placeHits: hits.filter(h => h > id.length).map(h => h - id.length - 1) };
}

// The stations matching `query`, best tier first and nearer the centre
// first within a tier, cut to `limit` rows: { rows, total }. Each row has
// the station, its distance and bearing from the centre, and the matched
// letter positions for the two columns.
function rank(sites, query, lat, lon, limit, metric) {
    var all = [];
    for (var site of sites) {
        var m = match(site, query);
        if (!m) continue;
        var km = distanceKm(lat, lon, site.lat, site.lon);
        all.push({ site: site, tier: m.tier, km: km, where: where(km, bearingDeg(lat, lon, site.lat, site.lon), metric),
                   idHits: m.idHits, placeHits: m.placeHits, place: place(site) });
    }
    all.sort((a, b) => a.tier - b.tier || a.km - b.km || (a.site.id < b.site.id ? -1 : 1));
    return { rows: all.slice(0, limit), total: all.length };
}

function escape(text) { return String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;"); }
// `text` as styled text with the letters at `hits` in `color`.
function mark(text, hits, color) {
    var out = "", open = false;
    for (var i = 0; i < text.length; i++) {
        var hit = hits.indexOf(i) >= 0;
        if (hit && !open) { out += "<font color=\"" + color + "\">"; open = true; }
        if (!hit && open) { out += "</font>"; open = false; }
        out += escape(text[i]);
    }
    return open ? out + "</font>" : out;
}
