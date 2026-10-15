# Inspect the object store configuration a source resolves to

Shows how a remote `src` will be accessed after environment variables
and `store_opts` are applied, without making a request. Use it to check
region, endpoint and signing before a long read. Credential values are
never returned, only whether static credentials are present.

## Usage

``` r
a5_store_config(src, store_opts = NULL)
```

## Arguments

- src:

  Path or URL to a GeoTIFF / COG. Supported schemes: local path (no
  scheme), `file://`, `http(s)://`, `s3://`, `gs://`, `az://`. See the
  "Remote sources" section of
  [a5px-package](https://belian-earth.github.io/a5px/reference/a5px-package.md)
  for how cloud clients are configured.

- store_opts:

  Named character vector or named list of object store options for a
  remote `src`, applied after the environment defaults so an explicit
  option always wins. Keys are the `object_store` config keys for the
  provider, e.g. `aws_region`, `aws_skip_signature`,
  `aws_access_key_id`, `aws_secret_access_key`, `aws_session_token`,
  `aws_endpoint`, `aws_virtual_hosted_style_request`, the `google_*` /
  `azure_*` equivalents, and for plain HTTP(S) the client keys such as
  `timeout` or `allow_http`. Unknown keys, and any key given with a
  local path, are errors. Inspect the result with `a5_store_config()`.

## Value

A named character vector. Always contains `provider` (one of `"local"`,
`"s3"`, `"gcs"`, `"azure"`, `"http"`); remote providers add `host` and
provider-specific fields such as `region`, `endpoint`, `skip_signature`
and `static_credentials`.

## See also

The "Remote sources" section of
[a5px-package](https://belian-earth.github.io/a5px/reference/a5px-package.md).

## Examples

``` r
a5_store_config(
  "s3://us-west-2.opendata.source.coop/tge-labs/aef/v1/annual/2020/x.tiff",
  store_opts = c(aws_region = "us-west-2", aws_skip_signature = "true")
)
#>                         provider                             host 
#>                             "s3" "us-west-2.opendata.source.coop" 
#>                           region                         endpoint 
#>                      "us-west-2"                               "" 
#>                   skip_signature     virtual_hosted_style_request 
#>                           "true"                          "false" 
#>               static_credentials                    session_token 
#>                          "false"                          "false" 
```
