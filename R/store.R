#' Validate the user-facing `store_opts` arg into parallel key / value
#' character vectors for Rust. Accepts `NULL`, a named character vector, or
#' a named list of length-1 atomics (logicals become "true" / "false").
#' @noRd
check_store_opts <- function(store_opts, call = rlang::caller_env()) {
  if (is.null(store_opts) || length(store_opts) == 0L) {
    return(list(keys = character(), values = character()))
  }
  if (!is.character(store_opts) && !is.list(store_opts)) {
    cli::cli_abort(
      "{.arg store_opts} must be a named character vector or a named list.",
      call = call
    )
  }
  keys <- names(store_opts)
  if (is.null(keys) || anyNA(keys) || any(!nzchar(trimws(keys)))) {
    cli::cli_abort(
      "Every element of {.arg store_opts} must be named.",
      call = call
    )
  }
  keys <- tolower(trimws(keys))
  if (anyDuplicated(keys) > 0L) {
    cli::cli_abort(
      "{.arg store_opts} has duplicated names: {.val {keys[duplicated(keys)]}}.",
      call = call
    )
  }
  values <- vapply(seq_along(store_opts), function(i) {
    v <- store_opts[[i]]
    if (!is.atomic(v) || length(v) != 1L || is.na(v)) {
      cli::cli_abort(
        "{.arg store_opts} element {.val {keys[i]}} must be a length-1 non-NA atomic value.",
        call = call
      )
    }
    if (is.logical(v)) tolower(as.character(v)) else as.character(v)
  }, character(1))
  list(keys = keys, values = values)
}

#' Inspect the object store configuration a source resolves to
#'
#' Shows how a remote `src` will be accessed after environment variables
#' and `store_opts` are applied, without making a request. Use it to check
#' region, endpoint and signing before a long read. Credential values are
#' never returned, only whether static credentials are present.
#'
#' @inheritParams a5_read_raster
#' @returns A named character vector. Always contains `provider` (one of
#'   `"local"`, `"s3"`, `"gcs"`, `"azure"`, `"http"`); remote providers add
#'   `host` and provider-specific fields such as `region`, `endpoint`,
#'   `skip_signature` and `static_credentials`.
#' @seealso The "Remote sources" section of [a5px-package].
#' @examples
#' a5_store_config(
#'   "s3://us-west-2.opendata.source.coop/tge-labs/aef/v1/annual/2020/x.tiff",
#'   store_opts = c(aws_region = "us-west-2", aws_skip_signature = "true")
#' )
#' @export
a5_store_config <- function(src, store_opts = NULL) {
  check_scalar_string(src, "src")
  store <- check_store_opts(store_opts)
  out <- a5_store_config_rs(src, store$keys, store$values)
  vapply(out, as.character, character(1))
}
