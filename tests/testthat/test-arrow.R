tif_dir <- function() {
  dev <- normalizePath(file.path(testthat::test_path(), "..", "..", "test-tifs"),
                       mustWork = FALSE)
  if (dir.exists(dev)) dev else NA_character_
}

skip_no_tifs <- function() {
  d <- tif_dir()
  if (is.na(d)) testthat::skip("test-tifs/ not present")
}

test_that("a5_read_raster_arrow returns an arrow Table with a FixedSizeList value column", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  tbl <- a5_read_raster_arrow(path, resolution = 14L, bands = 1:3,
                              value_type = "float32")
  expect_s3_class(tbl, "Table")
  expect_equal(tbl$num_columns, 2L)
  expect_setequal(names(tbl), c("cell", "value"))
  expect_equal(tbl$cell$type$ToString(), "uint64")
  expect_match(tbl$value$type$ToString(),
               "fixed_size_list<[A-Za-z]+: float>\\[3\\]")

  # schema metadata round-trips the band names
  meta <- tbl$schema$metadata
  expect_true(!is.null(meta))
  expect_equal(meta$a5px_band_names, "B02\nB03\nB04")
  expect_equal(meta$a5px_resolution, "14")
  expect_equal(meta$a5px_stat, "mean")

  # values agree with the tibble path
  ref <- a5_read_raster(path, resolution = 14L, stat = "mean", bands = 1:3)
  arrow_keys <- a5R::a5_u64_to_hex(a5R::a5_cell_from_arrow(tbl$cell))
  ref_keys   <- a5R::a5_u64_to_hex(ref$cell)
  ord <- match(arrow_keys, ref_keys)
  expect_false(anyNA(ord))

  # decode the FixedSizeList back to a matrix and compare values (float32 tol)
  rows <- tbl$value$as_vector()
  mat  <- do.call(rbind, lapply(rows, as.numeric))
  expected <- cbind(ref$B02[ord], ref$B03[ord], ref$B04[ord])
  expect_lt(max(abs(mat - expected), na.rm = TRUE), 1e-3)
})

test_that("a5_write_parquet round-trips an arrow Table", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  tbl <- a5_read_raster_arrow(path, resolution = 14L, bands = 1:3)
  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_write_parquet(tbl, dest)

  expect_true(file.exists(dest))
  back <- arrow::read_parquet(dest, as_data_frame = FALSE)
  expect_equal(back$num_rows, tbl$num_rows)
  expect_equal(back$schema$metadata$a5px_band_names, "B02\nB03\nB04")
})

test_that("a5_write_parquet accepts a tibble from a5_read_raster (per-band)", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  ref <- a5_read_raster(path, resolution = 12L, bands = 1:2)
  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_write_parquet(ref, dest)

  back <- arrow::read_parquet(dest, as_data_frame = FALSE)
  expect_setequal(names(back), c("cell", "B02", "B03"))
})

test_that("a5_write_parquet accepts a tibble from a5_read_raster (as_vector list col)", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  v <- a5_read_raster(path, resolution = 12L, bands = 1:3, as_vector = TRUE)
  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_write_parquet(v, dest)
  back <- arrow::read_parquet(dest, as_data_frame = FALSE)
  expect_setequal(names(back), c("cell", "value"))
  expect_match(back$value$type$ToString(),
               "fixed_size_list<[A-Za-z]+: double>\\[3\\]")
})
