#' Session cache of remote byte ranges
#'
#' Remote reads keep the byte ranges they fetch (header chunks and
#' compressed blocks) in one cache shared by every read in the R session,
#' so a read split into `bbox` chunks, or repeated, fetches each block once
#' rather than once per call. Ranges are kept per source URL and
#' `store_opts`, and the least recently used are dropped past the budget.
#' Local files bypass the cache.
#'
#' The cache assumes remote files do not change during the session. Clear
#' it after a source is rewritten in place, or after pointing a URL at
#' another endpoint through environment variables.
#'
#' The default budget is 512 MiB, or the value of the environment variable
#' `A5PX_CACHE_MB` when the first remote read starts.
#'
#' @param max_mb Numeric scalar (>= 0): the budget in MiB. `0` disables the
#'   cache and frees what it holds.
#' @returns `a5px_set_cache()` returns the previous budget in MiB,
#'   invisibly. `a5px_cache_info()` returns a list: `entries`, `size_mb`,
#'   `max_mb`, and the `hits` and `misses` since the cache was last
#'   cleared. `a5px_clear_cache()` returns `NULL`, invisibly.
#' @export
#' @examples
#' a5px_set_cache(1024)
#' a5px_cache_info()
#' a5px_clear_cache()
a5px_set_cache <- function(max_mb) {
  if (!is.numeric(max_mb) || length(max_mb) != 1L || is.na(max_mb) || max_mb < 0) {
    cli::cli_abort("{.arg max_mb} must be a single number >= 0.")
  }
  invisible(a5px_cache_set_rs(as.numeric(max_mb)))
}

#' @rdname a5px_set_cache
#' @export
a5px_cache_info <- function() {
  a5px_cache_info_rs()
}

#' @rdname a5px_set_cache
#' @export
a5px_clear_cache <- function() {
  a5px_cache_clear_rs()
  invisible(NULL)
}
