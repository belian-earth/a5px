#' Read a (Cloud-Optimised) GeoTIFF as A5 cells, returning an Arrow Table
#'
#' Same engine as [a5_read_raster()] but returns an Arrow `Table` whose value
#' column is a single `FixedSizeList<float, n_bands>`. Bypasses R-list
#' construction entirely (Rust hands a flat cell-major buffer straight to the
#' Arrow array constructor), making it the right entry point for embedding
#' rasters destined for Parquet.
#'
#' @param src,resolution,stat,bands,threads,io_concurrency See [a5_read_raster()].
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
                                 threads = 1L,
                                 io_concurrency = 8L,
                                 value_type = c("float64", "float32")) {
  rlang::check_installed("arrow", reason = "to construct Arrow tables")
  check_scalar_string(src, "src")
  resolution <- vctrs::vec_cast(resolution, integer(), x_arg = "resolution")
  vctrs::vec_assert(resolution, size = 1L)
  check_resolution(resolution)
  stats <- check_stats(stat)
  threads <- check_scalar_count(threads, "threads")
  io_concurrency <- check_scalar_count(io_concurrency, "io_concurrency")
  value_type <- rlang::arg_match(value_type)
  band_sel <- parse_bands_arg(bands)

  out <- a5_read_raster_flat_rs(
    src = src,
    resolution = resolution,
    stats = stats,
    bands_idx = band_sel$idx,
    bands_names = band_sel$names,
    threads = threads,
    io_concurrency = io_concurrency
  )

  cells <- new_a5_cell_from_rs(out$cell)
  band_names <- as.character(out$band_names)
  n_bands <- as.integer(out$n_bands)
  stats_out <- as.character(out$stats)

  cell_arr <- a5R::a5_cell_to_arrow(cells)
  inner_type <- switch(value_type, float64 = arrow::float64(), float32 = arrow::float32())
  fsl_type   <- arrow::fixed_size_list_of(inner_type, n_bands)
  n_cells    <- length(cells)

  flat_to_fsl <- function(flat) {
    mat  <- matrix(flat, nrow = n_cells, ncol = n_bands, byrow = TRUE)
    rows <- asplit(mat, 1L)
    arrow::Array$create(rows, type = fsl_type)
  }

  cols <- list(cell = cell_arr)
  if (length(stats_out) == 1L) {
    cols$value <- flat_to_fsl(out$value_flat[[1]])
  } else {
    for (s in stats_out) {
      cols[[s]] <- flat_to_fsl(out$value_flat[[s]])
    }
  }

  tbl <- do.call(arrow::Table$create, cols)
  meta <- list(
    a5px_band_names = paste(band_names, collapse = "\n"),
    a5px_resolution = as.character(resolution),
    a5px_stats      = paste(stats_out, collapse = "\n")
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
#' @param src,resolution,stat,bands,threads,io_concurrency See
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
                                 value_type = c("float64", "float32"),
                                 compression = c("zstd", "snappy", "none"),
                                 threads = 1L,
                                 io_concurrency = 8L) {
  check_scalar_string(src, "src")
  check_scalar_string(dest, "dest")
  resolution <- vctrs::vec_cast(resolution, integer(), x_arg = "resolution")
  vctrs::vec_assert(resolution, size = 1L)
  check_resolution(resolution)
  stats <- check_stats(stat)
  value_type <- rlang::arg_match(value_type)
  compression <- rlang::arg_match(compression)
  threads <- check_scalar_count(threads, "threads")
  io_concurrency <- check_scalar_count(io_concurrency, "io_concurrency")
  band_sel <- parse_bands_arg(bands)

  invisible(a5_raster_to_parquet_rs(
    src = src,
    dest = dest,
    resolution = resolution,
    stats = stats,
    bands_idx = band_sel$idx,
    bands_names = band_sel$names,
    value_type = value_type,
    compression = compression,
    threads = threads,
    io_concurrency = io_concurrency
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
