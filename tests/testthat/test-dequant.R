# aef_int8.tif: 128x128 2-band int8 GeoTIFF in EPSG:4326 (lon 12..12.1,
# lat 48..48.1, ~87 m pixels at 48N), tiled 64x64, deflate, nodata -128
# (a 10x10 top-left patch), band descriptions A01/A02, internal AVERAGE
# overviews at decimation 2 and 4. Values are deterministic in position
# (zero-based col c, row r): A01 = (3c + 7r) %% 255 - 127,
# A02 = (5c + 2r) %% 255 - 127, i.e. int8 codes spanning -127..127 that
# mimic Alpha Earth embedding quantization.
aef_tif <- function() {
  system.file("extdata", "aef_int8.tif", package = "a5px")
}

# local reference decode, deliberately independent of the dequant_aef export
aef_decode <- function(x) {
  sign(x) * (x / 127.5)^2
}

hexset <- function(cells) a5R::a5_u64_to_hex(cells)

edge_m <- function(res) {
  sqrt(as.numeric(a5R::a5_cell_area(res, units = "m^2")))
}

# join two reads on the cell key and return aligned data.frames
align_by_cell <- function(a, b) {
  key_a <- hexset(a$cell)
  key_b <- hexset(b$cell)
  common <- intersect(key_a, key_b)
  list(
    a = a[match(common, key_a), , drop = FALSE],
    b = b[match(common, key_b), , drop = FALSE]
  )
}

test_that("dequant = dequant_aef decodes per pixel before aggregating (terra reference)", {
  skip_if_not_installed("terra")
  f <- aef_tif()
  skip_if(f == "")

  got <- a5_read_raster(f, resolution = 12L, dequant = dequant_aef)

  # independent reference: terra pixel values + a5R cell assignment
  r <- terra::rast(f)
  vals <- terra::values(r)                       # ncell x 2, NA at nodata
  xy <- terra::xyFromCell(r, seq_len(terra::ncell(r)))
  cells <- a5R::a5_lonlat_to_cell(xy[, 1], xy[, 2], resolution = 12L)
  key <- hexset(cells)
  for (b in c("A01", "A02")) {
    ok <- !is.na(vals[, b])
    ref <- tapply(aef_decode(vals[ok, b]), key[ok], mean)
    expect_setequal(hexset(got$cell), names(ref))
    expect_equal(got[[b]][match(names(ref), hexset(got$cell))],
                 as.numeric(ref), tolerance = 1e-12)
  }

  # the premise: decoding after aggregation is NOT the same thing
  raw <- a5_read_raster(f, resolution = 12L, use_overviews = FALSE)
  j <- align_by_cell(got, raw)
  expect_gt(max(abs(j$a$A01 - aef_decode(j$b$A01))), 1e-3)
})

test_that("a linear dequant function commutes with the mean", {
  f <- aef_tif()
  skip_if(f == "")
  lin <- a5_read_raster(f, resolution = 12L, dequant = function(x) 2 * x + 1)
  raw <- a5_read_raster(f, resolution = 12L, use_overviews = FALSE)
  j <- align_by_cell(lin, raw)
  expect_equal(j$a$A01, 2 * j$b$A01 + 1, tolerance = 1e-12)
  expect_equal(j$a$A02, 2 * j$b$A02 + 1, tolerance = 1e-12)
})

test_that("monotone decode commutes with min/max; count is unchanged; sum/mean agree", {
  f <- aef_tif()
  skip_if(f == "")
  dq <- a5_read_raster(f, resolution = 12L, stat = c("min", "max", "sum", "count"),
                       dequant = dequant_aef)
  rw <- a5_read_raster(f, resolution = 12L, stat = c("min", "max", "count"),
                       use_overviews = FALSE)
  mn <- a5_read_raster(f, resolution = 12L, dequant = dequant_aef)
  j <- align_by_cell(dq, rw)
  expect_equal(j$a$A01_min, aef_decode(j$b$A01_min), tolerance = 1e-15)
  expect_equal(j$a$A01_max, aef_decode(j$b$A01_max), tolerance = 1e-15)
  expect_equal(j$a$A01_count, j$b$A01_count)
  k <- align_by_cell(dq, mn)
  expect_equal(k$a$A01_sum / k$a$A01_count, k$b$A01, tolerance = 1e-12)
})

test_that("dequant disables overview reads by default; explicit TRUE warns", {
  f <- aef_tif()
  skip_if(f == "")
  # an overview would be selected at this resolution if allowed
  expect_gt(a5px:::a5_select_overview_level_rs(f, edge_m(12L)), 0L)

  ord <- function(df) df[order(hexset(df$cell)), , drop = FALSE]
  d_def <- a5_read_raster(f, resolution = 12L, dequant = dequant_aef)
  d_off <- a5_read_raster(f, resolution = 12L, dequant = dequant_aef,
                          use_overviews = FALSE)
  expect_equal(ord(d_def), ord(d_off))

  expect_warning(
    a5_read_raster(f, resolution = 12L, dequant = dequant_aef, use_overviews = TRUE),
    "nearest or mode resampling"
  )
})

test_that("centroid mode applies dequant to the sampled value", {
  f <- aef_tif()
  skip_if(f == "")
  dq <- a5_read_raster(f, resolution = 16L, mode = "centroid", dequant = dequant_aef)
  rw <- a5_read_raster(f, resolution = 16L, mode = "centroid")
  j <- align_by_cell(dq, rw)
  expect_gt(nrow(j$a), 0L)
  expect_equal(j$a$A01, aef_decode(j$b$A01), tolerance = 1e-15)
  expect_equal(j$a$A02, aef_decode(j$b$A02), tolerance = 1e-15)
})

test_that("dequant reaches the Arrow and Parquet paths", {
  skip_if_not_installed("arrow")
  f <- aef_tif()
  skip_if(f == "")
  ref <- a5_read_raster(f, resolution = 12L, dequant = dequant_aef)

  tbl <- a5_read_raster_arrow(f, resolution = 12L, dequant = dequant_aef)
  arrow_cells <- a5R::a5_cell_from_arrow(tbl$cell)
  df <- as.data.frame(tbl)
  df$cell <- arrow_cells
  j <- align_by_cell(df, ref)
  expect_equal(nrow(j$a), nrow(ref))
  expect_equal(j$a$A01, j$b$A01, tolerance = 1e-12)
  expect_equal(j$a$A02, j$b$A02, tolerance = 1e-12)

  dest <- tempfile(fileext = ".parquet")
  on.exit(unlink(dest), add = TRUE)
  a5_raster_to_parquet(f, dest, resolution = 12L, dequant = dequant_aef)
  back <- as.data.frame(arrow::read_parquet(dest, as_data_frame = FALSE))
  back$cell <- a5R::a5_cell_from_arrow(
    arrow::read_parquet(dest, as_data_frame = FALSE)$cell
  )
  k <- align_by_cell(back, ref)
  expect_equal(nrow(k$a), nrow(ref))
  expect_equal(k$a$A01, k$b$A01, tolerance = 1e-12)
})

test_that("dequant rejects non-integer sources and bad arguments", {
  f <- aef_tif()
  skip_if(f == "")
  ovf <- system.file("extdata", "overview_cog.tif", package = "a5px")
  skip_if(ovf == "")
  # overview_cog.tif is float32: dequantization is undefined there
  expect_error(
    a5_read_raster(ovf, resolution = 12L, dequant = dequant_aef),
    "16 bits or fewer"
  )

  expect_error(a5_read_raster(f, resolution = 12L, dequant = "aef"),
               "vectorised function")
  expect_error(a5_read_raster(f, resolution = 12L, dequant = 42),
               "vectorised function")
  expect_error(a5_read_raster(f, resolution = 12L, dequant = function(x) 1),
               "vectorised")
})

test_that("dequant_aef implements the AEF decode", {
  x <- c(-127, -64, 0, 1, 64, 127)
  expect_equal(dequant_aef(x), sign(x) * (x / 127.5)^2)
  expect_equal(dequant_aef(c(-127, 127)), c(-1, 1), tolerance = 0.02)
})
