# a5px — current state

Last updated 2026-05-04. Forward (pixel-driven) raster → A5 cell engine,
streamed via async-tiff + object_store, with three output paths
(tibble / Arrow Table / direct-to-Parquet) and a verified cloud read of
Source Cooperative AEF embeddings.

## Public API

### Reading

- `a5_read_raster(src, resolution, stat, bands, threads, io_concurrency, as_vector)`
  → `tibble` keyed by `cell` (`a5R::a5_cell`) with one numeric column per band.
- `a5_read_raster_arrow(src, resolution, stat, bands, threads, io_concurrency, value_type)`
  → `arrow::Table` with `cell:uint64` and `value:FixedSizeList<float, n_bands>`.
  `value_type` is `"float64"` or `"float32"`. Schema metadata captures band names,
  resolution, stat (preserved through Parquet round-trip).
- `a5_raster_to_parquet(src, dest, resolution, stat, bands, value_type, compression, ...)`
  Rust-direct: read + RecordBatch build + Parquet write all in Rust. Skips the
  per-cell list-of-vectors materialisation that the R-Arrow path pays.

### Writing

- `a5_write_parquet(x, dest, compression = "zstd", ...)` — accepts the Arrow
  Table from `a5_read_raster_arrow()` or any `data.frame` from
  `a5_read_raster()` (per-band columns or `as_vector` list column).

### Args common to all readers

- `src`: local path **or** URL (`http(s)://`, `s3://`, `gs://`, `az://`, `file://`).
  S3/GCS/Azure go through `object_store::parse_url` with the matching feature
  enabled.
- `bands`: `NULL` (all), integer vector (1-based indices), or character vector
  (matched against the GDAL `DESCRIPTION` tag, falling back to `band_NN`).
- `stat`: `mean` / `sum` / `count` / `min` / `max`.
- `threads`, `io_concurrency`: tile-level concurrency knobs. Defaults `1` and `8`.

### Threading helpers

- `a5px_set_threads(n)` / `a5px_get_threads()`. Also picks up `A5PX_NUM_THREADS`
  env or `options(a5px.threads = ...)` via `.onLoad`.

### Profiler

- Set env `A5PX_PROFILE=1` to dump stage and sub-stage timings to stderr
  (io fetch / decode / build points / proj transform / a5 lonlat→cell /
  hashmap / push / merge). Sums across tile workers; divide by effective
  concurrency for wall.

## Stack

- `async-tiff` 0.3 with `tokio` + `object_store` + `reqwest` features.
- `object_store` 0.13 (aws / gcp / azure / http enabled).
- `proj4rs` 0.1 with `crs-definitions` for EPSG → proj string.
- `a5` 0.7.3 for cell math.
- `arrow-array` / `arrow-buffer` / `arrow-schema` 58 + `parquet` 58 for the
  Rust-direct Parquet writer (zstd + snappy codecs enabled).
- `extendr-api` 0.9 + rextendr 0.5.
- Rust 1.85 (parquet 58 requirement).

## Performance

### Local Sentinel-2 COG (test_cog.tif: 4959 × 4190 × 12 bands, A5 res 16)

| Stage | Wall |
|---|---:|
| Single-threaded baseline (post-cache) | 8.8 s |
| 8 threads | 5.0 s |
| 16 threads | 5.0 s (plateau) |

Started at ~9.5 s with 8 threads; the A5-cell cache (a `a5cell_contains_point`
fast-path before the search-based `lonlat_to_cell`) dropped it to ~5 s. Over 16
cores the bottleneck is now CPU-bound a5 indexing (~190 ns/px on cache hits,
26 M pixels), not I/O or HashMap merge.

### Comparison vs other pipelines (same job)

| Pipeline | Wall |
|---|---:|
| **a5px** | **5.0 s** |
| GDAL `gdal raster zonal-stats` (CLI only, raster strategy) | 38.7 s |
| GDAL `gdal raster zonal-stats` total (incl. polygon prep) | 70.7 s |
| terra forward (R reproject + a5R + dplyr) | 102.7 s |
| terra zonal (`a5_grid` → polys → `terra::extract`) | killed |

### Cloud (AEF on Source Cooperative, 8192² × 64 Int8 ZSTD planar COG)

| Path | bands | Wall |
|---|---:|---:|
| 64 (full) | 64 | 61.8 s |
| 1 (band-aware fetch) | 1 | 22.2 s |
| 8 → arrow → parquet (R Arrow path) | 8 | 31.3 s |
| 8 → parquet (Rust-direct) | 8 | **24.2 s** |

`a5_raster_to_parquet` produces bit-identical Parquet to the R-Arrow path
(verified by test). The 7 s saving is the eliminated R-side
list-of-vectors construction; it scales with `n_cells × n_bands`.

## Correctness / behaviour

- Pixels with all-bands == nodata are dropped before the cell entry is
  registered.
- **Nodata is dataset-wide** by GeoTIFF spec (`TIFFTAG_GDAL_NODATA`). a5px
  parses that tag with case-insensitive `nan` / `inf` support and uses a
  NaN-safe comparison so a NaN sentinel actually matches NaN pixels (the
  IEEE-754 `==` gotcha). Per-band nodata in GeoTIFF is not possible; VRT
  is the future per-band path (see Open work).
- `Scale` / `Offset` are not applied. Returned values are in the raster's
  native domain.
- CRS detection uses the EPSG code in the GeoKeyDirectory
  (`projected_type` or `geographic_type`). WKT-only CRS without an EPSG
  code errors with "missing geokey: EPSG code". (See Open work.)
- Cross-checked against `terra::extract` to within ~0.2–0.5 % (residual is
  cell-mean-of-means vs pixel-mean weighting at cell edges).

## Tests

`devtools::test()` → 58/58.

- `tests/testthat/test-read-raster.R` covers core read, stat semantics
  (`min ≤ mean ≤ max`, `sum = mean × count`), `as_vector`, band selection
  (by index and by name), invalid resolution, missing file, non-georef
  TIFF rejection, NaN nodata filtering.
- `tests/testthat/test-arrow.R` covers `a5_read_raster_arrow()` schema +
  metadata + value-equality vs the tibble path, `a5_write_parquet`
  round-trip for both inputs (Arrow Table and tibble), and bit-equality
  between the R-Arrow and Rust-direct Parquet outputs.

Test fixtures:

- `test-tifs/` (gitignored, large) — Sentinel-2 + Exeter COGs for local
  benchmarks and core tests.
- `inst/extdata/nan_nodata.tif` (8 KiB, committed) — NaN-nodata regression
  fixture, ships with the package.

## File map

```
DESCRIPTION                          # Imports a5R, arrow (Suggests), ...
NAMESPACE
NOTES.md                             # this file
R/
  arrow.R                            # a5_read_raster_arrow, a5_write_parquet, a5_raster_to_parquet
  read.R                             # a5_read_raster
  threads.R                          # a5px_set_threads / a5px_get_threads
  utils.R                            # check_resolution, new_a5_cell_from_rs, parse_bands_arg
  zzz.R                              # .onLoad — A5PX_NUM_THREADS / options(a5px.threads=)
  extendr-wrappers.R                 # auto-generated by rextendr
inst/extdata/
  nan_nodata.tif                     # 32x32 Float32 fixture for NaN-nodata test
src/rust/Cargo.toml                  # async-tiff, object_store, proj4rs, a5, arrow*, parquet, ...
src/rust/src/
  lib.rs
  threading.rs                       # rayon pool
  cell_raw.rs                        # u64 → b1..b8 list
  error.rs
  geo.rs                             # geotransform, nodata, band-name parsing
  band_fetch.rs                      # planar tile band-subset fetch path
  read.rs                            # async pipeline + process_tile + extendr bindings
  parquet_write.rs                   # Rust-direct RecordBatch + Parquet writer
src/rust/examples/probe.rs           # diagnostic dump of GDAL metadata for a remote COG
benchmarks/
  bench-read-raster.R                # thread sweep
  vs-terra.R                         # a5px vs terra
  vs-gdal-zonal.R                    # a5px vs gdal raster zonal-stats CLI
tests/testthat/
  test-read-raster.R
  test-arrow.R
```

## I/O + CPU split

The reader runs as a producer / consumer pipeline:

- **Producer**: async tasks issue tile fetches concurrently (capped by
  `io_concurrency`). Each pushes a `TileItem` (Tile or per-band byte
  ranges) into a bounded `async_channel` of depth `2 * cpu_workers`.
- **Consumers**: `cpu_workers` tokio blocking-pool tasks loop on
  `recv_blocking()`, decode + run `process_tile`, accumulate into
  per-worker `AHashMap`s. No global mutex.
- **Reduce**: when the channel closes (producer drops its sender),
  consumers exit and their per-worker maps are tree-reduced.

Bench on AEF cloud tile (Source Cooperative, 2.8 GiB ZSTD planar COG):

| Path | bands | io_concurrency | wall (s) |
|---|---:|---:|---:|
| Old (single async future per tile) | 1 | 16 | 22.2 |
| **New split** | 1 | 16 | **12.1** |
| New split | 1 | 64 | 17.0 (CF rate-limit) |
| New split | 1:8 | 16 | **23.9** |

Default `io_concurrency = max(cpu_workers, 16)`. Going higher hurts on
CloudFlare-fronted sources due to per-client request queuing; bump it
via `a5px_set_concurrency()` for fast on-prem object stores.

## Open work (in priority order)

1. **VRT support** — opens up per-band nodata, on-the-fly CRS warp, multi-tile
   mosaics. Async-tiff doesn't read VRT directly so this needs a thin
   reader (probably `gdalraster` for setup + a custom URL list resolver).
2. **`--pixels=all-touched` / `--pixels=fractional` aggregation modes** —
   both require per-pixel fan-out from the forward path: for each pixel
   compute the cell at its centre AND every cell whose centroid falls
   inside (all-touched) or weight by pixel-cell area overlap
   (fractional). Useful when cells and pixels are similar size and
   boundary precision matters; more expensive than the current
   centroid-of-pixel inclusion criterion.
3. **Strip-based GeoTIFFs** — currently errors with a clear "MVP requires
   tiled TIFF" message. Most modern COGs are tiled, but plain TIFFs
   sometimes aren't.
4. **Multi-thread plateau past 8 cores** — at 16 threads the local read
   gains nothing over 8. Profiling pointed at SMT contention + load
   imbalance from partial edge tiles; further wins would need intra-tile
   parallelism (rayon-chunk each tile) or a bigger raster.
5. **Tighter perf around a5 indexing** — `a5::lonlat_to_cell_with_hint`
   upstream PR would replace our `a5cell_contains_point` fast-path with
   a proper API and clean up the unsafe pointer caching of the previous
   cell entry.

## Permissions / settings

`.claude/settings.local.json` is gitignored and contains the broad project
allowlist (`Bash(cargo *)`, `Bash(Rscript *)`, etc). Tighten if desired.
