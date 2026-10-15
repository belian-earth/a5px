# Session cache of remote byte ranges

Remote reads keep the byte ranges they fetch (header chunks and
compressed blocks) in one cache shared by every read in the R session,
so a read split into `bbox` chunks, or repeated, fetches each block once
rather than once per call. Ranges are kept per source URL and
`store_opts`, and the least recently used are dropped past the budget.
Local files bypass the cache.

## Usage

``` r
a5px_set_cache(max_mb)

a5px_cache_info()

a5px_clear_cache()
```

## Arguments

- max_mb:

  Numeric scalar (\>= 0): the budget in MiB. `0` disables the cache and
  frees what it holds.

## Value

`a5px_set_cache()` returns the previous budget in MiB, invisibly.
`a5px_cache_info()` returns a list: `entries`, `size_mb`, `max_mb`, and
the `hits` and `misses` since the cache was last cleared.
`a5px_clear_cache()` returns `NULL`, invisibly.

## Details

The cache assumes remote files do not change during the session. Clear
it after a source is rewritten in place, or after pointing a URL at
another endpoint through environment variables.

The default budget is 512 MiB, held to an eighth of the memory limit of
the process's cgroup (a container, a Batch task or a systemd scope), so
a task limited to 2 GiB caches at most 256 MiB. The environment variable
`A5PX_CACHE_MB`, read when the first remote read starts, overrides it.

## Examples

``` r
a5px_set_cache(1024)
a5px_cache_info()
#> $entries
#> [1] 0
#> 
#> $size_mb
#> [1] 0
#> 
#> $max_mb
#> [1] 1024
#> 
#> $hits
#> [1] 0
#> 
#> $misses
#> [1] 0
#> 
a5px_clear_cache()
```
