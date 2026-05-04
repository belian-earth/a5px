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
#' @section Writing:
#' - [a5_write_parquet()] --- write a tibble or Arrow table to Parquet, with
#'   a schema tailored to A5 cell + value lists
#'
#' @section Configuration:
#' - [a5px_set_threads()] / [a5px_get_threads()] --- multi-threading control
#'   (independent of [a5R::a5_set_threads()])
#'
#' @section Common arguments:
#' All readers accept the same core arguments:
#' - `src` --- path or URL. Schemes: local path, `file://`, `http(s)://`,
#'   `s3://`, `gs://`, `az://`. Cloud reads stream byte ranges; the full file
#'   is never materialised.
#' - `resolution` --- A5 cell resolution (0--30); see [a5R::a5_cell_area()].
#' - `stat` --- one or more of `"mean"`, `"sum"`, `"count"`, `"min"`, `"max"`.
#'   A character vector emits one column per (band, stat) pair.
#' - `bands` --- `NULL` (all), integer vector (1-based), or character vector
#'   matched against the GDAL `DESCRIPTION` tag. For planar-layout TIFFs the
#'   reader fetches only the byte ranges of the selected bands.
#' - `threads`, `io_concurrency` --- tile-level concurrency knobs.
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
#' @keywords internal
"_PACKAGE"
