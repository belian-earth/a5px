# Overlay mode: area-weighted pixel-to-cell transfer approximated by k x k
# sub-point supersampling. Uses the aef_int8.tif fixture documented in
# test-dequant.R: 128x128 2-band int8, EPSG:4326, nodata -128 in a 10x10
# top-left patch (so 128*128 - 100 = 16284 valid pixels per band).
ovl_tif <- function() {
  system.file("extdata", "aef_int8.tif", package = "a5px")
}

hexset <- function(cells) a5R::a5_u64_to_hex(cells)

align_by_cell <- function(a, b) {
  key_a <- hexset(a$cell)
  key_b <- hexset(b$cell)
  common <- intersect(key_a, key_b)
  list(
    a = a[match(common, key_a), , drop = FALSE],
    b = b[match(common, key_b), , drop = FALSE]
  )
}

test_that("overlay conserves mass and effective pixel count", {
  f <- ovl_tif()
  skip_if(f == "")
  fwd <- a5_read_raster(f, resolution = 12L, stat = c("sum", "count"),
                        use_overviews = FALSE)
  ovl <- a5_read_raster(f, resolution = 12L, stat = c("sum", "count"),
                        mode = "overlay", use_overviews = FALSE)

  # per-pixel weights sum to 1, so the effective pixel count is exact
  expect_equal(sum(ovl$A01_count), 128L * 128L - 100L, tolerance = 1e-9)
  # weighted sum is mass-preserving: totals match the forward (unweighted) sum
  expect_equal(sum(ovl$A01_sum), sum(fwd$A01_sum), tolerance = 1e-9)
  expect_equal(sum(ovl$A02_sum), sum(fwd$A02_sum), tolerance = 1e-9)
})

test_that("overlay covers cells that contain no pixel centre", {
  f <- ovl_tif()
  skip_if(f == "")
  fwd <- a5_read_raster(f, resolution = 12L, stat = "count",
                        use_overviews = FALSE)
  ovl <- a5_read_raster(f, resolution = 12L, stat = "count",
                        mode = "overlay", use_overviews = FALSE)
  expect_true(all(hexset(fwd$cell) %in% hexset(ovl$cell)))
  extra <- setdiff(hexset(ovl$cell), hexset(fwd$cell))
  # sliver cells picked up only by area overlap carry fractional weight
  if (length(extra) > 0L) {
    expect_true(all(ovl$A01[match(extra, hexset(ovl$cell))] < 1))
  }
})

test_that("overlay weighted means match exactextract and converge with k", {
  skip_if_not_installed("exactextractr")
  skip_if_not_installed("sf")
  skip_if_not_installed("terra")
  f <- ovl_tif()
  skip_if(f == "")

  o2  <- a5_read_raster(f, resolution = 12L, mode = "overlay",
                        subsamples = 2L, use_overviews = FALSE)
  o16 <- a5_read_raster(f, resolution = 12L, mode = "overlay",
                        subsamples = 16L, use_overviews = FALSE)

  cells <- o16$cell
  polys <- sf::st_as_sf(
    data.frame(h = hexset(cells)),
    geom = sf::st_as_sfc(a5R::a5_cell_to_boundary(cells, segments = 16L))
  )
  ee <- exactextractr::exact_extract(terra::rast(f), polys, "mean",
                                     progress = FALSE)

  err_of <- function(o, band, ref) {
    m <- match(hexset(cells), hexset(o$cell))
    abs(o[[band]][m] - ref)
  }
  # measured on this fixture: k=16 max err ~0.02 (A01) / ~0.06 (A02) on a
  # value range of +-127; the convergence assertion below is the sharp check
  for (band in c("A01", "A02")) {
    ref <- ee[[paste0("mean.", band)]]
    expect_lt(max(err_of(o16, band, ref)), 0.1)
    # error shrinks with k (roughly quadratically; require at least 2x)
    expect_lt(mean(err_of(o16, band, ref)), mean(err_of(o2, band, ref)) / 2)
  }
})

test_that("a linear dequant commutes with the overlay weighted mean", {
  f <- ovl_tif()
  skip_if(f == "")
  lin <- a5_read_raster(f, resolution = 12L, mode = "overlay",
                        dequant = function(x) 2 * x + 1)
  raw <- a5_read_raster(f, resolution = 12L, mode = "overlay",
                        use_overviews = FALSE)
  j <- align_by_cell(lin, raw)
  expect_equal(j$a$A01, 2 * j$b$A01 + 1, tolerance = 1e-12)
})

test_that("overlay reaches the Arrow path", {
  skip_if_not_installed("arrow")
  f <- ovl_tif()
  skip_if(f == "")
  ref <- a5_read_raster(f, resolution = 12L, mode = "overlay",
                        subsamples = 4L, use_overviews = FALSE)
  tbl <- a5_read_raster_arrow(f, resolution = 12L, mode = "overlay",
                              subsamples = 4L, use_overviews = FALSE)
  df <- as.data.frame(tbl)
  df$cell <- a5R::a5_cell_from_arrow(tbl$cell)
  j <- align_by_cell(df, ref)
  expect_equal(nrow(j$a), nrow(ref))
  expect_equal(j$a$A01, j$b$A01, tolerance = 1e-12)
})

test_that("subsamples is validated and tied to overlay mode", {
  f <- ovl_tif()
  skip_if(f == "")
  expect_error(a5_read_raster(f, resolution = 12L, subsamples = 4L),
               "only used when")
  expect_error(a5_read_raster(f, resolution = 12L, mode = "centroid",
                              subsamples = 4L),
               "only used when")
  expect_error(a5_read_raster(f, resolution = 12L, mode = "overlay",
                              subsamples = 1L),
               "2..64")
  expect_error(a5_read_raster(f, resolution = 12L, mode = "overlay",
                              subsamples = 65L),
               "2..64")
})
