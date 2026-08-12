# Categorical stats (majority / fractions) on the aef_int8.tif fixture
# (see test-dequant.R for its full description). The int8 codes double as
# class labels here: values are deterministic in position, so per-cell
# class counts are reconstructed analytically.
cat_tif <- function() {
  system.file("extdata", "aef_int8.tif", package = "a5px")
}

hexset <- function(cells) a5R::a5_u64_to_hex(cells)

# analytic per-pixel codes and cell keys for band A01 (valid pixels only)
analytic_a01 <- function(resolution = 12L) {
  w <- 128L
  px <- 0.1 / w
  cr <- expand.grid(c = 0:(w - 1L), r = 0:(w - 1L))
  ok <- !(cr$c < 10 & cr$r < 10)
  lon <- 12 + (cr$c + 0.5) * px
  lat <- 48.1 - (cr$r + 0.5) * px
  list(
    key = hexset(a5R::a5_lonlat_to_cell(lon, lat, resolution = resolution))[ok],
    val = ((3 * cr$c + 7 * cr$r) %% 255 - 127)[ok]
  )
}

test_that("forward majority matches the analytic argmax with smallest-class ties", {
  f <- cat_tif()
  skip_if(f == "")
  got <- a5_read_raster(f, resolution = 12L, stat = "majority")
  a <- analytic_a01()
  ref <- vapply(split(a$val, a$key), function(v) {
    classes <- sort(unique(v))
    counts <- vapply(classes, function(cl) sum(v == cl), 0L)
    classes[which.max(counts)] # first max = smallest class on ties
  }, 0)
  expect_setequal(hexset(got$cell), names(ref))
  expect_equal(got$A01[match(names(ref), hexset(got$cell))], unname(ref))
})

test_that("forward fractions match analytic count shares and sum to 1", {
  f <- cat_tif()
  skip_if(f == "")
  got <- a5_read_raster(f, resolution = 12L, stat = "fractions")
  expect_type(got$A01, "list")
  expect_equal(unname(vapply(got$A01, sum, 0)), rep(1, nrow(got)),
               tolerance = 1e-12)

  a <- analytic_a01()
  by_cell <- split(a$val, a$key)
  for (i in sample.int(nrow(got), 10L)) {
    v <- by_cell[[hexset(got$cell)[i]]]
    shares <- got$A01[[i]]
    expect_equal(sort(as.integer(names(shares))), sort(unique(v)))
    for (cl in names(shares)) {
      expect_equal(shares[[cl]], sum(v == as.integer(cl)) / length(v),
                   tolerance = 1e-12)
    }
  }
})

test_that("overlay fractions match exactextract area fractions", {
  skip_if_not_installed("exactextractr")
  skip_if_not_installed("sf")
  skip_if_not_installed("terra")
  f <- cat_tif()
  skip_if(f == "")
  got <- a5_read_raster(f, resolution = 12L, stat = "fractions",
                        mode = "overlay", subsamples = 16L)
  polys <- sf::st_as_sf(
    data.frame(h = hexset(got$cell)),
    geom = sf::st_as_sfc(a5R::a5_cell_to_boundary(got$cell, segments = 16L))
  )
  ee <- exactextractr::exact_extract(terra::rast(f)[[1]], polys, "frac",
                                     progress = FALSE)
  # exactextract emits one frac_<class> column per class over the whole set
  err <- 0
  for (i in seq_len(nrow(got))) {
    shares <- got$A01[[i]]
    for (cl in names(shares)) {
      col <- paste0("frac_", cl)
      if (col %in% names(ee)) {
        err <- max(err, abs(shares[[cl]] - ee[[col]][i]))
      }
    }
  }
  expect_lt(err, 0.05)
})

test_that("majority combines with continuous stats in one pass", {
  f <- cat_tif()
  skip_if(f == "")
  both <- a5_read_raster(f, resolution = 12L, stat = c("mean", "majority"))
  mj <- a5_read_raster(f, resolution = 12L, stat = "majority")
  mn <- a5_read_raster(f, resolution = 12L, stat = "mean",
                       use_overviews = FALSE)
  expect_setequal(names(both),
                  c("cell", "A01_mean", "A02_mean", "A01_majority", "A02_majority"))
  key <- hexset(both$cell)
  expect_equal(both$A01_majority, mj$A01[match(key, hexset(mj$cell))])
  expect_equal(both$A01_mean, mn$A01[match(key, hexset(mn$cell))],
               tolerance = 1e-12)
})

test_that("categorical stats validate their constraints", {
  f <- cat_tif()
  skip_if(f == "")
  expect_error(a5_read_raster(f, resolution = 12L,
                              stat = c("fractions", "mean")),
               "only requested stat")
  expect_error(a5_read_raster(f, resolution = 12L, stat = "fractions",
                              as_vector = TRUE),
               "as_vector")
  expect_error(a5_read_raster(f, resolution = 12L, stat = "majority",
                              dequant = dequant_aef),
               "raw integer codes")
  ovf <- system.file("extdata", "overview_cog.tif", package = "a5px")
  skip_if(ovf == "")
  expect_error(a5_read_raster(ovf, resolution = 12L, stat = "majority"),
               "16 bits or fewer")
})

test_that("arrow path supports majority but rejects fractions", {
  skip_if_not_installed("arrow")
  f <- cat_tif()
  skip_if(f == "")
  expect_error(a5_read_raster_arrow(f, resolution = 12L, stat = "fractions"),
               "a5_read_raster")
  tbl <- a5_read_raster_arrow(f, resolution = 12L, stat = "majority")
  ref <- a5_read_raster(f, resolution = 12L, stat = "majority")
  df <- as.data.frame(tbl)
  df$cell <- a5R::a5_cell_from_arrow(tbl$cell)
  key <- hexset(df$cell)
  expect_equal(df$A01, ref$A01[match(key, hexset(ref$cell))])
})
