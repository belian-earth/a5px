//! Local-file variant of probe.rs — dump GeoKeyDirectory citation fields.

use async_tiff::metadata::TiffMetadataReader;
use async_tiff::metadata::cache::ReadaheadMetadataCache;
use async_tiff::reader::ObjectReader;
use object_store::ObjectStore;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path_str = std::env::args().nth(1).expect("usage: probe_local <path>");
    let path = std::path::Path::new(&path_str).canonicalize()?;
    let parent = path.parent().unwrap().to_path_buf();
    let fname = path.file_name().unwrap().to_string_lossy().to_string();
    let lfs = object_store::local::LocalFileSystem::new_with_prefix(parent)?;
    let store: Arc<dyn ObjectStore> = Arc::new(lfs);
    let p = object_store::path::Path::from(fname.as_str());
    let reader = ObjectReader::new(store, p);
    let cache = ReadaheadMetadataCache::new(reader.clone());
    let mut meta = TiffMetadataReader::try_open(&cache).await?;
    let ifds = meta.read_all_ifds(&cache).await?;
    let endianness = meta.endianness();
    let tiff = async_tiff::TIFF::new(ifds, endianness);
    let ifd = &tiff.ifds()[0];
    let geo = ifd.geo_key_directory().expect("no geokeys");
    println!("epsg_code = {:?}", geo.epsg_code());
    println!("model_type = {:?}", geo.model_type);
    println!("projected_type = {:?}", geo.projected_type);
    println!("geographic_type = {:?}", geo.geographic_type);
    println!("citation = {:?}", geo.citation);
    println!("geog_citation = {:?}", geo.geog_citation);
    println!("proj_citation = {:?}", geo.proj_citation);
    println!("projection = {:?}", geo.projection);
    println!("proj_coord_trans = {:?}", geo.proj_coord_trans);
    println!("proj_linear_units = {:?}", geo.proj_linear_units);
    println!("proj_linear_unit_size = {:?}", geo.proj_linear_unit_size);
    println!("proj_std_parallel1 = {:?}", geo.proj_std_parallel1);
    println!("proj_std_parallel2 = {:?}", geo.proj_std_parallel2);
    println!("proj_nat_origin_long = {:?}", geo.proj_nat_origin_long);
    println!("proj_nat_origin_lat = {:?}", geo.proj_nat_origin_lat);
    println!("proj_false_easting = {:?}", geo.proj_false_easting);
    println!("proj_false_northing = {:?}", geo.proj_false_northing);
    println!("proj_false_origin_long = {:?}", geo.proj_false_origin_long);
    println!("proj_false_origin_lat = {:?}", geo.proj_false_origin_lat);
    println!("proj_center_long = {:?}", geo.proj_center_long);
    println!("proj_center_lat = {:?}", geo.proj_center_lat);
    println!("proj_scale_at_nat_origin = {:?}", geo.proj_scale_at_nat_origin);
    println!("proj_scale_at_center = {:?}", geo.proj_scale_at_center);
    println!("proj_azimuth_angle = {:?}", geo.proj_azimuth_angle);
    println!("proj_straight_vert_pole_long = {:?}", geo.proj_straight_vert_pole_long);
    println!("geog_geodetic_datum = {:?}", geo.geog_geodetic_datum);
    println!("geog_ellipsoid = {:?}", geo.geog_ellipsoid);
    println!("geog_semi_major_axis = {:?}", geo.geog_semi_major_axis);
    println!("geog_semi_minor_axis = {:?}", geo.geog_semi_minor_axis);
    println!("geog_inv_flattening = {:?}", geo.geog_inv_flattening);
    println!("geog_prime_meridian = {:?}", geo.geog_prime_meridian);
    println!("geog_prime_meridian_long = {:?}", geo.geog_prime_meridian_long);
    println!("geog_angular_units = {:?}", geo.geog_angular_units);
    Ok(())
}
