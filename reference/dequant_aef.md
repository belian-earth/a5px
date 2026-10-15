# Dequantize Alpha Earth Foundations (AEF) embedding codes

The AEF int8 decode: `sign(x) * (x / 127.5)^2`. Per value, nonlinear and
sign-preserving, mapping the code range `-127..127` to roughly
`[-1, 1]`. Pass it as the `dequant` argument of
[`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md)
(and the Arrow / Parquet variants) so codes are decoded per pixel
*before* aggregation; a nonlinear decode does not commute with the mean,
so decoding aggregated codes gives biased values.

## Usage

``` r
dequant_aef(x)
```

## Arguments

- x:

  Raw integer codes (numeric vector).

## Value

The decoded values, same length as `x`.

## Examples

``` r
dequant_aef(c(-127, 0, 64, 127))
#> [1] -0.9921722  0.0000000  0.2519646  0.9921722
if (FALSE) { # \dontrun{
  emb <- a5_read_raster(src, resolution = 14L, dequant = dequant_aef)
} # }
```
