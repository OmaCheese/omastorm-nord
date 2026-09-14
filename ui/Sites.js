.pragma library
// The site picker's matching and ranking (DESIGN.md, picker as built) over
// hello.sites. Pure functions so a check can drive them without a window.
//
// Station ids are opaque (docs/protocol.md, DEC-12): a Swedish radar's is
// its SMHI area key (ornskoldsvik), every other radar's its ODIM node code
// (nohur), a composite's a region word (sweden, nordic). The name is the
// local spelling (Örnsköldsvik), the state column the county or region, and
// `country` the network's ISO code ('' for a composite spanning several).

// Country names a query can find a network by: English, then the local name.
var COUNTRIES = {
    SE: ["Sweden", "Sverige"], NO: ["Norway", "Norge"], FI: ["Finland", "Suomi"],
    DK: ["Denmark", "Danmark"], AX: ["Åland"], EE: ["Estonia", "Eesti"],
    LV: ["Latvia", "Latvija"], LT: ["Lithuania", "Lietuva"]
};
function countryName(code) { return code && COUNTRIES[code] ? COUNTRIES[code][0] : code || ""; }
function isGrid(site) { return site.kind === "grid"; }

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
//   0  the query starts the ID or one of its aliases (sevax is Vara)
//   1  the query starts a word of the name
//   2  the query starts a word of the county, or the country's code or name
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
    if ((site.aliases || []).some(a => fold(a).indexOf(q) === 0)) return { tier: 0, idHits: [], placeHits: [] };
    if ((i = wordStart(name, q)) >= 0) return { tier: 1, idHits: [], placeHits: range(i, q.length) };
    if (state && (i = wordStart(state, q)) >= 0) return { tier: 2, idHits: [], placeHits: range(stateAt + i, q.length) };
    if (site.country && [site.country].concat(COUNTRIES[site.country] || []).some(c => wordStart(fold(c), q) >= 0))
        return { tier: 2, idHits: [], placeHits: [] };
    if ((i = id.indexOf(q)) >= 0) return { tier: 3, idHits: range(i, q.length), placeHits: [] };
    if ((i = placeText.indexOf(q)) >= 0) return { tier: 3, idHits: [], placeHits: range(i, q.length) };
    var hits = subsequence(id + " " + placeText, q);
    if (!hits) return null;
    return { tier: 4, idHits: hits.filter(h => h < id.length), placeHits: hits.filter(h => h > id.length).map(h => h - id.length - 1) };
}

// The stations matching `query`, cut to `limit` rows: { rows, total }.
// Best tier first; within a tier the composites, then the radars grouped
// by country, the country with the radar nearest the centre first, and
// nearer the centre first inside a country. Each row has the station, its
// distance and bearing from the centre (none for a composite, which has no
// antenna), its tag (the country code, or COMPOSITE), whether it starts a
// new group, and the matched letter positions for the two columns.
function rank(sites, query, lat, lon, limit, metric) {
    var all = [], nearest = {};
    for (var site of sites) {
        var m = match(site, query);
        if (!m) continue;
        var km = distanceKm(lat, lon, site.lat, site.lon), grid = isGrid(site);
        var group = grid ? "" : site.country || "?";
        if (!grid && !(nearest[group] <= km)) nearest[group] = km;
        all.push({ site: site, tier: m.tier, km: km, group: group, tag: grid ? "COMPOSITE" : site.country || "",
                   where: grid ? "" : where(km, bearingDeg(lat, lon, site.lat, site.lon), metric),
                   idHits: m.idHits, placeHits: m.placeHits, place: place(site) });
    }
    var order = g => g === "" ? -1 : nearest[g];
    all.sort((a, b) => a.tier - b.tier || order(a.group) - order(b.group) || (a.group < b.group ? -1 : a.group > b.group ? 1 : 0)
             || a.km - b.km || (a.site.id < b.site.id ? -1 : 1));
    var rows = all.slice(0, limit);
    for (var j = 0; j < rows.length; j++) rows[j].groupStart = j > 0 && rows[j].group !== rows[j - 1].group;
    return { rows: rows, total: all.length };
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
