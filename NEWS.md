# a5px 0.0.0.9000

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
