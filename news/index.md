# Changelog

## a5px 0.2.0

- `src` takes several files, read in one pass: their tiles share the
  fetch queue (`io_concurrency`) and the workers (`cpu_workers`). With
  `separate = TRUE` each file adds its bands, as
  `gdalbuildvrt -separate`, so bands stored one per file (as in many
  Sentinel-2 archives) come back as columns of one table. With
  `separate = FALSE` (default) the files are pieces of one raster with
  the same bands and pixel size: every valid pixel enters its cell’s
  statistics once, so cells on a seam combine exactly, and where files
  overlap every capture counts. `composite` instead computes `stat` per
  file and reduces across files per cell (`"mean"`, `"median"`, `"min"`,
  `"max"`, `"sum"`, `"count"`, `"var"`, `"sd"`, `"majority"`), weighting
  each file by its valid pixel count in the cell; `composite = "count"`
  counts the captures of each cell. Pooled files must store each band
  with the same scale and offset unless `scoff` or `dequant` decodes
  them, and with `use_overviews` they all read one overview pixel size,
  the coarsest suitable one they share, so a mosaic whose small edge
  tiles lack overviews is not mixed with coarser pixels. In centroid
  mode each file samples only the cells on it. Errors from a worker are
  now reported instead of “consumer pool dropped channel”.

- **Breaking:** `a5_store_config()` is now
  [`a5px_store_config()`](https://belian-earth.github.io/a5px/reference/a5px_store_config.md),
  `a5_write_parquet()` is now
  [`a5px_write_parquet()`](https://belian-earth.github.io/a5px/reference/a5px_write_parquet.md),
  and
  [`a5_aggregate()`](https://belian-earth.github.io/a5px/reference/a5_aggregate.md)
  takes `resolution` in place of `to_resolution`. The `a5px_` prefix
  marks package machinery (configuration, cache, store, writer); `a5_`
  stays on the verbs that turn rasters into A5 cells. a5R moves from
  Depends to Imports, so attach it yourself to call its functions
  unqualified.

- **Breaking:**
  [`a5_aggregate()`](https://belian-earth.github.io/a5px/reference/a5_aggregate.md)
  weights `"mean"` by each row’s pixel count, the `npix` column when
  present or a column named by the new `weights` argument, and sums
  `npix` rather than averaging it, so an aggregated mean matches a
  direct read at the coarse resolution. Input cells must share one
  resolution.

- NaN pixels are missing data whatever the nodata tag: they counted as
  valid unless the nodata value was itself NaN, giving NaN means and
  inflated counts. Codes a `dequant` function maps to `NA` are missing
  too, and a bilinear or cubic centroid sample renormalises over the
  valid pixels around a NaN. `src_nodata` accepts `NaN` and infinities.

- A process forked after a read
  ([`parallel::mcparallel()`](https://rdrr.io/r/parallel/mcparallel.html),
  `mclapply()`) could hang forever on its first read, since the shared
  runtime’s threads do not exist in the child. Each process now builds
  its own.

- Resolution 30 is refused for now: the a5 crate returns resolution-29
  cells for about 30% of points at 30, so a read’s cells mixed
  resolutions.

- The default `cpu_workers` is the number of CPUs available to the
  process, honouring container and cgroup CPU quotas, rather than every
  core of the host.

- Errors carry condition classes: `a5px_error_source` (missing or
  unreadable sources, unsupported layouts, store and network failures),
  `a5px_error_input` with its subclasses `a5px_error_geometry` and
  `a5px_error_resolution`, and `a5px_error_internal`, all also
  `a5px_error`.

- `containment = "center"` is accepted beside `"centre"`. A `bbox`
  outside longitudes -180 to 180 or latitudes -90 to 90, and repeated
  `bands`, are errors.

- Remote reads keep the byte ranges they fetch (header chunks and
  compressed blocks) in a session cache, so a read split into `bbox`
  chunks, or repeated, fetches each block once: a 1 km AEF box over
  HTTPS read in 25 chunks took about 15 s, as one read does, against
  about 400 s before.
  [`a5px_set_cache()`](https://belian-earth.github.io/a5px/reference/a5px_set_cache.md)
  sets the budget (512 MiB by default, at most an eighth of a cgroup
  memory limit, or `A5PX_CACHE_MB`; 0 disables it),
  [`a5px_cache_info()`](https://belian-earth.github.io/a5px/reference/a5px_set_cache.md)
  reports it and
  [`a5px_clear_cache()`](https://belian-earth.github.io/a5px/reference/a5px_set_cache.md)
  empties it. Local files bypass the cache.

- A read that finds no pixels keeps the `npix` column when it was asked
  for, so its tibble, Arrow table or Parquet file has the same schema as
  any other read.

- A `bbox` far larger than the raster, up to the whole globe, now reads
  the whole raster. Tiles were chosen by projecting only the bbox’s
  outline, which collapses under transverse Mercator (both poles and
  every meridian over 90 degrees from the central one map to the central
  column), so a UTM raster returned no rows or part of its cells. The
  bbox is now cut to the raster’s footprint first, and a footprint that
  contains a pole reaches it.

- A pixel centred exactly on a `bbox` edge shared by two chunks of a
  read is now counted in one of them: the bbox is half-open on its
  `xmax` and `ymax` edges, except at 180 and 90.

- a5px now uses A5 1.0: the a5 Rust crate 1.0.0-beta.3 and a5R \>=
  0.9.9.9001. A5 1.0 introduces permanent cell ids with a new
  space-filling curve and face order, so **every cell id changes**.
  Cells keep their shape and position at every resolution except 1, so a
  read returns the same cells with the same values and pixel counts as
  before, under new ids, apart from the `aoi` and centroid changes
  below. Ids written by earlier versions are still valid cells but in
  other places; convert stored data once with
  [`a5R::a5_migrate()`](https://belian-earth.github.io/a5R/reference/a5_migrate.html)
  and never mix ids from the two versions.

- Rust dependencies are updated: proj4rs 0.2 (spherical oblique and
  south-polar Lambert azimuthal equal-area fixes), async-tiff 0.4 and
  object_store 0.14 (one reqwest 0.13 HTTP client, no panic on HTTP 500
  responses), and arrow and parquet 60. HTTPS now uses rustls with the
  aws-lc-rs provider. The minimum Rust version is 1.89.

- a5R stores the top cell byte XOR `0xFC` so that vctrs gap-filling
  yields `NA` rather than the world cell. Older a5R versions are
  incompatible: every cell id would be misread without error.
  Resolution-30 cells whose id starts with `0xFC` are no longer mistaken
  for `NA`.

- `aoi` is resolved to one A5 covering, the union of the per-feature
  coverings
  [`a5R::a5_polygon_to_cells()`](https://belian-earth.github.io/a5R/reference/a5_polygon_to_cells.html)
  now returns. Tiles are selected from the AOI’s bounding box padded by
  three cell edges, instead of the envelope of the compacted cells’
  boundaries: compacted cells are index parents, whose outlines need not
  contain the cells they stand for. With `containment = "overlapping"`,
  a5R now also selects cells that overlap the AOI by a thin sliver,
  which earlier versions missed, so such reads return a few more cells;
  every other cell keeps its statistics. An `aoi` covering the whole
  globe now selects every cell, and a
  [`wk::rct()`](https://paleolimbot.github.io/wk/reference/rct.html) 180
  degrees wide or more that does not is an error.

- `mode = "centroid"` now samples exactly the cells whose centre lies in
  the `bbox` (or the raster’s envelope). a5R joins polygon vertices by
  great circles, so the plain rectangle used before bulged poleward and
  selected cells centred outside the box. Only cells centred in that
  bulge change: up to 120 m for a box 1 degree wide, growing with width.

- [`a5_aggregate()`](https://belian-earth.github.io/a5px/reference/a5_aggregate.md)
  now groups each cell by the coarse cell containing its centre
  ([`a5R::a5_cell_to_supercell()`](https://belian-earth.github.io/a5R/reference/a5_spatial_hierarchy.html))
  instead of by index parent
  ([`a5R::a5_cell_to_parent()`](https://belian-earth.github.io/a5R/reference/a5_hierarchy.html)).
  The A5 index hierarchy is not spatially nested, so the index parent is
  often a neighbour of the containing cell. Results change for existing
  inputs.

- New `scoff` argument on
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md),
  [`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
  and
  [`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md):
  `scoff = TRUE` applies the scale and offset the file declares for each
  band (`value * scale + offset`) per pixel before aggregation, with
  nodata matched on the raw value. It works for every data type and,
  being linear, keeps overviews in use. A band without the tags takes
  the GDAL defaults (scale 1, offset 0). It cannot be combined with
  `dequant` or the categorical stats. The default stays `FALSE`.

- [`a5_raster_info()`](https://belian-earth.github.io/a5px/reference/a5_raster_info.md)
  gains `scale` and `offset` (per band, `NA` when undeclared),
  `band_metadata` (each band’s remaining GDAL metadata items, such as
  `UNITTYPE`) and `metadata` (dataset-level items), so consumers no
  longer need a second raster library to look up a product’s decoding.
  Band names now have XML entities unescaped.

- New `stat = "npix"`: a single per-cell column with the number of
  source pixels that had at least one valid band (in overlay mode, their
  summed pixel-area weight). When validity is uniform across bands it
  equals every `<band>_count` column, so `stat = c("mean", "npix")`
  carries the merge weights of a 64-band read in one column instead of
  64.

- Cloud reads fetch runs of up to four adjacent blocks per request batch
  on files without a predictor (planar or chunky, full-band or band
  subset; the previous byte-range path only served planar band subsets),
  so the object store coalesces each band’s contiguous blocks into one
  range request instead of one per block per band. `io_concurrency`
  counts fetch tasks as before, each now carrying up to four blocks
  (fewer on small reads, so at least `io_concurrency` tasks stay open).
  Files with a predictor or non-native byte order keep the per-block
  path.

- TIFF metadata is read through a chunked cache instead of an
  exponentially growing prefix, so a file whose IFDs sit at the end
  (plain GDAL output, not a COG) no longer reads the whole file to parse
  its header.

- `A5PX_PROFILE=1` now covers the whole call: open and metadata, tile
  planning, the read pipeline, output assembly, R output build, Parquet
  column build and encode, plus the time consumers spend waiting for
  tiles and producers spend blocked on a full channel.

- Reads are 2-4x faster at fine resolutions
  ([\#6](https://github.com/belian-earth/a5px/issues/6)), with identical
  cell sets and counts; means and other sums can differ from 0.1.0 in
  the last bit because pixels are now summed per run and per stripe.

  - The point-to-cell lookup (`locator.rs`) tests the previous pixel’s
    cell, the cells above it and the children of their parents with
    cached pentagons before falling back to the a5 search, which the a5
    crate’s own first estimate misses 42% of the time at res 18.
  - Pixel centres are projected to A5’s face frame by bilinear
    interpolation over 16x16 pixel windows (`grid_proj.rs`); each window
    carries a measured error bound and any pixel within that bound of a
    cell edge or of the `bbox` is projected exactly, so results equal
    the exact path. Windows that fail to project, straddle a
    dodecahedron face, the antimeridian or a pole fall back to exact
    projection.
  - Each block is indexed as 64-row stripes across all `cpu_workers`, so
    a read spanning fewer blocks than workers no longer leaves cores
    idle; stripes and workers merge as fixed pairwise trees.
  - Pixels are accumulated band by band in runs of consecutive pixels of
    one cell (sequential reads on planar data), into one contiguous
    accumulator slab per stripe instead of a heap allocation per cell.
  - [`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md)
    builds its columns straight from the accumulators (as float32 when
    asked, without the f64 copy) and encodes columns in parallel.
  - Accumulators are partitioned by cell id across `4 * cpu_workers`
    stores that stripes merge into as they finish, so no reduce step
    remains when the workers stop and peak memory is the final stores
    plus one stripe per thread (about 20% lower than 0.1.0 on a 64-band
    res-18 read). With more than one worker, merge order follows stripe
    completion, so sums can differ in the last bit between runs; cells,
    counts, min and max are exact, and single-worker reads stay
    reproducible. `A5PX_EXACT_PROJ=1` forces the exact per-pixel
    projection (diagnostics; results are identical either way).

## a5px 0.1.0

First minor release.

- Fixed: band subsets of big-endian (`MM`) planar TIFFs were decoded
  without byte swapping and returned garbage; such files now take the
  full-tile fetch path.

- Fixed: a float32 nodata value not exactly representable in single
  precision (for example a literal `-9999.9` tag) never matched, so
  nodata pixels were counted as valid. The sentinel is now rounded
  through f32 when the source is float32, for tag values and
  `src_nodata` alike.

- Fixed: any coordinate that failed to project aborted the whole read
  (`bbox = c(-180, -90, 180, 90)` on a LAEA raster errored with a
  tolerance message). Points are now projected individually and
  unprojectable ones dropped, in the bbox and tile filters, the pixel
  loops and the footprint envelope. Envelopes also sample 32 points per
  rectangle edge instead of corners and midpoints, so curved projected
  edges no longer clip the reported `bbox` or the tile selection.

- Fixed: bicubic and lanczos centroid sampling renormalised partial
  stencils over kernels with negative lobes, which could push values
  outside the data range next to nodata or the raster edge. Cells whose
  4x4 / 6x6 stencil is incomplete now fall back to bilinear over the
  valid pixels; complete stencils are unchanged.

- Memory: the per-cell accumulator now has three layouts chosen from the
  requested stats. `mean` / `sum` / `count` use 16 bytes per band per
  cell instead of 48, so a 64-band embedding read holds three times as
  many cells in the same memory. `min` / `max` add the extremes and
  `var` / `sd` the full Welford state. Results are unchanged.

- Memory: centroid mode no longer allocates a dense cells x bands buffer
  per CPU worker (16 bytes per cell-band per worker, so 6.6 GB for 0.8 M
  cells x 64 bands on 8 workers). Workers now return sparse per-tile
  partials merged into one buffer.

- Performance: forward reads with a nodata sentinel project only pixels
  with at least one valid band; all-nodata pixels are skipped before the
  CRS transform. Centroid setup projects cell centroids in parallel. The
  per-tile accumulator map is no longer pre-sized to a quarter of the
  tile’s pixels.

- New `aoi` and `containment` arguments on
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md),
  [`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
  and
  [`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md).
  `aoi` is a polygon in WGS 84 (anything
  [`a5R::a5_polygon_to_cells()`](https://belian-earth.github.io/a5R/reference/a5_polygon_to_cells.html)
  accepts); it is converted to the compacted A5 cell set selected by
  `containment` (`"centre"` or `"overlapping"`, a5R \>= 0.6.0) and only
  those cells appear in the output. Selection is cell-level: an included
  cell receives the statistics of all its valid pixels. Rust tests
  membership per cell change by walking ancestors against the compacted
  set, so large AOIs at fine resolutions cost little memory. Works in
  all three modes and combines with `bbox` for chunking; in
  `mode = "centroid"` the AOI cells are sampled directly, giving
  gap-free polygon coverage under `"overlapping"`.

- New `bbox_align = c("pixel", "block")` on
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md),
  [`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
  and
  [`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md).
  Under `"block"` a COG block is read whole when its origin pixel centre
  lies in `bbox` (half-open on the max edges) and the per-pixel bbox
  test is skipped, so every block belongs to exactly one member of any
  bbox partition. Callers that chunk large reads to bound memory no
  longer re-fetch the blocks straddling chunk edges (the second
  observation in [\#4](https://github.com/belian-earth/a5px/issues/4): a
  3x3 chunking cost 4x the bytes of a single read) and per-cell partial
  sums and counts add exactly across chunks.

- New
  [`a5_raster_info()`](https://belian-earth.github.io/a5px/reference/a5_raster_info.md)
  returns a raster’s dimensions, data type, nodata, band names,
  interleave, compression, block grid, usable overview levels, CRS and
  WGS 84 envelope without reading pixels.

- Remote sources are now configured from the environment and from a new
  `store_opts` argument on
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md),
  [`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
  and
  [`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md)
  ([\#4](https://github.com/belian-earth/a5px/issues/4)). Previously
  `s3://`, `gs://` and `az://` clients were built from the URL alone:
  `AWS_REGION` and credential variables were ignored, the region
  defaulted to `us-east-1`, and there was no unsigned mode, so public
  buckets outside us-east-1 were unreadable (off-AWS the client spent 13
  s timing out against instance metadata). Clients now start from
  `object_store`’s `from_env()` defaults, honour GDAL’s
  `AWS_NO_SIGN_REQUEST=YES`, and apply `store_opts` last so explicit
  keys win. Unknown keys, and any key passed with a local path, are
  errors. New `a5_store_config()` reports the resolved region, endpoint
  and signing mode without a request.

- Updated the bundled `a5` Rust crate from 0.7.3 to 0.10.0 and the Arrow
  / Parquet crates to 59. The a5 point-to-cell projection is faster and
  the cell cache fast path now converts each pixel to A5’s internal
  spherical frame once, sharing it between the cached pentagon test and
  the search fallback. On the 12-band Sentinel-2 test COG at resolution
  16 the a5 indexing sub-stage fell by about 37% and single-worker wall
  time by about 25%. Cell identifiers and values are unchanged.

- Requires a5R \>= 0.6.0. The overlay auto-`subsamples` rule and the
  overview target now use the true average cell edge length
  ([`a5R::a5_cell_edge_length_avg()`](https://belian-earth.github.io/a5R/reference/a5_resolution_stats.html))
  instead of `sqrt(cell_area)`, which overstates the edge by about 22%.
  Auto-selected `subsamples` rises by one step in some pixel/cell ratios
  and overview selection is marginally more conservative; both
  heuristics now match their documentation literally. Explicit
  `subsamples` values are unaffected.

- [`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
  and
  [`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md)
  gain `mode = "centroid"` and the `interp` argument, closing the mode
  gap with
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md):
  centroid samples now stream straight into an Arrow Table or a Parquet
  file from the Rust flat buffer, with no per-cell R materialisation.
  The `stat` argument is ignored under centroid (one sample per cell);
  table and file metadata record the pseudo-stat `"centroid"`.

- `mode = "centroid"` gains an `interp` argument: `"nearest"` (default,
  unchanged), `"bilinear"`, `"bicubic"` (Keys) or `"lanczos"`
  (Lanczos-3). Stencils crossing internal tile boundaries are handled by
  accumulating partial weighted sums per tile and merging additively, so
  no extra tile fetches are needed. Kernel weights renormalise over
  valid pixels: nodata holes and raster edges shrink the stencil instead
  of propagating NA, and a smooth kernel can recover cells whose
  centroid pixel is nodata. With `dequant`, stencil pixels are decoded
  before the kernel. Bilinear matches
  `terra::extract(method = "bilinear")` to float32 precision.

- New categorical stats for integer rasters of 16 bits or fewer (land
  cover, masks, zone IDs): `stat = "majority"` returns each cell’s
  most-weighted class (pixel counts under `mode = "forward"`, overlap
  areas under `mode = "overlay"`; ties break toward the smallest class
  code) and combines freely with the continuous stats in one pass.
  `stat = "fractions"` returns per-band list-columns of named per-class
  weight shares summing to 1
  ([`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md)
  only, as the sole stat). Both treat raw codes as class labels and
  cannot be combined with `dequant`. Distinct classes per cell are
  capped at 4096 so a continuous raster passed by mistake fails loudly.

- New sampling mode `mode = "overlay"` in
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md),
  [`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
  and
  [`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md):
  each pixel contributes to every A5 cell it overlaps, weighted by the
  overlapped fraction of its area, approximated by sub-pixel
  supersampling (`subsamples`, auto-selected by default). Under overlay,
  `mean` is the area-weighted mean, `sum` is mass-preserving (totals
  such as population counts are conserved exactly), and `count` is the
  effective fractional pixel count. Pixels interior to a cell take a
  fast path costing the same as `mode = "forward"`; only pixels
  straddling a cell boundary pay the supersampling cost. Agreement with
  exactextract’s exact area weighting is within 0.05% of the value range
  at `subsamples = 16` on the test fixture, converging quadratically in
  `subsamples`. All stats now use weighted accumulators internally;
  forward and centroid results are unchanged (weights of 1).

- [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md),
  [`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
  and
  [`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md)
  gain `dequant`: a per-pixel decode applied before aggregation, as any
  vectorised R function evaluated over the integer code domain and
  applied in Rust via a lookup table. The new export
  [`dequant_aef()`](https://belian-earth.github.io/a5px/reference/dequant_aef.md)
  implements the Alpha Earth Foundations int8 decode
  `sign(x) * (x / 127.5)^2`. Nonlinear decodes do not commute with
  aggregation, so quantized sources read without `dequant` produced
  biased cell statistics. Requires an integer source of 16 bits or
  fewer; nodata is matched against the raw code. Setting `dequant` flips
  the `use_overviews` default to `FALSE` because average-resampled
  overview pixels are means of quantized codes and decode incorrectly;
  passing `use_overviews = TRUE` explicitly warns and proceeds.
  `mode = "centroid"` decodes its sampled values with the same table.

- [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md),
  [`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
  and
  [`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md)
  gain `use_overviews` (default `TRUE`). When the requested `stat` is
  `"mean"`, the reader now reads the coarsest COG overview that still
  oversamples the target A5 cell instead of the full-resolution image,
  cutting I/O and CPU for aggregations to cells much coarser than the
  source pixels (roughly 6x faster in local tests aggregating a 30 m
  raster to ~6 km cells). Overviews are only used for `"mean"` — `sum`,
  `count`, `var`, `sd`, `min` and `max` are not preserved under
  decimation and always read full resolution. Sources without overviews
  read full resolution regardless. Set `use_overviews = FALSE` to force
  full resolution.

- Updated for a5R (\>= 0.4.0): the removed `a5R::a5_grid()` is replaced
  by
  [`a5R::a5_polygon_to_cells()`](https://belian-earth.github.io/a5R/reference/a5_polygon_to_cells.html)
  in the `mode = "centroid"` path. `wk` moves from Suggests to Imports.
