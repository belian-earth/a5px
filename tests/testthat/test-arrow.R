tif_dir <- function() {
  dev <- normalizePath(file.path(testthat::test_path(), "..", "..", "test-tifs"),
                       mustWork = FALSE)
  if (dir.exists(dev)) dev else NA_character_
}

skip_no_tifs <- function() {
  d <- tif_dir()
  if (is.na(d)) testthat::skip("test-tifs/ not present")
}

test_that("a5_read_raster_arrow default (wide) returns one column per band", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  tbl <- a5_read_raster_arrow(path, resolution = 14L, bands = 1:3,
                              value_type = "float32")
  expect_s3_class(tbl, "Table")
  expect_setequal(names(tbl), c("cell", "B02", "B03", "B04"))
  expect_equal(tbl$cell$type$ToString(), "uint64")
  for (b in c("B02", "B03", "B04")) {
    expect_equal(tbl[[b]]$type$ToString(), "float")
  }

  meta <- tbl$schema$metadata
  expect_equal(meta$a5px_band_names, "B02\nB03\nB04")
  expect_equal(meta$a5px_resolution, "14")
  expect_equal(meta$a5px_stat, "mean")
  expect_equal(meta$a5px_layout, "wide")

  # values agree with the tibble path
  ref <- a5_read_raster(path, resolution = 14L, stat = "mean", bands = 1:3)
  arrow_keys <- a5R::a5_u64_to_hex(a5R::a5_cell_from_arrow(tbl$cell))
  ref_keys   <- a5R::a5_u64_to_hex(ref$cell)
  ord <- match(arrow_keys, ref_keys)
  expect_false(anyNA(ord))
  for (b in c("B02", "B03", "B04")) {
    expect_lt(max(abs(tbl[[b]]$as_vector() - ref[[b]][ord]), na.rm = TRUE), 1e-3)
  }
})

test_that("a5_read_raster_arrow with as_vector = TRUE returns a FixedSizeList", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  tbl <- a5_read_raster_arrow(path, resolution = 14L, bands = 1:3,
                              value_type = "float32", as_vector = TRUE)
  expect_s3_class(tbl, "Table")
  expect_setequal(names(tbl), c("cell", "value"))
  expect_match(tbl$value$type$ToString(),
               "fixed_size_list<[A-Za-z]+: float>\\[3\\]")
  expect_equal(tbl$schema$metadata$a5px_layout, "fsl")

  ref <- a5_read_raster(path, resolution = 14L, stat = "mean", bands = 1:3)
  arrow_keys <- a5R::a5_u64_to_hex(a5R::a5_cell_from_arrow(tbl$cell))
  ref_keys   <- a5R::a5_u64_to_hex(ref$cell)
  ord <- match(arrow_keys, ref_keys)
  expect_false(anyNA(ord))
  rows <- tbl$value$as_vector()
  mat  <- do.call(rbind, lapply(rows, as.numeric))
  expected <- cbind(ref$B02[ord], ref$B03[ord], ref$B04[ord])
  expect_lt(max(abs(mat - expected), na.rm = TRUE), 1e-3)
})

test_that("a5_write_parquet round-trips an arrow Table (wide default)", {
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
  expect_setequal(names(back), c("cell", "B02", "B03", "B04"))
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

test_that("multi-stat arrow path (default wide) names columns <band>_<stat>", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  tbl <- a5_read_raster_arrow(path, resolution = 12L,
                              stat = c("mean", "count"),
                              bands = c("B02", "B03"))
  expect_setequal(names(tbl),
                  c("cell", "B02_mean", "B03_mean", "B02_count", "B03_count"))
  expect_equal(tbl$B02_mean$type$ToString(),  "double")
  expect_equal(tbl$B03_count$type$ToString(), "double")
  expect_equal(tbl$schema$metadata$a5px_stats, "mean\ncount")
  expect_equal(tbl$schema$metadata$a5px_layout, "wide")
})

test_that("multi-stat arrow path with as_vector = TRUE produces one FSL per stat", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  tbl <- a5_read_raster_arrow(path, resolution = 12L,
                              stat = c("mean", "count"),
                              bands = c("B02", "B03"),
                              as_vector = TRUE)
  expect_setequal(names(tbl), c("cell", "value_mean", "value_count"))
  expect_match(tbl$value_mean$type$ToString(),  "fixed_size_list<[A-Za-z]+: double>\\[2\\]")
  expect_match(tbl$value_count$type$ToString(), "fixed_size_list<[A-Za-z]+: double>\\[2\\]")
  expect_equal(tbl$schema$metadata$a5px_layout, "fsl")
})

test_that("multi-stat parquet (Rust-direct, default wide) round-trips", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_raster_to_parquet(path, dest, resolution = 12L,
                       stat = c("mean", "count"), bands = 1:2)
  back <- arrow::read_parquet(dest, as_data_frame = FALSE)
  expect_setequal(names(back),
                  c("cell", "B02_mean", "B03_mean", "B02_count", "B03_count"))
  expect_equal(back$schema$metadata$a5px_stats, "mean\ncount")
  expect_equal(back$schema$metadata$a5px_layout, "wide")
})

test_that("multi-stat parquet (Rust-direct, as_vector = TRUE) round-trips", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_raster_to_parquet(path, dest, resolution = 12L,
                       stat = c("mean", "count"), bands = 1:2,
                       as_vector = TRUE)
  back <- arrow::read_parquet(dest, as_data_frame = FALSE)
  expect_setequal(names(back), c("cell", "value_mean", "value_count"))
  expect_match(back$value_mean$type$ToString(),
               "fixed_size_list<[A-Za-z]+: double>\\[2\\]")
  expect_equal(back$schema$metadata$a5px_layout, "fsl")
})

test_that("a5_raster_to_parquet (default wide) matches the R-Arrow path bit-for-bit", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_raster_to_parquet(path, dest, resolution = 14L, bands = 1:3,
                       value_type = "float32")

  back <- arrow::read_parquet(dest, as_data_frame = FALSE)
  expect_setequal(names(back), c("cell", "B02", "B03", "B04"))
  for (b in c("B02", "B03", "B04")) {
    expect_equal(back[[b]]$type$ToString(), "float")
  }
  expect_equal(back$schema$metadata$a5px_band_names, "B02\nB03\nB04")
  expect_equal(back$schema$metadata$a5px_resolution, "14")
  expect_equal(back$schema$metadata$a5px_stat, "mean")
  expect_equal(back$schema$metadata$a5px_layout, "wide")

  ref <- a5_read_raster_arrow(path, resolution = 14L, bands = 1:3,
                              value_type = "float32")
  back_keys <- a5R::a5_u64_to_hex(a5R::a5_cell_from_arrow(back$cell))
  ref_keys  <- a5R::a5_u64_to_hex(a5R::a5_cell_from_arrow(ref$cell))
  ord <- match(ref_keys, back_keys)
  expect_false(anyNA(ord))
  for (b in c("B02", "B03", "B04")) {
    expect_equal(back[[b]]$as_vector()[ord], ref[[b]]$as_vector())
  }
})

test_that("a5_raster_to_parquet (as_vector = TRUE) matches the R-Arrow FSL path", {
  testthat::skip_if_not_installed("arrow")
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_raster_to_parquet(path, dest, resolution = 14L, bands = 1:3,
                       value_type = "float32", as_vector = TRUE)

  back <- arrow::read_parquet(dest, as_data_frame = FALSE)
  expect_setequal(names(back), c("cell", "value"))
  expect_match(back$value$type$ToString(),
               "fixed_size_list<[A-Za-z]+: float>\\[3\\]")
  expect_equal(back$schema$metadata$a5px_layout, "fsl")

  ref <- a5_read_raster_arrow(path, resolution = 14L, bands = 1:3,
                              value_type = "float32", as_vector = TRUE)
  back_keys <- a5R::a5_u64_to_hex(a5R::a5_cell_from_arrow(back$cell))
  ref_keys  <- a5R::a5_u64_to_hex(a5R::a5_cell_from_arrow(ref$cell))
  ord <- match(ref_keys, back_keys)
  expect_false(anyNA(ord))
  back_flat <- unlist(back$value$as_vector()[ord])
  ref_flat  <- unlist(ref$value$as_vector())
  expect_equal(back_flat, ref_flat)
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
