use async_tiff::reader::ObjectReader;
use async_tiff::metadata::TiffMetadataReader;
use async_tiff::metadata::cache::ReadaheadMetadataCache;
use object_store::ObjectStore;
use std::sync::Arc;
use object_store::http::HttpBuilder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = "https://data.source.coop/tge-labs/aef/v1/annual/2024/30N/xtth13f8z59uznid0-0000000000-0000008192.tiff";
    let parsed = url::Url::parse(url)?;
    let (store, path) = object_store::parse_url(&parsed)?;
    let store: Arc<dyn ObjectStore> = Arc::from(store);
    let reader = ObjectReader::new(store, path);
    let cache = ReadaheadMetadataCache::new(reader.clone());
    let mut meta = TiffMetadataReader::try_open(&cache).await?;
    let ifds = meta.read_all_ifds(&cache).await?;
    let endianness = meta.endianness();
    let tiff = async_tiff::TIFF::new(ifds, endianness);
    let ifd = &tiff.ifds()[0];
    println!("samples_per_pixel = {}", ifd.samples_per_pixel());
    println!("gdal_nodata = {:?}", ifd.gdal_nodata());
    let md = ifd.gdal_metadata();
    match md {
        Some(s) => {
            println!("gdal_metadata len = {}", s.len());
            // print first 4000 chars
            println!("gdal_metadata head:\n{}", &s[..s.len().min(4000)]);
        }
        None => println!("gdal_metadata = None"),
    }
    Ok(())
}
