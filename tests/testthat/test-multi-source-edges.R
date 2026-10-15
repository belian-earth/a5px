# Multi-source cases that the fixtures in test-multi-source.R share by
# construction: overview layout, scale and offset, distant sources, bboxes
# far larger than a raster, chunk edges and partly named sources.

hexkey <- function(cells) a5R::a5_u64_to_hex(cells)
ord <- function(df) df[order(hexkey(df$cell)), , drop = FALSE]

# A constant raster of `n` x `n` pixels written as a tiled GeoTIFF, or with
# `cog = TRUE` as a COG, which carries overviews.
const_tif <- function(value, path, crs = "EPSG:32633", ext = c(400000, 420480, 1000000, 1020480),
                      n = 2048L, cog = FALSE) {
  r <- terra::rast(nrows = n, ncols = n, crs = crs,
                   extent = terra::ext(ext[1], ext[2], ext[3], ext[4]))
  terra::values(r) <- value
  names(r) <- "v"
  if (cog) {
    terra::writeRaster(r, path, filetype = "COG", overwrite = TRUE,
                       gdal = c("BLOCKSIZE=256", "OVERVIEWS=AUTO"))
  } else {
    terra::writeRaster(r, path, overwrite = TRUE,
                       gdal = c("TILED=YES", "BLOCKXSIZE=256", "BLOCKYSIZE=256"))
  }
  path
}

test_that("pooled sources read one overview pixel size when only some have overviews", {
  skip_if_not_installed("terra")
  dir <- withr::local_tempdir()
  ov <- const_tif(1, file.path(dir, "ov.tif"), cog = TRUE)
  plain <- const_tif(3, file.path(dir, "plain.tif"))
  expect_gt(nrow(a5_raster_info(ov)$overviews), 0L)
  expect_equal(nrow(a5_raster_info(plain)$overviews), 0L)

  pooled <- a5_read_raster(c(ov, plain), 11L)
  expect_equal(unique(pooled$v), 2)
  comp <- a5_read_raster(c(ov, plain), 11L, composite = "mean")
  expect_equal(unique(comp$v), 2)
})

test_that("pooled sources that all have overviews still read them", {
  skip_if_not_installed("terra")
  dir <- withr::local_tempdir()
  withr::local_seed(1)
  noise <- stats::runif(2048L * 2048L)
  a <- const_tif(noise, file.path(dir, "a.tif"), cog = TRUE)
  b <- const_tif(noise, file.path(dir, "b.tif"), cog = TRUE)
  ov <- ord(a5_read_raster(a, 11L))
  full <- ord(a5_read_raster(a, 11L, use_overviews = FALSE))
  # an overview read differs from full resolution in the last digits
  expect_false(isTRUE(all.equal(ov$v, full$v, tolerance = 1e-12)))
  # two copies pool to the single read at the same level
  expect_equal(ord(a5_read_raster(c(a, b), 11L))$v, ov$v)
})

test_that("a bbox far larger than a transverse Mercator raster selects all of it", {
  skip_if_not_installed("terra")
  dir <- withr::local_tempdir()
  # beside the central meridian's column (x = 500 km), and straddling it
  beside <- const_tif(1, file.path(dir, "beside.tif"), n = 256L,
                      ext = c(400000, 420480, 1000000, 1020480))
  across <- const_tif(1, file.path(dir, "across.tif"), n = 256L,
                      ext = c(490000, 510480, 1000000, 1020480))
  for (f in c(beside, across)) {
    all <- a5_read_raster(f, 13L, stat = "count")
    for (bb in list(c(-180, -90, 180, 90), c(-179, -90, 179, 90), c(-180, -89, 180, 89))) {
      got <- a5_read_raster(f, 13L, stat = "count", bbox = bb)
      expect_equal(ord(got), ord(all), ignore_attr = TRUE)
    }
  }
  # every source of a pooled read, too
  both <- a5_read_raster(c(beside, across), 13L, stat = "count")
  got <- a5_read_raster(c(beside, across), 13L, stat = "count", bbox = c(-180, -90, 180, 90))
  expect_equal(ord(got), ord(both), ignore_attr = TRUE)
})

test_that("pooled sources must share scale and offset unless decoded", {
  skip_if_not_installed("terra")
  f <- system.file("extdata", "scoff_int16.tif", package = "a5px")
  skip_if(f == "")
  dir <- withr::local_tempdir()
  # the same codes, without scale and offset tags
  r <- terra::rast(f)
  terra::scoff(r) <- cbind(c(1, 1), c(0, 0))
  names(r) <- a5_raster_info(f)$band_names
  raw <- file.path(dir, "raw.tif")
  terra::writeRaster(r, raw, datatype = "INT2S", NAflag = -32768,
                     gdal = c("TILED=YES", "BLOCKXSIZE=256", "BLOCKYSIZE=256"))
  expect_error(a5_read_raster(c(f, raw), 12L), "scale")
  expect_error(a5_read_raster(c(f, raw), 12L, composite = "mean"), "scale")
  expect_error(a5_read_raster(c(f, raw), 12L, mode = "centroid"), "scale")
  expect_no_error(a5_read_raster(c(f, raw), 12L, scoff = TRUE))
  expect_no_error(a5_read_raster(c(f, raw), 12L, separate = TRUE))
  expect_no_error(a5_read_raster(c(f, f), 12L))
})

test_that("centroid mode over distant sources returns each source's cells", {
  skip_if_not_installed("terra")
  dir <- withr::local_tempdir()
  a <- const_tif(1, file.path(dir, "a.tif"), n = 128L,
                 ext = c(400000, 412800, 1000000, 1012800))
  b <- const_tif(3, file.path(dir, "b.tif"), n = 128L,
                 ext = c(700000, 712800, 1600000, 1612800))
  both <- a5_read_raster(c(a, b), 15L, mode = "centroid")
  each <- rbind(a5_read_raster(a, 15L, mode = "centroid"),
                a5_read_raster(b, 15L, mode = "centroid"))
  expect_equal(ord(both), ord(each), ignore_attr = TRUE)
  expect_setequal(both$v, c(1, 3))
})

test_that("a composite majority weighs each source by its majority class's pixels", {
  skip_if_not_installed("terra")
  dir <- withr::local_tempdir()
  # 10 x 10 pixels in the middle of one resolution-5 cell
  ctr <- a5R::a5_cell_to_lonlat(a5R::a5_lonlat_to_cell(10, 10, 5L), as_dataframe = TRUE)
  ext <- c(ctr$lon - 0.005, ctr$lon + 0.005, ctr$lat - 0.005, ctr$lat + 0.005)
  mk <- function(v, path) {
    r <- terra::rast(nrows = 10, ncols = 10, crs = "EPSG:4326",
                     extent = terra::ext(ext))
    terra::values(r) <- v
    names(r) <- "class"
    terra::writeRaster(r, path, datatype = "INT1U", NAflag = 255,
                       gdal = c("TILED=YES", "BLOCKXSIZE=16", "BLOCKYSIZE=16"))
    path
  }
  # A: 51 pixels of class 1, 49 of class 2; B: 60 of class 2, the rest nodata
  a <- mk(c(rep(1, 51), rep(2, 49)), file.path(dir, "a.tif"))
  b <- mk(c(rep(2, 60), rep(NA, 40)), file.path(dir, "b.tif"))
  pooled <- a5_read_raster(c(a, b), 5L, stat = "majority")
  comp <- a5_read_raster(c(a, b), 5L, stat = "majority", composite = "majority")
  expect_equal(nrow(comp), 1L)
  expect_equal(pooled$class, 2)
  expect_equal(comp$class, 2)
})

test_that("bbox chunks sharing an edge take a pixel centred on it once", {
  skip_if_not_installed("terra")
  dir <- withr::local_tempdir()
  # 0.25 degree pixels: centres at 0.125, 0.375, ... are exact in binary
  r <- terra::rast(nrows = 8, ncols = 8, crs = "EPSG:4326",
                   extent = terra::ext(10, 12, 10, 12))
  terra::values(r) <- seq_len(64)
  names(r) <- "v"
  f <- file.path(dir, "grid.tif")
  terra::writeRaster(r, f, gdal = c("TILED=YES", "BLOCKXSIZE=16", "BLOCKYSIZE=16"))
  whole <- a5_read_raster(f, 6L, stat = c("sum", "count"))
  chunks <- list(c(10, 10, 10.375, 10.375), c(10.375, 10, 12, 10.375),
                 c(10, 10.375, 10.375, 12), c(10.375, 10.375, 12, 12))
  parts <- do.call(rbind, lapply(chunks, function(b) {
    a5_read_raster(f, 6L, stat = c("sum", "count"), bbox = b)
  }))
  key <- hexkey(parts$cell)
  summed <- data.frame(
    key = unique(key),
    sum = as.numeric(tapply(parts$v_sum, key, sum)[unique(key)]),
    count = as.numeric(tapply(parts$v_count, key, sum)[unique(key)])
  )
  summed <- summed[order(summed$key), ]
  whole <- ord(whole)
  expect_equal(summed$key, hexkey(whole$cell))
  expect_equal(summed$sum, whole$v_sum)
  expect_equal(summed$count, whole$v_count)
  expect_equal(sum(summed$count), 64)
})

test_that("unnamed sources take their file name beside named ones", {
  skip_if_not_installed("terra")
  dir <- withr::local_tempdir()
  a <- const_tif(1, file.path(dir, "a.tif"), n = 64L, ext = c(400000, 406400, 1000000, 1006400))
  b <- const_tif(2, file.path(dir, "b.tif"), n = 64L, ext = c(400000, 406400, 1000000, 1006400))
  got <- a5_read_raster(c(red = a, b), 13L, separate = TRUE)
  expect_named(got, c("cell", "red", "b"))
})
