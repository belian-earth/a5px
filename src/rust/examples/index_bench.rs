//! Micro-benchmark for per-pixel A5 indexing strategies on a raster scan.
//!
//! Simulates a tile of square pixels (metres) in lon/lat scan order and
//! times three lookups, verifying they agree:
//!   current   : a5px 0.1.0 path (deserialised prev cell + a5cell_contains_point
//!               which recomputes the pentagon, then spherical_to_cell)
//!   crate     : a5::core::cell::spherical_to_cell alone (internal 1-entry cache)
//!   locator   : a5px::locator::CellLocator (neighbour-first, cached pentagons)
//!
//! usage: index_bench [resolution] [pixel_m] [tile]

use a5::coordinate_systems::Spherical;
use a5::core::cell::{a5cell_contains_point, spherical_to_cell};
use a5::core::serialization::deserialize;
use a5px::locator::{a5_spherical as sph, CellLocator, NO_CELL};
use ahash::AHashMap;
use std::time::Instant;

// --- strategy A: current a5px path
fn run_current(pts: &[Spherical], res: i32, out: &mut Vec<u64>) {
    let mut last_cell: Option<u64> = None;
    let mut last_a5: Option<a5::A5Cell> = None;
    for &p in pts {
        let cell = if let (Some(prev_id), Some(prev)) = (last_cell, last_a5.as_ref()) {
            match a5cell_contains_point(prev, p) {
                Ok(d) if d > 0.0 => prev_id,
                _ => match spherical_to_cell(p, res) {
                    Ok(id) => {
                        last_a5 = deserialize(id).ok();
                        id
                    }
                    Err(_) => NO_CELL,
                },
            }
        } else {
            match spherical_to_cell(p, res) {
                Ok(id) => {
                    last_a5 = deserialize(id).ok();
                    id
                }
                Err(_) => NO_CELL,
            }
        };
        last_cell = Some(cell);
        out.push(cell);
    }
}

// --- strategy B: crate only
fn run_crate(pts: &[Spherical], res: i32, out: &mut Vec<u64>) {
    for &p in pts {
        out.push(spherical_to_cell(p, res).unwrap_or(NO_CELL));
    }
}

// --- strategy C: a5px's neighbour-first locator (src/locator.rs)
fn run_locator(pts: &[Spherical], res: i32, w: usize, out: &mut Vec<u64>) {
    let mut loc = CellLocator::new(res);
    let mut prev_row: Vec<u64> = vec![NO_CELL; w];
    for (i, &p) in pts.iter().enumerate() {
        let c = i % w;
        let id = loc.locate(p, prev_row[c]);
        prev_row[c] = id;
        out.push(id);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let res: i32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(18);
    let pixel_m: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10.0);
    let tile: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1024);

    // Sabah-ish, matches the issue's partition
    let lon0 = 117.0;
    let lat0: f64 = 5.5;
    let dlat = pixel_m / 111_320.0;
    let dlon = pixel_m / (111_320.0 * lat0.to_radians().cos());
    let n = tile * tile;
    let mut pts: Vec<Spherical> = Vec::with_capacity(n);
    for r in 0..tile {
        let lat = lat0 - (r as f64 + 0.5) * dlat;
        for c in 0..tile {
            let lon = lon0 + (c as f64 + 0.5) * dlon;
            pts.push(sph(lon, lat));
        }
    }
    eprintln!("res {res}, {pixel_m} m pixels, {tile}x{tile} tile, {n} points");

    let mut a = Vec::with_capacity(n);
    let t = Instant::now();
    run_current(&pts, res, &mut a);
    let ta = t.elapsed();
    let mut b = Vec::with_capacity(n);
    let t = Instant::now();
    run_crate(&pts, res, &mut b);
    let tb = t.elapsed();
    let mut c = Vec::with_capacity(n);
    let t = Instant::now();
    run_locator(&pts, res, tile, &mut c);
    let tc = t.elapsed();

    let mut distinct = AHashMap::new();
    for &id in &b {
        *distinct.entry(id).or_insert(0u32) += 1;
    }
    let mism_ab = a.iter().zip(&b).filter(|(x, y)| x != y).count();
    let mism_cb = c.iter().zip(&b).filter(|(x, y)| x != y).count();
    let ns = |d: std::time::Duration| d.as_nanos() as f64 / n as f64;
    println!("distinct cells {} ({:.2} px/cell)", distinct.len(), n as f64 / distinct.len() as f64);
    println!("current : {:>8.0} ns/px   mismatch vs crate {}", ns(ta), mism_ab);
    println!("crate   : {:>8.0} ns/px", ns(tb));
    println!("locator : {:>8.0} ns/px   mismatch vs crate {}", ns(tc), mism_cb);
}
