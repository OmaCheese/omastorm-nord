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

// set_mosaic for `set`, leaving out radars `sites` (hello.sites) no longer
// lists; a full reach is sent as none.
function command(set, sites) {
    var out = [];
    for (var i = 0; i < set.sites.length; i++) {
        var s = set.sites[i];
        var site = sites.find(function (x) { return x.id === s.id; });
        if (!site) continue;
        var r = reachOf(s, site);
        out.push(r < fullKm(site) ? { id: s.id, reachKm: Math.round(r) } : { id: s.id });
    }
    return { type: "set_mosaic", sites: out, rule: set.rule || "lowest" };
}

// A rule's display name from hello.mosaic.rules.
function ruleName(rules, id) {
    var r = (rules || []).find(function (x) { return x.id === id; });
    return r ? r.name : id || "";
}
