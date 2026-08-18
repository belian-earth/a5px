# Centroid mode on the Arrow and Parquet paths. Parity is the claim under
# test: both must reproduce a5_read_raster(mode = "centroid") exactly, for
# nearest and for the smooth kernels. Uses the aef_int8.tif fixture
# documented in test-dequant.R.
aef_tif <- function() {
  system.file("extdata", "aef_int8.tif", package = "a5px")
}

hexset <- function(cells) a5R::a5_u64_to_hex(cells)

arrow_to_df <- function(tbl) {
  df <- as.data.frame(tbl)
  df$cell <- a5R::a5_cell_from_arrow(tbl$cell)
  df
}

align_by_cell <- function(a, b) {
  key_a <- hexset(a$cell)
  key_b <- hexset(b$cell)
  common <- intersect(key_a, key_b)
  list(
    a = a[match(common, key_a), , drop = FALSE],
    b = b[match(common, key_b), , drop = FALSE]
  )
}

test_that("arrow centroid matches the tibble path (nearest and bilinear)", {
  skip_if_not_installed("arrow")
  f <- aef_tif()
  skip_if(f == "")

  for (k in c("nearest", "bilinear")) {
    ref <- a5_read_raster(f, resolution = 16L, mode = "centroid", interp = k)
    tbl <- a5_read_raster_arrow(f, resolution = 16L, mode = "centroid",
                                interp = k)
    j <- align_by_cell(arrow_to_df(tbl), ref)
    expect_equal(nrow(j$a), nrow(ref))
    expect_equal(j$a$A01, j$b$A01, tolerance = 1e-12)
    expect_equal(j$a$A02, j$b$A02, tolerance = 1e-12)
  }
})

test_that("arrow centroid records the pseudo-stat and supports as_vector", {
  skip_if_not_installed("arrow")
  f <- aef_tif()
  skip_if(f == "")

  tbl <- a5_read_raster_arrow(f, resolution = 16L, mode = "centroid")
  expect_setequal(names(tbl), c("cell", "A01", "A02"))
  expect_equal(tbl$schema$metadata$a5px_stat, "centroid")
  expect_equal(tbl$schema$metadata$a5px_layout, "wide")

  fsl <- a5_read_raster_arrow(f, resolution = 16L, mode = "centroid",
                              as_vector = TRUE)
  expect_setequal(names(fsl), c("cell", "value"))
  expect_match(fsl$value$type$ToString(),
               "fixed_size_list<[A-Za-z]+: double>\\[2\\]")
  rows <- fsl$value$as_vector()
  mat <- do.call(rbind, lapply(rows, as.numeric))
  wide <- arrow_to_df(tbl)
  ord <- match(hexset(a5R::a5_cell_from_arrow(fsl$cell)), hexset(wide$cell))
  expect_false(anyNA(ord))
  expect_equal(mat[, 1], wide$A01[ord], tolerance = 1e-12)
  expect_equal(mat[, 2], wide$A02[ord], tolerance = 1e-12)
})

test_that("parquet centroid (Rust-direct) matches the tibble path", {
  skip_if_not_installed("arrow")
  f <- aef_tif()
  skip_if(f == "")

  ref <- a5_read_raster(f, resolution = 16L, mode = "centroid",
                        interp = "bicubic", dequant = dequant_aef)
  dest <- withr::local_tempfile(fileext = ".parquet")
  a5_raster_to_parquet(f, dest, resolution = 16L, mode = "centroid",
                       interp = "bicubic", dequant = dequant_aef)

  back <- arrow::read_parquet(dest, as_data_frame = FALSE)
  expect_equal(back$schema$metadata$a5px_stat, "centroid")
  df <- arrow_to_df(back)
  j <- align_by_cell(df, ref)
  expect_equal(nrow(j$a), nrow(ref))
  expect_equal(j$a$A01, j$b$A01, tolerance = 1e-12)
  expect_equal(j$a$A02, j$b$A02, tolerance = 1e-12)
})

test_that("centroid bbox subset restricts the sampled cells", {
  skip_if_not_installed("arrow")
  f <- aef_tif()
  skip_if(f == "")

  bb <- c(12.02, 48.02, 12.05, 48.05)
  ref <- a5_read_raster(f, resolution = 16L, mode = "centroid", bbox = bb)
  tbl <- a5_read_raster_arrow(f, resolution = 16L, mode = "centroid",
                              bbox = bb)
  df <- arrow_to_df(tbl)
  expect_setequal(hexset(df$cell), hexset(ref$cell))
  full <- a5_read_raster_arrow(f, resolution = 16L, mode = "centroid")
  expect_lt(nrow(df), full$num_rows)
})

test_that("interp is validated and tied to centroid mode on the arrow paths", {
  skip_if_not_installed("arrow")
  f <- aef_tif()
  skip_if(f == "")
  dest <- withr::local_tempfile(fileext = ".parquet")

  expect_error(
    a5_read_raster_arrow(f, resolution = 12L, interp = "bilinear"),
    "only used when"
  )
  expect_error(
    a5_read_raster_arrow(f, resolution = 12L, mode = "overlay",
                         interp = "lanczos"),
    "only used when"
  )
  expect_error(
    a5_raster_to_parquet(f, dest, resolution = 12L, interp = "bicubic"),
    "only used when"
  )
  expect_error(
    a5_read_raster_arrow(f, resolution = 16L, mode = "centroid",
                         subsamples = 4L),
    "only used when"
  )
  expect_warning(
    a5_read_raster_arrow(f, resolution = 16L, mode = "centroid",
                         stat = "sum"),
    "one sample per cell"
  )
})
