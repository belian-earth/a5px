# `stat = "npix"`: one per-cell column with the number of source pixels
# that had at least one valid band (overlay: their summed area weight).
ext <- function(name) system.file("extdata", name, package = "a5px")
ord <- function(df) {
  df <- as.data.frame(df)
  df$cell <- a5R::a5_u64_to_hex(df$cell)
  df[order(df$cell), , drop = FALSE]
}

test_that("npix equals every band's count when validity is uniform", {
  f <- ext("aef_int8.tif")  # int8 with a dataset-wide nodata sentinel
  skip_if(f == "")
  out <- ord(a5_read_raster(f, 14L, stat = c("mean", "count", "npix"), cpu_workers = 2L))
  expect_true("npix" %in% names(out))
  expect_identical(out$npix, out$A01_count)
  expect_identical(out$npix, out$A02_count)
  expect_true(all(out$npix >= 1))
  # alone, and in any position of the stat vector
  alone <- ord(a5_read_raster(f, 14L, stat = "npix"))
  expect_identical(names(alone), c("cell", "npix"))
  expect_identical(alone$npix, out$npix)
  first <- ord(a5_read_raster(f, 14L, stat = c("npix", "mean")))
  expect_identical(first$npix, out$npix)
  expect_identical(first$A01, out$A01_mean)
})

test_that("npix counts pixels with any valid band when bands differ", {
  f <- ext("nan_nodata.tif")  # NaN nodata, per band
  skip_if(f == "")
  out <- ord(a5_read_raster(f, 16L, stat = c("mean", "count", "npix")))
  cnt <- out[grep("_count$", names(out))]
  expect_true(all(out$npix >= do.call(pmax, cnt)))
  expect_true(all(out$npix <= Reduce(`+`, cnt)))
})

test_that("npix in overlay mode is the summed area weight", {
  f <- ext("aef_int8.tif")
  skip_if(f == "")
  out <- ord(a5_read_raster(f, 14L, stat = c("mean", "count", "npix"), mode = "overlay"))
  expect_equal(out$npix, out$A01_count, tolerance = 1e-12)
  fwd <- ord(a5_read_raster(f, 14L, stat = "npix"))
  # total weight is the number of valid pixels either way
  expect_equal(sum(out$npix), sum(fwd$npix), tolerance = 1e-9)
})

test_that("npix reaches the Arrow and Parquet outputs", {
  f <- ext("aef_int8.tif")
  skip_if(f == "")
  skip_if_not_installed("arrow")
  ref <- ord(a5_read_raster(f, 14L, stat = c("mean", "npix")))
  tbl <- a5_read_raster_arrow(f, 14L, stat = c("mean", "npix"))
  expect_true("npix" %in% names(tbl))
  df <- as.data.frame(tbl)
  df$cell <- a5R::a5_u64_to_hex(a5R::a5_cell_from_arrow(tbl$cell))
  df <- df[order(df$cell), ]
  expect_equal(df$npix, ref$npix)
  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_raster_to_parquet(f, dest, 14L, stat = c("mean", "npix"), value_type = "float32")
  pq <- as.data.frame(arrow::read_parquet(dest))
  expect_true("npix" %in% names(pq))
  expect_equal(sum(pq$npix), sum(ref$npix))
  # single-stat naming plus npix (a fresh path: arrow memory-maps the file
  # it just read, and Windows refuses to overwrite a mapped file)
  dest2 <- withr::local_tempfile(fileext = ".parquet")
  a5_raster_to_parquet(f, dest2, 14L, stat = c("mean", "npix"))
  expect_identical(tail(names(arrow::read_parquet(dest2)), 2), c("A02", "npix"))
})

test_that("npix is rejected with fractions", {
  f <- ext("aef_int8.tif")
  skip_if(f == "")
  expect_error(a5_read_raster(f, 14L, stat = c("fractions", "npix")), "fractions")
})
