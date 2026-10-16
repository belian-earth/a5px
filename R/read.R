#' Read a (Cloud-Optimised) GeoTIFF as A5-cell aggregated values
#'
#' Streams a GeoTIFF or COG, projects each pixel centre to WGS 84, indexes it
#' to an A5 cell at the requested resolution, and aggregates per-band values
#' into a tibble keyed by cell.
#'
#' The reader runs in Rust on top of `async-tiff` and `object_store`, never
#' loading the full raster into memory. CRS reprojection uses pure-Rust
#' `proj4rs`. Pixel sampling is forward (pixel-driven): each pixel contributes
#' its value to exactly one A5 cell, determined by the lon/lat of its centre.
#'
#' Three sampling modes are available:
#'
#' Multi-stat output naming: with `stat = c("mean", "max")` the wide
#' (per-band) tibble columns are `<band>_mean`, `<band>_max`. Single
#' underscore separator. Same convention applies in
#' [a5_read_raster_arrow()] and [a5_raster_to_parquet()] when
#' `as_vector = FALSE`.
#'
#' - `"forward"` (default): for each pixel, push its value into the cell
#'   containing its centroid. Good when cells are larger than (or
#'   comparable to) the pixels and you want a real aggregation. Equivalent
#'   to GDAL's `gdal raster zonal-stats --pixels=default`.
#' - `"overlay"`: each pixel contributes to every cell it overlaps,
#'   weighted by the overlapped fraction of its area (approximated by
#'   sub-pixel supersampling; see `subsamples`). More accurate than
#'   `"forward"` at cell/pixel boundaries, at some extra cost for pixels
#'   that straddle a cell edge. Under overlay, `"mean"` is the
#'   area-weighted mean, `"sum"` is mass-preserving (a pixel's value is
#'   split across the cells covering it, so totals such as population
#'   counts are conserved), and `"count"` is the effective (fractional)
#'   number of contributing pixels. `"var"` / `"sd"` use frequency
#'   weights with divisor `sum(w) - 1`.
#' - `"centroid"`: for each cell intersecting the raster, sample the pixel
#'   that contains the cell's centroid. Single value per cell, no
#'   aggregation. Use this when the cell size is comparable to or smaller
#'   than the pixel size — the forward path leaves gaps in that regime
#'   because each pixel only contributes to one cell.
#'
#' @param src Paths or URLs of one or more GeoTIFFs / COGs. Supported
#'   schemes: local path (no scheme), `file://`, `http(s)://`, `s3://`,
#'   `gs://`, `az://`. See the "Remote sources" section of [a5px-package]
#'   for how cloud clients are configured. Several sources are read in one
#'   pass: their tiles share `io_concurrency` and `cpu_workers`. `separate`
#'   sets how they combine; names of `src` label the sources.
#' @param store_opts Named character vector or named list of object store
#'   options for a remote `src`, applied after the environment defaults so
#'   an explicit option always wins. Keys are the `object_store` config
#'   keys for the provider, e.g. `aws_region`, `aws_skip_signature`,
#'   `aws_access_key_id`, `aws_secret_access_key`, `aws_session_token`,
#'   `aws_endpoint`, `aws_virtual_hosted_style_request`, the `google_*` /
#'   `azure_*` equivalents, and for plain HTTP(S) the client keys such as
#'   `timeout` or `allow_http`. Unknown keys, and any key given with a
#'   local path, are errors. Inspect the result with [a5px_store_config()].
#' @param separate How several sources combine.
#'   - `FALSE` (default): the sources are pieces of one raster, such as
#'     adjacent tiles of a mosaic or captures of one area on different
#'     dates. They must have the same bands (count and names) and the same
#'     ground pixel size. Every valid pixel of every source enters its
#'     cell's statistics once, so cells on a seam between sources combine
#'     exactly. Where sources overlap, every capture counts: `mean`, `min`
#'     and `max` are over all observations, and `sum` and `count` include
#'     the overlap once per source, so `count` measures how many
#'     observations cover each cell. This differs from GDAL, where the last
#'     source wins. Each band must also be stored with the same scale and
#'     offset in every source, unless `scoff` or `dequant` decodes the
#'     values first, and with `use_overviews` all sources read one overview
#'     pixel size: the coarsest suitable overview they all have, else full
#'     resolution.
#'   - `TRUE`: each source adds its bands, as `gdalbuildvrt -separate`.
#'     Sources need not share a grid, CRS or pixel size. A single-band
#'     source is named by its label and a multi-band source's bands are
#'     `<label>_<band>`; the label is the source's name in `src`, else its
#'     file name without extension. `bands` selects from this combined
#'     list.
#'
#'   Single-band files without band descriptions all report the band name
#'   `band_01`, so `separate = FALSE` cannot tell red, green and blue files
#'   apart and pools them into one band; set `separate = TRUE` for files
#'   that hold different bands.
#' @param composite How several sources with the same bands combine.
#'   `NULL` (default) pools their pixels, as described under `separate`.
#'   Otherwise `stat` is computed for each source and cell, then reduced
#'   across the sources: `"mean"`, `"median"`, `"min"`, `"max"`, `"sum"`,
#'   `"count"`, `"var"`, `"sd"` or `"majority"`. Each source's value is
#'   weighted by its valid pixel count in the cell, so a source that covers
#'   only a sliver of a cell (at a scene edge, or around masked cloud)
#'   carries little weight; `"min"`, `"max"`, `"sum"` and `"count"` are
#'   unweighted. A `"majority"` value is weighted by the pixels of its
#'   class alone. `"median"` is the weighted median of the sources' values,
#'   and `"var"` and `"sd"` measure the spread between the sources' values:
#'   they omit the spread within each source, which only `composite = NULL`
#'   with `stat = "var"` includes. `"count"` gives the number of sources with
#'   valid data in the cell, such as the number of cloud-free captures, and
#'   `"sum"` and `"count"` are 0 where no source has valid data. With
#'   `stat = "mean"`, `composite = "mean"` equals the pooled mean. Cells are
#'   the shared grid, so the sources' pixel grids need not be aligned,
#'   though they must have the same pixel size. In centroid mode
#'   each source's sample has weight 1. Not available with
#'   `separate = TRUE`, `"fractions"` or `"npix"`.
#' @param resolution Integer scalar A5 resolution (0--29). Resolution 30 is
#'   refused for now: A5 returns resolution-29 cells for some points at 30.
#' @param stat Aggregation. One of `"mean"`, `"sum"`, `"count"`, `"min"`,
#'   `"max"`, `"var"`, `"sd"`, `"majority"`, `"fractions"`, `"npix"`, or any
#'   non-duplicated subset of those (except `"fractions"`, which must be
#'   alone) for a one-pass multi-stat read. Default `"mean"`. `"var"` /
#'   `"sd"` use the sample formula (divisor n - 1) computed via Welford's
#'   online algorithm in the streaming aggregator, matching [stats::var()] /
#'   [stats::sd()]; cells covered by a single pixel return `NA`.
#'
#'   `"npix"` is per cell rather than per band: a single `npix` column with
#'   the number of source pixels that had at least one valid band (under
#'   `mode = "overlay"`, their summed pixel-area weight). When validity is
#'   the same across bands, as with a dataset-wide nodata sentinel, it
#'   equals every `<band>_count` column, so `stat = c("mean", "npix")`
#'   carries the weights for a later merge in one column instead of one
#'   per band. Not available in `mode = "centroid"`.
#'
#'   `"majority"` and `"fractions"` are categorical: they treat the raw
#'   integer codes as class labels, and require an integer source of 16
#'   bits or fewer (like `dequant`, with which they cannot be combined).
#'   `"majority"` returns the class with the greatest total weight in each
#'   cell (pixel count under `mode = "forward"`, overlap area under
#'   `mode = "overlay"`), ties broken toward the smallest class code.
#'   `"fractions"` returns, per band, a list-column of named numeric
#'   vectors: each cell's per-class share of its total valid weight
#'   (shares sum to 1; names are the class codes). `"fractions"` is only
#'   available in `a5_read_raster()` with `as_vector = FALSE`.
#' @param bands Bands to read. One of:
#'   - `NULL` (default): read every band.
#'   - integer / numeric vector: 1-based band indices to read.
#'   - character vector: band names matched against the GDAL `DESCRIPTION`
#'     tag (falling back to `band_NN` when descriptions are absent).
#' @param bbox Optional spatial subset, as a numeric `c(xmin, ymin, xmax,
#'   ymax)` in WGS 84 lon/lat. When
#'   supplied, only tiles overlapping the bbox (in raster CRS) are fetched
#'   from the COG, and pixels outside the bbox are skipped. The bbox is
#'   half-open on its `xmax` / `ymax` edges, except at 180 and 90, so bboxes
#'   that share an edge take a pixel centred on it once. For a box across
#'   the antimeridian give `xmin > xmax`, such as `c(179, -1, -179, 1)`.
#'   Pixel longitudes are wrapped to -180 to 180 before the test, so a
#'   raster stored in longitudes 0 to 360 meets bboxes west of 0, and a
#'   pixel centred on the antimeridian belongs to boxes starting at -180.
#'   `NULL` (default) reads the whole raster.
#' @param bbox_align How `bbox` selects pixels. `"pixel"` (default) keeps
#'   every pixel whose centre lies in the bbox. `"block"` keeps every
#'   internal COG block (tile) whose origin pixel centre lies in the bbox,
#'   whole, and skips the per-pixel test; the bbox is half-open on its
#'   `xmax` / `ymax` edges. Under `"block"` each block of the raster belongs
#'   to exactly one member of any partition of bboxes, so a caller that
#'   chunks a large read to bound memory fetches every block once instead
#'   of re-fetching the blocks straddling chunk edges, and per-cell partial
#'   sums and counts from the chunks add up exactly. The trade is
#'   block-granular chunk edges. When overviews are in use the block grid is
#'   that of the overview level read, which is the same for every chunk of
#'   a given `resolution` and `stat`. Requires `bbox`; not applicable to
#'   `mode = "centroid"`. See [a5_raster_info()] for the block grid.
#' @param aoi Optional area of interest: a polygon or multipolygon in WGS 84
#'   as anything [a5R::a5_polygon_to_cells()] accepts (a `wk` geometry,
#'   `sf` / `sfc`, WKT, a terra `SpatVector`, or a lon/lat matrix). The AOI
#'   is converted to the set of A5 cells at `resolution` selected by
#'   `containment`, and only those cells appear in the output. Selection is
#'   cell-level: an included cell receives the statistics of **all** its
#'   valid pixels, including any outside the polygon, and an excluded cell
#'   receives nothing. Hole interiors are excluded, and several features
#'   form one AOI (the union of their cells). Combine with `bbox` to chunk a
#'   large AOI; without `bbox`, tiles are selected by the AOI's bounding box
#'   padded by three cell edges, so a very irregular AOI still reads its
#'   whole envelope. In `mode = "centroid"` the AOI cells are sampled
#'   directly. Polygon edges are great circles, as in a5R, so a
#'   [wk::rct()] bulges poleward rather than following the parallels, and
#'   one 180 degrees wide or more is an error unless it covers the globe.
#' @param containment How `aoi` selects cells, as in
#'   [a5R::a5_polygon_to_cells()]: `"centre"` (default) keeps cells whose
#'   centre lies inside the polygon; `"overlapping"` also keeps every cell
#'   the polygon boundary touches, for gap-free coverage. Only used with
#'   `aoi`.
#' @param src_nodata Optional numeric scalar overriding the source nodata.
#'   Use this when the file's `TIFFTAG_GDAL_NODATA` tag is missing or wrong;
#'   it takes precedence over the metadata value when set. `NaN` and
#'   infinities are accepted. `NULL` (default) uses the file's tag.
#' @param mode Sampling mode. `"forward"` (default) aggregates pixels into
#'   cells by their centres; `"overlay"` aggregates with pixel-cell overlap
#'   weights; `"centroid"` samples one pixel per cell at the cell centroid.
#'   See Details.
#' @param subsamples Sub-point grid dimension per pixel for
#'   `mode = "overlay"`: each pixel straddling a cell boundary is split
#'   into `subsamples^2` sub-points whose per-cell counts give the overlap
#'   weights (interior pixels take a fast path and never pay this cost).
#'   `NULL` (default) auto-selects from the pixel/cell edge ratio so
#'   sub-point spacing is at most half the cell edge, clamped to `[2, 16]`.
#'   Larger values approximate exact area weighting more closely. Only
#'   used when `mode = "overlay"`; an error otherwise.
#' @param interp Resampling kernel for `mode = "centroid"`:
#'   `"nearest"` (default, the containing pixel), `"bilinear"` (2x2),
#'   `"bicubic"` (Keys 4x4), or `"lanczos"` (Lanczos-3, 6x6). Kernel
#'   weights are renormalised over valid pixels, so nodata holes and
#'   raster edges shrink the stencil instead of propagating `NA`; a cell
#'   whose whole stencil is invalid returns `NA`. With `dequant`, each
#'   stencil pixel is decoded before the kernel is applied (nonlinear
#'   decodes do not commute with interpolation). Use `"nearest"` for
#'   categorical rasters; the smooth kernels blend class codes into
#'   meaningless intermediates. Only used when `mode = "centroid"`; an
#'   error otherwise.
#' @param cpu_workers Number of CPU threads for decoding and indexing
#'   (each decoded block is indexed as row stripes across all workers).
#'   `NULL` (default) resolves from `getOption("a5px.cpu_workers")`, env
#'   `A5PX_CPU_WORKERS`, then the CPUs available to the R process, which
#'   respects container and cgroup CPU limits.
#' @param io_concurrency Maximum in-flight fetch tasks the producer issues
#'   to the I/O stage; each task carries one block, or a run of up to four
#'   adjacent blocks on files without a predictor. `NULL` (default) resolves from
#'   `getOption("a5px.io_concurrency")`, env `A5PX_IO_CONCURRENCY`, then
#'   `min(16, max(cpu_workers, 8))`. Bump this for cloud reads of multi-band
#'   embedding rasters where the network can absorb more parallelism than
#'   the CPU pool. See [a5px_set_concurrency()].
#' @param scoff Logical. If `TRUE`, apply the scale and offset the file
#'   declares for each band (`value * scale + offset`, from the GDAL band
#'   metadata reported by [a5_raster_info()]) per pixel before aggregation.
#'   A band that declares none takes the GDAL defaults, scale 1 and offset
#'   0, so `scoff = TRUE` on a file without these tags returns raw values.
#'   Works for every data type. nodata is matched against the raw value,
#'   before scaling. The decode is linear, so it commutes with the mean and
#'   overviews stay in use (see `use_overviews`). Cannot be combined with
#'   `dequant` or with `majority` / `fractions`; to chain a further decode,
#'   fold the scale into the function:
#'   `dequant = function(x) f(x * s + o)`. Default `FALSE` reads raw values,
#'   unlike `terra::rast()`, which applies scale and offset unless
#'   `raw = TRUE`.
#' @param dequant Optional per-pixel decode applied *before* aggregation: a
#'   vectorised R function mapping raw integer codes to decoded values, e.g.
#'   [dequant_aef] for Alpha Earth Foundations int8 embedding codes.
#'   Nonlinear decodes do not commute with aggregation, so quantized values
#'   must be decoded per pixel before the mean, not after; this argument
#'   does exactly that.
#'   The function is evaluated once over the full integer code domain to
#'   build a lookup table applied on the Rust side, which restricts
#'   `dequant` to sources with an integer data type of 16 bits or fewer
#'   (int8 / uint8 / int16 / uint16); other dtypes error at read time.
#'   nodata is matched against the raw code, before decoding, and a code
#'   the function maps to `NA` is skipped like nodata. `NULL` (default)
#'   reads raw values.
#' @param as_vector Logical. If `TRUE`, collapse the (selected) bands into
#'   list columns of fixed-length numeric vectors -- one per stat. Single
#'   stat returns a single column `value`; multi-stat returns
#'   `value_<stat>` columns (e.g. `value_mean`, `value_max`). Useful for
#'   embedding rasters. Default `FALSE` (one numeric column per band).
#' @param use_overviews Logical. When `TRUE` and `stat = "mean"`, read the
#'   coarsest COG overview that still oversamples the target A5 cell
#'   instead of the full-resolution image. For aggregations to cells much
#'   coarser than the source pixels this moves far fewer bytes and does far
#'   less per-pixel work for a near-identical mean. Ignored unless the stat is
#'   exactly `"mean"` (`sum`/`count`/`var`/`sd`/`min`/`max` are not preserved
#'   under decimation) and in `mode = "centroid"`. Set `FALSE` to always read
#'   full resolution. Requires the source to carry overviews; otherwise the
#'   full-resolution image is read regardless. The default is `TRUE` unless
#'   `dequant` is set: overviews are typically average-resampled, and an
#'   overview pixel that is a mean of quantized codes decodes incorrectly.
#'   Explicitly passing `TRUE` together with `dequant` warns and proceeds,
#'   for sources whose overviews were built with nearest or mode resampling.
#'
#' @returns A [tibble::tibble()] with columns:
#'   - `cell`: an [a5R::a5_cell] vector at `resolution`
#'   - one numeric column per band (named after the band's GDAL `DESCRIPTION`
#'     tag if present, else `band_01`, `band_02`, ...)
#'   - or, if `as_vector = TRUE`, a single list column `value` of length-N
#'     numeric vectors (N = number of bands)
#'
#' @details
#' **Scaling:** GDAL `Scale` / `Offset` tags are *not* applied by default;
#' returned values are in the raster's native domain. Set `scoff = TRUE` to
#' apply them per pixel before aggregation. [a5_raster_info()] reports the
#' values a file declares.
#'
#' **NoData:** a pixel's band value is skipped when it equals the
#' dataset-wide nodata value (GDAL's `TIFFTAG_GDAL_NODATA`, or
#' `src_nodata`), when it is `NaN`, whatever the nodata value, and when
#' `dequant` maps it to `NA`. GeoTIFF has no per-band nodata; bands read
#' from separate files with `separate = TRUE` each keep their file's.
#'
#' @export
#' @examples
#' \dontrun{
#'   path <- system.file("extdata", "example.tif", package = "a5px")
#'   tbl <- a5_read_raster(path, resolution = 14)
#'   tbl
#' }
a5_read_raster <- function(src,
                           resolution,
                           stat = "mean",
                           bands = NULL,
                           bbox = NULL,
                           bbox_align = c("pixel", "block"),
                           aoi = NULL,
                           containment = c("centre", "overlapping"),
                           src_nodata = NULL,
                           mode = c("forward", "overlay", "centroid"),
                           subsamples = NULL,
                           interp = c("nearest", "bilinear", "bicubic", "lanczos"),
                           cpu_workers = NULL,
                           io_concurrency = NULL,
                           scoff = FALSE,
                           dequant = NULL,
                           as_vector = FALSE,
                           use_overviews = is.null(dequant),
                           store_opts = NULL,
                           separate = FALSE,
                           composite = NULL) {
  store <- check_store_opts(store_opts)
  resolution <- vctrs::vec_cast(resolution, integer(), x_arg = "resolution")
  vctrs::vec_assert(resolution, size = 1L)
  check_resolution(resolution)
  stats <- check_stats(stat)
  srcs <- check_sources(src, separate, stats, composite)
  mode <- rlang::arg_match(mode)
  subsamples_v <- check_subsamples(subsamples, mode)
  interp <- rlang::arg_match(interp)
  if (interp != "nearest" && mode != "centroid") {
    cli::cli_abort(
      "{.arg interp} is only used when {.code mode = \"centroid\"}."
    )
  }
  cpu_workers <- if (is.null(cpu_workers)) resolve_cpu_workers()
                 else check_scalar_count(cpu_workers, "cpu_workers")
  io_concurrency <- if (is.null(io_concurrency)) resolve_io_concurrency(cpu_workers)
                    else check_scalar_count(io_concurrency, "io_concurrency")
  if (!is.logical(as_vector) || length(as_vector) != 1L || is.na(as_vector)) {
    cli::cli_abort("{.arg as_vector} must be a length-1 non-NA logical.")
  }
  band_sel <- parse_bands_arg(bands)
  bbox_v <- check_bbox(bbox)
  bbox_align_block <- check_bbox_align(bbox_align, bbox_v, mode)
  aoi_v <- check_aoi(aoi, resolution, containment)
  src_nodata_v <- check_src_nodata(src_nodata)
  dequant_v <- check_dequant(dequant, scoff)
  check_stat_context(stats, dequant, as_vector, fractions_ok = TRUE, scoff = scoff)
  warn_dequant_overviews(dequant, use_overviews)
  overview_target_m <- overview_target_metres(use_overviews, stats, resolution)

  if (mode == "centroid") {
    return(read_raster_centroid(
      srcs = srcs,
      resolution = resolution,
      bands_idx = band_sel$idx,
      bands_names = band_sel$names,
      bbox = if (length(bbox_v)) bbox_v else NULL,
      src_nodata = src_nodata_v,
      cpu_workers = cpu_workers,
      io_concurrency = io_concurrency,
      as_vector = as_vector,
      stats = stats,
      dequant_v = dequant_v,
      interp = interp,
      store = store,
      aoi_cells = aoi_v$cells
    ))
  }

  out <- rs_call(a5_read_raster_rs(
    src = srcs$src,
    src_labels = srcs$labels,
    separate = srcs$separate,
    composite = srcs$composite,
    store_keys = store$keys,
    store_values = store$values,
    resolution = resolution,
    stats = stats,
    bands_idx = band_sel$idx,
    bands_names = band_sel$names,
    bbox = bbox_v,
    src_nodata = src_nodata_v,
    cpu_workers = cpu_workers,
    io_concurrency = io_concurrency,
    overview_target_m = overview_target_m,
    dequant_lut = dequant_v$lut,
    dequant_min = dequant_v$min,
    scoff = dequant_v$scoff,
    overlay = identical(mode, "overlay"),
    subsamples = subsamples_v,
    cell_edge_m = cell_edge_metres(mode, resolution),
    bbox_align_block = bbox_align_block,
    tile_bbox = aoi_v$tile_bbox,
    aoi_covering = aoi_v$covering_stored
  ))

  cells <- out$cell

  if ("fractions" %in% stats) {
    # ragged CSR from Rust -> one list-column of named share vectors per band
    n <- length(cells)
    cols <- list(cell = cells)
    for (b in as.character(out$band_names)) {
      fr <- out$fractions[[b]]
      cls <- as.character(as.integer(fr$classes))
      shr <- as.numeric(fr$shares)
      off <- as.integer(fr$offsets) # length n + 1, starting at 0
      cols[[b]] <- lapply(seq_len(n), function(i) {
        if (off[i + 1L] > off[i]) {
          stats::setNames(shr[(off[i] + 1L):off[i + 1L]],
                          cls[(off[i] + 1L):off[i + 1L]])
        } else {
          stats::setNames(numeric(0), character(0))
        }
      })
    }
    return(tibble::tibble(!!!cols))
  }

  bands <- out$bands
  stats <- setdiff(stats, "npix")
  npix <- out$npix
  # Rust returns names via the named list itself, but be explicit
  if (length(stats) == 1L) {
    names(bands) <- as.character(out$band_names)
  } else if (length(stats) > 1L) {
    band_names <- as.character(out$band_names)
    # Rust iterates stat-major (outer = stat, inner = band), producing
    # [B0_s0, B1_s0, ..., B0_s1, B1_s1, ...]. outer(bands, stats) flattens
    # column-major to that exact order.
    names(bands) <- as.vector(outer(band_names, stats, paste, sep = "_"))
  }

  if (as_vector) {
    n <- length(cells)
    band_names <- as.character(out$band_names)
    n_bands <- length(band_names)
    cols <- list(cell = cells)
    for (s in stats) {
      keys <- if (length(stats) == 1L) band_names else paste(band_names, s, sep = "_")
      mat <- vapply(bands[keys], identity, numeric(n))
      if (n_bands == 1L) dim(mat) <- c(n, 1L)
      val <- lapply(seq_len(n), function(i) as.numeric(mat[i, ]))
      col_name <- if (length(stats) == 1L) "value" else paste0("value_", s)
      cols[[col_name]] <- val
    }
    if (!is.null(npix)) cols$npix <- as.numeric(npix)
    tibble::tibble(!!!cols)
  } else {
    if (!is.null(npix)) bands$npix <- as.numeric(npix)
    tibble::tibble(cell = cells, !!!bands)
  }
}

#' Centroid-mode read: enumerate cells covering the bbox via
#' [a5R::a5_polygon_to_cells()], then sample one pixel per cell.
#' @noRd
read_raster_centroid <- function(srcs, resolution, bands_idx, bands_names,
                                 bbox, src_nodata, cpu_workers,
                                 io_concurrency, as_vector, stats,
                                 dequant_v = list(lut = numeric(0), min = 0, scoff = FALSE),
                                 interp = "nearest",
                                 store = check_store_opts(NULL),
                                 aoi_cells = NULL) {
  warn_centroid_stat(stats)
  cells <- centroid_cells(srcs$src, resolution, bbox, store, aoi_cells)
  out <- rs_call(a5_sample_at_cells_rs(
    src = srcs$src,
    src_labels = srcs$labels,
    separate = srcs$separate,
    composite = srcs$composite,
    store_keys = store$keys,
    store_values = store$values,
    cells = cells,
    bands_idx = bands_idx,
    bands_names = bands_names,
    src_nodata = src_nodata,
    cpu_workers = cpu_workers,
    io_concurrency = io_concurrency,
    dequant_lut = dequant_v$lut,
    dequant_min = dequant_v$min,
    scoff = dequant_v$scoff,
    interp = interp
  ))
  cells_out <- out$cell
  bands <- out$bands
  names(bands) <- as.character(out$band_names)

  if (as_vector) {
    n <- length(cells_out)
    n_bands <- length(bands)
    mat <- vapply(bands, identity, numeric(n))
    if (n_bands == 1L) dim(mat) <- c(n, 1L)
    value <- lapply(seq_len(n), function(i) as.numeric(mat[i, ]))
    tibble::tibble(cell = cells_out, value = value)
  } else {
    tibble::tibble(cell = cells_out, !!!bands)
  }
}

#' Enumerate the uniform grid of A5 cells sampled in centroid mode: the
#' cells centred on each source, within `bbox` (flat boxes from
#' `check_bbox()`, or NULL) and the AOI covering `aoi_cells` when given.
#' Candidates are taken per source from its footprint (padded in Rust past
#' the bulge of its outline, and split at the antimeridian), so sources far
#' apart, a raster across the antimeridian or an AOI larger than the raster
#' never enumerate cells off the raster.
#' @noRd
centroid_cells <- function(src, resolution, bbox, store = check_store_opts(NULL),
                           aoi_cells = NULL, call = rlang::caller_env()) {
  user <- if (is.null(bbox)) NULL else split_boxes(bbox)
  boxes <- if (length(src)) {
    unlist(lapply(src, function(s) {
      env <- split_boxes(as.numeric(rs_call(a5_raster_bbox_lonlat_rs(s, store$keys, store$values))))
      if (is.null(user)) env else intersect_boxes(user, env)
    }), recursive = FALSE)
  } else {
    user
  }
  boxes <- unique(boxes)
  if (is.null(boxes) && !is.null(aoi_cells)) {
    # no source or bbox to bound the AOI
    return(a5R::a5_uncompact(aoi_cells, resolution = resolution))
  }
  # Cells whose centre lies in each box, at the requested resolution so each
  # cell gets one centroid sample. The candidates come from a covering of
  # the box padded past the covering's residual bulge, intersected with the
  # AOI's before it is expanded to cells.
  cells <- lapply(boxes, function(b) {
    cover <- bbox_covering(pad_bbox(b, bbox_pad_deg), resolution)
    if (!is.null(aoi_cells)) cover <- a5R::a5_intersect(cover, aoi_cells)
    if (a5R::a5_count(cover) == 0) return(a5R::a5_cell(character()))
    cells_centred_in(a5R::a5_uncompact(cover, resolution = resolution), b)
  })
  cells <- vctrs::vec_unique(vctrs::vec_c(a5R::a5_cell(character()), !!!cells))
  if (length(cells) == 0L) {
    msg <- if (is.null(aoi_cells)) {
      "No A5 cells have centroids within the requested bbox at resolution {resolution}. Use a finer resolution or a larger bbox."
    } else {
      "No AOI cells have centroids on the raster within the requested bbox at resolution {resolution}."
    }
    cli::cli_abort(msg, call = call, class = c("a5px_error_geometry", "a5px_error_input", "a5px_error"))
  }
  cells
}

#' The cells of `cells` whose centre lies in a WGS 84 bbox. The max edges
#' are open, so bboxes that share an edge take each cell once, except at the
#' globe's edge (180 E, 90 N), as for pixels in the Rust readers.
#' @noRd
cells_centred_in <- function(cells, bbox) {
  ll <- a5R::a5_cell_to_lonlat(cells, as_dataframe = TRUE)
  below_max <- function(v, hi, globe) if (hi >= globe) v <= hi else v < hi
  cells[ll$lon >= bbox[1] & below_max(ll$lon, bbox[3], 180) &
          ll$lat >= bbox[2] & below_max(ll$lat, bbox[4], 90)]
}

#' Degrees of padding that hold every cell centred in a bbox inside
#' `bbox_covering()` of the padded bbox. A 1 degree chord along a parallel
#' strays at most 0.0011 degrees (about 120 m, at 45 degrees latitude) from
#' it; meridian edges are exact.
#' @noRd
bbox_pad_deg <- 0.01

#' `bbox` grown by `pad` degrees on each side, clamped to the globe.
#' @noRd
pad_bbox <- function(bbox, pad) {
  c(max(bbox[1] - pad, -180), max(bbox[2] - pad, -90),
    min(bbox[3] + pad, 180), min(bbox[4] + pad, 90))
}

#' The A5 covering of the cells whose centre lies in a WGS 84 bbox, to within
#' the bulge of its edges. a5R joins polygon vertices by great circles, so a
#' bare rectangle bulges poleward (a 20 degree edge along 10N peaks at
#' 10.15N), and one 180 degrees wide or more is an error unless it covers the
#' globe. The bbox is split into strips at most 90 degrees wide with vertices
#' every degree, so the edges follow the parallels to within `bbox_pad_deg`.
#' @noRd
bbox_covering <- function(bbox, resolution) {
  brk <- unique(c(seq(bbox[1], bbox[3], by = 90), bbox[3]))
  if (length(brk) < 2L) brk <- bbox[c(1L, 3L)]
  n <- length(brk)
  strips <- Map(
    function(x0, x1) bbox_strip(x0, bbox[2], x1, bbox[4]),
    brk[-n], brk[-1L]
  )
  a5R::a5_union(
    a5R::a5_polygon_to_cells(do.call(c, unname(strips)), resolution = resolution)
  )
}

#' A lon/lat rectangle as a polygon with a vertex at least every `step`
#' degrees along each edge.
#' @noRd
bbox_strip <- function(x0, y0, x1, y1, step = 1) {
  sx <- seq(x0, x1, length.out = max(2, ceiling((x1 - x0) / step) + 1))
  sy <- seq(y0, y1, length.out = max(2, ceiling((y1 - y0) / step) + 1))
  x <- c(sx, rep(x1, length(sy) - 1), rev(sx)[-1], rep(x0, length(sy) - 1))
  y <- c(rep(y0, length(sx)), sy[-1], rep(y1, length(sx) - 1), rev(sy)[-1])
  wk::wk_polygon(wk::xy(x, y))
}

#' Centroid mode takes one sample per cell, so `stat` is meaningless there;
#' warn once in a while if the user set it to anything but the default.
#' @noRd
warn_centroid_stat <- function(stats) {
  if (length(stats) > 1L || stats[1] != "mean") {
    cli::cli_warn(
      "{.code mode = \"centroid\"} returns one sample per cell; the {.arg stat} arg is ignored.",
      .frequency = "regularly", .frequency_id = "a5px-centroid-stat"
    )
  }
}
