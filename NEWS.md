# a5px (development version)

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
