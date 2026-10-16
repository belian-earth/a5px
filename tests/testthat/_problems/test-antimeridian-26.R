# Extracted from test-antimeridian.R:26

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
dir <- withr::local_tempdir()
f <- geo_tif(file.path(dir, "g360.tif"), c(0, 360, -90, 90), 1)
n <- function(bb) sum(a5_read_raster(f, 3L, stat = "count", bbox = bb)$v_count)
expect_equal(n(c(-180, -90, 0, 90)), 180 * 180)
