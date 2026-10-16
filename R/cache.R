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
#' The default budget is 512 MiB, held to an eighth of the memory limit of
#' the process's cgroup (a container, a Batch task or a systemd scope), so
#' a task limited to 2 GiB caches at most 256 MiB. The environment variable
#' `A5PX_CACHE_MB`, read when the first remote read starts, overrides it.
#'
#' @param max_mb Numeric scalar (>= 0): the budget in MiB. `0` disables the
#'   cache and frees what it holds.
#' @returns `a5px_set_cache()` returns the previous budget in MiB,
#'   invisibly. `a5px_cache_info()` returns a list: `entries`, `size_mb`,
#'   `max_mb`, and since the cache was last cleared the `hits` and
#'   `misses`, the remote `requests` made and the `fetched_mb` they
#'   returned. `a5px_clear_cache()` returns `NULL`, invisibly.
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
