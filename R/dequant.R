#' Dequantize Alpha Earth Foundations (AEF) embedding codes
#'
#' The AEF int8 decode: `sign(x) * (x / 127.5)^2`. Per value, nonlinear and
#' sign-preserving, mapping the code range `-127..127` to roughly `[-1, 1]`.
#' Pass it as the `dequant` argument of [a5_read_raster()] (and the Arrow /
#' Parquet variants) so codes are decoded per pixel *before* aggregation;
#' a nonlinear decode does not commute with the mean, so decoding aggregated
#' codes gives biased values.
#'
#' @param x Raw integer codes (numeric vector).
#' @returns The decoded values, same length as `x`.
#' @export
#' @examples
#' dequant_aef(c(-127, 0, 64, 127))
#' \dontrun{
#'   emb <- a5_read_raster(src, resolution = 14L, dequant = dequant_aef)
#' }
dequant_aef <- function(x) {
  # branchless sign(x) * (x / 127.5)^2 as xn * |xn|
  xn <- x / 127.5
  xn * abs(xn)
}

#' LUT domain sent to Rust: covers every supported integer dtype
#' (int8 / uint8 / int16 / uint16). Rust validates the actual source dtype
#' against it at read time, so no metadata round-trip is needed here.
#' @noRd
dequant_lut_min <- -32768L
dequant_lut_max <- 65535L

#' Validate the user-facing `dequant` arg and build the code -> value LUT
#' passed to Rust. Returns list(lut = double(), min = double(1)); an empty
#' `lut` means "no dequantization".
#' @noRd
check_dequant <- function(dequant, call = rlang::caller_env()) {
  if (is.null(dequant)) {
    return(list(lut = numeric(0), min = 0))
  }
  if (!is.function(dequant)) {
    cli::cli_abort(
      "{.arg dequant} must be {.code NULL} or a vectorised function such as {.fn dequant_aef}.",
      call = call
    )
  }
  codes <- as.double(seq.int(dequant_lut_min, dequant_lut_max))
  lut <- dequant(codes)
  if (!is.numeric(lut) || length(lut) != length(codes)) {
    cli::cli_abort(
      "{.arg dequant} function must be vectorised: given a numeric vector of codes it must return an equal-length numeric vector.",
      call = call
    )
  }
  list(lut = as.double(lut), min = as.double(dequant_lut_min))
}

#' Warn when a dequant read is allowed to touch overviews. Average-built
#' overview pixels are means of quantized codes and decode incorrectly;
#' only nearest- or mode-resampled overviews hold valid codes.
#' @noRd
warn_dequant_overviews <- function(dequant, use_overviews) {
  if (!is.null(dequant) && isTRUE(use_overviews)) {
    cli::cli_warn(c(
      "{.code use_overviews = TRUE} with {.arg dequant} assumes the source's overviews were built with nearest or mode resampling.",
      "!" = "Average-built overviews store means of quantized codes, which decode incorrectly."
    ))
  }
  invisible(NULL)
}
