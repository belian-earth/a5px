#' Tile-level concurrency for raster reads
#'
#' a5px's reader runs as a producer / consumer pipeline:
#'
#' - **`cpu_workers`** is the number of CPU threads a read uses: the
#'   blocking-thread consumers that decode each block, and the pool of the
#'   same size that indexes each decoded block as row stripes, so a read
#'   that spans fewer blocks than workers still uses every worker. Pick
#'   this to match the number of physical cores you can spare.
#' - **`io_concurrency`** is the number of remote requests a read keeps in
#'   flight, across all its sources. Blocks next to each other in the file
#'   (a band's blocks along a row, say) are fetched in one request. For
#'   local files it is the number of blocks read at once.
#'
#' These two knobs replace the old `threads` arg, which conflated tokio
#' worker count and tile-level parallelism in confusing ways.
#'
#' Defaults are resolved per call: function args > options > env vars >
#' built-in default. For `cpu_workers` that is the number of CPUs available
#' to the R process, which respects affinity masks and container or cgroup
#' CPU quotas (a job limited to 4 CPUs on a 20-core host gets 4); for
#' `io_concurrency` it is 32. Set globally via
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
    v <- max(1L, a5px_available_cpus_rs())
  }
  as.integer(v)
}

#' @noRd
resolve_io_concurrency <- function() {
  v <- getOption("a5px.io_concurrency")
  if (is.null(v)) {
    env <- Sys.getenv("A5PX_IO_CONCURRENCY", unset = "")
    if (nzchar(env)) v <- suppressWarnings(as.integer(env))
  }
  if (is.null(v) || is.na(v) || v < 1L) {
    # Requests in flight. Reading a 5 km AEF window (257 requests) took
    # 5.7 s at 64, 7.1 s at 32 and 7.9 s at 16 straight from S3, and
    # 37-66 s at 32 against 70-107 s at 16 and 64 through Source
    # Cooperative's proxy, which slows under more. The setting used to
    # count fetch tasks of up to ten requests each.
    v <- 32L
  }
  as.integer(v)
}
