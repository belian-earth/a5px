#' @title a5px: Read Raster Data as A5 Cells via Pixel-Driven Aggregation
#'
#' @description
#' Rust-backed reader that streams (Cloud-Optimised) GeoTIFF rasters from
#' local files or cloud object stores and aggregates pixels into [A5
#' pentagonal DGGS](https://a5geo.org/) cells in one pass. Produced output is
#' interoperable with [a5R][a5R::a5R-package] cell vectors and can be returned
#' as a tibble, an Arrow table, or written straight to Parquet from Rust.
#'
#' "px" reflects the input domain: pixels in, cells out. The forward
#' (pixel-driven) algorithm is appropriate when the raster pixel size is
#' similar to or smaller than the target A5 cell area; for the inverse case
#' use a vector / cell-driven workflow (not yet implemented in a5px).
#'
#' @section Reading rasters:
#' - [a5_read_raster()] --- aggregate to a tibble keyed by `a5R::a5_cell`
#' - [a5_read_raster_arrow()] --- same engine, returns an [arrow::Table] with
#'   a `FixedSizeList<float, n_bands>` value column suitable for direct
#'   Parquet writes
#' - [a5_raster_to_parquet()] --- read and write Parquet entirely in Rust,
#'   bypassing the R-side Arrow round-trip (preferred for large embedding
#'   rasters)
#'
#' All three readers share the same `as_vector` switch: `FALSE` (default)
#' returns one column per band, `TRUE` returns a single fixed-length list /
#' `FixedSizeList` value column.
#'
#' @section Aggregating to coarser cells:
#' - [a5_aggregate()] --- pure-R aggregator that lifts an existing
#'   `a5_cell`-keyed tibble to a coarser resolution via the A5-native
#'   centroid hierarchy ([a5R::a5_cell_to_parent()]). Avoids re-reading the
#'   source raster when you already have a high-resolution result and want
#'   it summarised. Handles both wide (one column per band) and list-column
#'   layouts; list columns are reduced element-wise. Same `stat` vocabulary
#'   as the readers.
#'
#' @section Writing:
#' - [a5_write_parquet()] --- write a tibble or Arrow table to Parquet, with
#'   a schema tailored to A5 cell + value lists
#'
#' @section Inspecting sources:
#' - [a5_raster_info()] --- dimensions, data type, block grid, overview
#'   levels, CRS and WGS 84 envelope without reading pixels
#' - [a5_store_config()] --- the resolved object store configuration for a
#'   remote `src`
#'
#' @section Areas of interest:
#' `aoi` restricts a read to the A5 cells selected from a polygon by
#' [a5R::a5_polygon_to_cells()], with `containment = "centre"` (cells whose
#' centre is inside) or `"overlapping"` (every cell the polygon touches).
#' Selection is at the cell level: an included cell gets the statistics of
#' all its valid pixels, including any outside the polygon. This is the
#' DGGS-native reading of zonal extraction, distinct from tools such as
#' exactextract that clip pixels to the polygon; if you need clipped-pixel
#' semantics, aggregate the cells afterwards with the polygon coverage
#' fractions of your choice.
#'
#' @section Chunked reads:
#' Accumulators hold every touched cell in memory (roughly 4.5 GB per
#' million cells with 64 bands and `stat = c("mean", "count")`), so very
#' large reads are chunked by `bbox`. Pass `bbox_align = "block"` so each
#' chunk takes whole COG blocks and no block is fetched twice; read
#' `stat = c("sum", "count")` so per-cell partials from adjacent chunks add
#' exactly, and derive means afterwards. [a5_raster_info()] reports the
#' block grid and envelope to build the chunk grid from.
#'
#' @section Configuration:
#' - [a5px_set_concurrency()] / [a5px_get_concurrency()] --- two-knob control
#'   over the CPU consumer pool (`cpu_workers`) and the maximum in-flight
#'   tile fetches (`io_concurrency`). Independent of [a5R::a5_set_threads()].
#'
#' @section Common arguments:
#' All readers accept the same core arguments:
#' - `src` --- path or URL. Schemes: local path, `file://`, `http(s)://`,
#'   `s3://`, `gs://`, `az://`. Cloud reads stream byte ranges; the full file
#'   is never materialised. See "Remote sources" for client configuration
#'   and the `store_opts` argument.
#' - `resolution` --- A5 cell resolution (0--30); see [a5R::a5_cell_area()].
#' - `stat` --- one or more of `"mean"`, `"sum"`, `"count"`, `"min"`, `"max"`,
#'   `"var"`, `"sd"`. A character vector emits one column per (band, stat)
#'   pair. `var` / `sd` use Welford's online algorithm and the sample formula
#'   (divisor n - 1); cells with a single pixel return `NA`.
#' - `bands` --- `NULL` (all), integer vector (1-based), or character vector
#'   matched against the GDAL `DESCRIPTION` tag. For planar-layout TIFFs the
#'   reader fetches only the byte ranges of the selected bands.
#' - `cpu_workers`, `io_concurrency` --- tile-level concurrency knobs; see
#'   [a5px_set_concurrency()].
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
#'   instance metadata, which off-AWS costs a long timeout before failing,
#'   so set `AWS_NO_SIGN_REQUEST` or `store_opts = c(aws_skip_signature =
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
#' @section Supported formats and CRSes:
#' - Tiled GeoTIFF / Cloud-Optimised GeoTIFF (`async-tiff` 0.3, supports
#'   ZSTD, Deflate, LZW, JPEG and packbits/none). Strip-based TIFFs are not
#'   yet supported.
#' - CRS resolution tries, in order: EPSG code, WKT in a citation field
#'   (`proj4wkt`), and explicit GeoKey reconstruction (`+proj=laea`-style
#'   custom projections written by GDAL with no EPSG number). Reprojection
#'   uses pure-Rust `proj4rs`.
#' - NoData: dataset-wide `TIFFTAG_GDAL_NODATA` is honoured with NaN-safe
#'   comparison. Per-band nodata in GeoTIFF is a spec limitation; VRT
#'   support would unlock it.
#'
#' @section Performance:
#' Pixel-major streaming with an A5-cell containment cache cuts
#' `a5::lonlat_to_cell` overhead ~3x for dense scans. On a 12-band 26 M-pixel
#' Sentinel-2 COG the package is roughly 7x faster than `gdal raster
#' zonal-stats` (CLI plus polygon prep) and 11x faster than a hand-rolled
#' `terra` + `dplyr` pipeline.
#'
#' @section Profiling:
#' Set `A5PX_PROFILE=1` to print stage and sub-stage timings (io fetch,
#' decode, build points, proj transform, a5 indexing, hashmap lookup,
#' accumulator push, merge) summed across tile workers.
#'
#' @import a5R
#' @keywords internal
"_PACKAGE"
