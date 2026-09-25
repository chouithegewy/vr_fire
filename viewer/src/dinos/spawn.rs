//! Deterministic herds: every player gets the same herds in the same 1 km cells.

use super::species::Species;
use bevy::math::DVec2;

pub const CELL_M: f64 = 1000.0;
const SEED: u64 = 0x0d15_ea5e_f00d_cafe;

/// Small deterministic generator (splitmix64); the same on every platform.
#[derive(Clone, Copy, Debug)]
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f64()
    }
}

pub fn cell_of(p: DVec2) -> (i64, i64) {
    ((p.x / CELL_M).floor() as i64, (p.y / CELL_M).floor() as i64)
}

pub fn cell_rng(cx: i64, cy: i64) -> Rng {
    let mut r = Rng(SEED ^ (cx as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (cy as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f));
    r.next_u64();
    r
}

#[derive(Clone, Debug, PartialEq)]
pub struct Herd {
    pub species: Species,
    /// EPSG:5070 metres.
    pub centre: DVec2,
    pub members: Vec<DVec2>,
}

/// The herd living in a cell, if any (about one cell in six).
pub fn herd(cx: i64, cy: i64) -> Option<Herd> {
    let mut r = cell_rng(cx, cy);
    if r.f64() >= 1.0 / 6.0 {
        return None;
    }
    // Herbivores common, T. rex rare.
    let pick = r.f64() * 100.0;
    let (species, lo, hi, spread) = match pick {
        x if x < 30.0 => (Species::Stego, 2, 5, 40.0),
        x if x < 55.0 => (Species::Trike, 2, 4, 35.0),
        x if x < 70.0 => (Species::Brachio, 1, 3, 70.0),
        x if x < 90.0 => (Species::Raptor, 3, 6, 15.0),
        _ => (Species::TRex, 1, 1, 0.0),
    };
    let n = lo + (r.next_u64() % (hi - lo + 1) as u64) as usize;
    let base = DVec2::new(cx as f64, cy as f64) * CELL_M;
    let centre = base + DVec2::new(r.range(200.0, 800.0), r.range(200.0, 800.0));
    let members = (0..n)
        .map(|_| {
            let a = r.range(0.0, std::f64::consts::TAU);
            let d = spread * r.f64().sqrt();
            centre + DVec2::new(a.cos(), a.sin()) * d
        })
        .collect();
    Some(Herd { species, centre, members })
}

/// Cells whose centre is within `radius` of `p`, nearest first.
pub fn cells_near(p: DVec2, radius: f64) -> Vec<(i64, i64)> {
    let (lo, hi) = (cell_of(p - DVec2::splat(radius)), cell_of(p + DVec2::splat(radius)));
    let mut cells: Vec<_> = (lo.0..=hi.0)
        .flat_map(|cx| (lo.1..=hi.1).map(move |cy| (cx, cy)))
        .filter(|&(cx, cy)| cell_centre(cx, cy).distance(p) <= radius)
        .collect();
    cells.sort_by(|a, b| cell_centre(a.0, a.1).distance(p).total_cmp(&cell_centre(b.0, b.1).distance(p)));
    cells
}

pub fn cell_centre(cx: i64, cy: i64) -> DVec2 {
    (DVec2::new(cx as f64, cy as f64) + 0.5) * CELL_M
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn herds_are_deterministic_per_cell() {
        for cx in -2_050_000 / 1000..-2_050_000 / 1000 + 40 {
            for cy in 1_900..1_940 {
                assert_eq!(herd(cx, cy), herd(cx, cy));
            }
        }
    }

    #[test]
    fn about_one_cell_in_six_has_a_herd_with_every_species_present() {
        let mut n = 0;
        let mut seen = std::collections::HashSet::new();
        for cx in -2100..-2000 {
            for cy in 1800..1900 {
                if let Some(h) = herd(cx, cy) {
                    n += 1;
                    seen.insert(h.species);
                    let (lo, hi) = match h.species {
                        Species::Stego => (2, 5),
                        Species::Trike => (2, 4),
                        Species::Brachio => (1, 3),
                        Species::Raptor => (3, 6),
                        Species::TRex => (1, 1),
                    };
                    assert!((lo..=hi).contains(&h.members.len()));
                    // Herds stay inside their cell.
                    for m in &h.members {
                        assert_eq!(cell_of(*m), (cx, cy));
                    }
                }
            }
        }
        let frac = n as f64 / 10_000.0;
        assert!((0.14..0.19).contains(&frac), "{frac}");
        assert_eq!(seen.len(), 5);
    }

    #[test]
    fn nearby_cells_are_sorted_nearest_first() {
        let p = DVec2::new(-2_050_300.0, 1_900_700.0);
        let cells = cells_near(p, 1500.0);
        assert!(cells.len() >= 5);
        assert_eq!(cells[0], cell_of(p));
        let d: Vec<f64> = cells.iter().map(|c| cell_centre(c.0, c.1).distance(p)).collect();
        assert!(d.windows(2).all(|w| w[0] <= w[1]));
        assert!(d.iter().all(|&x| x <= 1500.0));
    }
}
