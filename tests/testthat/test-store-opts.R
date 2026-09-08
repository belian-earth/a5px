local_tif <- function() system.file("extdata", "laea_custom.tif", package = "a5px")

# Env vars object_store's from_env() would otherwise pick up from the
# machine running the tests. Cleared so assertions are deterministic.
aws_env_clear <- c(
  AWS_REGION = NA, AWS_DEFAULT_REGION = NA, AWS_NO_SIGN_REQUEST = NA,
  AWS_SKIP_SIGNATURE = NA, AWS_ACCESS_KEY_ID = NA, AWS_SECRET_ACCESS_KEY = NA,
  AWS_SESSION_TOKEN = NA, AWS_ENDPOINT = NA, AWS_ENDPOINT_URL = NA,
  AWS_ENDPOINT_URL_S3 = NA, AWS_VIRTUAL_HOSTED_STYLE_REQUEST = NA
)

s3_url <- "s3://us-west-2.opendata.source.coop/tge-labs/aef/v1/annual/2020/22S/x.tiff"

test_that("check_store_opts normalises names and values", {
  expect_equal(check_store_opts(NULL), list(keys = character(), values = character()))
  expect_equal(check_store_opts(character()), list(keys = character(), values = character()))
  expect_equal(
    check_store_opts(c(AWS_Region = "us-west-2", " aws_skip_signature " = "true")),
    list(keys = c("aws_region", "aws_skip_signature"), values = c("us-west-2", "true"))
  )
  expect_equal(
    check_store_opts(list(aws_region = "us-west-2", aws_skip_signature = TRUE, timeout = 30)),
    list(keys = c("aws_region", "aws_skip_signature", "timeout"),
         values = c("us-west-2", "true", "30"))
  )
})

test_that("check_store_opts rejects malformed input", {
  expect_error(check_store_opts("us-west-2"), "must be named")
  expect_error(check_store_opts(c(aws_region = "a", "b")), "must be named")
  expect_error(check_store_opts(c(aws_region = "a", AWS_REGION = "b")), "duplicated")
  expect_error(check_store_opts(list(aws_region = c("a", "b"))), "length-1")
  expect_error(check_store_opts(list(aws_region = NA_character_)), "length-1")
  expect_error(check_store_opts(42), "named character vector or a named list")
})

test_that("local sources report provider local and refuse store options", {
  f <- local_tif()
  skip_if(f == "")
  expect_equal(a5_store_config(f), c(provider = "local"))
  expect_error(a5_store_config(f, c(aws_region = "us-west-2")), "local path")
  expect_error(
    a5_read_raster(f, 10L, store_opts = c(aws_region = "us-west-2")),
    "local path"
  )
  expect_error(
    a5_read_raster(f, 10L, mode = "centroid", store_opts = c(aws_region = "us-west-2")),
    "local path"
  )
})

test_that("S3 config comes from env, then store_opts, and AWS_NO_SIGN_REQUEST maps to skip_signature", {
  withr::local_envvar(aws_env_clear)

  cfg <- a5_store_config(s3_url)
  expect_equal(cfg[["provider"]], "s3")
  expect_equal(cfg[["host"]], "us-west-2.opendata.source.coop")
  expect_equal(cfg[["region"]], "")
  expect_equal(cfg[["skip_signature"]], "false")
  expect_equal(cfg[["static_credentials"]], "false")

  withr::local_envvar(AWS_REGION = "eu-west-1", AWS_NO_SIGN_REQUEST = "yes")
  cfg <- a5_store_config(s3_url)
  expect_equal(cfg[["region"]], "eu-west-1")
  expect_equal(cfg[["skip_signature"]], "true")

  # explicit options override the environment
  cfg <- a5_store_config(s3_url, c(aws_region = "us-west-2", aws_skip_signature = "false"))
  expect_equal(cfg[["region"]], "us-west-2")
  expect_equal(cfg[["skip_signature"]], "false")

  # GDAL-style falsy value leaves signing on
  withr::local_envvar(AWS_NO_SIGN_REQUEST = "NO")
  expect_equal(a5_store_config(s3_url)[["skip_signature"]], "false")

  # static credentials are reported as a flag, never echoed
  withr::local_envvar(AWS_ACCESS_KEY_ID = "AKIAEXAMPLE", AWS_SECRET_ACCESS_KEY = "secret")
  cfg <- a5_store_config(s3_url)
  expect_equal(cfg[["static_credentials"]], "true")
  expect_false(any(grepl("AKIAEXAMPLE|secret", cfg)))
})

test_that("S3-shaped https URLs resolve to the S3 provider with the URL's region", {
  withr::local_envvar(aws_env_clear)
  cfg <- a5_store_config(
    "https://s3.us-west-2.amazonaws.com/us-west-2.opendata.source.coop/tge-labs/x.tiff",
    c(aws_skip_signature = "true")
  )
  expect_equal(cfg[["provider"]], "s3")
  expect_equal(cfg[["region"]], "us-west-2")
  expect_equal(cfg[["skip_signature"]], "true")
})

test_that("unknown store options error per provider", {
  withr::local_envvar(aws_env_clear)
  expect_error(a5_store_config(s3_url, c(bogus = "1")), "unknown option \"bogus\" for S3")
  expect_error(a5_store_config("gs://bucket/x.tiff", c(aws_region = "x")), "for GCS")
  expect_error(
    a5_store_config("https://data.source.coop/x.tiff", c(aws_region = "x")),
    "for HTTP"
  )
  expect_equal(
    a5_store_config("https://data.source.coop/x.tiff", c(timeout = "30s"))[["provider"]],
    "http"
  )
})

test_that("S3 origin read matches the CDN read (issue #4 acceptance)", {
  skip_on_cran()
  skip_if_not(nzchar(Sys.getenv("A5PX_TEST_REMOTE")), "set A5PX_TEST_REMOTE=1 to run")
  key <- "tge-labs/aef/v1/annual/2020/22S/xqbiklakqrt1rmhr0-0000008192-0000008192.tiff"
  cdn <- paste0("https://data.source.coop/", key)
  s3  <- paste0("s3://us-west-2.opendata.source.coop/", key)
  bbox <- c(-52.95, -19.62, -52.91, -19.58)
  ord <- function(df) df[order(a5R::a5_u64_to_hex(df$cell)), , drop = FALSE]
  read <- function(src, ...) {
    ord(a5_read_raster(src, 16L, bbox = bbox, bands = 1:4,
                       stat = c("mean", "count"), dequant = dequant_aef, ...))
  }

  ref <- read(cdn)
  expect_gt(nrow(ref), 0L)

  withr::with_envvar(
    c(AWS_NO_SIGN_REQUEST = "YES", AWS_REGION = "us-west-2"),
    expect_equal(read(s3), ref)
  )
  withr::with_envvar(
    aws_env_clear,
    expect_equal(
      read(s3, store_opts = c(aws_region = "us-west-2", aws_skip_signature = "true")),
      ref
    )
  )
})
