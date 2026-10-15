ov_tif <- function() system.file("extdata", "overview_cog.tif", package = "a5px")
hexkey <- function(cells) a5R::a5_u64_to_hex(cells)
ord <- function(df) df[order(hexkey(df$cell)), , drop = FALSE]

# An irregular polygon well inside the fixture's envelope: the central
# region with a notch cut out of one side.
fixture_aoi <- function() {
  b <- a5_raster_info(ov_tif())$bbox
  fx <- function(t) b[1] + t * (b[3] - b[1])
  fy <- function(t) b[2] + t * (b[4] - b[2])
  wk::wkt(sprintf(
    "POLYGON((%f %f, %f %f, %f %f, %f %f, %f %f, %f %f, %f %f))",
    fx(0.2), fy(0.2), fx(0.8), fy(0.2), fx(0.8), fy(0.8),
    fx(0.55), fy(0.8), fx(0.55), fy(0.5), fx(0.2), fy(0.8),
    fx(0.2), fy(0.2)
  ))
}

expected_cells <- function(res, containment) {
  a5R::a5_uncompact(
    a5R::a5_polygon_to_cells(fixture_aoi(), res, containment = containment),
    resolution = res
  )
}

test_that("containment is only accepted with aoi", {
  f <- ov_tif()
  skip_if(f == "")
  expect_error(a5_read_raster(f, 12L, containment = "overlapping"), "only used together")
})

test_that("forward mode returns exactly the AOI cells with full-cell statistics", {
  f <- ov_tif()
  skip_if(f == "")
  res <- 13L
  full <- ord(a5_read_raster(f, res, stat = c("mean", "count"), use_overviews = FALSE))
  for (cont in c("centre", "overlapping")) {
    out <- ord(a5_read_raster(f, res, aoi = fixture_aoi(), containment = cont,
                              stat = c("mean", "count"), use_overviews = FALSE))
    expect_setequal(hexkey(out$cell), hexkey(expected_cells(res, cont)))
    # every included cell carries the same statistics as in the unmasked read
    sub <- full[hexkey(full$cell) %in% hexkey(out$cell), ]
    expect_equal(out, sub, ignore_attr = TRUE, info = cont)
  }
  ctr <- expected_cells(res, "centre")
  ovl <- expected_cells(res, "overlapping")
  expect_true(all(hexkey(ctr) %in% hexkey(ovl)))
  expect_gt(length(ovl), length(ctr))
})

test_that("overlay mode with aoi matches the unmasked overlay read on included cells", {
  f <- ov_tif()
  skip_if(f == "")
  res <- 13L
  full <- ord(a5_read_raster(f, res, mode = "overlay", subsamples = 4L,
                             stat = c("mean", "count"), use_overviews = FALSE))
  out <- ord(a5_read_raster(f, res, mode = "overlay", subsamples = 4L,
                            aoi = fixture_aoi(), containment = "overlapping",
                            stat = c("mean", "count"), use_overviews = FALSE))
  expect_setequal(hexkey(out$cell), hexkey(expected_cells(res, "overlapping")))
  sub <- full[hexkey(full$cell) %in% hexkey(out$cell), ]
  expect_equal(out, sub, ignore_attr = TRUE)
})

test_that("centroid mode samples the AOI cells directly", {
  f <- ov_tif()
  skip_if(f == "")
  res <- 14L
  for (cont in c("centre", "overlapping")) {
    out <- a5_read_raster(f, res, mode = "centroid", aoi = fixture_aoi(),
                          containment = cont)
    expect_setequal(hexkey(out$cell), hexkey(expected_cells(res, cont)))
  }
  # a bbox further restricts the sampled centroids
  b <- a5_raster_info(f)$bbox
  half <- c(b[1], b[2], b[1] + 0.5 * (b[3] - b[1]), b[4])
  out <- a5_read_raster(f, res, mode = "centroid", aoi = fixture_aoi(), bbox = half)
  ll <- a5R::a5_cell_to_lonlat(out$cell, as_dataframe = TRUE)
  expect_true(all(ll$lon <= half[3]))
  expect_true(all(hexkey(out$cell) %in% hexkey(expected_cells(res, "centre"))))
})

test_that("aoi combines with bbox chunking", {
  f <- ov_tif()
  skip_if(f == "")
  res <- 13L
  ref <- ord(a5_read_raster(f, res, aoi = fixture_aoi(), containment = "overlapping",
                            stat = c("sum", "count"), use_overviews = FALSE))
  b <- a5_raster_info(f)$bbox
  xm <- b[1] + 0.5 * (b[3] - b[1])
  left <- c(b[1], b[2], xm, b[4])
  right <- c(xm, b[2], b[3], b[4])
  parts <- lapply(list(left, right), function(bb) {
    a5_read_raster(f, res, aoi = fixture_aoi(), containment = "overlapping",
                   bbox = bb, bbox_align = "block",
                   stat = c("sum", "count"), use_overviews = FALSE)
  })
  all <- vctrs::vec_rbind(!!!parts)
  key <- hexkey(all$cell)
  got <- data.frame(key = names(tapply(all[[2]], key, sum)),
                    sum = as.numeric(tapply(all[[2]], key, sum)),
                    count = as.numeric(tapply(all[[3]], key, sum)))
  got <- got[order(got$key), ]
  expect_equal(got$key, hexkey(ref$cell))
  expect_equal(got$count, ref[[3]])
  expect_equal(got$sum, ref[[2]], tolerance = 1e-9)
})

test_that("aoi works through the Arrow and Parquet paths", {
  skip_if_not_installed("arrow")
  f <- ov_tif()
  skip_if(f == "")
  res <- 13L
  ref <- a5_read_raster(f, res, aoi = fixture_aoi(), use_overviews = FALSE)
  tbl <- a5_read_raster_arrow(f, res, aoi = fixture_aoi(), use_overviews = FALSE)
  expect_setequal(
    hexkey(a5R::a5_cell_from_arrow(tbl$cell)),
    hexkey(ref$cell)
  )
  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_raster_to_parquet(f, dest, res, aoi = fixture_aoi(), use_overviews = FALSE)
  pq <- arrow::read_parquet(dest, as_data_frame = FALSE)
  expect_equal(nrow(pq), nrow(ref))
  # centroid + aoi through Arrow
  tbl_c <- a5_read_raster_arrow(f, 14L, mode = "centroid", aoi = fixture_aoi(),
                                containment = "overlapping")
  expect_setequal(
    hexkey(a5R::a5_cell_from_arrow(tbl_c$cell)),
    hexkey(expected_cells(14L, "overlapping"))
  )
})

test_that("an aoi outside the raster yields no cells", {
  f <- ov_tif()
  skip_if(f == "")
  far <- wk::rct(100, 10, 101, 11)
  out <- a5_read_raster(f, 12L, aoi = far, use_overviews = FALSE)
  expect_equal(nrow(out), 0L)
})

test_that("an aoi covering the globe selects every cell", {
  f <- ov_tif()
  skip_if(f == "")
  for (mode in c("forward", "centroid")) {
    full <- ord(a5_read_raster(f, 12L, mode = mode, use_overviews = FALSE))
    out <- ord(a5_read_raster(f, 12L, aoi = wk::rct(-180, -90, 180, 90),
                              mode = mode, use_overviews = FALSE))
    expect_gt(nrow(out), 0L)
    expect_equal(out, full, ignore_attr = TRUE, info = mode)
  }
})

test_that("the AOI tile box holds the bulge of great-circle edges", {
  # a 20 degree wide band: its top edge, a great circle, peaks at 61.37 N
  aoi <- wk::wkt("POLYGON((-10 59, 10 59, 10 61, -10 61, -10 59))",
                 crs = wk::wk_crs_longlat())
  bb <- a5px:::aoi_tile_bbox(aoi, 11L)
  expect_gt(bb[4], 61.37)
  # every cell of the covering has its centre inside the tile box
  cells <- a5R::a5_uncompact(a5R::a5_polygon_to_cells(aoi, resolution = 8L), resolution = 8L)
  ll <- a5R::a5_cell_to_lonlat(cells, as_dataframe = TRUE)
  bb8 <- a5px:::aoi_tile_bbox(aoi, 8L)
  expect_true(all(ll$lat >= bb8[2] & ll$lat <= bb8[4] & ll$lon >= bb8[1] & ll$lon <= bb8[3]))
})

test_that("an AOI across the antimeridian widens the tile box to every longitude", {
  aoi <- wk::wkt("POLYGON((179 -0.5, 181 -0.5, 181 0.5, 179 0.5, 179 -0.5))",
                 crs = wk::wk_crs_longlat())
  bb <- a5px:::aoi_tile_bbox(aoi, 9L)
  expect_equal(bb[c(1, 3)], c(-180, 180))
})

test_that("an AOI band whose edges bulge past its vertices keeps every covering cell", {
  skip_if_not_installed("terra")
  # raster lon -10..10, lat 58..62; the AOI's top edge peaks at 61.37 N
  r <- terra::rast(nrows = 400, ncols = 2000, crs = "EPSG:4326",
                   extent = terra::ext(-10, 10, 58, 62))
  terra::values(r) <- 1
  names(r) <- "v"
  f <- withr::local_tempfile(fileext = ".tif")
  terra::writeRaster(r, f, gdal = c("TILED=YES", "BLOCKXSIZE=256", "BLOCKYSIZE=256"))
  aoi <- wk::wkt("POLYGON((-10 59, 10 59, 10 61, -10 61, -10 59))",
                 crs = wk::wk_crs_longlat())
  got <- a5_read_raster(f, 9L, aoi = aoi, stat = "count")
  covering <- a5R::a5_uncompact(a5R::a5_polygon_to_cells(aoi, resolution = 9L), resolution = 9L)
  ll <- a5R::a5_cell_to_lonlat(covering, as_dataframe = TRUE)
  # covering cells centred on the raster all come back
  on_raster <- covering[ll$lon > -10 & ll$lon < 10 & ll$lat > 58 & ll$lat < 62]
  expect_true(all(a5R::a5_u64_to_hex(on_raster) %in% a5R::a5_u64_to_hex(got$cell)))
  expect_gt(max(a5R::a5_cell_to_lonlat(got$cell, as_dataframe = TRUE)$lat), 61.1)
})
