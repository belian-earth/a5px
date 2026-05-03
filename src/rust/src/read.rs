//! Forward pixel-driven raster → A5 cell aggregation.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use ahash::AHashMap;
use async_tiff::decoder::DecoderRegistry;
use async_tiff::metadata::TiffMetadataReader;
use async_tiff::metadata::cache::ReadaheadMetadataCache;
use async_tiff::reader::{AsyncFileReader, ObjectReader};
use async_tiff::tags::PlanarConfiguration;
use async_tiff::{TIFF, TypedArray};
use extendr_api::prelude::*;
use futures::stream::{self, StreamExt, TryStreamExt};
use object_store::ObjectStore;
use object_store::path::Path as ObjPath;
use proj4rs::Proj;
use proj4rs::transform::transform as proj_transform;

use crate::cell_raw::u64s_to_raw8_list;
use crate::error::{A5CogError, Result};
use crate::geo::{GeoTransform, extract_geotransform, parse_band_descriptions, parse_nodata};

// ---------------------------------------------------------------------------
// stat selector

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stat {
    Mean,
    Sum,
    Count,
    Min,
    Max,
}

impl Stat {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "mean" => Ok(Self::Mean),
            "sum" => Ok(Self::Sum),
            "count" => Ok(Self::Count),
            "min" => Ok(Self::Min),
            "max" => Ok(Self::Max),
            other => Err(A5CogError::Invalid(format!("unknown stat: {other}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// per-cell, per-band running accumulator

#[derive(Clone, Copy, Debug)]
struct Accum {
    sum: f64,
    count: u64,
    min: f64,
    max: f64,
}

impl Accum {
    #[inline]
    fn new() -> Self {
        Self {
            sum: 0.0,
            count: 0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        }
    }
    #[inline]
    fn push(&mut self, v: f64) {
        self.sum += v;
        self.count += 1;
        if v < self.min {
            self.min = v;
        }
        if v > self.max {
            self.max = v;
        }
    }
    #[inline]
    fn merge(&mut self, other: &Self) {
        self.sum += other.sum;
        self.count += other.count;
        if other.min < self.min {
            self.min = other.min;
        }
        if other.max > self.max {
            self.max = other.max;
        }
    }
    #[inline]
    fn finalise(&self, stat: Stat) -> f64 {
        match stat {
            Stat::Mean => {
                if self.count == 0 {
                    f64::NAN
                } else {
                    self.sum / self.count as f64
                }
            }
            Stat::Sum => self.sum,
            Stat::Count => self.count as f64,
            Stat::Min => {
                if self.count == 0 {
                    f64::NAN
                } else {
                    self.min
                }
            }
            Stat::Max => {
                if self.count == 0 {
                    f64::NAN
                } else {
                    self.max
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// src parsing

fn parse_src(src: &str) -> Result<(Arc<dyn ObjectStore>, ObjPath)> {
    let has_scheme = ["http://", "https://", "s3://", "gs://", "az://", "abfs://", "file://"]
        .iter()
        .any(|p| src.starts_with(p));
    if has_scheme {
        let url = url::Url::parse(src)?;
        let (store, path) = object_store::parse_url(&url)
            .map_err(|e| A5CogError::Invalid(format!("parse_url: {e}")))?;
        return Ok((Arc::from(store), path));
    }
    let p = std::path::Path::new(src);
    if !p.exists() {
        return Err(A5CogError::Invalid(format!("file not found: {src}")));
    }
    let abs = p.canonicalize()?;
    let parent = abs
        .parent()
        .unwrap_or_else(|| std::path::Path::new("/"))
        .to_path_buf();
    let fname = abs
        .file_name()
        .ok_or_else(|| A5CogError::Invalid("path has no file name".into()))?
        .to_string_lossy()
        .to_string();
    let lfs = object_store::local::LocalFileSystem::new_with_prefix(parent)?;
    let store: Arc<dyn ObjectStore> = Arc::new(lfs);
    let path = ObjPath::from(fname.as_str());
    Ok((store, path))
}

// ---------------------------------------------------------------------------
// pixel sampling helpers — band-major access into the decoded tile

fn read_pixel_chunky(data: &TypedArray, idx: usize) -> f64 {
    match data {
        TypedArray::UInt8(v) => v[idx] as f64,
        TypedArray::UInt16(v) => v[idx] as f64,
        TypedArray::UInt32(v) => v[idx] as f64,
        TypedArray::UInt64(v) => v[idx] as f64,
        TypedArray::Int8(v) => v[idx] as f64,
        TypedArray::Int16(v) => v[idx] as f64,
        TypedArray::Int32(v) => v[idx] as f64,
        TypedArray::Int64(v) => v[idx] as f64,
        TypedArray::Float32(v) => v[idx] as f64,
        TypedArray::Float64(v) => v[idx],
        TypedArray::Bool(v) => {
            if v[idx] {
                1.0
            } else {
                0.0
            }
        }
    }
}

// ---------------------------------------------------------------------------
// per-tile processor

#[allow(clippy::too_many_arguments)]
fn process_tile(
    tx: usize,
    ty: usize,
    data: TypedArray,
    shape: [usize; 3],
    planar: PlanarConfiguration,
    width: usize,
    height: usize,
    tile_w: usize,
    tile_h: usize,
    n_bands: usize,
    src_proj: &Proj,
    dst_proj: &Proj,
    gt: &GeoTransform,
    resolution: i32,
    nodata: Option<f64>,
) -> Result<AHashMap<u64, Vec<Accum>>> {
    let actual_w = tile_w.min(width.saturating_sub(tx * tile_w));
    let actual_h = tile_h.min(height.saturating_sub(ty * tile_h));
    if actual_w == 0 || actual_h == 0 {
        return Ok(AHashMap::new());
    }

    let src_is_latlong = src_proj.is_latlong();
    let dst_is_latlong = dst_proj.is_latlong();

    // build per-pixel projected coords, transform en-masse
    let n = actual_w * actual_h;
    let mut points: Vec<(f64, f64, f64)> = Vec::with_capacity(n);
    for r in 0..actual_h {
        let row_g = ty * tile_h + r;
        for c in 0..actual_w {
            let col_g = tx * tile_w + c;
            let (mut x, mut y) = gt.pixel_centre(col_g, row_g);
            if src_is_latlong {
                x = x.to_radians();
                y = y.to_radians();
            }
            points.push((x, y, 0.0));
        }
    }

    proj_transform(src_proj, dst_proj, &mut points[..])?;

    let mut local: AHashMap<u64, Vec<Accum>> = AHashMap::with_capacity(n / 4);

    // shape interpretation
    // chunky: shape = [tile_h, tile_w, n_bands]
    // planar: shape = [n_bands, tile_h, tile_w]
    // strides into the underlying flat buffer
    let (h_stride, w_stride, b_stride): (usize, usize, usize) = match planar {
        PlanarConfiguration::Chunky => {
            // pixel(r, c, b) = data[r * (tile_w * n_bands) + c * n_bands + b]
            (tile_w * n_bands, n_bands, 1)
        }
        PlanarConfiguration::Planar => {
            // pixel(b, r, c) = data[b * (tile_h * tile_w) + r * tile_w + c]
            (tile_w, 1, tile_h * tile_w)
        }
        other => {
            return Err(A5CogError::Unsupported(format!(
                "unhandled planar configuration: {other:?}"
            )));
        }
    };
    let _ = shape; // shape is implied by tile_w/tile_h/n_bands

    // small stack buffer reused per pixel to hold per-band values + validity
    let mut band_vals: Vec<f64> = vec![0.0; n_bands];
    let mut band_valid: Vec<bool> = vec![false; n_bands];

    // cell-caching: adjacent pixels at fine A5 resolutions frequently fall in the
    // same cell. Avoid re-hashing into the local map when the cell hasn't changed.
    let mut last_cell: Option<u64> = None;
    let mut last_entry_ptr: *mut Accum = std::ptr::null_mut();
    let nodata_some = nodata.is_some();
    let nodata_v = nodata.unwrap_or(0.0);

    for (idx, &(lon_o, lat_o, _)) in points.iter().enumerate() {
        let r = idx / actual_w;
        let c = idx % actual_w;

        // gather pixel values + validity first; skip pixel entirely if all-nodata
        let pixel_base = r * h_stride + c * w_stride;
        let mut any_valid = false;
        if nodata_some {
            for b in 0..n_bands {
                let off = pixel_base + b * b_stride;
                let v = read_pixel_chunky(&data, off);
                let valid = v != nodata_v;
                band_vals[b] = v;
                band_valid[b] = valid;
                any_valid |= valid;
            }
        } else {
            for b in 0..n_bands {
                let off = pixel_base + b * b_stride;
                band_vals[b] = read_pixel_chunky(&data, off);
                band_valid[b] = true;
            }
            any_valid = true;
        }
        if !any_valid {
            continue;
        }

        let lon_deg = if dst_is_latlong { lon_o.to_degrees() } else { lon_o };
        let lat_deg = if dst_is_latlong { lat_o.to_degrees() } else { lat_o };
        if !lon_deg.is_finite() || !lat_deg.is_finite() {
            continue;
        }

        let lonlat = a5::LonLat::new(lon_deg, lat_deg);
        let cell = match a5::lonlat_to_cell(lonlat, resolution) {
            Ok(id) => id,
            Err(_) => continue,
        };

        // SAFETY: `last_entry_ptr` is only dereferenced when `last_cell == Some(cell)`,
        // and the underlying Vec it points into lives in `local` (this function's
        // local map). We never mutate `local` between setting and reading the
        // pointer when the cell repeats, so the address stays valid.
        let entry: &mut [Accum] = if last_cell == Some(cell) && !last_entry_ptr.is_null() {
            unsafe { std::slice::from_raw_parts_mut(last_entry_ptr, n_bands) }
        } else {
            let v = local
                .entry(cell)
                .or_insert_with(|| vec![Accum::new(); n_bands]);
            last_cell = Some(cell);
            last_entry_ptr = v.as_mut_ptr();
            v.as_mut_slice()
        };
        for b in 0..n_bands {
            if band_valid[b] {
                entry[b].push(band_vals[b]);
            }
        }
    }

    Ok(local)
}

// ---------------------------------------------------------------------------
// async pipeline

#[allow(clippy::too_many_arguments)]
async fn read_raster_async(
    src: &str,
    resolution: i32,
    stat: Stat,
    io_concurrency: usize,
) -> Result<Output> {
    let (store, path) = parse_src(src)?;
    let reader = ObjectReader::new(store, path);
    let cache = ReadaheadMetadataCache::new(reader.clone());
    let mut meta = TiffMetadataReader::try_open(&cache).await?;
    let ifds = meta.read_all_ifds(&cache).await?;
    let endianness = meta.endianness();
    let tiff = TIFF::new(ifds, endianness);

    // grab the first IFD = full-resolution image (overviews follow)
    let ifd_owned = tiff
        .ifds()
        .first()
        .ok_or_else(|| A5CogError::Invalid("no IFDs".into()))?
        .clone();

    let geo = ifd_owned
        .geo_key_directory()
        .ok_or(A5CogError::MissingGeoKey("GeoKeyDirectory"))?;
    let epsg = geo
        .epsg_code()
        .ok_or(A5CogError::MissingGeoKey("EPSG code"))?;

    let src_proj = Proj::from_epsg_code(epsg)?;
    let dst_proj = Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs")?;

    let gt = extract_geotransform(&ifd_owned)?;

    let width = ifd_owned.image_width() as usize;
    let height = ifd_owned.image_height() as usize;
    let n_bands = ifd_owned.samples_per_pixel() as usize;

    let planar = ifd_owned.planar_configuration();

    let (tile_w, tile_h) = match (ifd_owned.tile_width(), ifd_owned.tile_height()) {
        (Some(w), Some(h)) => (w as usize, h as usize),
        _ => {
            return Err(A5CogError::Unsupported(
                "MVP requires tiled TIFF; strip-based not yet supported".into(),
            ));
        }
    };
    let (n_tiles_x, n_tiles_y) = ifd_owned
        .tile_count()
        .ok_or_else(|| A5CogError::Unsupported("non-tiled".into()))?;

    let nodata = parse_nodata(&ifd_owned);
    let band_names_v = parse_band_descriptions(&ifd_owned, n_bands);
    let band_names: Vec<String> = if band_names_v.is_empty() {
        (0..n_bands).map(|i| format!("band_{:02}", i + 1)).collect()
    } else {
        band_names_v
    };

    let global = Arc::new(StdMutex::new(AHashMap::<u64, Vec<Accum>>::new()));

    let tiles: Vec<(usize, usize)> = (0..n_tiles_y)
        .flat_map(|y| (0..n_tiles_x).map(move |x| (x, y)))
        .collect();

    let ifd_arc = Arc::new(ifd_owned);
    let src_proj_arc = Arc::new(src_proj);
    let dst_proj_arc = Arc::new(dst_proj);

    stream::iter(tiles)
        .map(|(tx, ty)| {
            let reader = reader.clone();
            let ifd = Arc::clone(&ifd_arc);
            let src_proj = Arc::clone(&src_proj_arc);
            let dst_proj = Arc::clone(&dst_proj_arc);
            let global = Arc::clone(&global);
            async move {
                let tile = ifd.fetch_tile(tx, ty, &reader as &dyn AsyncFileReader).await?;
                let arr = tile.decode(&DecoderRegistry::default())?;
                let (data, shape, _dt) = arr.into_inner();
                let local = tokio::task::spawn_blocking(move || {
                    process_tile(
                        tx,
                        ty,
                        data,
                        shape,
                        planar,
                        width,
                        height,
                        tile_w,
                        tile_h,
                        n_bands,
                        &src_proj,
                        &dst_proj,
                        &gt,
                        resolution,
                        nodata,
                    )
                })
                .await
                .map_err(|e| A5CogError::Invalid(format!("tile join error: {e}")))??;

                let mut g = global.lock().unwrap();
                for (cell, accs) in local {
                    let entry = g.entry(cell).or_insert_with(|| vec![Accum::new(); n_bands]);
                    for (i, a) in accs.iter().enumerate() {
                        entry[i].merge(a);
                    }
                }
                Ok::<(), A5CogError>(())
            }
        })
        .buffer_unordered(io_concurrency.max(1))
        .try_collect::<Vec<()>>()
        .await?;

    let map = Arc::try_unwrap(global)
        .map_err(|_| A5CogError::Invalid("residual references to result map".into()))?
        .into_inner()
        .unwrap();

    let n = map.len();
    let mut cells = Vec::with_capacity(n);
    let mut values: Vec<Vec<f64>> = (0..n_bands).map(|_| Vec::with_capacity(n)).collect();
    for (cell, accs) in map {
        cells.push(cell);
        for (b, a) in accs.iter().enumerate() {
            values[b].push(a.finalise(stat));
        }
    }

    Ok(Output {
        cells,
        values,
        band_names,
    })
}

struct Output {
    cells: Vec<u64>,
    values: Vec<Vec<f64>>,
    band_names: Vec<String>,
}

// ---------------------------------------------------------------------------
// extendr binding

/// Forward-aggregate a (Cloud-Optimised) GeoTIFF into A5 cells.
///
/// @param src Path or URL string (file://, http(s)://, s3://, gs://, az://).
/// @param resolution A5 resolution (0--30).
/// @param stat One of "mean", "sum", "count", "min", "max".
/// @param threads Worker threads (currently used for tile-level concurrency).
/// @param io_concurrency Number of tiles fetched concurrently.
/// @returns A list with `cell` (b1..b8 raw fields), `bands` (named numeric
///   vectors per band), and `band_names` (character).
/// @noRd
/// @keywords internal
#[extendr]
fn a5_read_raster_rs(
    src: &str,
    resolution: i32,
    stat: &str,
    threads: i32,
    io_concurrency: i32,
) -> Result<Robj> {
    if !(0..=30).contains(&resolution) {
        return Err(A5CogError::Invalid(format!(
            "resolution must be 0..=30, got {resolution}"
        )));
    }
    let stat = Stat::parse(stat)?;
    let threads = threads.max(1) as usize;
    let io_concurrency = io_concurrency.max(1) as usize;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .map_err(|e| A5CogError::Invalid(format!("tokio runtime: {e}")))?;

    let out: Output = runtime.block_on(read_raster_async(src, resolution, stat, io_concurrency))?;

    let cell_list = u64s_to_raw8_list(&out.cells);

    let mut band_pairs: Vec<(String, Robj)> = Vec::with_capacity(out.values.len());
    for (name, vals) in out.band_names.iter().zip(out.values.into_iter()) {
        band_pairs.push((name.clone(), Robj::from(vals)));
    }
    let bands = List::from_pairs(band_pairs);
    let band_names: Vec<&str> = out.band_names.iter().map(|s| s.as_str()).collect();

    Ok(list!(
        cell = cell_list,
        bands = bands,
        band_names = band_names
    )
    .into())
}

// adapt our error to extendr's
impl From<A5CogError> for extendr_api::Error {
    fn from(e: A5CogError) -> Self {
        extendr_api::Error::Other(e.to_string())
    }
}

extendr_module! {
    mod read;
    fn a5_read_raster_rs;
}
