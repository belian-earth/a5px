#' Read a (Cloud-Optimised) GeoTIFF as A5 cells, returning an Arrow Table
#'
#' Same engine as [a5_read_raster()] but returns an Arrow `Table` whose value
#' column is a single `FixedSizeList<float, n_bands>`. Bypasses R-list
#' construction entirely (Rust hands a flat cell-major buffer straight to the
#' Arrow array constructor), making it the right entry point for embedding
#' rasters destined for Parquet.
#'
#' @param src,resolution,stat,bands,bbox,src_nodata,cpu_workers,io_concurrency,as_vector,use_overviews See [a5_read_raster()].
#' @param value_type Storage type for the value column. `"float64"` (default)
#'   or `"float32"` (halves disk size for embeddings).
#'
#' @returns An [arrow::Table] with two columns:
#'   - `cell`: `uint64` (interoperable with [a5R::a5_cell_from_arrow()]).
#'   - `value`: `FixedSizeList<float, n_bands>`. Field-level metadata records
#'     the band names so dimension order is preserved when the table is
#'     written to Parquet and read back in another language.
#'
#' @seealso [a5_write_parquet()] to write the result to Parquet,
#'   [a5_read_raster()] for the tibble path.
#' @export
#' @examplesIf requireNamespace("arrow", quietly = TRUE)
#' \dontrun{
#'   tbl <- a5_read_raster_arrow(
#'     "https://data.source.coop/.../tile.tiff",
#'     resolution = 14L, bands = 1:8, value_type = "float32"
#'   )
#'   a5_write_parquet(tbl, "embeddings.parquet")
#' }
a5_read_raster_arrow <- function(src,
                                 resolution,
                                 stat = "mean",
                                 bands = NULL,
                                 bbox = NULL,
                                 src_nodata = NULL,
                                 cpu_workers = NULL,
                                 io_concurrency = NULL,
                                 as_vector = FALSE,
                                 value_type = c("float64", "float32"),
                                 use_overviews = TRUE) {
  rlang::check_installed("arrow", reason = "to construct Arrow tables")
  check_scalar_string(src, "src")
  resolution <- vctrs::vec_cast(resolution, integer(), x_arg = "resolution")
  vctrs::vec_assert(resolution, size = 1L)
  check_resolution(resolution)
  stats <- check_stats(stat)
  cpu_workers <- if (is.null(cpu_workers)) resolve_cpu_workers()
                 else check_scalar_count(cpu_workers, "cpu_workers")
  io_concurrency <- if (is.null(io_concurrency)) resolve_io_concurrency(cpu_workers)
                    else check_scalar_count(io_concurrency, "io_concurrency")
  value_type <- rlang::arg_match(value_type)
  if (!is.logical(as_vector) || length(as_vector) != 1L || is.na(as_vector)) {
    cli::cli_abort("{.arg as_vector} must be a length-1 non-NA logical.")
  }
  band_sel <- parse_bands_arg(bands)
  bbox_v <- check_bbox(bbox)
  src_nodata_v <- check_src_nodata(src_nodata)
  overview_target_m <- overview_target_metres(use_overviews, stats, resolution)

  out <- a5_read_raster_flat_rs(
    src = src,
    resolution = resolution,
    stats = stats,
    bands_idx = band_sel$idx,
    bands_names = band_sel$names,
    bbox = bbox_v,
    src_nodata = src_nodata_v,
    cpu_workers = cpu_workers,
    io_concurrency = io_concurrency,
    overview_target_m = overview_target_m
  )

  cells <- new_a5_cell_from_rs(out$cell)
  band_names <- as.character(out$band_names)
  n_bands <- as.integer(out$n_bands)
  stats_out <- as.character(out$stats)
  inner_type <- switch(value_type, float64 = arrow::float64(), float32 = arrow::float32())
  cell_arr <- a5R::a5_cell_to_arrow(cells)
  n_cells  <- length(cells)

  cols <- list(cell = cell_arr)
  if (as_vector) {
    fsl_type <- arrow::fixed_size_list_of(inner_type, n_bands)
    flat_to_fsl <- function(flat) {
      mat  <- matrix(flat, nrow = n_cells, ncol = n_bands, byrow = TRUE)
      rows <- asplit(mat, 1L)
      arrow::Array$create(rows, type = fsl_type)
    }
    if (length(stats_out) == 1L) {
      cols$value <- flat_to_fsl(out$value_flat[[1]])
    } else {
      for (s in stats_out) cols[[paste0("value_", s)]] <- flat_to_fsl(out$value_flat[[s]])
    }
  } else {
    # one Arrow column per (band, stat). Single stat -> column = band name;
    # multi-stat -> "<band>_<stat>". Matches the tibble path's wide layout.
    for (s_i in seq_along(stats_out)) {
      flat <- out$value_flat[[s_i]]
      mat  <- matrix(flat, nrow = n_cells, ncol = n_bands, byrow = TRUE)
      for (b_idx in seq_along(band_names)) {
        col_name <- if (length(stats_out) == 1L) band_names[b_idx]
                    else paste(band_names[b_idx], stats_out[s_i], sep = "_")
        cols[[col_name]] <- arrow::Array$create(mat[, b_idx], type = inner_type)
      }
    }
  }

  tbl <- do.call(arrow::Table$create, cols)
  meta <- list(
    a5px_band_names = paste(band_names, collapse = "\n"),
    a5px_resolution = as.character(resolution),
    a5px_stats      = paste(stats_out, collapse = "\n"),
    a5px_layout     = if (as_vector) "fsl" else "wide"
  )
  if (length(stats_out) == 1L) {
    meta$a5px_stat <- stats_out[[1]]  # legacy single-stat key
  }
  tbl$ReplaceSchemaMetadata(meta)
}

#' Read a raster and write it straight to Parquet (Rust-direct)
#'
#' Same engine and on-disk schema as [a5_read_raster_arrow()] +
#' [a5_write_parquet()], but skips the R/Arrow round-trip entirely:
#' the Rust side aggregates pixels into A5 cells, builds the
#' `cell:uint64` + `value:FixedSizeList<float, n_bands>` RecordBatch
#' from its existing flat buffer, and writes Parquet via the
#' `parquet` Rust crate. No intermediate R numeric materialisation.
#'
#' Pick this when you want a Parquet file at the end and don't need
#' the Arrow Table in R. For million-cell embedding rasters it avoids
#' the per-cell list-of-vectors construction that the R-Arrow path
#' does, which is the main remaining cost in that pipeline.
#'
#' @param src,resolution,stat,bands,bbox,src_nodata,cpu_workers,io_concurrency,as_vector,use_overviews See
#'   [a5_read_raster()].
#' @param dest Output Parquet path.
#' @param value_type Storage type for the value column. `"float64"`
#'   (default) or `"float32"` (halves the disk size for embeddings).
#' @param compression Parquet compression codec.
#'   One of `"zstd"` (default), `"snappy"`, `"none"`.
#' @returns `dest` invisibly.
#'
#' @export
#' @examples
#' \dontrun{
#'   a5_raster_to_parquet(
#'     "https://data.source.coop/.../tile.tiff",
#'     "embeddings.parquet",
#'     resolution = 14L, bands = 1:8, value_type = "float32"
#'   )
#' }
a5_raster_to_parquet <- function(src,
                                 dest,
                                 resolution,
                                 stat = "mean",
                                 bands = NULL,
                                 bbox = NULL,
                                 src_nodata = NULL,
                                 as_vector = FALSE,
                                 value_type = c("float64", "float32"),
                                 compression = c("zstd", "snappy", "none"),
                                 cpu_workers = NULL,
                                 io_concurrency = NULL,
                                 use_overviews = TRUE) {
  check_scalar_string(src, "src")
  check_scalar_string(dest, "dest")
  resolution <- vctrs::vec_cast(resolution, integer(), x_arg = "resolution")
  vctrs::vec_assert(resolution, size = 1L)
  check_resolution(resolution)
  stats <- check_stats(stat)
  value_type <- rlang::arg_match(value_type)
  compression <- rlang::arg_match(compression)
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
  overview_target_m <- overview_target_metres(use_overviews, stats, resolution)

  invisible(a5_raster_to_parquet_rs(
    src = src,
    dest = dest,
    resolution = resolution,
    stats = stats,
    bands_idx = band_sel$idx,
    bands_names = band_sel$names,
    bbox = bbox_v,
    src_nodata = src_nodata_v,
    as_vector = as_vector,
    value_type = value_type,
    compression = compression,
    cpu_workers = cpu_workers,
    io_concurrency = io_concurrency,
    overview_target_m = overview_target_m
  ))
}

#' Write A5-keyed values to a Parquet file
#'
#' Convenience writer. Accepts either an [arrow::Table] from
#' [a5_read_raster_arrow()] or a [tibble::tibble()] from [a5_read_raster()];
#' picks the right path under the hood. The on-disk schema is the same in
#' both cases when the tibble has a single fixed-length `value` list column
#' (i.e. was produced with `as_vector = TRUE`).
#'
#' @param x Output of [a5_read_raster()] (tibble) or [a5_read_raster_arrow()]
#'   (Arrow Table).
#' @param dest Output Parquet path.
#' @param compression Parquet compression codec. Default `"zstd"`.
#' @param ... Further arguments forwarded to [arrow::write_parquet()].
#' @returns `dest` invisibly.
#'
#' @export
#' @examplesIf requireNamespace("arrow", quietly = TRUE)
#' \dontrun{
#'   tbl <- a5_read_raster_arrow(path, resolution = 14L, bands = 1:8)
#'   a5_write_parquet(tbl, "out.parquet")
#' }
a5_write_parquet <- function(x, dest, compression = "zstd", ...) {
  rlang::check_installed("arrow", reason = "to write Parquet files")
  check_scalar_string(dest, "dest")

  out <- if (inherits(x, c("Table", "RecordBatch"))) {
    x
  } else if (inherits(x, "data.frame")) {
    tbl_to_arrow(x)
  } else {
    cli::cli_abort(
      "{.arg x} must be an arrow Table/RecordBatch or a data.frame, not {.cls {class(x)[1]}}."
    )
  }

  arrow::write_parquet(out, sink = dest, compression = compression, ...)
  invisible(dest)
}

#' Convert an a5_read_raster() tibble to an arrow Table
#' @noRd
tbl_to_arrow <- function(tbl) {
  if (!"cell" %in% names(tbl) || !inherits(tbl$cell, "a5_cell")) {
    cli::cli_abort("{.arg x} must have an {.field cell} column of class {.cls a5_cell}.")
  }
  cell_arr <- a5R::a5_cell_to_arrow(tbl$cell)
  rest <- tbl[setdiff(names(tbl), "cell")]
  if (length(rest) == 1L && is.list(rest[[1]]) && !is.data.frame(rest[[1]])) {
    # as_vector = TRUE path: single list column with equal-length numeric vectors
    lens <- lengths(rest[[1]])
    if (length(unique(lens)) != 1L) {
      cli::cli_abort("List column elements must all share the same length to write FixedSizeList.")
    }
    n_bands <- as.integer(lens[[1]])
    fsl_type <- arrow::fixed_size_list_of(arrow::float64(), n_bands)
    value_arr <- arrow::Array$create(rest[[1]], type = fsl_type)
    arrow::Table$create(cell = cell_arr, value = value_arr)
  } else {
    do.call(arrow::Table$create, c(list(cell = cell_arr), as.list(rest)))
  }
}
