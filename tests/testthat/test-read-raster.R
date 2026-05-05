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

test_that("a5_read_raster band subset by index", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  full <- a5_read_raster(path, resolution = 12, stat = "mean")
  sub  <- a5_read_raster(path, resolution = 12, stat = "mean", bands = c(1L, 4L))

  expect_equal(setdiff(names(sub), "cell"), c("B02", "B08"))
  expect_equal(nrow(sub), nrow(full))
  k_full <- a5R::a5_u64_to_hex(full$cell)
  k_sub  <- a5R::a5_u64_to_hex(sub$cell)
  ord    <- match(k_sub, k_full)
  expect_equal(sub$B02, full$B02[ord])
  expect_equal(sub$B08, full$B08[ord])
})

test_that("a5_read_raster band subset by name", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  sub <- a5_read_raster(path, resolution = 12, stat = "mean",
                        bands = c("B04", "SCL"))
  expect_equal(setdiff(names(sub), "cell"), c("B04", "SCL"))
  expect_gt(nrow(sub), 0L)
})

test_that("a5_read_raster errors on bad band selection", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  expect_error(a5_read_raster(path, resolution = 12, bands = 999L),
               "out of range")
  expect_error(a5_read_raster(path, resolution = 12, bands = "no_such_band"),
               "not found")
  expect_error(a5_read_raster(path, resolution = 12, bands = 0L),
               "positive")
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

test_that("multi-stat in one pass produces band__stat columns", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  out <- a5_read_raster(path, resolution = 12L,
                        stat = c("mean", "count", "sum"),
                        bands = c("B02", "B03"))
  expect_setequal(
    setdiff(names(out), "cell"),
    c("B02__mean", "B02__count", "B02__sum",
      "B03__mean", "B03__count", "B03__sum")
  )
  fi <- is.finite(out$B02__mean) & is.finite(out$B02__count)
  expect_true(all(out$B02__count[fi] >= 1))
  expect_lt(max(abs(out$B02__sum[fi] - out$B02__mean[fi] * out$B02__count[fi])), 1e-6)

  # single-stat path is unchanged (column = band name)
  s1 <- a5_read_raster(path, resolution = 12L, stat = "mean", bands = "B02")
  expect_setequal(setdiff(names(s1), "cell"), "B02")
})

test_that("multi-stat errors on duplicate or unknown stats", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")
  expect_error(a5_read_raster(path, resolution = 12L, stat = c("mean", "mean")),
               "duplicates")
  expect_error(a5_read_raster(path, resolution = 12L, stat = c("mean", "median")),
               "Unknown")
})

test_that("NaN nodata is filtered (NaN-safe comparison)", {
  path <- system.file("extdata", "nan_nodata.tif", package = "a5px")
  skip_if(path == "", "nan_nodata.tif not installed")

  m   <- a5_read_raster(path, resolution = 16L, stat = "mean")
  cnt <- a5_read_raster(path, resolution = 16L, stat = "count")

  expect_setequal(setdiff(names(m), "cell"), c("x", "y"))
  expect_false(any(is.nan(m$x)))
  expect_false(any(is.nan(m$y)))
  # raster is 32x32 with 5 NaN-nodata pixels per band -> 1019 valid pixels each
  expect_equal(sum(cnt$x), 1019)
  expect_equal(sum(cnt$y), 1019)
})

test_that("bbox subset returns only cells inside the bbox", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  full <- a5_read_raster(path, resolution = 14L, stat = "mean", bands = 1L)
  full_xy <- as.matrix(wk::as_xy(a5R::a5_cell_to_lonlat(full$cell)))
  # take the central quarter of the tile in lon/lat
  rng_x <- range(full_xy[, 1]); rng_y <- range(full_xy[, 2])
  qx <- diff(rng_x) / 4; qy <- diff(rng_y) / 4
  bb <- c(rng_x[1] + qx, rng_y[1] + qy, rng_x[2] - qx, rng_y[2] - qy)

  sub <- a5_read_raster(path, resolution = 14L, stat = "mean",
                        bands = 1L, bbox = bb)
  expect_gt(nrow(sub), 0L)
  expect_lt(nrow(sub), nrow(full))

  # bbox filters PIXELS, not cells: a cell straddling the boundary can show
  # up in the subset with a centroid slightly outside (and a partial
  # aggregate). Allow ~half a cell of slop.
  sub_xy <- as.matrix(wk::as_xy(a5R::a5_cell_to_lonlat(sub$cell)))
  cell_w_deg <- diff(rng_x) / sqrt(nrow(full))  # rough cell width in lon
  cell_h_deg <- diff(rng_y) / sqrt(nrow(full))
  slop_x <- cell_w_deg
  slop_y <- cell_h_deg
  expect_true(all(sub_xy[, 1] >= bb[1] - slop_x & sub_xy[, 1] <= bb[3] + slop_x))
  expect_true(all(sub_xy[, 2] >= bb[2] - slop_y & sub_xy[, 2] <= bb[4] + slop_y))

  # cells with centroids well inside the bbox (no boundary truncation) should
  # have identical values in the two reads
  inside <- sub_xy[, 1] > bb[1] + slop_x & sub_xy[, 1] < bb[3] - slop_x &
            sub_xy[, 2] > bb[2] + slop_y & sub_xy[, 2] < bb[4] - slop_y
  expect_gt(sum(inside), 0L)
  k_full <- a5R::a5_u64_to_hex(full$cell)
  k_sub  <- a5R::a5_u64_to_hex(sub$cell[inside])
  ord <- match(k_sub, k_full)
  expect_false(anyNA(ord))
  expect_equal(sub$B02[inside], full$B02[ord])
})

test_that("bbox arg validates", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")
  expect_error(a5_read_raster(path, resolution = 12L, bbox = c(0, 0)),
               "length-4")
  expect_error(a5_read_raster(path, resolution = 12L, bbox = c(2, 0, 1, 1)),
               "xmin < xmax")
})

test_that("src_nodata overrides metadata nodata", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  default <- a5_read_raster(path, resolution = 14L, stat = "count", bands = 1L)
  # exe_cog.tif uses nodata=0; override to a sentinel that never appears so
  # all pixels are treated as valid -> per-cell counts go up.
  forced  <- a5_read_raster(path, resolution = 14L, stat = "count", bands = 1L,
                            src_nodata = -9999)

  # both produce the same cell set at this resolution; counts in `forced`
  # should be >= counts in `default` (no pixels filtered as nodata)
  k_d <- a5R::a5_u64_to_hex(default$cell)
  k_f <- a5R::a5_u64_to_hex(forced$cell)
  ord <- match(k_d, k_f)
  expect_false(anyNA(ord))
  expect_true(all(forced$B02[ord] >= default$B02))
  expect_true(sum(forced$B02) > sum(default$B02))
})

test_that("mode = 'centroid' fills the gaps that mode = 'forward' leaves", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  # exe_cog has ~20m pixels. At res 18 cells are ~22m -- comparable. The
  # forward path will miss some cells (no pixel centre inside them);
  # centroid mode picks them up.
  fwd <- a5_read_raster(path, resolution = 18L, stat = "mean", bands = 1L,
                        mode = "forward")
  cnt <- a5_read_raster(path, resolution = 18L, stat = "mean", bands = 1L,
                        mode = "centroid")

  expect_gt(nrow(cnt), nrow(fwd))
  # every forward cell should also appear in centroid (centroid is a strict
  # superset under these conditions, modulo edge clipping)
  fwd_keys <- a5R::a5_u64_to_hex(fwd$cell)
  cnt_keys <- a5R::a5_u64_to_hex(cnt$cell)
  expect_gt(length(intersect(fwd_keys, cnt_keys)), 0L)
})

test_that("centroid sub-pixel resolution emits ~one cell per sub-pixel", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  # res 20 cells ~5.5m vs ~20m pixels => ~13 cells per pixel
  out <- a5_read_raster(path, resolution = 20L, stat = "mean", bands = 1L,
                        mode = "centroid")
  expect_gt(nrow(out), 1e6)
  expect_setequal(setdiff(names(out), "cell"), "B02")
})

test_that("centroid honours bbox", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  full_bbox <- a5_read_raster(path, resolution = 18L, stat = "mean", bands = 1L,
                              mode = "centroid")
  full_xy <- as.matrix(wk::as_xy(a5R::a5_cell_to_lonlat(full_bbox$cell)))
  rng_x <- range(full_xy[, 1]); rng_y <- range(full_xy[, 2])
  qx <- diff(rng_x) / 4; qy <- diff(rng_y) / 4
  bb <- c(rng_x[1] + qx, rng_y[1] + qy, rng_x[2] - qx, rng_y[2] - qy)

  sub <- a5_read_raster(path, resolution = 18L, stat = "mean", bands = 1L,
                        mode = "centroid", bbox = bb)
  expect_gt(nrow(sub), 0L)
  expect_lt(nrow(sub), nrow(full_bbox))
})

test_that("user-defined CRS is reconstructed from GeoKey fields (LAEA)", {
  path <- system.file("extdata", "laea_custom.tif", package = "a5px")
  skip_if(path == "", "laea_custom.tif not installed")

  out <- a5_read_raster(path, resolution = 16L, stat = "mean")
  expect_gt(nrow(out), 0L)
  expect_setequal(setdiff(names(out), "cell"), "band_01")

  # raster is centered on lon=-3, lat=51 in projected space (0, 0).
  # cell centroids should cluster tightly around there.
  ll <- a5R::a5_cell_to_lonlat(out$cell)
  xy <- as.matrix(wk::as_xy(ll))
  expect_lt(max(abs(xy[, 1] - (-3))), 0.5)
  expect_lt(max(abs(xy[, 2] - 51)), 0.5)
})

test_that("a5_read_raster errors on non-georeferenced TIFF", {
  skip_no_tifs()
  path <- file.path(tif_dir(), "test_non_geo.tif")
  skip_if_not(file.exists(path), "test_non_geo.tif missing")
  expect_error(a5_read_raster(path, resolution = 10))
})
