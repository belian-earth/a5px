//! Windowed bilinear projection of a pixel grid into A5's face frame.
//!
//! Projecting every pixel exactly costs ~280 ns: the source CRS inverse
//! (proj4rs), the authalic conversion and the dodecahedron projection. The
//! composite map from pixel coordinates to a face frame is smooth, so over
//! a small window it is bilinear to well under the width of a cell edge.
//! This projector samples the exact map on a lattice every `WIN` pixels,
//! interpolates inside each window, and reports for each window the
//! largest interpolation error it observed at five interior probes
//! (centre and edge midpoints), times a safety factor. Callers treat a
//! point as known only to within that margin: the locator projects a
//! pixel exactly only when it lies within the margin of a cell edge, and
//! the bbox filter does the same at the bbox edges. Measured on 10 m
//! pixels at res 18, the error of a 16 px window is ~0.2 mm (1e-5 of a
//! cell edge) and 0.005% of pixels need the exact path.
//!
//! A window is marked exact-only when any of its samples fails to project
//! (outside the projection's domain), when its corners disagree on the
//! nearest dodecahedron origin or straddle the antimeridian, or when it
//! reaches within a degree of a pole; its pixels then take the exact path,
//! as does any pixel whose containing cell belongs to another origin.

use a5::coordinate_systems::{Face, Spherical};
use a5::core::origin::find_nearest_origin;
use a5::core::utils::OriginId;
use a5::projections::dodecahedron::DodecahedronProjection;
use proj4rs::Proj;

use crate::error::Result;
use crate::geo::GeoTransform;
use crate::locator::{a5_spherical, ApproxFace};
use crate::read::proj_points;

/// Window edge in grid points.
pub(crate) const WIN: usize = 16;

/// `A5PX_EXACT_PROJ=1` marks every window exact-only, so each pixel takes
/// the exact projection path. For tests and diagnostics: results must be
/// identical either way. Read per projector build (once per stripe) so
/// tests can toggle it within a process.
fn force_exact() -> bool {
    std::env::var_os("A5PX_EXACT_PROJ").is_some_and(|v| !v.is_empty() && v != "0")
}
/// Safety factor applied to the largest probed interpolation error.
const SAFETY: f64 = 4.0;
/// Absolute floor on the margins (face units are O(1); degrees).
const MARGIN_FLOOR: f64 = 1e-12;

#[derive(Clone, Copy)]
struct Sample {
    lon: f64,
    lat: f64,
    sph: Spherical,
    ok: bool,
}

#[derive(Clone, Copy)]
struct Window {
    /// Bilinear interpolation of face and lon/lat is valid.
    ok: bool,
    /// Lon/lat interpolation valid (false near the poles / antimeridian
    /// even when face interpolation is).
    ll_ok: bool,
    origin: OriginId,
    face: [(f64, f64); 4],
    ll: [(f64, f64); 4],
    margin_face: f64,
    margin_deg: f64,
}

/// A grid point as seen by the locator and the bbox filter.
#[derive(Clone, Copy)]
pub(crate) struct GridPoint {
    /// Approximate face position, or `None` when the window is exact-only.
    pub approx: Option<ApproxFace>,
    /// Approximate lon/lat (degrees) and its margin, or `None`.
    pub lonlat: Option<(f64, f64, f64)>,
}

/// Exact-map inputs, shared by the lattice build and on-demand pixels.
pub(crate) struct ExactMap<'a> {
    pub src_proj: &'a Proj,
    pub dst_proj: &'a Proj,
    pub gt: &'a GeoTransform,
    pub src_is_latlong: bool,
    pub dst_is_latlong: bool,
}

impl ExactMap<'_> {
    /// Source CRS coordinate of fractional pixel position `(col, row)`,
    /// ready for `proj_points`.
    #[inline]
    fn src_xy(&self, col: f64, row: f64) -> (f64, f64, f64) {
        let (mut x, mut y) = self.gt.pixel_xy(col, row);
        if self.src_is_latlong {
            x = x.to_radians();
            y = y.to_radians();
        }
        (x, y, 0.0)
    }

    /// Lon/lat in degrees of a batch of projected points, NaN where the
    /// projection failed.
    #[inline]
    fn to_lonlat(&self, p: (f64, f64, f64)) -> (f64, f64) {
        if self.dst_is_latlong {
            (p.0.to_degrees(), p.1.to_degrees())
        } else {
            (p.0, p.1)
        }
    }

    /// Exact lon/lat (degrees) of one pixel position, `None` if it does not
    /// project.
    pub(crate) fn lonlat(&self, col: f64, row: f64) -> Result<Option<(f64, f64)>> {
        let mut pts = [self.src_xy(col, row)];
        proj_points(self.src_proj, self.dst_proj, &mut pts)?;
        let (lon, lat) = self.to_lonlat(pts[0]);
        Ok(if lon.is_finite() && lat.is_finite() { Some((lon, lat)) } else { None })
    }
}

pub(crate) struct GridProjector {
    nx: usize,
    ny: usize,
    /// Windows across / down.
    wx: usize,
    wy: usize,
    wins: Vec<Window>,
}

impl GridProjector {
    /// Projector for grid points `(col0 + i + offset, row0 + j + offset)`,
    /// `i < nx`, `j < ny` (offset 0.5 for pixel centres, 0 for corners).
    pub(crate) fn new(
        map: &ExactMap,
        col0: f64,
        row0: f64,
        nx: usize,
        ny: usize,
        offset: f64,
    ) -> Result<Self> {
        let wx = nx.div_ceil(WIN).max(1);
        let wy = ny.div_ceil(WIN).max(1);
        if force_exact() {
            let off = Window {
                ok: false,
                ll_ok: false,
                origin: 0,
                face: [(0.0, 0.0); 4],
                ll: [(0.0, 0.0); 4],
                margin_face: 0.0,
                margin_deg: 0.0,
            };
            return Ok(Self { nx, ny, wx, wy, wins: vec![off; wx * wy] });
        }
        // exact samples: corner lattice (wx+1)(wy+1) plus 5 probes per window
        let n_corner = (wx + 1) * (wy + 1);
        let n_probe = 5 * wx * wy;
        let mut pos: Vec<(f64, f64)> = Vec::with_capacity(n_corner + n_probe);
        for j in 0..=wy {
            for i in 0..=wx {
                pos.push(((i * WIN) as f64, (j * WIN) as f64));
            }
        }
        let w = WIN as f64;
        for j in 0..wy {
            for i in 0..wx {
                let (x0, y0) = ((i * WIN) as f64, (j * WIN) as f64);
                pos.push((x0 + 0.5 * w, y0 + 0.5 * w));
                pos.push((x0 + 0.5 * w, y0));
                pos.push((x0 + 0.5 * w, y0 + w));
                pos.push((x0, y0 + 0.5 * w));
                pos.push((x0 + w, y0 + 0.5 * w));
            }
        }
        let mut pts: Vec<(f64, f64, f64)> = pos
            .iter()
            .map(|&(i, j)| map.src_xy(col0 + i + offset, row0 + j + offset))
            .collect();
        proj_points(map.src_proj, map.dst_proj, &mut pts)?;
        let samples: Vec<Sample> = pts
            .iter()
            .map(|&p| {
                let (lon, lat) = map.to_lonlat(p);
                let ok = lon.is_finite() && lat.is_finite();
                let sph = if ok { a5_spherical(lon, lat) } else { a5_spherical(0.0, 0.0) };
                Sample { lon, lat, sph, ok }
            })
            .collect();

        let d = DodecahedronProjection::get_thread_local();
        let mut wins: Vec<Window> = Vec::with_capacity(wx * wy);
        for j in 0..wy {
            for i in 0..wx {
                let c = [
                    samples[j * (wx + 1) + i],
                    samples[j * (wx + 1) + i + 1],
                    samples[(j + 1) * (wx + 1) + i],
                    samples[(j + 1) * (wx + 1) + i + 1],
                ];
                let pb = n_corner + 5 * (j * wx + i);
                let probes = &samples[pb..pb + 5];
                let mut win = Window {
                    ok: false,
                    ll_ok: false,
                    origin: 0,
                    face: [(0.0, 0.0); 4],
                    ll: [(0.0, 0.0); 4],
                    margin_face: 0.0,
                    margin_deg: 0.0,
                };
                if c.iter().any(|s| !s.ok) || probes.iter().any(|s| !s.ok) {
                    wins.push(win);
                    continue;
                }
                let origin = find_nearest_origin(probes[0].sph).id;
                if c.iter().any(|s| find_nearest_origin(s.sph).id != origin) {
                    wins.push(win);
                    continue;
                }
                let mut faces_ok = true;
                for (k, s) in c.iter().enumerate() {
                    match d.forward(s.sph, origin) {
                        Ok(f) => win.face[k] = (f.x(), f.y()),
                        Err(_) => faces_ok = false,
                    }
                    win.ll[k] = (s.lon, s.lat);
                }
                if !faces_ok {
                    wins.push(win);
                    continue;
                }
                win.origin = origin;
                // probe positions in window units: centre, top, bottom, left, right
                let uv = [(0.5, 0.5), (0.5, 0.0), (0.5, 1.0), (0.0, 0.5), (1.0, 0.5)];
                let mut err_face: f64 = 0.0;
                let mut err_deg: f64 = 0.0;
                for (k, s) in probes.iter().enumerate() {
                    let (u, v) = uv[k];
                    let (ax, ay) = bilinear(&win.face, u, v);
                    match d.forward(s.sph, origin) {
                        Ok(f) => {
                            err_face = err_face.max(((ax - f.x()).powi(2) + (ay - f.y()).powi(2)).sqrt());
                        }
                        Err(_) => faces_ok = false,
                    }
                    let (alon, alat) = bilinear(&win.ll, u, v);
                    err_deg = err_deg.max((alon - s.lon).abs().max((alat - s.lat).abs()));
                }
                if !faces_ok {
                    wins.push(win);
                    continue;
                }
                win.ok = true;
                win.margin_face = SAFETY * err_face + MARGIN_FLOOR;
                // lon/lat interpolation breaks across the antimeridian and
                // near the poles (longitude wraps within a window)
                let lons: Vec<f64> = c.iter().map(|s| s.lon).collect();
                let lats: Vec<f64> = c.iter().map(|s| s.lat).collect();
                let lon_span = lons.iter().cloned().fold(f64::MIN, f64::max)
                    - lons.iter().cloned().fold(f64::MAX, f64::min);
                let near_pole = lats.iter().any(|l| l.abs() > 89.0)
                    || probes.iter().any(|s| s.lat.abs() > 89.0);
                win.ll_ok = lon_span < 180.0 && !near_pole;
                win.margin_deg = SAFETY * err_deg + MARGIN_FLOOR;
                wins.push(win);
            }
        }
        Ok(Self { nx, ny, wx, wy, wins })
    }

    /// Approximate position of grid point `(i, j)`.
    #[inline]
    pub(crate) fn point(&self, i: usize, j: usize) -> GridPoint {
        debug_assert!(i < self.nx && j < self.ny);
        let (wi, wj) = (i / WIN, j / WIN);
        let win = &self.wins[wj * self.wx + wi];
        if !win.ok {
            return GridPoint { approx: None, lonlat: None };
        }
        let u = (i - wi * WIN) as f64 / WIN as f64;
        let v = (j - wj * WIN) as f64 / WIN as f64;
        let (x, y) = bilinear(&win.face, u, v);
        let approx = Some(ApproxFace {
            origin: win.origin,
            face: Face::new(x, y),
            margin: win.margin_face,
        });
        let lonlat = if win.ll_ok {
            let (lon, lat) = bilinear(&win.ll, u, v);
            Some((lon, lat, win.margin_deg))
        } else {
            None
        };
        GridPoint { approx, lonlat }
    }

    /// Number of windows that fell back to exact projection.
    pub(crate) fn n_exact_only(&self) -> usize {
        self.wins.iter().filter(|w| !w.ok).count()
    }

    pub(crate) fn n_windows(&self) -> usize {
        self.wx * self.wy
    }
}

/// Bilinear interpolation over corners ordered (0,0), (1,0), (0,1), (1,1).
#[inline]
fn bilinear(c: &[(f64, f64); 4], u: f64, v: f64) -> (f64, f64) {
    let w00 = (1.0 - u) * (1.0 - v);
    let w10 = u * (1.0 - v);
    let w01 = (1.0 - u) * v;
    let w11 = u * v;
    (
        w00 * c[0].0 + w10 * c[1].0 + w01 * c[2].0 + w11 * c[3].0,
        w00 * c[0].1 + w10 * c[1].1 + w01 * c[2].1 + w11 * c[3].1,
    )
}

/// Bbox membership of a point known to within `margin` degrees.
/// `Some(in)` when decided, `None` when the point is within the margin of
/// an edge and needs the exact position.
#[inline]
pub(crate) fn bbox_classify(lon: f64, lat: f64, margin: f64, b: &[f64; 4]) -> Option<bool> {
    let inside = lon >= b[0] + margin && lon <= b[2] - margin && lat >= b[1] + margin && lat <= b[3] - margin;
    if inside {
        return Some(true);
    }
    let outside = lon < b[0] - margin || lon > b[2] + margin || lat < b[1] - margin || lat > b[3] + margin;
    if outside {
        return Some(false);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locator::CellLocator;
    use a5::core::cell::spherical_to_cell;

    /// Every grid point's approximate face position must be within the
    /// window margin of the exact one, and locating through the
    /// approximation must agree with the exact search, for a projected
    /// (UTM) and a lat/long source at fine and coarse pixels.
    #[test]
    fn grid_projector_is_within_margin_and_exact() {
        let dst = Proj::from_epsg_code(4326).unwrap();
        let cases: Vec<(Proj, GeoTransform, bool, f64)> = vec![
            (Proj::from_epsg_code(32650).unwrap(),
             GeoTransform([720000.0, 10.0, 0.0, 608000.0, 0.0, -10.0]), false, 10.0),
            (Proj::from_epsg_code(32650).unwrap(),
             GeoTransform([500000.0, 1000.0, 0.0, 6000000.0, 0.0, -1000.0]), false, 1000.0),
            (Proj::from_epsg_code(4326).unwrap(),
             GeoTransform([-3.0, 0.0001, 0.0, 51.5, 0.0, -0.0001]), true, 8.0),
            (Proj::from_proj_string("+proj=laea +lat_0=52 +lon_0=10 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs").unwrap(),
             GeoTransform([-2000000.0, 20000.0, 0.0, 2000000.0, 0.0, -20000.0]), false, 20000.0),
        ];
        let d = DodecahedronProjection::get_thread_local();
        for (src, gt, src_ll, px_m) in &cases {
            let map = ExactMap { src_proj: src, dst_proj: &dst, gt, src_is_latlong: *src_ll, dst_is_latlong: true };
            let (nx, ny) = (70usize, 40usize);
            let gp = GridProjector::new(&map, 0.0, 0.0, nx, ny, 0.5).unwrap();
            let res = 18;
            let mut loc = CellLocator::new(res, a5::cell_area(res) / (px_m * px_m));
            let mut n_approx = 0usize;
            for j in 0..ny {
                for i in 0..nx {
                    let p = gp.point(i, j);
                    let exact = map.lonlat(i as f64 + 0.5, j as f64 + 0.5).unwrap();
                    let Some((lon, lat)) = exact else { continue };
                    let sph = a5_spherical(lon, lat);
                    if let Some(a) = p.approx {
                        n_approx += 1;
                        let f = d.forward(sph, a.origin).unwrap();
                        let err = ((f.x() - a.face.x()).powi(2) + (f.y() - a.face.y()).powi(2)).sqrt();
                        assert!(err <= a.margin, "face err {err} > margin {}", a.margin);
                    }
                    if let Some((alon, alat, m)) = p.lonlat {
                        assert!((alon - lon).abs() <= m && (alat - lat).abs() <= m, "lonlat err > margin");
                    }
                    let mut ex = move || Some(sph);
                    let got = loc.locate(p.approx, &mut ex, &[]);
                    assert_eq!(got, spherical_to_cell(sph, res).unwrap());
                }
            }
            assert!(n_approx > 0, "no window was interpolated");
        }
    }
}
