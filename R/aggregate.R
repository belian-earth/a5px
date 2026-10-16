#' Aggregate A5-keyed values to a coarser resolution
#'
#' Lifts each row's A5 cell to its supercell at `resolution` via
#' [a5R::a5_cell_to_supercell()] and reduces
#' the selected value columns within each supercell group. Avoids re-reading
#' the source raster when you already have a high-resolution result and
#' want it summarised at a coarser scale.
#'
#' Each fine cell is assigned to the coarse cell that contains its centre.
#' The A5 index hierarchy ([a5R::a5_cell_to_parent()]) is not spatially
#' nested: for about half of cells one resolution apart, the index parent is
#' a neighbour of the containing cell, so grouping by it misplaces values.
#' The spatial rule is exact for centre placement but approximate for area,
#' because fine cells straddle coarse-cell boundaries. If you need
#' conservative aggregation (counts that must sum exactly), use a higher
#' input resolution so boundary error is small relative to cell area.
#' Locating centres runs in parallel under [a5R::a5_set_threads()].
#'
#' A cell's mean summarises its pixels, so `"mean"` is weighted by each
#' row's pixel count: the `npix` column when `x` has one (read with
#' `stat = c("mean", "npix")`), or the column named by `weights`. The
#' result then equals a direct read at the coarse resolution, up to cells
#' straddling coarse-cell boundaries. Without weights every row counts
#' once. The `npix` column itself is always summed. `"var"` and `"sd"` are
#' unweighted, over the rows' values.
#'
#' Both layouts produced by [a5_read_raster()] are supported:
#'   - wide: one numeric column per band (default).
#'   - list-of-numeric: a single fixed-length list column (`as_vector = TRUE`).
#'     Reduced element-wise.
#'
#' @param x A tibble or data.frame with exactly one column of class
#'   [a5R::a5_cell].
#' @param resolution Integer scalar. The coarser target resolution.
#'   Must be strictly less than the input cells' resolution, which must be
#'   the same for every row.
#' @param ... <[`tidy-select`][tidyselect::language]> columns to aggregate.
#'   Defaults to every non-cell numeric or list-of-numeric column.
#' @param stat Reducer. One of `"mean"` (default), `"sum"`, `"count"`,
#'   `"min"`, `"max"`, `"var"`, `"sd"`. `"count"` returns the number of
#'   non-`NA` source values per group. `"var"` / `"sd"` use the sample
#'   formula (divisor n - 1) to match R's [stats::var()] / [stats::sd()];
#'   single-row groups are returned as `NA`.
#' @param weights Name of a numeric column of row weights for `"mean"`, or
#'   `NULL` (default) to use `npix` when `x` has it and equal weights
#'   otherwise. Rows with weight 0 or `NA` do not count. The weights column
#'   is summed, like `npix`.
#'
#' @returns A tibble keyed by supercells at `resolution`, in ascending
#'   cell id, with the selected columns reduced. The cell column retains
#'   its original name and the [a5R::a5_cell] class.
#'
#' @export
#' @examples
#' \dontrun{
#'   fine <- a5_read_raster(path, resolution = 14L, stat = c("mean", "npix"))
#'   coarse <- a5_aggregate(fine, resolution = 10L)
#'
#'   # explicit column selection + sum reducer
#'   a5_aggregate(fine, 10L, B02, B03, stat = "sum")
#'
#'   # list-column input (as_vector = TRUE) is reduced element-wise
#'   v <- a5_read_raster(path, resolution = 14L, as_vector = TRUE)
#'   a5_aggregate(v, 10L)
#' }
a5_aggregate <- function(x,
                         resolution,
                         ...,
                         stat = c("mean", "sum", "count", "min", "max",
                                  "var", "sd"),
                         weights = NULL) {
  if (!is.data.frame(x)) {
    cli::cli_abort("{.arg x} must be a data.frame, not {.cls {class(x)[1]}}.")
  }
  stat <- rlang::arg_match(stat)
  resolution <- vctrs::vec_cast(resolution, integer(), x_arg = "resolution")
  vctrs::vec_assert(resolution, size = 1L)
  check_resolution(resolution)

  is_cell <- vapply(x, inherits, logical(1), what = "a5_cell")
  n_cell <- sum(is_cell)
  if (n_cell == 0L) {
    cli::cli_abort("{.arg x} has no column of class {.cls a5_cell}.")
  }
  if (n_cell > 1L) {
    cli::cli_abort(
      "{.arg x} has {n_cell} {.cls a5_cell} columns ({.val {names(x)[is_cell]}}); expected exactly one."
    )
  }
  cell_name <- names(x)[is_cell]
  cells <- x[[cell_name]]
  if (length(cells) == 0L) {
    cli::cli_abort("{.arg x} has zero rows; nothing to aggregate.")
  }

  res_in <- unique(a5R::a5_get_resolution(cells))
  res_in <- res_in[!is.na(res_in)]
  if (length(res_in) > 1L) {
    cli::cli_abort(
      "{.arg x} mixes cells of resolution {.val {sort(res_in)}}; aggregate one resolution at a time."
    )
  }
  if (length(res_in) == 0L || resolution >= res_in) {
    cli::cli_abort(
      "{.arg resolution} ({resolution}) must be strictly less than the input resolution ({res_in})."
    )
  }

  if (is.null(weights) && "npix" %in% names(x)) weights <- "npix"
  w <- aggregate_weights(x, weights, cell_name)
  sel_names <- aggregate_select_columns(x, cell_name, rlang::enquos(...))
  if (length(sel_names) == 0L) {
    cli::cli_abort(
      "No columns to aggregate. Pass column names via {.arg ...} or include numeric / list-of-numeric columns in {.arg x}."
    )
  }

  parents <- a5R::a5_cell_to_supercell(cells, resolution = resolution)
  gid <- vctrs::vec_group_id(parents)
  uniq_parents <- vctrs::vec_slice(parents, !duplicated(gid))

  out_cols <- list()
  out_cols[[cell_name]] <- uniq_parents
  for (nm in sel_names) {
    # pixel counts and the weights add up whatever the stat
    st <- if (nm == "npix" || identical(nm, weights)) "sum" else stat
    out_cols[[nm]] <- reduce_column(x[[nm]], gid, st, nm, w)
  }
  out <- tibble::tibble(!!!out_cols)
  # ascending cell id, as the readers return; zero-padded hex sorts as the id
  out[order(a5R::a5_u64_to_hex(uniq_parents)), , drop = FALSE]
}

#' Row weights for a weighted mean from the `weights` column, or `NULL`
#' (equal weights) without one. `NA` weights count as 0.
#' @noRd
aggregate_weights <- function(x, weights, cell_name, call = rlang::caller_env()) {
  if (is.null(weights)) {
    return(NULL)
  }
  if (!is.character(weights) || length(weights) != 1L || !weights %in% setdiff(names(x), cell_name)) {
    cli::cli_abort("{.arg weights} must name a column of {.arg x}.", call = call)
  }
  w <- x[[weights]]
  if (!is.numeric(w)) {
    cli::cli_abort("{.arg weights} column {.field {weights}} must be numeric.", call = call)
  }
  w <- as.numeric(w)
  if (any(w < 0, na.rm = TRUE)) {
    cli::cli_abort("{.arg weights} must not be negative.", call = call)
  }
  w[is.na(w)] <- 0
  w
}

#' Resolve the columns to aggregate.
#' @noRd
aggregate_select_columns <- function(x, cell_name, quos) {
  cand_names <- setdiff(names(x), cell_name)
  cand <- x[cand_names]

  if (length(quos) == 0L) {
    keep <- vapply(cand, is_aggregable_column, logical(1))
    return(cand_names[keep])
  }

  pos <- tidyselect::eval_select(rlang::expr(c(!!!quos)), data = cand)
  bad <- vapply(cand[pos], function(col) !is_aggregable_column(col), logical(1))
  if (any(bad)) {
    nm <- names(pos)[bad]
    cli::cli_abort(
      "Selected column{?s} {.field {nm}} {?is/are} not numeric or list-of-equal-length-numeric."
    )
  }
  names(pos)
}

#' Whether a column can be aggregated by a5_aggregate.
#' @noRd
is_aggregable_column <- function(col) {
  if (is.numeric(col)) return(TRUE)
  if (is.list(col) && !is.data.frame(col)) {
    if (length(col) == 0L) return(FALSE)
    first_len <- length(col[[1]])
    if (first_len == 0L) return(FALSE)
    return(all(vapply(col, function(e) is.numeric(e) && length(e) == first_len,
                      logical(1))))
  }
  FALSE
}

#' Group-reduce a single column. Dispatches on numeric vs list layout.
#' @noRd
reduce_column <- function(col, gid, stat, name, w = NULL) {
  if (is.list(col)) {
    mat <- tryCatch(
      do.call(rbind, lapply(col, as.numeric)),
      error = function(e) cli::cli_abort(
        "Column {.field {name}} could not be stacked to a matrix: {e$message}"
      )
    )
    reduce_matrix_groupwise(mat, gid, stat, w)
  } else {
    reduce_numeric_groupwise(as.numeric(col), gid, stat, w)
  }
}

#' Reduce a numeric vector by integer group id. Uses rowsum() for
#' sum/mean/count, the mean weighted by `w` when given; per-group apply for
#' min/max/var/sd.
#' @noRd
reduce_numeric_groupwise <- function(v, gid, stat, w = NULL) {
  if (stat %in% c("sum", "mean", "count")) {
    nz <- !is.na(v)
    sums <- as.numeric(rowsum(ifelse(nz, v, 0), gid, reorder = FALSE))
    counts <- as.numeric(rowsum(as.integer(nz), gid, reorder = FALSE))
    if (stat == "mean" && !is.null(w)) {
      wz <- ifelse(nz, w, 0)
      wsum <- as.numeric(rowsum(wz * ifelse(nz, v, 0), gid, reorder = FALSE))
      wtot <- as.numeric(rowsum(wz, gid, reorder = FALSE))
      return(ifelse(wtot > 0, wsum / wtot, NA_real_))
    }
    switch(stat,
      sum = sums,
      count = counts,
      mean = ifelse(counts > 0, sums / counts, NA_real_)
    )
  } else {
    fun <- per_group_scalar_fun(stat)
    locs <- vctrs::vec_group_loc(gid)$loc
    vapply(locs, function(idx) fun(v[idx]), numeric(1))
  }
}

#' Reduce an n x m matrix by integer group id, element-wise per column.
#' Returns a list of length n_groups, each a length-m numeric vector.
#' @noRd
reduce_matrix_groupwise <- function(mat, gid, stat, w = NULL) {
  n_bands <- ncol(mat)
  if (stat %in% c("sum", "mean", "count")) {
    nz <- !is.na(mat)
    sums   <- rowsum(ifelse(nz, mat, 0), gid, reorder = FALSE)
    counts <- rowsum(matrix(as.integer(nz), nrow = nrow(mat)), gid, reorder = FALSE)
    if (stat == "mean" && !is.null(w)) {
      wz <- ifelse(nz, w, 0)
      sums <- rowsum(wz * ifelse(nz, mat, 0), gid, reorder = FALSE)
      counts <- rowsum(wz, gid, reorder = FALSE)
    }
    out <- switch(stat,
      sum = sums,
      count = counts,
      mean = {
        m <- sums / counts
        m[counts == 0] <- NA_real_
        m
      }
    )
    storage.mode(out) <- "double"
    lapply(seq_len(nrow(out)), function(i) as.numeric(out[i, ]))
  } else {
    fun <- per_group_scalar_fun(stat)
    locs <- vctrs::vec_group_loc(gid)$loc
    lapply(locs, function(idx) {
      sub <- mat[idx, , drop = FALSE]
      vapply(seq_len(n_bands), function(b) fun(sub[, b]), numeric(1))
    })
  }
}

#' Reducer used for stats that don't fit the rowsum() shortcut
#' (min/max/var/sd). Returns a function (numeric) -> length-1 numeric.
#' @noRd
per_group_scalar_fun <- function(stat) {
  switch(stat,
    min = function(x) {
      x <- x[!is.na(x)]; if (length(x)) min(x) else NA_real_
    },
    max = function(x) {
      x <- x[!is.na(x)]; if (length(x)) max(x) else NA_real_
    },
    var = function(x) {
      x <- x[!is.na(x)]; if (length(x) >= 2L) stats::var(x) else NA_real_
    },
    sd  = function(x) {
      x <- x[!is.na(x)]; if (length(x) >= 2L) stats::sd(x)  else NA_real_
    }
  )
}
