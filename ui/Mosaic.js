.pragma library
// My mosaic (S25, docs/protocol.md "My mosaic") for the Qt surfaces: a set
// as state.mosaic names it ({sites: [{id, reachKm}], rule}) and the
// set_mosaic command that asks an engine for it.

var NOMINAL_KM = 240;

// A radar's full range (hello.sites[].rangeKm).
function fullKm(site) { return site && site.rangeKm > 0 ? site.rangeKm : NOMINAL_KM; }

// An entry's reach in force: its reachKm, at most the full range.
function reachOf(entry, site) {
    var full = fullKm(site);
    return entry && entry.reachKm > 0 ? Math.min(entry.reachKm, full) : full;
}

function valid(set) {
    return !!set && Array.isArray(set.sites) && set.sites.length > 0
        && set.sites.every(function (s) { return !!s && typeof s.id === "string"; });
}

// A height of the `height` rule (S30): 500 to 12,000 m in steps of 500.
function validHeight(m) { return typeof m === "number" && m >= 500 && m <= 12000 && m % 500 === 0; }

// set_mosaic for `set`, leaving out radars `sites` (hello.sites) no longer
// lists; a full reach is sent as none. A height set (S30) sends its height
// and what it is above, and goes as lowest beam to an engine whose
// hello.mosaic `rules` has no height.
function command(set, sites, rules) {
    var out = [];
    for (var i = 0; i < set.sites.length; i++) {
        var s = set.sites[i];
        var site = sites.find(function (x) { return x.id === s.id; });
        if (!site) continue;
        var r = reachOf(s, site);
        out.push(r < fullKm(site) ? { id: s.id, reachKm: Math.round(r) } : { id: s.id });
    }
    var rule = set.rule || "lowest";
    if (rule === "height" && rules && !rules.some(function (x) { return x.id === "height"; })) rule = "lowest";
    var c = { type: "set_mosaic", sites: out, rule: rule };
    if (rule === "height") {
        c.heightM = validHeight(set.heightM) ? set.heightM : 2000;
        c.above = set.above === "ground" ? "ground" : "sea";
    }
    return c;
}

// A rule's display name from hello.mosaic.rules.
function ruleName(rules, id) {
    var r = (rules || []).find(function (x) { return x.id === id; });
    return r ? r.name : id || "";
}

// A height's name, as the engine names its frames (S30).
function heightName(m, above) {
    return "Height " + m / 1000 + " km" + (above === "ground" ? " above ground" : "");
}

// A set's name: its rule's, or for a height set its height's.
function setName(rules, set) {
    return set && set.rule === "height" ? heightName(validHeight(set.heightM) ? set.heightM : 2000, set.above) : ruleName(rules, set ? set.rule : "");
}
