//! Geotransform extraction from GeoTIFF tags.
//!
//! GDAL-style affine geotransform `[gt0..gt5]` such that for pixel `(col, row)`:
//!   x = gt0 + col * gt1 + row * gt2
//!   y = gt3 + col * gt4 + row * gt5
//! `(col, row)` are CORNER coordinates of the pixel; pixel CENTER is at
//! `(col + 0.5, row + 0.5)`.

use async_tiff::ImageFileDirectory;

use crate::error::{A5CogError, Result};

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
