# Read a (Cloud-Optimised) GeoTIFF as A5 cells, returning an Arrow Table

Same engine as
[`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md)
but returns an Arrow `Table` whose value column is a single
`FixedSizeList<float, n_bands>`. Bypasses R-list construction entirely
(Rust hands a flat cell-major buffer straight to the Arrow array
constructor), making it the right entry point for embedding rasters
destined for Parquet.

## Usage

``` r
a5_read_raster_arrow(
  src,
  resolution,
  stat = "mean",
  bands = NULL,
  bbox = NULL,
  bbox_align = c("pixel", "block"),
  aoi = NULL,
  containment = c("centre", "overlapping"),
  src_nodata = NULL,
  mode = c("forward", "overlay", "centroid"),
  subsamples = NULL,
  interp = c("nearest", "bilinear", "bicubic", "lanczos"),
  cpu_workers = NULL,
  io_concurrency = NULL,
  scoff = FALSE,
  dequant = NULL,
  as_vector = FALSE,
  value_type = c("float64", "float32"),
  use_overviews = is.null(dequant),
  store_opts = NULL,
  separate = FALSE,
  composite = NULL
)
```

## Arguments

- src, resolution, stat, bands, bbox, src_nodata, cpu_workers,
  io_concurrency, scoff, dequant, subsamples, interp, as_vector,
  use_overviews, store_opts, bbox_align, aoi, containment, separate,
  composite:

  See
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md).

- mode:

  Sampling mode: `"forward"` (default), `"overlay"`, or `"centroid"`.
  See
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md).
  Under `"centroid"` the `stat` argument is ignored (one sample per
  cell) and the table metadata records the pseudo-stat `"centroid"`.

- value_type:

  Storage type for the value column. `"float64"` (default) or
  `"float32"` (halves disk size for embeddings).

## Value

An
[arrow::Table](https://arrow.apache.org/docs/r/reference/Table-class.html)
with two columns:

- `cell`: `uint64` (interoperable with
  [`a5R::a5_cell_from_arrow()`](https://belian-earth.github.io/a5R/reference/a5_cell_from_arrow.html)).

- `value`: `FixedSizeList<float, n_bands>`. Field-level metadata records
  the band names so dimension order is preserved when the table is
  written to Parquet and read back in another language.

## See also

[`a5_write_parquet()`](https://belian-earth.github.io/a5px/reference/a5_write_parquet.md)
to write the result to Parquet,
[`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md)
for the tibble path.

## Examples

``` r
if (FALSE) { # \dontrun{
  tbl <- a5_read_raster_arrow(
    "https://data.source.coop/.../tile.tiff",
    resolution = 14L, bands = 1:8, value_type = "float32"
  )
  a5_write_parquet(tbl, "embeddings.parquet")
} # }
```
