//! Several rasters in one read.
//!
//! A read takes one or more sources. With `separate`, each source adds its
//! bands to the output, like `gdalbuildvrt -separate`. Without it, the
//! sources are pieces of one raster: they must share bands and pixel size,
//! and every valid pixel of every source feeds the same accumulators.
//!
//! This module opens the sources' headers concurrently and resolves the band
//! selection over all of them into, per source, the local bands to read and
//! the output column each one lands in (its slot map).

use async_tiff::ImageFileDirectory;
use async_tiff::metadata::TiffMetadataReader;
use async_tiff::reader::{Endianness, ObjectReader};

use crate::range_cache::CachedReader;
use async_tiff::TIFF;
use proj4rs::Proj;

use crate::error::{A5CogError, Result};
use crate::geo::{
    build_src_proj, build_src_proj_described, extract_geotransform, parse_band_descriptions,
    parse_band_scale_offset,
};
use crate::store::{cache_key, parse_src, StoreOpts};

/// One source as passed from R: its path or URL and its label (the name in
/// `src`, else the file name without extension), used for band names under
/// `separate`.
#[derive(Clone)]
pub(crate) struct SourceSpec {
    pub src: String,
    pub label: String,
}

/// Build the source list from the extendr arguments; `labels` may be empty
/// (labels then default to the source string).
pub(crate) fn source_specs(src: Vec<String>, labels: Vec<String>) -> Result<Vec<SourceSpec>> {
    if src.is_empty() {
        return Err(A5CogError::Invalid("at least one source is required".into()));
    }
    if !labels.is_empty() && labels.len() != src.len() {
        return Err(A5CogError::Invalid(format!(
            "{} labels for {} sources",
            labels.len(),
            src.len()
        )));
    }
    Ok(src
        .into_iter()
        .enumerate()
        .map(|(i, s)| SourceSpec {
            label: labels.get(i).cloned().unwrap_or_else(|| s.clone()),
            src: s,
        })
        .collect())
}

/// A source with its header read: every IFD, and the band count and names
/// of the full-resolution image.
pub(crate) struct OpenedSource {
    pub spec: SourceSpec,
    pub reader: CachedReader,
    pub tiff: TIFF,
    pub endianness: Endianness,
    pub ifd0: ImageFileDirectory,
    pub n_bands: usize,
    /// GDAL band descriptions, else `band_NN`.
    pub band_names: Vec<String>,
}

async fn open_source(spec: SourceSpec, store_opts: &StoreOpts) -> Result<OpenedSource> {
    let (store, path) = parse_src(&spec.src, store_opts)?;
    let reader = CachedReader::new(ObjectReader::new(store, path), cache_key(&spec.src, store_opts));
    let cache = crate::meta_cache::ChunkedMetadataCache::new(reader.clone());
    let mut meta = TiffMetadataReader::try_open(&cache).await?;
    let ifds = meta.read_all_ifds(&cache).await?;
    let endianness = meta.endianness();
    let tiff = TIFF::new(ifds, endianness);
    let ifd0 = tiff
        .ifds()
        .first()
        .ok_or_else(|| A5CogError::Invalid(format!("{}: no IFDs", spec.src)))?
        .clone();
    let n_bands = ifd0.samples_per_pixel() as usize;
    let described = parse_band_descriptions(&ifd0, n_bands);
    let band_names = if described.is_empty() {
        (0..n_bands).map(|i| format!("band_{:02}", i + 1)).collect()
    } else {
        described
    };
    Ok(OpenedSource { spec, reader, tiff, endianness, ifd0, n_bands, band_names })
}

/// Open every source's header concurrently, in input order.
pub(crate) async fn open_sources(specs: Vec<SourceSpec>, store_opts: &StoreOpts) -> Result<Vec<OpenedSource>> {
    futures::future::try_join_all(specs.into_iter().map(|s| open_source(s, store_opts))).await
}

/// The band selection resolved over all sources.
pub(crate) struct BandPlan {
    /// Per source: local 0-based bands to read, in output order.
    pub selected: Vec<Vec<usize>>,
    /// Per source: output column of each entry of `selected`.
    pub slots: Vec<Vec<usize>>,
    /// Output band names, one per column.
    pub names: Vec<String>,
}

impl BandPlan {
    pub fn n_out(&self) -> usize {
        self.names.len()
    }
}

/// Resolve `bands_idx` (1-based) or `bands_names` against one band list;
/// both empty selects every band.
fn resolve_selection(all: &[String], bands_idx: &[i32], bands_names: &[String]) -> Result<Vec<usize>> {
    let n = all.len();
    if !bands_idx.is_empty() {
        bands_idx
            .iter()
            .map(|&i| {
                if i < 1 || (i as usize) > n {
                    Err(A5CogError::Invalid(format!("band index {i} out of range 1..={n}")))
                } else {
                    Ok((i - 1) as usize)
                }
            })
            .collect()
    } else if !bands_names.is_empty() {
        bands_names
            .iter()
            .map(|name| {
                all.iter().position(|d| d == name).ok_or_else(|| {
                    A5CogError::Invalid(format!("band name {name:?} not found; available: {all:?}"))
                })
            })
            .collect()
    } else {
        Ok((0..n).collect())
    }
}

/// Resolve the band selection. Without `separate`, every source must have
/// the same bands, and each reads the same selection into the same columns.
/// With `separate`, the selection indexes the combined band list, in which a
/// single-band source is named by its label and a multi-band source's bands
/// are `<label>_<band>`.
pub(crate) fn plan_bands(
    sources: &[OpenedSource],
    separate: bool,
    bands_idx: &[i32],
    bands_names: &[String],
) -> Result<BandPlan> {
    if !separate {
        let first = &sources[0];
        for s in &sources[1..] {
            if s.band_names != first.band_names {
                return Err(A5CogError::Invalid(format!(
                    "sources combined into one raster must have the same bands, but {} has {} ({}) \
                     and {} has {} ({}); to read each source's bands as separate columns, set \
                     `separate = TRUE`",
                    first.spec.src,
                    first.n_bands,
                    first.band_names.join(", "),
                    s.spec.src,
                    s.n_bands,
                    s.band_names.join(", "),
                )));
            }
        }
        let sel = resolve_selection(&first.band_names, bands_idx, bands_names)?;
        let names = sel.iter().map(|&b| first.band_names[b].clone()).collect();
        let slots: Vec<usize> = (0..sel.len()).collect();
        return Ok(BandPlan {
            selected: vec![sel; sources.len()],
            slots: vec![slots; sources.len()],
            names,
        });
    }
    // combined list of (source, local band) with its output name
    let mut combined: Vec<(usize, usize)> = Vec::new();
    let mut combined_names: Vec<String> = Vec::new();
    for (s, src) in sources.iter().enumerate() {
        for b in 0..src.n_bands {
            combined.push((s, b));
            combined_names.push(if src.n_bands == 1 {
                src.spec.label.clone()
            } else {
                format!("{}_{}", src.spec.label, src.band_names[b])
            });
        }
    }
    let mut seen = std::collections::HashSet::new();
    for name in &combined_names {
        if !seen.insert(name.as_str()) {
            return Err(A5CogError::Invalid(format!(
                "band name {name:?} appears more than once across sources; name the sources \
                 (`src = c(red = ..., green = ...)`) to make the names unique"
            )));
        }
    }
    let sel = resolve_selection(&combined_names, bands_idx, bands_names)?;
    let mut selected = vec![Vec::new(); sources.len()];
    let mut slots = vec![Vec::new(); sources.len()];
    let mut names = Vec::with_capacity(sel.len());
    for (out, &k) in sel.iter().enumerate() {
        let (s, b) = combined[k];
        selected[s].push(b);
        slots[s].push(out);
        names.push(combined_names[k].clone());
    }
    Ok(BandPlan { selected, slots, names })
}

/// Ground size in metres of the pixel at the centre of the full-resolution
/// image: its width and height, measured on the WGS 84 sphere between
/// projected neighbouring pixel centres. Unlike the nominal size in CRS
/// units, this compares across CRSs (UTM zones, Web Mercator).
fn ground_pixel_size_m(src: &OpenedSource) -> Result<(f64, f64)> {
    let geo = src
        .ifd0
        .geo_key_directory()
        .ok_or(A5CogError::MissingGeoKey("GeoKeyDirectory"))?;
    let src_proj = build_src_proj(geo)?;
    let dst_proj = Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs")?;
    let gt = extract_geotransform(&src.ifd0)?;
    let c = src.ifd0.image_width() as f64 * 0.5;
    let r = src.ifd0.image_height() as f64 * 0.5;
    let mut pts: Vec<(f64, f64, f64)> = [(c, r), (c + 1.0, r), (c, r + 1.0)]
        .iter()
        .map(|&(col, row)| {
            let (x, y) = gt.pixel_xy(col, row);
            if src_proj.is_latlong() { (x.to_radians(), y.to_radians(), 0.0) } else { (x, y, 0.0) }
        })
        .collect();
    crate::read::proj_points(&src_proj, &dst_proj, &mut pts[..])?;
    // dst is longlat: radians
    let dist = |a: (f64, f64, f64), b: (f64, f64, f64)| -> f64 {
        const R: f64 = 6_371_008.8;
        let h = ((b.1 - a.1) * 0.5).sin().powi(2)
            + a.1.cos() * b.1.cos() * ((b.0 - a.0) * 0.5).sin().powi(2);
        2.0 * R * h.sqrt().asin()
    };
    let (dx, dy) = (dist(pts[0], pts[1]), dist(pts[0], pts[2]));
    if !dx.is_finite() || !dy.is_finite() {
        return Err(A5CogError::Invalid(format!(
            "{}: could not measure the pixel size",
            src.spec.src
        )));
    }
    Ok((dx, dy))
}

/// How the source's CRS was resolved: `EPSG:<code>` or a proj string.
fn crs_description(src: &OpenedSource) -> Result<String> {
    let geo = src
        .ifd0
        .geo_key_directory()
        .ok_or(A5CogError::MissingGeoKey("GeoKeyDirectory"))?;
    Ok(build_src_proj_described(geo)?.1)
}

/// Relative tolerance on ground pixel size between sources combined into
/// one raster: covers UTM scale factor (0.9996 to about 1.001 across a zone)
/// and Web Mercator tiles of one nominal size at different latitudes, but
/// not a change of product resolution.
const PIXEL_SIZE_TOL: f64 = 0.02;

/// Sources combined into one raster must have the same pixel size: pooling
/// pixel counts across resolutions would weight the finer source by its
/// pixel density. Same CRS and same nominal size always pass.
pub(crate) fn check_same_pixel_size(sources: &[OpenedSource]) -> Result<()> {
    if sources.len() < 2 {
        return Ok(());
    }
    let first = &sources[0];
    let gt0 = extract_geotransform(&first.ifd0)?;
    let crs0 = crs_description(first)?;
    let size0 = ground_pixel_size_m(first)?;
    for s in &sources[1..] {
        let gt = extract_geotransform(&s.ifd0)?;
        let same_nominal = (gt.0[1] - gt0.0[1]).abs() <= 1e-9 * gt0.0[1].abs()
            && (gt.0[5] - gt0.0[5]).abs() <= 1e-9 * gt0.0[5].abs()
            && crs_description(s)? == crs0;
        if same_nominal {
            continue;
        }
        let size = ground_pixel_size_m(s)?;
        let rel = |a: f64, b: f64| (a - b).abs() / a.max(b);
        if rel(size.0, size0.0) > PIXEL_SIZE_TOL || rel(size.1, size0.1) > PIXEL_SIZE_TOL {
            return Err(A5CogError::Invalid(format!(
                "sources combined into one raster must have the same pixel size, but {} has \
                 {:.2} x {:.2} m pixels and {} has {:.2} x {:.2} m; resample them to one \
                 resolution, or set `separate = TRUE` to read them as separate bands",
                first.spec.src, size0.0, size0.1, s.spec.src, size.0, size.1,
            )));
        }
    }
    Ok(())
}

/// Sources combined into one raster and read as stored values (no `scoff`
/// or `dequant`) must store each selected band with the same scale and
/// offset, or codes of different meaning would be pooled: Sentinel-2 L2A
/// from processing baseline 04.00 on, for one, adds an offset of -1000.
/// A missing tag counts as scale 1, offset 0.
pub(crate) fn check_same_scale_offset(sources: &[OpenedSource], selected: &[Vec<usize>]) -> Result<()> {
    if sources.len() < 2 {
        return Ok(());
    }
    let tags = |s: &OpenedSource| {
        let (scale, offset) = parse_band_scale_offset(&s.ifd0, s.n_bands);
        let or = |v: f64, d: f64| if v.is_nan() { d } else { v };
        (0..s.n_bands).map(|b| (or(scale[b], 1.0), or(offset[b], 0.0))).collect::<Vec<_>>()
    };
    let first = tags(&sources[0]);
    for (s, sel) in sources.iter().zip(selected).skip(1) {
        let t = tags(s);
        for &b in sel {
            if t[b] != first[b] {
                return Err(A5CogError::Invalid(format!(
                    "sources combined into one raster store band {} ({}) with different scale \
                     and offset: {} has scale {} offset {}, {} has scale {} offset {}; set \
                     `scoff = TRUE` to decode each source before pooling",
                    b + 1,
                    s.band_names[b],
                    sources[0].spec.src,
                    first[b].0,
                    first[b].1,
                    s.spec.src,
                    t[b].0,
                    t[b].1,
                )));
            }
        }
    }
    Ok(())
}
