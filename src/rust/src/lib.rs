use extendr_api::prelude::*;

mod band_fetch;
mod cell_raw;
mod error;
mod geo;
mod read;
mod threading;

extendr_module! {
    mod a5px;
    use threading;
    use read;
}
