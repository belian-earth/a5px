# Tile-level concurrency for raster reads

a5px's reader runs as a producer / consumer pipeline:

## Usage

``` r
a5px_concurrency(cpu_workers = NULL, io_concurrency = NULL)
```

## Arguments

- cpu_workers:

  Integer scalar (\>= 1), `NULL` to leave it unchanged, or the result of
  an earlier call, to restore both settings.

- io_concurrency:

  Integer scalar (\>= 1), or `NULL` to leave it unchanged.

## Value

With no settings, a list of the current `cpu_workers` and
`io_concurrency`. Otherwise the previous settings, invisibly, as an
`a5px_concurrency` object.

## Details

- **`cpu_workers`** is the number of CPU threads a read uses: the
  blocking-thread consumers that decode each block, and the pool of the
  same size that indexes each decoded block as row stripes, so a read
  that spans fewer blocks than workers still uses every worker. Pick
  this to match the number of physical cores you can spare.

- **`io_concurrency`** is the number of remote requests a read keeps in
  flight, across all its sources. Blocks next to each other in the file
  (a band's blocks along a row, say) are fetched in one request. For
  local files it is the number of blocks read at once.

Defaults are resolved per call: function args \> options \> env vars \>
built-in default. For `cpu_workers` that is the number of CPUs available
to the R process, which respects affinity masks and container or cgroup
CPU quotas (a job limited to 4 CPUs on a 20-core host gets 4); for
`io_concurrency` it is 32. Set them for the session with
`a5px_concurrency()`, `options(a5px.cpu_workers = ...)` and
`options(a5px.io_concurrency = ...)`, or the environment variables
`A5PX_CPU_WORKERS` and `A5PX_IO_CONCURRENCY`.

`a5px_concurrency()` works like
[`options()`](https://rdrr.io/r/base/options.html) and
[`a5R::a5_threads()`](https://belian-earth.github.io/a5R/reference/a5_threads.html):
called with no settings it returns them, and called with settings it
changes them and returns the previous ones, which restore them when
passed back or when used with
[`with()`](https://rdrr.io/r/base/with.html).

## Examples

``` r
a5px_concurrency()
#> $cpu_workers
#> [1] 4
#> 
#> $io_concurrency
#> [1] 32
#> 
old <- a5px_concurrency(cpu_workers = 2, io_concurrency = 64)
a5px_concurrency(old) # restore

# for one expression
with(a5px_concurrency(cpu_workers = 1), a5px_concurrency()$cpu_workers)
#> [1] 1
```
