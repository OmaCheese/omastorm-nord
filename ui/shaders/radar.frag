#version 440
layout(location = 0) in vec2 qt_TexCoord0;
layout(location = 0) out vec4 fragColor;
// The map frame is defined once, here: Web Mercator over the whole network,
// shared with the tile layer and the overlay. The camera arrives as the view
// centre's offset from the radar site in Mercator units (the unit square is
// the world) so that a pixel's position relative to the site keeps float
// precision; the site's latitude turns that offset into ground distance and
// azimuth on a sphere.
layout(std140, binding = 0) uniform buf {
    mat4 qt_Matrix;
    float qt_Opacity;
    vec2 viewport;
    vec2 centerOffset;
    float unitsPerPixel;
    float siteLatDeg;
    int treatment;
    int bands;
    // Sweep geometry from frame state (docs/protocol.md).
    int rays;
    int gates;
    float firstGateM;
    float gateSpacingM;
    float elevationDeg;
    // Weak-return floor: measured codes 2..weakBelow-1 draw nothing (the
    // legend names the hidden dBZ). 0 draws every measured return.
    int weakBelow;
    // Frame kind (docs/protocol.md): 0 polar, 1 grid. A grid is the
    // engine's Web Mercator reprojection of a composite, placed by its
    // north-west corner's offset from the site and its size, both in
    // Mercator units, and sized in texels. No azimuth lookup is read for it.
    int kind;
    vec2 gridOrigin;
    vec2 gridSize;
    vec2 gridTexels;
    // 1 when a grid frame's texture is its one-channel code texture (the
    // raw code in R): the texel is rebuilt as class + 1 from `classes`, no
    // status bits, and the code in B, exactly the RGBA grid texel.
    int useCodes;
    // S29: 1 when the frame is a height slice (`CAPPI`): its no-data texels
    // (code 1, G bit 4) are "no radar at this height", drawn as a faint
    // diagonal hatch in `hatchColor` (straight alpha), never as nothing,
    // which reads as no rain. 0 for every other frame.
    int hatchNodata;
    // S29: the reach, a ground distance in metres from the site past which a
    // polar frame draws nothing; 0 draws the whole sweep.
    float reachM;
    vec4 hatchColor;
    // S30: the chosen radars of a My mosaic height frame, up to 12, each its
    // latitude and its longitude less the site's (radians) and its reach
    // (metres). A grid frame's no-data texel is hatched only inside one of
    // them; outside every reach no radar was chosen, and it draws nothing.
    // 0 circles: hatched everywhere, as before.
    int circleCount;
    vec4 circle0; vec4 circle1; vec4 circle2; vec4 circle3; vec4 circle4; vec4 circle5;
    vec4 circle6; vec4 circle7; vec4 circle8; vec4 circle9; vec4 circle10; vec4 circle11;
    // S24a: 1 when the frame is a storm height (`ETOP`): a texel with G bit
    // 8 is "at least" this high (the highest beam reaching it still holds
    // 18 dBZ), drawn in its colour under the same faint hatch. 0 otherwise.
    int hatchAtLeast;
};
// The sweep: one row per radial in ascending azimuth, one texel per gate.
// For a grid frame, the Web Mercator texture instead, row 0 north.
// R is palette class + 1 (0 draws nothing), G holds status bits (1 folded,
// 2 below threshold). Nearest sampling, no mipmaps.
layout(binding = 1) uniform sampler2D sweep;
// The frame's palette from socket state as a bands x 1 strip, sampled at texel
// centers so the radar and the legend share one source of color.
layout(binding = 2) uniform sampler2D swatches;
// 3600 x 1: entry i covers azimuth i / 10 degrees and names the row nearest
// its center as a little-endian 16-bit value in R and G.
layout(binding = 3) uniform sampler2D azimuthLut;
// With useCodes: 256 x 1, entry c holds code c's class + 1 in R (0 draws
// nothing), built by RadarMap from the frame's bounds, scale and offset.
layout(binding = 4) uniform sampler2D classes;
// Gates sit at slant range along the beam; the map is ground distance. The
// two are related on the 4/3 effective-radius earth exactly as pyart's
// antenna_to_cartesian places gates, which is how the golden reference is built.
// The sphere that turns longitude and latitude into ground distance, and the
// 4/3 effective radius the beam bends over.
const float R_M = 6371000.0;
const float EARTH_M = R_M * 4.0 / 3.0;
const float PI = 3.14159265358979;
// GLSL ES 100 (used by Qt on Wayland/EGL) cannot initialize constant arrays.
// Spell out the same 3x3 density mask so every packaged target compiles.
int densityAt(int slot) {
    if (slot == 0) return 0;
    if (slot == 1) return 7;
    if (slot == 2) return 3;
    if (slot == 3) return 6;
    if (slot == 4) return 4;
    if (slot == 5) return 8;
    if (slot == 6) return 2;
    if (slot == 7) return 5;
    return 1;
}
// Hyperbolics spelled with exp so every GLSL target qsb emits has them. For
// small arguments exp(x) - exp(-x) cancels to a few significant bits, so the
// small terms below take their series instead; the rendering test replays
// the same polynomials, and the GPU and CPU then agree past the row epsilon.
float cosh_(float x) { return 0.5 * (exp(x) + exp(-x)); }
float sinh_(float x) {
    if (abs(x) >= 1.0) return 0.5 * (exp(x) - exp(-x));
    float x2 = x * x;
    return x * (1.0 + x2 * (1.0 / 6.0 + x2 * (1.0 / 120.0 + x2 * (1.0 / 5040.0 + x2 / 362880.0))));
}
float sin_(float x) {
    if (abs(x) >= 1.0) return sin(x);
    float x2 = x * x;
    return x * (1.0 - x2 * (1.0 / 6.0 - x2 * (1.0 / 120.0 - x2 * (1.0 / 5040.0 - x2 / 362880.0))));
}
// Some GPU atan implementations move a bearing across a 0.1-degree LUT
// boundary. Reduce to |t| <= tan(pi/8); the alternating series' next term
// is < 1.9e-8 radians. This keeps the existing rendering-test tolerance.
float bearingAtan(float y, float x) {
    float ax = abs(x), ay = abs(y);
    float t = min(ax, ay) / max(max(ax, ay), 1e-30);
    bool reduce = t > 0.414213562373095;
    if (reduce) t = (t - 1.0) / (t + 1.0);
    float t2 = t*t;
    float a = t*(1.0-t2*(1.0/3.0-t2*(1.0/5.0-t2*(1.0/7.0-t2*(1.0/9.0-t2*(1.0/11.0-t2*(1.0/13.0-t2/15.0)))))));
    if (reduce) a += PI*.25;
    if (ay > ax) a = PI*.5-a;
    if (x < 0.0) a = PI-a;
    return y < 0.0 ? -a : a;
}
vec4 shade(vec4 code, vec2 pixel);
// S30: whether the point at latitude `lat` and longitude `dLon` from the
// site (radians) lies within circle `c` (great circle on the 6,371 km sphere).
bool within(vec4 c, float lat, float dLon) {
    float sdLat = sin((lat - c.x) * .5), sdLon = sin((dLon - c.y) * .5);
    float h = clamp(sdLat * sdLat + cos(lat) * cos(c.x) * sdLon * sdLon, 0.0, 1.0);
    return 2.0 * R_M * atan(sqrt(h), sqrt(max(0.0, 1.0 - h))) <= c.z;
}
// Whether the cell at Mercator offset `d` from the site is within any
// chosen radar's reach (spelled out: no uniform arrays in every target).
bool withinAny(vec2 d) {
    float lat0 = radians(siteLatDeg);
    float lat = atan(sinh_(log(tan(lat0) + 1.0 / cos(lat0)) - d.y * 2.0 * PI));
    float dLon = d.x * 2.0 * PI;
    if (circleCount > 0 && within(circle0, lat, dLon)) return true;
    if (circleCount > 1 && within(circle1, lat, dLon)) return true;
    if (circleCount > 2 && within(circle2, lat, dLon)) return true;
    if (circleCount > 3 && within(circle3, lat, dLon)) return true;
    if (circleCount > 4 && within(circle4, lat, dLon)) return true;
    if (circleCount > 5 && within(circle5, lat, dLon)) return true;
    if (circleCount > 6 && within(circle6, lat, dLon)) return true;
    if (circleCount > 7 && within(circle7, lat, dLon)) return true;
    if (circleCount > 8 && within(circle8, lat, dLon)) return true;
    if (circleCount > 9 && within(circle9, lat, dLon)) return true;
    if (circleCount > 10 && within(circle10, lat, dLon)) return true;
    if (circleCount > 11 && within(circle11, lat, dLon)) return true;
    return false;
}
void main() {
    // Every treatment paints 3 px screen cells; each cell samples the gate
    // under its center, so the lookup below runs once per cell, not per texel.
    vec2 pixel = qt_TexCoord0 * viewport;
    vec2 samplePixel = floor(pixel / 3.0) * 3.0 + 1.5;
    if (kind == 1) {
        // Grid lookup rule (docs/protocol.md): the cell centre's place in
        // the texture's Mercator rectangle, row 0 north. Outside the
        // rectangle draws nothing; inside, the nearest texel.
        vec2 g = (centerOffset + (samplePixel - viewport * .5) * unitsPerPixel - gridOrigin) / gridSize;
        if (gridTexels.x < 1.0 || gridTexels.y < 1.0
            || g.x < 0.0 || g.y < 0.0 || g.x >= 1.0 || g.y >= 1.0) { fragColor=vec4(0); return; }
        vec4 texel = texture(sweep, (floor(g * gridTexels) + .5) / gridTexels);
        if (useCodes == 1) {
            // Code texture: the raw code in R (a grayscale PNG reads it in
            // R, G and B alike); class + 1 from the lookup strip.
            // Code 1 keeps the grid texel's G bit 4 (no data), so a height
            // slice's holes are hatched (S30).
            float raw = floor(texel.r * 255.0 + .5);
            texel = vec4(texture(classes, vec2((raw + .5) / 256.0, .5)).r, raw == 1.0 ? 4.0 / 255.0 : 0.0, raw / 255.0, 1.0);
        }
        // S30: a no-data texel outside every chosen radar's reach is not
        // "no radar at this height" but no radar chosen: nothing.
        if (hatchNodata == 1 && circleCount > 0 && texel.r == 0.0 && floor(texel.g * 255.0 + .5) == 4.0
            && !withinAny(centerOffset + (samplePixel - viewport * .5) * unitsPerPixel)) { fragColor = vec4(0); return; }
        fragColor = shade(texel, pixel);
        return;
    }
    if (gates <= 0 || rays <= 0) { fragColor=vec4(0); return; }
    // The cell's Mercator offset from the site: x east, y south (tile rows
    // grow southward). In radians of longitude and of isometric latitude.
    vec2 d = centerOffset + (samplePixel - viewport * .5) * unitsPerPixel;
    float dLon = d.x * 2.0 * PI;
    float dPsi = -d.y * 2.0 * PI;
    // Latitude difference without subtracting two large latitudes:
    // atan(a) - atan(b) = atan((a - b) / (1 + a b)) with a = sinh(psi),
    // b = sinh(psi0) = tan(lat0), and sinh(psi) - sinh(psi0) factored.
    float lat0 = radians(siteLatDeg);
    float psi0 = log(tan(lat0) + 1.0 / cos(lat0));
    float dLat = bearingAtan(2.0 * cosh_(psi0 + dPsi * .5) * sinh_(dPsi * .5),
                            1.0 + tan(lat0) * sinh_(psi0 + dPsi));
    // Far from the site subtraction is well-conditioned and avoids the
    // quotient identity's lost quadrant in the opposite hemisphere.
    if (abs(dPsi) >= 1.0) dLat = atan(sinh_(psi0 + dPsi)) - lat0;
    float lat = lat0 + dLat;
    // Great-circle distance (haversine) and initial bearing from the site,
    // both written in the small differences so nearby cells stay exact.
    float sdLat = sin_(dLat * .5), sdLon = sin_(dLon * .5);
    float h = clamp(sdLat * sdLat + cos(lat0) * cos(lat) * sdLon * sdLon, 0.0, 1.0);
    float groundM = 2.0 * R_M * atan(sqrt(h), sqrt(max(0.0, 1.0 - h)));
    if (reachM > 0.0 && groundM > reachM) { fragColor=vec4(0); return; }
    float azimuth = degrees(bearingAtan(sin_(dLon) * cos(lat),
                                 sin_(dLat) + sin(lat0) * cos(lat) * 2.0 * sdLon * sdLon));
    if (azimuth < 0.0) azimuth += 360.0;
    // Ground distance to slant range, closed form: r = R sin(s/R) / cos(e + s/R).
    float arc = groundM / EARTH_M;
    if (radians(elevationDeg) + arc >= PI * .5) { fragColor=vec4(0); return; }
    float slantM = EARTH_M * sin(arc) / cos(radians(elevationDeg) + arc);
    float gate = (slantM - firstGateM) / gateSpacingM;
    // Nearest gate. More than half a gate before the first or past the last
    // is outside the sweep: nothing to draw, never a weak return.
    if (gate < -0.5 || gate >= float(gates) - 0.5) { fragColor=vec4(0); return; }
    // Azimuth clockwise from north, in tenths of a degree, names the row.
    float entry = clamp(floor(azimuth * 10.0), 0.0, 3599.0);
    vec4 lut = texture(azimuthLut, vec2((entry + .5) / 3600.0, .5));
    float row = floor(lut.r * 255.0 + .5) + 256.0 * floor(lut.g * 255.0 + .5);
    vec2 uv = vec2((floor(gate + .5) + .5) / float(gates), (row + .5) / float(rays));
    fragColor = shade(texture(sweep, uv), pixel);
}
// The palette, treatments, folded marker, and weak-return floor, shared by
// both kinds: `code` is the texel under the cell, `pixel` the fragment.
vec4 shade(vec4 code, vec2 pixel) {
    // The raw moment byte in B decides the floor, so a floor can sit inside a
    // palette band; folded and below-threshold codes (0, 1) are never weak.
    int raw = int(round(code.b * 255.0));
    if (weakBelow > 0 && raw >= 2 && raw < weakBelow) return vec4(0);
    int value = int(round(code.r * 255.0));
    int status = int(round(code.g * 255.0));
    vec2 phase = mod(pixel,3.0);
    if (value == 0) {
        // Folded is a two-tone X in every treatment, never an intensity swatch.
        // Its opaque dark backing keeps the marker legible in light themes too.
        // Bit 1 without bitwise operators, which the legacy GLSL targets lack.
        if (status - 2 * (status / 2) == 1) {
            ivec2 p = ivec2(floor(phase));
            bool cross = p.x == p.y || p.x + p.y == 2;
            vec3 color = cross ? vec3(245) : vec3(24);
            return vec4(color/255.0,1.0)*qt_Opacity;
        }
        // No radar at this height (S29): one pixel in six along the
        // diagonals, so the hole reads as a hole and not as clear sky.
        if (hatchNodata == 1 && status == 4 && mod(floor(pixel.x) + floor(pixel.y), 6.0) < 1.0)
            return vec4(hatchColor.rgb * hatchColor.a, hatchColor.a) * qt_Opacity;
        return vec4(0);
    }
    int b=value-1;
    if (b >= bands) return vec4(0);
    // A storm height that is only a lower bound (S24a, G bit 8, spelled
    // without bitwise operators): its colour under the faint hatch.
    if (hatchAtLeast == 1 && status / 8 - 2 * (status / 16) == 1 && mod(floor(pixel.x) + floor(pixel.y), 6.0) < 1.0)
        return vec4(hatchColor.rgb * hatchColor.a, hatchColor.a) * qt_Opacity;
    // Treatments grade coverage by quartile of the palette, whatever its length.
    int group=(b*4)/bands;
    float alpha=1.0;
    if (treatment == 1) {
        int count=group==0 ? 2 : group==1 ? 4 : group==2 ? 7 : 9;
        int slot=int(floor(phase.y))*3+int(floor(phase.x));
        alpha=densityAt(slot)<count ? 1.0 : 0.0;
    } else if (treatment == 2) {
        float side=group==0 ? 1.75 : group==1 ? 2.0 : group==2 ? 2.25 : 2.5;
        vec2 coverage=clamp(vec2(side*.5+.5)-abs(phase-1.5),0.0,1.0);
        alpha=coverage.x*coverage.y;
    }
    vec3 color=texture(swatches, vec2((float(b)+.5)/float(bands), .5)).rgb;
    return vec4(color*alpha,alpha)*qt_Opacity;
}
