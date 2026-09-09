//! Neighbour-first point -> A5 cell locator for raster scans.
//!
//! Consecutive pixels of a raster row are a few metres apart, so the cell of
//! pixel `(r, c)` is almost always the cell of `(r, c-1)`, the cell of
//! `(r-1, c)`, or an edge/vertex neighbour of one of those. Testing those
//! candidates with pre-computed pentagons is one dodecahedron projection
//! plus a handful of cross products per pixel; the general
//! `a5::core::cell::spherical_to_cell` search (an estimate, then up to ~26
//! spiral samples, each rebuilding a pentagon from a Hilbert decode) is
//! reached only when every candidate fails, which happens at tile / stripe
//! starts and after nodata gaps.
//!
//! At A5 resolution 18 on 10 m pixels roughly one pixel in five leaves the
//! previous cell, so the cost of the miss path dominates; this locator cuts
//! the per-pixel lookup by about 2x there and by 3x at resolutions 14-16
//! (see `examples/index_bench.rs`).
//!
//! `a5::core::*` paths are `#[doc(hidden)]` upstream but stable in practice
//! (a5R depends on the same ones).

use a5::coordinate_systems::{Face, Spherical};
use a5::core::cell::{get_pentagon, spherical_to_cell};
use a5::core::serialization::{deserialize, FIRST_HILBERT_RESOLUTION};
use a5::core::utils::OriginId;
use a5::projections::dodecahedron::DodecahedronProjection;
use a5::traversal::global_neighbors::get_global_cell_neighbors;
use ahash::AHashMap;

/// Sentinel for "no cell" (lookup failed, or no hint available).
pub const NO_CELL: u64 = u64::MAX;

/// Cached geometry cap: cleared when exceeded. Entries are created lazily
/// for tested cells only, so a 64-row stripe at res 18 holds ~15k.
const GEOM_CACHE_MAX: usize = 65_536;

/// Lon/lat in degrees -> A5's internal spherical frame (rotated authalic
/// sphere). This is the projection `a5::lonlat_to_cell` performs internally;
/// doing it once per point lets the cached pentagon tests and the search
/// fallback share it.
#[inline]
pub fn a5_spherical(lon_deg: f64, lat_deg: f64) -> Spherical {
    a5::core::coordinate_transforms::from_lon_lat(a5::LonLat::new(lon_deg, lat_deg))
}

/// Pre-computed cell geometry in its origin's face frame.
struct CellGeom {
    origin: OriginId,
    /// Pentagon vertices (CCW), 3 for res 1 quintants, 5 otherwise.
    verts: [(f64, f64); 5],
    n_verts: usize,
    /// Edge + vertex neighbours, built on first miss from this cell and kept
    /// in most-recently-hit order: a row scan that exits a cell eastward
    /// tends to exit it eastward again on the next row.
    nbrs: Option<Vec<u64>>,
}

impl CellGeom {
    fn build(id: u64) -> Option<Self> {
        let cell = deserialize(id).ok()?;
        let pent = get_pentagon(&cell).ok()?;
        let vv = pent.get_vertices_vec();
        if vv.len() < 3 || vv.len() > 5 {
            return None;
        }
        let mut verts = [(0.0, 0.0); 5];
        for (i, v) in vv.iter().enumerate() {
            verts[i] = (v.x(), v.y());
        }
        Some(Self { origin: cell.origin_id, verts, n_verts: vv.len(), nbrs: None })
    }

    /// Same half-plane test as `PentagonShape::contains_point` (a point on
    /// an edge counts as inside), without its per-call winding check.
    #[inline]
    fn contains(&self, p: Face) -> bool {
        let (px, py) = (p.x(), p.y());
        let n = self.n_verts;
        for i in 0..n {
            let (x1, y1) = self.verts[i];
            let (x2, y2) = self.verts[if i + 1 == n { 0 } else { i + 1 }];
            let cross = (x1 - x2) * (py - y1) - (y1 - y2) * (px - x1);
            if cross < 0.0 {
                return false;
            }
        }
        true
    }
}

/// Per-point memo of the dodecahedron projection: candidates almost always
/// share an origin, so the point is projected once per origin per pixel.
struct ProjMemo {
    origin: OriginId,
    face: Face,
    valid: bool,
}

impl ProjMemo {
    #[inline]
    fn new() -> Self {
        Self { origin: 0, face: Face::new(0.0, 0.0), valid: false }
    }

    #[inline]
    fn project(&mut self, p: Spherical, origin: OriginId) -> Option<Face> {
        if !self.valid || self.origin != origin {
            let d = DodecahedronProjection::get_thread_local();
            match d.forward(p, origin) {
                Ok(f) => {
                    self.origin = origin;
                    self.face = f;
                    self.valid = true;
                }
                Err(_) => return None,
            }
        }
        Some(self.face)
    }
}

/// Neighbour-first point -> cell locator. One per scan (tile stripe); holds
/// the previous hit and a cache of tested cells' geometry.
pub struct CellLocator {
    resolution: i32,
    /// Below the first Hilbert resolution `spherical_to_cell` is an exact
    /// O(1) estimate, so the candidate tests are skipped.
    direct: bool,
    geoms: AHashMap<u64, CellGeom>,
    last: u64,
}

impl CellLocator {
    pub fn new(resolution: i32) -> Self {
        Self {
            resolution,
            direct: resolution < FIRST_HILBERT_RESOLUTION,
            geoms: AHashMap::with_capacity(1024),
            last: NO_CELL,
        }
    }

    #[inline]
    fn geom(&mut self, id: u64) -> Option<&mut CellGeom> {
        if !self.geoms.contains_key(&id) {
            if self.geoms.len() >= GEOM_CACHE_MAX {
                self.geoms.clear();
            }
            let g = CellGeom::build(id)?;
            self.geoms.insert(id, g);
        }
        self.geoms.get_mut(&id)
    }

    #[inline]
    fn test(&mut self, id: u64, p: Spherical, memo: &mut ProjMemo) -> bool {
        match self.geom(id) {
            Some(g) => match memo.project(p, g.origin) {
                Some(f) => g.contains(f),
                None => false,
            },
            None => false,
        }
    }

    /// Cell containing `p` at this locator's resolution, or `NO_CELL`.
    /// `hint` is an extra candidate (the cell of the pixel above in a row
    /// scan, or the corner cell of the pixel for overlay sub-points);
    /// pass `NO_CELL` when there is none.
    pub fn locate(&mut self, p: Spherical, hint: u64) -> u64 {
        if self.direct {
            return spherical_to_cell(p, self.resolution).unwrap_or(NO_CELL);
        }
        let mut memo = ProjMemo::new();
        let last = self.last;
        if last != NO_CELL && self.test(last, p, &mut memo) {
            return last;
        }
        if hint != NO_CELL && hint != last && self.test(hint, p, &mut memo) {
            self.last = hint;
            return hint;
        }
        if last != NO_CELL {
            if let Some(hit) = self.test_neighbours(last, p, hint, &mut memo) {
                self.last = hit;
                return hit;
            }
        }
        match spherical_to_cell(p, self.resolution) {
            Ok(id) => {
                self.last = id;
                id
            }
            Err(_) => NO_CELL,
        }
    }

    /// Test the neighbours of `from` in most-recently-hit order; a hit is
    /// moved to the front of the list.
    fn test_neighbours(
        &mut self,
        from: u64,
        p: Spherical,
        skip: u64,
        memo: &mut ProjMemo,
    ) -> Option<u64> {
        let n = {
            let g = self.geom(from)?;
            if g.nbrs.is_none() {
                g.nbrs = Some(get_global_cell_neighbors(from, false));
            }
            g.nbrs.as_ref().map(|v| v.len()).unwrap_or(0)
        };
        for i in 0..n {
            // re-borrow per iteration: `test` may insert into `geoms`
            let cand = self.geoms.get(&from)?.nbrs.as_ref()?[i];
            if cand == skip {
                continue;
            }
            if self.test(cand, p, memo) {
                if i != 0 {
                    if let Some(v) = self.geoms.get_mut(&from).and_then(|g| g.nbrs.as_mut()) {
                        v.swap(0, i);
                    }
                }
                return Some(cand);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Row scan of a 10 m grid must agree with the crate's search at every
    /// pixel, across the resolutions callers use.
    #[test]
    fn locator_matches_spherical_to_cell() {
        for &res in &[1, 2, 8, 14, 16, 18, 20] {
            let w = 256usize;
            let h = 128usize;
            let lat0: f64 = 5.5;
            let dlat = 10.0 / 111_320.0;
            let dlon = 10.0 / (111_320.0 * lat0.to_radians().cos());
            let mut loc = CellLocator::new(res);
            let mut prev = vec![NO_CELL; w];
            for r in 0..h {
                for c in 0..w {
                    let p = a5_spherical(117.0 + c as f64 * dlon, lat0 - r as f64 * dlat);
                    let got = loc.locate(p, prev[c]);
                    prev[c] = got;
                    let want = spherical_to_cell(p, res).unwrap();
                    assert_eq!(got, want, "res {res} r {r} c {c}");
                }
            }
        }
    }

    /// Jumps (nodata gaps, bbox edges) must fall back cleanly.
    #[test]
    fn locator_handles_jumps() {
        let mut loc = CellLocator::new(18);
        let pts = [(117.0, 5.5), (117.0001, 5.5), (-0.1, 51.5), (117.0002, 5.5), (0.0, 0.0)];
        for &(lon, lat) in &pts {
            let p = a5_spherical(lon, lat);
            assert_eq!(loc.locate(p, NO_CELL), spherical_to_cell(p, 18).unwrap());
        }
    }
}
