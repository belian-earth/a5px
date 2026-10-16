# mean / sum / count reads keep one weight per cell, shared by its bands,
# and split a cell into per-band weights where the bands' valid pixels
# differ. Adding min selects the per-band layout, so the two must agree.

three_band_tif <- function(path, shared = FALSE) {
  r <- terra::rast(nrows = 96, ncols = 96, nlyrs = 3, crs = "EPSG:32633",
                   extent = terra::ext(400000, 409600, 1000000, 1009600))
  n <- 96 * 96
  v1 <- (seq_len(n) %% 17) + 0.25
  v2 <- (seq_len(n) %% 13) * 1.5
  v3 <- (seq_len(n) %% 7) - 3
  gap <- seq(1, n, by = 5)
  v1[gap] <- NaN
  if (shared) {
    v2[gap] <- NaN
    v3[gap] <- NaN
  } else {
    v2[seq(2, n, by = 3)] <- NaN
    v3[1:(n / 3)] <- NaN  # a whole third of the raster missing in one band
  }
  terra::values(r) <- cbind(v1, v2, v3)
  names(r) <- c("a", "b", "c")
  terra::writeRaster(r, path, datatype = "FLT4S", overwrite = TRUE, NAflag = NA,
                     gdal = c("TILED=YES", "BLOCKXSIZE=32", "BLOCKYSIZE=32"))
  path
}

msc <- c("mean", "sum", "count")
cols_of <- function(df) df[, grepl("_(mean|sum|count)$", names(df))]

test_that("the shared weight matches per-band weights in forward and overlay reads", {
  skip_if_not_installed("terra")
  for (shared in c(FALSE, TRUE)) {
    f <- three_band_tif(withr::local_tempfile(fileext = ".tif"), shared)
    for (mode in c("forward", "overlay")) {
      a <- a5_read_raster(f, 10L, stat = msc, mode = mode, cpu_workers = 1L)
      b <- a5_read_raster(f, 10L, stat = c(msc, "min"), mode = mode, cpu_workers = 1L)
      info <- paste(mode, shared)
      expect_identical(a$cell, b$cell, info = info)
      expect_identical(cols_of(a), cols_of(b), info = info)
      # several workers merge partitions in a varying order
      p <- a5_read_raster(f, 10L, stat = msc, mode = mode, cpu_workers = 4L)
      expect_equal(cols_of(p), cols_of(a), tolerance = 1e-12, info = info)
    }
  }
})

test_that("the shared weight holds across pooled, composite and Parquet reads", {
  skip_if_not_installed("terra")
  skip_if_not_installed("arrow")
  f <- three_band_tif(withr::local_tempfile(fileext = ".tif"))
  # a second raster beside the first, its bands missing in other places
  g <- withr::local_tempfile(fileext = ".tif")
  r <- terra::rast(f)
  terra::ext(r) <- terra::ext(409600, 419200, 1000000, 1009600)
  terra::values(r) <- terra::values(r)[, c(3, 1, 2)]
  names(r) <- c("a", "b", "c")
  terra::writeRaster(r, g, datatype = "FLT4S", NAflag = NA,
                     gdal = c("TILED=YES", "BLOCKXSIZE=32", "BLOCKYSIZE=32"))
  for (comp in list(NULL, "mean")) {
    a <- a5_read_raster(c(f, g), 9L, stat = msc, composite = comp, cpu_workers = 1L)
    b <- a5_read_raster(c(f, g), 9L, stat = c(msc, "min"), composite = comp, cpu_workers = 1L)
    expect_identical(cols_of(a), cols_of(b))
  }
  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_raster_to_parquet(c(f, g), dest, 9L, stat = msc, cpu_workers = 1L)
  pq <- as.data.frame(arrow::read_parquet(dest))
  ref <- a5_read_raster(c(f, g), 9L, stat = c(msc, "min"), cpu_workers = 1L)
  expect_identical(unname(as.list(cols_of(pq))), unname(as.list(cols_of(ref))))
})
