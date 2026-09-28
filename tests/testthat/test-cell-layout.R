# a5R >= 0.6.0.9001 stores b8 XOR 0xFC; NA is the full id 0xFC00000000000000.

tiny_tif_at <- function(lon, lat) {
  r <- terra::rast(xmin = lon - 1e-5, xmax = lon + 1e-5,
                   ymin = lat - 1e-5, ymax = lat + 1e-5,
                   nrows = 4, ncols = 4, crs = "EPSG:4326", vals = 1:16)
  f <- withr::local_tempfile(fileext = ".tif", .local_envir = parent.frame())
  terra::writeRaster(r, f, gdal = "TILED=YES")
  f
}

test_that("known cell ids round-trip through the Rust cell encoding", {
  f <- system.file("extdata", "overview_cog.tif", package = "a5px")
  skip_if(f == "")
  cells <- a5_read_raster(f, 12L, use_overviews = FALSE)$cell
  aoi <- a5R::a5_cell_to_parent(cells[1], resolution = 10L)
  expected <- a5R::a5_uncompact(
    a5R::a5_polygon_to_cells(aoi, 12L, containment = "overlapping"), 12L
  )
  out <- a5_read_raster(f, 12L, mode = "centroid", aoi = aoi,
                        containment = "overlapping")
  expect_gt(nrow(out), 0L)
  expect_true(all(format(out$cell) %in% format(expected)))
})

test_that("res-30 cells whose top byte is 0xFC are not read as NA", {
  id <- a5R::a5_cell("fc9529c9c837d6e7")
  ll <- a5R::a5_cell_to_lonlat(id, as_dataframe = TRUE)
  f <- tiny_tif_at(ll$lon, ll$lat)
  out <- a5_read_raster(f, 30L, mode = "centroid", aoi = id,
                        containment = "overlapping")
  expect_true("fc9529c9c837d6e7" %in% format(out$cell))
  expect_false(anyNA(out$cell))
  expect_true(all(startsWith(format(out$cell), "fc")))
})
