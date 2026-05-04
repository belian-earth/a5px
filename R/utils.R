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

#' Validate the user-facing `stat` arg into a non-empty character vector of
#' valid stat names.
#' @noRd
check_stats <- function(stat, call = rlang::caller_env()) {
  valid <- c("mean", "sum", "count", "min", "max")
  if (!is.character(stat) || length(stat) == 0L || anyNA(stat)) {
    cli::cli_abort(
      "{.arg stat} must be a non-empty character vector with no NAs.",
      call = call
    )
  }
  bad <- setdiff(stat, valid)
  if (length(bad) > 0L) {
    cli::cli_abort(
      c("Unknown stat(s): {.val {bad}}.",
        "i" = "Valid: {.val {valid}}."),
      call = call
    )
  }
  if (anyDuplicated(stat) > 0L) {
    cli::cli_abort("{.arg stat} must not contain duplicates.", call = call)
  }
  invisible(stat)
}

#' Normalise the user-facing `bands` arg into integer indices or character names.
#' Returns a list(idx = integer(), names = character()); at most one is non-empty.
#' @noRd
parse_bands_arg <- function(bands, call = rlang::caller_env()) {
  if (is.null(bands)) {
    return(list(idx = integer(), names = character()))
  }
  if (is.character(bands)) {
    if (anyNA(bands) || !length(bands)) {
      cli::cli_abort("{.arg bands} character vector must be non-empty and contain no NAs.",
                     call = call)
    }
    return(list(idx = integer(), names = bands))
  }
  if (is.numeric(bands)) {
    idx <- vctrs::vec_cast(bands, integer(), x_arg = "bands")
    if (anyNA(idx) || !length(idx) || any(idx < 1L)) {
      cli::cli_abort("{.arg bands} integer vector must be non-empty, positive, and contain no NAs.",
                     call = call)
    }
    return(list(idx = idx, names = character()))
  }
  cli::cli_abort(
    "{.arg bands} must be {.code NULL}, an integer vector, or a character vector.",
    call = call
  )
}
