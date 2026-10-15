# Inspect a raster's structure

Reads the metadata of a GeoTIFF / COG without touching pixel data:
dimensions, data type, nodata, band names, internal block (tile) grid,
overview levels, CRS and WGS 84 envelope. Use it to plan chunked reads
(see `bbox_align` in
[`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md))
or to check what a remote source looks like before a long read.

## Usage

``` r
a5_raster_info(src, store_opts = NULL)
```

## Arguments

- src:

  Path or URL to a GeoTIFF / COG. Supported schemes: local path (no
  scheme), `file://`, `http(s)://`, `s3://`, `gs://`, `az://`. See the
  "Remote sources" section of
  [a5px-package](https://belian-earth.github.io/a5px/reference/a5px-package.md)
  for how cloud clients are configured.

- store_opts:

  Named character vector or named list of object store options for a
  remote `src`, applied after the environment defaults so an explicit
  option always wins. Keys are the `object_store` config keys for the
  provider, e.g. `aws_region`, `aws_skip_signature`,
  `aws_access_key_id`, `aws_secret_access_key`, `aws_session_token`,
  `aws_endpoint`, `aws_virtual_hosted_style_request`, the `google_*` /
  `azure_*` equivalents, and for plain HTTP(S) the client keys such as
  `timeout` or `allow_http`. Unknown keys, and any key given with a
  local path, are errors. Inspect the result with
  [`a5_store_config()`](https://belian-earth.github.io/a5px/reference/a5_store_config.md).

## Value

A list with elements:

- `width`, `height`, `n_bands`: integer dimensions of the full
  resolution image.

- `dtype`: data type string, e.g. `"uint16"`, `"int8"`, `"float32"`;
  `"mixed"` when bands differ.

- `nodata`: numeric nodata value, `NA` if the file declares none.

- `scale`, `offset`: numeric vectors of length `n_bands` from the GDAL
  band metadata (`SCALE` and `OFFSET` items of the `GDAL_METADATA` tag),
  `NA` for a band that declares none. The decoded value is
  `raw * scale + offset`; see `scoff` in
  [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md).

- `band_names`: character vector from the GDAL `DESCRIPTION` tags, or
  `band_NN` placeholders.

- `band_metadata`: list of length `n_bands` of named character vectors
  holding each band's remaining metadata items, such as `UNITTYPE`.

- `metadata`: named character vector of the dataset-level metadata
  items.

- `interleave`: `"pixel"` or `"band"`; `compression`: codec name.

- `block`: integer `c(width, height)` of the internal tiles; `n_blocks`:
  integer `c(x, y)` tile counts. Zero for strip-based files, which a5px
  cannot read.

- `overviews`: data frame with one row per usable reduced-resolution
  level (`level` = IFD index, `width`, `height`, `block_width`,
  `block_height`), in file order. Zero rows when there are none.

- `crs`: `EPSG:<code>` when the file carries an EPSG code, otherwise the
  proj string a5px resolved.

- `geotransform`: the 6-element GDAL geotransform.

- `bbox`: WGS 84 envelope `c(xmin, ymin, xmax, ymax)` of the footprint.

## Details

Metadata is read from the file's own `GDAL_METADATA` tag (default domain
only). Sidecar `.aux.xml` files are not read.

## Examples

``` r
f <- system.file("extdata", "overview_cog.tif", package = "a5px")
info <- a5_raster_info(f)
info$block
#> [1] 256 256
info$overviews
#>   level width height block_width block_height
#> 1     1   256    256         256          256
#> 2     2   128    128         256          256
```
