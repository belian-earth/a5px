#' Inspect a raster's structure
#'
#' Reads the metadata of a GeoTIFF / COG without touching pixel data:
#' dimensions, data type, nodata, band names, internal block (tile) grid,
#' overview levels, CRS and WGS 84 envelope. Use it to plan chunked reads
#' (see `bbox_align` in [a5_read_raster()]) or to check what a remote source
#' looks like before a long read.
#'
#' Metadata is read from the file's own `GDAL_METADATA` tag (default domain
#' only). Sidecar `.aux.xml` files are not read.
#'
#' @inheritParams a5_read_raster
#' @returns A list with elements:
#'   - `width`, `height`, `n_bands`: integer dimensions of the full
#'     resolution image.
#'   - `dtype`: data type string, e.g. `"uint16"`, `"int8"`, `"float32"`;
#'     `"mixed"` when bands differ.
#'   - `nodata`: numeric nodata value, `NA` if the file declares none.
#'   - `scale`, `offset`: numeric vectors of length `n_bands` from the GDAL
#'     band metadata (`SCALE` and `OFFSET` items of the `GDAL_METADATA` tag),
#'     `NA` for a band that declares none. The decoded value is
#'     `raw * scale + offset`; see `scoff` in [a5_read_raster()].
#'   - `band_names`: character vector from the GDAL `DESCRIPTION` tags, or
#'     `band_NN` placeholders.
#'   - `band_metadata`: list of length `n_bands` of named character vectors
#'     holding each band's remaining metadata items, such as `UNITTYPE`.
#'   - `metadata`: named character vector of the dataset-level metadata
#'     items.
#'   - `interleave`: `"pixel"` or `"band"`; `compression`: codec name.
#'   - `block`: integer `c(width, height)` of the internal tiles;
#'     `n_blocks`: integer `c(x, y)` tile counts. Zero for strip-based
#'     files, which a5px cannot read.
#'   - `overviews`: data frame with one row per usable reduced-resolution
#'     level (`level` = IFD index, `width`, `height`, `block_width`,
#'     `block_height`), in file order. Zero rows when there are none.
#'   - `crs`: `EPSG:<code>` when the file carries an EPSG code, otherwise
#'     the proj string a5px resolved.
#'   - `geotransform`: the 6-element GDAL geotransform.
#'   - `bbox`: WGS 84 envelope `c(xmin, ymin, xmax, ymax)` of the footprint.
#' @examples
#' f <- system.file("extdata", "overview_cog.tif", package = "a5px")
#' info <- a5_raster_info(f)
#' info$block
#' info$overviews
#' @export
a5_raster_info <- function(src, store_opts = NULL) {
  check_scalar_string(src, "src")
  store <- check_store_opts(store_opts)
  r <- a5_raster_info_rs(src, store$keys, store$values)
  list(
    width = as.integer(r$width),
    height = as.integer(r$height),
    n_bands = as.integer(r$n_bands),
    dtype = as.character(r$dtype),
    nodata = if (is.nan(r$nodata)) NA_real_ else as.numeric(r$nodata),
    scale = nan_to_na(r$scale),
    offset = nan_to_na(r$offset),
    band_names = as.character(r$band_names),
    band_metadata = lapply(seq_len(r$n_bands), function(b) {
      metadata_items(r, b)
    }),
    metadata = metadata_items(r, 0L),
    interleave = as.character(r$interleave),
    compression = as.character(r$compression),
    block = c(as.integer(r$block_width), as.integer(r$block_height)),
    n_blocks = c(as.integer(r$n_blocks_x), as.integer(r$n_blocks_y)),
    overviews = data.frame(
      level = as.integer(r$overview_level),
      width = as.integer(r$overview_width),
      height = as.integer(r$overview_height),
      block_width = as.integer(r$overview_block_width),
      block_height = as.integer(r$overview_block_height)
    ),
    crs = as.character(r$crs),
    geotransform = as.numeric(r$geotransform),
    bbox = as.numeric(r$bbox)
  )
}

#' NaN is the Rust side's "absent" marker for scale and offset.
#' @noRd
nan_to_na <- function(x) {
  x <- as.numeric(x)
  x[is.nan(x)] <- NA_real_
  x
}

#' Named character vector of the metadata items of one band (0 = dataset).
#' @noRd
metadata_items <- function(r, band) {
  keep <- as.integer(r$md_band) == band
  out <- as.character(r$md_value)[keep]
  names(out) <- as.character(r$md_name)[keep]
  out
}
