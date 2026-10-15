# nocov start
.onLoad <- function(libname, pkgname) {
  check_a5r_compat()
  invisible(NULL)
}
# nocov end

#' Stop unless a5R's C API is one a5px understands and a5px's a5 crate
#' gives the same cell ids as a5R's. Cells cross between the two packages
#' as raw ids, so a mismatch (an a5R built on another A5 index version, or
#' a C API that changed) would give wrong cells without an error.
#' @noRd
check_a5r_compat <- function() {
  api <- a5px_a5r_api_version_rs()
  if (api != 1L) {
    stop(sprintf(
      "a5px %s needs version 1 of a5R's C API, but a5R %s provides version %d; install matching versions.",
      utils::packageVersion("a5px"), utils::packageVersion("a5R"), api
    ), call. = FALSE)
  }
  # points on faces, near a vertex, on the antimeridian and near a pole
  lon <- c(0, 98.8, -50.8, 179.99, -120, 31.72)
  lat <- c(0, 22.1, -0.6, -89.9, 60, 58.28)
  for (res in c(1L, 10L, 18L, 29L)) {
    ours <- a5px_lonlat_to_cells_rs(lon, lat, res)
    theirs <- a5R::a5_lonlat_to_cell(lon, lat, res)
    if (!identical(a5R::a5_u64_to_hex(ours), a5R::a5_u64_to_hex(theirs))) {
      info <- a5R::a5_info()
      stop(sprintf(
        "a5px %s and a5R %s (a5 crate %s) give different A5 cell ids at resolution %d; install matching versions.",
        utils::packageVersion("a5px"), info$version, info$crate, res
      ), call. = FALSE)
    }
  }
}
