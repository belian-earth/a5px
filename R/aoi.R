#' Resolve the user-facing `aoi` / `containment` args into one A5 covering
#' (an `a5_covering` of length one, or NULL), its compacted cells for Rust
#' (`a5R::a5_covering_stored()`), and a WGS 84 envelope that drives tile
#' selection when no `bbox` is given. The envelope is the AOI's bounding box
#' padded by `aoi_pad_edges` cell edges, enough to hold every pixel of every
#' selected cell: under `"overlapping"` a boundary cell keeps its full
#' statistics. Padding the geometry costs nothing, where the cells' own
#' boundaries would need the covering expanded to full resolution.
#' @noRd
check_aoi <- function(aoi, resolution, containment, call = rlang::caller_env()) {
  containment <- rlang::arg_match(containment, c("centre", "overlapping"),
                                  error_call = call)
  empty <- list(cells = NULL, covering_stored = a5R::a5_cell(character()),
                tile_bbox = numeric(0))
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
    # one covering per feature; their union is the AOI
    a5R::a5_union(
      a5R::a5_polygon_to_cells(aoi, resolution = resolution, containment = containment)
    ),
    error = function(e) {
      cli::cli_abort(
        c("{.arg aoi} could not be converted to A5 cells.",
          "x" = conditionMessage(e)),
        call = call
      )
    }
  )
  if (a5R::a5_count(cells) == 0) {
    cli::cli_abort(
      "No A5 cells selected by {.arg aoi} at resolution {resolution} with {.code containment = \"{containment}\"}.",
      call = call
    )
  }
  stored <- a5R::a5_covering_stored(cells)
  if (any(a5R::a5_get_resolution(stored) < 0L)) {
    # the world cell: an AOI covering the globe restricts nothing
    return(empty)
  }
  list(
    cells = cells,
    covering_stored = stored,
    tile_bbox = aoi_tile_bbox(aoi, resolution)
  )
}

#' Cell edges of padding around the AOI's bounding box. A cell's vertices lie
#' at most 1.12 average edge lengths from its centre (measured on 20,000
#' random cells at each of resolutions 6, 10, 13 and 18), so a cell that
#' overlaps the AOI reaches at most 2.24 edges beyond it.
#' @noRd
aoi_pad_edges <- 3

#' WGS 84 bounding box of `aoi`, padded by `aoi_pad_edges` cell edges at
#' `resolution` and clamped to the globe.
#' @noRd
aoi_tile_bbox <- function(aoi, resolution) {
  bb <- unclass(wk::wk_bbox(aoi))
  pad_m <- aoi_pad_edges * a5_edge_metres(resolution)
  dlat <- pad_m / 111195 # metres per degree of latitude, authalic sphere
  ymin <- max(bb$ymin - dlat, -90)
  ymax <- min(bb$ymax + dlat, 90)
  coslat <- cos(max(abs(ymin), abs(ymax)) * pi / 180)
  if (coslat < 1e-6) {
    return(c(-180, ymin, 180, ymax))
  }
  dlon <- dlat / coslat
  c(max(bb$xmin - dlon, -180), ymin, min(bb$xmax + dlon, 180), ymax)
}
