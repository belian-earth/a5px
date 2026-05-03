#' @importFrom rlang caller_env
NULL

check_resolution <- function(resolution,
                             min = 0L,
                             max = 30L,
                             call = rlang::caller_env()) {
  bad <- !is.na(resolution) & (resolution < min | resolution > max)
  if (any(bad)) {
    cli::cli_abort(
      "{.arg resolution} must be between {min} and {max}, not {resolution[which(bad)[1]]}.",
      call = call
    )
  }
  invisible(resolution)
}

check_scalar_string <- function(x, arg, call = rlang::caller_env()) {
  if (!is.character(x) || length(x) != 1L || is.na(x)) {
    cli::cli_abort(
      "{.arg {arg}} must be a length-1 non-NA character string.",
      call = call
    )
  }
  invisible(x)
}

check_scalar_count <- function(x, arg, min = 1L, call = rlang::caller_env()) {
  x <- vctrs::vec_cast(x, integer(), x_arg = arg)
  if (length(x) != 1L || is.na(x) || x < min) {
    cli::cli_abort(
      "{.arg {arg}} must be a length-1 integer >= {min}.",
      call = call
    )
  }
  invisible(x)
}

#' Build an a5_cell vector from the b1..b8 raw fields returned by Rust.
#' @noRd
new_a5_cell_from_rs <- function(x) {
  vctrs::new_rcrd(
    list(
      b1 = x$b1, b2 = x$b2, b3 = x$b3, b4 = x$b4,
      b5 = x$b5, b6 = x$b6, b7 = x$b7, b8 = x$b8
    ),
    class = "a5_cell"
  )
}
