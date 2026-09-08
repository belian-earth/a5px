//! Object store construction for `src` arguments.
//!
//! `object_store::parse_url` builds cloud clients from the URL alone: no
//! environment variables, no way to skip request signing, region defaulting
//! to us-east-1. That made `s3://` sources unusable off-AWS and mis-regioned
//! on-AWS (issue #4). This module builds each provider from its
//! `from_env()` builder, applies GDAL's `AWS_NO_SIGN_REQUEST` convention,
//! then layers caller-supplied `store_opts` on top so explicit options win.

use std::sync::Arc;

use object_store::aws::{AmazonS3Builder, AmazonS3ConfigKey};
use object_store::azure::{AzureConfigKey, MicrosoftAzureBuilder};
use object_store::gcp::{GoogleCloudStorageBuilder, GoogleConfigKey};
use object_store::http::HttpBuilder;
use object_store::local::LocalFileSystem;
use object_store::path::Path as ObjPath;
use object_store::{ClientConfigKey, ObjectStore, ObjectStoreScheme};

use crate::error::{A5CogError, Result};

/// Caller-supplied `(key, value)` pairs, keys already lower-cased.
pub(crate) type StoreOpts = Vec<(String, String)>;

/// Zip the two parallel character vectors R passes into option pairs.
pub(crate) fn parse_store_opts(keys: Vec<String>, values: Vec<String>) -> Result<StoreOpts> {
    if keys.len() != values.len() {
        return Err(A5CogError::Invalid(format!(
            "store_opts: {} keys but {} values",
            keys.len(),
            values.len()
        )));
    }
    Ok(keys
        .into_iter()
        .map(|k| k.trim().to_ascii_lowercase())
        .zip(values)
        .collect())
}

/// GDAL-style boolean env var: YES / TRUE / ON / 1, case-insensitive.
fn env_truthy(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(
            v.trim().to_ascii_uppercase().as_str(),
            "YES" | "TRUE" | "ON" | "1"
        ),
        Err(_) => false,
    }
}

fn unknown_key(provider: &str, key: &str) -> A5CogError {
    A5CogError::Invalid(format!(
        "store_opts: unknown option {key:?} for {provider} sources \
         (see object_store's {provider} config keys)"
    ))
}

fn reject_opts(what: &str, opts: &StoreOpts) -> Result<()> {
    if let Some((k, _)) = opts.first() {
        return Err(A5CogError::Invalid(format!(
            "store_opts: {k:?} was given but {what} sources take no store options"
        )));
    }
    Ok(())
}

/// Resolve `src` to an object store and the path within it.
///
/// Anything that parses as a URL with a multi-character scheme is remote
/// (single-letter "schemes" are Windows drive letters). Everything else is
/// a local path, which must exist.
pub(crate) fn parse_src(src: &str, opts: &StoreOpts) -> Result<(Arc<dyn ObjectStore>, ObjPath)> {
    if let Ok(url) = url::Url::parse(src) {
        if url.scheme().len() > 1 {
            return build_remote(&url, opts);
        }
    }
    reject_opts("local path", opts)?;
    let p = std::path::Path::new(src);
    if !p.exists() {
        return Err(A5CogError::Invalid(format!("file not found: {src}")));
    }
    let abs = p.canonicalize()?;
    let parent = abs
        .parent()
        .unwrap_or_else(|| std::path::Path::new("/"))
        .to_path_buf();
    let fname = abs
        .file_name()
        .ok_or_else(|| A5CogError::Invalid("path has no file name".into()))?
        .to_string_lossy()
        .to_string();
    let lfs = LocalFileSystem::new_with_prefix(parent)?;
    let store: Arc<dyn ObjectStore> = Arc::new(lfs);
    let path = ObjPath::from(fname.as_str());
    Ok((store, path))
}

/// Builder for one provider: env defaults, URL, then explicit options.
fn s3_builder(url: &url::Url, opts: &StoreOpts) -> Result<AmazonS3Builder> {
    let mut b = AmazonS3Builder::from_env().with_url(url.as_str());
    if env_truthy("AWS_NO_SIGN_REQUEST") {
        b = b.with_skip_signature(true);
    }
    for (k, v) in opts {
        let key = k
            .parse::<AmazonS3ConfigKey>()
            .map_err(|_| unknown_key("S3", k))?;
        b = b.with_config(key, v.clone());
    }
    Ok(b)
}

fn gcs_builder(url: &url::Url, opts: &StoreOpts) -> Result<GoogleCloudStorageBuilder> {
    let mut b = GoogleCloudStorageBuilder::from_env().with_url(url.as_str());
    for (k, v) in opts {
        let key = k
            .parse::<GoogleConfigKey>()
            .map_err(|_| unknown_key("GCS", k))?;
        b = b.with_config(key, v.clone());
    }
    Ok(b)
}

fn azure_builder(url: &url::Url, opts: &StoreOpts) -> Result<MicrosoftAzureBuilder> {
    let mut b = MicrosoftAzureBuilder::from_env().with_url(url.as_str());
    for (k, v) in opts {
        let key = k
            .parse::<AzureConfigKey>()
            .map_err(|_| unknown_key("Azure", k))?;
        b = b.with_config(key, v.clone());
    }
    Ok(b)
}

fn http_builder(url: &url::Url, opts: &StoreOpts) -> Result<HttpBuilder> {
    let base = &url[..url::Position::BeforePath];
    let mut b = HttpBuilder::new().with_url(base);
    for (k, v) in opts {
        let key = k
            .parse::<ClientConfigKey>()
            .map_err(|_| unknown_key("HTTP", k))?;
        b = b.with_config(key, v.clone());
    }
    Ok(b)
}

fn build_remote(url: &url::Url, opts: &StoreOpts) -> Result<(Arc<dyn ObjectStore>, ObjPath)> {
    let (scheme, path) = ObjectStoreScheme::parse(url)
        .map_err(|e| A5CogError::Invalid(format!("unsupported source URL {url}: {e}")))?;
    let store: Box<dyn ObjectStore> = match scheme {
        ObjectStoreScheme::AmazonS3 => Box::new(s3_builder(url, opts)?.build()?),
        ObjectStoreScheme::GoogleCloudStorage => Box::new(gcs_builder(url, opts)?.build()?),
        ObjectStoreScheme::MicrosoftAzure => Box::new(azure_builder(url, opts)?.build()?),
        ObjectStoreScheme::Http => Box::new(http_builder(url, opts)?.build()?),
        ObjectStoreScheme::Local => {
            reject_opts("file://", opts)?;
            Box::new(LocalFileSystem::new())
        }
        other => {
            return Err(A5CogError::Unsupported(format!(
                "source URL scheme {:?} is not supported ({url})",
                other
            )));
        }
    };
    Ok((Arc::from(store), path))
}

/// Region encoded in an S3 https host (`s3.<region>.amazonaws.com` or
/// `<bucket>.s3.<region>.amazonaws.com`). The builder derives it from the
/// URL at build time, overriding env and options, so the diagnostic must
/// report it the same way.
fn s3_url_region(host: &str) -> Option<String> {
    let parts: Vec<&str> = host.split('.').collect();
    match parts.as_slice() {
        ["s3", region, "amazonaws", "com"] => Some((*region).to_string()),
        [_, "s3", region, "amazonaws", "com"] => Some((*region).to_string()),
        _ => None,
    }
}

/// Diagnostic view of the resolved store configuration for `src`, after
/// environment defaults and `opts` are applied. Credential values are never
/// returned, only whether static credentials are set. Building the store
/// validates the configuration without touching the network.
pub(crate) fn describe(src: &str, opts: &StoreOpts) -> Result<Vec<(String, String)>> {
    let url = match url::Url::parse(src) {
        Ok(u) if u.scheme().len() > 1 => u,
        _ => {
            reject_opts("local path", opts)?;
            return Ok(vec![("provider".into(), "local".into())]);
        }
    };
    let (scheme, _) = ObjectStoreScheme::parse(&url)
        .map_err(|e| A5CogError::Invalid(format!("unsupported source URL {url}: {e}")))?;
    let host = url.host_str().unwrap_or("").to_string();
    let flag = |b: bool| if b { "true" } else { "false" }.to_string();
    let mut out: Vec<(String, String)> = Vec::new();
    match scheme {
        ObjectStoreScheme::AmazonS3 => {
            s3_builder(&url, opts)?.build()?;
            let b = s3_builder(&url, opts)?;
            let get = |k: AmazonS3ConfigKey| b.get_config_value(&k).unwrap_or_default();
            out.push(("provider".into(), "s3".into()));
            let region = s3_url_region(&host).unwrap_or_else(|| get(AmazonS3ConfigKey::Region));
            out.push(("host".into(), host.clone()));
            out.push(("region".into(), region));
            out.push(("endpoint".into(), get(AmazonS3ConfigKey::Endpoint)));
            out.push((
                "skip_signature".into(),
                get(AmazonS3ConfigKey::SkipSignature),
            ));
            out.push((
                "virtual_hosted_style_request".into(),
                get(AmazonS3ConfigKey::VirtualHostedStyleRequest),
            ));
            out.push((
                "static_credentials".into(),
                flag(!get(AmazonS3ConfigKey::AccessKeyId).is_empty()),
            ));
            out.push((
                "session_token".into(),
                flag(!get(AmazonS3ConfigKey::Token).is_empty()),
            ));
        }
        ObjectStoreScheme::GoogleCloudStorage => {
            gcs_builder(&url, opts)?.build()?;
            let b = gcs_builder(&url, opts)?;
            let get = |k: GoogleConfigKey| b.get_config_value(&k).unwrap_or_default();
            out.push(("provider".into(), "gcs".into()));
            out.push(("host".into(), host));
            out.push((
                "skip_signature".into(),
                get(GoogleConfigKey::SkipSignature),
            ));
            out.push((
                "static_credentials".into(),
                flag(!get(GoogleConfigKey::ServiceAccount).is_empty()
                    || !get(GoogleConfigKey::ServiceAccountKey).is_empty()
                    || !get(GoogleConfigKey::ApplicationCredentials).is_empty()),
            ));
        }
        ObjectStoreScheme::MicrosoftAzure => {
            azure_builder(&url, opts)?.build()?;
            let b = azure_builder(&url, opts)?;
            let get = |k: AzureConfigKey| b.get_config_value(&k).unwrap_or_default();
            out.push(("provider".into(), "azure".into()));
            out.push(("host".into(), host));
            out.push(("account".into(), get(AzureConfigKey::AccountName)));
            out.push((
                "skip_signature".into(),
                get(AzureConfigKey::SkipSignature),
            ));
            out.push((
                "static_credentials".into(),
                flag(!get(AzureConfigKey::AccessKey).is_empty()
                    || !get(AzureConfigKey::SasKey).is_empty()
                    || !get(AzureConfigKey::Token).is_empty()),
            ));
        }
        ObjectStoreScheme::Http => {
            http_builder(&url, opts)?.build()?;
            out.push(("provider".into(), "http".into()));
            out.push(("host".into(), host));
        }
        ObjectStoreScheme::Local => {
            reject_opts("file://", opts)?;
            out.push(("provider".into(), "local".into()));
        }
        other => {
            return Err(A5CogError::Unsupported(format!(
                "source URL scheme {:?} is not supported ({url})",
                other
            )));
        }
    }
    Ok(out)
}
