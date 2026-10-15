use extendr_api::prelude::*;

mod band_fetch;
mod cell_mask;
mod cell_raw;
mod composite;
mod error;
mod geo;
mod grid_proj;
pub mod locator;
mod meta_cache;
mod parquet_write;
mod range_cache;
mod read;
mod runtime;
mod sample;
mod sources;
mod store;

extendr_module! {
    mod a5px;
    use read;
    use range_cache;
}
