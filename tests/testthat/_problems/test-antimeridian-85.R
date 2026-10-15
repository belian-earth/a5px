# Extracted from test-antimeridian.R:85

# setup ------------------------------------------------------------------------
library(testthat)
test_env <- simulate_test_env(package = "a5px", path = "..")
attach(test_env, warn.conflicts = FALSE)

# prequel ----------------------------------------------------------------------
hexkey <- function(cells) a5R::a5_u64_to_hex(cells)
ord <- function(df) df[order(hexkey(df$cell)), , drop = FALSE]
geo_tif <- function(path, ext, res) {
  r <- terra::rast(crs = "EPSG:4326", resolution = res,
                   extent = terra::ext(ext[1], ext[2], ext[3], ext[4]))
  xy <- terra::xyFromCell(r, seq_len(terra::ncell(r)))
  lon <- ((xy[, 1] + 180) %% 360) - 180
  terra::values(r) <- round(cos(lon * pi / 180) * 100 + xy[, 2], 6)
  names(r) <- "v"
  terra::writeRaster(r, path, overwrite = TRUE,
                     gdal = c("TILED=YES", "BLOCKXSIZE=64", "BLOCKYSIZE=64"))
  path
}

# test -------------------------------------------------------------------------
skip_if_not_installed("terra")
r <- terra::rast(nrows = 300, ncols = 400, crs = "EPSG:32760",
                   extent = terra::ext(800000, 840000, 8200000, 8230000))
terra::values(r) <- 1
names(r) <- "v"
f <- withr::local_tempfile(fileext = ".tif")
terra::writeRaster(r, f, gdal = c("TILED=YES", "BLOCKXSIZE=64", "BLOCKYSIZE=64"))
bb <- a5_raster_info(f)$bbox
expect_lt(bb[1], 180)
expect_gt(bb[3], 180)
expect_lt(bb[3] - bb[1], 1)
t <- system.time(got <- a5_read_raster(f, 14L, mode = "centroid"))[["elapsed"]]
lon <- a5R::a5_cell_to_lonlat(got$cell, as_dataframe = TRUE)$lon
expect_true(any(lon > 179) && any(lon < -179))
expect_lt(t, 5)
whole <- sum(a5_read_raster(f, 12L, stat = "count")$v_count)
east <- sum(a5_read_raster(f, 12L, stat = "count", bbox = c(170, -20, 180, -10))$v_count)
west <- sum(a5_read_raster(f, 12L, stat = "count", bbox = c(-180, -20, -170, -10))$v_count)
expect_equal(whole, 300 * 400)
