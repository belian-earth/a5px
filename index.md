# a5px

a5px aggregates raster pixels into [A5](https://a5geo.org/) cells. A5 is
a discrete global grid system (DGGS): it divides the globe into
equal-area pentagonal cells at 31 resolutions. a5px streams GeoTIFF and
Cloud-Optimised GeoTIFF (COG) files from local disk or cloud object
stores and computes per-cell statistics in a single pass, in Rust.

Results are a tibble, an Arrow table or a Parquet file keyed by
[`a5R::a5_cell`](https://belian-earth.github.io/a5R/reference/a5_cell.html).
On a 12-band Sentinel-2 COG, a5px is about 7 times faster end to end
than `gdal raster zonal-stats` and 11 times faster than a `terra` and
`dplyr` pipeline.

## Installation

a5px is not on CRAN. Install the development version from GitHub:

``` r

# install.packages("pak")
pak::pak("belian-earth/a5px")
```

Building from source needs a [Rust toolchain](https://rustup.rs/) with
`rustc` 1.89 or later. pak also installs
[a5R](https://github.com/belian-earth/a5R), which provides the cell
type. The examples below plot with
[a5view](https://github.com/belian-earth/a5view):

``` r

pak::pak("belian-earth/a5view")
```

## Example

[Earth
Genome](https://source.coop/earthgenome/sentinel2-temporal-mosaics)
publishes annual cloud-free Sentinel-2 mosaics as one COG per band. This
reads the red, green and blue bands of a 2023 tile over the fjords of
south Greenland into resolution-14 cells, about 360 m across.
`separate = TRUE` makes each file a column, named after the file, and
the three files share one fetch queue. With the default `stat = "mean"`,
a5px reads the coarsest COG overview that still has several pixels per
cell, rather than every 10 m pixel.

``` r

library(a5px)

tile <- paste0(
  "https://data.source.coop/earthgenome/sentinel2-temporal-mosaics/",
  "23VMH_2023-01-01_2024-01-01/"
)
rgb <- paste0(tile, c("B04", "B03", "B02"), ".tif") # red, green, blue

fjords <- a5_read_raster(rgb, resolution = 14L, separate = TRUE)
fjords
#> # A tibble: 95,875 × 4
#>    cell               B04   B03   B02
#>    <a5_cell>        <dbl> <dbl> <dbl>
#>  1 0da201e280000000 8468. 8411. 8809.
#>  2 0da201de80000000 7548. 7455. 7794.
#>  3 0da2be2780000000 7176. 7007. 7260.
#>  4 0da201e080000000 8523. 8439. 8826.
#>  5 0da2be2980000000 6389. 6308. 6632.
#>  6 0da2be2580000000 8230. 7798. 8373.
#>  7 0da2f30c80000000  846.  818.  753.
#>  8 0da2f30a80000000  764.  723.  637.
#>  9 0da2f03a80000000 1188. 1190. 1176.
#> 10 0da1ff1080000000 8863. 8560. 8758.
#> # ℹ 95,865 more rows
```

a5view’s
[`geom_a5()`](https://belian-earth.github.io/a5view/reference/geom_a5.html)
draws the cells as a ggplot2 layer, and
[`cells_rgb()`](https://belian-earth.github.io/a5view/reference/cells_rgb.html)
turns the three bands into a true-colour composite. The `range` sets the
reflectance values (scaled by 10,000) that map to black and full
brightness.

``` r

library(ggplot2)
library(a5view)

ggplot(fjords, aes(cell = cell, fill = cells_rgb(B04, B03, B02, range = c(0, 4500)))) +
  geom_a5() +
  theme_void()
```

![](reference/figures/README-fjords-1.png)

`bbox` reads a subset. Here a glacier meets the fjord at resolution 15,
about 180 m across, with outlines to show the pentagonal cells:

``` r

glacier <- a5_read_raster(
  rgb, resolution = 15L, bbox = c(-45.36, 61.11, -45.14, 61.21), separate = TRUE
)

ggplot(glacier, aes(cell = cell, fill = cells_rgb(B04, B03, B02, range = c(0, 4500)))) +
  geom_a5(colour = "white", linewidth = 0.05) +
  theme_void()
```

![](reference/figures/README-glacier-1.png)

## What else it does

- **Three outputs** from one engine: a tibble
  ([`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md)),
  an Arrow table
  ([`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md))
  or a Parquet file written straight from Rust
  ([`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md)).
- **Statistics** in one pass: mean, sum, count, min, max, variance,
  standard deviation, majority class and class fractions.
- **Sampling modes**: pixel centres (the default), area-weighted overlay
  that conserves sums, or centroid sampling for cells finer than pixels.
- **Subsets** by bounding box or polygon, and chunked reads that fetch
  each COG block once.
- **Several files** in one read: bands stored one per file, tiles pooled
  into one product, or captures composited per cell.
- **Decoding** of scale and offset, or of quantized codes such as Alpha
  Earth Foundations embeddings, before aggregation.
- **Cloud sources** on S3, GCS, Azure and HTTP(S), configured from the
  environment, with a session cache of the byte ranges fetched.

[Getting
started](https://belian-earth.github.io/a5px/articles/a5px.html) covers
each of these with examples, and the
[reference](https://belian-earth.github.io/a5px/reference/) documents
every argument.

## Acknowledgements

Built on the [a5 Rust crate](https://github.com/felixpalmer/a5-rs) by
[Felix Palmer](https://github.com/felixpalmer) and on
[a5R](https://github.com/belian-earth/a5R) for the cell type and Arrow
interop. Streaming I/O and TIFF parsing come from the
[`async-tiff`](https://github.com/developmentseed/async-tiff) team at
Development Seed, with
[`object_store`](https://crates.io/crates/object_store) for cloud reads,
[`proj4rs`](https://crates.io/crates/proj4rs) for reprojection and
[`extendr`](https://extendr.github.io/extendr/extendr_api/) for the R
bindings.
