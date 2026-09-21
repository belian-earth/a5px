# Builds inst/extdata/scoff_int16.tif: the scale/offset fixture.
# 512x512, 2 x int16, UTM 33N at 30 m, 128x128 tiles, AVERAGE overviews at
# decimation 2 and 4, nodata -32768 over the top-left 64x64 pixels.
#   band 1 "rh98": code = col + row        scale 0.01, offset 0, unit "m"
#   band 2:        code = 2 * col - row    scale 0.5,  offset 10
# Band 2 also carries a custom item and the dataset one, for the
# band_metadata / metadata fields of a5_raster_info().
library(gdalraster)

n <- 512L
tmp <- tempfile(fileext = ".tif")
ds <- create("GTiff", tmp, n, n, 2L, "Int16", return_obj = TRUE)
ds$setProjection(epsg_to_wkt(32633))
ds$setGeoTransform(c(500000, 30, 0, 5000000, 0, -30))
cols <- 0:(n - 1L)
for (row in 0:(n - 1L)) {
  b1 <- cols + row
  b2 <- 2L * cols - row
  if (row < 64L) {
    b1[1:64] <- -32768L
    b2[1:64] <- -32768L
  }
  ds$write(1L, 0L, row, n, 1L, b1)
  ds$write(2L, 0L, row, n, 1L, b2)
}
for (b in 1:2) ds$setNoDataValue(b, -32768)
ds$setScale(1L, 0.01)
ds$setOffset(1L, 0)
ds$setUnitType(1L, "m")
ds$setDescription(1L, "rh98")
ds$setScale(2L, 0.5)
ds$setOffset(2L, 10)
ds$setMetadataItem(2L, "SOURCE", "synthetic <a5px> & co", "")
ds$setMetadataItem(0L, "PRODUCT", "scoff fixture", "")
ds$close()

translate(
  tmp, "inst/extdata/scoff_int16.tif",
  cl_arg = c(
    "-of", "COG", "-co", "BLOCKSIZE=128", "-co", "COMPRESS=DEFLATE",
    "-co", "PREDICTOR=2", "-co", "OVERVIEW_RESAMPLING=AVERAGE",
    "-co", "OVERVIEW_COUNT=2"
  )
)
