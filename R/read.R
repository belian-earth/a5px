#' Read a (Cloud-Optimised) GeoTIFF as A5-cell aggregated values
#'
#' Streams a GeoTIFF or COG, projects each pixel centre to WGS 84, indexes it
#' to an A5 cell at the requested resolution, and aggregates per-band values
#' into a tibble keyed by cell.
#'
#' The reader runs in Rust on top of `async-tiff` and `object_store`, never
#' loading the full raster into memory. CRS reprojection uses pure-Rust
#' `proj4rs`. Pixel sampling is forward (pixel-driven): each pixel contributes
#' its value to exactly one A5 cell, determined by the lon/lat of its centre.
#'
#' Forward sampling assumes the pixel size is smaller than (or comparable to)
#' the target cell size. When the raster pixel area is much larger than the
#' cell area, expect sparse cells; a future cell-driven mode will handle that
#' case.
#'
#' @param src Path or URL to a GeoTIFF / COG. Supported schemes: local path
#'   (no scheme), `file://`, `http(s)://`, `s3://`, `gs://`, `az://`.
#' @param resolution Integer scalar A5 resolution (0--30).
#' @param stat Aggregation. One of `"mean"`, `"sum"`, `"count"`, `"min"`,
#'   `"max"`. Default `"mean"`.
#' @param threads Tokio worker threads (also caps tile-level concurrency).
#'   Default 1.
#' @param io_concurrency Number of tiles fetched concurrently. Default 8.
#' @param as_vector Logical. If `TRUE`, collapse all bands into a single list
#'   column `value` of fixed-length numeric vectors (one entry per band, in
#'   band order). Useful for embedding rasters. Default `FALSE` (one numeric
#'   column per band).
#'
#' @returns A [tibble::tibble()] with columns:
#'   - `cell`: an [a5R::a5_cell] vector at `resolution`
#'   - one numeric column per band (named after the band's GDAL `DESCRIPTION`
#'     tag if present, else `band_01`, `band_02`, ...)
#'   - or, if `as_vector = TRUE`, a single list column `value` of length-N
#'     numeric vectors (N = number of bands)
#'
#' @details
#' **Scaling:** GDAL `Scale` / `Offset` tags are *not* applied. Returned
#' values are in the raster's native domain. Apply scale/offset on the R side
#' if needed.
#'
#' **NoData:** the dataset-level GDAL `NODATA_VALUE` tag (when present) is
#' used to skip per-band samples. Per-band nodata via `GDAL_METADATA` XML is
#' not yet supported.
#'
#' @export
#' @examples
#' \dontrun{
#'   path <- system.file("extdata", "example.tif", package = "a5px")
#'   tbl <- a5_read_raster(path, resolution = 14)
#'   tbl
#' }
a5_read_raster <- function(src,
                           resolution,
                           stat = c("mean", "sum", "count", "min", "max"),
                           threads = 1L,
                           io_concurrency = 8L,
                           as_vector = FALSE) {
  check_scalar_string(src, "src")
  resolution <- vctrs::vec_cast(resolution, integer(), x_arg = "resolution")
  vctrs::vec_assert(resolution, size = 1L)
  check_resolution(resolution)
  stat <- rlang::arg_match(stat)
  threads <- check_scalar_count(threads, "threads")
  io_concurrency <- check_scalar_count(io_concurrency, "io_concurrency")
  if (!is.logical(as_vector) || length(as_vector) != 1L || is.na(as_vector)) {
    cli::cli_abort("{.arg as_vector} must be a length-1 non-NA logical.")
  }

  out <- a5_read_raster_rs(
    src = src,
    resolution = resolution,
    stat = stat,
    threads = threads,
    io_concurrency = io_concurrency
  )

  cells <- new_a5_cell_from_rs(out$cell)
  bands <- out$bands
  band_names <- as.character(out$band_names)
  names(bands) <- band_names

  if (as_vector) {
    n <- length(cells)
    n_bands <- length(bands)
    mat <- vapply(bands, identity, numeric(n))
    if (n_bands == 1L) {
      dim(mat) <- c(n, 1L)
    }
    value <- lapply(seq_len(n), function(i) as.numeric(mat[i, ]))
    tibble::tibble(cell = cells, value = value)
  } else {
    tibble::tibble(cell = cells, !!!bands)
  }
}
