# Rasters, bboxes and AOIs on or across the antimeridian, and rasters stored
# in longitudes 0 to 360.

hexkey <- function(cells) a5R::a5_u64_to_hex(cells)
ord <- function(df) df[order(hexkey(df$cell)), , drop = FALSE]

# A geographic raster over `ext` whose values depend only on position, so
# the same place gives the same value whichever longitudes store it.
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

test_that("a raster stored in longitudes 0 to 360 meets bboxes west of 0", {
  skip_if_not_installed("terra")
  dir <- withr::local_tempdir()
  f <- geo_tif(file.path(dir, "g360.tif"), c(0, 360, -90, 90), 1)
  n <- function(bb) sum(a5_read_raster(f, 3L, stat = "count", bbox = bb)$v)
  expect_equal(n(c(-180, -90, 0, 90)), 180 * 180)
  expect_equal(n(c(0, -90, 180, 90)), 180 * 180)
  expect_equal(n(c(-180, -90, 180, 90)), 360 * 180)
  # it reads as the same raster stored in -180 to 180
  g <- geo_tif(file.path(dir, "g180.tif"), c(-180, 180, -90, 90), 1)
  expect_equal(ord(a5_read_raster(f, 3L, stat = c("sum", "count"))),
               ord(a5_read_raster(g, 3L, stat = c("sum", "count"))))
})

test_that("centroid mode samples every cell of a 0 to 360 raster", {
  skip_if_not_installed("terra")
  dir <- withr::local_tempdir()
  f <- geo_tif(file.path(dir, "g360.tif"), c(0, 360, -90, 90), 1)
  g <- geo_tif(file.path(dir, "g180.tif"), c(-180, 180, -90, 90), 1)
  a <- ord(a5_read_raster(f, 4L, mode = "centroid"))
  b <- ord(a5_read_raster(g, 4L, mode = "centroid"))
  expect_equal(hexkey(a$cell), hexkey(b$cell))
  expect_equal(a$v, b$v)
  expect_true(any(a5R::a5_cell_to_lonlat(a$cell, as_dataframe = TRUE)$lon < 0))
})

test_that("bbox chunks either side of the antimeridian partition a raster across it", {
  skip_if_not_installed("terra")
  f <- geo_tif(withr::local_tempfile(fileext = ".tif"), c(179.8, 180.2, -0.15, 0.15), 0.001)
  n_px <- 400 * 300
  whole <- sum(a5_read_raster(f, 12L, stat = "count")$v)
  east <- sum(a5_read_raster(f, 12L, stat = "count", bbox = c(179.8, -1, 180, 1))$v)
  west <- sum(a5_read_raster(f, 12L, stat = "count", bbox = c(-180, -1, -179.8, 1))$v)
  expect_equal(whole, n_px)
  expect_equal(east + west, n_px)
  expect_gt(east, 0)
  expect_gt(west, 0)
  # a bbox across the seam, xmin > xmax
  across <- sum(a5_read_raster(f, 12L, stat = "count", bbox = c(179.9, -1, -179.9, 1))$v)
  expect_equal(across, n_px / 2)
})

test_that("a projected raster across the antimeridian keeps a tight footprint", {
  skip_if_not_installed("terra")
  # UTM 60S (central meridian 177 E) at about 16 S: 180 E is near x = 820 km
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
  # centroid candidates stay on the raster: a few thousand cells, not a band
  t <- system.time(got <- a5_read_raster(f, 14L, mode = "centroid"))[["elapsed"]]
  lon <- a5R::a5_cell_to_lonlat(got$cell, as_dataframe = TRUE)$lon
  expect_true(any(lon > 179) && any(lon < -179))
  expect_lt(t, 5)
  # forward: chunks east and west of the seam add up to the whole
  whole <- sum(a5_read_raster(f, 12L, stat = "count")$v)
  east <- sum(a5_read_raster(f, 12L, stat = "count", bbox = c(170, -20, 180, -10))$v)
  west <- sum(a5_read_raster(f, 12L, stat = "count", bbox = c(-180, -20, -170, -10))$v)
  expect_equal(whole, 300 * 400)
  expect_equal(east + west, whole)
})

test_that("an AOI across the antimeridian reads both sides", {
  skip_if_not_installed("terra")
  f <- geo_tif(withr::local_tempfile(fileext = ".tif"), c(179.8, 180.2, -0.15, 0.15), 0.001)
  aoi <- wk::wkt("POLYGON((179.85 -0.1, 180.15 -0.1, 180.15 0.1, 179.85 0.1, 179.85 -0.1))",
                 crs = wk::wk_crs_longlat())
  got <- a5_read_raster(f, 13L, aoi = aoi, stat = "count")
  covering <- a5R::a5_uncompact(a5R::a5_polygon_to_cells(aoi, resolution = 13L), resolution = 13L)
  expect_setequal(hexkey(got$cell), hexkey(covering))
  ce <- a5_read_raster(f, 15L, aoi = aoi, mode = "centroid")
  lon <- a5R::a5_cell_to_lonlat(ce$cell, as_dataframe = TRUE)$lon
  expect_true(any(lon > 179) && any(lon < -179))
})

test_that("centroid mode with an AOI larger than the raster enumerates only the raster", {
  skip_if_not_installed("terra")
  f <- geo_tif(withr::local_tempfile(fileext = ".tif"), c(10, 10.04, 45, 45.04), 0.0005)
  aoi <- wk::rct(9, 44, 11, 46, crs = wk::wk_crs_longlat())
  t <- system.time(got <- a5_read_raster(f, 18L, aoi = aoi, mode = "centroid"))[["elapsed"]]
  direct <- a5_read_raster(f, 18L, mode = "centroid")
  expect_setequal(hexkey(got$cell), hexkey(direct$cell))
  expect_lt(t, 10)
})
