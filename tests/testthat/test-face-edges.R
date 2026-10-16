# Pixels near A5 face and quintant edges go to the cell a5R gives their
# centre. The sites lie on the edges of resolution-0 cells (faces) and at
# the corners of resolution-1 cells (quintants).

edge_sites <- function() {
  vertices <- function(cells) {
    xy <- wk::wk_coords(a5R::a5_cell_to_boundary(cells))
    xy <- unique(round(xy[, c("x", "y")], 6))
    # keep away from the poles, where a lon/lat raster is degenerate
    xy[abs(xy$y) < 85, ]
  }
  res0 <- a5R::a5_get_res0_cells()
  # points along face edges (the boundaries are densified), and quintant
  # corners
  faces <- vertices(res0)
  quintants <- vertices(a5R::a5_cell_to_children(res0[c(1, 2, 7, 12)], 1L))
  pick <- function(xy, n) xy[round(seq(1, nrow(xy), length.out = n)), ]
  rbind(pick(faces, 10L), pick(quintants, 8L))
}

site_tif <- function(lon, lat, half = 0.01, n = 40L) {
  r <- terra::rast(nrows = n, ncols = n, crs = "EPSG:4326",
                   extent = terra::ext(lon - half, lon + half, lat - half, lat + half))
  terra::values(r) <- seq_len(n * n)
  names(r) <- "v"
  f <- withr::local_tempfile(fileext = ".tif", .local_envir = parent.frame())
  terra::writeRaster(r, f, gdal = c("TILED=YES", "BLOCKXSIZE=16", "BLOCKYSIZE=16"))
  list(f = f, r = r)
}

test_that("cells at face and quintant edges match a5R pixel by pixel", {
  skip_if_not_installed("terra")
  sites <- edge_sites()
  expect_gte(nrow(sites), 12L)
  # the sites straddle faces as well as quintants
  faces_at <- function(i) {
    g <- expand.grid(x = sites$x[i] + c(-0.009, 0.009), y = sites$y[i] + c(-0.009, 0.009))
    length(unique(a5R::a5_u64_to_hex(a5R::a5_lonlat_to_cell(g$x, g$y, 0L))))
  }
  expect_gte(sum(vapply(seq_len(nrow(sites)), faces_at, 1L) > 1L), 3L)
  for (i in seq_len(nrow(sites))) {
    s <- site_tif(sites$x[i], sites$y[i])
    xy <- terra::xyFromCell(s$r, seq_len(terra::ncell(s$r)))
    v <- terra::values(s$r)[, 1]
    for (res in c(12L, 14L)) {
      truth <- a5R::a5_u64_to_hex(a5R::a5_lonlat_to_cell(xy[, 1], xy[, 2], res))
      want <- data.frame(key = names(table(truth)),
                         count = as.numeric(table(truth)),
                         sum = as.numeric(tapply(v, truth, sum)))
      got <- a5_read_raster(s$f, res, stat = c("sum", "count"), use_overviews = FALSE)
      got <- data.frame(key = a5R::a5_u64_to_hex(got$cell), sum = got$v_sum, count = got$v_count)
      got <- got[order(got$key), ]
      info <- sprintf("site %.4f, %.4f at %d", sites$x[i], sites$y[i], res)
      expect_identical(got$key, want$key, info = info)
      expect_equal(got$count, want$count, info = info)
      expect_equal(got$sum, want$sum, info = info)
    }
  }
})
