//! Object store construction for `src` arguments.
//!
//! `object_store::parse_url` builds cloud clients from the URL alone: no
//! environment variables, no way to skip request signing, region defaulting
//! to us-east-1. That made `s3://` sources unusable off-AWS and mis-regioned
//! on-AWS (issue #4). This module builds each provider from its
//! `from_env()` builder, applies GDAL's `AWS_NO_SIGN_REQUEST` convention,
//! then layers caller-supplied `store_opts` on top so explicit options win.
//!
//! GDAL's `/vsis3/`, `/vsigs/`, `/vsiaz/` and `/vsicurl/` paths name the
//! same objects as URLs and are read as those URLs.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

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

/// `src` with a GDAL virtual file system prefix turned into the URL it
/// names; other sources unchanged.
pub(crate) fn gdal_path(src: &str) -> Result<Cow<'_, str>> {
    let Some(rest) = src.strip_prefix("/vsi") else {
        return Ok(Cow::Borrowed(src));
    };
    for (prefix, scheme) in [("s3/", "s3://"), ("gs/", "gs://"), ("az/", "az://")] {
        if let Some(path) = rest.strip_prefix(prefix) {
            return Ok(Cow::Owned(format!("{scheme}{path}")));
        }
    }
    for prefix in ["curl/", "curl_streaming/"] {
        if let Some(url) = rest.strip_prefix(prefix) {
            return Ok(Cow::Borrowed(url));
        }
    }
    let name = src.split('/').nth(1).unwrap_or(src);
    Err(A5CogError::Invalid(format!(
        "{src}: GDAL's /{name}/ is not supported; give the file's URL or path \
         (/vsis3/, /vsigs/, /vsiaz/ and /vsicurl/ paths are read as URLs)"
    )))
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
/// The session cache key of `src`: the URL and the store options, which
/// can point one URL at another endpoint. `None` for a local file (no URL
/// scheme, or `file`), which the cache skips.
pub(crate) fn cache_key(src: &str, opts: &StoreOpts) -> Option<String> {
    let src = gdal_path(src).ok()?;
    let src = src.as_ref();
    match url::Url::parse(src) {
        Ok(u) if u.scheme().len() > 1 && u.scheme() != "file" => {
            let mut opts: Vec<String> = opts.iter().map(|(k, v)| format!("{k}={v}")).collect();
            opts.sort();
            Some(format!("{src}\n{}", opts.join("\n")))
        }
        _ => None,
    }
}

pub(crate) fn parse_src(src: &str, opts: &StoreOpts) -> Result<(Arc<dyn ObjectStore>, ObjPath)> {
    let src = gdal_path(src)?;
    let src = src.as_ref();
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
    let get = |k: AmazonS3ConfigKey| b.get_config_value(&k).unwrap_or_default();
    let allow_http = AmazonS3ConfigKey::Client(ClientConfigKey::AllowHttp);
    // an http:// endpoint (MinIO, a local mock) is what the caller asked for
    let http_endpoint = get(AmazonS3ConfigKey::Endpoint).starts_with("http://") && !opts_set(opts, &allow_http);
    // With no credentials configured the client asks the instance metadata
    // service, which off AWS retries for 14-27 s and then fails; where it
    // does not answer, and signing was not chosen either way, sign
    // nothing, as GDAL's /vsicurl/ reads such a URL.
    // Signing chosen either way (an option, or the variables) is kept.
    let chosen = opts_set(opts, &AmazonS3ConfigKey::SkipSignature)
        || std::env::var_os("AWS_SKIP_SIGNATURE").is_some()
        || std::env::var_os("AWS_NO_SIGN_REQUEST").is_some();
    let creds = !get(AmazonS3ConfigKey::AccessKeyId).is_empty()
        || !get(AmazonS3ConfigKey::WebIdentityTokenFile).is_empty()
        || !get(AmazonS3ConfigKey::ContainerCredentialsRelativeUri).is_empty()
        || !get(AmazonS3ConfigKey::ContainerCredentialsFullUri).is_empty();
    let unsigned = !chosen && !creds && !metadata_service_answers(&get(AmazonS3ConfigKey::MetadataEndpoint));
    if http_endpoint {
        b = b.with_allow_http(true);
    }
    if unsigned {
        b = b.with_skip_signature(true);
    }
    Ok(b)
}

/// Whether `opts` sets the S3 key `key`.
fn opts_set(opts: &StoreOpts, key: &AmazonS3ConfigKey) -> bool {
    opts.iter().any(|(k, _)| k.parse::<AmazonS3ConfigKey>().is_ok_and(|p| &p == key))
}

/// Whether the instance metadata service at `endpoint` (object_store's
/// default when empty) accepts a connection within 300 ms; probed once per
/// endpoint per session.
fn metadata_service_answers(endpoint: &str) -> bool {
    static SEEN: OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> = OnceLock::new();
    let endpoint = if endpoint.is_empty() { "http://169.254.169.254" } else { endpoint };
    let seen = SEEN.get_or_init(Default::default);
    if let Some(&ok) = seen.lock().unwrap_or_else(|e| e.into_inner()).get(endpoint) {
        return ok;
    }
    let ok = url::Url::parse(endpoint)
        .ok()
        .and_then(|u| u.socket_addrs(|| Some(80)).ok())
        .and_then(|a| a.into_iter().next())
        .is_some_and(|a| std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_millis(300)).is_ok());
    seen.lock().unwrap_or_else(|e| e.into_inner()).insert(endpoint.to_string(), ok);
    ok
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
    // an http:// URL is what the caller asked for
    let mut b = HttpBuilder::new().with_url(base);
    if url.scheme() == "http" {
        b = b.with_config(ClientConfigKey::AllowHttp, "true");
    }
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
    let src = gdal_path(src)?;
    let url = match url::Url::parse(src.as_ref()) {
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

#[cfg(test)]
mod gdal_tests {
    use super::gdal_path;

    #[test]
    fn gdal_paths_become_urls() {
        assert_eq!(gdal_path("/vsis3/b/k.tif").unwrap(), "s3://b/k.tif");
        assert_eq!(gdal_path("/vsigs/b/k.tif").unwrap(), "gs://b/k.tif");
        assert_eq!(gdal_path("/vsiaz/c/k.tif").unwrap(), "az://c/k.tif");
        assert_eq!(gdal_path("/vsicurl/https://h/k.tif").unwrap(), "https://h/k.tif");
        assert_eq!(gdal_path("/data/k.tif").unwrap(), "/data/k.tif");
        assert!(gdal_path("/vsizip/a.zip/k.tif").is_err());
    }
}
