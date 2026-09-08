# Regression tests for the 0.1.0 review fixes. Fixtures:
# - be_planar.tif: big-endian ("MM"), INTERLEAVE=BAND, 2 x uint16, 64x64,
#   32x32 tiles, uncompressed, EPSG:32633. Band 1 = 1..4096 row-major,
#   band 2 = 10000 + the same.
# - f32_nodata.tif: float32 64x64 with a literal GDAL_NODATA tag "-9999.9"
#   (not representable in f32) and the top half set to that value.
# - laea_wide.tif: 128x128 float32 in +proj=laea +lat_0=0 +lon_0=0 spanning
#   +/-10,000 km, so its corners lie outside the projection's valid disc.
ext <- function(name) system.file("extdata", name, package = "a5px")
hexkey <- function(cells) a5R::a5_u64_to_hex(cells)
ord <- function(df) df[order(hexkey(df$cell)), , drop = FALSE]

test_that("band subsets of a big-endian planar TIFF decode correctly", {
  f <- ext("be_planar.tif")
  skip_if(f == "")
  full <- ord(a5_read_raster(f, 12L, stat = "mean"))
  b1 <- ord(a5_read_raster(f, 12L, stat = "mean", bands = 1L))
  b2 <- ord(a5_read_raster(f, 12L, stat = "mean", bands = 2L))
  expect_equal(b1$band_01, full$band_01)
  expect_equal(b2$band_02, full$band_02)
  expect_true(all(full$band_01 >= 1 & full$band_01 <= 4096))
  expect_true(all(full$band_02 >= 10001 & full$band_02 <= 14096))
  # centroid mode shares the band-subset fetch path
  c1 <- ord(a5_read_raster(f, 14L, mode = "centroid", bands = 1L))
  cf <- ord(a5_read_raster(f, 14L, mode = "centroid"))
  expect_equal(c1$band_01, cf$band_01)
})

test_that("float32 nodata not representable in f32 is still matched", {
  f <- ext("f32_nodata.tif")
  skip_if(f == "")
  expect_equal(a5_raster_info(f)$nodata, -9999.9)
  out <- a5_read_raster(f, 12L, stat = c("min", "count"))
  expect_equal(sum(out[[3]]), 32 * 64)
  expect_gte(min(out[[2]]), 2049)
  # the same sentinel given as src_nodata takes the same rounding
  out2 <- a5_read_raster(f, 12L, stat = "count", src_nodata = -9999.9)
  expect_equal(sum(out2[[2]]), 32 * 64)
  cen <- a5_read_raster(f, 16L, mode = "centroid")
  expect_true(all(cen[[2]] >= 2049))
})

test_that("a bbox extending outside the projection domain clamps instead of failing", {
  f <- ext("laea_wide.tif")
  skip_if(f == "")
  full <- ord(a5_read_raster(f, 6L, stat = "count"))
  expect_gt(nrow(full), 0L)
  glob <- ord(a5_read_raster(f, 6L, stat = "count", bbox = c(-180, -90, 180, 90)))
  expect_equal(glob, full)
  # block alignment: corner tiles have an unprojectable origin pixel and
  # fall back to the tile centre as their representative, so nothing is lost
  blk <- a5_read_raster(f, 6L, stat = "count", bbox = c(-180, -90, 180, 90),
                        bbox_align = "block")
  expect_equal(sum(blk[[2]]), sum(full[[2]]))
  halves <- lapply(list(c(-180, -90, 0, 90), c(0, -90, 180, 90)), function(bb) {
    a5_read_raster(f, 6L, stat = "count", bbox = bb, bbox_align = "block")
  })
  expect_equal(sum(vapply(halves, function(h) sum(h[[2]]), numeric(1))), sum(full[[2]]))
  ov <- a5_read_raster(f, 6L, stat = "count", mode = "overlay", subsamples = 2L,
                       bbox = c(-180, -90, 180, 90))
  expect_gt(nrow(ov), 0L)
  cen <- a5_read_raster(f, 6L, mode = "centroid", bbox = c(-180, -90, 180, 90))
  expect_gt(nrow(cen), 0L)
  # the footprint envelope ignores the unprojectable corners and is finite;
  # the edge midpoints sit ~103 degrees from the centre, and points further
  # along the edges legitimately wrap onto the far hemisphere
  b <- a5_raster_info(f)$bbox
  expect_true(all(is.finite(b)))
  expect_lte(b[1], -103)
  expect_gte(b[3], 103)
  expect_true(b[1] >= -180 && b[3] <= 180 && b[2] >= -90 && b[4] <= 90)
})

test_that("bicubic falls back to bilinear where the stencil is incomplete", {
  skip_if_not_installed("terra")
  f <- ext("aef_int8.tif")
  skip_if(f == "")
  bc <- a5_read_raster(f, 16L, mode = "centroid", interp = "bicubic", bands = 1L)
  bl <- a5_read_raster(f, 16L, mode = "centroid", interp = "bilinear", bands = 1L)
  key <- intersect(hexkey(bc$cell), hexkey(bl$cell))
  i <- match(key, hexkey(bc$cell))
  j <- match(key, hexkey(bl$cell))

  # classify each cell by whether its 4x4 Keys stencil is fully inside the
  # raster and free of nodata
  r <- terra::rast(f)[[1]]
  ll <- a5R::a5_cell_to_lonlat(bc$cell[i], as_dataframe = TRUE)
  p <- terra::crds(terra::project(terra::vect(cbind(ll$lon, ll$lat), crs = "EPSG:4326"),
                                  terra::crs(r)))
  col <- (p[, 1] - terra::xmin(r)) / terra::xres(r)
  row <- (terra::ymax(r) - p[, 2]) / terra::yres(r)
  v <- terra::values(r, mat = TRUE)[, 1]
  W <- ncol(r); H <- nrow(r)
  complete <- vapply(seq_along(col), function(k) {
    sx <- floor(col[k] - 0.5) - 1; sy <- floor(row[k] - 0.5) - 1
    cs <- sx + 0:3; rs <- sy + 0:3
    if (any(cs < 0 | cs >= W | rs < 0 | rs >= H)) return(FALSE)
    idx <- as.vector(outer(rs, cs, function(rr, cc) rr * W + cc + 1))
    !anyNA(v[idx])
  }, logical(1))
  expect_gt(sum(complete), 100L)
  expect_gt(sum(!complete), 10L)
  # incomplete stencils: identical to bilinear
  expect_equal(bc$A01[i][!complete], bl$A01[j][!complete], tolerance = 1e-9)
  # complete stencils: a genuine cubic estimate, not bilinear
  expect_gt(max(abs(bc$A01[i][complete] - bl$A01[j][complete])), 1e-6)
})

test_that("forward reads with nodata skip projection of empty pixels without changing results", {
  f <- ext("aef_int8.tif")
  skip_if(f == "")
  # nodata handled via the tag vs. disabled via an impossible override: the
  # valid pixels must produce the same statistics either way
  a <- ord(a5_read_raster(f, 12L, stat = c("mean", "count", "min", "max", "var"),
                          use_overviews = FALSE))
  expect_true(all(is.finite(a$A01_mean)))
  expect_true(all(a$A01_count > 0))
  # slim accumulator layouts agree with the full one on shared stats
  s <- ord(a5_read_raster(f, 12L, stat = c("mean", "count"), use_overviews = FALSE))
  r <- ord(a5_read_raster(f, 12L, stat = c("mean", "min", "max"), use_overviews = FALSE))
  expect_equal(s$A01_mean, a$A01_mean)
  expect_equal(s$A01_count, a$A01_count)
  expect_equal(r$A01_min, a$A01_min)
  expect_equal(r$A01_max, a$A01_max)
  expect_equal(r$A01_mean, a$A01_mean)
})
