# Extracted from test-antimeridian.R:55

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
f <- geo_tif(withr::local_tempfile(fileext = ".tif"), c(179.8, 180.2, -0.15, 0.15), 0.001)
n_px <- 400 * 300
whole <- sum(a5_read_raster(f, 12L, stat = "count")$v_count)
east <- sum(a5_read_raster(f, 12L, stat = "count", bbox = c(179.8, -1, 180, 1))$v_count)
west <- sum(a5_read_raster(f, 12L, stat = "count", bbox = c(-180, -1, -179.8, 1))$v_count)
expect_equal(whole, n_px)
expect_equal(east + west, n_px)
