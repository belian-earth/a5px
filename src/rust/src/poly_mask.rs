//! Pixel mask from polygons: whether a pixel centre lies inside polygons
//! whose edges are great circles, as in a5R.
//!
//! The polygons are projected gnomonically, which maps great circles to
//! straight lines, so an even-odd test in the plane is exact for
//! great-circle edges. They share one projection centre when every vertex
//! lies within `MAX_REACH_DEG` of it, else each polygon has its own.

use crate::error::{A5CogError, Result};

/// The furthest a vertex may lie from its projection centre. The gnomonic
/// plane holds less than a hemisphere and stretches as sec^2 near its edge.
const MAX_REACH_DEG: f64 = 80.0;

type V3 = [f64; 3];

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalise(a: V3) -> Option<V3> {
    let n = dot(a, a).sqrt();
    (n > 1e-12).then(|| [a[0] / n, a[1] / n, a[2] / n])
}

fn unit(lon: f64, lat: f64) -> V3 {
    let (lon, lat) = (lon.to_radians(), lat.to_radians());
    [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()]
}

/// Squared distance from `(px, py)` to the segment `e`.
fn seg_dist2(px: f64, py: f64, e: &[f64; 4]) -> f64 {
    let (dx, dy) = (e[2] - e[0], e[3] - e[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((px - e[0]) * dx + (py - e[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let (ex, ey) = (e[0] + t * dx - px, e[1] + t * dy - py);
    ex * ex + ey * ey
}

/// Orientation of `c` about the line `a -> b`, the sign of their cross
/// product. Zero counts as positive, so a vertex on a line falls on one
/// side for every edge that shares it.
fn side(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> bool {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0) >= 0.0
}

/// Whether the segment `r -> q` crosses the edge `e`.
fn crosses(r: (f64, f64), q: (f64, f64), e: &[f64; 4]) -> bool {
    let (a, b) = ((e[0], e[1]), (e[2], e[3]));
    side(r, q, a) != side(r, q, b) && side(a, b, r) != side(a, b, q)
}

/// Polygons projected about one centre, with their edges indexed by a grid
/// over the plane. Each grid cell lists the edges that meet it and the
/// polygons containing its centre, so a test counts only the crossings
/// between the cell centre and the point, a few edges near a boundary and
/// none elsewhere.
struct Group {
    c: V3,
    e1: V3,
    e2: V3,
    /// Envelope of the edges in the plane: xmin, ymin, xmax, ymax.
    bb: [f64; 4],
    /// x0, y0, x1, y1 in the plane.
    edges: Vec<[f64; 4]>,
    /// Polygon of each edge, ascending.
    edge_poly: Vec<u32>,
    gx: usize,
    gy: usize,
    /// Grid cells per unit of the plane in x and y.
    sx: f64,
    sy: f64,
    /// Edges meeting each grid cell, ascending (so by polygon).
    cell_start: Vec<u32>,
    cell_edges: Vec<u32>,
    /// Polygons containing each grid cell's centre, ascending.
    ref_start: Vec<u32>,
    ref_polys: Vec<u32>,
}

/// Grid cells per edge, and the most cells a group's grid may have.
const CELLS_PER_EDGE: usize = 4;
const MAX_CELLS: usize = 1 << 21;

impl Group {
    /// `polys`: each polygon's rings, closed or not, as lon/lat vertices.
    fn new(polys: &[&[Vec<(f64, f64)>]], c: V3) -> Option<Group> {
        // tangent basis at the centre; near a pole, take x as the reference
        let e1 = normalise(cross([0.0, 0.0, 1.0], c)).or_else(|| normalise(cross(c, [0.0, 1.0, 0.0])))?;
        let e2 = cross(c, e1);
        let mut edges = Vec::new();
        let mut edge_poly = Vec::new();
        for (k, rings) in polys.iter().enumerate() {
            for ring in rings.iter() {
                let pts: Vec<(f64, f64)> = ring
                    .iter()
                    .map(|&(lon, lat)| {
                        let p = unit(lon, lat);
                        let d = dot(p, c);
                        (dot(p, e1) / d, dot(p, e2) / d)
                    })
                    .collect();
                let n = pts.len();
                for i in 0..n {
                    let (a, b) = (pts[i], pts[(i + 1) % n]);
                    if a != b {
                        edges.push([a.0, a.1, b.0, b.1]);
                        edge_poly.push(k as u32);
                    }
                }
            }
        }
        if edges.is_empty() {
            return None;
        }
        let mut bb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for e in &edges {
            bb[0] = bb[0].min(e[0].min(e[2]));
            bb[1] = bb[1].min(e[1].min(e[3]));
            bb[2] = bb[2].max(e[0].max(e[2]));
            bb[3] = bb[3].max(e[1].max(e[3]));
        }
        let (w, h) = (bb[2] - bb[0], bb[3] - bb[1]);
        if !(w > 0.0 && h > 0.0) {
            // no area
            return None;
        }
        let n_cells = (CELLS_PER_EDGE * edges.len()).clamp(1, MAX_CELLS) as f64;
        let gx = ((n_cells * w / h).sqrt().ceil() as usize).clamp(1, 1 << 12);
        let gy = ((n_cells / gx as f64).ceil() as usize).clamp(1, 1 << 12);
        let mut g = Group {
            c, e1, e2, bb, edges, edge_poly, gx, gy,
            sx: gx as f64 / w,
            sy: gy as f64 / h,
            cell_start: Vec::new(),
            cell_edges: Vec::new(),
            ref_start: Vec::new(),
            ref_polys: Vec::new(),
        };

        // (grid cell, edge) for every cell an edge passes through, found row
        // by row from the edge's x extent within the row, padded for rounding
        let eps = 1e-9 * w.max(h);
        let mut pairs: Vec<(u32, u32)> = Vec::with_capacity(3 * g.edges.len());
        for (k, e) in g.edges.iter().enumerate() {
            let (ylo, yhi) = (e[1].min(e[3]), e[1].max(e[3]));
            for r in g.cell_y(ylo - eps)..=g.cell_y(yhi + eps) {
                let ra = (g.bb[1] + r as f64 / g.sy).max(ylo);
                let rb = (g.bb[1] + (r + 1) as f64 / g.sy).min(yhi);
                let x_at = |y: f64| {
                    if e[3] == e[1] { e[0] } else { e[0] + (y - e[1]) * (e[2] - e[0]) / (e[3] - e[1]) }
                };
                let (xa, xb) = if e[3] == e[1] {
                    (e[0].min(e[2]), e[0].max(e[2]))
                } else {
                    let (p, q) = (x_at(ra), x_at(rb));
                    (p.min(q), p.max(q))
                };
                for col in g.cell_x(xa - eps)..=g.cell_x(xb + eps) {
                    pairs.push(((r * g.gx + col) as u32, k as u32));
                }
            }
        }
        pairs.sort_unstable();
        pairs.dedup();
        let n_grid = g.gx * g.gy;
        let mut cell_start = vec![0u32; n_grid + 1];
        for &(cell, _) in &pairs {
            cell_start[cell as usize + 1] += 1;
        }
        for i in 0..n_grid {
            cell_start[i + 1] += cell_start[i];
        }
        g.cell_edges = pairs.iter().map(|&(_, k)| k).collect();
        g.cell_start = cell_start;

        // polygons containing each cell centre, by a scanline through each
        // row of centres: the edges a row's centre line crosses all meet a
        // cell of that row
        let n_polys = polys.len();
        let mut odd = vec![false; n_polys];
        let mut odd_list: Vec<u32> = Vec::new();
        let mut ref_start = Vec::with_capacity(n_grid + 1);
        let mut ref_polys = Vec::new();
        ref_start.push(0u32);
        let mut row_edges: Vec<u32> = Vec::new();
        let mut xs: Vec<(f64, u32)> = Vec::new();
        for r in 0..g.gy {
            let y = g.bb[1] + (r as f64 + 0.5) / g.sy;
            row_edges.clear();
            row_edges.extend_from_slice(&g.cell_edges[g.cell_start[r * g.gx] as usize..g.cell_start[(r + 1) * g.gx] as usize]);
            row_edges.sort_unstable();
            row_edges.dedup();
            xs.clear();
            for &k in &row_edges {
                let e = &g.edges[k as usize];
                if (e[1] <= y) != (e[3] <= y) {
                    xs.push((e[0] + (y - e[1]) * (e[2] - e[0]) / (e[3] - e[1]), g.edge_poly[k as usize]));
                }
            }
            xs.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
            for &p in &odd_list {
                odd[p as usize] = false;
            }
            odd_list.clear();
            let mut next = 0usize;
            for col in 0..g.gx {
                let x = g.bb[0] + (col as f64 + 0.5) / g.sx;
                while next < xs.len() && xs[next].0 < x {
                    let p = xs[next].1;
                    odd[p as usize] = !odd[p as usize];
                    if odd[p as usize] {
                        odd_list.push(p);
                    } else {
                        odd_list.retain(|&q| q != p);
                    }
                    next += 1;
                }
                let start = ref_polys.len();
                ref_polys.extend_from_slice(&odd_list);
                ref_polys[start..].sort_unstable();
                ref_start.push(ref_polys.len() as u32);
            }
        }
        g.ref_start = ref_start;
        g.ref_polys = ref_polys;
        Some(g)
    }

    fn cell_x(&self, x: f64) -> usize {
        (((x - self.bb[0]) * self.sx).floor().max(0.0) as usize).min(self.gx - 1)
    }

    fn cell_y(&self, y: f64) -> usize {
        (((y - self.bb[1]) * self.sy).floor().max(0.0) as usize).min(self.gy - 1)
    }

    fn cell_slice(&self, cell: usize) -> &[u32] {
        &self.cell_edges[self.cell_start[cell] as usize..self.cell_start[cell + 1] as usize]
    }

    /// Whether `p` lies inside any polygon of the group, given `p` to
    /// within `margin` radians; `None` when it lies within the margin of an
    /// edge. A margin of 0 always decides.
    fn test(&self, p: V3, margin: f64) -> Option<bool> {
        let d = dot(p, self.c);
        if d <= 0.0 {
            // a hemisphere from the centre; every vertex is within 80 degrees
            return Some(false);
        }
        let (x, y) = (dot(p, self.e1) / d, dot(p, self.e2) / d);
        // gnomonic scale is at most sec^2 of the distance from the centre
        let mp = if margin > 0.0 { margin * 1.01 / (d * d) + 1e-15 } else { 0.0 };
        if x < self.bb[0] - mp || x > self.bb[2] + mp || y < self.bb[1] - mp || y > self.bb[3] + mp {
            return Some(false);
        }
        let (i, j) = (self.cell_x(x), self.cell_y(y));
        let cell = j * self.gx + i;
        let r = (self.bb[0] + (i as f64 + 0.5) / self.sx, self.bb[1] + (j as f64 + 0.5) / self.sy);
        let q = (x, y);
        let refs = &self.ref_polys[self.ref_start[cell] as usize..self.ref_start[cell + 1] as usize];
        let mp2 = mp * mp;
        let mut near = false;
        // walk the cell's edges and its centre's polygons together, by polygon:
        // a polygon contains the point when it contains the centre and an
        // even number of its edges cross between them, or the reverse
        let mut inside = false;
        let mut ri = 0usize;
        let mut cur = u32::MAX;
        let mut parity = false;
        for &k in self.cell_slice(cell) {
            let e = &self.edges[k as usize];
            let poly = self.edge_poly[k as usize];
            if poly != cur {
                inside |= parity;
                while ri < refs.len() && refs[ri] < poly {
                    // contains the centre and no edge of it meets the cell
                    inside = true;
                    ri += 1;
                }
                parity = ri < refs.len() && refs[ri] == poly;
                if parity {
                    ri += 1;
                }
                cur = poly;
            }
            if crosses(r, q, e) {
                parity = !parity;
            }
            if mp > 0.0 && !near && seg_dist2(x, y, e) <= mp2 {
                near = true;
            }
        }
        inside |= parity || ri < refs.len();
        if mp > 0.0 && !near {
            // edges within the margin may meet only a neighbouring cell
            'cells: for jj in self.cell_y(y - mp)..=self.cell_y(y + mp) {
                for ii in self.cell_x(x - mp)..=self.cell_x(x + mp) {
                    if ii == i && jj == j {
                        continue;
                    }
                    for &k in self.cell_slice(jj * self.gx + ii) {
                        if seg_dist2(x, y, &self.edges[k as usize]) <= mp2 {
                            near = true;
                            break 'cells;
                        }
                    }
                }
            }
        }
        if near { None } else { Some(inside) }
    }
}

/// A union of polygons, each an outer ring and its holes, with
/// great-circle edges. A point is inside when the even-odd rule over one
/// polygon's rings says so for some polygon, so holes are outside.
pub(crate) struct PolyMask {
    groups: Vec<Group>,
}

impl PolyMask {
    /// `xy`: lon/lat pairs, interleaved. `rings`: (polygon id, vertex
    /// count) pairs, interleaved, one per ring in `xy` order; the rings of
    /// a polygon are contiguous. `None` when there are no rings.
    pub(crate) fn from_rings(xy: &[f64], rings: &[i32]) -> Result<Option<PolyMask>> {
        if rings.is_empty() {
            return Ok(None);
        }
        if rings.len() % 2 != 0 || xy.len() % 2 != 0 {
            return Err(A5CogError::Invalid("mask rings are malformed".into()));
        }
        let mut polys: Vec<Vec<Vec<(f64, f64)>>> = Vec::new();
        let mut last_id: Option<i32> = None;
        let mut off = 0usize;
        for r in rings.chunks_exact(2) {
            let (id, n) = (r[0], r[1].max(0) as usize);
            if 2 * (off + n) > xy.len() {
                return Err(A5CogError::Invalid("mask rings are malformed".into()));
            }
            let mut ring: Vec<(f64, f64)> = (off..off + n).map(|i| (xy[2 * i], xy[2 * i + 1])).collect();
            off += n;
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            if last_id != Some(id) {
                polys.push(Vec::new());
                last_id = Some(id);
            }
            if ring.len() >= 3 {
                polys.last_mut().expect("pushed above").push(ring);
            }
        }
        polys.retain(|p| !p.is_empty());
        if polys.is_empty() {
            return Ok(Some(PolyMask { groups: Vec::new() }));
        }
        let min_dot = MAX_REACH_DEG.to_radians().cos();
        let centre_of = |ps: &[Vec<Vec<(f64, f64)>>]| -> Option<V3> {
            let mut s = [0.0; 3];
            for ring in ps.iter().flatten() {
                for &(lon, lat) in ring {
                    let u = unit(lon, lat);
                    s = [s[0] + u[0], s[1] + u[1], s[2] + u[2]];
                }
            }
            let c = normalise(s)?;
            ps.iter().flatten().flatten().all(|&(lon, lat)| dot(unit(lon, lat), c) >= min_dot).then_some(c)
        };
        let all: Vec<&[Vec<(f64, f64)>]> = polys.iter().map(|p| p.as_slice()).collect();
        let mut groups = Vec::new();
        if let Some(c) = centre_of(&polys) {
            groups.extend(Group::new(&all, c));
        } else {
            for (k, p) in polys.iter().enumerate() {
                let Some(c) = centre_of(std::slice::from_ref(p)) else {
                    return Err(A5CogError::Invalid(format!(
                        "mask polygon {} reaches more than {MAX_REACH_DEG} degrees from its centre; \
                         split it into smaller polygons",
                        k + 1
                    )));
                };
                groups.extend(Group::new(&[p.as_slice()], c));
            }
        }
        Ok(Some(PolyMask { groups }))
    }

    /// Membership of a point known to within `margin` degrees in lon and in
    /// lat; `None` when it lies within the margin of an edge and needs the
    /// exact position.
    pub(crate) fn classify(&self, lon: f64, lat: f64, margin: f64) -> Option<bool> {
        let p = unit(lon, lat);
        let m = margin.to_radians() * std::f64::consts::SQRT_2;
        let mut undecided = false;
        for g in &self.groups {
            match g.test(p, m) {
                Some(true) => return Some(true),
                None => undecided = true,
                Some(false) => {}
            }
        }
        if undecided { None } else { Some(false) }
    }

    /// Membership of an exact point.
    pub(crate) fn contains(&self, lon: f64, lat: f64) -> bool {
        let p = unit(lon, lat);
        self.groups.iter().any(|g| g.test(p, 0.0) == Some(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(polys: &[&[&[(f64, f64)]]]) -> PolyMask {
        let mut xy = Vec::new();
        let mut rings = Vec::new();
        for (k, p) in polys.iter().enumerate() {
            for r in p.iter() {
                rings.extend([k as i32, r.len() as i32]);
                xy.extend(r.iter().flat_map(|&(x, y)| [x, y]));
            }
        }
        PolyMask::from_rings(&xy, &rings).unwrap().unwrap()
    }

    const SQ: &[(f64, f64)] = &[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)];
    const HOLE: &[(f64, f64)] = &[(0.4, 0.4), (0.6, 0.4), (0.6, 0.6), (0.4, 0.6)];

    #[test]
    fn square_with_hole() {
        let m = mask(&[&[SQ, HOLE]]);
        assert!(m.contains(0.2, 0.2));
        assert!(!m.contains(0.5, 0.5));
        assert!(!m.contains(1.2, 0.5));
        assert!(!m.contains(-0.1, 0.5));
        assert_eq!(m.classify(0.2, 0.2, 1e-6), Some(true));
        assert_eq!(m.classify(0.5, 0.5, 1e-6), Some(false));
        // within the margin of an edge
        assert_eq!(m.classify(0.4 + 1e-7, 0.5, 1e-6), None);
    }

    #[test]
    fn edges_are_great_circles() {
        // the edge from (0, 60) to (20, 60) bulges north of the parallel,
        // to 60.38 N at 10 E
        let m = mask(&[&[&[(0.0, 50.0), (20.0, 50.0), (20.0, 60.0), (0.0, 60.0)]]]);
        assert!(m.contains(10.0, 60.3));
        assert!(!m.contains(10.0, 60.5));
        // and the southern edge to 50.43 N
        assert!(!m.contains(10.0, 50.1));
    }

    #[test]
    fn union_of_polygons_and_antimeridian() {
        let a: &[(f64, f64)] = &[(179.0, -1.0), (-179.0, -1.0), (-179.0, 1.0), (179.0, 1.0)];
        let b: &[(f64, f64)] = &[(10.0, 10.0), (11.0, 10.0), (11.0, 11.0), (10.0, 11.0)];
        let m = mask(&[&[a], &[b]]);
        assert!(m.contains(180.0, 0.0));
        assert!(m.contains(-179.5, 0.5));
        assert!(m.contains(10.5, 10.5));
        assert!(!m.contains(0.0, 0.0));
        // far apart polygons share no centre
        let c: &[(f64, f64)] = &[(-170.0, -10.0), (-169.0, -10.0), (-169.0, -9.0)];
        let m = mask(&[&[b], &[c]]);
        assert_eq!(m.groups.len(), 2);
        assert!(m.contains(-169.2, -9.5));
        assert!(!m.contains(-169.8, -9.5));
        assert!(m.contains(10.5, 10.5));
    }

    #[test]
    fn overlapping_polygons_do_not_cancel() {
        let b: &[(f64, f64)] = &[(0.5, 0.0), (1.5, 0.0), (1.5, 1.0), (0.5, 1.0)];
        let m = mask(&[&[SQ], &[b]]);
        assert!(m.contains(0.75, 0.5));
    }

    /// Even-odd over every edge of each polygon, in the group's plane.
    fn brute(g: &Group, lon: f64, lat: f64) -> bool {
        let p = unit(lon, lat);
        let d = dot(p, g.c);
        if d <= 0.0 {
            return false;
        }
        let (x, y) = (dot(p, g.e1) / d, dot(p, g.e2) / d);
        let n_polys = g.edge_poly.last().map_or(0, |&k| k as usize + 1);
        let mut odd = vec![false; n_polys];
        for (e, &k) in g.edges.iter().zip(&g.edge_poly) {
            if (e[1] <= y) != (e[3] <= y) && x < e[0] + (y - e[1]) * (e[2] - e[0]) / (e[3] - e[1]) {
                odd[k as usize] = !odd[k as usize];
            }
        }
        odd.iter().any(|&o| o)
    }

    #[test]
    fn grid_agrees_with_every_edge() {
        // a wiggly ring with a wiggly hole, and a second polygon beside it
        let wiggle = |cx: f64, cy: f64, r: f64, n: usize| -> Vec<(f64, f64)> {
            (0..n)
                .map(|i| {
                    let t = i as f64 / n as f64 * std::f64::consts::TAU;
                    let rr = r * (1.0 + 0.15 * (t * 37.0).sin() + 0.05 * (t * 401.0).sin());
                    (cx + rr * t.cos(), cy + rr * t.sin())
                })
                .collect()
        };
        let outer = wiggle(10.0, 50.0, 1.0, 5000);
        let hole = wiggle(10.1, 50.0, 0.4, 800);
        let other = wiggle(12.6, 50.2, 0.5, 300);
        let mut xy = Vec::new();
        let mut rings = Vec::new();
        for (k, r) in [(0, &outer), (0, &hole), (1, &other)] {
            rings.extend([k, r.len() as i32]);
            xy.extend(r.iter().flat_map(|&(x, y)| [x, y]));
        }
        let m = PolyMask::from_rings(&xy, &rings).unwrap().unwrap();
        assert_eq!(m.groups.len(), 1);
        let g = &m.groups[0];
        // a small LCG, so the test needs no dependency
        let mut s: u64 = 42;
        let mut rnd = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let (mut n_in, mut n_undecided) = (0, 0);
        for _ in 0..50_000 {
            let (lon, lat) = (8.5 + 5.0 * rnd(), 48.5 + 3.0 * rnd());
            let want = brute(g, lon, lat);
            assert_eq!(m.contains(lon, lat), want, "{lon} {lat}");
            match m.classify(lon, lat, 1e-4) {
                Some(got) => assert_eq!(got, want, "{lon} {lat}"),
                None => n_undecided += 1,
            }
            n_in += want as usize;
        }
        assert!(n_in > 10_000 && n_in < 40_000, "{n_in}");
        // a 1e-4 degree margin leaves only points near an edge undecided
        assert!(n_undecided < 2_500, "{n_undecided}");
    }

    #[test]
    fn rejects_a_polygon_too_large_to_project() {
        let r = PolyMask::from_rings(&[0.0, 0.0, 170.0, 0.0, 85.0, 80.0], &[0, 3]);
        assert!(r.is_err());
    }
}
