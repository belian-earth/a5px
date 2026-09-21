# scoff_int16.tif (benchmarks/make-scoff-fixture.R): 512x512, 2 x int16, UTM 33N at
# 30 m, AVERAGE overviews at decimation 2 and 4, nodata -32768 over the
# top-left 64x64 pixels.
#   band 1 "rh98": code = col + row       scale 0.01, offset 0, unit "m"
#   band 2:        code = 2 * col - row   scale 0.5,  offset 10
scoff_tif <- function() system.file("extdata", "scoff_int16.tif", package = "a5px")
ext <- function(name) system.file("extdata", name, package = "a5px")
ord <- function(df) df[order(a5R::a5_u64_to_hex(df$cell)), , drop = FALSE]

test_that("a5_raster_info reports per-band scale, offset and metadata", {
  f <- scoff_tif()
  skip_if(f == "")
  info <- a5_raster_info(f)
  expect_equal(info$scale, c(0.01, 0.5))
  expect_equal(info$offset, c(0, 10))
  expect_equal(info$nodata, -32768)
  expect_equal(info$band_metadata[[1]], c(UNITTYPE = "m"))
  expect_equal(info$band_metadata[[2]], c(SOURCE = "synthetic <a5px> & co"))
  expect_equal(info$metadata, c(PRODUCT = "scoff fixture"))
})

test_that("scale and offset are NA and metadata empty when undeclared", {
  f <- ext("be_planar.tif")
  skip_if(f == "")
  info <- a5_raster_info(f)
  expect_equal(info$scale, c(NA_real_, NA_real_))
  expect_equal(info$offset, c(NA_real_, NA_real_))
  expect_length(info$band_metadata, 2L)
  expect_length(info$metadata, 0L)
})

test_that("scale and offset match gdalraster", {
  skip_if_not_installed("gdalraster")
  f <- scoff_tif()
  skip_if(f == "")
  ds <- methods::new(gdalraster::GDALRaster, f)
  on.exit(ds$close())
  info <- a5_raster_info(f)
  expect_equal(info$scale, c(ds$getScale(1L), ds$getScale(2L)))
  expect_equal(info$offset, c(ds$getOffset(1L), ds$getOffset(2L)))
})

test_that("scoff equals the per-band dequant function at full resolution", {
  f <- scoff_tif()
  skip_if(f == "")
  stats <- c("mean", "min", "max", "sd", "sum")
  got <- ord(a5_read_raster(f, 13L, stat = stats, scoff = TRUE, use_overviews = FALSE))
  b1 <- ord(a5_read_raster(f, 13L, stat = stats, bands = 1L,
                           dequant = function(x) x * 0.01))
  b2 <- ord(a5_read_raster(f, 13L, stat = stats, bands = 2L,
                           dequant = function(x) x * 0.5 + 10))
  expect_equal(got[names(b1)], b1, ignore_attr = TRUE)
  expect_equal(got[names(b2)], b2, ignore_attr = TRUE)
})

test_that("scoff follows the band selection", {
  f <- scoff_tif()
  skip_if(f == "")
  full <- ord(a5_read_raster(f, 13L, scoff = TRUE, use_overviews = FALSE))
  b2 <- ord(a5_read_raster(f, 13L, scoff = TRUE, bands = 2L, use_overviews = FALSE))
  expect_equal(b2$band_02, full$band_02)
})

test_that("scoff keeps overviews in use and commutes with the mean", {
  f <- scoff_tif()
  skip_if(f == "")
  raw <- ord(a5_read_raster(f, 11L))
  dec <- ord(a5_read_raster(f, 11L, scoff = TRUE))
  expect_equal(dec$rh98, raw$rh98 * 0.01)
  expect_equal(dec$band_02, raw$band_02 * 0.5 + 10)
  # an overview really was read, and its mean tracks the full-resolution one
  # (cells on the nodata edge can differ between levels, so compare shared cells)
  edge_m <- as.numeric(a5R::a5_cell_edge_length_avg(11L, units = "m"))
  expect_gt(a5px:::a5_select_overview_level_rs(f, edge_m, character(), character()), 0L)
  full <- ord(a5_read_raster(f, 11L, scoff = TRUE, use_overviews = FALSE))
  key <- a5R::a5_u64_to_hex
  m <- match(key(dec$cell), key(full$cell))
  expect_gt(sum(!is.na(m)), 30L)
  diff <- abs(dec$rh98 - full$rh98[m])
  expect_lt(stats::median(diff, na.rm = TRUE), 0.05)
})

test_that("scoff applies in overlay and centroid modes and the Arrow path", {
  f <- scoff_tif()
  skip_if(f == "")
  for (mode in c("overlay", "centroid")) {
    raw <- ord(a5_read_raster(f, 13L, mode = mode, use_overviews = FALSE))
    dec <- ord(a5_read_raster(f, 13L, mode = mode, scoff = TRUE, use_overviews = FALSE))
    expect_equal(dec$rh98, raw$rh98 * 0.01)
    expect_equal(dec$band_02, raw$band_02 * 0.5 + 10)
  }
  raw <- ord(a5_read_raster(f, 12L, mode = "centroid", interp = "bilinear"))
  dec <- ord(a5_read_raster(f, 12L, mode = "centroid", interp = "bilinear", scoff = TRUE))
  expect_equal(dec$band_02, raw$band_02 * 0.5 + 10)

  skip_if_not_installed("arrow")
  tab <- as.data.frame(a5_read_raster_arrow(f, 13L, scoff = TRUE, use_overviews = FALSE))
  ref <- a5_read_raster(f, 13L, scoff = TRUE, use_overviews = FALSE)
  expect_equal(sort(tab$rh98), sort(ref$rh98))

  pq <- tempfile(fileext = ".parquet")
  on.exit(unlink(pq))
  a5_raster_to_parquet(f, pq, 13L, scoff = TRUE, use_overviews = FALSE)
  expect_equal(sort(arrow::read_parquet(pq)$band_02), sort(ref$band_02))
})

test_that("scoff is a no-op on a source without scale or offset tags", {
  f <- ext("f32_nodata.tif")
  skip_if(f == "")
  expect_equal(a5_read_raster(f, 12L, scoff = TRUE), a5_read_raster(f, 12L))
})

test_that("scoff rejects dequant and categorical stats", {
  f <- scoff_tif()
  skip_if(f == "")
  expect_error(
    a5_read_raster(f, 12L, scoff = TRUE, dequant = function(x) x),
    "cannot be combined"
  )
  expect_error(a5_read_raster(f, 12L, stat = "majority", scoff = TRUE), "scoff")
  expect_error(a5_read_raster(f, 12L, scoff = NA), "scoff")
})
