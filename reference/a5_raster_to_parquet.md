# Read a raster and write it straight to Parquet (Rust-direct)

Same engine and on-disk schema as
[`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md) +
[`a5px_write_parquet()`](https://belian-earth.github.io/a5px/reference/a5px_write_parquet.md),
but skips the R/Arrow round-trip entirely: the Rust side aggregates
pixels into A5 cells, builds the `cell:uint64` +
`value:FixedSizeList<float, n_bands>` RecordBatch from its existing flat
buffer, and writes Parquet via the `parquet` Rust crate. No intermediate
R numeric materialisation.

## Usage

``` r
a5_raster_to_parquet(
  src,
  dest,
  resolution,
  stat = "mean",
  bands = NULL,
  bbox = NULL,
  bbox_align = c("pixel", "block"),
  aoi = NULL,
  containment = c("centre", "overlapping"),
  mask = NULL,
  src_nodata = NULL,
  mode = c("forward", "overlay", "centroid"),
  subsamples = NULL,
  interp = c("nearest", "bilinear", "bicubic", "lanczos"),
  as_vector = FALSE,
  value_type = c("float64", "float32"),
  compression = c("zstd", "snappy", "none"),
  cpu_workers = NULL,
  io_concurrency = NULL,
  scoff = FALSE,
  dequant = NULL,
  use_overviews = is.null(dequant),
  store_opts = NULL,
  separate = FALSE,
  composite = NULL
)
```

## Arguments

- src, resolution, stat, bands, bbox, src_nodata, cpu_workers,
  io_concurrency, scoff, dequant, subsamples, interp, as_vector,
  use_overviews, store_opts, bbox_align, aoi, containment, mask,
  separate, composite:

  See
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md).

- dest:

  Output Parquet path.

- mode:

  Sampling mode: `"forward"` (default), `"overlay"`, or `"centroid"`.
  See
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md).
  Under `"centroid"` the `stat` argument is ignored (one sample per
  cell) and the file metadata records the pseudo-stat `"centroid"`.

- value_type:

  Storage type for the value column. `"float64"` (default) or
  `"float32"` (halves the disk size for embeddings).

- compression:

  Parquet compression codec. One of `"zstd"` (default), `"snappy"`,
  `"none"`.

## Value

`dest` invisibly.

## Details

Pick this when you want a Parquet file at the end and don't need the
Arrow Table in R. For million-cell embedding rasters it avoids the
per-cell list-of-vectors construction that the R-Arrow path does, which
is the main remaining cost in that pipeline.

## Examples

``` r
if (FALSE) { # \dontrun{
  a5_raster_to_parquet(
    "https://data.source.coop/.../tile.tiff",
    "embeddings.parquet",
    resolution = 14L, bands = 1:8, value_type = "float32"
  )
} # }
```
