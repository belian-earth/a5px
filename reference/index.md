# Package index

## Reading rasters

Aggregate GeoTIFF and COG pixels into A5 cells, from one file or
several.

- [`a5_read_raster()`](https://belian-earth.github.io/a5px/reference/a5_read_raster.md)
  : Read a (Cloud-Optimised) GeoTIFF as A5-cell aggregated values
- [`a5_read_raster_arrow()`](https://belian-earth.github.io/a5px/reference/a5_read_raster_arrow.md)
  : Read a (Cloud-Optimised) GeoTIFF as A5 cells, returning an Arrow
  Table
- [`a5_raster_to_parquet()`](https://belian-earth.github.io/a5px/reference/a5_raster_to_parquet.md)
  : Read a raster and write it straight to Parquet (Rust-direct)

## Decoding

Decode quantized values before aggregation.

- [`dequant_aef()`](https://belian-earth.github.io/a5px/reference/dequant_aef.md)
  : Dequantize Alpha Earth Foundations (AEF) embedding codes

## Inspecting sources

Raster metadata and object store configuration, without reading pixels.

- [`a5_raster_info()`](https://belian-earth.github.io/a5px/reference/a5_raster_info.md)
  : Inspect a raster's structure
- [`a5_store_config()`](https://belian-earth.github.io/a5px/reference/a5_store_config.md)
  : Inspect the object store configuration a source resolves to

## Coarser resolutions

Summarise A5-keyed results at a coarser resolution without re-reading.

- [`a5_aggregate()`](https://belian-earth.github.io/a5px/reference/a5_aggregate.md)
  : Aggregate A5-keyed values to a coarser resolution

## Writing

Write A5-keyed results to Parquet.

- [`a5_write_parquet()`](https://belian-earth.github.io/a5px/reference/a5_write_parquet.md)
  : Write A5-keyed values to a Parquet file

## Configuration

Worker and fetch concurrency.

- [`a5px_set_concurrency()`](https://belian-earth.github.io/a5px/reference/a5px_set_concurrency.md)
  [`a5px_get_concurrency()`](https://belian-earth.github.io/a5px/reference/a5px_set_concurrency.md)
  : Tile-level concurrency for raster reads

## Package overview

Common arguments, remote sources and supported formats.

- [`a5px`](https://belian-earth.github.io/a5px/reference/a5px-package.md)
  [`a5px-package`](https://belian-earth.github.io/a5px/reference/a5px-package.md)
  : a5px: Read Raster Data as A5 Cells via Pixel-Driven Aggregation
