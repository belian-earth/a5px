use extendr_api::prelude::*;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod band_fetch;
mod cell_raw;
mod error;
mod geo;
mod parquet_write;
mod read;
mod threading;

extendr_module! {
    mod a5px;
    use threading;
    use read;
}
