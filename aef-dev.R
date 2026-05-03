#' Download AEF tile index from Source Cooperative
#'
#' Fetches the AEF tile index for a given year, adds a `gdal_path` column
#' pointing to the VRT-wrapped S3 paths, and writes to a local parquet.
#'
#' @param year Numeric. Year to filter (e.g. 2020).
#' @param outfile Character. Output parquet path.
#' @param force Logical. Re-download even if `outfile` exists.
#' @return `outfile` path.
aef_index_srccoop <- function(
  year,
  outfile = fs::path(
    glue::glue("aef-index-src-coop/", "year={year}/"),
    "aef_index.parquet"
  ),
  force = FALSE
) {
  assertthat::assert_that(inherits(outfile, "character"))
  assertthat::assert_that(inherits(force, "logical"))

  fs::dir_create(fs::path_dir(outfile))

  con <- DBI::dbConnect(duckdb::duckdb(), dbdir = ":memory:")
  on.exit(DBI::dbDisconnect(con, shutdown = TRUE))
  q <- glue::glue_sql(
    "  INSTALL httpfs;

           LOAD httpfs;

           COPY (
         SELECT *,
                regexp_replace(location, '^VRT:/+', 'VRT:///') AS gdal_path
           FROM 'https://data.source.coop/tge-labs/aef/v1/annual/aef_index.parquet'
          WHERE year = {year}
      ) TO {outfile}
           WITH (FORMAT 'parquet', COMPRESSION 'zstd');",
    .con = con
  )

  DBI::dbExecute(
    con,
    q
  )
  return(outfile)
}

#' Query AEF index by spatial extent and year
#'
#' @param x Either a numeric vector of length 4 specifying a bounding box
#' (xmin, ymin, xmax, ymax), or a WKT string or WKB blob specifying a geometry
#' to filter by spatial intersection.
#' @param aef_parquet Character. Path to the AEF index parquet file.
#' @param year Numeric. Year to filter by.
#' @return A tibble with AEF index records that intersect the spatial extent.
#' @details
#' This function supports two modes of spatial filtering:
#' - Bounding box: Pass a numeric vector of length 4 for faster queries
#' - Geometry: Pass WKT string or WKB blob for precise spatial intersection
#'
#' When using geometry filtering, the function performs true spatial intersection
#' rather than just bounding box overlap, which is more accurate but slightly slower.
aef_index_query <- function(
  x,
  aef_parquet,
  year
) {
  # Validate inputs
  assertthat::assert_that(inherits(aef_parquet, "character"))
  assertthat::assert_that(
    inherits(x, "numeric") ||
      inherits(x, "character") ||
      inherits(x, "blob") ||
      inherits(x, "list")
  )
  if (inherits(x, "numeric")) {
    assertthat::assert_that(
      length(x) == 4,
      msg = "Bounding box must be a numeric vector of length 4 (xmin, ymin, xmax, ymax)"
    )
  }
  assertthat::assert_that(inherits(year, "integer"))

  con <- DBI::dbConnect(duckdb::duckdb(), dbdir = ":memory:")
  on.exit(DBI::dbDisconnect(con, shutdown = TRUE))

  DBI::dbExecute(
    con,
    "INSTALL spatial;
    LOAD spatial;"
  )

  # Build spatial filter based on input type
  if (inherits(x, "numeric")) {
    # Fast bounding box query
    bbox <- x
    spatial_filter <- DBI::SQL(glue::glue(
      "ST_Intersects(
        geom,
        ST_MakeEnvelope({bbox[1]}, {bbox[2]}, {bbox[3]}, {bbox[4]})
      )"
    ))
  } else {
    # Precise geometry intersection query — write WKB to temp table
    if (inherits(x, "blob") || inherits(x, "list")) {
      # Convert WKB to WKT
      x <- gdalraster::g_wk2wk(x)
    }
    spatial_filter <- DBI::SQL(glue::glue_sql(
      "ST_Intersects(
        geom,
        ST_GeomFromText({x})
      )",
      .con = con
    ))
  }

  sp_query <- glue::glue_sql(
    "
  SELECT * EXCLUDE(geom),
  ST_AsWKB(geom) AS geometry
  FROM {aef_parquet}
  WHERE {spatial_filter}
    AND year = {year}",
    .con = con
  )

  df <- DBI::dbGetQuery(con, sp_query)
  df$geometry <- blob::as_blob(df$geometry)
  tibble::as_tibble(df)
}
