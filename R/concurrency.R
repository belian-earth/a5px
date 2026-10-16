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
#' Defaults are resolved per call: function args > options > env vars >
#' built-in default. For `cpu_workers` that is the number of CPUs available
#' to the R process, which respects affinity masks and container or cgroup
#' CPU quotas (a job limited to 4 CPUs on a 20-core host gets 4); for
#' `io_concurrency` it is 32. Set them for the session with
#' `a5px_concurrency()`, `options(a5px.cpu_workers = ...)` and
#' `options(a5px.io_concurrency = ...)`, or the environment variables
#' `A5PX_CPU_WORKERS` and `A5PX_IO_CONCURRENCY`.
#'
#' `a5px_concurrency()` works like [options()] and `a5R::a5_threads()`:
#' called with no settings it returns them, and called with settings it
#' changes them and returns the previous ones, which restore them when
#' passed back or when used with [with()].
#'
#' @param cpu_workers Integer scalar (>= 1), `NULL` to leave it unchanged,
#'   or the result of an earlier call, to restore both settings.
#' @param io_concurrency Integer scalar (>= 1), or `NULL` to leave it
#'   unchanged.
#' @returns With no settings, a list of the current `cpu_workers` and
#'   `io_concurrency`. Otherwise the previous settings, invisibly, as an
#'   `a5px_concurrency` object.
#'
#' @export
#' @examples
#' a5px_concurrency()
#' old <- a5px_concurrency(cpu_workers = 2, io_concurrency = 64)
#' a5px_concurrency(old) # restore
#'
#' # for one expression
#' with(a5px_concurrency(cpu_workers = 1), a5px_concurrency()$cpu_workers)
a5px_concurrency <- function(cpu_workers = NULL, io_concurrency = NULL) {
  if (inherits(cpu_workers, "a5px_concurrency")) {
    prev <- concurrency_options()
    options(a5px.cpu_workers = cpu_workers$cpu_workers,
            a5px.io_concurrency = cpu_workers$io_concurrency)
    return(invisible(prev))
  }
  if (is.null(cpu_workers) && is.null(io_concurrency)) {
    return(list(cpu_workers = resolve_cpu_workers(),
                io_concurrency = resolve_io_concurrency()))
  }
  # check both before changing either
  if (!is.null(cpu_workers)) cpu_workers <- check_scalar_count(cpu_workers, "cpu_workers")
  if (!is.null(io_concurrency)) io_concurrency <- check_scalar_count(io_concurrency, "io_concurrency")
  prev <- concurrency_options()
  if (!is.null(cpu_workers)) options(a5px.cpu_workers = as.integer(cpu_workers))
  if (!is.null(io_concurrency)) options(a5px.io_concurrency = as.integer(io_concurrency))
  invisible(prev)
}

#' The session options behind the settings, `NULL` where unset, so a
#' restore also returns an unset option to its default.
#' @noRd
concurrency_options <- function() {
  structure(
    list(cpu_workers = getOption("a5px.cpu_workers"),
         io_concurrency = getOption("a5px.io_concurrency")),
    class = "a5px_concurrency"
  )
}

#' @export
with.a5px_concurrency <- function(data, expr, ...) {
  on.exit(a5px_concurrency(data), add = TRUE)
  eval(substitute(expr), parent.frame())
}

#' @export
print.a5px_concurrency <- function(x, ...) {
  shown <- function(v) if (is.null(v)) "default" else format(v)
  cat("<a5px_concurrency> previous settings\n",
      "  cpu_workers:    ", shown(x$cpu_workers), "\n",
      "  io_concurrency: ", shown(x$io_concurrency), "\n", sep = "")
  invisible(x)
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
