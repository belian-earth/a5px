# Guards for the parallel read path and the windowed projector.
#
# - cpu_workers = 1 (one partition, stripes merged in order) versus 4
#   (row stripes on a pool, cell-id partitions merged as stripes finish)
#   must give identical cell sets and (forward) counts; sums, and overlay
#   counts built from 1/k^2 weights, may differ in the last bit from merge
#   order.
# - A5PX_EXACT_PROJ=1 (read per stripe) forces every projector window onto
#   the exact per-pixel projection; the bilinear windows with their error margins
#   must give the same cells, counts and (with one worker, same summation
#   order) the same values.
ext <- function(name) system.file("extdata", name, package = "a5px")
hexkey <- function(cells) a5R::a5_u64_to_hex(cells)
ord <- function(df) {
  df <- as.data.frame(df)
  df$cell <- hexkey(df$cell)
  df[order(df$cell), , drop = FALSE]
}
num_cols <- function(df) names(df)[vapply(df, is.numeric, logical(1))]

# fixtures spanning lat/long + nodata, big-endian planar, float32 nodata,
# chunky NaN nodata, a multi-block COG (4 tiles x 4 stripes), and a coarse
# LAEA grid (direct lookup path)
cases <- list(
  list(f = "aef_int8.tif", res = 17L),
  list(f = "be_planar.tif", res = 18L),
  list(f = "f32_nodata.tif", res = 18L),
  list(f = "nan_nodata.tif", res = 19L),
  list(f = "overview_cog.tif", res = 16L),
  list(f = "laea_wide.tif", res = 5L)
)

# Forward counts are integer sums, exact in any order. Overlay counts are
# sums of 1/k^2 weights, not representable for k not a power of two, so
# they carry the same last-bit order dependence as means.
expect_same_cells_counts <- function(a, b, tol_mean, exact_counts = TRUE, label = "") {
  expect_identical(a$cell, b$cell, info = label)
  within <- function(nm, tol) {
    scale <- max(abs(a[[nm]][is.finite(a[[nm]])]), 1)
    expect_lt(max(abs(a[[nm]] - b[[nm]]) / scale), tol, label = paste(label, nm))
  }
  for (nm in grep("_count$", names(a), value = TRUE)) {
    if (exact_counts) expect_identical(a[[nm]], b[[nm]], info = paste(label, nm)) else within(nm, 1e-12)
  }
  for (nm in grep("_mean$", names(a), value = TRUE)) within(nm, tol_mean)
}

test_that("1 and 4 workers give identical cells and counts, means within 1e-9", {
  for (cs in cases) {
    f <- ext(cs$f)
    skip_if(f == "")
    for (mode in c("forward", "overlay")) {
      one <- ord(a5_read_raster(f, cs$res, stat = c("mean", "count"), mode = mode,
                                cpu_workers = 1L, io_concurrency = 1L))
      four <- ord(a5_read_raster(f, cs$res, stat = c("mean", "count"), mode = mode,
                                 cpu_workers = 4L, io_concurrency = 4L))
      expect_gt(nrow(one), 0)
      expect_same_cells_counts(one, four, 1e-9, exact_counts = mode == "forward",
                               label = paste(cs$f, mode))
    }
  }
})

test_that("windowed projection matches the exact projection", {
  for (cs in cases) {
    f <- ext(cs$f)
    skip_if(f == "")
    for (mode in c("forward", "overlay")) {
      approx <- ord(a5_read_raster(f, cs$res, stat = c("mean", "count"), mode = mode,
                                   cpu_workers = 1L, io_concurrency = 1L))
      exact <- withr::with_envvar(c(A5PX_EXACT_PROJ = "1"), {
        ord(a5_read_raster(f, cs$res, stat = c("mean", "count"), mode = mode,
                           cpu_workers = 1L, io_concurrency = 1L))
      })
      rownames(approx) <- rownames(exact) <- NULL
      expect_identical(approx$cell, exact$cell)
      for (nm in num_cols(approx)) expect_identical(approx[[nm]], exact[[nm]])
    }
  }
})

test_that("a pixel-aligned bbox matches between 1 and 4 workers and both projections", {
  f <- ext("overview_cog.tif")
  skip_if(f == "")
  full_raw <- a5_read_raster(f, 16L, stat = c("mean", "count"), cpu_workers = 1L)
  full <- ord(full_raw)
  ll <- as.data.frame(a5R::a5_cell_to_lonlat(full_raw$cell))
  bb <- c(quantile(ll[[1]], 0.3), quantile(ll[[2]], 0.3), quantile(ll[[1]], 0.7), quantile(ll[[2]], 0.7))
  one <- ord(a5_read_raster(f, 16L, stat = c("mean", "count"), bbox = bb, cpu_workers = 1L))
  four <- ord(a5_read_raster(f, 16L, stat = c("mean", "count"), bbox = bb, cpu_workers = 4L))
  expect_gt(nrow(one), 0)
  expect_lt(nrow(one), nrow(full))
  expect_same_cells_counts(one, four, 1e-9, label = "bbox")
  exact <- withr::with_envvar(c(A5PX_EXACT_PROJ = "1"), {
    ord(a5_read_raster(f, 16L, stat = c("mean", "count"), bbox = bb, cpu_workers = 1L))
  })
  rownames(one) <- rownames(exact) <- NULL
  expect_identical(one$cell, exact$cell)
  for (nm in num_cols(one)) expect_identical(one[[nm]], exact[[nm]])
})
