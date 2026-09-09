//! Point -> A5 cell locator for raster scans.
//!
//! The general lookup, `a5::core::cell::spherical_to_cell`, costs ~2.5 us
//! at fine resolutions: its first estimate is right only ~58% of the time
//! (measured at res 18), and each spiral sample rebuilds a pentagon from a
//! Hilbert decode. A raster scan can do far better because consecutive
//! pixels are a few metres apart, so the containing cell is almost always
//! one already seen: the previous pixel's cell, or a cell of the row above.
//!
//! Per point the locator tests, with cached pentagons and one dodecahedron
//! projection per origin:
//! 1. the previous hit;
//! 2. each caller hint (row-above cells around the column, or a pixel's
//!    corner cells for overlay sub-points);
//! 3. the children of the parents (at `resolution - k`) of those cells,
//!    in most-recently-hit order. A5 children do not nest geometrically
//!    in their parent, so this is a candidate set, never an assumption;
//! 4. the general search.
//! Step 3 is what makes a *new* cell cheap: at res 18 on 10 m pixels one
//! pixel in five starts a cell, and enumerating a cell's neighbours
//! upstream costs as much as the search (2.2 us). Measured on that grid:
//! 791 ns/px with neighbour enumeration, 424 ns/px with window parents.
//!
//! Points may arrive as an approximate face coordinate with an error
//! margin (from the bilinear grid projector). Containment is then decided
//! with that margin and the exact projection is computed only for points
//! within the margin of an edge, so results are identical to the exact
//! path (see `grid_proj.rs`).
//!
//! `a5::core::*` paths are `#[doc(hidden)]` upstream but stable in practice
//! (a5R depends on the same ones).

use a5::coordinate_systems::{Face, Spherical};
use a5::core::cell::{get_pentagon, spherical_to_cell};
use a5::core::serialization::{deserialize, FIRST_HILBERT_RESOLUTION};
use a5::core::utils::OriginId;
use a5::projections::dodecahedron::DodecahedronProjection;
use ahash::AHashMap;

/// Sentinel for "no cell" (lookup failed, or no hint available).
pub const NO_CELL: u64 = u64::MAX;

/// Geometry cache caps: cleared when exceeded. Entries are created lazily
/// for tested cells only.
const GEOM_CACHE_MAX: usize = 65_536;
const KIDS_CACHE_MAX: usize = 8_192;

/// Lon/lat in degrees -> A5's internal spherical frame (rotated authalic
/// sphere). This is the projection `a5::lonlat_to_cell` performs internally;
/// doing it once per point lets the cached pentagon tests and the search
/// fallback share it.
#[inline]
pub fn a5_spherical(lon_deg: f64, lat_deg: f64) -> Spherical {
    a5::core::coordinate_transforms::from_lon_lat(a5::LonLat::new(lon_deg, lat_deg))
}

/// A point's approximate position in one origin's face frame, with the
/// maximum error of that approximation (face units).
#[derive(Clone, Copy, Debug)]
pub struct ApproxFace {
    pub origin: OriginId,
    pub face: Face,
    pub margin: f64,
}

/// Pre-computed cell geometry in its origin's face frame.
struct CellGeom {
    id: u64,
    origin: OriginId,
    /// Pentagon vertices (CCW), 3 for res 1 quintants, 5 otherwise.
    verts: [(f64, f64); 5],
    /// 1 / edge length, for signed distances.
    inv_len: [f64; 5],
    n_verts: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    In,
    Out,
    Ambiguous,
}

impl CellGeom {
    fn build(id: u64) -> Option<Self> {
        let cell = deserialize(id).ok()?;
        let pent = get_pentagon(&cell).ok()?;
        let vv = pent.get_vertices_vec();
        if vv.len() < 3 || vv.len() > 5 {
            return None;
        }
        let n = vv.len();
        let mut verts = [(0.0, 0.0); 5];
        let mut inv_len = [0.0; 5];
        for (i, v) in vv.iter().enumerate() {
            verts[i] = (v.x(), v.y());
        }
        for i in 0..n {
            let (x1, y1) = verts[i];
            let (x2, y2) = verts[if i + 1 == n { 0 } else { i + 1 }];
            let len = ((x1 - x2).powi(2) + (y1 - y2).powi(2)).sqrt();
            inv_len[i] = if len > 0.0 { 1.0 / len } else { 0.0 };
        }
        Some(Self { id, origin: cell.origin_id, verts, inv_len, n_verts: n })
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

    /// Containment of a point known only to within `margin` of `p`.
    #[inline]
    fn classify(&self, p: Face, margin: f64) -> Side {
        let (px, py) = (p.x(), p.y());
        let n = self.n_verts;
        let mut min_d = f64::INFINITY;
        for i in 0..n {
            let (x1, y1) = self.verts[i];
            let (x2, y2) = self.verts[if i + 1 == n { 0 } else { i + 1 }];
            let d = ((x1 - x2) * (py - y1) - (y1 - y2) * (px - x1)) * self.inv_len[i];
            if d < -margin {
                return Side::Out;
            }
            if d < min_d {
                min_d = d;
            }
        }
        if min_d > margin {
            Side::In
        } else {
            Side::Ambiguous
        }
    }
}

/// Per-point state: the approximate face point, the exact spherical point
/// (computed on demand, at most once) and its projections per origin.
struct PointEval<'a> {
    approx: Option<ApproxFace>,
    exact: &'a mut dyn FnMut() -> Option<Spherical>,
    sph: Option<Option<Spherical>>,
    proj_origin: OriginId,
    proj_face: Face,
    proj_valid: bool,
}

impl PointEval<'_> {
    #[inline]
    fn spherical(&mut self) -> Option<Spherical> {
        if self.sph.is_none() {
            self.sph = Some((self.exact)());
        }
        self.sph.unwrap()
    }

    /// Exact face coordinates in `origin`'s frame.
    #[inline]
    fn exact_face(&mut self, origin: OriginId) -> Option<Face> {
        if !self.proj_valid || self.proj_origin != origin {
            let s = self.spherical()?;
            let d = DodecahedronProjection::get_thread_local();
            let f = d.forward(s, origin).ok()?;
            self.proj_origin = origin;
            self.proj_face = f;
            self.proj_valid = true;
        }
        Some(self.proj_face)
    }

    #[inline]
    fn inside(&mut self, g: &CellGeom) -> bool {
        if let Some(a) = self.approx {
            if a.origin == g.origin {
                match g.classify(a.face, a.margin) {
                    Side::In => return true,
                    Side::Out => return false,
                    Side::Ambiguous => {}
                }
            }
        }
        match self.exact_face(g.origin) {
            Some(f) => g.contains(f),
            None => false,
        }
    }
}

/// Neighbour-first point -> cell locator. One per scan (tile stripe); holds
/// the previous hit and caches of tested cells' geometry.
pub struct CellLocator {
    resolution: i32,
    /// Below the first Hilbert resolution `spherical_to_cell` is an exact
    /// O(1) estimate, so the candidate tests are skipped.
    direct: bool,
    /// Parent resolution for the children stage, or `None` below res 4.
    parent_res: Option<i32>,
    geoms: AHashMap<u64, CellGeom>,
    /// Children geometry per parent, most-recently-hit first.
    kids: AHashMap<u64, Vec<CellGeom>>,
    last: u64,
}

impl CellLocator {
    /// `px_per_cell` is the number of source pixels per target cell
    /// (any positive value; it only picks how far up the parents sit).
    pub fn new(resolution: i32, px_per_cell: f64) -> Self {
        // Parents two levels up hold 16 children: with >= 4 px per cell a
        // parent covers >= 64 pixels, enough scan to amortise building its
        // children. Coarser pixels take three levels (64 children) so a
        // parent still spans a run of pixels.
        let k = if px_per_cell >= 4.0 { 2 } else { 3 };
        let parent_res = if resolution - k >= FIRST_HILBERT_RESOLUTION {
            Some(resolution - k)
        } else {
            None
        };
        Self {
            resolution,
            direct: resolution < FIRST_HILBERT_RESOLUTION,
            parent_res,
            geoms: AHashMap::with_capacity(1024),
            kids: AHashMap::with_capacity(256),
            last: NO_CELL,
        }
    }

    #[inline]
    fn geom(&mut self, id: u64) -> Option<&CellGeom> {
        if !self.geoms.contains_key(&id) {
            if self.geoms.len() >= GEOM_CACHE_MAX {
                self.geoms.clear();
            }
            let g = CellGeom::build(id)?;
            self.geoms.insert(id, g);
        }
        self.geoms.get(&id)
    }

    #[inline]
    fn test(&mut self, id: u64, ev: &mut PointEval) -> bool {
        match self.geom(id) {
            Some(g) => ev.inside(g),
            None => false,
        }
    }

    /// Test the children of `parent`; a hit moves to the front.
    fn test_children(&mut self, parent: u64, ev: &mut PointEval) -> Option<u64> {
        if !self.kids.contains_key(&parent) {
            if self.kids.len() >= KIDS_CACHE_MAX {
                self.kids.clear();
            }
            let ids = a5::cell_to_children(parent, Some(self.resolution)).ok()?;
            let v: Vec<CellGeom> = ids.into_iter().filter_map(CellGeom::build).collect();
            self.kids.insert(parent, v);
        }
        let v = self.kids.get_mut(&parent)?;
        let mut hit = usize::MAX;
        for (i, g) in v.iter().enumerate() {
            if ev.inside(g) {
                hit = i;
                break;
            }
        }
        if hit == usize::MAX {
            return None;
        }
        if hit != 0 {
            v.swap(0, hit);
        }
        Some(v[0].id)
    }

    /// Cell containing the point, or `NO_CELL`.
    ///
    /// `approx` is the point's approximate face coordinate with its error
    /// margin, or `None`; `exact` yields the exact spherical point and is
    /// called at most once, only when needed; `hints` are candidate cells
    /// (`NO_CELL` entries are skipped).
    pub fn locate(
        &mut self,
        approx: Option<ApproxFace>,
        exact: &mut dyn FnMut() -> Option<Spherical>,
        hints: &[u64],
    ) -> u64 {
        let mut ev = PointEval {
            approx,
            exact,
            sph: None,
            proj_origin: 0,
            proj_face: Face::new(0.0, 0.0),
            proj_valid: false,
        };
        if self.direct {
            return match ev.spherical() {
                Some(s) => spherical_to_cell(s, self.resolution).unwrap_or(NO_CELL),
                None => NO_CELL,
            };
        }
        let last = self.last;
        if last != NO_CELL && self.test(last, &mut ev) {
            return last;
        }
        for &h in hints {
            if h != NO_CELL && h != last && self.test(h, &mut ev) {
                self.last = h;
                return h;
            }
        }
        if let Some(pres) = self.parent_res {
            let mut parents = [NO_CELL; 8];
            let mut np = 0usize;
            let push = |c: u64, parents: &mut [u64; 8], np: &mut usize| {
                if c == NO_CELL || *np >= parents.len() {
                    return;
                }
                if let Ok(p) = a5::cell_to_parent(c, Some(pres)) {
                    if !parents[..*np].contains(&p) {
                        parents[*np] = p;
                        *np += 1;
                    }
                }
            };
            push(last, &mut parents, &mut np);
            for &h in hints {
                push(h, &mut parents, &mut np);
            }
            for i in 0..np {
                if let Some(hit) = self.test_children(parents[i], &mut ev) {
                    self.last = hit;
                    return hit;
                }
            }
        }
        match ev.spherical().and_then(|s| spherical_to_cell(s, self.resolution).ok()) {
            Some(id) => {
                self.last = id;
                id
            }
            None => NO_CELL,
        }
    }

    /// Convenience for callers holding the exact spherical point.
    #[inline]
    pub fn locate_exact(&mut self, sph: Spherical, hints: &[u64]) -> u64 {
        let mut f = move || Some(sph);
        self.locate(None, &mut f, hints)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(lat0: f64, lon0: f64, w: usize, h: usize, pixel_m: f64) -> Vec<Spherical> {
        let dlat = pixel_m / 111_320.0;
        let dlon = pixel_m / (111_320.0 * lat0.to_radians().cos());
        let mut v = Vec::with_capacity(w * h);
        for r in 0..h {
            for c in 0..w {
                v.push(a5_spherical(lon0 + c as f64 * dlon, lat0 - r as f64 * dlat));
            }
        }
        v
    }

    /// Row scan must agree with the crate's search at every pixel, across
    /// resolutions and pixel sizes (cells much larger and much smaller
    /// than pixels), with row-above window hints.
    #[test]
    fn locator_matches_spherical_to_cell() {
        for &(res, pixel_m) in &[(1, 10.0), (2, 10.0), (4, 10.0), (8, 100.0), (14, 10.0),
                                 (16, 10.0), (18, 10.0), (18, 100.0), (20, 10.0), (22, 1.0)] {
            let (w, h) = (128usize, 64usize);
            let pts = grid(5.5, 117.0, w, h, pixel_m);
            let px_per_cell = a5::cell_area(res) / (pixel_m * pixel_m);
            let mut loc = CellLocator::new(res, px_per_cell);
            let mut prev = vec![NO_CELL; w];
            for r in 0..h {
                for c in 0..w {
                    let mut hints = [NO_CELL; 5];
                    hints[0] = prev[c];
                    if c + 1 < w { hints[1] = prev[c + 1]; }
                    if c + 2 < w { hints[2] = prev[c + 2]; }
                    if c + 3 < w { hints[3] = prev[c + 3]; }
                    if c >= 1 { hints[4] = prev[c - 1]; }
                    let p = pts[r * w + c];
                    let got = loc.locate_exact(p, &hints);
                    prev[c] = got;
                    assert_eq!(got, spherical_to_cell(p, res).unwrap(), "res {res} px {pixel_m} r {r} c {c}");
                }
            }
        }
    }

    /// Jumps (nodata gaps, bbox edges), other hemispheres, near the poles
    /// and the antimeridian must fall back cleanly.
    #[test]
    fn locator_handles_jumps() {
        let mut loc = CellLocator::new(18, 5.0);
        let pts = [(117.0, 5.5), (117.0001, 5.5), (-0.1, 51.5), (117.0002, 5.5), (0.0, 0.0),
                   (179.9999, -45.0), (-179.9999, -45.0), (10.0, 89.99), (-120.0, -89.99)];
        for &(lon, lat) in &pts {
            let p = a5_spherical(lon, lat);
            assert_eq!(loc.locate_exact(p, &[]), spherical_to_cell(p, 18).unwrap());
        }
    }

    /// An approximate face point with a margin must give the exact answer,
    /// including when the margin is generous (forces the exact path).
    #[test]
    fn locator_approx_margin_is_exact() {
        let res = 18;
        let pts = grid(5.5, 117.0, 64, 16, 10.0);
        let d = DodecahedronProjection::get_thread_local();
        let origin = deserialize(spherical_to_cell(pts[0], res).unwrap()).unwrap().origin_id;
        for &margin in &[0.0, 1e-9, 1e-6, 1e-3] {
            let mut loc = CellLocator::new(res, 5.0);
            for &p in &pts {
                let f = d.forward(p, origin).unwrap();
                // perturb within the margin
                let fp = Face::new(f.x() + 0.5 * margin, f.y() - 0.3 * margin);
                let approx = Some(ApproxFace { origin, face: fp, margin });
                let mut ex = move || Some(p);
                assert_eq!(loc.locate(approx, &mut ex, &[]), spherical_to_cell(p, res).unwrap());
            }
        }
    }
}
