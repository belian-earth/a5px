tif_dir <- function() {
  dev <- normalizePath(file.path(testthat::test_path(), "..", "..", "test-tifs"),
                       mustWork = FALSE)
  if (dir.exists(dev)) dev else NA_character_
}

skip_no_tifs <- function() {
  d <- tif_dir()
  if (is.na(d)) testthat::skip("test-tifs/ not present")
}

test_that("a5_read_raster reads exe_cog.tif and returns a tibble keyed by a5_cell", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  out <- a5_read_raster(path, resolution = 14, stat = "mean")

  expect_s3_class(out, "tbl_df")
  expect_true("cell" %in% names(out))
  expect_s3_class(out$cell, "a5_cell")
  expect_true(all(a5R::a5_get_resolution(out$cell) == 14L))
  expect_setequal(setdiff(names(out), "cell"), c("B02", "B03", "B04", "B08", "SCL"))
  expect_gt(nrow(out), 0L)
  # all bands should have at least some finite means after the all-nodata skip
  expect_true(all(vapply(out[, -1L], function(x) any(is.finite(x)), logical(1))))
  # no NaN cells should remain (all-nodata pixels are dropped)
  expect_true(all(vapply(out[, -1L],
                         function(x) sum(is.nan(x)) == 0L,
                         logical(1))))
})

test_that("a5_read_raster honours stat selection", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  # cell-output ordering is not guaranteed across stat calls, so join on cell
  align_by_cell <- function(a, b) {
    key_a <- a5R::a5_u64_to_hex(a$cell)
    key_b <- a5R::a5_u64_to_hex(b$cell)
    common <- intersect(key_a, key_b)
    a <- a[match(common, key_a), , drop = FALSE]
    b <- b[match(common, key_b), , drop = FALSE]
    list(a = a, b = b)
  }

  m <- a5_read_raster(path, resolution = 12, stat = "mean")
  s <- a5_read_raster(path, resolution = 12, stat = "sum")
  n <- a5_read_raster(path, resolution = 12, stat = "count")
  mn <- a5_read_raster(path, resolution = 12, stat = "min")
  mx <- a5_read_raster(path, resolution = 12, stat = "max")

  expect_equal(nrow(m), nrow(s))
  expect_equal(nrow(m), nrow(n))

  # count is integer-valued
  expect_true(all(n$B02 == round(n$B02)))

  # min <= mean <= max
  pair <- align_by_cell(mn, m)
  fi <- is.finite(pair$a$B02) & is.finite(pair$b$B02)
  expect_true(all(pair$a$B02[fi] <= pair$b$B02[fi]))

  pair <- align_by_cell(m, mx)
  fi <- is.finite(pair$a$B02) & is.finite(pair$b$B02)
  expect_true(all(pair$a$B02[fi] <= pair$b$B02[fi]))

  # sum = mean * count (modulo float error). aligning all three:
  k_m <- a5R::a5_u64_to_hex(m$cell)
  k_s <- a5R::a5_u64_to_hex(s$cell)
  k_n <- a5R::a5_u64_to_hex(n$cell)
  common <- Reduce(intersect, list(k_m, k_s, k_n))
  m_a <- m[match(common, k_m), , drop = FALSE]
  s_a <- s[match(common, k_s), , drop = FALSE]
  n_a <- n[match(common, k_n), , drop = FALSE]
  fi <- is.finite(m_a$B02) & is.finite(s_a$B02) & is.finite(n_a$B02)
  expect_lt(max(abs(s_a$B02[fi] - m_a$B02[fi] * n_a$B02[fi])), 1e-6)
})

test_that("a5_read_raster as_vector = TRUE collapses bands to a list column", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  v <- a5_read_raster(path, resolution = 12, stat = "mean", as_vector = TRUE)
  expect_named(v, c("cell", "value"))
  expect_type(v$value, "list")
  expect_equal(unique(lengths(v$value)), 5L)
})

test_that("a5_read_raster errors on missing file", {
  expect_error(
    a5_read_raster("/no/such/file.tif", resolution = 10),
    "file not found"
  )
})

test_that("a5_read_raster validates resolution", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")
  expect_error(a5_read_raster(path, resolution = 31), "must be between")
  expect_error(a5_read_raster(path, resolution = -1), "must be between")
})

test_that("a5_read_raster errors on non-georeferenced TIFF", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "test_non_geo.tif")
  skip_if_not(file.exists(path), "test_non_geo.tif missing")
  expect_error(a5_read_raster(path, resolution = 10))
})
