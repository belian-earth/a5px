#' @importFrom rlang caller_env
NULL

check_resolution <- function(resolution,
                             min = 0L,
                             max = 29L,
                             call = rlang::caller_env()) {
  if (any(resolution == 30L, na.rm = TRUE)) {
    # the a5 crate (1.0.0) returns resolution-29 cells for some
    # points at 30, so a read's cells would mix resolutions
    cli::cli_abort(
      "Resolution 30 is not supported yet: A5 returns resolution-29 cells for some points at 30. Use resolution 29 or less.",
      call = call,
      class = c("a5px_error_resolution", "a5px_error_input", "a5px_error")
    )
  }
  bad <- !is.na(resolution) & (resolution < min | resolution > max)
  if (any(bad)) {
    cli::cli_abort(
      "{.arg resolution} must be between {min} and {max}, not {resolution[which(bad)[1]]}.",
      call = call,
      class = c("a5px_error_resolution", "a5px_error_input", "a5px_error")
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

#' Validate `src`, `separate` and `composite`. Returns the sources, their
#' labels (the names of `src`, else each file name without its extension),
#' the `separate` flag and the composite reducer (`""` for none).
#' @noRd
check_sources <- function(src, separate, stats, composite = NULL,
                          call = rlang::caller_env()) {
  if (!is.character(src) || length(src) == 0L || anyNA(src) || any(!nzchar(src))) {
    cli::cli_abort(
      "{.arg src} must be a character vector of paths or URLs.",
      call = call
    )
  }
  if (!is.logical(separate) || length(separate) != 1L || is.na(separate)) {
    cli::cli_abort("{.arg separate} must be {.code TRUE} or {.code FALSE}.", call = call)
  }
  labels <- sub("\\.[^.]*$", "", basename(sub("[?#].*$", "", src)))
  nm <- names(src)
  if (!is.null(nm)) {
    named <- !is.na(nm) & nzchar(nm)
    labels[named] <- nm[named]
  }
  if (separate) {
    if ("npix" %in% stats) {
      cli::cli_abort(
        "{.val npix} counts the pixels of one raster; with {.code separate = TRUE} use each band's {.val count}.",
        call = call
      )
    }
    dup <- unique(labels[duplicated(labels)])
    if (length(dup)) {
      cli::cli_abort(
        c("Sources need distinct labels when {.code separate = TRUE}.",
          "x" = "{.val {dup}} label{?s} more than one source.",
          "i" = "Name the sources, for example {.code src = c(red = ..., green = ...)}."),
        call = call
      )
    }
  }
  if (is.null(composite)) {
    composite <- ""
  } else {
    composite <- rlang::arg_match0(
      composite,
      c("mean", "median", "min", "max", "sum", "count", "var", "sd", "majority"),
      arg_nm = "composite", error_call = call
    )
    if (separate) {
      cli::cli_abort(
        "{.arg composite} combines sources with the same bands, so it does not apply with {.code separate = TRUE}.",
        call = call
      )
    }
    bad <- intersect(stats, c("fractions", "npix"))
    if (length(bad)) {
      cli::cli_abort("{.arg composite} does not apply to {.val {bad}}.", call = call)
    }
  }
  list(src = unname(src), labels = unname(labels), separate = separate,
       composite = composite)
}

#' Validate a user-supplied bbox into flat boxes for Rust: one box, or two
#' when `xmin > xmax` asks for a box across the antimeridian.
#' @noRd
check_bbox <- function(bbox, call = rlang::caller_env()) {
  if (is.null(bbox)) {
    return(numeric(0))
  }
  v <- vctrs::vec_cast(bbox, double(), x_arg = "bbox")
  if (length(v) != 4L || anyNA(v)) {
    cli::cli_abort(
      "{.arg bbox} must be a length-4 numeric {.code c(xmin, ymin, xmax, ymax)} in WGS 84.",
      call = call,
      class = c("a5px_error_geometry", "a5px_error_input", "a5px_error")
    )
  }
  if (v[1] < -180 || v[1] > 180 || v[3] < -180 || v[3] > 180 || v[2] < -90 || v[4] > 90) {
    cli::cli_abort(
      c("{.arg bbox} must lie within longitudes -180 to 180 and latitudes -90 to 90.",
        "i" = "For a box across the antimeridian give {.code xmin > xmax}, such as {.code c(179, -1, -179, 1)}."),
      call = call,
      class = c("a5px_error_geometry", "a5px_error_input", "a5px_error")
    )
  }
  if (v[1] == v[3] || v[2] >= v[4]) {
    cli::cli_abort(
      "{.arg bbox} must satisfy {.code xmin != xmax} and {.code ymin < ymax}.",
      call = call,
      class = c("a5px_error_geometry", "a5px_error_input", "a5px_error")
    )
  }
  if (v[1] > v[3]) {
    # across the antimeridian: the part east of xmin and the part west of xmax
    return(c(v[1], v[2], 180, v[4], -180, v[2], v[3], v[4]))
  }
  v
}

#' Flat `xmin, ymin, xmax, ymax, ...` boxes as a list of length-4 vectors.
#' @noRd
split_boxes <- function(v) {
  if (!length(v)) return(list())
  lapply(seq(1L, length(v), by = 4L), function(i) v[i:(i + 3L)])
}

#' The pieces of boxes `a` inside boxes `b` (lists of length-4 vectors).
#' @noRd
intersect_boxes <- function(a, b) {
  out <- list()
  for (x in a) for (y in b) {
    z <- c(max(x[1], y[1]), max(x[2], y[2]), min(x[3], y[3]), min(x[4], y[4]))
    if (z[1] < z[3] && z[2] < z[4]) out[[length(out) + 1L]] <- z
  }
  out
}

#' Boxes for the longitude range `xmin..xmax` (possibly past 180, as
#' `a5R::a5_bbox()` reports a polygon across the antimeridian) and
#' latitudes `ymin..ymax`, split at the antimeridian.
#' @noRd
range_boxes <- function(xmin, ymin, xmax, ymax) {
  if (xmax - xmin >= 360) return(list(c(-180, ymin, 180, ymax)))
  a <- ((xmin + 180) %% 360) - 180
  b <- a + (xmax - xmin)
  if (b <= 180) list(c(a, ymin, b, ymax))
  else list(c(a, ymin, 180, ymax), c(-180, ymin, b - 360, ymax))
}

#' Resolve `bbox_align` to the logical Rust flag. `"block"` needs a bbox to
#' align and only applies to the pixel-driven modes.
#' @noRd
check_bbox_align <- function(bbox_align, bbox_v, mode, call = rlang::caller_env()) {
  bbox_align <- rlang::arg_match(bbox_align, c("pixel", "block"), error_call = call)
  if (bbox_align == "pixel") {
    return(FALSE)
  }
  if (!length(bbox_v)) {
    cli::cli_abort(
      "{.code bbox_align = \"block\"} requires {.arg bbox}.",
      call = call
    )
  }
  if (identical(mode, "centroid")) {
    cli::cli_abort(
      "{.code bbox_align = \"block\"} does not apply to {.code mode = \"centroid\"}, which is cell-driven.",
      call = call
    )
  }
  TRUE
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
             "majority", "fractions", "npix")
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
                               scoff = FALSE,
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
  if ("npix" %in% stats && has_frac) {
    cli::cli_abort("{.val npix} cannot be combined with {.val fractions}.", call = call)
  }
  if (has_cat && (!is.null(dequant) || isTRUE(scoff))) {
    cli::cli_abort(
      "majority/fractions operate on raw integer codes and cannot be combined with {.arg dequant} or {.arg scoff}.",
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

#' Average A5 cell edge length in metres at `resolution` (a5R >= 0.6.0;
#' individual edges vary by roughly +/-10%). Shared by the overlay auto-k
#' rule and the overview target so both mean "cell edge" literally.
#' @noRd
a5_edge_metres <- function(resolution) {
  as.numeric(a5R::a5_cell_edge_length_avg(resolution, units = "m"))
}

#' A5 cell edge length in metres, passed to Rust for overlay auto-k.
#' 0 when overlay is off (the value is unused there).
#' @noRd
cell_edge_metres <- function(mode, resolution) {
  if (!identical(mode, "overlay")) {
    return(0)
  }
  a5_edge_metres(resolution)
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
  a5_edge_metres(resolution)
}

#' Normalise the user-facing `bands` arg into integer indices or character names.
#' Returns a list(idx = integer(), names = character()); at most one is non-empty.
#' @noRd
parse_bands_arg <- function(bands, call = rlang::caller_env()) {
  if (is.null(bands)) {
    return(list(idx = integer(), names = character()))
  }
  if (anyDuplicated(bands)) {
    cli::cli_abort(
      "{.arg bands} must not repeat a band; {.val {unique(bands[duplicated(bands)])}} appears more than once.",
      call = call
    )
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

#' Run a call into Rust and re-raise its error as a classed condition, so
#' callers (an HTTP service, say) can tell a bad source from bad input:
#' - `a5px_error_source`: the file or object store: missing, unreadable,
#'   an unsupported TIFF layout, or a network failure;
#' - `a5px_error_geometry` (also `a5px_error_input`): a bbox, AOI or CRS
#'   that cannot be used;
#' - `a5px_error_input`: other invalid arguments or sources, such as files
#'   that cannot be pooled;
#' - `a5px_error_interrupted`: the user interrupted the read;
#' - `a5px_error_internal`: a fault in a5px.
#' Every class also carries `a5px_error`.
#' @noRd
rs_call <- function(expr, call = rlang::caller_env()) {
  # once R holds the output, hand the memory the read freed back to the OS
  on.exit(a5px_release_memory_rs(), add = TRUE)
  tryCatch(expr, error = function(e) {
    msg <- conditionMessage(e)
    cls <- rs_error_class(msg)
    if (is.null(cls)) stop(e)
    rlang::abort(msg, class = c(cls, "a5px_error"), call = call)
  })
}

#' Condition classes for a message from the Rust error type, by its
#' prefix; `NULL` for anything else (an R error raised during the call).
#' @noRd
rs_error_class <- function(msg) {
  pre <- sub(":.*$", "", msg)
  switch(pre,
    "io error" = , "object_store error" = , "async-tiff error" = ,
    "missing geokey" = , "unsupported configuration" = ,
    "url parse error" = "a5px_error_source",
    "proj error" = c("a5px_error_geometry", "a5px_error_input"),
    "invalid input" = {
      if (grepl("file not found|not found:|no IFDs", msg)) {
        "a5px_error_source"
      } else if (grepl("bbox|reproject|footprint|geotransform", msg)) {
        c("a5px_error_geometry", "a5px_error_input")
      } else {
        "a5px_error_input"
      }
    },
    "interrupted" = "a5px_error_interrupted",
    "internal error" = , "worker task failed" = , "parquet/arrow error" = ,
    "a5 error" = "a5px_error_internal",
    NULL
  )
}

#' Provenance of a read, recorded in Arrow and Parquet metadata, so a file
#' says which A5 index its ids belong to and how its values were made.
#' `a5_index` is the key shared across the stack; the crate is the one a5px
#' was built with, which `check_a5r_compat()` holds to a5R's ids.
#' @noRd
read_provenance <- function(mode, dequant_v, overview_target_m, src_nodata_v) {
  decode <- if (isTRUE(dequant_v$scoff)) "scoff" else if (length(dequant_v$lut)) "dequant" else "none"
  c(
    a5_index = a5R::a5_info()$index,
    a5_crate = a5px_a5_crate_version_rs(),
    a5px_version = as.character(utils::packageVersion("a5px")),
    a5px_mode = mode,
    a5px_decode = decode,
    a5px_overviews = if (identical(mode, "centroid") || !(overview_target_m > 0)) "off" else "allowed",
    a5px_src_nodata = if (length(src_nodata_v)) format(src_nodata_v, digits = 17) else "file"
  )
}
