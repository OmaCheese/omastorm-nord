//! Terrain (S30, `docs/protocol.md` "Terrain"): the mean height of the
//! ground per Web Mercator texel of 2,000 m (the composites' and My
//! mosaic's lattice) over the box of every Nordic radar's reach, in steps
//! of 10 m, for heights "above ground". Made once, offline, by
//! `scripts/terrain-grid.py` from the Terrarium tiles the blockage tables
//! use (`data/README.md` names the sources); embedded, and inflated on
//! first use (about 5 MB). Outside the box the ground is at sea level.

use crate::composite::{MERCATOR_R, PIXEL_M, mercator_y};
use std::io::Read;
use std::sync::LazyLock;

const FILE: &[u8] = include_bytes!("../data/terrain-nordic-2km.bin");
const MAGIC: &[u8; 8] = b"OMTERR1\0";
/// The sphere the lookup rule measures ground distance and bearing on.
const SPHERE_M: f64 = 6_371_000.0;

/// The grid: one byte per texel, row 0 north.
pub struct Grid {
    /// Lattice index of the west edge (x = `col0` × 2,000 m) and of the
    /// north edge (y = `north0` × 2,000 m).
    pub col0: i64,
    pub north0: i64,
    pub width: usize,
    pub height: usize,
    step_m: f64,
    steps: Vec<u8>,
    max_m: f64,
}

impl Grid {
    /// The file `scripts/terrain-grid.py` writes: the header, then zlib of
    /// the rows, each delta-coded from the west.
    pub fn parse(bytes: &[u8]) -> Result<Grid, String> {
        let head = bytes
            .get(..32)
            .filter(|h| h.starts_with(MAGIC))
            .ok_or("not a terrain grid")?;
        let word = |at: usize| u32::from_le_bytes(head[at..at + 4].try_into().unwrap());
        let (col0, north0) = (i64::from(word(8) as i32), i64::from(word(12) as i32));
        let (width, height) = (word(16) as usize, word(20) as usize);
        if f64::from(word(24)) != PIXEL_M {
            return Err(format!("a {} m texel, not {PIXEL_M}", word(24)));
        }
        let step_m = f64::from(word(28)) / 10.0;
        let mut steps = Vec::with_capacity(width * height);
        flate2::read::ZlibDecoder::new(&bytes[32..])
            .read_to_end(&mut steps)
            .map_err(|e| e.to_string())?;
        if steps.len() != width * height {
            return Err(format!(
                "{} bytes for {width} × {height} texels",
                steps.len()
            ));
        }
        for row in steps.chunks_exact_mut(width.max(1)) {
            for c in 1..row.len() {
                row[c] = row[c].wrapping_add(row[c - 1]);
            }
        }
        let max_m = f64::from(steps.iter().copied().max().unwrap_or(0)) * step_m;
        Ok(Grid {
            col0,
            north0,
            width,
            height,
            step_m,
            steps,
            max_m,
        })
    }

    /// The texel whose west edge is lattice column `col` and whose north
    /// edge is lattice row `north`, metres; 0 outside the grid.
    pub fn texel(&self, col: i64, north: i64) -> f64 {
        let (c, r) = (col - self.col0, self.north0 - north);
        if c < 0 || r < 0 || c >= self.width as i64 || r >= self.height as i64 {
            return 0.0;
        }
        f64::from(self.steps[r as usize * self.width + c as usize]) * self.step_m
    }

    /// The texel holding (`lat`, `lon`), metres.
    pub fn at(&self, lat: f64, lon: f64) -> f64 {
        let (x, y) = (MERCATOR_R * lon.to_radians(), mercator_y(lat));
        self.texel(
            (x / PIXEL_M).floor() as i64,
            (y / PIXEL_M).floor() as i64 + 1,
        )
    }

    /// The highest texel, metres.
    pub fn max_m(&self) -> f64 {
        self.max_m
    }
}

static GRID: LazyLock<Result<Grid, String>> = LazyLock::new(|| {
    let grid = Grid::parse(FILE);
    if let Err(e) = &grid {
        eprintln!("Terrain: {e}; every height above ground is above sea level");
    }
    grid
});

/// The embedded grid; `None` only if the file does not parse.
pub fn grid() -> Option<&'static Grid> {
    GRID.as_ref().ok()
}

/// The ground under (`lat`, `lon`), metres above sea level; 0 off the grid.
pub fn at(lat: f64, lon: f64) -> f64 {
    grid().map_or(0.0, |g| g.at(lat, lon))
}

/// The highest ground in the grid, metres.
pub fn max_m() -> f64 {
    grid().map_or(0.0, Grid::max_m)
}

/// The point `d` metres from (`lat`, `lon`) along the initial bearing
/// `bearing` (degrees), on the lookup rule's 6,371 km sphere.
pub fn destination(lat: f64, lon: f64, bearing: f64, d: f64) -> (f64, f64) {
    let (phi, lambda, theta, delta) = (
        lat.to_radians(),
        lon.to_radians(),
        bearing.to_radians(),
        d / SPHERE_M,
    );
    let phi2 = (phi.sin() * delta.cos() + phi.cos() * delta.sin() * theta.cos()).asin();
    let lambda2 = lambda
        + (theta.sin() * delta.sin() * phi.cos()).atan2(delta.cos() - phi.sin() * phi2.sin());
    (phi2.to_degrees(), lambda2.to_degrees())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn the_grid_parses_and_knows_the_nordic_ground() {
        let g = grid().expect("engine/data/terrain-nordic-2km.bin parses");
        assert!(g.width > 1000 && g.height > 1000, "{} × {}", g.width, g.height);
        // Every radar of the table lies well inside the box.
        for s in crate::providers::table().sites {
            if s.kind == crate::protocol::SiteKind::Polar {
                let (x, y) = (MERCATOR_R * s.lon.to_radians(), mercator_y(s.lat));
                let c = (x / PIXEL_M).floor() as i64 - g.col0;
                let r = g.north0 - ((y / PIXEL_M).floor() as i64 + 1);
                assert!(
                    c > 100 && r > 100 && c < g.width as i64 - 100 && r < g.height as i64 - 100,
                    "{}",
                    s.id
                );
            }
        }
        // Norway's highest texels (Jotunheimen) are 1.5-2.5 km; the sea is 0.
        assert!((1500.0..2550.0).contains(&g.max_m()), "{}", g.max_m());
        assert_eq!(at(57.8, 9.5), 0.0, "the Skagerrak");
        assert_eq!(at(55.5, 18.0), 0.0, "the Baltic");
        let vidda = at(60.1, 7.5);
        assert!((900.0..1600.0).contains(&vidda), "Hardangervidda {vidda}");
        let jotun = at(61.6, 8.3);
        assert!(jotun > 1300.0, "Jotunheimen {jotun}");
        let vara = at(58.256, 12.826);
        assert!((50.0..300.0).contains(&vara), "Vara {vara}");
        // Off the grid: sea level.
        assert_eq!(at(40.0, -20.0), 0.0);
    }

    #[test]
    fn rows_are_delta_coded_from_the_west() {
        let steps: [[u8; 4]; 2] = [[0, 10, 5, 255], [3, 3, 250, 1]];
        let mut delta = Vec::new();
        for row in steps {
            delta.push(row[0]);
            for c in 1..4 {
                delta.push(row[c].wrapping_sub(row[c - 1]));
            }
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        z.write_all(&delta).unwrap();
        let mut file = MAGIC.to_vec();
        for word in [100i32 as u32, 50i32 as u32, 4, 2, 2000, 100] {
            file.extend(word.to_le_bytes());
        }
        file.extend(z.finish().unwrap());
        let g = Grid::parse(&file).unwrap();
        assert_eq!((g.col0, g.north0, g.width, g.height), (100, 50, 4, 2));
        assert_eq!(g.texel(100, 50), 0.0);
        assert_eq!(g.texel(103, 50), 2550.0);
        assert_eq!(g.texel(102, 49), 2500.0);
        assert_eq!(g.texel(99, 50), 0.0, "west of the grid");
        assert_eq!(g.texel(100, 48), 0.0, "south of the grid");
        assert_eq!(g.max_m(), 2550.0);
        // A point: the texel whose Mercator square holds it.
        let lon = ((101.5 * PIXEL_M) / MERCATOR_R).to_degrees();
        let lat = crate::composite::mercator_lat(48.5 * PIXEL_M);
        assert_eq!(g.at(lat, lon), 30.0, "column 101, the row whose north edge is 49");
        assert!(Grid::parse(b"not a grid").is_err());
    }
}
