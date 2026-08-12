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

#' Validate a user-supplied bbox.
#' @noRd
check_bbox <- function(bbox, call = rlang::caller_env()) {
  if (is.null(bbox)) {
    return(numeric(0))
  }
  v <- vctrs::vec_cast(bbox, double(), x_arg = "bbox")
  if (length(v) != 4L || anyNA(v)) {
    cli::cli_abort(
      "{.arg bbox} must be a length-4 numeric {.code c(xmin, ymin, xmax, ymax)} in WGS 84.",
      call = call
    )
  }
  if (v[1] >= v[3] || v[2] >= v[4]) {
    cli::cli_abort(
      "{.arg bbox} must satisfy {.code xmin < xmax} and {.code ymin < ymax}.",
      call = call
    )
  }
  v
}

#' Validate a user-supplied src_nodata override.
#' @noRd
check_src_nodata <- function(src_nodata, call = rlang::caller_env()) {
  if (is.null(src_nodata)) {
    return(numeric(0))
  }
  v <- vctrs::vec_cast(src_nodata, double(), x_arg = "src_nodata")
  if (length(v) != 1L) {
    cli::cli_abort(
      "{.arg src_nodata} must be a length-1 numeric (or NULL).",
      call = call
    )
  }
  v
}

#' Validate the user-facing `stat` arg into a non-empty character vector of
#' valid stat names.
#' @noRd
check_stats <- function(stat, call = rlang::caller_env()) {
  valid <- c("mean", "sum", "count", "min", "max", "var", "sd",
             "majority", "fractions")
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

#' Cross-argument rules for the categorical stats (majority / fractions).
#' @noRd
check_stat_context <- function(stats, dequant, as_vector = FALSE,
                               fractions_ok = TRUE,
                               call = rlang::caller_env()) {
  has_frac <- "fractions" %in% stats
  has_cat <- has_frac || "majority" %in% stats
  if (has_frac) {
    if (!fractions_ok) {
      cli::cli_abort(
        "{.val fractions} is only available in {.fn a5_read_raster}.",
        call = call
      )
    }
    if (length(stats) > 1L) {
      cli::cli_abort(
        "{.val fractions} must be the only requested stat.",
        call = call
      )
    }
    if (isTRUE(as_vector)) {
      cli::cli_abort(
        "{.val fractions} cannot be combined with {.code as_vector = TRUE}.",
        call = call
      )
    }
  }
  if (has_cat && !is.null(dequant)) {
    cli::cli_abort(
      "majority/fractions operate on raw integer codes and cannot be combined with {.arg dequant}.",
      call = call
    )
  }
  invisible(stats)
}

#' Validate the user-facing `subsamples` arg. Returns the integer passed to
#' Rust: 0 = auto-select k, otherwise the explicit sub-point grid dimension.
#' @noRd
check_subsamples <- function(subsamples, mode, call = rlang::caller_env()) {
  if (is.null(subsamples)) {
    return(0L)
  }
  if (!identical(mode, "overlay")) {
    cli::cli_abort(
      "{.arg subsamples} is only used when {.code mode = \"overlay\"}.",
      call = call
    )
  }
  s <- vctrs::vec_cast(subsamples, integer(), x_arg = "subsamples")
  if (length(s) != 1L || is.na(s) || s < 2L || s > 64L) {
    cli::cli_abort(
      "{.arg subsamples} must be a single integer in 2..64, or NULL for auto.",
      call = call
    )
  }
  s
}

#' A5 cell edge length in metres, passed to Rust for overlay auto-k.
#' 0 when overlay is off (the value is unused there).
#' @noRd
cell_edge_metres <- function(mode, resolution) {
  if (!identical(mode, "overlay")) {
    return(0)
  }
  sqrt(as.numeric(a5R::a5_cell_area(resolution, units = "m^2")))
}

#' Resolve the overview target passed to Rust.
#'
#' Returns the A5 cell edge length in metres at `resolution` when overview use
#' is enabled and the requested stat is exactly `"mean"` (the only stat that
#' survives reading a decimated, averaging-built overview). Otherwise returns
#' `0`, which tells Rust to read full resolution. `sum`/`count`/`var`/`sd`/
#' `min`/`max` are not decimation-invariant, so overviews are never used for
#' them even when `use_overviews = TRUE`.
#' @noRd
overview_target_metres <- function(use_overviews, stats, resolution,
                                   call = rlang::caller_env()) {
  if (!is.logical(use_overviews) || length(use_overviews) != 1L ||
        is.na(use_overviews)) {
    cli::cli_abort("{.arg use_overviews} must be a length-1 non-NA logical.",
                   call = call)
  }
  if (!isTRUE(use_overviews) || !identical(stats, "mean")) {
    return(0)
  }
  sqrt(as.numeric(a5R::a5_cell_area(resolution, units = "m^2")))
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
