//! Forward pixel-driven raster → A5 cell aggregation.

use std::sync::Arc;

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

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::cell_raw::u64s_to_raw8_list;
use crate::error::{A5CogError, Result};
use crate::geo::{
    GeoTransform, build_src_proj, extract_geotransform, is_nodata, parse_band_descriptions,
    parse_nodata,
};

// stage timers (only emit if A5PX_PROFILE env var is set, e.g. A5PX_PROFILE=1)
static T_FETCH_NS: AtomicU64 = AtomicU64::new(0);
static T_DECODE_NS: AtomicU64 = AtomicU64::new(0);
static T_BUILD_PTS_NS: AtomicU64 = AtomicU64::new(0);
static T_PROJ_NS: AtomicU64 = AtomicU64::new(0);
static T_INDEX_NS: AtomicU64 = AtomicU64::new(0);
static T_MERGE_NS: AtomicU64 = AtomicU64::new(0);
// sub-stage timers inside the per-pixel loop
static T_PIX_READ_NS: AtomicU64 = AtomicU64::new(0);
static T_A5_CELL_NS: AtomicU64 = AtomicU64::new(0);
static T_HM_NS: AtomicU64 = AtomicU64::new(0);
static T_PUSH_NS: AtomicU64 = AtomicU64::new(0);

fn profile_enabled() -> bool {
    std::env::var_os("A5PX_PROFILE").is_some()
}

fn reset_timers() {
    for t in [
        &T_FETCH_NS, &T_DECODE_NS, &T_BUILD_PTS_NS, &T_PROJ_NS,
        &T_INDEX_NS, &T_MERGE_NS,
        &T_PIX_READ_NS, &T_A5_CELL_NS, &T_HM_NS, &T_PUSH_NS,
    ] {
        t.store(0, Ordering::Relaxed);
    }
}

fn print_timers(total: f64) {
    let one = |label: &str, ns: u64| {
        let s = ns as f64 / 1e9;
        eprintln!(
            "  {label:<22} {s:>7.3} s  ({:>5.1}%)",
            100.0 * s / total
        )
    };
    eprintln!("[a5px profile, total {:.3} s, sum across tile workers]", total);
    one("io fetch", T_FETCH_NS.load(Ordering::Relaxed));
    one("decode", T_DECODE_NS.load(Ordering::Relaxed));
    one("build points", T_BUILD_PTS_NS.load(Ordering::Relaxed));
    one("proj transform", T_PROJ_NS.load(Ordering::Relaxed));
    one("a5 index + accum", T_INDEX_NS.load(Ordering::Relaxed));
    eprintln!("    of which:");
    one("  pixel read+nodata", T_PIX_READ_NS.load(Ordering::Relaxed));
    one("  a5 lonlat->cell", T_A5_CELL_NS.load(Ordering::Relaxed));
    one("  hashmap lookup", T_HM_NS.load(Ordering::Relaxed));
    one("  push to accums", T_PUSH_NS.load(Ordering::Relaxed));
    one("merge into global", T_MERGE_NS.load(Ordering::Relaxed));
}

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

    fn as_str(&self) -> &'static str {
        match self {
            Self::Mean => "mean",
            Self::Sum => "sum",
            Self::Count => "count",
            Self::Min => "min",
            Self::Max => "max",
        }
    }
}

fn parse_stats(stats: &[String]) -> Result<Vec<Stat>> {
    if stats.is_empty() {
        return Err(A5CogError::Invalid("at least one stat is required".into()));
    }
    stats.iter().map(|s| Stat::parse(s.as_str())).collect()
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

pub(crate) fn parse_src_pub(src: &str) -> Result<(Arc<dyn ObjectStore>, ObjPath)> {
    parse_src(src)
}

pub(crate) fn read_pixel_chunky_pub(data: &TypedArray, idx: usize) -> f64 {
    read_pixel_chunky(data, idx)
}

fn parse_src(src: &str) -> Result<(Arc<dyn ObjectStore>, ObjPath)> {
    // url::Url::parse only succeeds when src has a scheme; treat anything
    // that parses as a URL as remote and let object_store::parse_url decide
    // whether the scheme is supported. Anything that doesn't parse falls
    // through to the local-path branch.
    if let Ok(url) = url::Url::parse(src) {
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
    data_n_bands: usize,
    data_band_offsets: &[usize],
    src_proj: &Proj,
    dst_proj: &Proj,
    gt: &GeoTransform,
    resolution: i32,
    nodata: Option<f64>,
    bbox_lonlat: Option<[f64; 4]>,
) -> Result<AHashMap<u64, Vec<Accum>>> {
    let n_out = data_band_offsets.len();
    let actual_w = tile_w.min(width.saturating_sub(tx * tile_w));
    let actual_h = tile_h.min(height.saturating_sub(ty * tile_h));
    if actual_w == 0 || actual_h == 0 {
        return Ok(AHashMap::new());
    }

    let src_is_latlong = src_proj.is_latlong();
    let dst_is_latlong = dst_proj.is_latlong();

    let prof = profile_enabled();

    // build per-pixel projected coords, transform en-masse
    let n = actual_w * actual_h;
    let t_pts = Instant::now();
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
    if prof {
        T_BUILD_PTS_NS.fetch_add(t_pts.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    let t_proj = Instant::now();
    proj_transform(src_proj, dst_proj, &mut points[..])?;
    if prof {
        T_PROJ_NS.fetch_add(t_proj.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    let mut local: AHashMap<u64, Vec<Accum>> = AHashMap::with_capacity(n / 4);

    // shape interpretation
    // chunky: shape = [tile_h, tile_w, n_bands]
    // planar: shape = [n_bands, tile_h, tile_w]
    // strides into the underlying flat buffer
    let (h_stride, w_stride, b_stride): (usize, usize, usize) = match planar {
        PlanarConfiguration::Chunky => {
            // pixel(r, c, b) = data[r * (tile_w * data_n_bands) + c * data_n_bands + b]
            (tile_w * data_n_bands, data_n_bands, 1)
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
    let _ = shape; // shape is implied by tile_w/tile_h/data_n_bands

    // small stack buffer reused per pixel to hold per-selected-band values + validity
    let mut band_vals: Vec<f64> = vec![0.0; n_out];
    let mut band_valid: Vec<bool> = vec![false; n_out];

    // cell-caching: adjacent pixels at fine A5 resolutions almost always fall in
    // the same cell. Keep the previous cell's A5Cell and try
    // `a5cell_contains_point` (a single projection + pentagon test) before
    // falling back to the full search-based `a5::lonlat_to_cell` (~26 estimates).
    let mut last_cell: Option<u64> = None;
    let mut last_a5cell: Option<a5::A5Cell> = None;
    let mut last_entry_ptr: *mut Accum = std::ptr::null_mut();
    // hoist nodata branch out of the per-pixel loop
    let nodata_v = nodata;

    // local sub-stage accumulators (reduced once at end-of-tile)
    let mut sub_pix: u64 = 0;
    let mut sub_a5: u64 = 0;
    let mut sub_hm: u64 = 0;
    let mut sub_push: u64 = 0;

    let t_idx = Instant::now();
    for (idx, &(lon_o, lat_o, _)) in points.iter().enumerate() {
        let r = idx / actual_w;
        let c = idx % actual_w;

        // gather pixel values + validity first; skip pixel entirely if all-nodata
        let pixel_base = r * h_stride + c * w_stride;
        let t = if prof { Some(Instant::now()) } else { None };
        let mut any_valid = false;
        if let Some(nd) = nodata_v {
            for (out_b, &src_b) in data_band_offsets.iter().enumerate() {
                let off = pixel_base + src_b * b_stride;
                let v = read_pixel_chunky(&data, off);
                let valid = !is_nodata(v, nd);
                band_vals[out_b] = v;
                band_valid[out_b] = valid;
                any_valid |= valid;
            }
        } else {
            for (out_b, &src_b) in data_band_offsets.iter().enumerate() {
                let off = pixel_base + src_b * b_stride;
                band_vals[out_b] = read_pixel_chunky(&data, off);
                band_valid[out_b] = true;
            }
            any_valid = true;
        }
        if let Some(t0) = t { sub_pix += t0.elapsed().as_nanos() as u64; }
        if !any_valid {
            continue;
        }

        let lon_deg = if dst_is_latlong { lon_o.to_degrees() } else { lon_o };
        let lat_deg = if dst_is_latlong { lat_o.to_degrees() } else { lat_o };
        if !lon_deg.is_finite() || !lat_deg.is_finite() {
            continue;
        }

        if let Some(b) = bbox_lonlat {
            if lon_deg < b[0] || lon_deg > b[2] || lat_deg < b[1] || lat_deg > b[3] {
                continue;
            }
        }

        let t = if prof { Some(Instant::now()) } else { None };
        let lonlat = a5::LonLat::new(lon_deg, lat_deg);
        let cell = if let (Some(prev_id), Some(prev_a5)) = (last_cell, last_a5cell.as_ref()) {
            match a5::core::cell::a5cell_contains_point(prev_a5, lonlat) {
                Ok(d) if d > 0.0 => prev_id,
                _ => match a5::lonlat_to_cell(lonlat, resolution) {
                    Ok(id) => {
                        last_a5cell = a5::core::serialization::deserialize(id).ok();
                        id
                    }
                    Err(_) => continue,
                },
            }
        } else {
            match a5::lonlat_to_cell(lonlat, resolution) {
                Ok(id) => {
                    last_a5cell = a5::core::serialization::deserialize(id).ok();
                    id
                }
                Err(_) => continue,
            }
        };
        if let Some(t0) = t { sub_a5 += t0.elapsed().as_nanos() as u64; }

        let t = if prof { Some(Instant::now()) } else { None };
        // SAFETY: `last_entry_ptr` is only dereferenced when `last_cell == Some(cell)`,
        // and the underlying Vec it points into lives in `local` (this function's
        // local map). We never mutate `local` between setting and reading the
        // pointer when the cell repeats, so the address stays valid.
        let entry: &mut [Accum] = if last_cell == Some(cell) && !last_entry_ptr.is_null() {
            unsafe { std::slice::from_raw_parts_mut(last_entry_ptr, n_out) }
        } else {
            let v = local
                .entry(cell)
                .or_insert_with(|| vec![Accum::new(); n_out]);
            last_cell = Some(cell);
            last_entry_ptr = v.as_mut_ptr();
            v.as_mut_slice()
        };
        if let Some(t0) = t { sub_hm += t0.elapsed().as_nanos() as u64; }

        let t = if prof { Some(Instant::now()) } else { None };
        for b in 0..n_out {
            if band_valid[b] {
                entry[b].push(band_vals[b]);
            }
        }
        if let Some(t0) = t { sub_push += t0.elapsed().as_nanos() as u64; }
    }
    if prof {
        T_INDEX_NS.fetch_add(t_idx.elapsed().as_nanos() as u64, Ordering::Relaxed);
        T_PIX_READ_NS.fetch_add(sub_pix, Ordering::Relaxed);
        T_A5_CELL_NS.fetch_add(sub_a5, Ordering::Relaxed);
        T_HM_NS.fetch_add(sub_hm, Ordering::Relaxed);
        T_PUSH_NS.fetch_add(sub_push, Ordering::Relaxed);
    }

    Ok(local)
}

// ---------------------------------------------------------------------------
// async pipeline

/// Item moved from the I/O producer to the CPU consumer pool. The producer
/// only does the network/disk read; the consumer decodes + processes.
pub(crate) struct TileItem {
    pub tx: usize,
    pub ty: usize,
    pub payload: TilePayload,
}

pub(crate) enum TilePayload {
    /// Output of `ImageFileDirectory::fetch_tile`. Decode happens on the consumer.
    Full(async_tiff::Tile),
    /// Per-selected-band compressed bytes for planar layouts. Used when the
    /// caller asked for a band subset of an INTERLEAVE=BAND TIFF with
    /// predictor=None: only those bands' byte ranges were fetched.
    PlanarSubset(Vec<bytes::Bytes>),
}

#[allow(clippy::too_many_arguments)]
async fn read_raster_async(
    src: &str,
    resolution: i32,
    stats: Vec<Stat>,
    bands_idx: Vec<i32>,
    bands_names: Vec<String>,
    bbox_lonlat: Option<[f64; 4]>,
    src_nodata_override: Option<f64>,
    cpu_workers: usize,
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

    let src_proj = build_src_proj(geo)?;
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
    let all_band_names: Vec<String> = if band_names_v.is_empty() {
        (0..n_bands).map(|i| format!("band_{:02}", i + 1)).collect()
    } else {
        band_names_v
    };

    // resolve band selection to 0-based indices
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
    let band_names: Vec<String> = selected_bands
        .iter()
        .map(|&i| all_band_names[i].clone())
        .collect();

    // override nodata if user specified it; otherwise use what async-tiff exposed
    let nodata = src_nodata_override.or(nodata);

    // Bbox-driven tile filter. For most projections the bbox of 4 corners +
    // 4 edge midpoints (re-projected to the raster CRS) is a sufficient
    // axis-aligned envelope to pick the candidate tiles. The per-pixel
    // lon/lat check inside process_tile then exact-filters at the boundary.
    let tiles: Vec<(usize, usize)> = if let Some(b) = bbox_lonlat {
        let (tx_lo, ty_lo, tx_hi, ty_hi) = match projected_tile_range(
            b, &src_proj, &dst_proj, &gt, width, height, tile_w, tile_h,
        )? {
            Some(rng) => rng,
            None => return Ok(empty_output(band_names, n_out, &stats)),
        };
        let mut v = Vec::with_capacity((tx_hi - tx_lo + 1) * (ty_hi - ty_lo + 1));
        for ty in ty_lo..=ty_hi {
            for tx in tx_lo..=tx_hi {
                v.push((tx, ty));
            }
        }
        v
    } else {
        (0..n_tiles_y)
            .flat_map(|y| (0..n_tiles_x).map(move |x| (x, y)))
            .collect()
    };

    // decide once whether the band-aware fetch path applies
    let use_band_fetch = matches!(planar, PlanarConfiguration::Planar)
        && n_out < n_bands
        && matches!(
            ifd_owned.predictor(),
            None | Some(async_tiff::tags::Predictor::None)
        );
    let identity_offsets: Vec<usize> = (0..n_out).collect();

    let ifd_arc = Arc::new(ifd_owned);
    let src_proj_arc = Arc::new(src_proj);
    let dst_proj_arc = Arc::new(dst_proj);
    let selected_bands_arc: Arc<Vec<usize>> = Arc::new(selected_bands);
    let identity_offsets_arc: Arc<Vec<usize>> = Arc::new(identity_offsets);
    let registry_arc: Arc<DecoderRegistry> = Arc::new(DecoderRegistry::default());

    // Producer / consumer pipeline. The producer issues all tile fetches
    // concurrently (`io_concurrency`), pushes a TileItem into a bounded
    // channel. `cpu_workers` blocking-pool tasks consume from the channel,
    // each with its own AHashMap accumulator. Once all senders are dropped
    // the channel closes, consumers drain remaining items, exit, and we
    // tree-reduce the per-worker maps into one. No global mutex.
    let channel_depth = (cpu_workers * 2).max(4);
    let (tx_chan_outer, rx_chan) = async_channel::bounded::<TileItem>(channel_depth);
    // We hand a clone to the producer; we keep tx_chan_outer in scope only
    // long enough to finish setting up the producer, then drop it. With no
    // remaining senders, recv_blocking() in consumers will return Err and
    // they'll exit cleanly.

    // Spawn consumer workers up-front so they're ready as soon as the
    // producer starts pushing. Each runs on the tokio blocking pool.
    let mut consumer_handles: Vec<tokio::task::JoinHandle<Result<AHashMap<u64, Vec<Accum>>>>> =
        Vec::with_capacity(cpu_workers);
    for _ in 0..cpu_workers {
        let rx = rx_chan.clone();
        let ifd = Arc::clone(&ifd_arc);
        let src_proj = Arc::clone(&src_proj_arc);
        let dst_proj = Arc::clone(&dst_proj_arc);
        let selected_bands = Arc::clone(&selected_bands_arc);
        let identity_offsets = Arc::clone(&identity_offsets_arc);
        let registry = Arc::clone(&registry_arc);
        let gt_c = gt;
        let bbox_lonlat_c = bbox_lonlat;
        consumer_handles.push(tokio::task::spawn_blocking(move || {
            let mut local: AHashMap<u64, Vec<Accum>> = AHashMap::new();
            let prof = profile_enabled();
            while let Ok(item) = rx.recv_blocking() {
                let (data, shape, data_n_bands_eff, offsets_arc): (
                    TypedArray,
                    [usize; 3],
                    usize,
                    Arc<Vec<usize>>,
                ) = match item.payload {
                    TilePayload::Full(tile) => {
                        let t_dec = Instant::now();
                        let arr = tile.decode(&registry)?;
                        if prof {
                            T_DECODE_NS
                                .fetch_add(t_dec.elapsed().as_nanos() as u64, Ordering::Relaxed);
                        }
                        let (data, sh, _) = arr.into_inner();
                        (data, sh, n_bands, Arc::clone(&selected_bands))
                    }
                    TilePayload::PlanarSubset(bytes) => {
                        let t_dec = Instant::now();
                        let (typed, sh) = crate::band_fetch::decode_planar_subset_bytes(
                            bytes, &ifd, &registry,
                        )?;
                        if prof {
                            T_DECODE_NS
                                .fetch_add(t_dec.elapsed().as_nanos() as u64, Ordering::Relaxed);
                        }
                        (typed, sh, n_out, Arc::clone(&identity_offsets))
                    }
                };
                let tile_local = process_tile(
                    item.tx,
                    item.ty,
                    data,
                    shape,
                    planar,
                    width,
                    height,
                    tile_w,
                    tile_h,
                    data_n_bands_eff,
                    &offsets_arc,
                    &src_proj,
                    &dst_proj,
                    &gt_c,
                    resolution,
                    nodata,
                    bbox_lonlat_c,
                )?;
                let t_merge = Instant::now();
                for (cell, accs) in tile_local {
                    let entry = local.entry(cell).or_insert_with(|| vec![Accum::new(); n_out]);
                    for (e, a) in entry.iter_mut().zip(accs.iter()) {
                        e.merge(a);
                    }
                }
                if prof {
                    T_MERGE_NS.fetch_add(t_merge.elapsed().as_nanos() as u64, Ordering::Relaxed);
                }
            }
            Ok::<AHashMap<u64, Vec<Accum>>, A5CogError>(local)
        }));
    }
    // Consumers each hold their own rx clone; drop the outer one so the
    // channel closes the moment the producer finishes.
    drop(rx_chan);

    // Producer. Move the outer sender clone into the block so that when the
    // block exits, it's dropped and the channel closes.
    {
        let reader = reader.clone();
        let ifd = Arc::clone(&ifd_arc);
        let selected_bands = Arc::clone(&selected_bands_arc);
        let tx_chan = tx_chan_outer;
        let producer = stream::iter(tiles)
            .map(|(tx, ty)| {
                let reader = reader.clone();
                let ifd = Arc::clone(&ifd);
                let selected_bands = Arc::clone(&selected_bands);
                let tx_chan = tx_chan.clone();
                async move {
                    let prof = profile_enabled();
                    let t_fetch = Instant::now();
                    let payload = if use_band_fetch {
                        let bytes = crate::band_fetch::fetch_planar_subset_bytes(
                            &reader as &dyn AsyncFileReader,
                            &ifd,
                            tx,
                            ty,
                            &selected_bands,
                        )
                        .await?;
                        TilePayload::PlanarSubset(bytes)
                    } else {
                        let tile = ifd
                            .fetch_tile(tx, ty, &reader as &dyn AsyncFileReader)
                            .await?;
                        TilePayload::Full(tile)
                    };
                    if prof {
                        T_FETCH_NS
                            .fetch_add(t_fetch.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    }
                    tx_chan
                        .send(TileItem { tx, ty, payload })
                        .await
                        .map_err(|_| {
                            A5CogError::Invalid("consumer pool dropped channel".into())
                        })?;
                    Ok::<(), A5CogError>(())
                }
            })
            .buffer_unordered(io_concurrency.max(1))
            .try_collect::<Vec<()>>();
        // tx_chan is moved into this block so end-of-scope drops it,
        // closing the channel once the producer finishes (the per-task
        // clones go away with their futures).
        let producer_result = producer.await;
        // If the producer errored mid-flight, abort the consumer pool so
        // it doesn't keep burning CPU on already-queued tiles after R has
        // seen the error. tokio's default behaviour for a dropped
        // JoinHandle is to detach, not cancel.
        if let Err(e) = producer_result {
            for h in &consumer_handles {
                h.abort();
            }
            return Err(e);
        }
    }

    // Drain consumers and tree-reduce. Collect all results first (rather
    // than short-circuit on the first Err) so a panic / error in worker N
    // doesn't detach workers N+1.. while they're still running.
    let mut consumer_results: Vec<Result<AHashMap<u64, Vec<Accum>>>> =
        Vec::with_capacity(cpu_workers);
    for h in consumer_handles {
        match h.await {
            Ok(inner) => consumer_results.push(inner),
            Err(join_err) => consumer_results.push(Err(A5CogError::WorkerJoin(format!(
                "tile-consumer worker: {join_err}"
            )))),
        }
    }
    let mut local_maps: Vec<AHashMap<u64, Vec<Accum>>> = Vec::with_capacity(cpu_workers);
    for r in consumer_results {
        local_maps.push(r?);
    }
    let map = local_maps
        .into_iter()
        .reduce(|mut a, b| {
            for (cell, accs) in b {
                let entry = a.entry(cell).or_insert_with(|| vec![Accum::new(); n_out]);
                for (e, ac) in entry.iter_mut().zip(accs.iter()) {
                    e.merge(ac);
                }
            }
            a
        })
        .unwrap_or_default();

    let n_stats = stats.len();
    let n = map.len();
    let mut cells = Vec::with_capacity(n);
    // cell-major flat layout per stat: flat_values[s][i*n_out + b] is the s-th
    // stat of band b of cell i.
    let mut flat_per_stat: Vec<Vec<f64>> =
        (0..n_stats).map(|_| Vec::with_capacity(n * n_out)).collect();
    for (cell, accs) in map {
        cells.push(cell);
        for a in accs.iter() {
            for (s_i, s) in stats.iter().enumerate() {
                flat_per_stat[s_i].push(a.finalise(*s));
            }
        }
    }

    Ok(Output {
        cells,
        flat_values: flat_per_stat,
        n_bands: n_out,
        band_names,
        stats: stats.iter().map(|s| s.as_str().to_string()).collect(),
    })
}

struct Output {
    cells: Vec<u64>,
    /// One Vec per stat. Each Vec is cell-major flat: cell `i` band `b` is at
    /// index `i * n_bands + b`. Outer index matches the order of `stats`.
    flat_values: Vec<Vec<f64>>,
    n_bands: usize,
    band_names: Vec<String>,
    stats: Vec<String>,
}

/// Decode an extendr-passed `Vec<f64>` whose length is the cheap NULL
/// sentinel: empty `Vec` means "user passed NULL"; other lengths are
/// validated by the caller against the expected shape.
fn opt_f64_arg<const N: usize>(v: Vec<f64>, label: &str) -> Result<Option<[f64; N]>> {
    if v.is_empty() {
        return Ok(None);
    }
    if v.len() != N {
        return Err(A5CogError::Invalid(format!(
            "{label} must be length {N} (or NULL); got len {}",
            v.len()
        )));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(A5CogError::Invalid(format!(
            "{label} must contain finite numeric values"
        )));
    }
    let mut out = [0.0f64; N];
    out.copy_from_slice(&v);
    Ok(Some(out))
}

fn parse_bbox_arg(v: Vec<f64>) -> Result<Option<[f64; 4]>> {
    opt_f64_arg::<4>(v, "bbox")
}

fn parse_src_nodata_arg(v: Vec<f64>) -> Result<Option<f64>> {
    opt_f64_arg::<1>(v, "src_nodata").map(|opt| opt.map(|a| a[0]))
}

fn empty_output(band_names: Vec<String>, n_out: usize, stats: &[Stat]) -> Output {
    Output {
        cells: Vec::new(),
        flat_values: stats.iter().map(|_| Vec::new()).collect(),
        n_bands: n_out,
        band_names,
        stats: stats.iter().map(|s| s.as_str().to_string()).collect(),
    }
}

/// Reproject a WGS84 lon/lat bbox into the raster CRS, take the axis-aligned
/// bounding box of the resulting points, clamp to the raster, and return the
/// inclusive tile-index range that covers it. Returns `Ok(None)` if the bbox
/// reproject yields nothing inside the raster.
#[allow(clippy::too_many_arguments)]
fn projected_tile_range(
    bbox_lonlat: [f64; 4],
    src_proj: &Proj,
    dst_proj: &Proj,
    gt: &GeoTransform,
    width: usize,
    height: usize,
    tile_w: usize,
    tile_h: usize,
) -> Result<Option<(usize, usize, usize, usize)>> {
    let (xmin_ll, ymin_ll, xmax_ll, ymax_ll) =
        (bbox_lonlat[0], bbox_lonlat[1], bbox_lonlat[2], bbox_lonlat[3]);
    if xmin_ll >= xmax_ll || ymin_ll >= ymax_ll {
        return Err(A5CogError::Invalid(format!(
            "bbox must satisfy xmin<xmax & ymin<ymax; got [{xmin_ll}, {ymin_ll}, {xmax_ll}, {ymax_ll}]"
        )));
    }
    let xmid = (xmin_ll + xmax_ll) * 0.5;
    let ymid = (ymin_ll + ymax_ll) * 0.5;
    // 4 corners + 4 edge midpoints. Curved projections rarely bulge enough
    // for this to miss; per-pixel filter inside `process_tile` is the exact one.
    let mut points: Vec<(f64, f64, f64)> = vec![
        (xmin_ll, ymin_ll, 0.0),
        (xmax_ll, ymin_ll, 0.0),
        (xmin_ll, ymax_ll, 0.0),
        (xmax_ll, ymax_ll, 0.0),
        (xmid, ymin_ll, 0.0),
        (xmid, ymax_ll, 0.0),
        (xmin_ll, ymid, 0.0),
        (xmax_ll, ymid, 0.0),
    ];
    // dst_proj is +proj=longlat +datum=WGS84; latlong needs radians
    let dst_is_latlong = dst_proj.is_latlong();
    if dst_is_latlong {
        for p in &mut points {
            p.0 = p.0.to_radians();
            p.1 = p.1.to_radians();
        }
    }
    // forward direction: WGS84 (dst) -> raster CRS (src)
    proj_transform(dst_proj, src_proj, &mut points[..])?;
    if src_proj.is_latlong() {
        for p in &mut points {
            p.0 = p.0.to_degrees();
            p.1 = p.1.to_degrees();
        }
    }
    let mut xmin = f64::INFINITY;
    let mut ymin = f64::INFINITY;
    let mut xmax = f64::NEG_INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    for &(x, y, _) in &points {
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        if x < xmin { xmin = x; }
        if x > xmax { xmax = x; }
        if y < ymin { ymin = y; }
        if y > ymax { ymax = y; }
    }
    if !xmin.is_finite() || !xmax.is_finite() {
        return Err(A5CogError::Invalid(
            "could not reproject bbox into raster CRS (all points NaN)".into(),
        ));
    }

    // Invert the (axis-aligned) geotransform to map projected x/y -> pixel.
    if gt.0[2] != 0.0 || gt.0[4] != 0.0 {
        return Err(A5CogError::Unsupported(
            "rotated geotransform with bbox is not yet supported".into(),
        ));
    }
    let col_from_x = |x: f64| -> f64 { (x - gt.0[0]) / gt.0[1] };
    let row_from_y = |y: f64| -> f64 { (y - gt.0[3]) / gt.0[5] };
    let cols = [col_from_x(xmin), col_from_x(xmax)];
    let rows = [row_from_y(ymin), row_from_y(ymax)];
    let col_lo_f = cols[0].min(cols[1]).floor();
    let col_hi_f = cols[0].max(cols[1]).ceil();
    let row_lo_f = rows[0].min(rows[1]).floor();
    let row_hi_f = rows[0].max(rows[1]).ceil();

    let w_f = width as f64;
    let h_f = height as f64;
    if col_hi_f < 0.0 || col_lo_f >= w_f || row_hi_f < 0.0 || row_lo_f >= h_f {
        return Ok(None);
    }
    let col_lo = col_lo_f.max(0.0) as usize;
    let col_hi = col_hi_f.min(w_f - 1.0).max(0.0) as usize;
    let row_lo = row_lo_f.max(0.0) as usize;
    let row_hi = row_hi_f.min(h_f - 1.0).max(0.0) as usize;

    let tx_lo = col_lo / tile_w;
    let tx_hi = col_hi / tile_w;
    let ty_lo = row_lo / tile_h;
    let ty_hi = row_hi / tile_h;
    Ok(Some((tx_lo, ty_lo, tx_hi, ty_hi)))
}

// ---------------------------------------------------------------------------
// extendr binding

/// Forward-aggregate a (Cloud-Optimised) GeoTIFF into A5 cells.
///
/// @param src Path or URL string (file://, http(s)://, s3://, gs://, az://).
/// @param resolution A5 resolution (0--30).
/// @param stats Character vector of stats: subset of "mean", "sum", "count",
///   "min", "max". Length-1 behaves identically to the previous scalar API.
/// @param bands_idx 1-based band indices to read. Empty = all (unless
///   bands_names is non-empty).
/// @param bands_names Band names to read (matched against the GDAL DESCRIPTION
///   tag, falling back to band_NN). Empty = all (unless bands_idx is non-empty).
/// @param threads Worker threads (currently used for tile-level concurrency).
/// @param io_concurrency Number of tiles fetched concurrently.
/// @returns A list with `cell` (b1..b8 raw fields), `bands` (named numeric
///   vectors; key form is `<band>` for length-1 stats and `<band>__<stat>`
///   for length>1), `band_names`, and `stats` (character).
/// @noRd
/// @keywords internal
#[extendr]
fn a5_read_raster_rs(
    src: &str,
    resolution: i32,
    stats: Vec<String>,
    bands_idx: Vec<i32>,
    bands_names: Vec<String>,
    bbox: Vec<f64>,
    src_nodata: Vec<f64>,
    cpu_workers: i32,
    io_concurrency: i32,
) -> Result<Robj> {
    if !(0..=30).contains(&resolution) {
        return Err(A5CogError::Invalid(format!(
            "resolution must be 0..=30, got {resolution}"
        )));
    }
    if !bands_idx.is_empty() && !bands_names.is_empty() {
        return Err(A5CogError::Invalid(
            "specify bands by index OR by name, not both".into(),
        ));
    }
    let stats_e = parse_stats(&stats)?;
    let cpu_workers = cpu_workers.max(1) as usize;
    let io_concurrency = io_concurrency.max(1) as usize;

    let runtime = crate::runtime::shared_runtime()?;

    let prof = profile_enabled();
    if prof {
        reset_timers();
    }
    let t0 = Instant::now();

    let bbox_opt = parse_bbox_arg(bbox)?;
    let src_nodata_opt = parse_src_nodata_arg(src_nodata)?;

    let out: Output = runtime.block_on(read_raster_async(
        src,
        resolution,
        stats_e,
        bands_idx,
        bands_names,
        bbox_opt,
        src_nodata_opt,
        cpu_workers,
        io_concurrency,
    ))?;

    if prof {
        print_timers(t0.elapsed().as_secs_f64());
    }

    let cell_list = u64s_to_raw8_list(&out.cells);

    // de-interleave each per-stat flat buffer into one Vec<f64> per band per stat
    let n_cells = out.cells.len();
    let n_bands = out.n_bands;
    let n_stats = out.stats.len();
    let mut band_pairs: Vec<(String, Robj)> = Vec::with_capacity(n_bands * n_stats);
    for (s_i, s_name) in out.stats.iter().enumerate() {
        for (b, b_name) in out.band_names.iter().enumerate() {
            let mut col: Vec<f64> = Vec::with_capacity(n_cells);
            for i in 0..n_cells {
                col.push(out.flat_values[s_i][i * n_bands + b]);
            }
            let key = if n_stats == 1 {
                b_name.clone()
            } else {
                format!("{}__{}", b_name, s_name)
            };
            band_pairs.push((key, Robj::from(col)));
        }
    }
    let bands = List::from_pairs(band_pairs);
    let band_names: Vec<&str> = out.band_names.iter().map(|s| s.as_str()).collect();
    let stats_out: Vec<&str> = out.stats.iter().map(|s| s.as_str()).collect();

    Ok(list!(
        cell = cell_list,
        bands = bands,
        band_names = band_names,
        stats = stats_out
    )
    .into())
}

/// Forward-aggregate a (Cloud-Optimised) GeoTIFF into A5 cells, returning a
/// flat cell-major numeric buffer suitable for direct construction of an
/// Arrow `FixedSizeList<float64, n_bands>` array on the R side.
///
/// @param src Path or URL string.
/// @param resolution A5 resolution (0--30).
/// @param stats Character vector of stats (subset of mean/sum/count/min/max).
/// @param bands_idx 1-based band indices to read (empty for all unless
///   `bands_names` is provided).
/// @param bands_names Band names to read (matched against the GDAL
///   DESCRIPTION tag).
/// @param threads Worker threads.
/// @param io_concurrency Number of tiles fetched concurrently.
/// @returns A list with `cell` (b1..b8 raw), `value_flat` (named list of
///   numeric vectors, one per stat in `stats` order, each of length
///   `n_cells * n_bands` cell-major), `band_names`, `stats`, `n_bands`.
/// @noRd
/// @keywords internal
#[extendr]
fn a5_read_raster_flat_rs(
    src: &str,
    resolution: i32,
    stats: Vec<String>,
    bands_idx: Vec<i32>,
    bands_names: Vec<String>,
    bbox: Vec<f64>,
    src_nodata: Vec<f64>,
    cpu_workers: i32,
    io_concurrency: i32,
) -> Result<Robj> {
    if !(0..=30).contains(&resolution) {
        return Err(A5CogError::Invalid(format!(
            "resolution must be 0..=30, got {resolution}"
        )));
    }
    if !bands_idx.is_empty() && !bands_names.is_empty() {
        return Err(A5CogError::Invalid(
            "specify bands by index OR by name, not both".into(),
        ));
    }
    let stats_e = parse_stats(&stats)?;
    let cpu_workers = cpu_workers.max(1) as usize;
    let io_concurrency = io_concurrency.max(1) as usize;

    let runtime = crate::runtime::shared_runtime()?;

    let prof = profile_enabled();
    if prof {
        reset_timers();
    }
    let t0 = Instant::now();

    let bbox_opt = parse_bbox_arg(bbox)?;
    let src_nodata_opt = parse_src_nodata_arg(src_nodata)?;

    let out: Output = runtime.block_on(read_raster_async(
        src,
        resolution,
        stats_e,
        bands_idx,
        bands_names,
        bbox_opt,
        src_nodata_opt,
        cpu_workers,
        io_concurrency,
    ))?;

    if prof {
        print_timers(t0.elapsed().as_secs_f64());
    }

    let cell_list = u64s_to_raw8_list(&out.cells);
    let band_names: Vec<&str> = out.band_names.iter().map(|s| s.as_str()).collect();
    let n_bands = out.n_bands as i32;
    let stats_out: Vec<&str> = out.stats.iter().map(|s| s.as_str()).collect();

    let value_pairs: Vec<(String, Robj)> = out
        .stats
        .iter()
        .zip(out.flat_values.into_iter())
        .map(|(s, v)| (s.clone(), Robj::from(v)))
        .collect();
    let value_flat = List::from_pairs(value_pairs);

    Ok(list!(
        cell = cell_list,
        value_flat = value_flat,
        band_names = band_names,
        stats = stats_out,
        n_bands = n_bands
    )
    .into())
}

// adapt our error to extendr's
impl From<A5CogError> for extendr_api::Error {
    fn from(e: A5CogError) -> Self {
        extendr_api::Error::Other(e.to_string())
    }
}

/// Forward-aggregate a (Cloud-Optimised) GeoTIFF straight into a Parquet
/// file. RecordBatch construction and Parquet write happen in Rust without
/// the R Arrow round-trip — appropriate for large embedding rasters where
/// the per-cell list-of-vectors materialisation in R becomes a bottleneck.
///
/// @param src Path or URL string.
/// @param dest Output Parquet path.
/// @param resolution A5 resolution (0--30).
/// @param stats Character vector of stats (subset of mean/sum/count/min/max).
/// @param bands_idx,bands_names Band selection (see `a5_read_raster_rs`).
/// @param value_type Storage type for the value column ("float64" | "float32").
/// @param compression Parquet compression codec ("zstd" | "snappy" | "none").
/// @param threads Worker threads.
/// @param io_concurrency Number of tiles fetched concurrently.
/// @returns The destination path (character scalar) on success.
/// @noRd
/// @keywords internal
#[extendr]
fn a5_raster_to_parquet_rs(
    src: &str,
    dest: &str,
    resolution: i32,
    stats: Vec<String>,
    bands_idx: Vec<i32>,
    bands_names: Vec<String>,
    bbox: Vec<f64>,
    src_nodata: Vec<f64>,
    value_type: &str,
    compression: &str,
    cpu_workers: i32,
    io_concurrency: i32,
) -> Result<String> {
    if !(0..=30).contains(&resolution) {
        return Err(A5CogError::Invalid(format!(
            "resolution must be 0..=30, got {resolution}"
        )));
    }
    if !bands_idx.is_empty() && !bands_names.is_empty() {
        return Err(A5CogError::Invalid(
            "specify bands by index OR by name, not both".into(),
        ));
    }
    let stats_e = parse_stats(&stats)?;
    let value_type_e = crate::parquet_write::ValueType::parse(value_type)?;
    let compression_e = crate::parquet_write::CompressionChoice::parse(compression)?;
    let cpu_workers = cpu_workers.max(1) as usize;
    let io_concurrency = io_concurrency.max(1) as usize;

    let runtime = crate::runtime::shared_runtime()?;

    let prof = profile_enabled();
    if prof {
        reset_timers();
    }
    let t0 = Instant::now();

    let bbox_opt = parse_bbox_arg(bbox)?;
    let src_nodata_opt = parse_src_nodata_arg(src_nodata)?;

    let out: Output = runtime.block_on(read_raster_async(
        src,
        resolution,
        stats_e,
        bands_idx,
        bands_names,
        bbox_opt,
        src_nodata_opt,
        cpu_workers,
        io_concurrency,
    ))?;

    if prof {
        print_timers(t0.elapsed().as_secs_f64());
    }

    crate::parquet_write::write_arrow_parquet(
        dest,
        out.cells,
        out.flat_values,
        out.n_bands,
        &out.band_names,
        &out.stats,
        resolution,
        value_type_e,
        compression_e,
    )?;

    Ok(dest.to_string())
}

/// Sample one pixel value per A5 cell. Inverse / cell-driven path.
///
/// @param src Path or URL string.
/// @param cells_raw a5R-style cell list (b1..b8 raw fields).
/// @param bands_idx,bands_names Band selection.
/// @param src_nodata Length-1 vec or empty for no override.
/// @param threads,io_concurrency See `a5_read_raster_rs`.
/// @returns A list with `cell` (b1..b8 raw), `bands` (named numeric), and
///   `band_names`.
/// @noRd
/// @keywords internal
#[extendr]
fn a5_sample_at_cells_rs(
    src: &str,
    cells_raw: List,
    bands_idx: Vec<i32>,
    bands_names: Vec<String>,
    src_nodata: Vec<f64>,
    cpu_workers: i32,
    io_concurrency: i32,
) -> Result<Robj> {
    if !bands_idx.is_empty() && !bands_names.is_empty() {
        return Err(A5CogError::Invalid(
            "specify bands by index OR by name, not both".into(),
        ));
    }
    let cpu_workers = cpu_workers.max(1) as usize;
    let io_concurrency = io_concurrency.max(1) as usize;
    let src_nodata_opt = parse_src_nodata_arg(src_nodata)?;
    let cells_in = crate::cell_raw::raw8_list_to_u64s(&cells_raw);

    let runtime = crate::runtime::shared_runtime()?;

    let out: crate::sample::CentroidOutput =
        runtime.block_on(crate::sample::sample_at_cells_async(
            src,
            cells_in,
            bands_idx,
            bands_names,
            src_nodata_opt,
            cpu_workers,
            io_concurrency,
        ))?;

    let cell_list = u64s_to_raw8_list(&out.cells);
    let n_cells = out.cells.len();
    let n_bands = out.n_bands;
    let mut band_pairs: Vec<(String, Robj)> = Vec::with_capacity(n_bands);
    for (b, name) in out.band_names.iter().enumerate() {
        let mut col: Vec<f64> = Vec::with_capacity(n_cells);
        for i in 0..n_cells {
            col.push(out.flat[i * n_bands + b]);
        }
        band_pairs.push((name.clone(), Robj::from(col)));
    }
    let bands = List::from_pairs(band_pairs);
    let band_names: Vec<&str> = out.band_names.iter().map(|s| s.as_str()).collect();
    Ok(list!(cell = cell_list, bands = bands, band_names = band_names).into())
}

/// Compute the WGS84 lon/lat bbox of the raster at `src`, by projecting the
/// 4 corners + 4 edge midpoints of the raster's projected extent into
/// WGS84 and taking the axis-aligned envelope.
/// @returns `c(xmin, ymin, xmax, ymax)`.
/// @noRd
/// @keywords internal
#[extendr]
fn a5_raster_bbox_lonlat_rs(src: &str) -> Result<Vec<f64>> {
    let runtime = crate::runtime::shared_runtime()?;
    runtime.block_on(async move {
        let (store, path) = parse_src(src)?;
        let reader = ObjectReader::new(store, path);
        let cache = ReadaheadMetadataCache::new(reader.clone());
        let mut meta = TiffMetadataReader::try_open(&cache).await?;
        let ifds = meta.read_all_ifds(&cache).await?;
        let endianness = meta.endianness();
        let tiff = TIFF::new(ifds, endianness);
        let ifd = tiff
            .ifds()
            .first()
            .ok_or_else(|| A5CogError::Invalid("no IFDs".into()))?
            .clone();
        let geo = ifd
            .geo_key_directory()
            .ok_or(A5CogError::MissingGeoKey("GeoKeyDirectory"))?;
        let src_proj = crate::geo::build_src_proj(geo)?;
        let dst_proj = Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs")?;
        let gt = crate::geo::extract_geotransform(&ifd)?;
        let w = ifd.image_width() as f64;
        let h = ifd.image_height() as f64;

        // 8 sample points around the raster footprint
        let pts_px = [
            (0.0, 0.0), (w, 0.0), (0.0, h), (w, h),
            (w * 0.5, 0.0), (w * 0.5, h), (0.0, h * 0.5), (w, h * 0.5),
        ];
        let mut points: Vec<(f64, f64, f64)> = pts_px
            .iter()
            .map(|&(c, r)| {
                let x = gt.0[0] + c * gt.0[1] + r * gt.0[2];
                let y = gt.0[3] + c * gt.0[4] + r * gt.0[5];
                (x, y, 0.0)
            })
            .collect();
        if src_proj.is_latlong() {
            for p in &mut points {
                p.0 = p.0.to_radians();
                p.1 = p.1.to_radians();
            }
        }
        proj_transform(&src_proj, &dst_proj, &mut points[..])?;
        if dst_proj.is_latlong() {
            for p in &mut points {
                p.0 = p.0.to_degrees();
                p.1 = p.1.to_degrees();
            }
        }
        let mut xmin = f64::INFINITY;
        let mut ymin = f64::INFINITY;
        let mut xmax = f64::NEG_INFINITY;
        let mut ymax = f64::NEG_INFINITY;
        for &(x, y, _) in &points {
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            if x < xmin { xmin = x; }
            if x > xmax { xmax = x; }
            if y < ymin { ymin = y; }
            if y > ymax { ymax = y; }
        }
        if !xmin.is_finite() {
            return Err(A5CogError::Invalid(
                "could not project raster footprint into WGS84".into(),
            ));
        }
        Ok(vec![xmin, ymin, xmax, ymax])
    })
}

extendr_module! {
    mod read;
    fn a5_read_raster_rs;
    fn a5_read_raster_flat_rs;
    fn a5_raster_to_parquet_rs;
    fn a5_sample_at_cells_rs;
    fn a5_raster_bbox_lonlat_rs;
}
