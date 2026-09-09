# a5px (development version)

* Reads are 2-4x faster at fine resolutions (#6), with identical cell sets
  and counts; means and other sums can differ from 0.1.0 in the last bit
  because pixels are now summed per run and per stripe.
  - The point-to-cell lookup (`locator.rs`) tests the previous pixel's
    cell, the cells above it and the children of their parents with cached
    pentagons before falling back to the a5 search, which the a5 crate's
    own first estimate misses 42% of the time at res 18.
  - Pixel centres are projected to A5's face frame by bilinear
    interpolation over 16x16 pixel windows (`grid_proj.rs`); each window
    carries a measured error bound and any pixel within that bound of a
    cell edge or of the `bbox` is projected exactly, so results equal the
    exact path. Windows that fail to project, straddle a dodecahedron
    face, the antimeridian or a pole fall back to exact projection.
  - Each block is indexed as 64-row stripes across all `cpu_workers`, so a
    read spanning fewer blocks than workers no longer leaves cores idle;
    stripes and workers merge as fixed pairwise trees.
  - Pixels are accumulated band by band in runs of consecutive pixels of
    one cell (sequential reads on planar data), into one contiguous
    accumulator slab per stripe instead of a heap allocation per cell.
  - `a5_raster_to_parquet()` builds its columns straight from the
    accumulators (as float32 when asked, without the f64 copy) and encodes
    columns in parallel.
  - Accumulators are partitioned by cell id across `4 * cpu_workers`
    stores that stripes merge into as they finish, so no reduce step
    remains when the workers stop and peak memory is the final stores plus
    one stripe per thread (about 20% lower than 0.1.0 on a 64-band res-18
    read). Merge order follows stripe completion, so sums can differ in
    the last bit between runs; cells, counts, min and max are exact.

# a5px 0.1.0

First minor release.

* Fixed: band subsets of big-endian (`MM`) planar TIFFs were decoded
  without byte swapping and returned garbage; such files now take the
  full-tile fetch path.

* Fixed: a float32 nodata value not exactly representable in single
  precision (for example a literal `-9999.9` tag) never matched, so nodata
  pixels were counted as valid. The sentinel is now rounded through f32
  when the source is float32, for tag values and `src_nodata` alike.

* Fixed: any coordinate that failed to project aborted the whole read
  (`bbox = c(-180, -90, 180, 90)` on a LAEA raster errored with a
  tolerance message). Points are now projected individually and
  unprojectable ones dropped, in the bbox and tile filters, the pixel
  loops and the footprint envelope. Envelopes also sample 32 points per
  rectangle edge instead of corners and midpoints, so curved projected
  edges no longer clip the reported `bbox` or the tile selection.

* Fixed: bicubic and lanczos centroid sampling renormalised partial
  stencils over kernels with negative lobes, which could push values
  outside the data range next to nodata or the raster edge. Cells whose
  4x4 / 6x6 stencil is incomplete now fall back to bilinear over the
  valid pixels; complete stencils are unchanged.

* Memory: the per-cell accumulator now has three layouts chosen from the
  requested stats. `mean` / `sum` / `count` use 16 bytes per band per cell
  instead of 48, so a 64-band embedding read holds three times as many
  cells in the same memory. `min` / `max` add the extremes and `var` /
  `sd` the full Welford state. Results are unchanged.

* Memory: centroid mode no longer allocates a dense cells x bands buffer
  per CPU worker (16 bytes per cell-band per worker, so 6.6 GB for 0.8 M
  cells x 64 bands on 8 workers). Workers now return sparse per-tile
  partials merged into one buffer.

* Performance: forward reads with a nodata sentinel project only pixels
  with at least one valid band; all-nodata pixels are skipped before the
  CRS transform. Centroid setup projects cell centroids in parallel. The
  per-tile accumulator map is no longer pre-sized to a quarter of the
  tile's pixels.

* New `aoi` and `containment` arguments on `a5_read_raster()`,
  `a5_read_raster_arrow()` and `a5_raster_to_parquet()`. `aoi` is a
  polygon in WGS 84 (anything `a5R::a5_polygon_to_cells()` accepts); it is
  converted to the compacted A5 cell set selected by `containment`
  (`"centre"` or `"overlapping"`, a5R >= 0.6.0) and only those cells appear
  in the output. Selection is cell-level: an included cell receives the
  statistics of all its valid pixels. Rust tests membership per cell
  change by walking ancestors against the compacted set, so large AOIs at
  fine resolutions cost little memory. Works in all three modes and
  combines with `bbox` for chunking; in `mode = "centroid"` the AOI cells
  are sampled directly, giving gap-free polygon coverage under
  `"overlapping"`.

* New `bbox_align = c("pixel", "block")` on `a5_read_raster()`,
  `a5_read_raster_arrow()` and `a5_raster_to_parquet()`. Under `"block"`
  a COG block is read whole when its origin pixel centre lies in `bbox`
  (half-open on the max edges) and the per-pixel bbox test is skipped, so
  every block belongs to exactly one member of any bbox partition. Callers
  that chunk large reads to bound memory no longer re-fetch the blocks
  straddling chunk edges (the second observation in #4: a 3x3 chunking
  cost 4x the bytes of a single read) and per-cell partial sums and counts
  add exactly across chunks.

* New `a5_raster_info()` returns a raster's dimensions, data type, nodata,
  band names, interleave, compression, block grid, usable overview levels,
  CRS and WGS 84 envelope without reading pixels.

* Remote sources are now configured from the environment and from a new
  `store_opts` argument on `a5_read_raster()`, `a5_read_raster_arrow()`
  and `a5_raster_to_parquet()` (#4). Previously `s3://`, `gs://` and
  `az://` clients were built from the URL alone: `AWS_REGION` and
  credential variables were ignored, the region defaulted to `us-east-1`,
  and there was no unsigned mode, so public buckets outside us-east-1 were
  unreadable (off-AWS the client spent 13 s timing out against instance
  metadata). Clients now start from `object_store`'s `from_env()`
  defaults, honour GDAL's `AWS_NO_SIGN_REQUEST=YES`, and apply
  `store_opts` last so explicit keys win. Unknown keys, and any key passed
  with a local path, are errors. New `a5_store_config()` reports the
  resolved region, endpoint and signing mode without a request.

* Updated the bundled `a5` Rust crate from 0.7.3 to 0.10.0 and the Arrow /
  Parquet crates to 59. The a5 point-to-cell projection is faster and the
  cell cache fast path now converts each pixel to A5's internal spherical
  frame once, sharing it between the cached pentagon test and the search
  fallback. On the 12-band Sentinel-2 test COG at resolution 16 the a5
  indexing sub-stage fell by about 37% and single-worker wall time by
  about 25%. Cell identifiers and values are unchanged.

* Requires a5R >= 0.6.0. The overlay auto-`subsamples` rule and the
  overview target now use the true average cell edge length
  (`a5R::a5_cell_edge_length_avg()`) instead of `sqrt(cell_area)`, which
  overstates the edge by about 22%. Auto-selected `subsamples` rises by one
  step in some pixel/cell ratios and overview selection is marginally more
  conservative; both heuristics now match their documentation literally.
  Explicit `subsamples` values are unaffected.

* `a5_read_raster_arrow()` and `a5_raster_to_parquet()` gain
  `mode = "centroid"` and the `interp` argument, closing the mode gap with
  `a5_read_raster()`: centroid samples now stream straight into an Arrow
  Table or a Parquet file from the Rust flat buffer, with no per-cell R
  materialisation. The `stat` argument is ignored under centroid (one
  sample per cell); table and file metadata record the pseudo-stat
  `"centroid"`.

* `mode = "centroid"` gains an `interp` argument: `"nearest"` (default,
  unchanged), `"bilinear"`, `"bicubic"` (Keys) or `"lanczos"` (Lanczos-3).
  Stencils crossing internal tile boundaries are handled by accumulating
  partial weighted sums per tile and merging additively, so no extra tile
  fetches are needed. Kernel weights renormalise over valid pixels: nodata
  holes and raster edges shrink the stencil instead of propagating NA, and
  a smooth kernel can recover cells whose centroid pixel is nodata. With
  `dequant`, stencil pixels are decoded before the kernel. Bilinear matches
  `terra::extract(method = "bilinear")` to float32 precision.

* New categorical stats for integer rasters of 16 bits or fewer (land cover,
  masks, zone IDs): `stat = "majority"` returns each cell's most-weighted
  class (pixel counts under `mode = "forward"`, overlap areas under
  `mode = "overlay"`; ties break toward the smallest class code) and
  combines freely with the continuous stats in one pass.
  `stat = "fractions"` returns per-band list-columns of named per-class
  weight shares summing to 1 (`a5_read_raster()` only, as the sole stat).
  Both treat raw codes as class labels and cannot be combined with
  `dequant`. Distinct classes per cell are capped at 4096 so a continuous
  raster passed by mistake fails loudly.

* New sampling mode `mode = "overlay"` in `a5_read_raster()`,
  `a5_read_raster_arrow()` and `a5_raster_to_parquet()`: each pixel
  contributes to every A5 cell it overlaps, weighted by the overlapped
  fraction of its area, approximated by sub-pixel supersampling
  (`subsamples`, auto-selected by default). Under overlay, `mean` is the
  area-weighted mean, `sum` is mass-preserving (totals such as population
  counts are conserved exactly), and `count` is the effective fractional
  pixel count. Pixels interior to a cell take a fast path costing the same
  as `mode = "forward"`; only pixels straddling a cell boundary pay the
  supersampling cost. Agreement with exactextract's exact area weighting is
  within 0.05% of the value range at `subsamples = 16` on the test fixture,
  converging quadratically in `subsamples`. All stats now use weighted
  accumulators internally; forward and centroid results are unchanged
  (weights of 1).

* `a5_read_raster()`, `a5_read_raster_arrow()` and `a5_raster_to_parquet()`
  gain `dequant`: a per-pixel decode applied before aggregation, as any
  vectorised R function evaluated over the integer code domain and applied
  in Rust via a lookup table. The new export `dequant_aef()` implements the
  Alpha Earth Foundations int8 decode `sign(x) * (x / 127.5)^2`.
  Nonlinear decodes do not commute with aggregation, so quantized sources
  read without `dequant` produced biased cell statistics. Requires an integer
  source of 16 bits or fewer; nodata is matched against the raw code. Setting
  `dequant` flips the `use_overviews` default to `FALSE` because
  average-resampled overview pixels are means of quantized codes and decode
  incorrectly; passing `use_overviews = TRUE` explicitly warns and proceeds.
  `mode = "centroid"` decodes its sampled values with the same table.

* `a5_read_raster()`, `a5_read_raster_arrow()` and `a5_raster_to_parquet()`
  gain `use_overviews` (default `TRUE`). When the requested `stat` is `"mean"`,
  the reader now reads the coarsest COG overview that still oversamples the
  target A5 cell instead of the full-resolution image, cutting I/O and CPU for
  aggregations to cells much coarser than the source pixels (roughly 6x faster
  in local tests aggregating a 30 m raster to ~6 km cells). Overviews are only
  used for `"mean"` — `sum`, `count`, `var`, `sd`, `min` and `max` are not
  preserved under decimation and always read full resolution. Sources without
  overviews read full resolution regardless. Set `use_overviews = FALSE` to
  force full resolution.

* Updated for a5R (>= 0.4.0): the removed `a5R::a5_grid()` is replaced by
  `a5R::a5_polygon_to_cells()` in the `mode = "centroid"` path. `wk` moves from
  Suggests to Imports.
</content>
