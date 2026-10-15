# Aggregate A5-keyed values to a coarser resolution

Lifts each row's A5 cell to its supercell at `to_resolution` via
[`a5R::a5_cell_to_supercell()`](https://belian-earth.github.io/a5R/reference/a5_spatial_hierarchy.html)
and reduces the selected value columns within each supercell group.
Avoids re-reading the source raster when you already have a
high-resolution result and want it summarised at a coarser scale.

## Usage

``` r
a5_aggregate(
  x,
  to_resolution,
  ...,
  stat = c("mean", "sum", "count", "min", "max", "var", "sd")
)
```

## Arguments

- x:

  A tibble or data.frame with exactly one column of class
  [a5R::a5_cell](https://belian-earth.github.io/a5R/reference/a5_cell.html).

- to_resolution:

  Integer scalar. The coarser target resolution. Must be strictly less
  than the input cells' resolution.

- ...:

  \<[`tidy-select`](https://tidyselect.r-lib.org/reference/language.html)\>
  columns to aggregate. Defaults to every non-cell numeric or
  list-of-numeric column.

- stat:

  Reducer. One of `"mean"` (default), `"sum"`, `"count"`, `"min"`,
  `"max"`, `"var"`, `"sd"`. `"count"` returns the number of non-`NA`
  source values per group. `"var"` / `"sd"` use the sample formula
  (divisor n - 1) to match R's
  [`stats::var()`](https://rdrr.io/r/stats/cor.html) /
  [`stats::sd()`](https://rdrr.io/r/stats/sd.html); single-row groups
  are returned as `NA`.

## Value

A tibble keyed by supercells at `to_resolution`, with the selected
columns reduced. The cell column retains its original name and the
[a5R::a5_cell](https://belian-earth.github.io/a5R/reference/a5_cell.html)
class.

## Details

Each fine cell is assigned to the coarse cell that contains its centre.
The A5 index hierarchy
([`a5R::a5_cell_to_parent()`](https://belian-earth.github.io/a5R/reference/a5_hierarchy.html))
is not spatially nested: for about half of cells one resolution apart,
the index parent is a neighbour of the containing cell, so grouping by
it misplaces values. The spatial rule is exact for centre placement but
approximate for area, because fine cells straddle coarse-cell
boundaries. If you need conservative aggregation (counts that must sum
exactly), use a higher input resolution so boundary error is small
relative to cell area. Locating centres runs in parallel under
[`a5R::a5_set_threads()`](https://belian-earth.github.io/a5R/reference/a5_threads.html).

Both layouts produced by
[`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md)
are supported:

- wide: one numeric column per band (default).

- list-of-numeric: a single fixed-length list column
  (`as_vector = TRUE`). Reduced element-wise.

## Examples

``` r
if (FALSE) { # \dontrun{
  fine <- a5_read_raster(path, resolution = 14L)
  coarse <- a5_aggregate(fine, to_resolution = 10L)

  # explicit column selection + sum reducer
  a5_aggregate(fine, 10L, B02, B03, stat = "sum")

  # list-column input (as_vector = TRUE) is reduced element-wise
  v <- a5_read_raster(path, resolution = 14L, as_vector = TRUE)
  a5_aggregate(v, 10L)
} # }
```
