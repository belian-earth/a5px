make_wide <- function(seed = 1L) {
  set.seed(seed)
  cells <- a5R::a5_grid(c(10, 50, 11, 51), resolution = 8L)
  n <- length(cells)
  tibble::tibble(
    cell = cells,
    B02  = stats::rnorm(n, 100, 10),
    B03  = stats::rnorm(n, 200, 20),
    B04  = stats::rnorm(n, 300, 30)
  )
}

make_listcol <- function(seed = 1L) {
  set.seed(seed)
  cells <- a5R::a5_grid(c(10, 50, 11, 51), resolution = 8L)
  n <- length(cells)
  tibble::tibble(
    cell = cells,
    value = lapply(seq_len(n), function(i) stats::rnorm(3, c(100, 200, 300), 10))
  )
}

test_that("wide aggregate (mean) reduces to coarser cells with band cols intact", {
  fine <- make_wide()
  out  <- a5_aggregate(fine, to_resolution = 5L)

  expect_s3_class(out, "tbl_df")
  expect_s3_class(out$cell, "a5_cell")
  expect_setequal(names(out), c("cell", "B02", "B03", "B04"))
  expect_lt(nrow(out), nrow(fine))
  expect_equal(unique(a5R::a5_get_resolution(out$cell)), 5L)

  # Manual reference: group fine cells by their parent at res 5 and mean.
  parents <- a5R::a5_cell_to_parent(fine$cell, resolution = 5L)
  ref_means <- tapply(fine$B02, a5R::a5_u64_to_hex(parents), mean)
  out_keys  <- a5R::a5_u64_to_hex(out$cell)
  expect_equal(out$B02[match(names(ref_means), out_keys)],
               as.numeric(ref_means), tolerance = 1e-9)
})

test_that("wide aggregate respects stat = sum / count", {
  fine <- make_wide()
  s <- a5_aggregate(fine, to_resolution = 5L, stat = "sum")
  c_ <- a5_aggregate(fine, to_resolution = 5L, stat = "count")

  expect_setequal(names(s),  c("cell", "B02", "B03", "B04"))
  expect_setequal(names(c_), c("cell", "B02", "B03", "B04"))

  # sum over the full dataset must equal sum of group sums
  expect_equal(sum(s$B02), sum(fine$B02))
  # count must total the input row count
  expect_equal(sum(c_$B02), nrow(fine))
})

test_that("wide aggregate respects stat = min / max", {
  fine <- make_wide()
  mn <- a5_aggregate(fine, to_resolution = 5L, stat = "min")
  mx <- a5_aggregate(fine, to_resolution = 5L, stat = "max")
  expect_true(min(mn$B02) >= min(fine$B02) - 1e-12)
  expect_true(max(mx$B02) <= max(fine$B02) + 1e-12)
  # element-wise: every parent's min <= max
  expect_true(all(mn$B02 <= mx$B02))
})

test_that("wide aggregate var / sd match per-group stats::var / stats::sd", {
  fine <- make_wide()
  vv <- a5_aggregate(fine, to_resolution = 5L, stat = "var")
  ss <- a5_aggregate(fine, to_resolution = 5L, stat = "sd")

  parents <- a5R::a5_cell_to_parent(fine$cell, resolution = 5L)
  ref_var <- tapply(fine$B02, a5R::a5_u64_to_hex(parents), function(x) {
    if (length(x) >= 2L) stats::var(x) else NA_real_
  })
  out_keys <- a5R::a5_u64_to_hex(vv$cell)
  expect_equal(vv$B02[match(names(ref_var), out_keys)],
               as.numeric(ref_var), tolerance = 1e-9)
  expect_equal(ss$B02, sqrt(vv$B02), tolerance = 1e-12)
})

test_that("var / sd are NA for single-observation groups", {
  cells <- a5R::a5_grid(c(10, 50, 11, 51), resolution = 8L)
  testthat::skip_if(length(cells) == 0L)
  one <- vctrs::vec_slice(cells, 1L)
  fine <- tibble::tibble(cell = one, B02 = 42)
  vv <- a5_aggregate(fine, to_resolution = 5L, stat = "var")
  ss <- a5_aggregate(fine, to_resolution = 5L, stat = "sd")
  expect_equal(nrow(vv), 1L)
  expect_true(is.na(vv$B02))
  expect_true(is.na(ss$B02))
})

test_that("list-column var reduces element-wise per band", {
  fine <- make_listcol()
  out <- a5_aggregate(fine, to_resolution = 5L, stat = "var")
  expect_setequal(names(out), c("cell", "value"))
  expect_true(all(lengths(out$value) == 3L))

  parents <- a5R::a5_cell_to_parent(fine$cell, resolution = 5L)
  mat <- do.call(rbind, fine$value)
  ref <- aggregate.data.frame(mat, by = list(a5R::a5_u64_to_hex(parents)),
                              FUN = function(x) {
                                if (length(x) >= 2L) stats::var(x) else NA_real_
                              })
  out_keys <- a5R::a5_u64_to_hex(out$cell)
  ord <- match(ref$Group.1, out_keys)
  out_mat <- do.call(rbind, out$value[ord])
  expect_equal(unname(out_mat), unname(as.matrix(ref[, -1])), tolerance = 1e-9)
})

test_that("tidy-select picks a subset of columns", {
  fine <- make_wide()
  out <- a5_aggregate(fine, 5L, B02, B04, stat = "sum")
  expect_setequal(names(out), c("cell", "B02", "B04"))
})

test_that("list-column aggregate reduces element-wise", {
  fine <- make_listcol()
  out <- a5_aggregate(fine, to_resolution = 5L)

  expect_setequal(names(out), c("cell", "value"))
  expect_true(is.list(out$value))
  expect_true(all(lengths(out$value) == 3L))
  expect_equal(unique(a5R::a5_get_resolution(out$cell)), 5L)

  # Reference: stack per-row vectors and take mean per parent group, per band.
  parents <- a5R::a5_cell_to_parent(fine$cell, resolution = 5L)
  mat <- do.call(rbind, fine$value)
  ref <- aggregate.data.frame(mat, by = list(a5R::a5_u64_to_hex(parents)), FUN = mean)
  out_keys <- a5R::a5_u64_to_hex(out$cell)
  ord <- match(ref$Group.1, out_keys)
  out_mat <- do.call(rbind, out$value[ord])
  expect_equal(unname(out_mat), unname(as.matrix(ref[, -1])), tolerance = 1e-9)
})

test_that("list-column count is per-band non-NA count", {
  fine <- make_listcol()
  out <- a5_aggregate(fine, to_resolution = 5L, stat = "count")
  expect_setequal(names(out), c("cell", "value"))
  expect_equal(sum(unlist(out$value)), nrow(fine) * 3L)
})

test_that("errors: no a5_cell column", {
  bad <- tibble::tibble(x = 1:5, y = 6:10)
  expect_error(a5_aggregate(bad, 4L), "no column of class")
})

test_that("errors: multiple a5_cell columns", {
  fine <- make_wide()
  fine2 <- tibble::tibble(cell = fine$cell, other_cell = fine$cell, B02 = fine$B02)
  expect_error(a5_aggregate(fine2, 5L), "expected exactly one")
})

test_that("errors: to_resolution >= current resolution", {
  fine <- make_wide()  # res 8
  expect_error(a5_aggregate(fine, 8L), "strictly less")
  expect_error(a5_aggregate(fine, 9L), "strictly less")
})

test_that("errors: zero rows", {
  fine <- make_wide()[0, ]
  expect_error(a5_aggregate(fine, 5L), "zero rows")
})

test_that("errors: tidy-select with non-aggregable column", {
  fine <- make_wide()
  fine$tag <- letters[seq_len(nrow(fine))]
  expect_error(a5_aggregate(fine, 5L, tag), "not numeric")
})

test_that("default selection skips non-aggregable cols", {
  fine <- make_wide()
  fine$tag <- letters[seq_len(nrow(fine))]
  out <- a5_aggregate(fine, 5L)
  expect_setequal(names(out), c("cell", "B02", "B03", "B04"))
})

test_that("on a real reader output the result resolves cleanly", {
  testthat::skip_if_not_installed("arrow")
  d <- normalizePath(file.path(testthat::test_path(), "..", "..", "test-tifs"),
                     mustWork = FALSE)
  if (!dir.exists(d)) testthat::skip("test-tifs/ not present")
  path <- file.path(d, "exe_cog.tif")
  skip_if_not(file.exists(path), "exe_cog.tif missing")

  fine <- a5_read_raster(path, resolution = 14L, bands = 1:3)
  coarse <- a5_aggregate(fine, to_resolution = 11L)
  expect_lt(nrow(coarse), nrow(fine))
  expect_equal(unique(a5R::a5_get_resolution(coarse$cell)), 11L)
  expect_setequal(names(coarse), c("cell", "B02", "B03", "B04"))
})
