# Predictors on multi-band planar (INTERLEAVE=BAND) TIFFs: async-tiff 0.4.0
# reversed them with a stride of samples-per-pixel instead of 1 (upstream
# issue #346), so these files read as garbage. Fixtures, both be_planar.tif
# re-encoded with gdal_translate (TILED, 32x32 blocks, DEFLATE,
# INTERLEAVE=BAND), hold the same pixels as it:
# - planar_pred2.tif: uint16, PREDICTOR=2 (horizontal differencing)
# - planar_pred3.tif: float32, PREDICTOR=3 (floating point)
ext <- function(name) system.file("extdata", name, package = "a5px")
hexkey <- function(cells) a5R::a5_u64_to_hex(cells)
ord <- function(df) df[order(hexkey(df$cell)), , drop = FALSE]

for (f in c("planar_pred2.tif", "planar_pred3.tif")) {
  test_that(paste(f, "reads the same pixels as the file without a predictor"), {
    skip_if(ext(f) == "")
    ref <- ord(a5_read_raster(ext("be_planar.tif"), 14L, stat = c("mean", "min", "max")))
    got <- ord(a5_read_raster(ext(f), 14L, stat = c("mean", "min", "max")))
    expect_identical(hexkey(got$cell), hexkey(ref$cell))
    expect_equal(as.data.frame(got)[-1], as.data.frame(ref)[-1])
    expect_true(all(got$band_01_min >= 1 & got$band_01_max <= 4096))
  })

  test_that(paste(f, "decodes band subsets and centroid samples"), {
    skip_if(ext(f) == "")
    ref <- ord(a5_read_raster(ext("be_planar.tif"), 12L, stat = "mean", bands = 2L))
    got <- ord(a5_read_raster(ext(f), 12L, stat = "mean", bands = 2L))
    expect_equal(got$band_02, ref$band_02)
    ref_c <- ord(a5_read_raster(ext("be_planar.tif"), 14L, mode = "centroid"))
    got_c <- ord(a5_read_raster(ext(f), 14L, mode = "centroid"))
    expect_equal(as.data.frame(got_c)[-1], as.data.frame(ref_c)[-1])
  })
}
