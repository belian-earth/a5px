//! Cell-driven (centroid) sampling.
//!
//! For each input A5 cell, project the cell centroid into the raster CRS,
//! find the pixel that contains it, and read the per-band value at that
//! pixel. One sample per cell, no aggregation — equivalent to nearest-
//! neighbour resampling onto the A5 grid.
//!
//! Use this when the cell size is comparable to or smaller than the pixel
//! size. The forward (pixel-driven) path leaves gaps in the A5 grid in
//! that regime because each pixel only contributes to the cell containing
//! its centre.
//!
//! Implementation notes:
//! - Cells are grouped by the tile their centroid falls into. We fetch
//!   each tile at most once and sample every cell that landed in it.
//! - Band-aware planar fetching applies here too: when the file is
//!   INTERLEAVE=BAND with predictor=None and a band subset is requested,
//!   only those bands' byte ranges are pulled per tile.
//! - Output uses the same `Output` shape as the forward path so the R
//!   wrappers can stay shared. Stats are degenerate (each cell has one
//!   value): mean = sum = min = max = the sampled value, count = 1.

use std::sync::Arc;

use ahash::AHashMap;
use async_tiff::decoder::DecoderRegistry;
use async_tiff::metadata::TiffMetadataReader;
use async_tiff::metadata::cache::ReadaheadMetadataCache;
use async_tiff::reader::{AsyncFileReader, ObjectReader};
use async_tiff::tags::PlanarConfiguration;
use async_tiff::{TIFF, TypedArray};
use futures::stream::{self, StreamExt, TryStreamExt};
use proj4rs::Proj;
use proj4rs::transform::transform as proj_transform;

use crate::error::{A5CogError, Result};
use crate::geo::{
    build_src_proj, extract_geotransform, is_nodata, parse_band_descriptions, parse_nodata,
};

/// Output mirrors the forward path so the R-side wrappers don't need to
/// branch on mode.
pub(crate) struct CentroidOutput {
    pub cells: Vec<u64>,
    /// Cell-major flat values, length `n_cells * n_bands`. One sample per
    /// (cell, band).
    pub flat: Vec<f64>,
    pub n_bands: usize,
    pub band_names: Vec<String>,
}

/// Resampling kernel for the centroid sample. All kernels are separable;
/// per-band weights are renormalised over the valid (non-nodata, in-raster)
/// stencil pixels, so partial stencils at raster edges or nodata holes stay
/// unbiased instead of propagating NA.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Interp {
    Nearest,
    Bilinear,
    Bicubic,
    Lanczos,
}

impl Interp {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "nearest" => Ok(Self::Nearest),
            "bilinear" => Ok(Self::Bilinear),
            "bicubic" => Ok(Self::Bicubic),
            "lanczos" => Ok(Self::Lanczos),
            other => Err(A5CogError::Invalid(format!(
                "unknown interp: {other:?} (expected nearest/bilinear/bicubic/lanczos)"
            ))),
        }
    }

    /// Stencil offsets relative to `floor(coord - 0.5)` along one axis:
    /// (first_offset, count). Nearest is special-cased to the containing
    /// pixel in `kernel_1d`.
    fn footprint(&self) -> (i64, usize) {
        match self {
            Self::Nearest => (0, 1),
            Self::Bilinear => (0, 2),
            Self::Bicubic => (-1, 4),
            Self::Lanczos => (-2, 6),
        }
    }
}

/// Keys cubic convolution kernel, a = -0.5 (Keys 1981).
#[inline]
fn keys_cubic(x: f64) -> f64 {
    let a = -0.5;
    let x = x.abs();
    if x <= 1.0 {
        (a + 2.0) * x * x * x - (a + 3.0) * x * x + 1.0
    } else if x <= 2.0 {
        a * x * x * x - 5.0 * a * x * x + 8.0 * a * x - 4.0 * a
    } else {
        0.0
    }
}

/// Lanczos-3 kernel.
#[inline]
fn lanczos3(x: f64) -> f64 {
    const A: f64 = 3.0;
    let x = x.abs();
    if x < 1e-12 {
        return 1.0;
    }
    if x >= A {
        return 0.0;
    }
    let pix = std::f64::consts::PI * x;
    A * pix.sin() * (pix / A).sin() / (pix * pix)
}

/// One axis of separable kernel weights at fractional pixel coordinate
/// `coord` (integer values on pixel corners). Returns the first stencil
/// pixel index along the axis and the number of weights written; weights
/// are normalised to sum to 1.
#[inline]
fn kernel_1d(interp: Interp, coord: f64, w: &mut [f64; 6]) -> (i64, usize) {
    if interp == Interp::Nearest {
        w[0] = 1.0;
        return (coord.floor() as i64, 1);
    }
    // pixel (i) holds the value at centre i + 0.5; base is the last pixel
    // whose centre is at or left of the sample point
    let u = coord - 0.5;
    let b = u.floor();
    let t = u - b;
    let (off0, n) = interp.footprint();
    match interp {
        Interp::Bilinear => {
            w[0] = 1.0 - t;
            w[1] = t;
        }
        Interp::Bicubic => {
            for (j, wj) in w.iter_mut().take(n).enumerate() {
                *wj = keys_cubic(t - (off0 + j as i64) as f64);
            }
        }
        Interp::Lanczos => {
            for (j, wj) in w.iter_mut().take(n).enumerate() {
                *wj = lanczos3(t - (off0 + j as i64) as f64);
            }
        }
        Interp::Nearest => unreachable!(),
    }
    let tot: f64 = w[..n].iter().sum();
    for wj in w[..n].iter_mut() {
        *wj /= tot;
    }
    (b as i64 + off0, n)
}

/// First stencil pixel index and stencil length along one axis, matching
/// what `kernel_1d` will produce for the same coordinate.
#[inline]
fn stencil_start(interp: Interp, coord: f64) -> (i64, usize) {
    if interp == Interp::Nearest {
        return (coord.floor() as i64, 1);
    }
    let (off0, n) = interp.footprint();
    ((coord - 0.5).floor() as i64 + off0, n)
}

/// Item handed from the I/O producer to the CPU consumer pool. Carries the
/// fetched-but-not-yet-decoded payload plus the list of cells whose stencil
/// touches this tile (cell index in the original input + the fractional
/// pixel coordinates of the cell centroid). A stencil crossing tile edges
/// appears in several tiles; each worker accumulates the partial weighted
/// sums for the pixels it owns and the merge is additive.
struct TileWork {
    tx: usize,
    ty: usize,
    payload: crate::read::TilePayload,
    entries: Vec<(usize, f64, f64)>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn sample_at_cells_async(
    src: &str,
    cells_in: Vec<u64>,
    bands_idx: Vec<i32>,
    bands_names: Vec<String>,
    src_nodata_override: Option<f64>,
    cpu_workers: usize,
    io_concurrency: usize,
    dequant: Option<Arc<crate::read::DequantLut>>,
    interp: Interp,
) -> Result<CentroidOutput> {
    let (store, path) = crate::read::parse_src_pub(src)?;
    let reader = ObjectReader::new(store, path);
    let cache = ReadaheadMetadataCache::new(reader.clone());
    let mut meta = TiffMetadataReader::try_open(&cache).await?;
    let ifds = meta.read_all_ifds(&cache).await?;
    let endianness = meta.endianness();
    let tiff = TIFF::new(ifds, endianness);
    let ifd_owned = tiff
        .ifds()
        .first()
        .ok_or_else(|| A5CogError::Invalid("no IFDs".into()))?
        .clone();

    if let Some(dq) = dequant.as_deref() {
        crate::read::validate_dequant_dtype(&ifd_owned, dq)?;
    }

    let geo = ifd_owned
        .geo_key_directory()
        .ok_or(A5CogError::MissingGeoKey("GeoKeyDirectory"))?;
    let src_proj = build_src_proj(geo)?;
    let dst_proj = Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs")?;

    let gt = extract_geotransform(&ifd_owned)?;
    if gt.0[2] != 0.0 || gt.0[4] != 0.0 {
        return Err(A5CogError::Unsupported(
            "rotated geotransform is not supported in centroid mode".into(),
        ));
    }

    let width = ifd_owned.image_width() as usize;
    let height = ifd_owned.image_height() as usize;
    let n_bands = ifd_owned.samples_per_pixel() as usize;
    let planar = ifd_owned.planar_configuration();
    let (tile_w, tile_h) = match (ifd_owned.tile_width(), ifd_owned.tile_height()) {
        (Some(w), Some(h)) => (w as usize, h as usize),
        _ => {
            return Err(A5CogError::Unsupported(
                "centroid mode requires a tiled TIFF".into(),
            ));
        }
    };

    let nodata = src_nodata_override.or(parse_nodata(&ifd_owned));
    let band_names_v = parse_band_descriptions(&ifd_owned, n_bands);
    let all_band_names: Vec<String> = if band_names_v.is_empty() {
        (0..n_bands).map(|i| format!("band_{:02}", i + 1)).collect()
    } else {
        band_names_v
    };

    // resolve band selection to 0-based indices (mirrors read.rs)
    let selected_bands: Vec<usize> = if !bands_idx.is_empty() {
        bands_idx
            .iter()
            .map(|&i| {
                if i < 1 || (i as usize) > n_bands {
                    Err(A5CogError::Invalid(format!(
                        "band index {i} out of range 1..={n_bands}"
                    )))
                } else {
                    Ok((i - 1) as usize)
                }
            })
            .collect::<Result<Vec<_>>>()?
    } else if !bands_names.is_empty() {
        bands_names
            .iter()
            .map(|name| {
                all_band_names
                    .iter()
                    .position(|d| d == name)
                    .ok_or_else(|| {
                        A5CogError::Invalid(format!(
                            "band name {name:?} not found; available: {all_band_names:?}"
                        ))
                    })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        (0..n_bands).collect()
    };
    let n_out = selected_bands.len();
    let band_names: Vec<String> = selected_bands.iter().map(|&i| all_band_names[i].clone()).collect();

    // Group cells by every tile their stencil touches.
    // bucket_entry = (output_index_in_cells_in, col_f, row_f)
    let mut buckets: AHashMap<(usize, usize), Vec<(usize, f64, f64)>> =
        AHashMap::with_capacity(cells_in.len() / 16 + 1);

    let dst_is_latlong = dst_proj.is_latlong();
    let src_is_latlong = src_proj.is_latlong();
    let inv_gt1 = 1.0 / gt.0[1];
    let inv_gt5 = 1.0 / gt.0[5];

    for (i, &cell) in cells_in.iter().enumerate() {
        let ll = match a5::cell_to_lonlat(cell) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mut p = (ll.longitude(), ll.latitude(), 0.0);
        // dst_proj is +longlat → convert deg → rad for the inverse
        if dst_is_latlong {
            p.0 = p.0.to_radians();
            p.1 = p.1.to_radians();
        }
        // forward direction here: WGS84 (dst) → raster CRS (src)
        if proj_transform(&dst_proj, &src_proj, &mut p).is_err() {
            continue;
        }
        if src_is_latlong {
            p.0 = p.0.to_degrees();
            p.1 = p.1.to_degrees();
        }
        let (x, y) = (p.0, p.1);
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        let col = (x - gt.0[0]) * inv_gt1;
        let row = (y - gt.0[3]) * inv_gt5;
        // the centroid itself must lie inside the raster (as for nearest);
        // stencil pixels beyond the edge are skipped and renormalised away
        if col < 0.0 || row < 0.0 || col >= width as f64 || row >= height as f64 {
            continue;
        }
        let (sx, nx) = stencil_start(interp, col);
        let (sy, ny) = stencil_start(interp, row);
        let cx_lo = sx.max(0) as usize;
        let cx_hi = (sx + nx as i64 - 1).min(width as i64 - 1) as usize;
        let cy_lo = sy.max(0) as usize;
        let cy_hi = (sy + ny as i64 - 1).min(height as i64 - 1) as usize;
        if cx_lo > cx_hi || cy_lo > cy_hi {
            continue;
        }
        for ty in (cy_lo / tile_h)..=(cy_hi / tile_h) {
            for tx in (cx_lo / tile_w)..=(cx_hi / tile_w) {
                buckets.entry((tx, ty)).or_default().push((i, col, row));
            }
        }
    }

    if buckets.is_empty() {
        return Ok(CentroidOutput {
            cells: Vec::new(),
            flat: Vec::new(),
            n_bands: n_out,
            band_names,
        });
    }

    let n_in = cells_in.len();

    let use_band_fetch = matches!(planar, PlanarConfiguration::Planar)
        && n_out < n_bands
        && matches!(
            ifd_owned.predictor(),
            None | Some(async_tiff::tags::Predictor::None)
        );
    let identity_offsets: Vec<usize> = (0..n_out).collect();

    let ifd_arc = Arc::new(ifd_owned);
    let registry_arc: Arc<DecoderRegistry> = Arc::new(DecoderRegistry::default());
    let selected_bands_arc: Arc<Vec<usize>> = Arc::new(selected_bands);
    let identity_offsets_arc: Arc<Vec<usize>> = Arc::new(identity_offsets);

    let tasks: Vec<((usize, usize), Vec<(usize, f64, f64)>)> = buckets.into_iter().collect();

    // Producer-consumer pipeline (mirrors read.rs):
    // - Producer fetches each tile's bytes (with `io_concurrency` in flight).
    // - `cpu_workers` blocking-pool consumers decode + sample into per-worker
    //   buffers, then we merge their outputs once everyone exits.
    let channel_depth = (cpu_workers * 2).max(4);
    let (tx_chan_outer, rx_chan) = async_channel::bounded::<TileWork>(channel_depth);

    let mut consumer_handles: Vec<
        tokio::task::JoinHandle<Result<(Vec<f64>, Vec<f64>)>>,
    > = Vec::with_capacity(cpu_workers);
    for _ in 0..cpu_workers {
        let rx = rx_chan.clone();
        let ifd = Arc::clone(&ifd_arc);
        let registry = Arc::clone(&registry_arc);
        let identity_offsets = Arc::clone(&identity_offsets_arc);
        let selected_bands = Arc::clone(&selected_bands_arc);
        let dequant_c = dequant.clone();
        consumer_handles.push(tokio::task::spawn_blocking(move || {
            // partial weighted sums and weight totals per (cell, band);
            // the cross-worker merge is a plain elementwise addition
            let mut sums = vec![0.0f64; n_in * n_out];
            let mut wsums = vec![0.0f64; n_in * n_out];
            let mut wx = [0.0f64; 6];
            let mut wy = [0.0f64; 6];
            while let Ok(work) = rx.recv_blocking() {
                let (data, _shape, data_n_bands_eff, offsets_arc): (
                    TypedArray,
                    [usize; 3],
                    usize,
                    Arc<Vec<usize>>,
                ) = match work.payload {
                    crate::read::TilePayload::PlanarSubset(bytes) => {
                        let (typed, sh) = crate::band_fetch::decode_planar_subset_bytes(
                            bytes, &ifd, &registry,
                        )?;
                        (typed, sh, n_out, Arc::clone(&identity_offsets))
                    }
                    crate::read::TilePayload::Full(tile) => {
                        let arr = tile.decode(&registry)?;
                        let (data, sh, _) = arr.into_inner();
                        (data, sh, n_bands, Arc::clone(&selected_bands))
                    }
                };
                let (h_stride, w_stride, b_stride): (usize, usize, usize) = match planar {
                    PlanarConfiguration::Chunky => {
                        (tile_w * data_n_bands_eff, data_n_bands_eff, 1)
                    }
                    PlanarConfiguration::Planar => (tile_w, 1, tile_h * tile_w),
                    other => {
                        return Err(A5CogError::Unsupported(format!(
                            "unhandled planar configuration: {other:?}"
                        )));
                    }
                };
                let tile_x0 = work.tx * tile_w;
                let tile_y0 = work.ty * tile_h;
                let actual_w = tile_w.min(width.saturating_sub(tile_x0));
                let actual_h = tile_h.min(height.saturating_sub(tile_y0));
                for (i, col, row) in work.entries {
                    let (sx, nx) = kernel_1d(interp, col, &mut wx);
                    let (sy, ny) = kernel_1d(interp, row, &mut wy);
                    let base = i * n_out;
                    for jy in 0..ny {
                        let gy = sy + jy as i64;
                        if gy < tile_y0 as i64 || gy >= (tile_y0 + actual_h) as i64 {
                            continue;
                        }
                        let r = gy as usize - tile_y0;
                        for jx in 0..nx {
                            let gx = sx + jx as i64;
                            if gx < tile_x0 as i64 || gx >= (tile_x0 + actual_w) as i64 {
                                continue;
                            }
                            let c = gx as usize - tile_x0;
                            let wgt = wx[jx] * wy[jy];
                            if wgt == 0.0 {
                                continue;
                            }
                            let pixel_base = r * h_stride + c * w_stride;
                            for (out_b, &src_b) in offsets_arc.iter().enumerate() {
                                let off = pixel_base + src_b * b_stride;
                                let raw = crate::read::read_pixel_chunky_pub(&data, off);
                                // nodata compares the raw code; dequant
                                // decodes each stencil pixel BEFORE the
                                // kernel (nonlinear decodes do not commute
                                // with interpolation)
                                let valid_v = match nodata {
                                    Some(nd) => !is_nodata(raw, nd),
                                    None => true,
                                };
                                if valid_v {
                                    let v = match dequant_c.as_deref() {
                                        Some(d) => d.apply(raw),
                                        None => raw,
                                    };
                                    sums[base + out_b] += wgt * v;
                                    wsums[base + out_b] += wgt;
                                }
                            }
                        }
                    }
                }
            }
            Ok::<(Vec<f64>, Vec<f64>), A5CogError>((sums, wsums))
        }));
    }
    drop(rx_chan);

    {
        let reader = reader.clone();
        let ifd = Arc::clone(&ifd_arc);
        let selected_bands = Arc::clone(&selected_bands_arc);
        let tx_chan = tx_chan_outer;
        let producer = stream::iter(tasks)
            .map(|((tx, ty), entries)| {
                let reader = reader.clone();
                let ifd = Arc::clone(&ifd);
                let selected_bands = Arc::clone(&selected_bands);
                let tx_chan = tx_chan.clone();
                async move {
                    let payload = if use_band_fetch {
                        let bytes = crate::band_fetch::fetch_planar_subset_bytes(
                            &reader as &dyn AsyncFileReader,
                            &ifd,
                            tx,
                            ty,
                            &selected_bands,
                        )
                        .await?;
                        crate::read::TilePayload::PlanarSubset(bytes)
                    } else {
                        let tile = ifd
                            .fetch_tile(tx, ty, &reader as &dyn AsyncFileReader)
                            .await?;
                        crate::read::TilePayload::Full(tile)
                    };
                    tx_chan
                        .send(TileWork { tx, ty, payload, entries })
                        .await
                        .map_err(|_| {
                            A5CogError::Invalid("centroid consumer pool dropped channel".into())
                        })?;
                    Ok::<(), A5CogError>(())
                }
            })
            .buffer_unordered(io_concurrency.max(1))
            .try_collect::<Vec<()>>();
        // tx_chan moved in; end-of-scope drops it and closes the channel.
        let producer_result = producer.await;
        if let Err(e) = producer_result {
            for h in &consumer_handles {
                h.abort();
            }
            return Err(e);
        }
    }

    // Merge per-worker partials by addition: a stencil split across tiles
    // (and therefore across workers) sums back to the full kernel. Collect
    // all worker results before short-circuiting so a single failure
    // doesn't detach the remaining workers.
    let mut consumer_results: Vec<Result<(Vec<f64>, Vec<f64>)>> =
        Vec::with_capacity(cpu_workers);
    for h in consumer_handles {
        match h.await {
            Ok(inner) => consumer_results.push(inner),
            Err(join_err) => consumer_results.push(Err(A5CogError::WorkerJoin(format!(
                "centroid-consumer worker: {join_err}"
            )))),
        }
    }
    let mut sums = vec![0.0f64; n_in * n_out];
    let mut wsums = vec![0.0f64; n_in * n_out];
    for r in consumer_results {
        let (s, w) = r?;
        for (acc, v) in sums.iter_mut().zip(s) {
            *acc += v;
        }
        for (acc, v) in wsums.iter_mut().zip(w) {
            *acc += v;
        }
    }

    // Per-band weight renormalisation: nodata or off-raster stencil pixels
    // contribute nothing, and the remaining weights rescale to 1. A band
    // whose total weight is ~0 (all stencil pixels invalid) is NA; cells
    // with no valid band are dropped, matching the nearest-only behaviour.
    const MIN_W: f64 = 1e-9;
    let mut cells_out = Vec::new();
    let mut flat_out: Vec<f64> = Vec::new();
    for i in 0..n_in {
        let base = i * n_out;
        if !(0..n_out).any(|b| wsums[base + b] > MIN_W) {
            continue;
        }
        cells_out.push(cells_in[i]);
        for b in 0..n_out {
            flat_out.push(if wsums[base + b] > MIN_W {
                sums[base + b] / wsums[base + b]
            } else {
                f64::NAN
            });
        }
    }

    Ok(CentroidOutput {
        cells: cells_out,
        flat: flat_out,
        n_bands: n_out,
        band_names,
    })
}
