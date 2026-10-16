# a5px: Read Raster Data as A5 Cells via Pixel-Driven Aggregation

a5px aggregates raster pixels into [A5](https://a5geo.org/) cells. A5 is
a discrete global grid system that divides the globe into equal-area
pentagonal cells at 31 resolutions. a5px streams GeoTIFF and
Cloud-Optimised GeoTIFF files from local disk or cloud object stores and
computes per-cell statistics in a single pass, in Rust. Results are
keyed by
[a5R](https://belian-earth.github.io/a5R/reference/a5R-package.html)
cells and come back as a tibble, an Arrow table or a Parquet file.

"px" reflects the direction of the default algorithm: pixels in, cells
out. Each pixel goes to the cell containing its centre, which suits
cells larger than pixels. For cells smaller than pixels,
`mode = "centroid"` samples the raster at each cell centre instead.

## Reading rasters

- [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md):
  a tibble keyed by
  [`a5R::a5_cell`](https://belian-earth.github.io/a5R/reference/a5_cell.html).

- [`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md):
  the same engine, returning an
  [arrow::Table](https://arrow.apache.org/docs/r/reference/Table-class.html)
  with a `FixedSizeList<float, n_bands>` value column.

- [`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md):
  reads and writes Parquet entirely in Rust, without the R-side Arrow
  round trip; best for large embedding rasters.

`as_vector = FALSE` (default) returns one column per band;
`as_vector = TRUE` returns one fixed-length list column per stat.

## Sampling modes

- `"forward"` (default): each pixel goes wholly to the cell containing
  its centre.

- `"overlay"`: each pixel is shared between the cells it overlaps, in
  proportion to area, so `sum` conserves totals.

- `"centroid"`: one value per cell, sampled at its centre with the
  `interp` kernel; for cells smaller than pixels.

## Several files

`src` takes several files, read in one pass with a shared fetch queue
and worker pool. `separate = TRUE` makes each file's bands columns, for
bands stored one per file. Otherwise the files are pieces of one raster
with the same bands and pixel size: every valid pixel enters its cell
once, so cells on a seam combine exactly, and where files overlap every
capture counts. `composite` instead computes `stat` per file and reduces
across files per cell, weighted by each file's valid pixel count.

## Aggregating to coarser cells

[`a5_aggregate()`](https://belian-earth.github.io/a5px/reference/a5_aggregate.md)
summarises an existing `a5_cell`-keyed result at a coarser resolution
without re-reading the raster. Each cell goes to the coarse cell that
contains its centre
([`a5R::a5_cell_to_supercell()`](https://belian-earth.github.io/a5R/reference/a5_spatial_hierarchy.html)).
It handles both the wide and the list-column layout, with the same
`stat` vocabulary as the readers.

## Writing

[`a5px_write_parquet()`](https://belian-earth.github.io/a5px/reference/a5px_write_parquet.md)
writes a tibble or Arrow table from the readers to Parquet, with a
schema for A5 cells and value lists.

## Inspecting sources

- [`a5_raster_info()`](https://belian-earth.github.io/a5px/reference/a5_raster_info.md):
  dimensions, data type, block grid, overview levels, CRS, band scale
  and offset, and the WGS 84 envelope, without reading pixels.

- [`a5px_store_config()`](https://belian-earth.github.io/a5px/reference/a5px_store_config.md):
  the object store configuration a remote `src` resolves to.

## Areas of interest

`aoi` restricts a read to the A5 cells selected from a polygon by
[`a5R::a5_polygon_to_cells()`](https://belian-earth.github.io/a5R/reference/a5_polygon_to_cells.html):
`containment = "centre"` keeps cells whose centre is inside,
`"overlapping"` every cell the polygon touches. Selection is by cell: an
included cell gets the statistics of all its valid pixels, including any
outside the polygon. Tools such as exactextract instead clip pixels to
the polygon; for that, aggregate the cells afterwards with coverage
fractions of your choice.

## Chunked reads

The accumulators hold every touched cell in memory (about 4.5 GB per
million cells with 64 bands and `stat = c("mean", "count")`), so split
very large reads by `bbox`. A pixel centred on an edge two chunks share
belongs to one of them, so with `stat = c("sum", "count")` the per-cell
partials of adjacent chunks add up exactly, and means follow. A remote
block that several chunks touch is fetched once and then served from the
session cache
([`a5px_set_cache()`](https://belian-earth.github.io/a5px/reference/a5px_set_cache.md)),
as long as the cache holds it. With `bbox_align = "block"` each chunk
takes whole COG blocks instead, so no chunk reads a block another chunk
owns.
[`a5_raster_info()`](https://belian-earth.github.io/a5px/reference/a5_raster_info.md)
reports the block grid and envelope to build the chunks from.

## Configuration

[`a5px_concurrency()`](https://belian-earth.github.io/a5px/reference/a5px_concurrency.md)
sets and reports the session defaults for the worker pool
(`cpu_workers`) and the number of remote requests in flight
(`io_concurrency`). They are independent of
[`a5R::a5_set_threads()`](https://belian-earth.github.io/a5R/reference/a5_threads.html).
Remote reads are usually limited by latency, so a higher
`io_concurrency` often helps. Remote reads keep the byte ranges they
fetch in a session cache, so reads split into `bbox` chunks fetch each
block once;
[`a5px_set_cache()`](https://belian-earth.github.io/a5px/reference/a5px_set_cache.md)
sets its budget.

## Common arguments

All readers accept the same core arguments:

- `src`: one or more paths or URLs: local, `file://`, `http(s)://`,
  `s3://`, `gs://` or `az://`. Remote reads fetch byte ranges and never
  download the whole file; see "Remote sources".

- `resolution`: A5 resolution, 0 to 29 (30 is not yet supported);
  [`a5R::a5_cell_area()`](https://belian-earth.github.io/a5R/reference/a5_resolution_stats.html)
  gives the cell area at each.

- `stat`: one or more of `"mean"`, `"sum"`, `"count"`, `"min"`, `"max"`,
  `"var"`, `"sd"`, `"majority"`, `"fractions"` and `"npix"`. Several
  give one column per band and stat. `var` and `sd` use the sample
  formula (divisor n - 1), so a cell with one pixel returns `NA`.

- `bands`: `NULL` (all), 1-based indices, or names matched against the
  GDAL `DESCRIPTION` band tag. For band-interleaved files only the
  selected bands are fetched.

- `scoff`, `dequant`: decode stored values per pixel before aggregation.

- `cpu_workers`, `io_concurrency`: see
  [`a5px_concurrency()`](https://belian-earth.github.io/a5px/reference/a5px_concurrency.md).

## Remote sources

`src` may be an `s3://`, `gs://`, `az://` or `http(s)://` URL; reads are
byte-range requests and the file is never downloaded whole. Cloud
clients are configured from the environment first and from the readers'
`store_opts` argument second, so an explicit option always wins.
[`a5px_store_config()`](https://belian-earth.github.io/a5px/reference/a5px_store_config.md)
shows the resolved configuration without making a request.

- S3: every `AWS_*` variable `object_store` understands, including
  `AWS_REGION` / `AWS_DEFAULT_REGION`, `AWS_ACCESS_KEY_ID`,
  `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_ENDPOINT` /
  `AWS_ENDPOINT_URL_S3`, `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` and
  `AWS_WEB_IDENTITY_TOKEN_FILE`, plus GDAL's `AWS_NO_SIGN_REQUEST=YES`
  for public buckets (mapped to `aws_skip_signature`). Without static,
  container or web-identity credentials the client takes them from the
  instance metadata service (on EC2, ECS or Batch); where none answers
  within 300 ms and signing was not set either way, requests go
  unsigned, which reads public data and gets "access denied" from a
  private bucket. The shared credentials file (`~/.aws/credentials`) and
  `AWS_PROFILE` are not read. With no region the client assumes
  `us-east-1` and a bucket elsewhere fails, so set `AWS_REGION` or
  `aws_region`. Path-style
  `https://s3.<region>.amazonaws.com/<bucket>/...` and virtual-hosted
  `https://<bucket>.s3.<region>.amazonaws.com/...` URLs are treated as
  S3 rather than plain HTTP and take the same configuration, so stale
  credentials in the environment can be refused where an unsigned
  request would succeed; set `aws_skip_signature` for public data. An
  `http://` endpoint (`AWS_ENDPOINT`, as for MinIO) is allowed without
  `allow_http`.

- GCS: the `GOOGLE_*` variables (`GOOGLE_SERVICE_ACCOUNT`,
  `GOOGLE_SERVICE_ACCOUNT_KEY`, `GOOGLE_APPLICATION_CREDENTIALS`);
  `google_skip_signature` for public buckets.

- Azure: the `AZURE_*` variables (`AZURE_STORAGE_ACCOUNT_NAME`,
  `AZURE_STORAGE_ACCESS_KEY`, `AZURE_STORAGE_SAS_KEY`, ...);
  `azure_skip_signature` for public containers.

- Plain HTTP(S): no authentication. `store_opts` accepts client keys
  such as `timeout` and `connect_timeout`.

- GDAL paths `/vsis3/bucket/key`, `/vsigs/bucket/key`,
  `/vsiaz/container/key` and `/vsicurl/<url>` are read as the matching
  `s3://`, `gs://`, `az://` or plain URL. Other `/vsi` paths, such as
  `/vsizip/`, are not supported.

## Supported formats and CRSs

- Tiled GeoTIFF and COG with ZSTD, Deflate, LZW, JPEG, PackBits or no
  compression. Strip-based TIFFs are not yet supported.

- CRSs are resolved from an EPSG code, else WKT in a citation field
  (`proj4wkt`), else the GeoKey parameters, which covers custom
  projections written by GDAL without an EPSG code, such as a centred
  Lambert azimuthal equal-area. Reprojection uses pure-Rust `proj4rs`.

- NoData: the dataset-wide `TIFFTAG_GDAL_NODATA`, compared safely for
  NaN. GeoTIFF does not store per-band NoData; reading bands from
  separate files with `separate = TRUE` gives each its own.

## Performance

On a 12-band, 26-million-pixel Sentinel-2 COG, a5px is about 7 times
faster end to end than `gdal raster zonal-stats` (including polygon
preparation) and 11 times faster than a `terra` and `dplyr` pipeline.

## Profiling

Set `A5PX_PROFILE=1` to print stage timings (fetch, decode, projection,
cell indexing, accumulation, merge) summed across workers.

## See also

Useful links:

- <https://github.com/belian-earth/a5px>

- <https://belian-earth.github.io/a5px/>

- Report bugs at <https://github.com/belian-earth/a5px/issues>

## Author

**Maintainer**: Hugh Graham <hugh@belian.earth>

Authors:

- Hugh Graham <hugh@belian.earth>

Other contributors:

- belian.earth \[copyright holder\]
