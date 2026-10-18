ov_tif <- function() system.file("extdata", "overview_cog.tif", package = "a5px")
hexkey <- function(cells) a5R::a5_u64_to_hex(cells)
ord <- function(df) df[order(hexkey(df$cell)), , drop = FALSE]

# WKT of a ring at fractions (tx, ty) of the fixture's lon/lat envelope.
frac_ring <- function(tx, ty) {
  b <- a5_raster_info(ov_tif())$bbox
  x <- b[1] + tx * (b[3] - b[1])
  y <- b[2] + ty * (b[4] - b[2])
  sprintf("(%s)", paste(sprintf("%.10f %.10f", c(x, x[1]), c(y, y[1])), collapse = ", "))
}

triangle <- function() wk::wkt(sprintf("POLYGON (%s)", frac_ring(c(0.1, 0.9, 0.3), c(0.15, 0.3, 0.85))))

holed <- function() {
  wk::wkt(sprintf("POLYGON (%s, %s)", frac_ring(c(0.1, 0.9, 0.9, 0.1), c(0.1, 0.1, 0.9, 0.9)),
                  frac_ring(c(0.35, 0.6, 0.6, 0.35), c(0.3, 0.3, 0.7, 0.7))))
}

# The fixture with every pixel whose centre lies outside `mask` set to NaN,
# decided independently by s2, whose edges are also great circles.
masked_copy <- function(mask) {
  r <- terra::rast(ov_tif())
  xy <- terra::xyFromCell(r, seq_len(terra::ncell(r)))
  ll <- terra::project(xy, from = terra::crs(r), to = "EPSG:4326")
  inside <- s2::s2_intersects(s2::s2_lnglat(ll[, 1], ll[, 2]),
                              s2::s2_union_agg(s2::as_s2_geography(wk::wk_set_geodesic(mask, TRUE))))
  v <- terra::values(r, mat = FALSE)
  v[!inside] <- NA
  terra::values(r) <- v
  out <- withr::local_tempfile(fileext = ".tif", .local_envir = parent.frame())
  terra::writeRaster(r, out, datatype = "FLT4S",
                     gdal = c("TILED=YES", "BLOCKXSIZE=256", "BLOCKYSIZE=256"))
  attr(out, "n_inside") <- sum(inside & !is.na(terra::values(terra::rast(ov_tif()), mat = FALSE)))
  out
}

skip_mask_deps <- function() {
  skip_if(ov_tif() == "")
  skip_if_not_installed("terra")
  skip_if_not_installed("s2")
}

test_that("mask keeps exactly the pixels whose centre lies inside it", {
  skip_mask_deps()
  for (m in list(triangle(), holed())) {
    ref_tif <- masked_copy(m)
    st <- c("mean", "count", "npix", "min", "max")
    ref <- ord(a5_read_raster(ref_tif, 15L, stat = st, use_overviews = FALSE))
    got <- ord(a5_read_raster(ov_tif(), 15L, stat = st, mask = m, use_overviews = FALSE))
    expect_equal(got, ref, ignore_attr = TRUE)
    # count adds up to the kept pixels
    expect_equal(sum(got$band_01_count), attr(ref_tif, "n_inside"))
  }
})

test_that("a mask hole drops its pixels", {
  skip_mask_deps()
  outer <- wk::wkt(sprintf("POLYGON (%s)", frac_ring(c(0.1, 0.9, 0.9, 0.1), c(0.1, 0.1, 0.9, 0.9))))
  a <- sum(a5_read_raster(ov_tif(), 14L, stat = c("mean", "count"), mask = outer, use_overviews = FALSE)$band_01_count)
  b <- sum(a5_read_raster(ov_tif(), 14L, stat = c("mean", "count"), mask = holed(), use_overviews = FALSE)$band_01_count)
  expect_lt(b, a)
  # the hole is about 0.25 x 0.4 of the envelope, a sixth of the outer ring
  expect_equal(1 - b / a, 0.25 * 0.4 / 0.64, tolerance = 0.05)
})

test_that("several features form one mask", {
  skip_mask_deps()
  two <- wk::wkt(c(
    sprintf("POLYGON (%s)", frac_ring(c(0.1, 0.4, 0.4, 0.1), c(0.1, 0.1, 0.4, 0.4))),
    sprintf("POLYGON (%s)", frac_ring(c(0.5, 0.9, 0.9, 0.5), c(0.5, 0.5, 0.9, 0.9)))
  ))
  ref <- ord(a5_read_raster(masked_copy(two), 15L, stat = c("mean", "count"), use_overviews = FALSE))
  got <- ord(a5_read_raster(ov_tif(), 15L, stat = c("mean", "count"), mask = two, use_overviews = FALSE))
  expect_equal(got, ref, ignore_attr = TRUE)
  # an sf multipolygon of the same parts reads the same
  skip_if_not_installed("sf")
  mp <- sf::st_union(sf::st_as_sfc(as.character(two), crs = 4326))
  got_sf <- ord(a5_read_raster(ov_tif(), 15L, stat = c("mean", "count"), mask = mp, use_overviews = FALSE))
  expect_equal(got_sf, got, ignore_attr = TRUE)
})

test_that("mask combines with aoi, bbox, bbox_align and overlay mode", {
  skip_mask_deps()
  m <- triangle()
  ref_tif <- masked_copy(m)
  aoi <- wk::wkt(sprintf("POLYGON (%s)", frac_ring(c(0.2, 0.7, 0.7, 0.2), c(0.2, 0.2, 0.6, 0.6))))
  ref <- ord(a5_read_raster(ref_tif, 14L, stat = c("mean", "count"), aoi = aoi, use_overviews = FALSE))
  got <- ord(a5_read_raster(ov_tif(), 14L, stat = c("mean", "count"), aoi = aoi, mask = m,
                            use_overviews = FALSE))
  expect_gt(nrow(got), 0)
  expect_equal(got, ref, ignore_attr = TRUE)

  b <- a5_raster_info(ov_tif())$bbox
  bb <- c(b[1], b[2], (b[1] + b[3]) / 2, b[4])
  for (align in c("pixel", "block")) {
    ref <- ord(a5_read_raster(ref_tif, 15L, stat = "count", bbox = bb, bbox_align = align,
                              use_overviews = FALSE))
    got <- ord(a5_read_raster(ov_tif(), 15L, stat = "count", bbox = bb, bbox_align = align,
                              mask = m, use_overviews = FALSE))
    expect_equal(got, ref, ignore_attr = TRUE, info = align)
  }

  ref <- ord(a5_read_raster(ref_tif, 14L, stat = c("mean", "count"), mode = "overlay",
                            use_overviews = FALSE))
  got <- ord(a5_read_raster(ov_tif(), 14L, stat = c("mean", "count"), mode = "overlay",
                            mask = m, use_overviews = FALSE))
  expect_equal(got, ref, ignore_attr = TRUE)
})

test_that("mask works through the Arrow and Parquet paths", {
  skip_mask_deps()
  skip_if_not_installed("arrow")
  m <- triangle()
  ref <- ord(a5_read_raster(ov_tif(), 14L, stat = c("mean", "count"), mask = m))
  tbl <- a5_read_raster_arrow(ov_tif(), 14L, stat = c("mean", "count"), mask = m)
  expect_setequal(hexkey(a5R::a5_cell_from_arrow(tbl$cell)), hexkey(ref$cell))
  pq <- withr::local_tempfile(fileext = ".parquet")
  a5_raster_to_parquet(ov_tif(), pq, 14L, stat = c("mean", "count"), mask = m)
  expect_equal(nrow(arrow::read_parquet(pq)), nrow(ref))
})

test_that("a mask outside the raster gives an empty result", {
  skip_if(ov_tif() == "")
  far <- wk::wkt("POLYGON ((100 10, 101 10, 101 11, 100 11, 100 10))")
  out <- a5_read_raster(ov_tif(), 14L, stat = c("mean", "count"), mask = far)
  expect_equal(nrow(out), 0L)
})

test_that("mask rejects centroid mode, cells and projected input", {
  skip_if(ov_tif() == "")
  expect_error(a5_read_raster(ov_tif(), 14L, mode = "centroid", mask = triangle()), "centroid")
  cell <- a5R::a5_lonlat_to_cell(13.8, 45.2, 8L)
  expect_error(a5_read_raster(ov_tif(), 14L, mask = cell), "aoi")
  expect_error(a5_read_raster(ov_tif(), 14L, mask = wk::wkt("LINESTRING (0 0, 1 1)")),
               class = "a5px_error_geometry")
  skip_if_not_installed("sf")
  proj <- sf::st_transform(sf::st_as_sfc(as.character(triangle()), crs = 4326), 32633)
  expect_error(a5_read_raster(ov_tif(), 14L, mask = proj), class = "a5px_error_geometry")
})
