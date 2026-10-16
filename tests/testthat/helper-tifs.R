# Rasters for the reader tests. The Sentinel-2 extract they were written
# against (test-tifs/exe_cog.tif) is not shipped; without it, a stand-in
# with the same layout is generated once per session: 361 x 342 pixels in
# British National Grid, five uint16 bands named B02, B03, B04, B08 and
# SCL, nodata 0, LZW-compressed 512-pixel blocks. test_non_geo.tif is a
# TIFF without georeferencing.

tif_dir <- function() {
  dev <- normalizePath(file.path(testthat::test_path(), "..", "..", "test-tifs"),
                       mustWork = FALSE)
  if (file.exists(file.path(dev, "exe_cog.tif"))) {
    return(dev)
  }
  gen <- file.path(tempdir(), "a5px-test-tifs")
  if (!file.exists(file.path(gen, "exe_cog.tif"))) {
    make_test_tifs(gen)
  }
  gen
}

skip_no_tifs <- function() {
  if (!file.exists(file.path(testthat::test_path(), "..", "..", "test-tifs", "exe_cog.tif"))) {
    testthat::skip_if_not_installed("terra")
  }
}

make_test_tifs <- function(dir) {
  dir.create(dir, showWarnings = FALSE, recursive = TRUE)
  set.seed(42)
  nc <- 361L
  nr <- 342L
  r <- terra::rast(nrows = nr, ncols = nc, nlyrs = 5, crs = "EPSG:27700",
                   extent = terra::ext(290000, 290000 + nc * 20, 88000, 88000 + nr * 20))
  col <- rep(seq_len(nc), times = nr)
  row <- rep(seq_len(nr), each = nc)
  base <- 400 + 3 * col + 2 * row
  bands <- sapply(c(1, 1.2, 1.1, 2.5), function(k) round(k * base + stats::runif(nc * nr, 0, 50)))
  scl <- 4 + (col %/% 60 + row %/% 60) %% 4
  vals <- cbind(bands, scl)
  # a no-data corner, as at a swath edge
  vals[col + row < 60, ] <- 0
  terra::values(r) <- vals
  names(r) <- c("B02", "B03", "B04", "B08", "SCL")
  terra::writeRaster(r, file.path(dir, "exe_cog.tif"), datatype = "INT2U", NAflag = 0,
                     gdal = c("TILED=YES", "BLOCKXSIZE=512", "BLOCKYSIZE=512", "COMPRESS=LZW"))
  plain <- terra::rast(nrows = 64, ncols = 64, crs = "", extent = terra::ext(0, 64, 0, 64),
                       vals = seq_len(64 * 64))
  terra::writeRaster(plain, file.path(dir, "test_non_geo.tif"), datatype = "INT2U",
                     gdal = c("TILED=YES", "BLOCKXSIZE=16", "BLOCKYSIZE=16"))
  invisible(dir)
}
