//! Geotransform extraction from GeoTIFF tags.
//!
//! GDAL-style affine geotransform `[gt0..gt5]` such that for pixel `(col, row)`:
//!   x = gt0 + col * gt1 + row * gt2
//!   y = gt3 + col * gt4 + row * gt5
//! `(col, row)` are CORNER coordinates of the pixel; pixel CENTER is at
//! `(col + 0.5, row + 0.5)`.

use async_tiff::ImageFileDirectory;
use async_tiff::geo::GeoKeyDirectory;
use proj4rs::Proj;

use crate::error::{A5CogError, Result};

/// EPSG sentinel for "user-defined CRS" per the GeoTIFF spec.
const EPSG_USER_DEFINED: u16 = 32767;

/// Resolve the source CRS into a [`proj4rs::Proj`].
///
/// Tries, in order:
/// 1. EPSG code from the GeoKeyDirectory (`projected_type` or
///    `geographic_type`), via [`proj4rs::Proj::from_epsg_code`]. Skipped when
///    the code is `0` or the user-defined sentinel (`32767`).
/// 2. A WKT string in `proj_citation` / `geog_citation` / `citation`
///    (whichever looks like WKT), parsed via [`proj4wkt::wkt_to_projstring`]
///    then handed to [`proj4rs::Proj::from_proj_string`].
/// 3. Reconstruction of a proj string from the explicit GeoKey fields
///    (ProjCoordTransGeoKey + ProjNatOrigin/Center/StdParallel/etc).
///    This covers GDAL-written user-defined projections (no EPSG, no WKT
///    citation) such as a centered LAEA produced via
///    `+proj=laea +lon_0=... +lat_0=...`.
pub(crate) fn build_src_proj(geo: &GeoKeyDirectory) -> Result<Proj> {
    let mut last_err: Option<A5CogError> = None;

    if let Some(epsg) = geo.epsg_code() {
        if epsg != EPSG_USER_DEFINED && epsg != 0 {
            match Proj::from_epsg_code(epsg) {
                Ok(p) => return Ok(p),
                Err(e) => last_err = Some(e.into()),
            }
        }
    }

    if let Some(wkt) = first_wkt_citation(geo) {
        match proj4wkt::wkt_to_projstring(wkt) {
            Ok(proj_str) => match Proj::from_proj_string(&proj_str) {
                Ok(p) => return Ok(p),
                Err(e) => last_err = Some(e.into()),
            },
            Err(e) => last_err = Some(A5CogError::Invalid(format!("WKT parse: {e:?}"))),
        }
    }

    if let Some(proj_str) = build_proj_string_from_geokeys(geo) {
        match Proj::from_proj_string(&proj_str) {
            Ok(p) => return Ok(p),
            Err(e) => last_err = Some(e.into()),
        }
    }

    Err(last_err.unwrap_or(A5CogError::MissingGeoKey(
        "EPSG code, WKT citation, or recognisable GeoKey projection params",
    )))
}

fn first_wkt_citation(geo: &GeoKeyDirectory) -> Option<&str> {
    [
        geo.proj_citation.as_deref(),
        geo.geog_citation.as_deref(),
        geo.citation.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|s| looks_like_wkt(s))
}

/// Reconstruct a proj string from explicit GeoKey projection parameters.
/// Returns `None` if the projection is unknown/unsupported or essential
/// parameters are missing.
///
/// Supports the common projections written by GDAL when a custom
/// (`+proj=...`) string is set without an EPSG code: TM, Mercator, LCC
/// (1SP and 2SP), LAEA, AEA, polar / oblique stereographic, equirectangular,
/// orthographic, sinusoidal, cylindrical-equal-area, azimuthal-equidistant.
pub(crate) fn build_proj_string_from_geokeys(geo: &GeoKeyDirectory) -> Option<String> {
    let coord_trans = geo.proj_coord_trans?;
    let lon_0 = geo
        .proj_nat_origin_long
        .or(geo.proj_center_long)
        .or(geo.proj_false_origin_long)
        .or(geo.proj_straight_vert_pole_long);
    let lat_0 = geo
        .proj_nat_origin_lat
        .or(geo.proj_center_lat)
        .or(geo.proj_false_origin_lat);
    let x_0 = geo.proj_false_easting.or(geo.proj_false_origin_easting);
    let y_0 = geo.proj_false_northing.or(geo.proj_false_origin_northing);

    let (proj, extras): (&str, Vec<String>) = match coord_trans {
        // CT_TransverseMercator
        1 => {
            let k = geo.proj_scale_at_nat_origin.unwrap_or(1.0);
            ("tmerc", vec![format!("+k_0={k}")])
        }
        // CT_Mercator. 1SP uses ProjScaleAtNatOriginGeoKey, 2SP uses
        // ProjStdParallel1GeoKey (-> +lat_ts).
        7 => {
            if let Some(p1) = geo.proj_std_parallel1 {
                ("merc", vec![format!("+lat_ts={p1}")])
            } else {
                let k = geo.proj_scale_at_nat_origin.unwrap_or(1.0);
                ("merc", vec![format!("+k_0={k}")])
            }
        }
        // CT_LambertConfConic_2SP
        8 => {
            let p1 = geo.proj_std_parallel1?;
            let p2 = geo.proj_std_parallel2?;
            ("lcc", vec![format!("+lat_1={p1}"), format!("+lat_2={p2}")])
        }
        // CT_LambertConfConic_Helmert (1SP)
        9 => {
            let k = geo.proj_scale_at_nat_origin.unwrap_or(1.0);
            ("lcc", vec![format!("+k_0={k}")])
        }
        // CT_LambertAzimEqualArea (no scale parameter; +lon_0/+lat_0 set below)
        10 => ("laea", vec![]),
        // CT_AlbersEqualArea (no scale parameter)
        11 => {
            let p1 = geo.proj_std_parallel1?;
            let p2 = geo.proj_std_parallel2?;
            ("aea", vec![format!("+lat_1={p1}"), format!("+lat_2={p2}")])
        }
        // CT_AzimuthalEquidistant (no scale parameter)
        12 => ("aeqd", vec![]),
        // CT_EquidistantConic
        13 => {
            let p1 = geo.proj_std_parallel1?;
            let p2 = geo.proj_std_parallel2?;
            ("eqdc", vec![format!("+lat_1={p1}"), format!("+lat_2={p2}")])
        }
        // CT_Stereographic
        14 => {
            let k = geo.proj_scale_at_nat_origin.unwrap_or(1.0);
            ("stere", vec![format!("+k_0={k}")])
        }
        // CT_PolarStereographic. Variant A uses scale_at_nat_origin;
        // variant B uses std_parallel1 (-> +lat_ts) for the latitude of true
        // scale (NSIDC sea-ice tiles, etc.).
        15 => {
            if let Some(p1) = geo.proj_std_parallel1 {
                ("stere", vec![format!("+lat_ts={p1}")])
            } else {
                let k = geo.proj_scale_at_nat_origin.unwrap_or(1.0);
                ("stere", vec![format!("+k_0={k}")])
            }
        }
        // CT_ObliqueStereographic ("double stereographic", e.g. RD New /
        // Amersfoort). proj distinguishes this from CT_Stereographic via the
        // +proj=sterea projection name (the conformal-sphere step matters).
        16 => {
            let k = geo.proj_scale_at_nat_origin.unwrap_or(1.0);
            ("sterea", vec![format!("+k_0={k}")])
        }
        // CT_Equirectangular. Optional ProjStdParallel1GeoKey gives the
        // parallel of true scale (+lat_ts).
        17 => (
            "eqc",
            geo.proj_std_parallel1
                .map(|p| format!("+lat_ts={p}"))
                .into_iter()
                .collect(),
        ),
        // CT_Gnomonic / CT_Orthographic / CT_Sinusoidal (no scale parameter)
        19 => ("gnom", vec![]),
        21 => ("ortho", vec![]),
        24 => ("sinu", vec![]),
        // CT_CylindricalEqualArea. Requires a standard parallel (+lat_ts);
        // covers Behrmann, Lambert cylindrical, Gall-Peters, etc. Falling
        // back to lat_ts=0 is the equator-based default and gives wrong
        // scaling for the named variants.
        28 => (
            "cea",
            geo.proj_std_parallel1
                .map(|p| format!("+lat_ts={p}"))
                .into_iter()
                .collect(),
        ),
        _ => return None,
    };

    let mut parts: Vec<String> = vec![format!("+proj={proj}")];
    if let Some(v) = lon_0 { parts.push(format!("+lon_0={v}")); }
    if let Some(v) = lat_0 { parts.push(format!("+lat_0={v}")); }
    if let Some(v) = x_0 { parts.push(format!("+x_0={v}")); }
    if let Some(v) = y_0 { parts.push(format!("+y_0={v}")); }
    parts.extend(extras);
    if let Some(pm) = prime_meridian_param(geo) {
        parts.push(pm);
    }
    parts.push(ellipsoid_param(geo));
    parts.push("+no_defs".to_string());
    Some(parts.join(" "))
}

/// Render a `+pm=...` proj parameter from the GeogPrimeMeridian* GeoKeys.
/// Returns None when unset or Greenwich (the proj default).
fn prime_meridian_param(geo: &GeoKeyDirectory) -> Option<String> {
    // Named EPSG meridian codes that proj recognises by name. 8901 = Greenwich
    // (the default; we skip it to keep the proj string tidy).
    if let Some(code) = geo.geog_prime_meridian {
        let name = match code {
            8901 => return None, // Greenwich (default)
            8902 => Some("lisbon"),
            8903 => Some("paris"),
            8904 => Some("bogota"),
            8905 => Some("madrid"),
            8906 => Some("rome"),
            8907 => Some("bern"),
            8908 => Some("jakarta"),
            8909 => Some("ferro"),
            8910 => Some("brussels"),
            8911 => Some("stockholm"),
            8912 => Some("athens"),
            8913 => Some("oslo"),
            _ => None,
        };
        if let Some(n) = name {
            return Some(format!("+pm={n}"));
        }
    }
    if let Some(deg) = geo.geog_prime_meridian_long {
        if deg != 0.0 {
            return Some(format!("+pm={deg}"));
        }
    }
    None
}

/// Pick the best available ellipsoid spec from the GeoKey fields.
fn ellipsoid_param(geo: &GeoKeyDirectory) -> String {
    if let Some(ellps_epsg) = geo.geog_ellipsoid {
        if let Some(name) = ellps_name_for_epsg(ellps_epsg) {
            return format!("+ellps={name}");
        }
    }
    let a = geo.geog_semi_major_axis;
    let b = geo.geog_semi_minor_axis;
    let rf = geo.geog_inv_flattening;
    if let Some(a_) = a {
        if let Some(rf_) = rf {
            return format!("+a={a_} +rf={rf_}");
        }
        if let Some(b_) = b {
            return format!("+a={a_} +b={b_}");
        }
        return format!("+a={a_}");
    }
    "+ellps=WGS84".to_string()
}

/// Common EPSG ellipsoid codes -> proj +ellps= shorthand. Falls back to
/// returning None for codes outside this list (caller will try explicit
/// semi-major/minor instead).
fn ellps_name_for_epsg(code: u16) -> Option<&'static str> {
    // Codes outside this list fall through to the explicit semi-major /
    // inv-flattening fields the GeoKey directory carries. Three mappings
    // were dropped from earlier versions because they were wrong:
    //   - 7034 (Clarke 1880, original) is NOT proj's clrk80 (Clarke 1880 RGS)
    //   - 7048 (GRS 1980 Authalic Sphere) is a sphere, NOT the GRS80 ellipsoid
    // Both now route through the explicit-parameter fallback.
    Some(match code {
        7001 => "airy",
        7002 => "mod_airy",
        7003 => "aust_SA",     // Australian National Spheroid
        7004 => "bessel",
        7008 => "clrk66",      // Clarke 1866
        7012 => "clrk80",      // Clarke 1880 (RGS) - proj's clrk80
        7019 => "GRS80",
        7022 => "intl",        // International 1924
        7030 => "WGS84",
        7035 => "sphere",      // sphere of radius 6371000
        7043 => "WGS72",
        _ => return None,
    })
}

fn looks_like_wkt(s: &str) -> bool {
    let s = s.trim_start();
    matches!(
        s.split(|c: char| c == '[' || c == '(' || c.is_whitespace())
            .next()
            .unwrap_or(""),
        "PROJCS"
            | "PROJCRS"
            | "GEOGCS"
            | "GEOGCRS"
            | "GEODCRS"
            | "BOUNDCRS"
            | "COMPD_CS"
            | "COMPDCRS"
            | "ENGCRS"
            | "ENGCS"
            | "VERTCS"
            | "VERTCRS"
            | "TIMECRS"
    )
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GeoTransform(pub [f64; 6]);

impl GeoTransform {
    /// Compute the projected (x, y) of a pixel centre at index (col, row).
    #[inline]
    pub fn pixel_centre(&self, col: usize, row: usize) -> (f64, f64) {
        let c = col as f64 + 0.5;
        let r = row as f64 + 0.5;
        let x = self.0[0] + c * self.0[1] + r * self.0[2];
        let y = self.0[3] + c * self.0[4] + r * self.0[5];
        (x, y)
    }
}

pub(crate) fn extract_geotransform(ifd: &ImageFileDirectory) -> Result<GeoTransform> {
    if let Some(m) = ifd.model_transformation() {
        if m.len() < 16 {
            return Err(A5CogError::Invalid(
                "ModelTransformation tag has fewer than 16 elements".into(),
            ));
        }
        // 4x4 row-major: m[0..4]=row 0 (a, b, _, c), m[4..8]=row 1 (d, e, _, f)
        // GeoTransform layout: gt0 = c, gt1 = a, gt2 = b, gt3 = f, gt4 = d, gt5 = e
        return Ok(GeoTransform([m[3], m[0], m[1], m[7], m[4], m[5]]));
    }

    let scale = ifd
        .model_pixel_scale()
        .ok_or(A5CogError::MissingGeoKey("ModelPixelScale"))?;
    let tie = ifd
        .model_tiepoint()
        .ok_or(A5CogError::MissingGeoKey("ModelTiepoint"))?;
    if scale.len() < 3 || tie.len() < 6 {
        return Err(A5CogError::Invalid(
            "ModelPixelScale or ModelTiepoint malformed".into(),
        ));
    }
    let (sx, sy) = (scale[0], scale[1]);
    let (i, j, _k, x, y, _z) = (tie[0], tie[1], tie[2], tie[3], tie[4], tie[5]);
    // Standard GeoTIFF: tiepoint at corner (I,J) in raster space maps to (X,Y) in
    // model space; positive Y is up so row increases as y decreases.
    let gt0 = x - i * sx;
    let gt1 = sx;
    let gt2 = 0.0;
    let gt3 = y + j * sy;
    let gt4 = 0.0;
    let gt5 = -sy;
    Ok(GeoTransform([gt0, gt1, gt2, gt3, gt4, gt5]))
}

/// Dataset-wide nodata, parsed from the `TIFFTAG_GDAL_NODATA` ASCII tag that
/// async-tiff exposes via [`ImageFileDirectory::gdal_nodata`].
///
/// GeoTIFF only supports a single nodata value per dataset; if the writer
/// originally had per-band nodata, GDAL collapses to the most recent value
/// (with a warning) and writes the single tag. True per-band nodata requires
/// VRT or a sidecar metadata file, which a5px does not yet read.
///
/// Parses common GDAL spellings: integers, decimals, scientific notation,
/// `nan`, `+nan`, `inf`, `-inf`. Returns `None` if the tag is absent or the
/// value can't be parsed as f64.
pub(crate) fn parse_nodata(ifd: &ImageFileDirectory) -> Option<f64> {
    let s = ifd.gdal_nodata()?;
    let t = s.trim();
    // case-insensitive nan: f64::from_str only accepts lowercase
    let lc = t.to_ascii_lowercase();
    lc.parse::<f64>().ok()
}

/// Compare a pixel value against a nodata sentinel, with NaN-safe semantics.
/// `nodata` of `NaN` matches any pixel where `v.is_nan()` (since NaN != NaN
/// under IEEE 754, a naive `v == nodata` would never match).
#[inline]
pub(crate) fn is_nodata(v: f64, nodata: f64) -> bool {
    if nodata.is_nan() {
        v.is_nan()
    } else {
        v == nodata
    }
}

/// Parse GDAL's per-band `<Item name="DESCRIPTION" sample="N">band_name</Item>`
/// from the GDAL_METADATA XML. Returns descriptions in band order or an empty
/// vec if none parseable.
pub(crate) fn parse_band_descriptions(ifd: &ImageFileDirectory, n_bands: usize) -> Vec<String> {
    let xml = match ifd.gdal_metadata() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut out: Vec<String> = (0..n_bands).map(|i| format!("band_{:02}", i + 1)).collect();
    let mut found_any = false;
    walk_items(xml, |sample, attrs, body| {
        if !attr_eq(attrs, "name", "DESCRIPTION") {
            return;
        }
        if let Some(s) = sample {
            if s < n_bands {
                out[s] = body.trim().to_string();
                found_any = true;
            }
        }
    });
    if found_any { out } else { Vec::new() }
}

/// Walk every `<Item ...>body</Item>` element and call `f(sample, attrs, body)`.
/// `sample` is the parsed value of the `sample="N"` attribute, if present.
fn walk_items<F: FnMut(Option<usize>, &str, &str)>(xml: &str, mut f: F) {
    let mut rest = xml;
    while let Some(idx) = rest.find("<Item") {
        rest = &rest[idx..];
        let close = match rest.find('>') {
            Some(c) => c,
            None => break,
        };
        let attrs = &rest[..close];
        let body_end = match rest[close..].find("</Item>") {
            Some(e) => close + e,
            None => break,
        };
        let body = &rest[close + 1..body_end];
        let sample = parse_attr(attrs, "sample").and_then(|s| s.parse::<usize>().ok());
        f(sample, attrs, body);
        rest = &rest[body_end + "</Item>".len()..];
    }
}

fn parse_attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{}=\"", name);
    let i = attrs.find(&needle)?;
    let tail = &attrs[i + needle.len()..];
    let q = tail.find('"')?;
    Some(&tail[..q])
}

fn attr_eq(attrs: &str, name: &str, value: &str) -> bool {
    parse_attr(attrs, name) == Some(value)
}
