# A/B equivalence and timing harness: two a5px builds over every fixture.
#
# Each build runs in its own R process (a session cannot load two versions),
# writing normalised outputs to an RDS; `compare` then checks cell sets are
# identical and values agree to 1e-9 of each column's magnitude (summation
# order moves the last bits).
#
#   R_LIBS=<lib_with_baseline>  Rscript benchmarks/ab-fixtures.R run base
#   R_LIBS=<lib_with_candidate> Rscript benchmarks/ab-fixtures.R run new
#   Rscript benchmarks/ab-fixtures.R compare
#
# Install each build into its own library first, e.g.
#   R CMD INSTALL --no-docs --no-test-load -l /tmp/lib_new .
# (release build; see dev/NOTES.md on why never devtools::load_all()).
#
# Environment:
#   A5PX_AB_DIR   output directory (default benchmarks/ab-out)
#   A5PX_AB_AEF   optional synthetic AEF-like COG (benchmarks/make-aef-synth.py);
#                 read with a block-aligned bbox over its top-left 2048² pixels
#   A5PX_AB_S2    optional large Sentinel-2 COG (default test-tifs/test_cog.tif)
#   A5PX_AB_WORKERS  cpu_workers for every read (default 4)
#
# Coverage: lat/long, UTM, custom LAEA, BNG; NaN / float32 / int8 nodata;
# overviews; planar and chunky; forward and overlay; mean+count,
# min+max+sd+sum, majority, fractions; full extent and a pixel-aligned
# sub-bbox; resolutions 2..22.

args <- commandArgs(trailingOnly = TRUE)
mode <- if (length(args) >= 1) args[[1]] else "compare"
out_dir <- Sys.getenv("A5PX_AB_DIR", "benchmarks/ab-out")
dir.create(out_dir, showWarnings = FALSE, recursive = TRUE)

fixtures <- function() {
  ex <- function(f) system.file("extdata", f, package = "a5px")
  fx <- list(
    aef_int8 = list(f = ex("aef_int8.tif"), res = c(10L, 14L, 17L), cat = TRUE),
    be_planar = list(f = ex("be_planar.tif"), res = c(14L, 18L, 21L), cat = TRUE),
    f32_nodata = list(f = ex("f32_nodata.tif"), res = c(14L, 18L, 21L), cat = FALSE),
    laea_custom = list(f = ex("laea_custom.tif"), res = c(16L, 19L, 22L), cat = FALSE),
    laea_wide = list(f = ex("laea_wide.tif"), res = c(2L, 3L, 5L, 8L), cat = FALSE),
    nan_nodata = list(f = ex("nan_nodata.tif"), res = c(16L, 19L), cat = FALSE),
    overview_cog = list(f = ex("overview_cog.tif"), res = c(10L, 13L, 16L), cat = FALSE)
  )
  s2 <- Sys.getenv("A5PX_AB_S2", "test-tifs/test_cog.tif")
  if (file.exists(s2)) fx$s2 <- list(f = s2, res = c(14L, 17L), cat = FALSE, mean_only = TRUE)
  aef <- Sys.getenv("A5PX_AB_AEF", "")
  if (nzchar(aef) && file.exists(aef)) {
    # top-left 2048 x 2048 pixels of the synthetic file, block aligned
    fx$aef_synth <- list(f = aef, res = c(16L, 18L), cat = FALSE, dequant = TRUE,
                         bbox = c(118.985162341833, 5.31213184598557, 119.170536338303, 5.49664877339024))
  }
  fx
}

# pixel-aligned bbox over the middle of a result's cells
sub_bbox <- function(cells) {
  ll <- as.data.frame(a5R::a5_cell_to_lonlat(cells))
  lon <- ll[[1]]; lat <- ll[[2]]
  c(quantile(lon, 0.23), quantile(lat, 0.31), quantile(lon, 0.77), quantile(lat, 0.69))
}

# hex cell keys, list columns (fractions) flattened, rows sorted by cell
normalise <- function(out) {
  df <- as.data.frame(out)
  df$cell <- a5R::a5_u64_to_hex(out$cell)
  for (nm in names(df)) {
    if (is.list(df[[nm]])) {
      df[[nm]] <- vapply(df[[nm]], function(x) paste(names(x), signif(unlist(x), 12), collapse = ";"), "")
    }
  }
  df[order(df$cell), , drop = FALSE]
}

run <- function(tag) {
  suppressPackageStartupMessages({library(a5px); library(a5R)})
  workers <- as.integer(Sys.getenv("A5PX_AB_WORKERS", "4"))
  results <- list(); timings <- c()
  read <- function(fi, res, st, mode, bbox, align) {
    tryCatch(
      a5_read_raster(fi$f, resolution = res, stat = st, mode = mode, bbox = bbox, bbox_align = align,
                     dequant = if (isTRUE(fi$dequant)) dequant_aef else NULL,
                     cpu_workers = workers, io_concurrency = workers),
      error = function(e) e)
  }
  for (nm in names(fixtures())) {
    fi <- fixtures()[[nm]]
    for (res in fi$res) {
      stat_sets <- list(c("mean", "count"), c("min", "max", "sd", "sum"))
      if (isTRUE(fi$cat)) stat_sets <- c(stat_sets, list("majority"), list("fractions"))
      if (isTRUE(fi$mean_only)) stat_sets <- stat_sets[1]
      for (mode in c("forward", "overlay")) for (st in stat_sets) {
        if (mode == "overlay" && identical(st, "fractions")) next
        key <- sprintf("%s|r%d|%s|%s|full", nm, res, mode, paste(st, collapse = "+"))
        t0 <- Sys.time()
        out <- read(fi, res, st, mode, fi$bbox, if (is.null(fi$bbox)) "pixel" else "block")
        timings[key] <- as.numeric(Sys.time() - t0, units = "secs")
        if (inherits(out, "error")) { results[[key]] <- conditionMessage(out); next }
        results[[key]] <- normalise(out)
        cat(sprintf("%-62s %6d cells %6.2f s\n", key, nrow(out), timings[key]))
        if (nrow(out) < 20 || isTRUE(fi$mean_only) || mode == "overlay") next
        key2 <- sub("full$", "subbbox", key)
        out2 <- read(fi, res, st, mode, sub_bbox(out$cell), "pixel")
        results[[key2]] <- if (inherits(out2, "error")) conditionMessage(out2) else normalise(out2)
      }
    }
  }
  saveRDS(list(results = results, timings = timings, version = as.character(packageVersion("a5px")),
               lib = dirname(system.file(package = "a5px"))),
          file.path(out_dir, sprintf("ab_%s.rds", tag)))
  cat(sprintf("\n%s: %d configurations from %s\n", tag, length(results), dirname(system.file(package = "a5px"))))
}

compare <- function() {
  a <- readRDS(file.path(out_dir, "ab_base.rds")); b <- readRDS(file.path(out_dir, "ab_new.rds"))
  cat(sprintf("base: a5px %s (%s)\nnew : a5px %s (%s)\n\n", a$version, a$lib, b$version, b$lib))
  keys <- union(names(a$results), names(b$results)); bad <- 0
  tm <- function(t) if (is.null(t) || is.na(t)) "" else sprintf("%6.2fs", t)
  for (k in keys) {
    x <- a$results[[k]]; y <- b$results[[k]]
    if (is.null(x) || is.null(y)) { cat(sprintf("%-62s MISSING in one build\n", k)); bad <- bad + 1; next }
    if (is.character(x) || is.character(y)) {
      cat(sprintf("%-62s ERROR base=%s new=%s\n", k, substr(as.character(x)[1], 1, 40), substr(as.character(y)[1], 1, 40)))
      bad <- bad + 1; next
    }
    if (!identical(x$cell, y$cell)) {
      cat(sprintf("%-62s CELLS DIFFER (%d vs %d rows, %d common)\n", k, nrow(x), nrow(y), length(intersect(x$cell, y$cell))))
      bad <- bad + 1; next
    }
    worst <- 0
    for (nm in setdiff(names(x), "cell")) {
      if (is.character(x[[nm]])) {
        if (!identical(x[[nm]], y[[nm]])) { cat(sprintf("%-62s FRACTIONS DIFFER in %s\n", k, nm)); bad <- bad + 1 }
        next
      }
      d <- abs(x[[nm]] - y[[nm]])
      d[is.nan(x[[nm]]) & is.nan(y[[nm]])] <- 0
      if (any(is.na(d))) { cat(sprintf("%-62s NA MISMATCH in %s\n", k, nm)); bad <- bad + 1; next }
      scale <- max(abs(x[[nm]][is.finite(x[[nm]])]), 1e-300)
      worst <- max(worst, max(d / scale))
    }
    flag <- if (worst > 1e-9) { bad <- bad + 1; "  <-- TOO LARGE" } else ""
    cat(sprintf("%-62s rows %7d  max diff %.1e  base %s new %s%s\n", k, nrow(x), worst, tm(a$timings[k]), tm(b$timings[k]), flag))
  }
  cat(sprintf("\n%d configurations, %d problems\n", length(keys), bad))
  invisible(bad)
}

switch(mode,
  run = run(if (length(args) >= 2) args[[2]] else "new"),
  compare = { if (compare() > 0) quit(status = 1) },
  stop("usage: ab-fixtures.R run <base|new> | compare"))
