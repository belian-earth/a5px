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
  # a5-js spells it "center"
  if (identical(containment, "center")) containment <- "centre"
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
        call = call,
        class = c("a5px_error_geometry", "a5px_error_input", "a5px_error")
      )
    }
  )
  if (a5R::a5_count(cells) == 0) {
    cli::cli_abort(
      "No A5 cells selected by {.arg aoi} at resolution {resolution} with {.code containment = \"{containment}\"}.",
      call = call,
      class = c("a5px_error_geometry", "a5px_error_input", "a5px_error")
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

#' WGS 84 boxes around `aoi`, padded by `aoi_pad_edges` cell edges at
#' `resolution`, flattened for Rust (`xmin, ymin, xmax, ymax, ...`).
#' Polygon edges are great circles, as in `a5R::a5_polygon_to_cells()`, and
#' bulge poleward past their vertices (an edge along 60 N peaks at 63.4 N),
#' so each feature's box is `a5R::a5_bbox()`, not its vertices' envelope.
#' A feature across the antimeridian gives two boxes, one each side.
#' @noRd
aoi_tile_bbox <- function(aoi, resolution) {
  bb <- if (inherits(aoi, "a5_cell")) {
    # cells: their own edges' bulge is well inside the padding
    unclass(wk::wk_bbox(aoi))
  } else {
    unclass(a5R::a5_bbox(aoi))
  }
  pad_boxes(bb, aoi_pad_edges * a5_edge_metres(resolution) / 111195) # metres per degree, authalic sphere
}

#' The boxes of `bb` (a5_bbox() or wk_bbox() fields) padded by `dlat`
#' degrees of latitude and the same distance in longitude, flattened.
#' @noRd
pad_boxes <- function(bb, dlat) {
  boxes <- list()
  for (k in seq_along(bb$xmin)) {
    if (anyNA(c(bb$xmin[k], bb$ymin[k], bb$xmax[k], bb$ymax[k]))) next
    ymin <- max(bb$ymin[k] - dlat, -90)
    ymax <- min(bb$ymax[k] + dlat, 90)
    coslat <- cos(max(abs(ymin), abs(ymax)) * pi / 180)
    dlon <- if (dlat == 0) 0 else if (coslat < 1e-6) 360 else dlat / coslat
    boxes <- c(boxes, range_boxes(bb$xmin[k] - dlon, ymin, bb$xmax[k] + dlon, ymax))
  }
  unlist(unique(boxes))
}

#' Resolve the user-facing `mask` into rings for Rust and the WGS 84 boxes
#' that hold it. `xy` interleaves lon and lat; `rings` interleaves, for each
#' ring, the polygon it belongs to and its vertex count. The first ring of
#' a polygon is its outer ring and the rest are holes.
#' @noRd
check_mask <- function(mask, mode, call = rlang::caller_env()) {
  empty <- list(xy = numeric(0), rings = integer(0), tile_bbox = numeric(0))
  if (is.null(mask)) {
    return(empty)
  }
  if (identical(mode, "centroid")) {
    cli::cli_abort(
      c("{.arg mask} is not used in {.code mode = \"centroid\"}, which samples one point per cell.",
        "i" = "Use {.arg aoi} to select the cells."),
      call = call
    )
  }
  if (inherits(mask, "a5_cell")) {
    cli::cli_abort(
      c("{.arg mask} must be polygons, not A5 cells.",
        "i" = "{.arg aoi} with cells keeps exactly the pixels whose centre lies in them."),
      call = call
    )
  }
  geom_error <- function(e) {
    cli::cli_abort(
      c("{.arg mask} could not be read as polygons.", "x" = conditionMessage(e)),
      call = call,
      class = c("a5px_error_geometry", "a5px_error_input", "a5px_error")
    )
  }
  if (inherits(mask, "SpatVector")) {
    rlang::check_installed("terra", reason = "to read a terra SpatVector `mask`.")
    if (!isTRUE(terra::is.lonlat(mask, perhaps = TRUE, warn = FALSE))) {
      geom_error(simpleError("`mask` must have longitude/latitude coordinates."))
    }
    mask <- wk::wkt(terra::geom(mask, wkt = TRUE))
  }
  # a5_bbox() checks the CRS, the geometry types and the coordinate ranges,
  # and bounds each feature with its edges' poleward bulge
  bb <- tryCatch(unclass(a5R::a5_bbox(mask)), error = geom_error)
  if (is.matrix(mask) || (is.data.frame(mask) && !inherits(mask, "sf"))) {
    m <- if (is.matrix(mask)) mask else as.matrix(mask[c("lon", "lat")])
    mask <- wk::wk_polygon(wk::xy(as.numeric(m[, 1]), as.numeric(m[, 2])))
  }
  coords <- tryCatch(wk::wk_coords(mask), error = geom_error)
  if (nrow(coords) == 0L) {
    geom_error(simpleError("`mask` contains no polygon rings."))
  }
  # wk_coords() lists each ring's vertices together, in order
  step <- function(v) c(TRUE, v[-1L] != v[-length(v)])
  new_part <- step(coords$feature_id) | step(coords$part_id)
  new_ring <- new_part | step(coords$ring_id)
  ring <- cumsum(new_ring)
  part <- cumsum(new_part)[new_ring]
  list(
    xy = as.vector(rbind(as.numeric(coords$x), as.numeric(coords$y))),
    rings = as.vector(rbind(part, tabulate(ring, max(ring)))),
    tile_bbox = pad_boxes(bb, 0)
  )
}

#' The boxes for tile selection: the mask's, intersected with the AOI's
#' when both are given. Only pixels inside the mask contribute, so its
#' boxes suffice.
#' @noRd
tile_boxes <- function(aoi_boxes, mask_boxes) {
  if (!length(mask_boxes)) return(aoi_boxes)
  if (!length(aoi_boxes)) return(mask_boxes)
  a <- matrix(aoi_boxes, nrow = 4)
  m <- matrix(mask_boxes, nrow = 4)
  out <- list()
  for (i in seq_len(ncol(a))) {
    for (j in seq_len(ncol(m))) {
      b <- c(max(a[1, i], m[1, j]), max(a[2, i], m[2, j]),
             min(a[3, i], m[3, j]), min(a[4, i], m[4, j]))
      if (b[1] <= b[3] && b[2] <= b[4]) out <- c(out, list(b))
    }
  }
  # disjoint: every pixel fails one test or the other
  if (!length(out)) return(mask_boxes)
  unlist(out)
}
