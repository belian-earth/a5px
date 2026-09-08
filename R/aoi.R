#' Resolve the user-facing `aoi` / `containment` args into the compacted A5
#' cell set (an `a5_cell` vector, or NULL), its raw form for Rust, and the
#' WGS 84 envelope of those cells' boundaries, which drives tile selection
#' when no `bbox` is given. The envelope of the cell boundaries, not of the
#' polygon, so that under `"overlapping"` every pixel of a boundary cell is
#' fetched and each included cell keeps its full statistics.
#' @noRd
check_aoi <- function(aoi, resolution, containment, call = rlang::caller_env()) {
  containment <- rlang::arg_match(containment, c("centre", "overlapping"),
                                  error_call = call)
  empty <- list(cells = NULL, cells_raw = list(), tile_bbox = numeric(0))
  if (is.null(aoi)) {
    if (containment != "centre") {
      cli::cli_abort(
        "{.arg containment} is only used together with {.arg aoi}.",
        call = call
      )
    }
    return(empty)
  }
  cells <- tryCatch(
    a5R::a5_polygon_to_cells(aoi, resolution = resolution, containment = containment),
    error = function(e) {
      cli::cli_abort(
        c("{.arg aoi} could not be converted to A5 cells.",
          "x" = conditionMessage(e)),
        call = call
      )
    }
  )
  if (length(cells) == 0L) {
    cli::cli_abort(
      "No A5 cells selected by {.arg aoi} at resolution {resolution} with {.code containment = \"{containment}\"}.",
      call = call
    )
  }
  bb <- unclass(wk::wk_bbox(a5R::a5_cell_to_boundary(cells)))
  eps <- 1e-9
  tile_bbox <- c(bb$xmin - eps, bb$ymin - eps, bb$xmax + eps, bb$ymax + eps)
  list(cells = cells, cells_raw = vctrs::vec_data(cells), tile_bbox = tile_bbox)
}
