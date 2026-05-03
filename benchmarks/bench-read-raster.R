# quick-and-dirty perf probe — install the package first:
#   R CMD INSTALL --no-test-load --no-docs .
library(a5px)
library(a5R)

path <- "test-tifs/test_cog.tif"
stopifnot(file.exists(path))

bench_one <- function(threads, io_concurrency, resolution = 16, reps = 3) {
  reps_t <- replicate(
    reps,
    {
      t0 <- Sys.time()
      out <- a5_read_raster(
        path,
        resolution = resolution,
        stat = "mean",
        threads = threads,
        io_concurrency = io_concurrency
      )
      list(t = as.numeric(Sys.time() - t0, units = "secs"), n = nrow(out))
    },
    simplify = FALSE
  )
  data.frame(
    threads = threads,
    io_concurrency = io_concurrency,
    resolution = resolution,
    median_s = median(vapply(reps_t, `[[`, numeric(1), "t")),
    cells = reps_t[[1]]$n
  )
}

grid <- expand.grid(threads = c(1L, 2L, 4L, 8L, 16L), resolution = 16L)
res <- do.call(
  rbind,
  Map(bench_one, grid$threads, pmax(grid$threads, 4L), grid$resolution)
)
print(res)


out <- a5_read_raster(
  path,
  resolution = 16,
  stat = "mean",
  threads = 16,
  io_concurrency = 16
)


a5view::a5_view(out, fill = B05, border = NULL)
out
