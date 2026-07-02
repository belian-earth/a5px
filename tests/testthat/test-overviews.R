# overview_cog.tif is a 512x512 float32 plane in UTM 33N (30 m pixels) with
# internal AVERAGE overviews at decimation 2 and 4 (256x256, 128x128). The
# value is a linear function of position, so any positional error in the
# derived overview geotransform would surface as a value mismatch.
ov_tif <- function() {
  system.file("extdata", "overview_cog.tif", package = "a5px")
}

# A5 cell edge length (metres) at a resolution, matching the value a5px passes
# to Rust when use_overviews = TRUE and stat = "mean".
edge_m <- function(res) {
  sqrt(as.numeric(a5R::a5_cell_area(res, units = "m^2")))
}

hexset <- function(cells) a5R::a5_u64_to_hex(cells)

test_that("overview level selection picks coarser levels for coarser cells", {
  f <- ov_tif()
  skip_if(f == "")
  # res 11 cells (~2.8 km) are far coarser than the 30 m pixels: the decim-4
  # overview (level 2) still oversamples them.
  expect_equal(a5px:::a5_select_overview_level_rs(f, edge_m(11L)), 2L)
  # res 14 cells (~360 m) only leave room for the decim-2 overview (level 1).
  expect_equal(a5px:::a5_select_overview_level_rs(f, edge_m(14L)), 1L)
  # very fine cells (finer than the source pixels) fall back to full resolution.
  expect_equal(a5px:::a5_select_overview_level_rs(f, edge_m(20L)), 0L)
  # a target of 0 disables overview use entirely.
  expect_equal(a5px:::a5_select_overview_level_rs(f, 0), 0L)
})

test_that("use_overviews preserves the mean against a full-resolution read", {
  f <- ov_tif()
  skip_if(f == "")
  full <- a5_read_raster(f, resolution = 11L, use_overviews = FALSE)
  ov   <- a5_read_raster(f, resolution = 11L, use_overviews = TRUE)

  # an overview really was used (not a silent full-res fallback)
  expect_gt(a5px:::a5_select_overview_level_rs(f, edge_m(11L)), 0L)

  # same coverage at this resolution
  expect_setequal(hexset(ov$cell), hexset(full$cell))

  # per-cell means agree to well within the ~40-unit value range
  j <- merge(
    data.frame(h = hexset(full$cell), full = full[[2]]),
    data.frame(h = hexset(ov$cell),   ov   = ov[[2]]),
    by = "h"
  )
  expect_lt(max(abs(j$full - j$ov)), 0.5)
})

test_that("non-mean stats ignore overviews even when use_overviews = TRUE", {
  f <- ov_tif()
  skip_if(f == "")
  # overview_target is 0 for any stat other than a lone "mean", so TRUE and
  # FALSE take the identical full-resolution path.
  mx_t <- a5_read_raster(f, resolution = 11L, stat = "max", use_overviews = TRUE)
  mx_f <- a5_read_raster(f, resolution = 11L, stat = "max", use_overviews = FALSE)
  ord <- function(df) df[order(hexset(df$cell)), , drop = FALSE]
  expect_equal(ord(mx_t), ord(mx_f))
})

test_that("multi-stat including mean does not use overviews", {
  f <- ov_tif()
  skip_if(f == "")
  # a multi-stat request is not a lone "mean"; overviews stay off.
  expect_equal(
    overview_target_metres(TRUE, c("mean", "max"), 11L),
    0
  )
  expect_gt(overview_target_metres(TRUE, "mean", 11L), 0)
})

test_that("single-IFD rasters are unaffected by use_overviews", {
  f <- system.file("extdata", "laea_custom.tif", package = "a5px")
  skip_if(f == "")
  # no overviews present -> always full resolution regardless of the target.
  expect_equal(a5px:::a5_select_overview_level_rs(f, 1e6), 0L)

  ord <- function(df) df[order(hexset(df$cell)), , drop = FALSE]
  a <- ord(a5_read_raster(f, resolution = 14L, use_overviews = TRUE))
  b <- ord(a5_read_raster(f, resolution = 14L, use_overviews = FALSE))
  expect_equal(a, b)
})

test_that("use_overviews must be a length-1 non-NA logical", {
  f <- ov_tif()
  skip_if(f == "")
  expect_error(a5_read_raster(f, resolution = 11L, use_overviews = NA),
               "length-1 non-NA logical")
  expect_error(a5_read_raster(f, resolution = 11L, use_overviews = c(TRUE, FALSE)),
               "length-1 non-NA logical")
})
