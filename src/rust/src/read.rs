//! Forward pixel-driven raster → A5 cell aggregation.

use std::sync::Arc;

use ahash::AHashMap;
use async_tiff::decoder::DecoderRegistry;
use async_tiff::metadata::TiffMetadataReader;
use async_tiff::reader::{AsyncFileReader, ObjectReader};
use async_tiff::tags::PlanarConfiguration;
use async_tiff::{TIFF, TypedArray};
use extendr_api::prelude::*;
use futures::stream::{self, StreamExt, TryStreamExt};
use crate::store::{parse_src, parse_store_opts, StoreOpts};
use proj4rs::Proj;
use proj4rs::transform::transform as proj_transform;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::cell_mask::{CellMask, MaskCache};
use crate::grid_proj::{bbox_classify, ExactMap, GridProjector};
use crate::locator::{a5_spherical, CellLocator, NO_CELL};
use crate::cell_raw::{raw8_list_to_u64s, u64s_to_raw8_list};
use crate::error::{A5CogError, Result};
use crate::geo::{
    GeoTransform, build_src_proj, extract_geotransform, is_nodata, parse_band_descriptions,
    parse_nodata,
};

// stage timers (only emit if A5PX_PROFILE env var is set, e.g. A5PX_PROFILE=1)
static T_FETCH_NS: AtomicU64 = AtomicU64::new(0);
static T_DECODE_NS: AtomicU64 = AtomicU64::new(0);
static T_PROJ_NS: AtomicU64 = AtomicU64::new(0);
static T_INDEX_NS: AtomicU64 = AtomicU64::new(0);
static T_STRIPE_MERGE_NS: AtomicU64 = AtomicU64::new(0);
// serial tail after the consumers finish
static T_REDUCE_NS: AtomicU64 = AtomicU64::new(0);
static T_FLATTEN_NS: AtomicU64 = AtomicU64::new(0);
// sub-stage timers inside the per-pixel loop
static T_PIX_READ_NS: AtomicU64 = AtomicU64::new(0);
static T_A5_CELL_NS: AtomicU64 = AtomicU64::new(0);
static T_HM_NS: AtomicU64 = AtomicU64::new(0);
static T_PUSH_NS: AtomicU64 = AtomicU64::new(0);
// overlay-mode fast-path effectiveness (pixel counts, not timings)
static N_OVERLAY_INTERIOR: AtomicU64 = AtomicU64::new(0);
static N_OVERLAY_BOUNDARY: AtomicU64 = AtomicU64::new(0);
// grid projector: windows built / windows that fell back to exact projection
static N_WINDOWS: AtomicU64 = AtomicU64::new(0);
static N_WINDOWS_EXACT: AtomicU64 = AtomicU64::new(0);
// whole-call phases (wall)
static T_OPEN_NS: AtomicU64 = AtomicU64::new(0);
static T_OPEN_STORE_NS: AtomicU64 = AtomicU64::new(0);
static T_OPEN_HEADER_NS: AtomicU64 = AtomicU64::new(0);
static T_OPEN_IFDS_NS: AtomicU64 = AtomicU64::new(0);
static T_PLAN_NS: AtomicU64 = AtomicU64::new(0);
static T_PIPELINE_NS: AtomicU64 = AtomicU64::new(0);
static T_ROUT_NS: AtomicU64 = AtomicU64::new(0);
pub(crate) static T_PQ_COLUMNS_NS: AtomicU64 = AtomicU64::new(0);
pub(crate) static T_PQ_WRITE_NS: AtomicU64 = AtomicU64::new(0);
// pipeline waits (summed over tasks): consumers blocked waiting for a tile,
// producer tasks blocked on the full channel
static T_RECV_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static T_SEND_WAIT_NS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn profile_enabled() -> bool {
    std::env::var_os("A5PX_PROFILE").is_some()
}

fn reset_timers() {
    for t in [
        &T_FETCH_NS, &T_DECODE_NS, &T_PROJ_NS,
        &T_INDEX_NS, &T_STRIPE_MERGE_NS, &T_REDUCE_NS, &T_FLATTEN_NS,
        &T_PIX_READ_NS, &T_A5_CELL_NS, &T_HM_NS, &T_PUSH_NS,
        &N_OVERLAY_INTERIOR, &N_OVERLAY_BOUNDARY, &N_WINDOWS, &N_WINDOWS_EXACT,
        &T_OPEN_NS, &T_OPEN_STORE_NS, &T_OPEN_HEADER_NS, &T_OPEN_IFDS_NS, &T_PLAN_NS, &T_PIPELINE_NS, &T_ROUT_NS, &T_PQ_COLUMNS_NS, &T_PQ_WRITE_NS,
        &T_RECV_WAIT_NS, &T_SEND_WAIT_NS,
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
    eprintln!("[a5px profile, total {:.3} s = whole call; stage lines sum across tile workers]", total);
    eprintln!("  call phases (wall):");
    one("  open + metadata", T_OPEN_NS.load(Ordering::Relaxed));
    one("    store client", T_OPEN_STORE_NS.load(Ordering::Relaxed));
    one("    tiff header", T_OPEN_HEADER_NS.load(Ordering::Relaxed));
    one("    read all IFDs", T_OPEN_IFDS_NS.load(Ordering::Relaxed));
    one("  tile planning", T_PLAN_NS.load(Ordering::Relaxed));
    one("  read pipeline", T_PIPELINE_NS.load(Ordering::Relaxed));
    one("  assemble output", T_REDUCE_NS.load(Ordering::Relaxed));
    one("  flatten output", T_FLATTEN_NS.load(Ordering::Relaxed));
    one("  R output build", T_ROUT_NS.load(Ordering::Relaxed));
    one("  parquet columns", T_PQ_COLUMNS_NS.load(Ordering::Relaxed));
    one("  parquet encode+write", T_PQ_WRITE_NS.load(Ordering::Relaxed));
    eprintln!("  pipeline waits (summed over tasks):");
    one("  consumer idle (no tile)", T_RECV_WAIT_NS.load(Ordering::Relaxed));
    one("  producer blocked (full)", T_SEND_WAIT_NS.load(Ordering::Relaxed));
    eprintln!("  stages:");
    one("io fetch", T_FETCH_NS.load(Ordering::Relaxed));
    one("decode", T_DECODE_NS.load(Ordering::Relaxed));
    one("proj transform", T_PROJ_NS.load(Ordering::Relaxed));
    one("a5 index + accum", T_INDEX_NS.load(Ordering::Relaxed));
    eprintln!("    of which:");
    one("  nodata validity", T_PIX_READ_NS.load(Ordering::Relaxed));
    one("  a5 lonlat->cell", T_A5_CELL_NS.load(Ordering::Relaxed));
    one("  run build", T_HM_NS.load(Ordering::Relaxed));
    one("  read + push runs", T_PUSH_NS.load(Ordering::Relaxed));
    one("partition merge", T_STRIPE_MERGE_NS.load(Ordering::Relaxed));
    let n_win = N_WINDOWS.load(Ordering::Relaxed);
    if n_win > 0 {
        eprintln!(
            "  projection windows: {n_win}, exact-only {} ({:.2}%)",
            N_WINDOWS_EXACT.load(Ordering::Relaxed),
            100.0 * N_WINDOWS_EXACT.load(Ordering::Relaxed) as f64 / n_win as f64
        );
    }
    let n_int = N_OVERLAY_INTERIOR.load(Ordering::Relaxed);
    let n_bnd = N_OVERLAY_BOUNDARY.load(Ordering::Relaxed);
    if n_int + n_bnd > 0 {
        eprintln!(
            "  overlay interior fast path: {n_int} of {} pixels ({:.1}%)",
            n_int + n_bnd,
            100.0 * n_int as f64 / (n_int + n_bnd) as f64
        );
    }
}

// ---------------------------------------------------------------------------
// stat selector

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stat {
    Mean,
    Sum,
    Count,
    Min,
    Max,
    Var,
    Sd,
    /// Most-weighted class code (categorical rasters). Backed by the
    /// per-cell class-weight map rather than the continuous accumulator.
    Majority,
}

impl Stat {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "mean" => Ok(Self::Mean),
            "sum" => Ok(Self::Sum),
            "count" => Ok(Self::Count),
            "min" => Ok(Self::Min),
            "max" => Ok(Self::Max),
            "var" => Ok(Self::Var),
            "sd" => Ok(Self::Sd),
            "majority" => Ok(Self::Majority),
            other => Err(A5CogError::Invalid(format!("unknown stat: {other}"))),
        }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Mean => "mean",
            Self::Sum => "sum",
            Self::Count => "count",
            Self::Min => "min",
            Self::Max => "max",
            Self::Var => "var",
            Self::Sd => "sd",
            Self::Majority => "majority",
        }
    }

    /// Whether this stat reads the continuous (weighted Welford) accumulator.
    fn needs_cont(&self) -> bool {
        !matches!(self, Self::Majority)
    }
}

/// Split the R-side stat vector into enum stats plus the `fractions` flag.
/// "fractions" has a per-cell variable-length output (class -> weight share)
/// so it is not a `Stat`; the R wrappers enforce that it arrives alone.
/// Returns (per-band stats, fractions?, npix?).
fn parse_stats(stats: &[String]) -> Result<(Vec<Stat>, bool, bool)> {
    if stats.is_empty() {
        return Err(A5CogError::Invalid("at least one stat is required".into()));
    }
    let fractions = stats.iter().any(|s| s == "fractions");
    if fractions && stats.len() > 1 {
        return Err(A5CogError::Invalid(
            "\"fractions\" must be the only requested stat".into(),
        ));
    }
    let npix = stats.iter().any(|s| s == "npix");
    let parsed: Vec<Stat> = stats
        .iter()
        .filter(|s| s.as_str() != "fractions" && s.as_str() != "npix")
        .map(|s| Stat::parse(s.as_str()))
        .collect::<Result<Vec<_>>>()?;
    Ok((parsed, fractions, npix))
}

/// Per-band accumulation config, derived once from the requested stats.
#[derive(Clone, Copy)]
struct AccCfg {
    has_cont: bool,
    has_cat: bool,
    /// One weight per cell: pixels (or pixel-area weight) with any valid band.
    has_npix: bool,
}

impl AccCfg {
    fn from_stats(stats: &[Stat], fractions: bool, npix: bool) -> Self {
        Self {
            has_npix: npix,
            has_cont: stats.iter().any(|s| s.needs_cont()),
            has_cat: fractions || stats.iter().any(|s| matches!(s, Stat::Majority)),
        }
    }
}

// ---------------------------------------------------------------------------
// pre-aggregation dequantization

/// Per-pixel decode applied before values enter the accumulators, as a lookup
/// table over the integer code domain `[min, min + lut.len())`. Built on the
/// R side (which can evaluate an arbitrary R function over the finite code
/// domain); applied here because nonlinear decodes do not commute with
/// aggregation — the mean of decoded codes is not the decode of the mean.
pub(crate) struct DequantLut {
    pub lut: Vec<f64>,
    pub min: i64,
}

impl DequantLut {
    /// `v` is an integer code read from the raster (exact in f64 for all
    /// supported dtypes). Out-of-range codes cannot occur once the source
    /// dtype has been validated; NaN is a safe backstop, not a code path.
    #[inline]
    pub fn apply(&self, v: f64) -> f64 {
        let i = (v as i64).wrapping_sub(self.min) as usize;
        self.lut.get(i).copied().unwrap_or(f64::NAN)
    }
}

/// Decode the extendr-passed LUT args: empty `lut` means "no dequant".
pub(crate) fn parse_dequant_arg(lut: Vec<f64>, min: f64) -> Option<DequantLut> {
    if lut.is_empty() {
        None
    } else {
        Some(DequantLut {
            lut,
            min: min as i64,
        })
    }
}

/// The value range of an integer source of 16 bits or fewer, or `None` for
/// any other dtype. The finite-code-domain features (dequant LUTs and the
/// categorical class maps) are only defined for these sources.
fn integer_code_range(ifd: &async_tiff::ImageFileDirectory) -> Option<(i64, i64)> {
    use async_tiff::DataType;
    match crate::band_fetch::derive_data_type(ifd) {
        Some(DataType::Bool) => Some((0, 1)),
        Some(DataType::UInt8) => Some((0, u8::MAX as i64)),
        Some(DataType::UInt16) => Some((0, u16::MAX as i64)),
        Some(DataType::Int8) => Some((i8::MIN as i64, i8::MAX as i64)),
        Some(DataType::Int16) => Some((i16::MIN as i64, i16::MAX as i64)),
        _ => None,
    }
}

/// Dequantization is only defined for quantized integer codes; require an
/// integer dtype whose full range the LUT covers.
pub(crate) fn validate_dequant_dtype(
    ifd: &async_tiff::ImageFileDirectory,
    dq: &DequantLut,
) -> Result<()> {
    let Some((lo, hi)) = integer_code_range(ifd) else {
        return Err(A5CogError::Unsupported(format!(
            "dequant requires an integer source of 16 bits or fewer; source data type is {:?}",
            crate::band_fetch::derive_data_type(ifd)
        )));
    };
    let covered_hi = dq.min + dq.lut.len() as i64 - 1;
    if lo < dq.min || hi > covered_hi {
        return Err(A5CogError::Invalid(format!(
            "dequant LUT domain [{}, {covered_hi}] does not cover the source dtype range [{lo}, {hi}]",
            dq.min
        )));
    }
    Ok(())
}

/// majority / fractions treat raw codes as class labels; require an integer
/// dtype of 16 bits or fewer so the class space is finite.
fn validate_categorical_dtype(ifd: &async_tiff::ImageFileDirectory) -> Result<()> {
    if integer_code_range(ifd).is_none() {
        return Err(A5CogError::Unsupported(format!(
            "majority/fractions require an integer source of 16 bits or fewer; source data type is {:?}",
            crate::band_fetch::derive_data_type(ifd)
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// per-cell, per-band running accumulator

/// Per-band running accumulator. Three layouts exist so memory scales with
/// the statistics actually requested: `mean` / `sum` / `count` need only a
/// weighted sum and a weight (16 B per band per cell), `min` / `max` add
/// two extremes, and `var` / `sd` add the weighted Welford state. For a
/// 64-band embedding raster the slim layout is a third of the memory of
/// the full one, which is what bounds how many cells a read can hold.
pub(crate) trait AccLayout: Copy + Send + Sync + 'static {
    fn new() -> Self;
    fn push(&mut self, v: f64, w: f64);
    /// Push a run of unit-weight values (consecutive pixels of one cell).
    #[inline]
    fn push_run(&mut self, vals: &[f64]) {
        for &v in vals {
            self.push(v, 1.0);
        }
    }
    fn merge(&mut self, other: &Self);
    fn finalise(&self, stat: Stat) -> f64;
}

/// Which accumulator layout a stat set needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Sum,
    Range,
    Full,
}

fn layout_for(stats: &[Stat]) -> Layout {
    if stats.iter().any(|s| matches!(s, Stat::Var | Stat::Sd)) {
        Layout::Full
    } else if stats.iter().any(|s| matches!(s, Stat::Min | Stat::Max)) {
        Layout::Range
    } else {
        Layout::Sum
    }
}

/// Weighted sum Σ w·v and total weight Σ w. Under forward/centroid sampling
/// every weight is 1.0, so `sum` is the plain sum and `sum_w` the pixel
/// count; under overlay sampling the weights are pixel-cell overlap
/// fractions, making Sum mass-preserving and Count the effective
/// (fractional) source-pixel count.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AccSum {
    sum: f64,
    sum_w: f64,
}

impl AccLayout for AccSum {
    #[inline]
    fn new() -> Self {
        Self { sum: 0.0, sum_w: 0.0 }
    }
    #[inline]
    fn push(&mut self, v: f64, w: f64) {
        if w <= 0.0 {
            return;
        }
        self.sum += w * v;
        self.sum_w += w;
    }
    #[inline]
    fn push_run(&mut self, vals: &[f64]) {
        let mut s = 0.0;
        for &v in vals {
            s += v;
        }
        self.sum += s;
        self.sum_w += vals.len() as f64;
    }
    #[inline]
    fn merge(&mut self, other: &Self) {
        self.sum += other.sum;
        self.sum_w += other.sum_w;
    }
    #[inline]
    fn finalise(&self, stat: Stat) -> f64 {
        finalise_sum(self.sum, self.sum_w, stat)
    }
}

/// `AccSum` plus presence-based min / max (any positive overlap counts fully).
#[derive(Clone, Copy, Debug)]
pub(crate) struct AccRange {
    sum: f64,
    sum_w: f64,
    min: f64,
    max: f64,
}

impl AccLayout for AccRange {
    #[inline]
    fn new() -> Self {
        Self { sum: 0.0, sum_w: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY }
    }
    #[inline]
    fn push(&mut self, v: f64, w: f64) {
        if w <= 0.0 {
            return;
        }
        self.sum += w * v;
        self.sum_w += w;
        if v < self.min {
            self.min = v;
        }
        if v > self.max {
            self.max = v;
        }
    }
    #[inline]
    fn push_run(&mut self, vals: &[f64]) {
        let mut s = 0.0;
        let (mut lo, mut hi) = (self.min, self.max);
        for &v in vals {
            s += v;
            if v < lo {
                lo = v;
            }
            if v > hi {
                hi = v;
            }
        }
        self.sum += s;
        self.sum_w += vals.len() as f64;
        self.min = lo;
        self.max = hi;
    }
    #[inline]
    fn merge(&mut self, other: &Self) {
        self.sum += other.sum;
        self.sum_w += other.sum_w;
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
            Stat::Min => if self.sum_w == 0.0 { f64::NAN } else { self.min },
            Stat::Max => if self.sum_w == 0.0 { f64::NAN } else { self.max },
            _ => finalise_sum(self.sum, self.sum_w, stat),
        }
    }
}

/// `AccRange` plus weighted Welford (West 1979) running mean and sum of
/// squared deviations for var / sd. The mean is also derivable as
/// sum / sum_w; `mean_w` is kept so the M2 update stays numerically stable
/// when combining workers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AccFull {
    sum: f64,
    sum_w: f64,
    min: f64,
    max: f64,
    mean_w: f64,
    m2: f64,
}

impl AccLayout for AccFull {
    #[inline]
    fn new() -> Self {
        Self {
            sum: 0.0,
            sum_w: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            mean_w: 0.0,
            m2: 0.0,
        }
    }
    #[inline]
    fn push(&mut self, v: f64, w: f64) {
        if w <= 0.0 {
            return;
        }
        self.sum += w * v;
        self.sum_w += w;
        if v < self.min {
            self.min = v;
        }
        if v > self.max {
            self.max = v;
        }
        let delta = v - self.mean_w;
        self.mean_w += (w / self.sum_w) * delta;
        self.m2 += w * delta * (v - self.mean_w);
    }
    #[inline]
    fn merge(&mut self, other: &Self) {
        if other.sum_w == 0.0 {
            return;
        }
        if self.sum_w == 0.0 {
            *self = *other;
            return;
        }
        let w_a = self.sum_w;
        let w_b = other.sum_w;
        let w = w_a + w_b;
        let delta = other.mean_w - self.mean_w;
        let new_mean = self.mean_w + delta * w_b / w;
        self.m2 += other.m2 + delta * delta * w_a * w_b / w;
        self.mean_w = new_mean;
        self.sum += other.sum;
        self.sum_w += other.sum_w;
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
            Stat::Min => if self.sum_w == 0.0 { f64::NAN } else { self.min },
            Stat::Max => if self.sum_w == 0.0 { f64::NAN } else { self.max },
            // Sample variance / stdev with frequency weights: divisor
            // Σw - 1, matching R's var() / sd() when all weights are 1.
            // Cells with effective sample size <= 1 yield NaN.
            Stat::Var => if self.sum_w <= 1.0 { f64::NAN } else { self.m2 / (self.sum_w - 1.0) },
            Stat::Sd => {
                if self.sum_w <= 1.0 { f64::NAN } else { (self.m2 / (self.sum_w - 1.0)).sqrt() }
            }
            _ => finalise_sum(self.sum, self.sum_w, stat),
        }
    }
}

/// Stats derivable from the weighted sum alone. Stats a layout does not
/// carry are never requested for it (`layout_for` guarantees that), so they
/// finalise to NaN rather than a wrong number.
#[inline]
fn finalise_sum(sum: f64, sum_w: f64, stat: Stat) -> f64 {
    match stat {
        Stat::Mean => if sum_w == 0.0 { f64::NAN } else { sum / sum_w },
        Stat::Sum => sum,
        Stat::Count => sum_w,
        _ => f64::NAN,
    }
}

// ---------------------------------------------------------------------------
// categorical accumulation (majority / fractions)

/// Per-(cell, band) class-weight map. A plain vector with linear scan: a
/// genuinely categorical raster puts a handful of classes in each cell, and
/// the hard cap below turns a continuous raster passed by mistake into a
/// clear error instead of unbounded memory growth.
type ClassWeights = Vec<(i32, f64)>;

const MAX_CLASSES_PER_CELL: usize = 4096;

#[inline]
fn cat_push(m: &mut ClassWeights, class: i32, w: f64) -> Result<()> {
    if w <= 0.0 {
        return Ok(());
    }
    match m.iter_mut().find(|(c, _)| *c == class) {
        Some((_, wt)) => *wt += w,
        None => {
            if m.len() >= MAX_CLASSES_PER_CELL {
                return Err(A5CogError::Invalid(format!(
                    "more than {MAX_CLASSES_PER_CELL} distinct classes in a single cell; \
                     majority/fractions require a categorical raster"
                )));
            }
            m.push((class, w));
        }
    }
    Ok(())
}

/// Most-weighted class, ties broken toward the smallest class code so the
/// result is deterministic regardless of accumulation order. NaN when the
/// cell/band saw no valid pixels.
fn finalise_majority(m: &ClassWeights) -> f64 {
    let mut best: Option<(i32, f64)> = None;
    for &(c, w) in m {
        best = Some(match best {
            None => (c, w),
            Some((bc, bw)) => {
                if w > bw || (w == bw && c < bc) {
                    (c, w)
                } else {
                    (bc, bw)
                }
            }
        });
    }
    match best {
        Some((c, _)) => c as f64,
        None => f64::NAN,
    }
}

/// Per-cell accumulation state: continuous accumulators and/or class-weight
/// maps, one per selected band, allocated only for what the requested stats
/// actually need.
#[derive(Clone)]
/// Accumulators for a set of cells in one contiguous slab: `cont[i * n_out + b]`
/// is band `b` of the i-th cell first seen. One allocation per store instead
/// of one per cell, cache-friendly merges and finalisation, and stable slot
/// indices while cells are added (unlike map entry pointers).
pub(crate) struct CellStore<L: AccLayout> {
    n_out: usize,
    cfg: AccCfg,
    index: AHashMap<u64, u32>,
    cells: Vec<u64>,
    cont: Vec<L>,
    cat: Vec<ClassWeights>,
    /// Per-cell weight (pixels with any valid band), when `cfg.has_npix`.
    npix: Vec<f64>,
}

impl<L: AccLayout> CellStore<L> {
    fn new(n_out: usize, cfg: AccCfg, cap: usize) -> Self {
        Self {
            n_out,
            cfg,
            index: AHashMap::with_capacity(cap),
            cells: Vec::with_capacity(cap),
            cont: Vec::with_capacity(if cfg.has_cont { cap * n_out } else { 0 }),
            cat: Vec::with_capacity(if cfg.has_cat { cap * n_out } else { 0 }),
            npix: Vec::with_capacity(if cfg.has_npix { cap } else { 0 }),
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.cells.len()
    }

    /// Slot of `cell`, adding it if new.
    #[inline]
    fn slot(&mut self, cell: u64) -> u32 {
        if let Some(&s) = self.index.get(&cell) {
            return s;
        }
        let s = self.cells.len() as u32;
        self.cells.push(cell);
        self.index.insert(cell, s);
        if self.cfg.has_cont {
            self.cont.extend(std::iter::repeat(L::new()).take(self.n_out));
        }
        if self.cfg.has_cat {
            self.cat.extend(std::iter::repeat_with(Vec::new).take(self.n_out));
        }
        if self.cfg.has_npix {
            self.npix.push(0.0);
        }
        s
    }

    #[inline]
    fn add_npix(&mut self, slot: u32, w: f64) {
        if self.cfg.has_npix {
            self.npix[slot as usize] += w;
        }
    }

    #[inline]
    fn cont_at(&mut self, slot: u32, b: usize) -> &mut L {
        &mut self.cont[slot as usize * self.n_out + b]
    }

    #[inline]
    fn cat_at(&mut self, slot: u32, b: usize) -> &mut ClassWeights {
        &mut self.cat[slot as usize * self.n_out + b]
    }

    /// Merge cell `i` of `other` into `self` (`self` first in the sum order).
    #[inline]
    fn merge_cell_from(&mut self, other: &CellStore<L>, i: usize) -> Result<()> {
        let n_out = self.n_out;
        let s = self.slot(other.cells[i]) as usize;
        if self.cfg.has_cont {
            for b in 0..n_out {
                self.cont[s * n_out + b].merge(&other.cont[i * n_out + b]);
            }
        }
        if self.cfg.has_cat {
            for b in 0..n_out {
                for &(c, w) in &other.cat[i * n_out + b] {
                    cat_push(&mut self.cat[s * n_out + b], c, w)?;
                }
            }
        }
        if self.cfg.has_npix {
            self.npix[s] += other.npix[i];
        }
        Ok(())
    }
}

impl<L: AccLayout> CellStore<L> {
    /// Finalised value of `stat` for local cell `i`, band `b`.
    #[inline]
    fn value(&self, stat: Stat, i: usize, b: usize) -> f64 {
        let idx = i * self.n_out + b;
        match stat {
            Stat::Majority => finalise_majority(&self.cat[idx]),
            _ => self.cont[idx].finalise(stat),
        }
    }
}

#[inline]
fn partition_of(cell: u64, log2p: u32) -> usize {
    if log2p == 0 {
        return 0;
    }
    (cell.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - log2p)) as usize
}

/// The read's accumulators, spread over `2^log2p` disjoint stores by cell
/// id hash, each behind a mutex. Stripes accumulate locally, then merge
/// into the partitions (one lock per partition touched, held for one
/// group), so cells are copied once per stripe that touches them, the
/// merge work runs on the stripe threads, and nothing is left to reduce
/// when the workers finish. Peak memory is the final stores plus one
/// stripe store per thread.
///
/// Merge order into a partition follows stripe completion, so with more
/// than one worker sums can differ between runs in the last bit; counts,
/// min and max are exact. One worker uses one partition and merges its
/// stripes in order, so its output is reproducible.
pub(crate) struct PartitionedStore<L: AccLayout> {
    parts: Vec<std::sync::Mutex<CellStore<L>>>,
    log2p: u32,
}

impl<L: AccLayout> PartitionedStore<L> {
    /// `expected_cells` sizes the partitions up front so their slabs and
    /// maps do not grow (and copy) while other threads are indexing.
    fn new(log2p: u32, n_out: usize, cfg: AccCfg, expected_cells: usize) -> Self {
        let np = 1usize << log2p;
        let per_part = (expected_cells / np + expected_cells / (np * 8) + 64).min(1 << 26);
        let parts = (0..np)
            .map(|_| std::sync::Mutex::new(CellStore::new(n_out, cfg, per_part)))
            .collect();
        Self { parts, log2p }
    }

    /// Merge a stripe's store into the partitions: cells are grouped by
    /// partition (counting sort) and each group merged under one lock.
    fn absorb(&self, delta: &CellStore<L>) -> Result<()> {
        let n = delta.len();
        if n == 0 {
            return Ok(());
        }
        if self.log2p == 0 {
            let mut guard = self.parts[0]
                .lock()
                .map_err(|_| A5CogError::Internal("partition store poisoned".into()))?;
            for i in 0..n {
                guard.merge_cell_from(delta, i)?;
            }
            return Ok(());
        }
        let np = 1usize << self.log2p;
        let part: Vec<u8> = delta.cells.iter().map(|&c| partition_of(c, self.log2p) as u8).collect();
        let mut start = vec![0usize; np + 1];
        for &p in &part {
            start[p as usize + 1] += 1;
        }
        for p in 0..np {
            start[p + 1] += start[p];
        }
        let mut order: Vec<u32> = vec![0; n];
        let mut fill = start.clone();
        for (i, &p) in part.iter().enumerate() {
            order[fill[p as usize]] = i as u32;
            fill[p as usize] += 1;
        }
        for p in 0..np {
            let idxs = &order[start[p]..start[p + 1]];
            if idxs.is_empty() {
                continue;
            }
            let mut guard = self.parts[p]
                .lock()
                .map_err(|_| A5CogError::Internal("partition store poisoned".into()))?;
            for &i in idxs {
                guard.merge_cell_from(delta, i as usize)?;
            }
        }
        Ok(())
    }

    fn into_parts(self) -> Result<Vec<CellStore<L>>> {
        self.parts
            .into_iter()
            .map(|m| m.into_inner().map_err(|_| A5CogError::Internal("partition store poisoned".into())))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// src parsing


/// Project a batch point by point, mapping per-point failures to NaN so a
/// single unprojectable coordinate (a bbox corner outside a LAEA disc, a
/// pixel beyond an orthographic horizon) does not abort the whole read.
/// Every consumer already skips non-finite coordinates. Projection-level
/// failures (no inverse / forward defined) still propagate.
pub(crate) fn proj_points(src: &Proj, dst: &Proj, points: &mut [(f64, f64, f64)]) -> Result<()> {
    use proj4rs::errors::Error as PE;
    for p in points.iter_mut() {
        match proj_transform(src, dst, p) {
            Ok(()) => {}
            Err(PE::NoInverseProjectionDefined) | Err(PE::NoForwardProjectionDefined) => {
                return Err(A5CogError::Proj(format!(
                    "projection has no inverse/forward transform: {}",
                    src.projname()
                )));
            }
            Err(_) => *p = (f64::NAN, f64::NAN, 0.0),
        }
    }
    Ok(())
}

/// Pixels are compared to nodata after widening to f64, so a Float32 source
/// whose nodata is not exactly representable in f32 (`-9999.9`) would never
/// match unless the sentinel is rounded through f32 the same way.
pub(crate) fn nodata_in_source_precision(
    ifd: &async_tiff::ImageFileDirectory,
    nodata: Option<f64>,
) -> Option<f64> {
    match crate::band_fetch::derive_data_type(ifd) {
        Some(async_tiff::DataType::Float32) => nodata.map(|v| v as f32 as f64),
        _ => nodata,
    }
}

// ---------------------------------------------------------------------------
// pixel sampling helpers — band-major access into the decoded tile

pub(crate) fn read_pixel_chunky_pub(data: &TypedArray, idx: usize) -> f64 {
    read_pixel_chunky(data, idx)
}

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
// typed pixel access

/// Append the valid values of `n` pixels starting at `base` with stride
/// `stride` to `out`: nodata pixels are skipped and the dequant LUT (when
/// present) applied, matching the array type once per run rather than per
/// pixel.
#[inline]
fn read_run_values(
    data: &TypedArray,
    base: usize,
    stride: usize,
    n: usize,
    nodata: Option<f64>,
    dequant: Option<&DequantLut>,
    out: &mut Vec<f64>,
) {
    macro_rules! run {
        ($v:expr, $conv:expr) => {{
            let v = $v;
            for k in 0..n {
                let x: f64 = $conv(v[base + k * stride]);
                if let Some(nd) = nodata {
                    if is_nodata(x, nd) {
                        continue;
                    }
                }
                out.push(match dequant {
                    Some(d) => d.apply(x),
                    None => x,
                });
            }
        }};
    }
    match data {
        TypedArray::UInt8(v) => run!(v, |x: u8| x as f64),
        TypedArray::UInt16(v) => run!(v, |x: u16| x as f64),
        TypedArray::UInt32(v) => run!(v, |x: u32| x as f64),
        TypedArray::UInt64(v) => run!(v, |x: u64| x as f64),
        TypedArray::Int8(v) => run!(v, |x: i8| x as f64),
        TypedArray::Int16(v) => run!(v, |x: i16| x as f64),
        TypedArray::Int32(v) => run!(v, |x: i32| x as f64),
        TypedArray::Int64(v) => run!(v, |x: i64| x as f64),
        TypedArray::Float32(v) => run!(v, |x: f32| x as f64),
        TypedArray::Float64(v) => run!(v, |x: f64| x),
        TypedArray::Bool(v) => run!(v, |x: bool| if x { 1.0 } else { 0.0 }),
    }
}

/// Mark `valid[c] = true` for pixels `c < n` whose value at
/// `base + c * stride` is not `nodata`; returns how many became valid.
#[inline]
fn mark_valid(data: &TypedArray, base: usize, stride: usize, n: usize, nodata: f64, valid: &mut [bool]) -> usize {
    let mut newly = 0usize;
    macro_rules! run {
        ($v:expr, $conv:expr) => {{
            let v = $v;
            for c in 0..n {
                if !valid[c] && !is_nodata($conv(v[base + c * stride]), nodata) {
                    valid[c] = true;
                    newly += 1;
                }
            }
        }};
    }
    match data {
        TypedArray::UInt8(v) => run!(v, |x: u8| x as f64),
        TypedArray::UInt16(v) => run!(v, |x: u16| x as f64),
        TypedArray::UInt32(v) => run!(v, |x: u32| x as f64),
        TypedArray::UInt64(v) => run!(v, |x: u64| x as f64),
        TypedArray::Int8(v) => run!(v, |x: i8| x as f64),
        TypedArray::Int16(v) => run!(v, |x: i16| x as f64),
        TypedArray::Int32(v) => run!(v, |x: i32| x as f64),
        TypedArray::Int64(v) => run!(v, |x: i64| x as f64),
        TypedArray::Float32(v) => run!(v, |x: f32| x as f64),
        TypedArray::Float64(v) => run!(v, |x: f64| x),
        TypedArray::Bool(v) => run!(v, |x: bool| if x { 1.0 } else { 0.0 }),
    }
    newly
}

// ---------------------------------------------------------------------------
// per-tile processor

/// Per-read parameters shared by every tile and row stripe.
struct TileCtx<'a> {
    planar: PlanarConfiguration,
    width: usize,
    height: usize,
    tile_w: usize,
    tile_h: usize,
    src_proj: &'a Proj,
    dst_proj: &'a Proj,
    gt: &'a GeoTransform,
    resolution: i32,
    /// Source pixels per target cell (area ratio), for the locator.
    px_per_cell: f64,
    nodata: Option<f64>,
    bbox_lonlat: Option<[f64; 4]>,
    dequant: Option<&'a DequantLut>,
    mask: Option<&'a CellMask>,
    cfg: AccCfg,
    /// Sub-point grid dimension for overlay mode; `None` is the forward path.
    overlay_k: Option<usize>,
}

impl TileCtx<'_> {
    /// (row, column, band) strides into the decoded buffer.
    /// chunky: shape = [tile_h, tile_w, n_bands]; planar: [n_bands, tile_h, tile_w].
    fn strides(&self, data_n_bands: usize) -> Result<(usize, usize, usize)> {
        match self.planar {
            PlanarConfiguration::Chunky => {
                Ok((self.tile_w * data_n_bands, data_n_bands, 1))
            }
            PlanarConfiguration::Planar => Ok((self.tile_w, 1, self.tile_h * self.tile_w)),
            other => Err(A5CogError::Unsupported(format!(
                "unhandled planar configuration: {other:?}"
            ))),
        }
    }

    /// Valid (width, height) of tile `(tx, ty)`; edge tiles are partial.
    fn actual_dims(&self, tx: usize, ty: usize) -> (usize, usize) {
        (
            self.tile_w.min(self.width.saturating_sub(tx * self.tile_w)),
            self.tile_h.min(self.height.saturating_sub(ty * self.tile_h)),
        )
    }

    fn exact_map(&self) -> ExactMap<'_> {
        ExactMap {
            src_proj: self.src_proj,
            dst_proj: self.dst_proj,
            gt: self.gt,
            src_is_latlong: self.src_proj.is_latlong(),
            dst_is_latlong: self.dst_proj.is_latlong(),
        }
    }
}

/// One decoded tile plus the band mapping that applies to its layout.
struct TileData<'a> {
    tx: usize,
    ty: usize,
    data: &'a TypedArray,
    data_n_bands: usize,
    band_offsets: &'a [usize],
}

/// Rows per indexing work unit. A 1024-row block becomes 16 stripes, enough
/// to keep every `cpu_workers` thread busy when a read spans only a handful
/// of blocks; each stripe pays one cold locator start and one store merge.
const STRIPE_ROWS: usize = 64;

/// Process one tile as row stripes into the partitioned store, on the
/// index pool when there is one, else one stripe at a time on the
/// calling thread (stripes also bound the per-stripe delta size).
fn process_tile_striped<L: AccLayout>(
    pool: Option<&rayon::ThreadPool>,
    ctx: &TileCtx,
    tile: &TileData,
    parts: &PartitionedStore<L>,
) -> Result<()> {
    let (_, actual_h) = ctx.actual_dims(tile.tx, tile.ty);
    let run = |rows: std::ops::Range<usize>| match ctx.overlay_k {
        Some(k) => process_tile_overlay::<L>(ctx, tile, rows, k, parts),
        None => process_tile::<L>(ctx, tile, rows, parts),
    };
    let stripes: Vec<std::ops::Range<usize>> = (0..actual_h)
        .step_by(STRIPE_ROWS)
        .map(|r0| r0..(r0 + STRIPE_ROWS).min(actual_h))
        .collect();
    match pool {
        Some(pool) if stripes.len() > 1 => pool.install(|| {
            use rayon::prelude::*;
            stripes.into_par_iter().try_for_each(run)
        }),
        _ => stripes.into_iter().try_for_each(run),
    }
}

/// Row-above hints for column `c`: the cells of the pixels above at
/// `c-1..=c+3`. A row scan moves east, so the cell that starts at this
/// column was usually visited on the previous row a little further on.
#[inline]
fn row_hints(prev_row: &[u64], c: usize) -> [u64; 5] {
    let w = prev_row.len();
    let mut h = [NO_CELL; 5];
    h[0] = prev_row[c];
    if c + 1 < w {
        h[1] = prev_row[c + 1];
    }
    if c + 2 < w {
        h[2] = prev_row[c + 2];
    }
    if c + 3 < w {
        h[3] = prev_row[c + 3];
    }
    if c >= 1 {
        h[4] = prev_row[c - 1];
    }
    h
}

const NO_SLOT: u32 = u32::MAX;

/// Forward (pixel-centre) indexing of rows `rows` of one tile.
///
/// Each row takes three passes: a band-major validity sweep (only with a
/// nodata sentinel), the cell lookup per valid pixel, then accumulation
/// band by band over runs of consecutive pixels in the same cell. The
/// band-major passes read each band plane sequentially on planar data and
/// touch each cell's accumulator once per run rather than once per pixel.
fn process_tile<L: AccLayout>(
    ctx: &TileCtx,
    tile: &TileData,
    rows: std::ops::Range<usize>,
    parts: &PartitionedStore<L>,
) -> Result<()> {
    let n_out = tile.band_offsets.len();
    let (actual_w, actual_h) = ctx.actual_dims(tile.tx, tile.ty);
    let rows = rows.start.min(actual_h)..rows.end.min(actual_h);
    if actual_w == 0 || rows.is_empty() {
        return Ok(());
    }
    let data = tile.data;
    let (tx, ty) = (tile.tx, tile.ty);
    let (tile_w, tile_h) = (ctx.tile_w, ctx.tile_h);
    let resolution = ctx.resolution;
    let nodata = ctx.nodata;
    let dequant = ctx.dequant;
    let mask = ctx.mask;
    let cfg = ctx.cfg;
    let prof = profile_enabled();
    let mut mask_cache = MaskCache::new();
    let (h_stride, w_stride, b_stride) = ctx.strides(tile.data_n_bands)?;
    let n = actual_w * rows.len();

    // Windowed projection of this stripe's pixel centres (see grid_proj.rs).
    let map = ctx.exact_map();
    let r0 = rows.start;
    let t_proj = Instant::now();
    let gp = GridProjector::new(
        &map,
        (tx * tile_w) as f64,
        (ty * tile_h + r0) as f64,
        actual_w,
        rows.len(),
        0.5,
    )?;
    if prof {
        T_PROJ_NS.fetch_add(t_proj.elapsed().as_nanos() as u64, Ordering::Relaxed);
        N_WINDOWS.fetch_add(gp.n_windows() as u64, Ordering::Relaxed);
        N_WINDOWS_EXACT.fetch_add(gp.n_exact_only() as u64, Ordering::Relaxed);
    }

    // Capacity is a guess; tiles at coarse resolutions touch a handful of
    // cells and a huge pre-allocation per tile was pure memset.
    let mut store: CellStore<L> = CellStore::new(n_out, cfg, (n / 64).clamp(16, 65_536));
    let mut locator = CellLocator::new(resolution, ctx.px_per_cell);
    let mut prev_row: Vec<u64> = vec![NO_CELL; actual_w];
    let mut any_valid: Vec<bool> = vec![true; actual_w];
    let mut row_slot: Vec<u32> = vec![NO_SLOT; actual_w];
    let mut runs: Vec<(u32, usize, usize)> = Vec::with_capacity(actual_w / 2 + 1);
    let mut vals: Vec<f64> = Vec::with_capacity(actual_w);

    let mut sub_pix: u64 = 0;
    let mut sub_a5: u64 = 0;
    let mut sub_hm: u64 = 0;
    let mut sub_push: u64 = 0;
    let t_idx = Instant::now();

    for r in rows.clone() {
        let row_base = r * h_stride;

        // --- pass 1: which pixels have at least one valid band
        let t = if prof { Some(Instant::now()) } else { None };
        if let Some(nd) = nodata {
            any_valid.iter_mut().for_each(|v| *v = false);
            let mut n_valid = 0usize;
            for &src_b in tile.band_offsets {
                n_valid += mark_valid(data, row_base + src_b * b_stride, w_stride, actual_w, nd, &mut any_valid);
                if n_valid == actual_w {
                    break;
                }
            }
        }
        if let Some(t0) = t { sub_pix += t0.elapsed().as_nanos() as u64; }

        // --- pass 2: cell per valid pixel
        let t = if prof { Some(Instant::now()) } else { None };
        let jj = r - r0;
        let rowf = (ty * tile_h + r) as f64 + 0.5;
        for c in 0..actual_w {
            row_slot[c] = NO_SLOT;
            if nodata.is_some() && !any_valid[c] {
                continue;
            }
            let p = gp.point(c, jj);
            let colf = (tx * tile_w + c) as f64 + 0.5;
            // exact lon/lat, computed at most once per pixel
            let mut exact_ll: Option<Option<(f64, f64)>> = None;
            if let Some(b) = ctx.bbox_lonlat {
                let decided = match p.lonlat {
                    Some((lon, lat, m)) => bbox_classify(lon, lat, m, &b),
                    None => None,
                };
                let inside = match decided {
                    Some(x) => x,
                    None => {
                        let ll = map.lonlat(colf, rowf)?;
                        exact_ll = Some(ll);
                        match ll {
                            Some((lon, lat)) => {
                                lon >= b[0] && lon <= b[2] && lat >= b[1] && lat <= b[3]
                            }
                            None => false,
                        }
                    }
                };
                if !inside {
                    continue;
                }
            }
            let mut exact = || -> Option<a5::coordinate_systems::Spherical> {
                let ll = match exact_ll {
                    Some(x) => x,
                    None => map.lonlat(colf, rowf).ok()?,
                };
                ll.map(|(lon, lat)| a5_spherical(lon, lat))
            };
            let hints = row_hints(&prev_row, c);
            let cell = locator.locate(p.approx, &mut exact, &hints);
            if cell == NO_CELL {
                continue;
            }
            prev_row[c] = cell;
            if !mask_cache.allows(mask, cell, resolution) {
                continue;
            }
            row_slot[c] = store.slot(cell);
        }
        if let Some(t0) = t { sub_a5 += t0.elapsed().as_nanos() as u64; }

        // --- pass 3: runs of consecutive pixels in one cell, band by band
        let t = if prof { Some(Instant::now()) } else { None };
        runs.clear();
        let mut c = 0usize;
        while c < actual_w {
            let s = row_slot[c];
            if s == NO_SLOT {
                c += 1;
                continue;
            }
            let start = c;
            while c < actual_w && row_slot[c] == s {
                c += 1;
            }
            runs.push((s, start, c));
        }
        if let Some(t0) = t { sub_hm += t0.elapsed().as_nanos() as u64; }
        if runs.is_empty() {
            continue;
        }
        let t = if prof { Some(Instant::now()) } else { None };
        if cfg.has_npix {
            for &(slot, c0, c1) in &runs {
                store.add_npix(slot, (c1 - c0) as f64);
            }
        }
        for (out_b, &src_b) in tile.band_offsets.iter().enumerate() {
            let band_base = row_base + src_b * b_stride;
            for &(slot, c0, c1) in &runs {
                vals.clear();
                read_run_values(data, band_base + c0 * w_stride, w_stride, c1 - c0, nodata, dequant, &mut vals);
                if vals.is_empty() {
                    continue;
                }
                if cfg.has_cont {
                    store.cont_at(slot, out_b).push_run(&vals);
                }
                if cfg.has_cat {
                    let m = store.cat_at(slot, out_b);
                    for &v in &vals {
                        cat_push(m, v as i32, 1.0)?;
                    }
                }
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

    let t_merge = Instant::now();
    parts.absorb(&store)?;
    if prof {
        T_STRIPE_MERGE_NS.fetch_add(t_merge.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// overlay (area-weighted) tile processing

/// Read one pixel's selected-band values and validity into the caller's
/// buffers. nodata is compared against the raw code; the dequant LUT (when
/// present) is applied after, matching the forward path.
#[inline]
#[allow(clippy::too_many_arguments)]
fn gather_bands(
    data: &TypedArray,
    pixel_base: usize,
    b_stride: usize,
    offsets: &[usize],
    nodata: Option<f64>,
    dequant: Option<&DequantLut>,
    band_vals: &mut [f64],
    band_valid: &mut [bool],
) -> bool {
    let mut any_valid = false;
    for (out_b, &src_b) in offsets.iter().enumerate() {
        let off = pixel_base + src_b * b_stride;
        let raw = read_pixel_chunky(data, off);
        let valid = match nodata {
            Some(nd) => !is_nodata(raw, nd),
            None => true,
        };
        band_vals[out_b] = match dequant {
            Some(d) => d.apply(raw),
            None => raw,
        };
        band_valid[out_b] = valid;
        any_valid |= valid;
    }
    any_valid
}

/// Overlay-mode configuration as passed from R. `subsamples == 0` means
/// auto-select k from the pixel/cell edge ratio (done after overview level
/// selection, so a decimated read supersamples its own pixel size).
pub(crate) struct OverlayParams {
    pub subsamples: usize,
    pub cell_edge_m: f64,
}

/// Sub-point grid dimension for overlay mode. Auto mode keeps sub-point
/// spacing at or below half the cell edge so cells finer than the pixel are
/// still hit, clamped to [2, 16].
fn resolve_overlay_k(
    p: &OverlayParams,
    gt: &GeoTransform,
    height: usize,
    src_is_latlong: bool,
) -> Result<usize> {
    if !(p.cell_edge_m > 0.0) {
        return Err(A5CogError::Invalid(
            "overlay requires a positive cell_edge_m".into(),
        ));
    }
    if p.subsamples > 0 {
        return Ok(p.subsamples);
    }
    let centre_lat = gt.0[3] + (height as f64 * 0.5) * gt.0[5];
    let (px, py) = pixel_size_m(gt, src_is_latlong, centre_lat);
    let pmax = px.max(py);
    Ok(((2.0 * pmax / p.cell_edge_m).ceil() as usize).clamp(2, 16))
}

/// Push one pixel's bands into `store` slot `slot` with weight `w`.
#[inline]
fn push_pixel<L: AccLayout>(
    store: &mut CellStore<L>,
    slot: u32,
    cfg: AccCfg,
    band_vals: &[f64],
    band_valid: &[bool],
    w: f64,
) -> Result<()> {
    store.add_npix(slot, w);
    for b in 0..band_vals.len() {
        if band_valid[b] {
            if cfg.has_cont {
                store.cont_at(slot, b).push(band_vals[b], w);
            }
            if cfg.has_cat {
                cat_push(store.cat_at(slot, b), band_vals[b] as i32, w)?;
            }
        }
    }
    Ok(())
}

/// Area-weighted processing of rows `rows` of one tile for
/// `mode = "overlay"`: each pixel contributes to every A5 cell it overlaps,
/// weighted by the overlapped fraction of its area, approximated by k x k
/// sub-point supersampling.
///
/// Cost containment: the (w+1) x (h+1) pixel-corner lattice of the stripe
/// is projected through the grid projector (one shared corner row per
/// stripe) and each corner is indexed to a cell. A pixel whose four
/// corners share a cell is interior: it takes a fast path equivalent to
/// forward sampling with weight 1. Only pixels straddling a cell boundary
/// (or the bbox edge) pay the k² sub-point cost, and their sub-points are
/// generated in the source CRS, where the pixel grid is exactly affine,
/// then batch-projected and located with the corner cells as hints.
fn process_tile_overlay<L: AccLayout>(
    ctx: &TileCtx,
    tile: &TileData,
    rows: std::ops::Range<usize>,
    k: usize,
    parts: &PartitionedStore<L>,
) -> Result<()> {
    let mut mask_cache = MaskCache::new();
    let n_out = tile.band_offsets.len();
    let (actual_w, actual_h) = ctx.actual_dims(tile.tx, tile.ty);
    let rows = rows.start.min(actual_h)..rows.end.min(actual_h);
    if actual_w == 0 || rows.is_empty() {
        return Ok(());
    }
    let data = tile.data;
    let (tx, ty) = (tile.tx, tile.ty);
    let (tile_w, tile_h) = (ctx.tile_w, ctx.tile_h);
    let gt = ctx.gt;
    let resolution = ctx.resolution;
    let nodata = ctx.nodata;
    let dequant = ctx.dequant;
    let mask = ctx.mask;
    let cfg = ctx.cfg;
    let bbox_lonlat = ctx.bbox_lonlat;

    let src_is_latlong = ctx.src_proj.is_latlong();
    let dst_is_latlong = ctx.dst_proj.is_latlong();
    let prof = profile_enabled();
    let map = ctx.exact_map();

    // --- corner lattice: windowed projection, index each corner to a cell
    let r0 = rows.start;
    let cw = actual_w + 1;
    let ch = rows.len() + 1;
    let t_proj = Instant::now();
    let gp = GridProjector::new(&map, (tx * tile_w) as f64, (ty * tile_h + r0) as f64, cw, ch, 0.0)?;
    if prof {
        T_PROJ_NS.fetch_add(t_proj.elapsed().as_nanos() as u64, Ordering::Relaxed);
        N_WINDOWS.fetch_add(gp.n_windows() as u64, Ordering::Relaxed);
        N_WINDOWS_EXACT.fetch_add(gp.n_exact_only() as u64, Ordering::Relaxed);
    }

    let t_idx = Instant::now();
    let mut locator = CellLocator::new(resolution, ctx.px_per_cell);
    let mut corner_cell: Vec<u64> = vec![NO_CELL; cw * ch];
    // corner lon/lat for the bbox test: `None` when it needs the exact
    // position (within the margin of a bbox edge, or an exact-only window)
    let mut corner_in_bbox: Vec<bool> = vec![true; cw * ch];
    let mut prev_row: Vec<u64> = vec![NO_CELL; cw];
    for jj in 0..ch {
        let rowf = (ty * tile_h + r0 + jj) as f64;
        for i in 0..cw {
            let idx = jj * cw + i;
            let colf = (tx * tile_w + i) as f64;
            let p = gp.point(i, jj);
            let mut exact_ll: Option<Option<(f64, f64)>> = None;
            if let Some(b) = bbox_lonlat {
                let decided = match p.lonlat {
                    Some((lon, lat, m)) => bbox_classify(lon, lat, m, &b),
                    None => None,
                };
                corner_in_bbox[idx] = match decided {
                    Some(x) => x,
                    None => {
                        let ll = map.lonlat(colf, rowf)?;
                        exact_ll = Some(ll);
                        match ll {
                            Some((lon, lat)) => lon >= b[0] && lon <= b[2] && lat >= b[1] && lat <= b[3],
                            None => false,
                        }
                    }
                };
            }
            let mut exact = || -> Option<a5::coordinate_systems::Spherical> {
                let ll = match exact_ll {
                    Some(x) => x,
                    None => map.lonlat(colf, rowf).ok()?,
                };
                ll.map(|(lon, lat)| a5_spherical(lon, lat))
            };
            let hints = row_hints(&prev_row, i);
            let cell = locator.locate(p.approx, &mut exact, &hints);
            corner_cell[idx] = cell;
            if cell != NO_CELL {
                prev_row[i] = cell;
            }
        }
    }

    let (h_stride, w_stride, b_stride) = ctx.strides(tile.data_n_bands)?;

    let n = actual_w * rows.len();
    let mut store: CellStore<L> = CellStore::new(n_out, cfg, (n / 64).clamp(16, 65_536));
    let mut band_vals: Vec<f64> = vec![0.0; n_out];
    let mut band_valid: Vec<bool> = vec![false; n_out];

    // --- interior pass; boundary pixels are deferred
    let mut boundary: Vec<(usize, usize)> = Vec::new();
    let mut n_interior: u64 = 0;
    for r in rows.clone() {
        for c in 0..actual_w {
            let pixel_base = r * h_stride + c * w_stride;
            if !gather_bands(
                data, pixel_base, b_stride, tile.band_offsets,
                nodata, dequant, &mut band_vals, &mut band_valid,
            ) {
                continue;
            }
            let i00 = (r - r0) * cw + c;
            let ids = [
                corner_cell[i00],
                corner_cell[i00 + 1],
                corner_cell[i00 + cw],
                corner_cell[i00 + cw + 1],
            ];
            let one_cell = ids[0] != NO_CELL && ids[1..].iter().all(|&x| x == ids[0]);
            let corners_in_bbox = bbox_lonlat.is_none()
                || [i00, i00 + 1, i00 + cw, i00 + cw + 1].iter().all(|&i| corner_in_bbox[i]);
            if one_cell && corners_in_bbox {
                n_interior += 1;
                let cell = ids[0];
                if !mask_cache.allows(mask, cell, resolution) {
                    continue;
                }
                let slot = store.slot(cell);
                push_pixel(&mut store, slot, cfg, &band_vals, &band_valid, 1.0)?;
            } else {
                boundary.push((r, c));
            }
        }
    }

    // --- boundary pass: k x k sub-points in source CRS, batch-projected
    let inv_k = 1.0 / k as f64;
    let w_sub = inv_k * inv_k;
    let kk = k * k;
    const CHUNK: usize = 1024;
    let mut pts: Vec<(f64, f64, f64)> = Vec::with_capacity(CHUNK.min(boundary.len()) * kk);
    let mut touched: Vec<(u64, u32)> = Vec::with_capacity(8);
    let in_bbox = |lon: f64, lat: f64| -> bool {
        match bbox_lonlat {
            None => true,
            Some(b) => lon >= b[0] && lon <= b[2] && lat >= b[1] && lat <= b[3],
        }
    };
    for chunk in boundary.chunks(CHUNK) {
        pts.clear();
        for &(r, c) in chunk {
            let row0 = (ty * tile_h + r) as f64;
            let col0 = (tx * tile_w + c) as f64;
            for j in 0..k {
                let rowf = row0 + (j as f64 + 0.5) * inv_k;
                for i in 0..k {
                    let colf = col0 + (i as f64 + 0.5) * inv_k;
                    let (mut x, mut y) = gt.pixel_xy(colf, rowf);
                    if src_is_latlong {
                        x = x.to_radians();
                        y = y.to_radians();
                    }
                    pts.push((x, y, 0.0));
                }
            }
        }
        let t_proj = Instant::now();
        proj_points(ctx.src_proj, ctx.dst_proj, &mut pts[..])?;
        if prof {
            T_PROJ_NS.fetch_add(t_proj.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        for (pi, &(r, c)) in chunk.iter().enumerate() {
            let pixel_base = r * h_stride + c * w_stride;
            if !gather_bands(
                data, pixel_base, b_stride, tile.band_offsets,
                nodata, dequant, &mut band_vals, &mut band_valid,
            ) {
                continue;
            }
            // hints: the pixel's four corner cells
            let i00 = (r - r0) * cw + c;
            let hints = [corner_cell[i00], corner_cell[i00 + 1], corner_cell[i00 + cw], corner_cell[i00 + cw + 1]];
            touched.clear();
            for &(lon_o, lat_o, _) in &pts[pi * kk..(pi + 1) * kk] {
                let lon = if dst_is_latlong { lon_o.to_degrees() } else { lon_o };
                let lat = if dst_is_latlong { lat_o.to_degrees() } else { lat_o };
                if !lon.is_finite() || !lat.is_finite() || !in_bbox(lon, lat) {
                    continue;
                }
                let id = locator.locate_exact(a5_spherical(lon, lat), &hints);
                if id == NO_CELL {
                    continue;
                }
                match touched.iter_mut().find(|(cell, _)| *cell == id) {
                    Some((_, cnt)) => *cnt += 1,
                    None => touched.push((id, 1)),
                }
            }
            for &(cell, cnt) in &touched {
                if !mask_cache.allows(mask, cell, resolution) {
                    continue;
                }
                let slot = store.slot(cell);
                push_pixel(&mut store, slot, cfg, &band_vals, &band_valid, cnt as f64 * w_sub)?;
            }
        }
    }
    if prof {
        T_INDEX_NS.fetch_add(t_idx.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    N_OVERLAY_INTERIOR.fetch_add(n_interior, Ordering::Relaxed);
    N_OVERLAY_BOUNDARY.fetch_add(boundary.len() as u64, Ordering::Relaxed);

    let t_merge = Instant::now();
    parts.absorb(&store)?;
    if prof {
        T_STRIPE_MERGE_NS.fetch_add(t_merge.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    Ok(())
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
    /// Compressed bytes of one block from a strip fetch: one buffer per
    /// selected band (planar) or a single buffer (chunky). Used when the
    /// file has no predictor and native byte order, so the bytes decode
    /// straight into the accumulation layout.
    Block { bufs: Vec<bytes::Bytes>, chunky: bool },
}

/// Blocks per fetch task under strip fetching. Runs of adjacent blocks in
/// a tile row are contiguous per band in GDAL-written files, so one task
/// fetching a run turns into one coalesced request per band.
const STRIP_BLOCKS: usize = 4;

/// Group the planned tiles into fetch units: runs of up to `max_len`
/// consecutive `tx` within one tile row, in planning order.
fn plan_strips(tiles: &[(usize, usize)], max_len: usize) -> Vec<Vec<(usize, usize)>> {
    let mut out: Vec<Vec<(usize, usize)>> = Vec::new();
    for &(tx, ty) in tiles {
        match out.last_mut() {
            Some(run) if run.len() < max_len && run.last().map(|&(x, y)| y == ty && x + 1 == tx).unwrap_or(false) => {
                run.push((tx, ty));
            }
            _ => out.push(vec![(tx, ty)]),
        }
    }
    out
}

/// Read-time arguments shared by the dispatchers below.
#[allow(clippy::too_many_arguments)]
pub(crate) struct ReadArgs<'a> {
    pub src: &'a str,
    pub store_opts: StoreOpts,
    pub resolution: i32,
    pub stats: Vec<Stat>,
    pub bands_idx: Vec<i32>,
    pub bands_names: Vec<String>,
    pub bbox_lonlat: Option<[f64; 4]>,
    pub src_nodata_override: Option<f64>,
    pub cpu_workers: usize,
    pub io_concurrency: usize,
    pub overview_target_m: f64,
    pub dequant: Option<Arc<DequantLut>>,
    pub overlay: Option<OverlayParams>,
    pub fractions: bool,
    pub npix: bool,
    pub bbox_align_block: bool,
    pub tile_bbox: Option<[f64; 4]>,
    pub mask: Option<Arc<CellMask>>,
}

macro_rules! dispatch_layout {
    ($a:expr, $f:expr) => {{
        let a = $a;
        match layout_for(&a.stats) {
            Layout::Sum => $f(read_raster_async_impl::<AccSum>(a).await?),
            Layout::Range => $f(read_raster_async_impl::<AccRange>(a).await?),
            Layout::Full => $f(read_raster_async_impl::<AccFull>(a).await?),
        }
    }};
}

async fn read_raster_async(a: ReadArgs<'_>) -> Result<Output> {
    Ok(dispatch_layout!(a, |agg: Aggregate<_>| agg.into_output()))
}

/// Read and write straight to Parquet from the cell slab (no f64 flatten).
async fn read_raster_to_parquet_async(
    a: ReadArgs<'_>,
    dest: &str,
    resolution: i32,
    value_type: crate::parquet_write::ValueType,
    compression: crate::parquet_write::CompressionChoice,
    as_vector: bool,
) -> Result<()> {
    dispatch_layout!(a, |agg: Aggregate<_>| {
        crate::parquet_write::write_aggregate_parquet(&agg, dest, resolution, value_type, compression, as_vector)
    })
}

async fn read_raster_async_impl<L: AccLayout>(a: ReadArgs<'_>) -> Result<Aggregate<L>> {
    let ReadArgs {
        src, store_opts, resolution, stats, bands_idx, bands_names, bbox_lonlat,
        src_nodata_override, cpu_workers, io_concurrency, overview_target_m, dequant,
        overlay, fractions, npix, bbox_align_block, tile_bbox, mask,
    } = a;
    let cfg = AccCfg::from_stats(&stats, fractions, npix);
    let prof_call = profile_enabled();
    let t_open = Instant::now();
    let (store, path) = parse_src(src, &store_opts)?;
    let reader = ObjectReader::new(store, path);
    let cache = crate::meta_cache::ChunkedMetadataCache::new(reader.clone());
    if prof_call {
        T_OPEN_STORE_NS.fetch_add(t_open.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    let t_hdr = Instant::now();
    let mut meta = TiffMetadataReader::try_open(&cache).await?;
    if prof_call {
        T_OPEN_HEADER_NS.fetch_add(t_hdr.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    let t_ifds = Instant::now();
    let ifds = meta.read_all_ifds(&cache).await?;
    if prof_call {
        T_OPEN_IFDS_NS.fetch_add(t_ifds.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    let endianness = meta.endianness();
    let tiff = TIFF::new(ifds, endianness);

    // IFD 0 = full-resolution image. CRS, geotransform, nodata and band
    // descriptions are read from it (overview IFDs in a GDAL COG generally do
    // not carry their own geo tags).
    let ifd0 = tiff
        .ifds()
        .first()
        .ok_or_else(|| A5CogError::Invalid("no IFDs".into()))?
        .clone();

    if let Some(dq) = dequant.as_deref() {
        validate_dequant_dtype(&ifd0, dq)?;
    }
    if cfg.has_cat {
        validate_categorical_dtype(&ifd0)?;
        if dequant.is_some() {
            return Err(A5CogError::Invalid(
                "dequant cannot be combined with majority/fractions: categorical \
                 stats operate on the raw integer codes"
                    .into(),
            ));
        }
    }

    let geo = ifd0
        .geo_key_directory()
        .ok_or(A5CogError::MissingGeoKey("GeoKeyDirectory"))?;

    let src_proj = build_src_proj(geo)?;
    let dst_proj = Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs")?;

    let gt0 = extract_geotransform(&ifd0)?;
    let full_w = ifd0.image_width() as usize;
    let full_h = ifd0.image_height() as usize;
    let n_bands = ifd0.samples_per_pixel() as usize;

    // Pick the overview level to read. `overview_target_m` is the A5 cell edge
    // length in metres at the requested resolution (0 = overviews disabled);
    // the caller only enables it for stat = "mean", where reading a decimated
    // overview that still oversamples each cell yields a near-identical mean
    // for a fraction of the I/O and CPU. Returns 0 (full res) when disabled,
    // when there are no usable overviews, or when none is coarse enough.
    let level = select_overview_level(
        &tiff,
        full_w,
        full_h,
        n_bands,
        &gt0,
        src_proj.is_latlong(),
        overview_target_m,
    );
    let ifd_owned = tiff
        .ifds()
        .get(level)
        .ok_or_else(|| A5CogError::Invalid("overview level out of range".into()))?
        .clone();

    // Derive the geotransform of the chosen level from IFD 0 by the dimension
    // ratio: the overview covers the same ground extent with fewer pixels, so
    // its pixel size scales by full_dim / level_dim while the origin is fixed.
    let gt = if level == 0 {
        gt0
    } else {
        derive_level_geotransform(
            &gt0,
            full_w,
            full_h,
            ifd_owned.image_width() as usize,
            ifd_owned.image_height() as usize,
        )
    };

    let width = ifd_owned.image_width() as usize;
    let height = ifd_owned.image_height() as usize;

    // overlay k is resolved against the selected level's pixel size, so an
    // overview read supersamples the decimated pixels it actually visits
    // Source pixels per target cell (area ratio) at the level being read;
    // the locator uses it to pick how far up the candidate parents sit.
    let px_per_cell: f64 = {
        let centre_lat = gt.0[3] + (height as f64 * 0.5) * gt.0[5];
        let (px, py) = pixel_size_m(&gt, src_proj.is_latlong(), centre_lat);
        a5::cell_area(resolution) / (px * py).max(1e-9)
    };
    let overlay_k: Option<usize> = match overlay.as_ref() {
        None => None,
        Some(p) => Some(resolve_overlay_k(p, &gt, height, src_proj.is_latlong())?),
    };

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

    // nodata + band descriptions live on IFD 0; overview IFDs omit them.
    let nodata = parse_nodata(&ifd0);
    let band_names_v = parse_band_descriptions(&ifd0, n_bands);
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
    let nodata = nodata_in_source_precision(&ifd0, src_nodata_override.or(nodata));

    // Bbox-driven tile filter. For most projections the bbox of 4 corners +
    // 4 edge midpoints (re-projected to the raster CRS) is a sufficient
    // axis-aligned envelope to pick the candidate tiles. The per-pixel
    // lon/lat check inside process_tile then exact-filters at the boundary.
    // `bbox_lonlat` (user bbox) drives both tile selection and the per-pixel
    // filter; `tile_bbox` (the AOI cells' envelope) only drives tile
    // selection when no user bbox is given, so AOI cells keep every pixel.
    if prof_call {
        T_OPEN_NS.fetch_add(t_open.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    let t_plan = Instant::now();
    let tiles: Vec<(usize, usize)> = if let Some(b) = bbox_lonlat.or(tile_bbox) {
        let (tx_lo, ty_lo, tx_hi, ty_hi) = match projected_tile_range(
            b, &src_proj, &dst_proj, &gt, width, height, tile_w, tile_h,
        )? {
            Some(rng) => rng,
            None => return Ok(Aggregate::empty(band_names, n_out, stats, fractions, cfg)),
        };
        let mut v = Vec::with_capacity((tx_hi - tx_lo + 1) * (ty_hi - ty_lo + 1));
        for ty in ty_lo..=ty_hi {
            for tx in tx_lo..=tx_hi {
                v.push((tx, ty));
            }
        }
        if bbox_align_block {
            // Block alignment: keep a tile iff its origin pixel centre lies in
            // the bbox (half-open on the max edges). The envelope range above
            // is a superset of those tiles, so filtering it is exact. Every
            // tile of the level then belongs to exactly one bbox of any
            // partition, so chunked callers never fetch a tile twice.
            v = filter_tiles_by_origin(
                &v, b, &src_proj, &dst_proj, &gt, tile_w, tile_h,
            )?;
            if v.is_empty() {
                return Ok(Aggregate::empty(band_names, n_out, stats, fractions, cfg));
            }
        }
        v
    } else {
        (0..n_tiles_y)
            .flat_map(|y| (0..n_tiles_x).map(move |x| (x, y)))
            .collect()
    };
    if prof_call {
        T_PLAN_NS.fetch_add(t_plan.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    let t_pipe = Instant::now();
    // Under block alignment the whole tile is in, so the per-pixel bbox
    // test is skipped.
    let bbox_pixel_filter: Option<[f64; 4]> = if bbox_align_block { None } else { bbox_lonlat };

    // Strip fetching decodes raw block bytes without byte swapping or
    // predictor reversal, so it applies only to native byte order and no
    // predictor; other files take async-tiff's per-block `fetch_tile`.
    let raw_ok = endianness.is_native()
        && matches!(
            ifd_owned.predictor(),
            None | Some(async_tiff::tags::Predictor::None)
        );
    let is_chunky = matches!(planar, PlanarConfiguration::Chunky);
    let use_strips = raw_ok && (is_chunky || matches!(planar, PlanarConfiguration::Planar));
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
    // Row-stripe indexing pool. Blocks are the fetch/decode unit but a read
    // often spans fewer blocks than `cpu_workers`; each consumer splits its
    // decoded block into stripes and runs them here, so all workers stay
    // busy and the tail of a read is one stripe, not one block. Consumers
    // block in `install` while their stripes run, so the active CPU thread
    // count stays at `cpu_workers` (plus decode overlap).
    let index_pool: Option<Arc<rayon::ThreadPool>> = if cpu_workers > 1 {
        Some(Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(cpu_workers)
                .thread_name(|i| format!("a5px-index-{i}"))
                .build()
                .map_err(|e| A5CogError::Internal(format!("index pool: {e}")))?,
        ))
    } else {
        None
    };
    // 4 partitions per worker keeps lock contention negligible; capped so
    // small reads do not pay for empty stores. One worker runs stripes in
    // order, so a single partition costs nothing and keeps its sums
    // deterministic.
    let log2p: u32 = if cpu_workers <= 1 {
        0
    } else {
        ((cpu_workers * 4).next_power_of_two().trailing_zeros()).clamp(2, 6)
    };
    // cells expected from the tiles selected, for pre-sizing the partitions
    let expected_cells: usize = {
        let px = (tiles.len() as f64) * (tile_w as f64) * (tile_h as f64);
        let cells = px / px_per_cell.max(1e-9);
        cells.min(px).max(0.0) as usize
    };
    let parts: Arc<PartitionedStore<L>> = Arc::new(PartitionedStore::new(log2p, n_out, cfg, expected_cells));
    let mut consumer_handles: Vec<tokio::task::JoinHandle<Result<()>>> =
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
        let bbox_lonlat_c = bbox_pixel_filter;
        let dequant_c = dequant.clone();
        let mask_c = mask.clone();
        let pool = index_pool.clone();
        let parts = Arc::clone(&parts);
        consumer_handles.push(tokio::task::spawn_blocking(move || {
            let ctx = TileCtx {
                planar,
                width,
                height,
                tile_w,
                tile_h,
                src_proj: &src_proj,
                dst_proj: &dst_proj,
                gt: &gt_c,
                resolution,
                px_per_cell,
                nodata,
                bbox_lonlat: bbox_lonlat_c,
                dequant: dequant_c.as_deref(),
                mask: mask_c.as_deref(),
                cfg,
                overlay_k,
            };
            let prof = profile_enabled();
            loop {
                let t_wait = Instant::now();
                let Ok(item) = rx.recv_blocking() else { break };
                if prof {
                    T_RECV_WAIT_NS.fetch_add(t_wait.elapsed().as_nanos() as u64, Ordering::Relaxed);
                }
                let (data, _shape, data_n_bands_eff, offsets_arc): (
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
                    TilePayload::Block { bufs, chunky } => {
                        let t_dec = Instant::now();
                        let (typed, sh) = crate::band_fetch::decode_block_bytes(
                            bufs, &ifd, &registry, chunky,
                        )?;
                        if prof {
                            T_DECODE_NS
                                .fetch_add(t_dec.elapsed().as_nanos() as u64, Ordering::Relaxed);
                        }
                        if chunky {
                            (typed, sh, n_bands, Arc::clone(&selected_bands))
                        } else {
                            (typed, sh, n_out, Arc::clone(&identity_offsets))
                        }
                    }
                };
                let td = TileData {
                    tx: item.tx,
                    ty: item.ty,
                    data: &data,
                    data_n_bands: data_n_bands_eff,
                    band_offsets: &offsets_arc,
                };
                process_tile_striped::<L>(pool.as_deref(), &ctx, &td, &parts)?;
            }
            Ok::<(), A5CogError>(())
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
        let strips = if use_strips { plan_strips(&tiles, STRIP_BLOCKS) } else { tiles.iter().map(|&t| vec![t]).collect() };
        // io_concurrency counts blocks in flight; a strip task carries up
        // to STRIP_BLOCKS of them.
        let task_conc = if use_strips { (io_concurrency / STRIP_BLOCKS).max(2) } else { io_concurrency.max(1) };
        let producer = stream::iter(strips)
            .map(|blocks| {
                let reader = reader.clone();
                let ifd = Arc::clone(&ifd);
                let selected_bands = Arc::clone(&selected_bands);
                let tx_chan = tx_chan.clone();
                async move {
                    let prof = profile_enabled();
                    let t_fetch = Instant::now();
                    let items: Vec<TileItem> = if use_strips {
                        let per_block = crate::band_fetch::fetch_blocks_bytes(
                            &reader as &dyn AsyncFileReader,
                            &ifd,
                            &blocks,
                            &selected_bands,
                            is_chunky,
                        )
                        .await?;
                        blocks
                            .iter()
                            .zip(per_block)
                            .map(|(&(tx, ty), bufs)| TileItem { tx, ty, payload: TilePayload::Block { bufs, chunky: is_chunky } })
                            .collect()
                    } else {
                        let (tx, ty) = blocks[0];
                        let tile = ifd
                            .fetch_tile(tx, ty, &reader as &dyn AsyncFileReader)
                            .await?;
                        vec![TileItem { tx, ty, payload: TilePayload::Full(tile) }]
                    };
                    if prof {
                        T_FETCH_NS
                            .fetch_add(t_fetch.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    }
                    let t_send = Instant::now();
                    for item in items {
                        tx_chan
                            .send(item)
                            .await
                            .map_err(|_| {
                                A5CogError::Invalid("consumer pool dropped channel".into())
                            })?;
                    }
                    if prof {
                        T_SEND_WAIT_NS.fetch_add(t_send.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    }
                    Ok::<(), A5CogError>(())
                }
            })
            .buffer_unordered(task_conc)
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

    // Drain consumers. Collect all results first (rather than short-circuit
    // on the first Err) so a panic / error in worker N doesn't detach
    // workers N+1.. while they're still running.
    let mut consumer_results: Vec<Result<()>> = Vec::with_capacity(cpu_workers);
    for h in consumer_handles {
        match h.await {
            Ok(inner) => consumer_results.push(inner),
            Err(join_err) => consumer_results.push(Err(A5CogError::WorkerJoin(format!(
                "tile-consumer worker: {join_err}"
            )))),
        }
    }
    for r in consumer_results {
        r?;
    }
    if prof_call {
        T_PIPELINE_NS.fetch_add(t_pipe.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    let t_reduce = Instant::now();
    let parts = Arc::try_unwrap(parts)
        .map_err(|_| A5CogError::Internal("partition store still shared after workers joined".into()))?
        .into_parts()?;
    let agg = Aggregate::from_parts(parts, n_out, band_names, stats, fractions, index_pool);
    if profile_enabled() {
        T_REDUCE_NS.fetch_add(t_reduce.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    Ok(agg)
}

/// A finished read before flattening: the partition slabs plus output
/// metadata. Output cell order is partition order, then first-seen order
/// within each partition (`cells` is that concatenation).
pub(crate) struct Aggregate<L: AccLayout> {
    parts: Vec<CellStore<L>>,
    cells: Vec<u64>,
    pub(crate) n_out: usize,
    pub(crate) band_names: Vec<String>,
    pub(crate) stats: Vec<Stat>,
    fractions: bool,
    /// Index pool of the read, reused for flattening and Parquet encoding.
    pub(crate) pool: Option<Arc<rayon::ThreadPool>>,
}

impl<L: AccLayout> Aggregate<L> {
    fn empty(band_names: Vec<String>, n_out: usize, stats: Vec<Stat>, fractions: bool, _cfg: AccCfg) -> Self {
        Self::from_parts(Vec::new(), n_out, band_names, stats, fractions, None)
    }

    fn from_parts(
        parts: Vec<CellStore<L>>,
        n_out: usize,
        band_names: Vec<String>,
        stats: Vec<Stat>,
        fractions: bool,
        pool: Option<Arc<rayon::ThreadPool>>,
    ) -> Self {
        let mut cells = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
        for p in &parts {
            cells.extend_from_slice(&p.cells);
        }
        Self { parts, cells, n_out, band_names, stats, fractions, pool }
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.cells.len()
    }

    #[inline]
    pub(crate) fn cells(&self) -> &[u64] {
        &self.cells
    }

    /// Per-cell pixel weights in output order, when requested.
    pub(crate) fn npix(&self) -> Option<Vec<f64>> {
        if !self.parts.iter().any(|p| p.cfg.has_npix) {
            return None;
        }
        let mut v = Vec::with_capacity(self.len());
        for p in &self.parts {
            v.extend_from_slice(&p.npix);
        }
        Some(v)
    }

    /// Values of `stat` for band `b` over all cells, in output order.
    pub(crate) fn column_iter(&self, stat: Stat, b: usize) -> impl Iterator<Item = f64> + '_ {
        self.parts
            .iter()
            .flat_map(move |p| (0..p.len()).map(move |i| p.value(stat, i, b)))
    }

    /// Fill `out` (length `len() * n_out`) cell-major with `stat`, one
    /// partition per task on the pool.
    pub(crate) fn flat_into(&self, stat: Stat, out: &mut [f64]) {
        let n_out = self.n_out;
        debug_assert_eq!(out.len(), self.len() * n_out);
        let mut slices: Vec<&mut [f64]> = Vec::with_capacity(self.parts.len());
        let mut rest = out;
        for p in &self.parts {
            let (a, b) = rest.split_at_mut(p.len() * n_out);
            slices.push(a);
            rest = b;
        }
        let fill = |(p, s): (&CellStore<L>, &mut [f64])| {
            for i in 0..p.len() {
                for b in 0..n_out {
                    s[i * n_out + b] = p.value(stat, i, b);
                }
            }
        };
        match self.pool.as_deref() {
            Some(pool) if self.parts.len() > 1 => pool.install(|| {
                use rayon::prelude::*;
                self.parts.par_iter().zip(slices.into_par_iter()).for_each(fill)
            }),
            _ => self.parts.iter().zip(slices).for_each(fill),
        }
    }

    /// Flatten into the cell-major per-stat layout the R paths consume.
    fn into_output(self) -> Output {
    let t_flatten = Instant::now();
    let n_out = self.n_out;
    let n_stats = self.stats.len();
    let n = self.len();
    // cell-major flat layout per stat: flat_values[s][i*n_out + b] is the s-th
    // stat of band b of cell i.
    let mut flat_per_stat: Vec<Vec<f64>> =
        (0..n_stats).map(|_| vec![0.0; n * n_out]).collect();
    for (s_i, s) in self.stats.iter().enumerate() {
        self.flat_into(*s, &mut flat_per_stat[s_i]);
    }
    let fractions = self.fractions;
    let mut frac_out: Option<FracOut> = if fractions {
        Some(FracOut {
            classes: vec![Vec::new(); n_out],
            shares: vec![Vec::new(); n_out],
            offsets: vec![vec![0i32]; n_out],
        })
    } else {
        None
    };
    if let Some(fr) = frac_out.as_mut() {
        for (p, i) in self.parts.iter().flat_map(|p| (0..p.len()).map(move |i| (p, i))) {
            for b in 0..n_out {
                // classes sorted ascending so output order is deterministic;
                // shares are each class's fraction of the cell's valid weight
                let mut sorted = p.cat[i * n_out + b].clone();
                sorted.sort_unstable_by_key(|&(c, _)| c);
                let tot: f64 = sorted.iter().map(|&(_, w)| w).sum();
                if tot > 0.0 {
                    for &(c, w) in &sorted {
                        fr.classes[b].push(c);
                        fr.shares[b].push(w / tot);
                    }
                }
                fr.offsets[b].push(fr.classes[b].len() as i32);
            }
        }
    }
    if profile_enabled() {
        T_FLATTEN_NS.fetch_add(t_flatten.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    let npix = self.npix();
    let Aggregate { cells, band_names, stats, .. } = self;

    Output {
        cells,
        flat_values: flat_per_stat,
        n_bands: n_out,
        band_names,
        stats: stats.iter().map(|s| s.as_str().to_string()).collect(),
        fractions: frac_out,
        npix,
    }
    }
}

struct Output {
    cells: Vec<u64>,
    /// One Vec per stat. Each Vec is cell-major flat: cell `i` band `b` is at
    /// index `i * n_bands + b`. Outer index matches the order of `stats`.
    flat_values: Vec<Vec<f64>>,
    n_bands: usize,
    band_names: Vec<String>,
    stats: Vec<String>,
    /// Present only for a "fractions" read; ragged per-cell class shares.
    fractions: Option<FracOut>,
    /// Per-cell pixel weight, for `stat = "npix"`.
    npix: Option<Vec<f64>>,
}

/// Class-share output in CSR form, one entry per band: `offsets[b]` has
/// `n_cells + 1` values delimiting cell `i`'s slice of `classes[b]` /
/// `shares[b]`, in the same cell order as `Output::cells`.
struct FracOut {
    classes: Vec<Vec<i32>>,
    shares: Vec<Vec<f64>>,
    offsets: Vec<Vec<i32>>,
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

fn parse_overlay_args(
    overlay: bool,
    subsamples: i32,
    cell_edge_m: f64,
) -> Result<Option<OverlayParams>> {
    if !overlay {
        return Ok(None);
    }
    if !(0..=64).contains(&subsamples) {
        return Err(A5CogError::Invalid(format!(
            "subsamples must be in 0..=64 (0 = auto); got {subsamples}"
        )));
    }
    Ok(Some(OverlayParams {
        subsamples: subsamples as usize,
        cell_edge_m,
    }))
}


/// Minimum linear oversampling kept when choosing an overview: the selected
/// level's pixel must be at least this many times finer than the target cell
/// edge, so each cell still receives ~`OVERVIEW_MIN_OVERSAMPLE^2` samples and
/// the forward (pixel-driven) path does not leave gaps.
const OVERVIEW_MIN_OVERSAMPLE: f64 = 4.0;

/// Approximate ground pixel size (metres) along each axis. For projected CRSs
/// (metres) this is the geotransform scale directly; for geographic CRSs the
/// degree scale is converted at the given centre latitude. Skew terms are
/// folded in so rotated transforms still yield a sane magnitude.
fn pixel_size_m(gt: &GeoTransform, is_latlong: bool, centre_lat_deg: f64) -> (f64, f64) {
    let dx = gt.0[1].abs().max(gt.0[2].abs());
    let dy = gt.0[4].abs().max(gt.0[5].abs());
    if is_latlong {
        const M_PER_DEG: f64 = 111_320.0;
        let coslat = centre_lat_deg.to_radians().cos().abs().max(1e-6);
        (dx * M_PER_DEG * coslat, dy * M_PER_DEG)
    } else {
        (dx, dy)
    }
}

/// Choose the IFD index to read for a forward mean aggregation. `target_m` is
/// the A5 cell edge length in metres; `<= 0` disables overview use. Picks the
/// coarsest overview whose pixel is still at least `OVERVIEW_MIN_OVERSAMPLE`x
/// finer than the cell, else full resolution (level 0). Only reduced-resolution
/// (non-mask), tiled IFDs with the full band count are considered.
fn select_overview_level(
    tiff: &TIFF,
    full_w: usize,
    full_h: usize,
    n_bands: usize,
    gt0: &GeoTransform,
    src_is_latlong: bool,
    target_m: f64,
) -> usize {
    if !(target_m > 0.0) || full_w == 0 || full_h == 0 {
        return 0;
    }
    let centre_lat = gt0.0[3] + (full_h as f64 * 0.5) * gt0.0[5];
    let (px0, py0) = pixel_size_m(gt0, src_is_latlong, centre_lat);
    let budget = target_m / OVERVIEW_MIN_OVERSAMPLE;
    let mut best = 0usize;
    // coarsest pixel found so far that still fits the budget (full res never
    // exceeds itself, so seed below it to force a real overview to win).
    let mut best_px = f64::NEG_INFINITY;
    for (i, ifd) in tiff.ifds().iter().enumerate() {
        if i == 0 {
            continue;
        }
        // must be a reduced-resolution overview, not a mask/auxiliary IFD
        if let Some(st) = ifd.new_subfile_type() {
            if st & 0x1 == 0 || st & 0x4 != 0 {
                continue;
            }
        }
        let w = ifd.image_width() as usize;
        let h = ifd.image_height() as usize;
        if w == 0 || h == 0 || w >= full_w || h >= full_h {
            continue;
        }
        if ifd.samples_per_pixel() as usize != n_bands {
            continue;
        }
        if ifd.tile_width().is_none() || ifd.tile_height().is_none() {
            continue;
        }
        let px = px0 * (full_w as f64 / w as f64);
        let py = py0 * (full_h as f64 / h as f64);
        let pmax = px.max(py);
        if pmax <= budget && pmax > best_px {
            best_px = pmax;
            best = i;
        }
    }
    best
}

/// Geotransform of an overview level, derived from IFD 0 by the dimension
/// ratio. The overview spans the same ground extent with `lw x lh` pixels, so
/// pixel scale and skew terms scale by `full / level` while the origin is fixed.
fn derive_level_geotransform(
    gt0: &GeoTransform,
    full_w: usize,
    full_h: usize,
    lw: usize,
    lh: usize,
) -> GeoTransform {
    let sx = full_w as f64 / lw as f64;
    let sy = full_h as f64 / lh as f64;
    GeoTransform([
        gt0.0[0],
        gt0.0[1] * sx,
        gt0.0[2] * sy,
        gt0.0[3],
        gt0.0[4] * sx,
        gt0.0[5] * sy,
    ])
}

/// Keep the tiles whose representative point projects into `bbox_lonlat`,
/// half-open on the max edges so a partition of bboxes sharing edges
/// assigns each tile to exactly one member. The representative is the
/// origin pixel centre (top-left pixel of the tile); when that point is
/// outside the projection's domain (a corner tile of a LAEA raster padded
/// beyond the disc) the tile centre and then the far corner are tried, so
/// a tile with any projectable content still has a deterministic owner.
fn filter_tiles_by_origin(
    tiles: &[(usize, usize)],
    bbox_lonlat: [f64; 4],
    src_proj: &Proj,
    dst_proj: &Proj,
    gt: &GeoTransform,
    tile_w: usize,
    tile_h: usize,
) -> Result<Vec<(usize, usize)>> {
    const CANDIDATES: usize = 3;
    let mut points: Vec<(f64, f64, f64)> = Vec::with_capacity(tiles.len() * CANDIDATES);
    for &(tx, ty) in tiles {
        let c0 = (tx * tile_w) as f64;
        let r0 = (ty * tile_h) as f64;
        let cands = [
            (c0 + 0.5, r0 + 0.5),
            (c0 + tile_w as f64 * 0.5, r0 + tile_h as f64 * 0.5),
            (c0 + tile_w as f64 - 0.5, r0 + tile_h as f64 - 0.5),
        ];
        for (c, r) in cands {
            let x = gt.0[0] + c * gt.0[1] + r * gt.0[2];
            let y = gt.0[3] + c * gt.0[4] + r * gt.0[5];
            points.push((x, y, 0.0));
        }
    }
    if src_proj.is_latlong() {
        for p in &mut points {
            p.0 = p.0.to_radians();
            p.1 = p.1.to_radians();
        }
    }
    proj_points(src_proj, dst_proj, &mut points[..])?;
    let dst_is_latlong = dst_proj.is_latlong();
    let [xmin, ymin, xmax, ymax] = bbox_lonlat;
    Ok(tiles
        .iter()
        .enumerate()
        .filter_map(|(k, &t)| {
            let rep = points[k * CANDIDATES..(k + 1) * CANDIDATES]
                .iter()
                .find(|&&(x, y, _)| x.is_finite() && y.is_finite())?;
            let lon = if dst_is_latlong { rep.0.to_degrees() } else { rep.0 };
            let lat = if dst_is_latlong { rep.1.to_degrees() } else { rep.1 };
            let inside = lon >= xmin && lon < xmax && lat >= ymin && lat < ymax;
            if inside { Some(t) } else { None }
        })
        .collect())
}

/// Points along the boundary of an axis-aligned rectangle: the 4 corners
/// plus `EDGE_SAMPLES - 1` interior samples per edge.
fn rect_edge_samples(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    const EDGE_SAMPLES: usize = 32;
    let mut v = Vec::with_capacity(4 * EDGE_SAMPLES);
    for i in 0..EDGE_SAMPLES {
        let t = i as f64 / EDGE_SAMPLES as f64;
        v.push((x0 + t * (x1 - x0), y0)); // bottom, left -> right
        v.push((x1, y0 + t * (y1 - y0))); // right, bottom -> top
        v.push((x1 - t * (x1 - x0), y1)); // top, right -> left
        v.push((x0, y1 - t * (y1 - y0))); // left, top -> bottom
    }
    v
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
    // Densely sampled rectangle boundary: the projected edges are curves and
    // their extreme point is not always a corner or midpoint. Points that
    // fail to project (outside the CRS domain) become NaN and are skipped;
    // the per-pixel filter inside `process_tile` is the exact one.
    let mut points: Vec<(f64, f64, f64)> = rect_edge_samples(xmin_ll, ymin_ll, xmax_ll, ymax_ll)
        .into_iter()
        .map(|(x, y)| (x, y, 0.0))
        .collect();
    // dst_proj is +proj=longlat +datum=WGS84; latlong needs radians
    let dst_is_latlong = dst_proj.is_latlong();
    if dst_is_latlong {
        for p in &mut points {
            p.0 = p.0.to_radians();
            p.1 = p.1.to_radians();
        }
    }
    // forward direction: WGS84 (dst) -> raster CRS (src)
    proj_points(dst_proj, src_proj, &mut points[..])?;
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
/// @param overview_target_m A5 cell edge length in metres; when > 0 a COG
///   overview that still oversamples the cell is read instead of full
///   resolution. 0 disables overview use (always read IFD 0).
/// @param dequant_lut Pre-aggregation decode LUT over the integer code domain
///   starting at `dequant_min`; empty = no dequantization.
/// @param dequant_min First code covered by `dequant_lut`.
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
    overview_target_m: f64,
    dequant_lut: Vec<f64>,
    dequant_min: f64,
    overlay: bool,
    subsamples: i32,
    cell_edge_m: f64,
    bbox_align_block: bool,
    tile_bbox: Vec<f64>,
    aoi_cells_raw: List,
    store_keys: Vec<String>,
    store_values: Vec<String>,
) -> Result<Robj> {
    let store_opts = parse_store_opts(store_keys, store_values)?;
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
    let (stats_e, fractions, npix) = parse_stats(&stats)?;
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
    let dequant = parse_dequant_arg(dequant_lut, dequant_min).map(Arc::new);
    let overlay_opt = parse_overlay_args(overlay, subsamples, cell_edge_m)?;
    let tile_bbox_opt = opt_f64_arg::<4>(tile_bbox, "tile_bbox")?;
    let mask = CellMask::from_cells(&raw8_list_to_u64s(&aoi_cells_raw)).map(Arc::new);

    let out: Output = runtime.block_on(read_raster_async(ReadArgs {
        src,
        store_opts,
        resolution,
        stats: stats_e,
        bands_idx,
        bands_names,
        bbox_lonlat: bbox_opt,
        src_nodata_override: src_nodata_opt,
        cpu_workers,
        io_concurrency,
        overview_target_m,
        dequant,
        overlay: overlay_opt,
        fractions: fractions,
        npix,
        bbox_align_block,
        tile_bbox: tile_bbox_opt,
        mask,
    }))?;

    let t_rout = Instant::now();
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
                format!("{}_{}", b_name, s_name)
            };
            band_pairs.push((key, Robj::from(col)));
        }
    }
    let bands = List::from_pairs(band_pairs);
    let band_names: Vec<&str> = out.band_names.iter().map(|s| s.as_str()).collect();
    let stats_out: Vec<&str> = out.stats.iter().map(|s| s.as_str()).collect();

    // ragged class-share output for a "fractions" read: per band, CSR arrays
    // (classes / shares / offsets) that the R wrapper splits into a list col
    let fractions_robj: Robj = match out.fractions {
        None => ().into(),
        Some(fr) => {
            let mut pairs: Vec<(String, Robj)> = Vec::with_capacity(out.band_names.len());
            for (b, name) in out.band_names.iter().enumerate() {
                pairs.push((
                    name.clone(),
                    list!(
                        classes = fr.classes[b].clone(),
                        shares = fr.shares[b].clone(),
                        offsets = fr.offsets[b].clone()
                    )
                    .into(),
                ));
            }
            List::from_pairs(pairs).into()
        }
    };

    let npix_robj: Robj = match out.npix {
        None => ().into(),
        Some(v) => Robj::from(v),
    };
    let result: Robj = list!(
        cell = cell_list,
        bands = bands,
        band_names = band_names,
        stats = stats_out,
        fractions = fractions_robj,
        npix = npix_robj
    )
    .into();
    if prof {
        T_ROUT_NS.fetch_add(t_rout.elapsed().as_nanos() as u64, Ordering::Relaxed);
        print_timers(t0.elapsed().as_secs_f64());
    }
    Ok(result)
}

/// Forward-aggregate a (Cloud-Optimised) GeoTIFF into A5 cells, returning a
/// flat cell-major numeric buffer suitable for direct construction of an
/// Arrow `FixedSizeList<float64, n_bands>` array on the R side.
///
/// @param src Path or URL string.
/// @param resolution A5 resolution (0--30).
/// @param stats Character vector of stats (any non-"fractions" subset of the
///   stats accepted by `a5_read_raster`).
/// @param bands_idx 1-based band indices to read (empty for all unless
///   `bands_names` is provided).
/// @param bands_names Band names to read (matched against the GDAL
///   DESCRIPTION tag).
/// @param threads Worker threads.
/// @param io_concurrency Number of tiles fetched concurrently.
/// @param overview_target_m A5 cell edge length in metres; when > 0 a COG
///   overview that still oversamples the cell is read instead of full
///   resolution. 0 disables overview use (always read IFD 0).
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
    overview_target_m: f64,
    dequant_lut: Vec<f64>,
    dequant_min: f64,
    overlay: bool,
    subsamples: i32,
    cell_edge_m: f64,
    bbox_align_block: bool,
    tile_bbox: Vec<f64>,
    aoi_cells_raw: List,
    store_keys: Vec<String>,
    store_values: Vec<String>,
) -> Result<Robj> {
    let store_opts = parse_store_opts(store_keys, store_values)?;
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
    let (stats_e, fractions, npix) = parse_stats(&stats)?;
    if fractions {
        return Err(A5CogError::Unsupported(
            "\"fractions\" is only available via a5_read_raster()".into(),
        ));
    }
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
    let dequant = parse_dequant_arg(dequant_lut, dequant_min).map(Arc::new);
    let overlay_opt = parse_overlay_args(overlay, subsamples, cell_edge_m)?;
    let tile_bbox_opt = opt_f64_arg::<4>(tile_bbox, "tile_bbox")?;
    let mask = CellMask::from_cells(&raw8_list_to_u64s(&aoi_cells_raw)).map(Arc::new);

    let out: Output = runtime.block_on(read_raster_async(ReadArgs {
        src,
        store_opts,
        resolution,
        stats: stats_e,
        bands_idx,
        bands_names,
        bbox_lonlat: bbox_opt,
        src_nodata_override: src_nodata_opt,
        cpu_workers,
        io_concurrency,
        overview_target_m,
        dequant,
        overlay: overlay_opt,
        fractions: false,
        npix,
        bbox_align_block,
        tile_bbox: tile_bbox_opt,
        mask,
    }))?;

    let t_rout = Instant::now();
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
    let npix_robj: Robj = match out.npix {
        None => ().into(),
        Some(v) => Robj::from(v),
    };

    let result: Robj = list!(
        cell = cell_list,
        value_flat = value_flat,
        band_names = band_names,
        stats = stats_out,
        n_bands = n_bands,
        npix = npix_robj
    )
    .into();
    if prof {
        T_ROUT_NS.fetch_add(t_rout.elapsed().as_nanos() as u64, Ordering::Relaxed);
        print_timers(t0.elapsed().as_secs_f64());
    }
    Ok(result)
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
/// @param stats Character vector of stats (any non-"fractions" subset of the
///   stats accepted by `a5_read_raster`).
/// @param bands_idx,bands_names Band selection (see `a5_read_raster_rs`).
/// @param value_type Storage type for the value column ("float64" | "float32").
/// @param compression Parquet compression codec ("zstd" | "snappy" | "none").
/// @param threads Worker threads.
/// @param io_concurrency Number of tiles fetched concurrently.
/// @param overview_target_m A5 cell edge length in metres; when > 0 a COG
///   overview that still oversamples the cell is read instead of full
///   resolution. 0 disables overview use (always read IFD 0).
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
    as_vector: bool,
    value_type: &str,
    compression: &str,
    cpu_workers: i32,
    io_concurrency: i32,
    overview_target_m: f64,
    dequant_lut: Vec<f64>,
    dequant_min: f64,
    overlay: bool,
    subsamples: i32,
    cell_edge_m: f64,
    bbox_align_block: bool,
    tile_bbox: Vec<f64>,
    aoi_cells_raw: List,
    store_keys: Vec<String>,
    store_values: Vec<String>,
) -> Result<String> {
    let store_opts = parse_store_opts(store_keys, store_values)?;
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
    let (stats_e, fractions, npix) = parse_stats(&stats)?;
    if fractions {
        return Err(A5CogError::Unsupported(
            "\"fractions\" is only available via a5_read_raster()".into(),
        ));
    }
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
    let dequant = parse_dequant_arg(dequant_lut, dequant_min).map(Arc::new);
    let overlay_opt = parse_overlay_args(overlay, subsamples, cell_edge_m)?;
    let tile_bbox_opt = opt_f64_arg::<4>(tile_bbox, "tile_bbox")?;
    let mask = CellMask::from_cells(&raw8_list_to_u64s(&aoi_cells_raw)).map(Arc::new);


    runtime.block_on(read_raster_to_parquet_async(
        ReadArgs {
        src,
        store_opts,
        resolution,
        stats: stats_e,
        bands_idx,
        bands_names,
        bbox_lonlat: bbox_opt,
        src_nodata_override: src_nodata_opt,
        cpu_workers,
        io_concurrency,
        overview_target_m,
        dequant,
        overlay: overlay_opt,
        fractions: false,
        npix,
        bbox_align_block,
        tile_bbox: tile_bbox_opt,
        mask,
    },
        dest,
        resolution,
        value_type_e,
        compression_e,
        as_vector,
    ))?;

    if prof {
        print_timers(t0.elapsed().as_secs_f64());
    }
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
/// Shared argument parsing + sampler invocation for the centroid entry
/// points (R-list, flat/Arrow, and Parquet outputs).
#[allow(clippy::too_many_arguments)]
fn run_sample_at_cells(
    src: &str,
    store_opts: StoreOpts,
    cells_raw: List,
    bands_idx: Vec<i32>,
    bands_names: Vec<String>,
    src_nodata: Vec<f64>,
    cpu_workers: i32,
    io_concurrency: i32,
    dequant_lut: Vec<f64>,
    dequant_min: f64,
    interp: &str,
) -> Result<crate::sample::CentroidOutput> {
    if !bands_idx.is_empty() && !bands_names.is_empty() {
        return Err(A5CogError::Invalid(
            "specify bands by index OR by name, not both".into(),
        ));
    }
    let cpu_workers = cpu_workers.max(1) as usize;
    let io_concurrency = io_concurrency.max(1) as usize;
    let src_nodata_opt = parse_src_nodata_arg(src_nodata)?;
    let dequant = parse_dequant_arg(dequant_lut, dequant_min).map(Arc::new);
    let interp_e = crate::sample::Interp::parse(interp)?;
    let cells_in = crate::cell_raw::raw8_list_to_u64s(&cells_raw);

    let runtime = crate::runtime::shared_runtime()?;

    runtime.block_on(crate::sample::sample_at_cells_async(
        src,
        store_opts,
        cells_in,
        bands_idx,
        bands_names,
        src_nodata_opt,
        cpu_workers,
        io_concurrency,
        dequant,
        interp_e,
    ))
}

#[extendr]
fn a5_sample_at_cells_rs(
    src: &str,
    cells_raw: List,
    bands_idx: Vec<i32>,
    bands_names: Vec<String>,
    src_nodata: Vec<f64>,
    cpu_workers: i32,
    io_concurrency: i32,
    dequant_lut: Vec<f64>,
    dequant_min: f64,
    interp: &str,
    store_keys: Vec<String>,
    store_values: Vec<String>,
) -> Result<Robj> {
    let store_opts = parse_store_opts(store_keys, store_values)?;
    let out = run_sample_at_cells(
        src,
        store_opts,
        cells_raw,
        bands_idx,
        bands_names,
        src_nodata,
        cpu_workers,
        io_concurrency,
        dequant_lut,
        dequant_min,
        interp,
    )?;

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

/// Flat-output variant of `a5_sample_at_cells_rs`: same sampler, but the
/// result mirrors the `a5_read_raster_flat_rs` shape (cell-major flat buffer
/// under a single pseudo-stat "centroid") so the R-side Arrow assembly is
/// shared across modes.
/// @noRd
/// @keywords internal
#[extendr]
fn a5_sample_at_cells_flat_rs(
    src: &str,
    cells_raw: List,
    bands_idx: Vec<i32>,
    bands_names: Vec<String>,
    src_nodata: Vec<f64>,
    cpu_workers: i32,
    io_concurrency: i32,
    dequant_lut: Vec<f64>,
    dequant_min: f64,
    interp: &str,
    store_keys: Vec<String>,
    store_values: Vec<String>,
) -> Result<Robj> {
    let store_opts = parse_store_opts(store_keys, store_values)?;
    let out = run_sample_at_cells(
        src,
        store_opts,
        cells_raw,
        bands_idx,
        bands_names,
        src_nodata,
        cpu_workers,
        io_concurrency,
        dequant_lut,
        dequant_min,
        interp,
    )?;

    let cell_list = u64s_to_raw8_list(&out.cells);
    let band_names: Vec<&str> = out.band_names.iter().map(|s| s.as_str()).collect();
    let n_bands = out.n_bands as i32;
    let value_flat = List::from_pairs(vec![("centroid".to_string(), Robj::from(out.flat))]);
    Ok(list!(
        cell = cell_list,
        value_flat = value_flat,
        band_names = band_names,
        stats = "centroid",
        n_bands = n_bands
    )
    .into())
}

/// Centroid samples straight to Parquet: same sampler as
/// `a5_sample_at_cells_rs`, with the RecordBatch built from the flat buffer
/// and written by the Rust `parquet` crate (no R materialisation). The
/// single pseudo-stat is "centroid", so columns are plain band names
/// (`as_vector = false`) or a single `value` FixedSizeList.
/// @noRd
/// @keywords internal
#[extendr]
fn a5_sample_to_parquet_rs(
    src: &str,
    dest: &str,
    resolution: i32,
    cells_raw: List,
    bands_idx: Vec<i32>,
    bands_names: Vec<String>,
    src_nodata: Vec<f64>,
    as_vector: bool,
    value_type: &str,
    compression: &str,
    cpu_workers: i32,
    io_concurrency: i32,
    dequant_lut: Vec<f64>,
    dequant_min: f64,
    interp: &str,
    store_keys: Vec<String>,
    store_values: Vec<String>,
) -> Result<String> {
    let store_opts = parse_store_opts(store_keys, store_values)?;
    let value_type_e = crate::parquet_write::ValueType::parse(value_type)?;
    let compression_e = crate::parquet_write::CompressionChoice::parse(compression)?;
    let out = run_sample_at_cells(
        src,
        store_opts,
        cells_raw,
        bands_idx,
        bands_names,
        src_nodata,
        cpu_workers,
        io_concurrency,
        dequant_lut,
        dequant_min,
        interp,
    )?;

    crate::parquet_write::write_arrow_parquet(
        dest,
        out.cells,
        vec![out.flat],
        out.n_bands,
        &out.band_names,
        &["centroid".to_string()],
        resolution,
        value_type_e,
        compression_e,
        as_vector,
    )?;

    Ok(dest.to_string())
}

/// WGS 84 envelope of a raster footprint: project the 4 corners + 4 edge
/// midpoints of the projected extent and take the axis-aligned envelope.
fn footprint_lonlat(
    src_proj: &Proj,
    dst_proj: &Proj,
    gt: &GeoTransform,
    w: usize,
    h: usize,
) -> Result<[f64; 4]> {
    let w = w as f64;
    let h = h as f64;
    let mut points: Vec<(f64, f64, f64)> = rect_edge_samples(0.0, 0.0, w, h)
        .into_iter()
        .map(|(c, r)| {
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
    proj_points(src_proj, dst_proj, &mut points[..])?;
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
    Ok([xmin, ymin, xmax, ymax])
}

/// Open `src` and read all IFDs.
async fn open_tiff(src: &str, store_opts: &StoreOpts) -> Result<TIFF> {
    let (store, path) = parse_src(src, store_opts)?;
    let reader = ObjectReader::new(store, path);
    let cache = crate::meta_cache::ChunkedMetadataCache::new(reader.clone());
    let mut meta = TiffMetadataReader::try_open(&cache).await?;
    let ifds = meta.read_all_ifds(&cache).await?;
    let endianness = meta.endianness();
    Ok(TIFF::new(ifds, endianness))
}

/// Compute the WGS84 lon/lat bbox of the raster at `src`.
/// @noRd
/// @keywords internal
#[extendr]
fn a5_raster_bbox_lonlat_rs(
    src: &str,
    store_keys: Vec<String>,
    store_values: Vec<String>,
) -> Result<Vec<f64>> {
    let store_opts = parse_store_opts(store_keys, store_values)?;
    let runtime = crate::runtime::shared_runtime()?;
    runtime.block_on(async move {
        let tiff = open_tiff(src, &store_opts).await?;
        let ifd = tiff
            .ifds()
            .first()
            .ok_or_else(|| A5CogError::Invalid("no IFDs".into()))?;
        let geo = ifd
            .geo_key_directory()
            .ok_or(A5CogError::MissingGeoKey("GeoKeyDirectory"))?;
        let src_proj = crate::geo::build_src_proj(geo)?;
        let dst_proj = Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs")?;
        let gt = crate::geo::extract_geotransform(ifd)?;
        let b = footprint_lonlat(
            &src_proj, &dst_proj, &gt,
            ifd.image_width() as usize, ifd.image_height() as usize,
        )?;
        Ok(b.to_vec())
    })
}

/// Structural metadata of the raster at `src`: dimensions, block grid,
/// overview levels, data type, nodata, band names, CRS and WGS 84 envelope.
/// Overview rows list every reduced-resolution IFD that a5px would consider
/// (same filter as `select_overview_level`), in IFD order.
/// @noRd
/// @keywords internal
#[extendr]
fn a5_raster_info_rs(
    src: &str,
    store_keys: Vec<String>,
    store_values: Vec<String>,
) -> Result<Robj> {
    let store_opts = parse_store_opts(store_keys, store_values)?;
    let runtime = crate::runtime::shared_runtime()?;
    runtime.block_on(async move {
        let tiff = open_tiff(src, &store_opts).await?;
        let ifd0 = tiff
            .ifds()
            .first()
            .ok_or_else(|| A5CogError::Invalid("no IFDs".into()))?;
        let geo = ifd0
            .geo_key_directory()
            .ok_or(A5CogError::MissingGeoKey("GeoKeyDirectory"))?;
        let (src_proj, crs) = crate::geo::build_src_proj_described(geo)?;
        let dst_proj = Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs")?;
        let gt = crate::geo::extract_geotransform(ifd0)?;
        let full_w = ifd0.image_width() as usize;
        let full_h = ifd0.image_height() as usize;
        let n_bands = ifd0.samples_per_pixel() as usize;
        let bbox = footprint_lonlat(&src_proj, &dst_proj, &gt, full_w, full_h)?;
        let (block_w, block_h) = match (ifd0.tile_width(), ifd0.tile_height()) {
            (Some(w), Some(h)) => (w as i32, h as i32),
            _ => (0, 0),
        };
        let (n_blocks_x, n_blocks_y) = ifd0.tile_count().unwrap_or((0, 0));
        let dtype = crate::band_fetch::derive_data_type(ifd0)
            .map(|d| format!("{d:?}").to_ascii_lowercase())
            .unwrap_or_else(|| "mixed".to_string());
        let nodata = parse_nodata(ifd0).unwrap_or(f64::NAN);
        let band_names_v = parse_band_descriptions(ifd0, n_bands);
        let band_names: Vec<String> = if band_names_v.is_empty() {
            (0..n_bands).map(|i| format!("band_{:02}", i + 1)).collect()
        } else {
            band_names_v
        };
        let interleave = match ifd0.planar_configuration() {
            PlanarConfiguration::Chunky => "pixel",
            PlanarConfiguration::Planar => "band",
            _ => "unknown",
        };
        let compression = format!("{:?}", ifd0.compression()).to_ascii_lowercase();

        let mut ov_level: Vec<i32> = Vec::new();
        let mut ov_w: Vec<i32> = Vec::new();
        let mut ov_h: Vec<i32> = Vec::new();
        let mut ov_bw: Vec<i32> = Vec::new();
        let mut ov_bh: Vec<i32> = Vec::new();
        for (i, ifd) in tiff.ifds().iter().enumerate().skip(1) {
            if let Some(st) = ifd.new_subfile_type() {
                if st & 0x1 == 0 || st & 0x4 != 0 {
                    continue;
                }
            }
            let w = ifd.image_width() as usize;
            let h = ifd.image_height() as usize;
            if w == 0 || h == 0 || w >= full_w || h >= full_h {
                continue;
            }
            if ifd.samples_per_pixel() as usize != n_bands {
                continue;
            }
            let (Some(tw), Some(th)) = (ifd.tile_width(), ifd.tile_height()) else {
                continue;
            };
            ov_level.push(i as i32);
            ov_w.push(w as i32);
            ov_h.push(h as i32);
            ov_bw.push(tw as i32);
            ov_bh.push(th as i32);
        }

        Ok(list!(
            width = full_w as i32,
            height = full_h as i32,
            n_bands = n_bands as i32,
            dtype = dtype,
            nodata = nodata,
            band_names = band_names,
            interleave = interleave,
            compression = compression,
            block_width = block_w,
            block_height = block_h,
            n_blocks_x = n_blocks_x as i32,
            n_blocks_y = n_blocks_y as i32,
            overview_level = ov_level,
            overview_width = ov_w,
            overview_height = ov_h,
            overview_block_width = ov_bw,
            overview_block_height = ov_bh,
            crs = crs,
            geotransform = gt.0.to_vec(),
            bbox = bbox.to_vec()
        )
        .into())
    })
}

/// Diagnostic: the IFD index `a5_read_raster_rs` would read for the given
/// `overview_target_m` (the A5 cell edge in metres; 0 = overviews disabled).
/// 0 means full resolution. Exposed for testing / introspection.
/// @noRd
/// @keywords internal
#[extendr]
fn a5_select_overview_level_rs(
    src: &str,
    overview_target_m: f64,
    store_keys: Vec<String>,
    store_values: Vec<String>,
) -> Result<i32> {
    let store_opts = parse_store_opts(store_keys, store_values)?;
    let runtime = crate::runtime::shared_runtime()?;
    runtime.block_on(async move {
        let (store, path) = parse_src(src, &store_opts)?;
        let reader = ObjectReader::new(store, path);
        let cache = crate::meta_cache::ChunkedMetadataCache::new(reader.clone());
        let mut meta = TiffMetadataReader::try_open(&cache).await?;
        let ifds = meta.read_all_ifds(&cache).await?;
        let endianness = meta.endianness();
        let tiff = TIFF::new(ifds, endianness);
        let ifd0 = tiff
            .ifds()
            .first()
            .ok_or_else(|| A5CogError::Invalid("no IFDs".into()))?
            .clone();
        let geo = ifd0
            .geo_key_directory()
            .ok_or(A5CogError::MissingGeoKey("GeoKeyDirectory"))?;
        let src_proj = build_src_proj(geo)?;
        let gt0 = extract_geotransform(&ifd0)?;
        let full_w = ifd0.image_width() as usize;
        let full_h = ifd0.image_height() as usize;
        let n_bands = ifd0.samples_per_pixel() as usize;
        let level = select_overview_level(
            &tiff,
            full_w,
            full_h,
            n_bands,
            &gt0,
            src_proj.is_latlong(),
            overview_target_m,
        );
        Ok(level as i32)
    })
}

/// Diagnostic: the object store configuration `src` resolves to after
/// environment defaults and `store_opts` are applied. Never returns
/// credential values. Building the store validates the configuration
/// offline.
/// @noRd
/// @keywords internal
#[extendr]
fn a5_store_config_rs(
    src: &str,
    store_keys: Vec<String>,
    store_values: Vec<String>,
) -> Result<Robj> {
    let store_opts = parse_store_opts(store_keys, store_values)?;
    let pairs = crate::store::describe(src, &store_opts)?;
    let pairs: Vec<(String, Robj)> = pairs
        .into_iter()
        .map(|(k, v)| (k, Robj::from(v)))
        .collect();
    Ok(List::from_pairs(pairs).into())
}

extendr_module! {
    mod read;
    fn a5_store_config_rs;
    fn a5_read_raster_rs;
    fn a5_read_raster_flat_rs;
    fn a5_raster_to_parquet_rs;
    fn a5_sample_at_cells_rs;
    fn a5_sample_at_cells_flat_rs;
    fn a5_sample_to_parquet_rs;
    fn a5_raster_bbox_lonlat_rs;
    fn a5_raster_info_rs;
    fn a5_select_overview_level_rs;
}
