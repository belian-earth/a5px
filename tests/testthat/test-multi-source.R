ov_tif <- function() system.file("extdata", "overview_cog.tif", package = "a5px")
hexkey <- function(cells) a5R::a5_u64_to_hex(cells)
ord <- function(df) df[order(hexkey(df$cell)), , drop = FALSE]

write_tiled <- function(x, path) {
  terra::writeRaster(x, path, overwrite = TRUE,
                     gdal = c("TILED=YES", "BLOCKXSIZE=128", "BLOCKYSIZE=128"))
  path
}

# Fixtures derived from the bundled raster: a three-band stack, each of its
# bands as a single-band file, the left and right halves of the first band
# split on a pixel boundary, and a copy of the stack at twice the pixel size.
fixtures <- function() {
  skip_if_not_installed("terra")
  f <- ov_tif()
  skip_if(f == "")
  dir <- withr::local_tempdir(.local_envir = parent.frame())
  r <- terra::rast(f)
  stack <- c(r, r * 2 + 1, sqrt(abs(r)))
  names(stack) <- c("a", "b", "c")
  ext <- terra::ext(r)
  mid <- ext$xmin + 256 * terra::res(r)[1]
  list(
    stack = write_tiled(stack, file.path(dir, "stack.tif")),
    a = write_tiled(stack[["a"]], file.path(dir, "a.tif")),
    b = write_tiled(stack[["b"]], file.path(dir, "b.tif")),
    c = write_tiled(stack[["c"]], file.path(dir, "c.tif")),
    whole = write_tiled(stack[["a"]], file.path(dir, "whole.tif")),
    left = write_tiled(terra::crop(stack[["a"]], terra::ext(ext$xmin, mid, ext$ymin, ext$ymax)),
                       file.path(dir, "left.tif")),
    right = write_tiled(terra::crop(stack[["a"]], terra::ext(mid, ext$xmax, ext$ymin, ext$ymax)),
                        file.path(dir, "right.tif")),
    coarse = write_tiled(terra::aggregate(stack[["a"]], 2), file.path(dir, "coarse.tif"))
  )
}

all_stats <- c("mean", "sum", "count", "min", "max", "sd")

test_that("separate single-band files equal the multi-band read", {
  fx <- fixtures()
  full <- ord(a5_read_raster(fx$stack, 13L, stat = all_stats, use_overviews = FALSE))
  sep <- ord(a5_read_raster(c(a = fx$a, b = fx$b, c = fx$c), 13L, stat = all_stats,
                            separate = TRUE, use_overviews = FALSE))
  expect_equal(sep, full, ignore_attr = TRUE)
  expect_named(sep, names(full))
})

test_that("separate labels default to file names and select bands from the combined list", {
  fx <- fixtures()
  sep <- a5_read_raster(c(fx$a, fx$stack), 13L, separate = TRUE, use_overviews = FALSE)
  expect_named(sep, c("cell", "a", "stack_a", "stack_b", "stack_c"))
  sub <- a5_read_raster(c(fx$a, fx$stack), 13L, separate = TRUE, use_overviews = FALSE,
                        bands = c("stack_c", "a"))
  expect_named(sub, c("cell", "stack_c", "a"))
  expect_equal(ord(sub)$a, ord(sep)$a)
  expect_equal(ord(sub)$stack_c, ord(sep)$stack_c)
})

test_that("a raster split into halves pools to the whole read, for every stat", {
  fx <- fixtures()
  whole <- ord(a5_read_raster(fx$whole, 13L, stat = c(all_stats, "npix"), use_overviews = FALSE))
  halves <- ord(a5_read_raster(c(fx$left, fx$right), 13L, stat = c(all_stats, "npix"),
                               use_overviews = FALSE))
  expect_equal(halves, whole, ignore_attr = TRUE)
  # cells on the seam take pixels from both halves
  left <- a5_read_raster(fx$left, 13L, use_overviews = FALSE)
  right <- a5_read_raster(fx$right, 13L, use_overviews = FALSE)
  expect_gt(length(intersect(hexkey(left$cell), hexkey(right$cell))), 0L)
})

test_that("overlay mode pools halves to the whole read", {
  fx <- fixtures()
  whole <- ord(a5_read_raster(fx$whole, 13L, stat = c("mean", "sum"), mode = "overlay",
                              use_overviews = FALSE))
  halves <- ord(a5_read_raster(c(fx$left, fx$right), 13L, stat = c("mean", "sum"),
                               mode = "overlay", use_overviews = FALSE))
  expect_equal(halves, whole, ignore_attr = TRUE)
})

test_that("overlapping sources pool every capture", {
  fx <- fixtures()
  one <- ord(a5_read_raster(fx$whole, 13L, stat = c("mean", "count", "min"), use_overviews = FALSE))
  two <- ord(a5_read_raster(c(fx$whole, fx$a), 13L, stat = c("mean", "count", "min"),
                            use_overviews = FALSE))
  expect_equal(two$a_mean, one$a_mean)
  expect_equal(two$a_min, one$a_min)
  expect_equal(two$a_count, 2 * one$a_count)
})

test_that("centroid mode samples every source", {
  fx <- fixtures()
  full <- ord(a5_read_raster(fx$stack, 16L, mode = "centroid", interp = "bilinear"))
  sep <- ord(a5_read_raster(c(a = fx$a, b = fx$b, c = fx$c), 16L, mode = "centroid",
                            interp = "bilinear", separate = TRUE))
  expect_equal(sep, full, ignore_attr = TRUE)
  # pooled copies average identical samples
  one <- ord(a5_read_raster(fx$whole, 16L, mode = "centroid"))
  two <- ord(a5_read_raster(c(fx$whole, fx$a), 16L, mode = "centroid"))
  expect_equal(two, one, ignore_attr = TRUE)
})

test_that("Arrow and Parquet outputs take several sources", {
  skip_if_not_installed("arrow")
  fx <- fixtures()
  src <- c(a = fx$a, b = fx$b, c = fx$c)
  ref <- ord(a5_read_raster(src, 13L, separate = TRUE, use_overviews = FALSE))
  arr <- a5_read_raster_arrow(src, 13L, separate = TRUE, use_overviews = FALSE)
  expect_equal(arr$schema$names, c("cell", "a", "b", "c"))
  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_raster_to_parquet(src, dest, 13L, separate = TRUE, use_overviews = FALSE)
  pq <- as.data.frame(arrow::read_parquet(dest))
  expect_equal(nrow(pq), nrow(ref))
  expect_equal(sort(pq$b), sort(ref$b))
})

test_that("combining sources validates bands, pixel size, labels and npix", {
  fx <- fixtures()
  expect_error(a5_read_raster(c(fx$stack, fx$a), 13L), "same bands")
  expect_error(a5_read_raster(c(fx$stack, fx$a), 13L), "separate = TRUE")
  expect_error(a5_read_raster(c(fx$whole, fx$coarse), 13L), "same pixel size")
  expect_error(
    a5_read_raster(c(x = fx$a, x = fx$b), 13L, separate = TRUE),
    "distinct labels"
  )
  expect_error(
    a5_read_raster(c(fx$a, fx$b), 13L, separate = TRUE, stat = c("mean", "npix")),
    "npix"
  )
  expect_error(a5_read_raster(character(), 13L), "character vector")
  expect_error(a5_read_raster(fx$a, 13L, separate = NA), "separate")
})
