# compare a5px against the two natural terra pipelines for the same job:
# aggregate every pixel of a Sentinel-2 COG into A5 cells (mean per band).
# Run after `R CMD INSTALL --no-test-load --no-docs .`

suppressMessages({
  library(a5px); library(a5R); library(terra); library(dplyr); library(tibble)
})

PATH <- "test-tifs/test_cog.tif"
RES  <- 16L
stopifnot(file.exists(PATH))

time_it <- function(label, expr) {
  t0 <- Sys.time()
  val <- force(expr)
  dt <- as.numeric(Sys.time() - t0, units = "secs")
  cat(sprintf("[%-26s] %7.2f s\n", label, dt))
  invisible(list(label = label, t = dt, val = val))
}

# ---------------------------------------------------------------- a5px -----
cat("\n=== a5px ===\n")
a5px_set_threads(8)
a5_out <- time_it("a5px mean (8 thr)", {
  a5_read_raster(PATH, resolution = RES, stat = "mean",
                 threads = 8L, io_concurrency = 16L)
})$val
cat(sprintf("  cells: %d   bands: %d\n", nrow(a5_out), ncol(a5_out) - 1L))

# ----------------------- terra: forward pipeline (R-side, no polygons) -----
cat("\n=== terra forward (project pixels then aggregate) ===\n")
r <- rast(PATH)
n_bands <- nlyr(r)

terra_fwd <- time_it("terra forward total", {
  pix_xy <- xyFromCell(r, 1:ncell(r))
  ll <- project(pix_xy, from = crs(r), to = "EPSG:4326")
  cells <- a5R::a5_lonlat_to_cell(ll[, 1], ll[, 2], resolution = RES)
  vals <- values(r)  # matrix [npix x nbands]
  # mask dataset-level nodata = 0
  vals[vals == 0] <- NA_real_
  df <- tibble(cell = a5R::a5_u64_to_hex(cells), as_tibble(vals))
  out <- df |>
    group_by(cell) |>
    summarise(across(everything(), \(x) mean(x, na.rm = TRUE)), .groups = "drop")
  out
})$val
cat(sprintf("  cells: %d\n", nrow(terra_fwd)))

# --------- terra: zonal pipeline (a5 cells -> polygons -> terra::extract) ----
# This is the "obvious" pre-a5px approach. Often slow because of polygon
# rasterisation cost. Skip if expected runtime is unreasonable.
RUN_ZONAL <- TRUE
if (RUN_ZONAL) {
  cat("\n=== terra zonal (a5 polygons -> extract) ===\n")
  t_zonal <- time_it("terra zonal total", {
    bbox_ll <- ext(project(r, "EPSG:4326"))
    grid <- a5R::a5_uncompact(
      a5R::a5_polygon_to_cells(
        wk::rct(bbox_ll$xmin, bbox_ll$ymin, bbox_ll$xmax, bbox_ll$ymax),
        resolution = RES
      ),
      resolution = RES
    )
    bnd_wkt <- a5R::a5_cell_to_boundary(grid, format = "wkt")
    polys <- vect(as.character(bnd_wkt), crs = "EPSG:4326") |>
      project(crs(r))
    # extract per-cell mean for each band
    ex <- extract(r, polys, fun = mean, na.rm = TRUE)
    tibble(cell = a5R::a5_u64_to_hex(grid)) |>
      bind_cols(as_tibble(ex[, -1L, drop = FALSE]))
  })
}
