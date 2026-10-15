# Read a (Cloud-Optimised) GeoTIFF as A5-cell aggregated values

Streams a GeoTIFF or COG, projects each pixel centre to WGS 84, indexes
it to an A5 cell at the requested resolution, and aggregates per-band
values into a tibble keyed by cell.

## Usage

``` r
a5_read_raster(
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
  use_overviews = is.null(dequant),
  store_opts = NULL,
  separate = FALSE,
  composite = NULL
)
```

## Arguments

- src:

  Paths or URLs of one or more GeoTIFFs / COGs. Supported schemes: local
  path (no scheme), `file://`, `http(s)://`, `s3://`, `gs://`, `az://`.
  See the "Remote sources" section of
  [a5px-package](https://belian-earth.github.io/a5px/reference/a5px-package.md)
  for how cloud clients are configured. Several sources are read in one
  pass: their tiles share `io_concurrency` and `cpu_workers`. `separate`
  sets how they combine; names of `src` label the sources.

- resolution:

  Integer scalar A5 resolution (0–30).

- stat:

  Aggregation. One of `"mean"`, `"sum"`, `"count"`, `"min"`, `"max"`,
  `"var"`, `"sd"`, `"majority"`, `"fractions"`, `"npix"`, or any
  non-duplicated subset of those (except `"fractions"`, which must be
  alone) for a one-pass multi-stat read. Default `"mean"`. `"var"` /
  `"sd"` use the sample formula (divisor n - 1) computed via Welford's
  online algorithm in the streaming aggregator, matching
  [`stats::var()`](https://rdrr.io/r/stats/cor.html) /
  [`stats::sd()`](https://rdrr.io/r/stats/sd.html); cells covered by a
  single pixel return `NA`.

  `"npix"` is per cell rather than per band: a single `npix` column with
  the number of source pixels that had at least one valid band (under
  `mode = "overlay"`, their summed pixel-area weight). When validity is
  the same across bands, as with a dataset-wide nodata sentinel, it
  equals every `<band>_count` column, so `stat = c("mean", "npix")`
  carries the weights for a later merge in one column instead of one per
  band. Not available in `mode = "centroid"`.

  `"majority"` and `"fractions"` are categorical: they treat the raw
  integer codes as class labels, and require an integer source of 16
  bits or fewer (like `dequant`, with which they cannot be combined).
  `"majority"` returns the class with the greatest total weight in each
  cell (pixel count under `mode = "forward"`, overlap area under
  `mode = "overlay"`), ties broken toward the smallest class code.
  `"fractions"` returns, per band, a list-column of named numeric
  vectors: each cell's per-class share of its total valid weight (shares
  sum to 1; names are the class codes). `"fractions"` is only available
  in `a5_read_raster()` with `as_vector = FALSE`.

- bands:

  Bands to read. One of:

  - `NULL` (default): read every band.

  - integer / numeric vector: 1-based band indices to read.

  - character vector: band names matched against the GDAL `DESCRIPTION`
    tag (falling back to `band_NN` when descriptions are absent).

- bbox:

  Optional spatial subset, as a numeric `c(xmin, ymin, xmax, ymax)` in
  WGS 84 lon/lat. When supplied, only tiles overlapping the bbox (in
  raster CRS) are fetched from the COG, and pixels outside the bbox are
  skipped. `NULL` (default) reads the whole raster.

- bbox_align:

  How `bbox` selects pixels. `"pixel"` (default) keeps every pixel whose
  centre lies in the bbox. `"block"` keeps every internal COG block
  (tile) whose origin pixel centre lies in the bbox, whole, and skips
  the per-pixel test; the bbox is half-open on its `xmax` / `ymax`
  edges. Under `"block"` each block of the raster belongs to exactly one
  member of any partition of bboxes, so a caller that chunks a large
  read to bound memory fetches every block once instead of re-fetching
  the blocks straddling chunk edges, and per-cell partial sums and
  counts from the chunks add up exactly. The trade is block-granular
  chunk edges. When overviews are in use the block grid is that of the
  overview level read, which is the same for every chunk of a given
  `resolution` and `stat`. Requires `bbox`; not applicable to
  `mode = "centroid"`. See
  [`a5_raster_info()`](https://belian-earth.github.io/a5px/reference/a5_raster_info.md)
  for the block grid.

- aoi:

  Optional area of interest: a polygon or multipolygon in WGS 84 as
  anything
  [`a5R::a5_polygon_to_cells()`](https://belian-earth.github.io/a5R/reference/a5_polygon_to_cells.html)
  accepts (a `wk` geometry, `sf` / `sfc`, WKT, a terra `SpatVector`, or
  a lon/lat matrix). The AOI is converted to the set of A5 cells at
  `resolution` selected by `containment`, and only those cells appear in
  the output. Selection is cell-level: an included cell receives the
  statistics of **all** its valid pixels, including any outside the
  polygon, and an excluded cell receives nothing. Hole interiors are
  excluded, and several features form one AOI (the union of their
  cells). Combine with `bbox` to chunk a large AOI; without `bbox`,
  tiles are selected by the AOI's bounding box padded by three cell
  edges, so a very irregular AOI still reads its whole envelope. In
  `mode = "centroid"` the AOI cells are sampled directly. Polygon edges
  are great circles, as in a5R, so a
  [`wk::rct()`](https://paleolimbot.github.io/wk/reference/rct.html)
  bulges poleward rather than following the parallels, and one 180
  degrees wide or more is an error unless it covers the globe.

- containment:

  How `aoi` selects cells, as in
  [`a5R::a5_polygon_to_cells()`](https://belian-earth.github.io/a5R/reference/a5_polygon_to_cells.html):
  `"centre"` (default) keeps cells whose centre lies inside the polygon;
  `"overlapping"` also keeps every cell the polygon boundary touches,
  for gap-free coverage. Only used with `aoi`.

- src_nodata:

  Optional numeric scalar overriding the source nodata. Use this when
  the file's `TIFFTAG_GDAL_NODATA` tag is missing or wrong; it takes
  precedence over the metadata value when set. `NULL` (default) uses
  whatever `async-tiff` exposes.

- mode:

  Sampling mode. `"forward"` (default) aggregates pixels into cells by
  their centres; `"overlay"` aggregates with pixel-cell overlap weights;
  `"centroid"` samples one pixel per cell at the cell centroid. See
  Details.

- subsamples:

  Sub-point grid dimension per pixel for `mode = "overlay"`: each pixel
  straddling a cell boundary is split into `subsamples^2` sub-points
  whose per-cell counts give the overlap weights (interior pixels take a
  fast path and never pay this cost). `NULL` (default) auto-selects from
  the pixel/cell edge ratio so sub-point spacing is at most half the
  cell edge, clamped to `[2, 16]`. Larger values approximate exact area
  weighting more closely. Only used when `mode = "overlay"`; an error
  otherwise.

- interp:

  Resampling kernel for `mode = "centroid"`: `"nearest"` (default, the
  containing pixel), `"bilinear"` (2x2), `"bicubic"` (Keys 4x4), or
  `"lanczos"` (Lanczos-3, 6x6). Kernel weights are renormalised over
  valid pixels, so nodata holes and raster edges shrink the stencil
  instead of propagating `NA`; a cell whose whole stencil is invalid
  returns `NA`. With `dequant`, each stencil pixel is decoded before the
  kernel is applied (nonlinear decodes do not commute with
  interpolation). Use `"nearest"` for categorical rasters; the smooth
  kernels blend class codes into meaningless intermediates. Only used
  when `mode = "centroid"`; an error otherwise.

- cpu_workers:

  Number of CPU threads for decoding and indexing (each decoded block is
  indexed as row stripes across all workers). `NULL` (default) resolves
  from `getOption("a5px.cpu_workers")`, env `A5PX_CPU_WORKERS`, then
  [`parallel::detectCores()`](https://rdrr.io/r/parallel/detectCores.html).

- io_concurrency:

  Maximum in-flight fetch tasks the producer issues to the I/O stage;
  each task carries one block, or a run of up to four adjacent blocks on
  files without a predictor. `NULL` (default) resolves from
  `getOption("a5px.io_concurrency")`, env `A5PX_IO_CONCURRENCY`, then
  `min(32, max(cpu_workers, 8))`. Bump this for cloud reads of
  multi-band embedding rasters where the network can absorb more
  parallelism than the CPU pool. See
  [`a5px_set_concurrency()`](https://belian-earth.github.io/a5px/reference/a5px_set_concurrency.md).

- scoff:

  Logical. If `TRUE`, apply the scale and offset the file declares for
  each band (`value * scale + offset`, from the GDAL band metadata
  reported by
  [`a5_raster_info()`](https://belian-earth.github.io/a5px/reference/a5_raster_info.md))
  per pixel before aggregation. A band that declares none takes the GDAL
  defaults, scale 1 and offset 0, so `scoff = TRUE` on a file without
  these tags returns raw values. Works for every data type. nodata is
  matched against the raw value, before scaling. The decode is linear,
  so it commutes with the mean and overviews stay in use (see
  `use_overviews`). Cannot be combined with `dequant` or with `majority`
  / `fractions`; to chain a further decode, fold the scale into the
  function: `dequant = function(x) f(x * s + o)`. Default `FALSE` reads
  raw values, unlike
  [`terra::rast()`](https://rspatial.github.io/terra/reference/rast.html),
  which applies scale and offset unless `raw = TRUE`.

- dequant:

  Optional per-pixel decode applied *before* aggregation: a vectorised R
  function mapping raw integer codes to decoded values, e.g.
  [dequant_aef](https://belian-earth.github.io/a5px/reference/dequant_aef.md)
  for Alpha Earth Foundations int8 embedding codes. Nonlinear decodes do
  not commute with aggregation, so quantized values must be decoded per
  pixel before the mean, not after; this argument does exactly that. The
  function is evaluated once over the full integer code domain to build
  a lookup table applied on the Rust side, which restricts `dequant` to
  sources with an integer data type of 16 bits or fewer (int8 / uint8 /
  int16 / uint16); other dtypes error at read time. nodata is matched
  against the raw code, before decoding. `NULL` (default) reads raw
  values.

- as_vector:

  Logical. If `TRUE`, collapse the (selected) bands into list columns of
  fixed-length numeric vectors – one per stat. Single stat returns a
  single column `value`; multi-stat returns `value_<stat>` columns (e.g.
  `value_mean`, `value_max`). Useful for embedding rasters. Default
  `FALSE` (one numeric column per band).

- use_overviews:

  Logical. When `TRUE` and `stat = "mean"`, read the coarsest COG
  overview that still oversamples the target A5 cell instead of the
  full-resolution image. For aggregations to cells much coarser than the
  source pixels this moves far fewer bytes and does far less per-pixel
  work for a near-identical mean. Ignored unless the stat is exactly
  `"mean"` (`sum`/`count`/`var`/`sd`/`min`/`max` are not preserved under
  decimation) and in `mode = "centroid"`. Set `FALSE` to always read
  full resolution. Requires the source to carry overviews; otherwise the
  full-resolution image is read regardless. The default is `TRUE` unless
  `dequant` is set: overviews are typically average-resampled, and an
  overview pixel that is a mean of quantized codes decodes incorrectly.
  Explicitly passing `TRUE` together with `dequant` warns and proceeds,
  for sources whose overviews were built with nearest or mode
  resampling.

- store_opts:

  Named character vector or named list of object store options for a
  remote `src`, applied after the environment defaults so an explicit
  option always wins. Keys are the `object_store` config keys for the
  provider, e.g. `aws_region`, `aws_skip_signature`,
  `aws_access_key_id`, `aws_secret_access_key`, `aws_session_token`,
  `aws_endpoint`, `aws_virtual_hosted_style_request`, the `google_*` /
  `azure_*` equivalents, and for plain HTTP(S) the client keys such as
  `timeout` or `allow_http`. Unknown keys, and any key given with a
  local path, are errors. Inspect the result with
  [`a5_store_config()`](https://belian-earth.github.io/a5px/reference/a5_store_config.md).

- separate:

  How several sources combine.

  - `FALSE` (default): the sources are pieces of one raster, such as
    adjacent tiles of a mosaic or captures of one area on different
    dates. They must have the same bands (count and names) and the same
    ground pixel size. Every valid pixel of every source enters its
    cell's statistics once, so cells on a seam between sources combine
    exactly. Where sources overlap, every capture counts: `mean`, `min`
    and `max` are over all observations, and `sum` and `count` include
    the overlap once per source, so `count` measures how many
    observations cover each cell. This differs from GDAL, where the last
    source wins.

  - `TRUE`: each source adds its bands, as `gdalbuildvrt -separate`.
    Sources need not share a grid, CRS or pixel size. A single-band
    source is named by its label and a multi-band source's bands are
    `<label>_<band>`; the label is the source's name in `src`, else its
    file name without extension. `bands` selects from this combined
    list.

  Single-band files without band descriptions all report the band name
  `band_01`, so `separate = FALSE` cannot tell red, green and blue files
  apart and pools them into one band; set `separate = TRUE` for files
  that hold different bands.

- composite:

  How several sources with the same bands combine. `NULL` (default)
  pools their pixels, as described under `separate`. Otherwise `stat` is
  computed for each source and cell, then reduced across the sources:
  `"mean"`, `"median"`, `"min"`, `"max"`, `"sum"`, `"count"`, `"var"`,
  `"sd"` or `"majority"`. Each source's value is weighted by its valid
  pixel count in the cell, so a source that covers only a sliver of a
  cell (at a scene edge, or around masked cloud) carries little weight;
  `"min"`, `"max"`, `"sum"` and `"count"` are unweighted. `"count"`
  gives the number of sources with valid data in the cell, such as the
  number of cloud-free captures. With `stat = "mean"`,
  `composite = "mean"` equals the pooled mean. Cells are the shared
  grid, so the sources need not be aligned. In centroid mode each
  source's sample has weight 1. Not available with `separate = TRUE`,
  `"fractions"` or `"npix"`.

## Value

A
[`tibble::tibble()`](https://tibble.tidyverse.org/reference/tibble.html)
with columns:

- `cell`: an
  [a5R::a5_cell](https://belian-earth.github.io/a5R/reference/a5_cell.html)
  vector at `resolution`

- one numeric column per band (named after the band's GDAL `DESCRIPTION`
  tag if present, else `band_01`, `band_02`, ...)

- or, if `as_vector = TRUE`, a single list column `value` of length-N
  numeric vectors (N = number of bands)

## Details

The reader runs in Rust on top of `async-tiff` and `object_store`, never
loading the full raster into memory. CRS reprojection uses pure-Rust
`proj4rs`. Pixel sampling is forward (pixel-driven): each pixel
contributes its value to exactly one A5 cell, determined by the lon/lat
of its centre.

Three sampling modes are available:

Multi-stat output naming: with `stat = c("mean", "max")` the wide
(per-band) tibble columns are `<band>_mean`, `<band>_max`. Single
underscore separator. Same convention applies in
[`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
and
[`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md)
when `as_vector = FALSE`.

- `"forward"` (default): for each pixel, push its value into the cell
  containing its centroid. Good when cells are larger than (or
  comparable to) the pixels and you want a real aggregation. Equivalent
  to GDAL's `gdal raster zonal-stats --pixels=default`.

- `"overlay"`: each pixel contributes to every cell it overlaps,
  weighted by the overlapped fraction of its area (approximated by
  sub-pixel supersampling; see `subsamples`). More accurate than
  `"forward"` at cell/pixel boundaries, at some extra cost for pixels
  that straddle a cell edge. Under overlay, `"mean"` is the
  area-weighted mean, `"sum"` is mass-preserving (a pixel's value is
  split across the cells covering it, so totals such as population
  counts are conserved), and `"count"` is the effective (fractional)
  number of contributing pixels. `"var"` / `"sd"` use frequency weights
  with divisor `sum(w) - 1`.

- `"centroid"`: for each cell intersecting the raster, sample the pixel
  that contains the cell's centroid. Single value per cell, no
  aggregation. Use this when the cell size is comparable to or smaller
  than the pixel size — the forward path leaves gaps in that regime
  because each pixel only contributes to one cell.

**Scaling:** GDAL `Scale` / `Offset` tags are *not* applied by default;
returned values are in the raster's native domain. Set `scoff = TRUE` to
apply them per pixel before aggregation.
[`a5_raster_info()`](https://belian-earth.github.io/a5px/reference/a5_raster_info.md)
reports the values a file declares.

**NoData:** the dataset-level GDAL `NODATA_VALUE` tag (when present) is
used to skip per-band samples. Per-band nodata via `GDAL_METADATA` XML
is not yet supported.

## Examples

``` r
if (FALSE) { # \dontrun{
  path <- system.file("extdata", "example.tif", package = "a5px")
  tbl <- a5_read_raster(path, resolution = 14)
  tbl
} # }
```
