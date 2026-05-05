#' Tile-level concurrency for raster reads
#'
#' a5px's reader runs as a producer / consumer pipeline:
#'
#' - **`cpu_workers`** is the size of the consumer pool (the blocking-thread
#'   workers that decode each tile and drive [a5R::a5_lonlat_to_cell()] /
#'   [a5R::a5_cell_to_lonlat()] over its pixels). Pick this to match the
#'   number of physical cores you can spare.
#' - **`io_concurrency`** is the number of in-flight tile fetches the
#'   producer issues at once. For local files leave this at the default
#'   (`io_concurrency = cpu_workers`); for cloud reads of multi-band
#'   embedding rasters, push it higher (`32`, `64`+) so the network stays
#'   busy while CPU workers consume from a bounded channel.
#'
#' These two knobs replace the old `threads` arg, which conflated tokio
#' worker count and tile-level parallelism in confusing ways.
#'
#' Defaults are resolved per call: function args > options > env vars >
#' built-in default ([parallel::detectCores()] for `cpu_workers`, the same
#' value capped at 32 for `io_concurrency`). Set globally via
#' [a5px_set_concurrency()], `options(a5px.cpu_workers = ...)`,
#' `options(a5px.io_concurrency = ...)`, or environment variables
#' `A5PX_CPU_WORKERS` / `A5PX_IO_CONCURRENCY`.
#'
#' @param cpu_workers Integer scalar (>= 1) or `NULL`.
#' @param io_concurrency Integer scalar (>= 1) or `NULL`.
#' @returns `a5px_set_concurrency()` returns the previous setting invisibly.
#'   `a5px_get_concurrency()` returns a list with the current resolved
#'   defaults.
#'
#' @export
#' @examples
#' a5px_set_concurrency(cpu_workers = 8, io_concurrency = 32)
#' a5px_get_concurrency()
a5px_set_concurrency <- function(cpu_workers = NULL, io_concurrency = NULL) {
  prev <- a5px_get_concurrency()
  if (!is.null(cpu_workers)) {
    cpu_workers <- check_scalar_count(cpu_workers, "cpu_workers")
    options(a5px.cpu_workers = as.integer(cpu_workers))
  }
  if (!is.null(io_concurrency)) {
    io_concurrency <- check_scalar_count(io_concurrency, "io_concurrency")
    options(a5px.io_concurrency = as.integer(io_concurrency))
  }
  invisible(prev)
}

#' @rdname a5px_set_concurrency
#' @export
a5px_get_concurrency <- function() {
  list(
    cpu_workers    = resolve_cpu_workers(),
    io_concurrency = resolve_io_concurrency()
  )
}

#' @noRd
resolve_cpu_workers <- function() {
  v <- getOption("a5px.cpu_workers")
  if (is.null(v)) {
    env <- Sys.getenv("A5PX_CPU_WORKERS", unset = "")
    if (nzchar(env)) v <- suppressWarnings(as.integer(env))
  }
  if (is.null(v) || is.na(v) || v < 1L) {
    v <- max(1L, parallel::detectCores(logical = TRUE))
  }
  as.integer(v)
}

#' @noRd
resolve_io_concurrency <- function(cpu_workers = NULL) {
  v <- getOption("a5px.io_concurrency")
  if (is.null(v)) {
    env <- Sys.getenv("A5PX_IO_CONCURRENCY", unset = "")
    if (nzchar(env)) v <- suppressWarnings(as.integer(env))
  }
  if (is.null(v) || is.na(v) || v < 1L) {
    if (is.null(cpu_workers)) cpu_workers <- resolve_cpu_workers()
    # Conservative cloud default: between 8 (so small machines still get
    # some I/O parallelism) and 16 (CloudFlare / S3 / Source Cooperative
    # tend to queue or throttle past ~16 simultaneous range requests, so
    # going higher tends to hurt rather than help). Tune up via
    # a5px_set_concurrency() / A5PX_IO_CONCURRENCY for fast on-prem stores.
    v <- min(16L, max(cpu_workers, 8L))
  }
  as.integer(v)
}
