# Synthetic AEF-like COG for benchmarks/ab-fixtures.R: 64 bands Int8, planar,
# 1024 blocks, ZSTD, UTM 50N at 10 m (Sabah), nodata -128, smooth fields + noise.
#   python3 benchmarks/make-aef-synth.py benchmarks/ab-out/aef_synth_4096.tif 4096
# Needs the GDAL python bindings and numpy.
import numpy as np, sys
from osgeo import gdal, osr
gdal.UseExceptions()
out = sys.argv[1]; n = int(sys.argv[2]); nb = 64
drv = gdal.GetDriverByName("GTiff")
ds = drv.Create(out, n, n, nb, gdal.GDT_Int8, options=[
    "TILED=YES","BLOCKXSIZE=1024","BLOCKYSIZE=1024","COMPRESS=ZSTD","INTERLEAVE=BAND","BIGTIFF=YES"])
srs = osr.SpatialReference(); srs.ImportFromEPSG(32650)
ds.SetProjection(srs.ExportToWkt())
ds.SetGeoTransform((720000.0, 10.0, 0.0, 608000.0, 0.0, -10.0))
rng = np.random.default_rng(1)
yy, xx = np.mgrid[0:n, 0:n]
for b in range(nb):
    base = 40*np.sin(xx/(97.0+b)) + 40*np.cos(yy/(71.0+b))
    arr = np.clip(base + rng.normal(0, 8, size=(n, n)), -127, 127).astype(np.int8)
    band = ds.GetRasterBand(b+1); band.WriteArray(arr); band.SetNoDataValue(-128)
ds.FlushCache(); ds = None
print("wrote", out)
