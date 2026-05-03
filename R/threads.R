#' Get or set the worker-thread count for raster reads
#'
#' Controls how many tiles are decoded and processed concurrently inside the
#' Rust runtime. Independent of [a5R::a5_set_threads()].
#'
#' @param n Integer scalar (>= 1). Number of worker threads.
#' @returns `a5cog_set_threads()` returns the previous count invisibly.
#'   `a5cog_get_threads()` returns the current count.
#'
#' @export
#' @examples
#' a5cog_set_threads(2)
#' a5cog_get_threads()
a5cog_set_threads <- function(n = 1L) {
  n <- check_scalar_count(n, "n")
  prev <- a5cog_get_threads_rs()
  a5cog_set_threads_rs(n)
  invisible(as.integer(prev))
}

#' @rdname a5cog_set_threads
#' @export
a5cog_get_threads <- function() {
  as.integer(a5cog_get_threads_rs())
}
