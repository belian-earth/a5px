//! Per-primitive costs on the pixel -> cell path (one thread, ns per call).
use a5::core::cell::{get_pentagon, spherical_to_cell};
use a5::core::serialization::deserialize;
use a5::projections::dodecahedron::DodecahedronProjection;
use a5::traversal::global_neighbors::get_global_cell_neighbors;
use a5px::locator::{a5_spherical, CellLocator, NO_CELL};
use ahash::AHashMap;
use std::time::Instant;

fn main() {
    let res: i32 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(18);
    let tile = 1024usize;
    let n = tile * tile;
    // UTM 50N grid, 10 m, matches the synthetic AEF file
    let src = proj4rs::Proj::from_epsg_code(32650).unwrap();
    let dst = proj4rs::Proj::from_epsg_code(4326).unwrap();
    let mut pts: Vec<(f64, f64, f64)> = Vec::with_capacity(n);
    for r in 0..tile { for c in 0..tile { pts.push((720000.0 + 10.0 * (c as f64 + 0.5), 608000.0 - 10.0 * (r as f64 + 0.5), 0.0)); } }
    let ns = |t: Instant, k: usize| t.elapsed().as_nanos() as f64 / k as f64;

    let t = Instant::now();
    for p in pts.iter_mut() { proj4rs::transform::transform(&src, &dst, p).unwrap(); }
    println!("{:<44} {:>7.0} ns", "proj4rs UTM->lonlat (per point)", ns(t, n));
    let ll: Vec<(f64, f64)> = pts.iter().map(|p| (p.0.to_degrees(), p.1.to_degrees())).collect();

    let t = Instant::now();
    let sph: Vec<_> = ll.iter().map(|&(lo, la)| a5_spherical(lo, la)).collect();
    println!("{:<44} {:>7.0} ns", "from_lon_lat (authalic + rotate)", ns(t, n));

    // cells (truth) and distinct set
    let cells: Vec<u64> = sph.iter().map(|&p| spherical_to_cell(p, res).unwrap()).collect();
    let mut distinct: Vec<u64> = cells.clone(); distinct.sort_unstable(); distinct.dedup();
    println!("{:<44} {:>7}", "distinct cells", distinct.len());

    let c0 = deserialize(cells[0]).unwrap();
    let t = Instant::now();
    let d = DodecahedronProjection::get_thread_local();
    let mut acc = 0.0;
    for &p in &sph { let f = d.forward(p, c0.origin_id).unwrap(); acc += f.x(); }
    println!("{:<44} {:>7.0} ns   ({acc:.3})", "dodecahedron forward (per point)", ns(t, n));

    let pent = get_pentagon(&c0).unwrap();
    let f0 = d.forward(sph[0], c0.origin_id).unwrap();
    let t = Instant::now();
    let mut inside = 0u64;
    for _ in 0..n { if pent.contains_point(f0) > 0.0 { inside += 1; } }
    println!("{:<44} {:>7.0} ns   ({inside})", "PentagonShape::contains_point (crate)", ns(t, n));

    let t = Instant::now();
    let mut k = 0usize;
    for &id in &distinct { let c = deserialize(id).unwrap(); let p = get_pentagon(&c).unwrap(); k += p.get_vertices_vec().len(); }
    println!("{:<44} {:>7.0} ns   ({k})", "deserialize + get_pentagon (per cell)", ns(t, distinct.len()));

    let t = Instant::now();
    let mut k = 0usize;
    for &id in &distinct { k += get_global_cell_neighbors(id, false).len(); }
    println!("{:<44} {:>7.0} ns   (avg {:.1} nbrs)", "get_global_cell_neighbors (per cell)", ns(t, distinct.len()), k as f64 / distinct.len() as f64);

    // cold search: defeat the crate cache by alternating far-apart points
    let far: Vec<_> = (0..n).map(|i| sph[(i * 7919) % n]).collect();
    let t = Instant::now();
    let mut k = 0u64;
    for &p in &far { k ^= spherical_to_cell(p, res).unwrap(); }
    println!("{:<44} {:>7.0} ns   ({k})", "spherical_to_cell cold (per point)", ns(t, n));

    let t = Instant::now();
    let mut loc = CellLocator::new(res, a5::cell_area(res) / 100.0); let mut prev = vec![NO_CELL; tile]; let mut k = 0u64;
    for (i, &p) in sph.iter().enumerate() { let c = i % tile; let mut h = [NO_CELL; 5]; h[0] = prev[c]; if c + 1 < tile { h[1] = prev[c + 1]; } if c + 2 < tile { h[2] = prev[c + 2]; } if c + 3 < tile { h[3] = prev[c + 3]; } if c >= 1 { h[4] = prev[c - 1]; } let id = loc.locate_exact(p, &h); prev[c] = id; k ^= id; }
    println!("{:<44} {:>7.0} ns   ({k})", "CellLocator::locate (per point)", ns(t, n));

    let mut m: AHashMap<u64, usize> = AHashMap::with_capacity(distinct.len() * 2);
    for (i, &id) in distinct.iter().enumerate() { m.insert(id, i); }
    let t = Instant::now();
    let mut k = 0usize;
    for &id in &cells { k += m[&id]; }
    println!("{:<44} {:>7.0} ns   ({k})", "AHashMap get, {} keys (per lookup)", ns(t, n));
}
