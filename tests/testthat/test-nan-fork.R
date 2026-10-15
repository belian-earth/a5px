# NaN pixels are missing data whatever the nodata tag, and reads work in
# forked children.

nan_tif <- function(path, nodata = NULL) {
  r <- terra::rast(nrows = 64, ncols = 64, crs = "EPSG:32633",
                   extent = terra::ext(400000, 406400, 1000000, 1006400))
  v <- rep(c(1, 2, 3, 4), length.out = 64 * 64)
  v[seq(1, length(v), by = 7)] <- NaN
  if (!is.null(nodata)) v[seq(2, length(v), by = 11)] <- nodata
  terra::values(r) <- v
  names(r) <- "v"
  # write the NaNs as stored values: with no NAflag, terra writes no tag
  terra::writeRaster(r, path, datatype = "FLT4S", overwrite = TRUE,
                     NAflag = if (is.null(nodata)) NA else nodata,
                     gdal = c("TILED=YES", "BLOCKXSIZE=16", "BLOCKYSIZE=16"))
  path
}

test_that("NaN pixels are skipped without a nodata tag", {
  skip_if_not_installed("terra")
  f <- nan_tif(withr::local_tempfile(fileext = ".tif"))
  vals <- terra::values(terra::rast(f))[, 1]
  got <- a5_read_raster(f, 9L, stat = c("mean", "count", "min", "max", "npix"))
  expect_equal(sum(got$v_count), sum(!is.na(vals)))
  expect_equal(sum(got$npix), sum(!is.na(vals)))
  expect_false(anyNA(got$v_mean))
  expect_equal(weighted.mean(got$v_mean, got$v_count), mean(vals, na.rm = TRUE))
})

test_that("NaN pixels are skipped beside a non-NaN nodata, and src_nodata may be NaN", {
  skip_if_not_installed("terra")
  f <- nan_tif(withr::local_tempfile(fileext = ".tif"), nodata = -9999)
  vals <- terra::values(terra::rast(f))[, 1]
  got <- a5_read_raster(f, 9L, stat = c("mean", "count"))
  expect_equal(sum(got$v_count), sum(!is.na(vals)))
  expect_false(anyNA(got$v_mean))
  expect_no_error(a5_read_raster(f, 9L, src_nodata = NaN))
})

test_that("overlay and centroid modes skip NaN pixels", {
  skip_if_not_installed("terra")
  f <- nan_tif(withr::local_tempfile(fileext = ".tif"))
  ov <- a5_read_raster(f, 9L, mode = "overlay")
  expect_false(anyNA(ov$v))
  # a bilinear stencil with a NaN pixel renormalises over the valid ones
  ce <- a5_read_raster(f, 15L, mode = "centroid", interp = "bilinear")
  near <- a5_read_raster(f, 15L, mode = "centroid", interp = "nearest")
  expect_false(anyNA(ce$v))
  expect_gte(nrow(ce), nrow(near))
})

test_that("a dequant function returning NA marks those codes missing", {
  f <- system.file("extdata", "scoff_int16.tif", package = "a5px")
  skip_if(f == "")
  # band 1 codes are col + row; drop the odd ones
  dq <- function(x) ifelse(x %% 2 == 1, NA, x)
  got <- a5_read_raster(f, 12L, bands = 1L, stat = c("mean", "count", "npix"),
                        dequant = dq, use_overviews = FALSE)
  raw <- a5_read_raster(f, 12L, bands = 1L, stat = c("count", "npix"), use_overviews = FALSE)
  expect_false(anyNA(got[[2]]))
  expect_lt(sum(got$npix), sum(raw$npix))
  expect_equal(sum(got$npix), sum(got[[3]]))
})

test_that("a forked child reads after the parent has", {
  skip_on_os("windows")
  f <- system.file("extdata", "overview_cog.tif", package = "a5px")
  skip_if(f == "")
  parent <- a5_read_raster(f, 12L)
  job <- parallel::mcparallel(nrow(a5_read_raster(f, 12L)))
  res <- parallel::mccollect(job, wait = FALSE, timeout = 60)
  expect_false(is.null(res), info = "the forked read did not finish within 60 s")
  expect_equal(res[[1]], nrow(parent))
  # centroid mode, which runs its own thread pool, too
  job <- parallel::mcparallel(nrow(a5_read_raster(f, 14L, mode = "centroid")))
  res <- parallel::mccollect(job, wait = FALSE, timeout = 60)
  expect_false(is.null(res))
})
