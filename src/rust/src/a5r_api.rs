//! a5R's C API for `a5_cell` vectors (a5R >= 0.9.9.9003; signatures in
//! a5R's `inst/include/a5R.h`).
//!
//! a5px used to rebuild a5R's internal byte layout of `a5_cell` itself (eight
//! raw fields, the top byte XOR 0xFC, an NA sentinel), so a layout change in
//! a5R would have given wrong ids without an error. Converting through
//! a5R's own functions removes that coupling. They are looked up once with
//! `R_GetCCallable`, which needs a5R's namespace loaded; importing a5R
//! guarantees it before a5px loads. Every call must be made on R's main
//! thread, as for any R API.

use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

use extendr_api::prelude::*;
use extendr_api::SEXP;

use crate::error::{A5CogError, Result};

/// R's `R_xlen_t` (`ptrdiff_t`).
type RXlen = isize;

extern "C" {
    fn R_GetCCallable(package: *const c_char, name: *const c_char) -> Option<unsafe extern "C" fn() -> *mut c_void>;
}

struct Api {
    version: unsafe extern "C" fn() -> c_int,
    from_u64: unsafe extern "C" fn(*const u64, *const c_int, RXlen) -> SEXP,
    to_u64: unsafe extern "C" fn(SEXP, *mut u64, *mut c_int) -> RXlen,
}

static API: OnceLock<Api> = OnceLock::new();

fn api() -> &'static Api {
    API.get_or_init(|| unsafe {
        let get = |name: &std::ffi::CStr| {
            R_GetCCallable(c"a5R".as_ptr(), name.as_ptr())
                .expect("a5R registers its C API (a5R >= 0.9.9.9003)")
        };
        Api {
            version: std::mem::transmute::<unsafe extern "C" fn() -> *mut c_void, unsafe extern "C" fn() -> c_int>(
                get(c"a5R_api_version"),
            ),
            from_u64: std::mem::transmute::<
                unsafe extern "C" fn() -> *mut c_void,
                unsafe extern "C" fn(*const u64, *const c_int, RXlen) -> SEXP,
            >(get(c"a5R_cells_from_u64")),
            to_u64: std::mem::transmute::<
                unsafe extern "C" fn() -> *mut c_void,
                unsafe extern "C" fn(SEXP, *mut u64, *mut c_int) -> RXlen,
            >(get(c"a5R_cells_to_u64")),
        }
    })
}

/// The version of a5R's C API.
pub(crate) fn version() -> i32 {
    unsafe { (api().version)() }
}

/// The ids of an `a5_cell` vector, NA cells dropped. The R wrappers pass
/// only `a5_cell` vectors.
pub(crate) fn cells_to_u64(cells: &Robj) -> Result<Vec<u64>> {
    if !cells.inherits("a5_cell") {
        return Err(A5CogError::Invalid("expected an a5_cell vector".into()));
    }
    let api = api();
    unsafe {
        let sexp = cells.get();
        let n = (api.to_u64)(sexp, std::ptr::null_mut(), std::ptr::null_mut());
        let n = usize::try_from(n).map_err(|_| A5CogError::Internal("negative a5_cell length".into()))?;
        let mut ids = vec![0u64; n];
        let mut is_na = vec![0 as c_int; n];
        (api.to_u64)(sexp, ids.as_mut_ptr(), is_na.as_mut_ptr());
        Ok(ids.into_iter().zip(is_na).filter(|&(_, na)| na == 0).map(|(id, _)| id).collect())
    }
}

/// An `a5_cell` vector of `ids`.
pub(crate) fn cells_from_u64(ids: &[u64]) -> Robj {
    unsafe { Robj::from_sexp((api().from_u64)(ids.as_ptr(), std::ptr::null(), ids.len() as RXlen)) }
}

/// The a5 crate version a5px is built with: the exact pin in Cargo.toml,
/// which a unit test holds this to.
pub(crate) const A5_CRATE_VERSION: &str = "1.0.0-beta.3";

/// The a5 crate version a5px is built with, recorded in output metadata.
/// @noRd
/// @keywords internal
#[extendr]
fn a5px_a5_crate_version_rs() -> &'static str {
    A5_CRATE_VERSION
}

/// The version of a5R's C API, checked when a5px loads.
/// @noRd
/// @keywords internal
#[extendr]
fn a5px_a5r_api_version_rs() -> i32 {
    version()
}

/// Cells of lon/lat points at `resolution`, by a5px's own a5 crate, for
/// the load-time check that a5px and a5R agree on cell ids.
/// @noRd
/// @keywords internal
#[extendr]
fn a5px_lonlat_to_cells_rs(lon: &[f64], lat: &[f64], resolution: i32) -> Result<Robj> {
    let ids = lon
        .iter()
        .zip(lat)
        .map(|(&x, &y)| {
            a5::lonlat_to_cell(a5::LonLat::new(x, y), resolution).map_err(|e| A5CogError::A5(e.to_string()))
        })
        .collect::<Result<Vec<u64>>>()?;
    Ok(cells_from_u64(&ids))
}

extendr_module! {
    mod a5r_api;
    fn a5px_a5r_api_version_rs;
    fn a5px_lonlat_to_cells_rs;
    fn a5px_a5_crate_version_rs;
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_version_matches_the_cargo_pin() {
        let toml = include_str!("../Cargo.toml");
        let pin = format!("a5 = '={}'", super::A5_CRATE_VERSION);
        assert!(toml.contains(&pin), "Cargo.toml does not pin {pin}");
    }
}
