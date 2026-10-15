# Tile-level concurrency for raster reads

a5px's reader runs as a producer / consumer pipeline:

## Usage

``` r
a5px_set_concurrency(cpu_workers = NULL, io_concurrency = NULL)

a5px_get_concurrency()
```

## Arguments

- cpu_workers:

  Integer scalar (\>= 1) or `NULL`.

- io_concurrency:

  Integer scalar (\>= 1) or `NULL`.

## Value

`a5px_set_concurrency()` returns the previous setting invisibly.
`a5px_get_concurrency()` returns a list with the current resolved
defaults.

## Details

- **`cpu_workers`** is the number of CPU threads a read uses: the
  blocking-thread consumers that decode each block, and the pool of the
  same size that indexes each decoded block as row stripes, so a read
  that spans fewer blocks than workers still uses every worker. Pick
  this to match the number of physical cores you can spare.

- **`io_concurrency`** is the number of in-flight tile fetches the
  producer issues at once. For local files the default is enough; for
  cloud reads of multi-band embedding rasters, push it higher (`32`,
  `64`+) so the network stays busy while CPU workers consume from a
  bounded channel.

These two knobs replace the old `threads` arg, which conflated tokio
worker count and tile-level parallelism in confusing ways.

Defaults are resolved per call: function args \> options \> env vars \>
built-in default. For `cpu_workers` that is the number of CPUs available
to the R process, which respects affinity masks and container or cgroup
CPU quotas (a job limited to 4 CPUs on a 20-core host gets 4); for
`io_concurrency` it is `min(16, max(cpu_workers, 8))`. Set globally via
`a5px_set_concurrency()`, `options(a5px.cpu_workers = ...)`,
`options(a5px.io_concurrency = ...)`, or environment variables
`A5PX_CPU_WORKERS` / `A5PX_IO_CONCURRENCY`.

## Examples

``` r
a5px_set_concurrency(cpu_workers = 8, io_concurrency = 32)
a5px_get_concurrency()
#> $cpu_workers
#> [1] 8
#> 
#> $io_concurrency
#> [1] 32
#> 
```
