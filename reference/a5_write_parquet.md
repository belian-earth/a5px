# Write A5-keyed values to a Parquet file

Convenience writer. Accepts either an
[arrow::Table](https://arrow.apache.org/docs/r/reference/Table-class.html)
from
[`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
or a
[`tibble::tibble()`](https://tibble.tidyverse.org/reference/tibble.html)
from
[`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md);
picks the right path under the hood. The on-disk schema is the same in
both cases when the tibble has a single fixed-length `value` list column
(i.e. was produced with `as_vector = TRUE`).

## Usage

``` r
a5_write_parquet(x, dest, compression = "zstd", ...)
```

## Arguments

- x:

  Output of
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md)
  (tibble) or
  [`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
  (Arrow Table).

- dest:

  Output Parquet path.

- compression:

  Parquet compression codec. Default `"zstd"`.

- ...:

  Further arguments forwarded to
  [`arrow::write_parquet()`](https://arrow.apache.org/docs/r/reference/write_parquet.html).

## Value

`dest` invisibly.

## Examples

``` r
if (FALSE) { # \dontrun{
  tbl <- a5_read_raster_arrow(path, resolution = 14L, bands = 1:8)
  a5_write_parquet(tbl, "out.parquet")
} # }
```
