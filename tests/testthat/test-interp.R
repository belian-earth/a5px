# Interpolated centroid sampling. Uses aef_int8.tif (see test-dequant.R;
# internal 64x64 tiles, so stencils near col/row 64 straddle tile
# boundaries) and overview_cog.tif (float32, value linear in position, so
# any kernel that reproduces linear functions must return the exact plane).
interp_tif <- function() {
  system.file("extdata", "aef_int8.tif", package = "a5px")
}

hexset <- function(cells) a5R::a5_u64_to_hex(cells)

test_that("bilinear centroid sampling matches terra::extract", {
  skip_if_not_installed("terra")
  f <- interp_tif()
  skip_if(f == "")
  bl <- a5_read_raster(f, resolution = 16L, mode = "centroid",
                       interp = "bilinear")
  xy <- a5R::a5_cell_to_lonlat(bl$cell, as_dataframe = TRUE)
  tv <- terra::extract(terra::rast(f), cbind(xy$lon, xy$lat),
                       method = "bilinear")
  ok <- !is.na(tv$A01)
  expect_gt(sum(ok), 1000L)
  expect_equal(bl$A01[ok], tv$A01[ok], tolerance = 1e-6)
  expect_equal(bl$A02[!is.na(tv$A02)], tv$A02[!is.na(tv$A02)],
               tolerance = 1e-6)
})

test_that("interp = 'nearest' reproduces the plain centroid sample", {
  f <- interp_tif()
  skip_if(f == "")
  a <- a5_read_raster(f, resolution = 16L, mode = "centroid")
  b <- a5_read_raster(f, resolution = 16L, mode = "centroid",
                      interp = "nearest")
  ord <- function(df) df[order(hexset(df$cell)), , drop = FALSE]
  expect_equal(ord(a), ord(b))
})

test_that("smooth kernels reproduce a linear plane at cell centroids", {
  skip_if_not_installed("terra")
  f <- system.file("extdata", "overview_cog.tif", package = "a5px")
  skip_if(f == "")
  # recover the plane analytically: the raster value is linear in position,
  # so an exact least-squares fit on pixel centres has zero residual
  r <- terra::rast(f)
  xy <- terra::xyFromCell(r, seq_len(terra::ncell(r)))
  v <- terra::values(r, mat = FALSE)
  fit <- stats::lm(v ~ xy[, 1] + xy[, 2])
  expect_lt(max(abs(stats::residuals(fit))), 1e-3)

  for (kern in c("bilinear", "bicubic")) {
    got <- a5_read_raster(f, resolution = 16L, mode = "centroid",
                          interp = kern)
    cxy <- a5R::a5_cell_to_lonlat(got$cell, as_dataframe = TRUE)
    pxy <- terra::project(
      terra::vect(cbind(cxy$lon, cxy$lat), crs = "EPSG:4326"),
      terra::crs(r)
    )
    m <- terra::crds(pxy)
    pred <- fit$coefficients[[1]] + fit$coefficients[[2]] * m[, 1] +
      fit$coefficients[[3]] * m[, 2]
    # interior cells only: edge stencils renormalise over a partial kernel
    px <- terra::res(r)[1]
    e <- terra::ext(r)
    interior <- m[, 1] > e$xmin + 4 * px & m[, 1] < e$xmax - 4 * px &
      m[, 2] > e$ymin + 4 * px & m[, 2] < e$ymax - 4 * px
    expect_gt(sum(interior), 500L)
    expect_equal(got[[2]][interior], unname(pred[interior]),
                 tolerance = 1e-5)
  }
  # lanczos is not exactly linear-reproducing; require close agreement
  got <- a5_read_raster(f, resolution = 16L, mode = "centroid",
                        interp = "lanczos")
  cxy <- a5R::a5_cell_to_lonlat(got$cell, as_dataframe = TRUE)
  pxy <- terra::project(
    terra::vect(cbind(cxy$lon, cxy$lat), crs = "EPSG:4326"),
    terra::crs(r)
  )
  m <- terra::crds(pxy)
  pred <- fit$coefficients[[1]] + fit$coefficients[[2]] * m[, 1] +
    fit$coefficients[[3]] * m[, 2]
  px <- terra::res(r)[1]
  e <- terra::ext(r)
  interior <- m[, 1] > e$xmin + 4 * px & m[, 1] < e$xmax - 4 * px &
    m[, 2] > e$ymin + 4 * px & m[, 2] < e$ymax - 4 * px
  value_range <- diff(range(v))
  expect_lt(max(abs(got[[2]][interior] - pred[interior])) / value_range, 0.01)
})

test_that("nodata stencil pixels renormalise instead of poisoning", {
  f <- interp_tif()
  skip_if(f == "")
  nn <- a5_read_raster(f, resolution = 16L, mode = "centroid")
  bl <- a5_read_raster(f, resolution = 16L, mode = "centroid",
                       interp = "bilinear")
  # every sampled value is finite; the smooth kernel may recover cells whose
  # centroid pixel is nodata but whose wider stencil is not
  expect_true(all(is.finite(bl$A01)))
  expect_gte(nrow(bl), nrow(nn))
})

test_that("dequant decodes stencil pixels before the kernel", {
  f <- interp_tif()
  skip_if(f == "")
  raw <- a5_read_raster(f, resolution = 16L, mode = "centroid",
                        interp = "bilinear")
  lin <- a5_read_raster(f, resolution = 16L, mode = "centroid",
                        interp = "bilinear", dequant = function(x) 2 * x + 1)
  key <- intersect(hexset(raw$cell), hexset(lin$cell))
  i <- match(key, hexset(raw$cell))
  j <- match(key, hexset(lin$cell))
  # a linear decode commutes with the (linear) kernel
  expect_equal(lin$A01[j], 2 * raw$A01[i] + 1, tolerance = 1e-12)
  # a nonlinear decode does not: decoding the interpolated raw values is a
  # different (wrong) quantity
  aef <- a5_read_raster(f, resolution = 16L, mode = "centroid",
                        interp = "bilinear", dequant = dequant_aef)
  k <- match(key, hexset(aef$cell))
  expect_gt(max(abs(aef$A01[k] - dequant_aef(raw$A01[i])), na.rm = TRUE), 1e-4)
})

test_that("interp is validated and tied to centroid mode", {
  f <- interp_tif()
  skip_if(f == "")
  expect_error(a5_read_raster(f, resolution = 12L, interp = "bilinear"),
               "only used when")
  expect_error(a5_read_raster(f, resolution = 12L, mode = "overlay",
                              interp = "bilinear"),
               "only used when")
  expect_error(a5_read_raster(f, resolution = 16L, mode = "centroid",
                              interp = "cubic"),
               "must be one of")
})
