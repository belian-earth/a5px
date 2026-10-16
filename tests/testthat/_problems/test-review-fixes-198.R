# Extracted from test-review-fixes.R:198

# setup ------------------------------------------------------------------------
library(testthat)
test_env <- simulate_test_env(package = "a5px", path = "..")
attach(test_env, warn.conflicts = FALSE)

# prequel ----------------------------------------------------------------------
ext <- function(name) system.file("extdata", name, package = "a5px")
hexkey <- function(cells) a5R::a5_u64_to_hex(cells)
ord <- function(df) df[order(hexkey(df$cell)), , drop = FALSE]

# test -------------------------------------------------------------------------
skip_if_not_installed("terra")
skip_if_not_installed("sf")
dir <- withr::local_tempdir()
r <- terra::rast(nrows = 64, ncols = 64, crs = "EPSG:32633",
                   extent = terra::ext(400000, 406400, 1000000, 1006400), nlyrs = 2)
terra::values(r) <- cbind(seq_len(64 * 64), rep(c(255, 0), each = 32, length.out = 64 * 64))
src <- file.path(dir, "src.tif")
terra::writeRaster(r, src, datatype = "INT2U", gdal = c("TILED=YES", "BLOCKXSIZE=16", "BLOCKYSIZE=16"))
masked <- file.path(dir, "masked.tif")
sf::gdal_utils("translate", src, masked,
                 options = c("-b", "1", "-mask", "2", "-co", "TILED=YES",
                             "-co", "BLOCKXSIZE=16", "-co", "BLOCKYSIZE=16"),
                 config_options = c(GDAL_TIFF_INTERNAL_MASK = "YES"))
valid <- sum(!is.na(terra::values(terra::rast(masked))))
expect_equal(valid, 64 * 64 / 2)
