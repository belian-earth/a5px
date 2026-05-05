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
#' Two sampling modes are available:
#'
#' Multi-stat output naming: with `stat = c("mean", "max")` the wide
#' (per-band) tibble columns are `<band>_mean`, `<band>_max`. Single
#' underscore separator. Same convention applies in
#' [a5_read_raster_arrow()] and [a5_raster_to_parquet()] when
#' `as_vector = FALSE`.
#'
#' - `"forward"` (default): for each pixel, push its value into the cell
#'   containing its centroid. Good when cells are larger than (or
#'   comparable to) the pixels and you want a real aggregation. Equivalent
#'   to GDAL's `gdal raster zonal-stats --pixels=default`.
#' - `"centroid"`: for each cell intersecting the raster, sample the pixel
#'   that contains the cell's centroid. Single value per cell, no
#'   aggregation. Use this when the cell size is comparable to or smaller
#'   than the pixel size — the forward path leaves gaps in that regime
#'   because each pixel only contributes to one cell.
#'
#' @param src Path or URL to a GeoTIFF / COG. Supported schemes: local path
#'   (no scheme), `file://`, `http(s)://`, `s3://`, `gs://`, `az://`.
#' @param resolution Integer scalar A5 resolution (0--30).
#' @param stat Aggregation. One of `"mean"`, `"sum"`, `"count"`, `"min"`,
#'   `"max"`, `"var"`, `"sd"`, or any non-duplicated subset of those for a
#'   one-pass multi-stat read. Default `"mean"`. `"var"` / `"sd"` use the
#'   sample formula (divisor n - 1) computed via Welford's online algorithm
#'   in the streaming aggregator, matching [stats::var()] / [stats::sd()];
#'   cells covered by a single pixel return `NA`.
#' @param bands Bands to read. One of:
#'   - `NULL` (default): read every band.
#'   - integer / numeric vector: 1-based band indices to read.
#'   - character vector: band names matched against the GDAL `DESCRIPTION`
#'     tag (falling back to `band_NN` when descriptions are absent).
#' @param bbox Optional spatial subset, as a numeric `c(xmin, ymin, xmax,
#'   ymax)` in WGS 84 lon/lat. Same convention as [a5R::a5_grid()]. When
#'   supplied, only tiles overlapping the bbox (in raster CRS) are fetched
#'   from the COG, and pixels outside the bbox are skipped. `NULL` (default)
#'   reads the whole raster.
#' @param src_nodata Optional numeric scalar overriding the source nodata.
#'   Use this when the file's `TIFFTAG_GDAL_NODATA` tag is missing or wrong;
#'   it takes precedence over the metadata value when set. `NULL` (default)
#'   uses whatever `async-tiff` exposes.
#' @param mode Sampling mode. `"forward"` (default) aggregates pixels into
#'   cells; `"centroid"` samples one pixel per cell at the cell centroid.
#'   See Details.
#' @param cpu_workers Number of CPU consumers in the tile-processing pool.
#'   `NULL` (default) resolves from `getOption("a5px.cpu_workers")`, env
#'   `A5PX_CPU_WORKERS`, then [parallel::detectCores()].
#' @param io_concurrency Maximum in-flight tile fetches the producer issues
#'   to the I/O stage. `NULL` (default) resolves from
#'   `getOption("a5px.io_concurrency")`, env `A5PX_IO_CONCURRENCY`, then
#'   `min(32, max(cpu_workers, 8))`. Bump this for cloud reads of multi-band
#'   embedding rasters where the network can absorb more parallelism than
#'   the CPU pool. See [a5px_set_concurrency()].
#' @param as_vector Logical. If `TRUE`, collapse the (selected) bands into
#'   list columns of fixed-length numeric vectors -- one per stat. Single
#'   stat returns a single column `value`; multi-stat returns
#'   `value_<stat>` columns (e.g. `value_mean`, `value_max`). Useful for
#'   embedding rasters. Default `FALSE` (one numeric column per band).
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
                           stat = "mean",
                           bands = NULL,
                           bbox = NULL,
                           src_nodata = NULL,
                           mode = c("forward", "centroid"),
                           cpu_workers = NULL,
                           io_concurrency = NULL,
                           as_vector = FALSE) {
  check_scalar_string(src, "src")
  resolution <- vctrs::vec_cast(resolution, integer(), x_arg = "resolution")
  vctrs::vec_assert(resolution, size = 1L)
  check_resolution(resolution)
  stats <- check_stats(stat)
  mode <- rlang::arg_match(mode)
  cpu_workers <- if (is.null(cpu_workers)) resolve_cpu_workers()
                 else check_scalar_count(cpu_workers, "cpu_workers")
  io_concurrency <- if (is.null(io_concurrency)) resolve_io_concurrency(cpu_workers)
                    else check_scalar_count(io_concurrency, "io_concurrency")
  if (!is.logical(as_vector) || length(as_vector) != 1L || is.na(as_vector)) {
    cli::cli_abort("{.arg as_vector} must be a length-1 non-NA logical.")
  }
  band_sel <- parse_bands_arg(bands)
  bbox_v <- check_bbox(bbox)
  src_nodata_v <- check_src_nodata(src_nodata)

  if (mode == "centroid") {
    return(read_raster_centroid(
      src = src,
      resolution = resolution,
      bands_idx = band_sel$idx,
      bands_names = band_sel$names,
      bbox = if (length(bbox_v)) bbox_v else NULL,
      src_nodata = src_nodata_v,
      cpu_workers = cpu_workers,
      io_concurrency = io_concurrency,
      as_vector = as_vector,
      stats = stats
    ))
  }

  out <- a5_read_raster_rs(
    src = src,
    resolution = resolution,
    stats = stats,
    bands_idx = band_sel$idx,
    bands_names = band_sel$names,
    bbox = bbox_v,
    src_nodata = src_nodata_v,
    cpu_workers = cpu_workers,
    io_concurrency = io_concurrency
  )

  cells <- new_a5_cell_from_rs(out$cell)
  bands <- out$bands
  # Rust returns names via the named list itself, but be explicit
  if (length(stats) == 1L) {
    names(bands) <- as.character(out$band_names)
  } else {
    band_names <- as.character(out$band_names)
    # Rust iterates stat-major (outer = stat, inner = band), producing
    # [B0_s0, B1_s0, ..., B0_s1, B1_s1, ...]. outer(bands, stats) flattens
    # column-major to that exact order.
    names(bands) <- as.vector(outer(band_names, stats, paste, sep = "_"))
  }

  if (as_vector) {
    n <- length(cells)
    band_names <- as.character(out$band_names)
    n_bands <- length(band_names)
    cols <- list(cell = cells)
    for (s in stats) {
      keys <- if (length(stats) == 1L) band_names else paste(band_names, s, sep = "_")
      mat <- vapply(bands[keys], identity, numeric(n))
      if (n_bands == 1L) dim(mat) <- c(n, 1L)
      val <- lapply(seq_len(n), function(i) as.numeric(mat[i, ]))
      col_name <- if (length(stats) == 1L) "value" else paste0("value_", s)
      cols[[col_name]] <- val
    }
    tibble::tibble(!!!cols)
  } else {
    tibble::tibble(cell = cells, !!!bands)
  }
}

#' Centroid-mode read: enumerate cells covering the bbox via [a5R::a5_grid()],
#' then sample one pixel per cell.
#' @noRd
read_raster_centroid <- function(src, resolution, bands_idx, bands_names,
                                 bbox, src_nodata, cpu_workers,
                                 io_concurrency, as_vector, stats) {
  if (length(stats) > 1L || stats[1] != "mean") {
    cli::cli_warn(
      "{.code mode = \"centroid\"} returns one sample per cell; the {.arg stat} arg is ignored.",
      .frequency = "regularly", .frequency_id = "a5px-centroid-stat"
    )
  }
  if (is.null(bbox)) {
    bbox <- as.numeric(a5_raster_bbox_lonlat_rs(src))
  }
  cells <- a5R::a5_grid(bbox, resolution = resolution)
  if (length(cells) == 0L) {
    cli::cli_abort("a5_grid returned 0 cells for the requested bbox at resolution {resolution}.")
  }
  out <- a5_sample_at_cells_rs(
    src = src,
    cells_raw = vctrs::vec_data(cells),
    bands_idx = bands_idx,
    bands_names = bands_names,
    src_nodata = src_nodata,
    cpu_workers = cpu_workers,
    io_concurrency = io_concurrency
  )
  cells_out <- new_a5_cell_from_rs(out$cell)
  bands <- out$bands
  names(bands) <- as.character(out$band_names)

  if (as_vector) {
    n <- length(cells_out)
    n_bands <- length(bands)
    mat <- vapply(bands, identity, numeric(n))
    if (n_bands == 1L) dim(mat) <- c(n, 1L)
    value <- lapply(seq_len(n), function(i) as.numeric(mat[i, ]))
    tibble::tibble(cell = cells_out, value = value)
  } else {
    tibble::tibble(cell = cells_out, !!!bands)
  }
}
