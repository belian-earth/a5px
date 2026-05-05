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
use std::sync::Mutex as StdMutex;

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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn sample_at_cells_async(
    src: &str,
    cells_in: Vec<u64>,
    bands_idx: Vec<i32>,
    bands_names: Vec<String>,
    src_nodata_override: Option<f64>,
    io_concurrency: usize,
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

    // Group cells by the tile their centroid falls into.
    // bucket_entry = (output_index_in_cells_in, col_in_tile, row_in_tile)
    let mut buckets: AHashMap<(usize, usize), Vec<(usize, usize, usize)>> =
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
        if col < 0.0 || row < 0.0 {
            continue;
        }
        let col_u = col.floor() as usize;
        let row_u = row.floor() as usize;
        if col_u >= width || row_u >= height {
            continue;
        }
        let tx = col_u / tile_w;
        let ty = row_u / tile_h;
        let col_in_tile = col_u - tx * tile_w;
        let row_in_tile = row_u - ty * tile_h;
        buckets
            .entry((tx, ty))
            .or_default()
            .push((i, col_in_tile, row_in_tile));
    }

    if buckets.is_empty() {
        return Ok(CentroidOutput {
            cells: Vec::new(),
            flat: Vec::new(),
            n_bands: n_out,
            band_names,
        });
    }

    // Pre-allocate output. We track validity so we can drop cells that fell
    // outside the raster or hit nodata in every band.
    let n_in = cells_in.len();
    let flat = Arc::new(StdMutex::new(vec![f64::NAN; n_in * n_out]));
    let valid = Arc::new(StdMutex::new(vec![false; n_in]));

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

    let tasks: Vec<((usize, usize), Vec<(usize, usize, usize)>)> = buckets.into_iter().collect();

    stream::iter(tasks)
        .map(|((tx, ty), entries)| {
            let reader = reader.clone();
            let ifd = Arc::clone(&ifd_arc);
            let registry = Arc::clone(&registry_arc);
            let selected_bands = Arc::clone(&selected_bands_arc);
            let identity_offsets = Arc::clone(&identity_offsets_arc);
            let flat = Arc::clone(&flat);
            let valid = Arc::clone(&valid);
            async move {
                let (data, _shape, data_n_bands_eff, offsets_arc): (
                    TypedArray,
                    [usize; 3],
                    usize,
                    Arc<Vec<usize>>,
                ) = if use_band_fetch {
                    let (typed, sh) = crate::band_fetch::fetch_planar_subset(
                        &reader as &dyn AsyncFileReader,
                        &ifd,
                        tx,
                        ty,
                        &selected_bands,
                        &registry,
                    )
                    .await?;
                    (typed, sh, n_out, Arc::clone(&identity_offsets))
                } else {
                    let tile = ifd
                        .fetch_tile(tx, ty, &reader as &dyn AsyncFileReader)
                        .await?;
                    let arr = tile.decode(&registry)?;
                    let (data, sh, _dt) = arr.into_inner();
                    (data, sh, n_bands, Arc::clone(&selected_bands))
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

                let mut local_flat = vec![(0usize, [0.0f64; 0])]; // placeholder, we write straight to global
                let _ = &mut local_flat;
                let mut updates: Vec<(usize, Vec<f64>)> = Vec::with_capacity(entries.len());
                let mut valid_updates: Vec<usize> = Vec::with_capacity(entries.len());
                for (i, c, r) in entries {
                    let pixel_base = r * h_stride + c * w_stride;
                    let mut row = vec![f64::NAN; n_out];
                    let mut any_valid = false;
                    for (out_b, &src_b) in offsets_arc.iter().enumerate() {
                        let off = pixel_base + src_b * b_stride;
                        let v = crate::read::read_pixel_chunky_pub(&data, off);
                        let valid_v = match nodata {
                            Some(nd) => !is_nodata(v, nd),
                            None => true,
                        };
                        if valid_v {
                            row[out_b] = v;
                            any_valid = true;
                        }
                    }
                    if any_valid {
                        updates.push((i, row));
                        valid_updates.push(i);
                    }
                }
                {
                    let mut g = flat.lock().unwrap();
                    for (i, row) in updates {
                        let base = i * n_out;
                        for (b, v) in row.into_iter().enumerate() {
                            g[base + b] = v;
                        }
                    }
                }
                {
                    let mut v = valid.lock().unwrap();
                    for i in valid_updates {
                        v[i] = true;
                    }
                }
                Ok::<(), A5CogError>(())
            }
        })
        .buffer_unordered(io_concurrency.max(1))
        .try_collect::<Vec<()>>()
        .await?;

    // Filter to cells with at least one valid band reading.
    let flat_full = Arc::try_unwrap(flat)
        .map_err(|_| A5CogError::Invalid("residual reference to flat".into()))?
        .into_inner()
        .unwrap();
    let valid_v = Arc::try_unwrap(valid)
        .map_err(|_| A5CogError::Invalid("residual reference to valid".into()))?
        .into_inner()
        .unwrap();

    let kept: usize = valid_v.iter().filter(|&&b| b).count();
    let mut cells_out = Vec::with_capacity(kept);
    let mut flat_out: Vec<f64> = Vec::with_capacity(kept * n_out);
    for (i, &is_valid) in valid_v.iter().enumerate() {
        if !is_valid {
            continue;
        }
        cells_out.push(cells_in[i]);
        let base = i * n_out;
        flat_out.extend_from_slice(&flat_full[base..base + n_out]);
    }

    Ok(CentroidOutput {
        cells: cells_out,
        flat: flat_out,
        n_bands: n_out,
        band_names,
    })
}
