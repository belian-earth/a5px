# Benchmark `gdal raster zonal-stats` (GDAL >= 3.10) against the same job
# the other benchmarks measure: per-A5-cell mean of every band of a Sentinel-2
# COG. Times polygon prep and the CLI invocation separately so the comparison
# can be read either with or without the polygon-prep cost.

suppressMessages({
  library(a5px); library(a5R); library(terra); library(dplyr); library(tibble)
})

PATH <- "test-tifs/test_cog.tif"
RES  <- 16L
stopifnot(file.exists(PATH))

time_it <- function(label, expr) {
  t0 <- Sys.time()
  val <- force(expr)
  dt <- as.numeric(Sys.time() - t0, units = "secs")
  cat(sprintf("[%-32s] %7.2f s\n", label, dt))
  invisible(list(label = label, t = dt, val = val))
}

tmpdir <- tempfile("a5gdal_")
dir.create(tmpdir)
zones_gpkg  <- file.path(tmpdir, "zones.gpkg")
output_gpkg <- file.path(tmpdir, "out.gpkg")

cat("\n=== polygon prep (shared cost: any polygon-driven backend pays this) ===\n")
prep <- time_it("a5 grid + boundary + project + writeVector", {
  r <- rast(PATH)
  bbox_ll <- ext(project(r, "EPSG:4326"))
  grid <- a5R::a5_uncompact(
    a5R::a5_polygon_to_cells(
      wk::rct(bbox_ll$xmin, bbox_ll$ymin, bbox_ll$xmax, bbox_ll$ymax),
      resolution = RES
    ),
    resolution = RES
  )
  bnd_wkt <- a5R::a5_cell_to_boundary(grid, format = "wkt")
  polys <- vect(as.character(bnd_wkt), crs = "EPSG:4326") |>
    project(crs(r))
  polys$cell <- a5R::a5_u64_to_hex(grid)
  writeVector(polys, zones_gpkg, overwrite = TRUE)
  list(n = nrow(polys), gpkg = zones_gpkg)
})
cat(sprintf("  zones written: %d polygons -> %s\n", prep$val$n, prep$val$gpkg))

# ---------- gdal raster zonal-stats: all 12 bands, mean only --------------
cat("\n=== gdal raster zonal-stats CLI ===\n")
band_args <- paste(sprintf("-b %d", seq_len(12L)), collapse = " ")

cli_one <- function(strategy, pixels) {
  out_path <- file.path(tmpdir, sprintf("out_%s_%s.gpkg", strategy, pixels))
  cmd <- sprintf(
    "gdal raster zonal-stats --quiet --overwrite %s --zones %s --stat mean --strategy %s --pixels %s %s %s",
    band_args,
    shQuote(zones_gpkg),
    strategy,
    pixels,
    shQuote(PATH),
    shQuote(out_path)
  )
  cat("\n$ ", cmd, "\n", sep = "")
  rec <- time_it(sprintf("gdal zonal-stats (%s/%s)", strategy, pixels), {
    status <- system(cmd, intern = FALSE)
    if (status != 0L) stop("gdal CLI exited ", status)
    invisible(NULL)
  })
  rec$out_path <- out_path
  rec
}

z1 <- cli_one("raster", "default")

cat("\n=== summary ===\n")
total_raster <- prep$t + z1$t
cat(sprintf("polygon prep                          : %7.2f s\n", prep$t))
cat(sprintf("gdal zonal-stats (raster strategy)    : %7.2f s   total: %7.2f s\n",
            z1$t, total_raster))
