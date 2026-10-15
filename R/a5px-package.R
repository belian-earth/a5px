#' @title a5px: Read Raster Data as A5 Cells via Pixel-Driven Aggregation
#'
#' @description
#' a5px aggregates raster pixels into [A5](https://a5geo.org/) cells. A5 is a
#' discrete global grid system that divides the globe into equal-area
#' pentagonal cells at 31 resolutions. a5px streams GeoTIFF and
#' Cloud-Optimised GeoTIFF files from local disk or cloud object stores and
#' computes per-cell statistics in a single pass, in Rust. Results are keyed
#' by [a5R][a5R::a5R-package] cells and come back as a tibble, an Arrow
#' table or a Parquet file.
#'
#' "px" reflects the direction of the default algorithm: pixels in, cells
#' out. Each pixel goes to the cell containing its centre, which suits cells
#' larger than pixels. For cells smaller than pixels, `mode = "centroid"`
#' samples the raster at each cell centre instead.
#'
#' @section Reading rasters:
#' - [a5_read_raster()]: a tibble keyed by `a5R::a5_cell`.
#' - [a5_read_raster_arrow()]: the same engine, returning an
#'   [arrow::Table] with a `FixedSizeList<float, n_bands>` value column.
#' - [a5_raster_to_parquet()]: reads and writes Parquet entirely in Rust,
#'   without the R-side Arrow round trip; best for large embedding rasters.
#'
#' `as_vector = FALSE` (default) returns one column per band;
#' `as_vector = TRUE` returns one fixed-length list column per stat.
#'
#' @section Sampling modes:
#' - `"forward"` (default): each pixel goes wholly to the cell containing
#'   its centre.
#' - `"overlay"`: each pixel is shared between the cells it overlaps, in
#'   proportion to area, so `sum` conserves totals.
#' - `"centroid"`: one value per cell, sampled at its centre with the
#'   `interp` kernel; for cells smaller than pixels.
#'
#' @section Several files:
#' `src` takes several files, read in one pass with a shared fetch queue
#' and worker pool. `separate = TRUE` makes each file's bands columns, for
#' bands stored one per file. Otherwise the files are pieces of one raster
#' with the same bands and pixel size: every valid pixel enters its cell
#' once, so cells on a seam combine exactly, and where files overlap every
#' capture counts. `composite` instead computes `stat` per file and reduces
#' across files per cell, weighted by each file's valid pixel count.
#'
#' @section Aggregating to coarser cells:
#' [a5_aggregate()] summarises an existing `a5_cell`-keyed result at a
#' coarser resolution without re-reading the raster. Each cell goes to the
#' coarse cell that contains its centre ([a5R::a5_cell_to_supercell()]).
#' It handles both the wide and the list-column layout, with the same
#' `stat` vocabulary as the readers.
#'
#' @section Writing:
#' [a5_write_parquet()] writes a tibble or Arrow table from the readers to
#' Parquet, with a schema for A5 cells and value lists.
#'
#' @section Inspecting sources:
#' - [a5_raster_info()]: dimensions, data type, block grid, overview levels,
#'   CRS, band scale and offset, and the WGS 84 envelope, without reading
#'   pixels.
#' - [a5_store_config()]: the object store configuration a remote `src`
#'   resolves to.
#'
#' @section Areas of interest:
#' `aoi` restricts a read to the A5 cells selected from a polygon by
#' [a5R::a5_polygon_to_cells()]: `containment = "centre"` keeps cells whose
#' centre is inside, `"overlapping"` every cell the polygon touches.
#' Selection is by cell: an included cell gets the statistics of all its
#' valid pixels, including any outside the polygon. Tools such as
#' exactextract instead clip pixels to the polygon; for that, aggregate the
#' cells afterwards with coverage fractions of your choice.
#'
#' @section Chunked reads:
#' The accumulators hold every touched cell in memory (about 4.5 GB per
#' million cells with 64 bands and `stat = c("mean", "count")`), so split
#' very large reads by `bbox`. With `bbox_align = "block"` each chunk takes
#' whole COG blocks, so no block is fetched twice; with
#' `stat = c("sum", "count")` the per-cell partials of adjacent chunks add
#' up exactly, and means follow. [a5_raster_info()] reports the block grid
#' and envelope to build the chunks from.
#'
#' @section Configuration:
#' [a5px_set_concurrency()] and [a5px_get_concurrency()] set the session
#' defaults for the worker pool (`cpu_workers`) and the number of tile
#' fetches in flight (`io_concurrency`). They are independent of
#' [a5R::a5_set_threads()]. Remote reads are usually limited by latency,
#' so a higher `io_concurrency` often helps.
#'
#' @section Common arguments:
#' All readers accept the same core arguments:
#' - `src`: one or more paths or URLs: local, `file://`, `http(s)://`,
#'   `s3://`, `gs://` or `az://`. Remote reads fetch byte ranges and never
#'   download the whole file; see "Remote sources".
#' - `resolution`: A5 resolution, 0 to 30; [a5R::a5_cell_area()] gives the
#'   cell area at each.
#' - `stat`: one or more of `"mean"`, `"sum"`, `"count"`, `"min"`, `"max"`,
#'   `"var"`, `"sd"`, `"majority"`, `"fractions"` and `"npix"`. Several give
#'   one column per band and stat. `var` and `sd` use the sample formula
#'   (divisor n - 1), so a cell with one pixel returns `NA`.
#' - `bands`: `NULL` (all), 1-based indices, or names matched against the
#'   GDAL `DESCRIPTION` band tag. For band-interleaved files only the
#'   selected bands are fetched.
#' - `scoff`, `dequant`: decode stored values per pixel before aggregation.
#' - `cpu_workers`, `io_concurrency`: see [a5px_set_concurrency()].
#'
#' @section Remote sources:
#' `src` may be an `s3://`, `gs://`, `az://` or `http(s)://` URL; reads are
#' byte-range requests and the file is never downloaded whole. Cloud
#' clients are configured from the environment first and from the readers'
#' `store_opts` argument second, so an explicit option always wins.
#' [a5_store_config()] shows the resolved configuration without making a
#' request.
#'
#' - S3: every `AWS_*` variable `object_store` understands, including
#'   `AWS_REGION` / `AWS_DEFAULT_REGION`, `AWS_ACCESS_KEY_ID`,
#'   `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_ENDPOINT` /
#'   `AWS_ENDPOINT_URL_S3`, `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` and
#'   `AWS_WEB_IDENTITY_TOKEN_FILE`, plus GDAL's `AWS_NO_SIGN_REQUEST=YES`
#'   for public buckets (mapped to `aws_skip_signature`). Without static,
#'   container or web-identity credentials the client falls back to
#'   instance metadata, which off AWS waits for a long timeout before
#'   failing, so set `AWS_NO_SIGN_REQUEST` or `store_opts = c(aws_skip_signature =
#'   "true")` for public data. The shared credentials file
#'   (`~/.aws/credentials`) and `AWS_PROFILE` are not read. With no region
#'   the client assumes `us-east-1` and a bucket elsewhere fails, so set
#'   `AWS_REGION` or `aws_region`. Path-style
#'   `https://s3.<region>.amazonaws.com/<bucket>/...` and virtual-hosted
#'   `https://<bucket>.s3.<region>.amazonaws.com/...` URLs are treated as
#'   S3 rather than plain HTTP and take the same configuration.
#' - GCS: the `GOOGLE_*` variables (`GOOGLE_SERVICE_ACCOUNT`,
#'   `GOOGLE_SERVICE_ACCOUNT_KEY`, `GOOGLE_APPLICATION_CREDENTIALS`);
#'   `google_skip_signature` for public buckets.
#' - Azure: the `AZURE_*` variables (`AZURE_STORAGE_ACCOUNT_NAME`,
#'   `AZURE_STORAGE_ACCESS_KEY`, `AZURE_STORAGE_SAS_KEY`, ...);
#'   `azure_skip_signature` for public containers.
#' - Plain HTTP(S): no authentication. `store_opts` accepts client keys
#'   such as `timeout`, `connect_timeout` and `allow_http`.
#'
#' @section Supported formats and CRSs:
#' - Tiled GeoTIFF and COG with ZSTD, Deflate, LZW, JPEG, PackBits or no
#'   compression. Strip-based TIFFs are not yet supported.
#' - CRSs are resolved from an EPSG code, else WKT in a citation field
#'   (`proj4wkt`), else the GeoKey parameters, which covers custom
#'   projections written by GDAL without an EPSG code, such as a centred
#'   Lambert azimuthal equal-area. Reprojection uses pure-Rust `proj4rs`.
#' - NoData: the dataset-wide `TIFFTAG_GDAL_NODATA`, compared safely for
#'   NaN. GeoTIFF does not store per-band NoData; reading bands from
#'   separate files with `separate = TRUE` gives each its own.
#'
#' @section Performance:
#' On a 12-band, 26-million-pixel Sentinel-2 COG, a5px is about 7 times
#' faster end to end than `gdal raster zonal-stats` (including polygon
#' preparation) and 11 times faster than a `terra` and `dplyr` pipeline.
#'
#' @section Profiling:
#' Set `A5PX_PROFILE=1` to print stage timings (fetch, decode, projection,
#' cell indexing, accumulation, merge) summed across workers.
#'
#' @import a5R
#' @keywords internal
"_PACKAGE"
