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
#' Three sampling modes are available:
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
#' - `"overlay"`: each pixel contributes to every cell it overlaps,
#'   weighted by the overlapped fraction of its area (approximated by
#'   sub-pixel supersampling; see `subsamples`). More accurate than
#'   `"forward"` at cell/pixel boundaries, at some extra cost for pixels
#'   that straddle a cell edge. Under overlay, `"mean"` is the
#'   area-weighted mean, `"sum"` is mass-preserving (a pixel's value is
#'   split across the cells covering it, so totals such as population
#'   counts are conserved), and `"count"` is the effective (fractional)
#'   number of contributing pixels. `"var"` / `"sd"` use frequency
#'   weights with divisor `sum(w) - 1`.
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
#'   `"max"`, `"var"`, `"sd"`, `"majority"`, `"fractions"`, or any
#'   non-duplicated subset of those (except `"fractions"`, which must be
#'   alone) for a one-pass multi-stat read. Default `"mean"`. `"var"` /
#'   `"sd"` use the sample formula (divisor n - 1) computed via Welford's
#'   online algorithm in the streaming aggregator, matching [stats::var()] /
#'   [stats::sd()]; cells covered by a single pixel return `NA`.
#'
#'   `"majority"` and `"fractions"` are categorical: they treat the raw
#'   integer codes as class labels, and require an integer source of 16
#'   bits or fewer (like `dequant`, with which they cannot be combined).
#'   `"majority"` returns the class with the greatest total weight in each
#'   cell (pixel count under `mode = "forward"`, overlap area under
#'   `mode = "overlay"`), ties broken toward the smallest class code.
#'   `"fractions"` returns, per band, a list-column of named numeric
#'   vectors: each cell's per-class share of its total valid weight
#'   (shares sum to 1; names are the class codes). `"fractions"` is only
#'   available in `a5_read_raster()` with `as_vector = FALSE`.
#' @param bands Bands to read. One of:
#'   - `NULL` (default): read every band.
#'   - integer / numeric vector: 1-based band indices to read.
#'   - character vector: band names matched against the GDAL `DESCRIPTION`
#'     tag (falling back to `band_NN` when descriptions are absent).
#' @param bbox Optional spatial subset, as a numeric `c(xmin, ymin, xmax,
#'   ymax)` in WGS 84 lon/lat. When
#'   supplied, only tiles overlapping the bbox (in raster CRS) are fetched
#'   from the COG, and pixels outside the bbox are skipped. `NULL` (default)
#'   reads the whole raster.
#' @param src_nodata Optional numeric scalar overriding the source nodata.
#'   Use this when the file's `TIFFTAG_GDAL_NODATA` tag is missing or wrong;
#'   it takes precedence over the metadata value when set. `NULL` (default)
#'   uses whatever `async-tiff` exposes.
#' @param mode Sampling mode. `"forward"` (default) aggregates pixels into
#'   cells by their centres; `"overlay"` aggregates with pixel-cell overlap
#'   weights; `"centroid"` samples one pixel per cell at the cell centroid.
#'   See Details.
#' @param subsamples Sub-point grid dimension per pixel for
#'   `mode = "overlay"`: each pixel straddling a cell boundary is split
#'   into `subsamples^2` sub-points whose per-cell counts give the overlap
#'   weights (interior pixels take a fast path and never pay this cost).
#'   `NULL` (default) auto-selects from the pixel/cell edge ratio so
#'   sub-point spacing is at most half the cell edge, clamped to `[2, 16]`.
#'   Larger values approximate exact area weighting more closely. Only
#'   used when `mode = "overlay"`; an error otherwise.
#' @param interp Resampling kernel for `mode = "centroid"`:
#'   `"nearest"` (default, the containing pixel), `"bilinear"` (2x2),
#'   `"bicubic"` (Keys 4x4), or `"lanczos"` (Lanczos-3, 6x6). Kernel
#'   weights are renormalised over valid pixels, so nodata holes and
#'   raster edges shrink the stencil instead of propagating `NA`; a cell
#'   whose whole stencil is invalid returns `NA`. With `dequant`, each
#'   stencil pixel is decoded before the kernel is applied (nonlinear
#'   decodes do not commute with interpolation). Use `"nearest"` for
#'   categorical rasters; the smooth kernels blend class codes into
#'   meaningless intermediates. Only used when `mode = "centroid"`; an
#'   error otherwise.
#' @param cpu_workers Number of CPU consumers in the tile-processing pool.
#'   `NULL` (default) resolves from `getOption("a5px.cpu_workers")`, env
#'   `A5PX_CPU_WORKERS`, then [parallel::detectCores()].
#' @param io_concurrency Maximum in-flight tile fetches the producer issues
#'   to the I/O stage. `NULL` (default) resolves from
#'   `getOption("a5px.io_concurrency")`, env `A5PX_IO_CONCURRENCY`, then
#'   `min(32, max(cpu_workers, 8))`. Bump this for cloud reads of multi-band
#'   embedding rasters where the network can absorb more parallelism than
#'   the CPU pool. See [a5px_set_concurrency()].
#' @param dequant Optional per-pixel decode applied *before* aggregation: a
#'   vectorised R function mapping raw integer codes to decoded values, e.g.
#'   [dequant_aef] for Alpha Earth Foundations int8 embedding codes.
#'   Nonlinear decodes do not commute with aggregation, so quantized values
#'   must be decoded per pixel before the mean, not after; this argument
#'   does exactly that.
#'   The function is evaluated once over the full integer code domain to
#'   build a lookup table applied on the Rust side, which restricts
#'   `dequant` to sources with an integer data type of 16 bits or fewer
#'   (int8 / uint8 / int16 / uint16); other dtypes error at read time.
#'   nodata is matched against the raw code, before decoding. `NULL`
#'   (default) reads raw values.
#' @param as_vector Logical. If `TRUE`, collapse the (selected) bands into
#'   list columns of fixed-length numeric vectors -- one per stat. Single
#'   stat returns a single column `value`; multi-stat returns
#'   `value_<stat>` columns (e.g. `value_mean`, `value_max`). Useful for
#'   embedding rasters. Default `FALSE` (one numeric column per band).
#' @param use_overviews Logical. When `TRUE` and `stat = "mean"`, read the
#'   coarsest COG overview that still oversamples the target A5 cell
#'   instead of the full-resolution image. For aggregations to cells much
#'   coarser than the source pixels this moves far fewer bytes and does far
#'   less per-pixel work for a near-identical mean. Ignored unless the stat is
#'   exactly `"mean"` (`sum`/`count`/`var`/`sd`/`min`/`max` are not preserved
#'   under decimation) and in `mode = "centroid"`. Set `FALSE` to always read
#'   full resolution. Requires the source to carry overviews; otherwise the
#'   full-resolution image is read regardless. The default is `TRUE` unless
#'   `dequant` is set: overviews are typically average-resampled, and an
#'   overview pixel that is a mean of quantized codes decodes incorrectly.
#'   Explicitly passing `TRUE` together with `dequant` warns and proceeds,
#'   for sources whose overviews were built with nearest or mode resampling.
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
                           mode = c("forward", "overlay", "centroid"),
                           subsamples = NULL,
                           interp = c("nearest", "bilinear", "bicubic", "lanczos"),
                           cpu_workers = NULL,
                           io_concurrency = NULL,
                           dequant = NULL,
                           as_vector = FALSE,
                           use_overviews = is.null(dequant)) {
  check_scalar_string(src, "src")
  resolution <- vctrs::vec_cast(resolution, integer(), x_arg = "resolution")
  vctrs::vec_assert(resolution, size = 1L)
  check_resolution(resolution)
  stats <- check_stats(stat)
  mode <- rlang::arg_match(mode)
  subsamples_v <- check_subsamples(subsamples, mode)
  interp <- rlang::arg_match(interp)
  if (interp != "nearest" && mode != "centroid") {
    cli::cli_abort(
      "{.arg interp} is only used when {.code mode = \"centroid\"}."
    )
  }
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
  dequant_v <- check_dequant(dequant)
  check_stat_context(stats, dequant, as_vector, fractions_ok = TRUE)
  warn_dequant_overviews(dequant, use_overviews)
  overview_target_m <- overview_target_metres(use_overviews, stats, resolution)

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
      stats = stats,
      dequant_v = dequant_v,
      interp = interp
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
    io_concurrency = io_concurrency,
    overview_target_m = overview_target_m,
    dequant_lut = dequant_v$lut,
    dequant_min = dequant_v$min,
    overlay = identical(mode, "overlay"),
    subsamples = subsamples_v,
    cell_edge_m = cell_edge_metres(mode, resolution)
  )

  cells <- new_a5_cell_from_rs(out$cell)

  if ("fractions" %in% stats) {
    # ragged CSR from Rust -> one list-column of named share vectors per band
    n <- length(cells)
    cols <- list(cell = cells)
    for (b in as.character(out$band_names)) {
      fr <- out$fractions[[b]]
      cls <- as.character(as.integer(fr$classes))
      shr <- as.numeric(fr$shares)
      off <- as.integer(fr$offsets) # length n + 1, starting at 0
      cols[[b]] <- lapply(seq_len(n), function(i) {
        if (off[i + 1L] > off[i]) {
          stats::setNames(shr[(off[i] + 1L):off[i + 1L]],
                          cls[(off[i] + 1L):off[i + 1L]])
        } else {
          stats::setNames(numeric(0), character(0))
        }
      })
    }
    return(tibble::tibble(!!!cols))
  }

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

#' Centroid-mode read: enumerate cells covering the bbox via
#' [a5R::a5_polygon_to_cells()], then sample one pixel per cell.
#' @noRd
read_raster_centroid <- function(src, resolution, bands_idx, bands_names,
                                 bbox, src_nodata, cpu_workers,
                                 io_concurrency, as_vector, stats,
                                 dequant_v = list(lut = numeric(0), min = 0),
                                 interp = "nearest") {
  if (length(stats) > 1L || stats[1] != "mean") {
    cli::cli_warn(
      "{.code mode = \"centroid\"} returns one sample per cell; the {.arg stat} arg is ignored.",
      .frequency = "regularly", .frequency_id = "a5px-centroid-stat"
    )
  }
  if (is.null(bbox)) {
    bbox <- as.numeric(a5_raster_bbox_lonlat_rs(src))
  }
  # a5R >= 0.4.0 replaced a5_grid() with a5_polygon_to_cells() (centre-in-polygon
  # semantics, returns compacted cells). Uncompact to a uniform grid at the
  # requested resolution so each cell gets one centroid sample.
  cells <- a5R::a5_uncompact(
    a5R::a5_polygon_to_cells(
      wk::rct(bbox[1], bbox[2], bbox[3], bbox[4]),
      resolution = resolution
    ),
    resolution = resolution
  )
  if (length(cells) == 0L) {
    cli::cli_abort(
      "No A5 cells have centroids within the requested bbox at resolution {resolution}. Use a finer resolution or a larger bbox."
    )
  }
  out <- a5_sample_at_cells_rs(
    src = src,
    cells_raw = vctrs::vec_data(cells),
    bands_idx = bands_idx,
    bands_names = bands_names,
    src_nodata = src_nodata,
    cpu_workers = cpu_workers,
    io_concurrency = io_concurrency,
    dequant_lut = dequant_v$lut,
    dequant_min = dequant_v$min,
    interp = interp
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
