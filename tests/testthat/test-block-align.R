ov_tif <- function() system.file("extdata", "overview_cog.tif", package = "a5px")
hexkey <- function(cells) a5R::a5_u64_to_hex(cells)

# Split a WGS 84 envelope into an nx x ny grid of bboxes sharing edges.
bbox_grid <- function(b, nx, ny) {
  xs <- seq(b[1], b[3], length.out = nx + 1L)
  ys <- seq(b[2], b[4], length.out = ny + 1L)
  out <- list()
  for (i in seq_len(nx)) for (j in seq_len(ny)) {
    out[[length(out) + 1L]] <- c(xs[i], ys[j], xs[i + 1L], ys[j + 1L])
  }
  out
}

# Read a partition chunk by chunk and merge per-cell sum / count.
read_chunked <- function(f, res, chunks, ...) {
  parts <- lapply(chunks, function(b) {
    a5_read_raster(f, res, bbox = b, stat = c("sum", "count"), ...)
  })
  all <- vctrs::vec_rbind(!!!parts)
  key <- hexkey(all$cell)
  s <- tapply(all[[2]], key, sum)
  n <- tapply(all[[3]], key, sum)
  data.frame(key = names(s), sum = as.numeric(s), count = as.numeric(n))
}

test_that("a5_raster_info describes the fixture", {
  f <- ov_tif()
  skip_if(f == "")
  info <- a5_raster_info(f)
  expect_equal(info$width, 512L)
  expect_equal(info$height, 512L)
  expect_equal(info$n_bands, 1L)
  expect_equal(info$dtype, "float32")
  expect_equal(info$block, c(256L, 256L))
  expect_equal(info$n_blocks, c(2L, 2L))
  expect_equal(nrow(info$overviews), 2L)
  expect_equal(info$overviews$width, c(256L, 128L))
  expect_equal(info$crs, "EPSG:32633")
  expect_length(info$geotransform, 6L)
  expect_length(info$bbox, 4L)
  expect_lt(info$bbox[1], info$bbox[3])
  expect_lt(info$bbox[2], info$bbox[4])
  expect_equal(info$bbox, as.numeric(a5px:::a5_raster_bbox_lonlat_rs(f, character(), character())))
  expect_true(is.na(info$nodata))
  expect_equal(info$interleave, "pixel")
})

test_that("a5_raster_info reports nodata and band names when present", {
  f <- system.file("extdata", "nan_nodata.tif", package = "a5px")
  skip_if(f == "")
  info <- a5_raster_info(f)
  expect_equal(info$n_bands, 2L)
  expect_length(info$band_names, 2L)
  expect_equal(nrow(info$overviews), 0L)
})

test_that("bbox_align is validated", {
  f <- ov_tif()
  skip_if(f == "")
  expect_error(a5_read_raster(f, 12L, bbox_align = "block"), "requires .*bbox")
  expect_error(a5_read_raster(f, 12L, bbox_align = "tile"), "must be one of")
  b <- a5_raster_info(f)$bbox
  expect_error(
    a5_read_raster(f, 12L, bbox = b, bbox_align = "block", mode = "centroid"),
    "centroid"
  )
})

test_that("block-aligned chunks partition the raster exactly", {
  f <- ov_tif()
  skip_if(f == "")
  b <- a5_raster_info(f)$bbox
  res <- 13L
  ref <- a5_read_raster(f, res, stat = c("sum", "count"), use_overviews = FALSE)
  ref <- data.frame(key = hexkey(ref$cell), sum = ref[[2]], count = ref[[3]])
  ref <- ref[order(ref$key), ]

  for (grid in list(c(3L, 3L), c(2L, 2L), c(4L, 1L))) {
    chunks <- bbox_grid(b, grid[1], grid[2])
    got <- read_chunked(f, res, chunks, bbox_align = "block", use_overviews = FALSE)
    got <- got[order(got$key), ]
    expect_equal(got$key, ref$key, info = paste(grid, collapse = "x"))
    expect_equal(got$count, ref$count, info = paste(grid, collapse = "x"))
    expect_equal(got$sum, ref$sum, tolerance = 1e-9, info = paste(grid, collapse = "x"))
  }
})

test_that("block alignment reads whole blocks and the pixel default does not", {
  f <- ov_tif()
  skip_if(f == "")
  b <- a5_raster_info(f)$bbox
  # a bbox covering the left ~40% of the envelope: only the two left-hand
  # blocks have their origin (top-left pixel) inside, so block mode reads
  # exactly half the pixels; pixel mode reads roughly 40%.
  left <- c(b[1], b[2], b[1] + 0.4 * (b[3] - b[1]), b[4])
  blk <- a5_read_raster(f, 12L, bbox = left, bbox_align = "block",
                        stat = "count", use_overviews = FALSE)
  pix <- a5_read_raster(f, 12L, bbox = left, bbox_align = "pixel",
                        stat = "count", use_overviews = FALSE)
  expect_equal(sum(blk[[2]]), 2 * 256 * 256)
  expect_lt(sum(pix[[2]]), sum(blk[[2]]))
})

test_that("block-aligned chunks partition an overview level too", {
  f <- ov_tif()
  skip_if(f == "")
  b <- a5_raster_info(f)$bbox
  # res 11 selects the decimation-4 overview (128x128, one block); count
  # sums must still match the unchunked overview read.
  ref <- a5_read_raster(f, 11L, stat = "mean", use_overviews = TRUE)
  chunks <- bbox_grid(b, 2L, 2L)
  parts <- lapply(chunks, function(bb) {
    a5_read_raster(f, 11L, bbox = bb, bbox_align = "block",
                   stat = "mean", use_overviews = TRUE)
  })
  got <- vctrs::vec_rbind(!!!parts)
  expect_equal(sort(hexkey(got$cell)), sort(hexkey(ref$cell)))
})
