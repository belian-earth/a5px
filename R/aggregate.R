#' Aggregate A5-keyed values to a coarser resolution
#'
#' Lifts each row's A5 cell to its parent at `to_resolution` via
#' [a5R::a5_cell_to_parent()] (the A5-native centroid hierarchy) and reduces
#' the selected value columns within each parent group. Avoids re-reading
#' the source raster when you already have a high-resolution result and
#' want it summarised at a coarser scale.
#'
#' Uses the centroid rule: each child cell is assigned to the parent whose
#' centroid contains it. This is exact for centroid placement but only
#' approximate for area, because A5 parent and child cells are not perfectly
#' nested. For embedding-style and distributional summaries this is a
#' non-issue; if you need conservative aggregation (counts that must sum
#' exactly), use a higher input resolution so boundary error is small
#' relative to cell area.
#'
#' Both layouts produced by [a5_read_raster()] are supported:
#'   - wide: one numeric column per band (default).
#'   - list-of-numeric: a single fixed-length list column (`as_vector = TRUE`).
#'     Reduced element-wise.
#'
#' @param x A tibble or data.frame with exactly one column of class
#'   [a5R::a5_cell].
#' @param to_resolution Integer scalar. The coarser target resolution.
#'   Must be strictly less than the input cells' resolution.
#' @param ... <[`tidy-select`][tidyselect::language]> columns to aggregate.
#'   Defaults to every non-cell numeric or list-of-numeric column.
#' @param stat Reducer. One of `"mean"` (default), `"sum"`, `"count"`,
#'   `"min"`, `"max"`, `"var"`, `"sd"`. `"count"` returns the number of
#'   non-`NA` source values per group. `"var"` / `"sd"` use the sample
#'   formula (divisor n - 1) to match R's [stats::var()] / [stats::sd()];
#'   single-row groups are returned as `NA`.
#'
#' @returns A tibble keyed by parent cells at `to_resolution`, with the
#'   selected columns reduced. The cell column retains its original name
#'   and the [a5R::a5_cell] class.
#'
#' @export
#' @examples
#' \dontrun{
#'   fine <- a5_read_raster(path, resolution = 14L)
#'   coarse <- a5_aggregate(fine, to_resolution = 10L)
#'
#'   # explicit column selection + sum reducer
#'   a5_aggregate(fine, 10L, B02, B03, stat = "sum")
#'
#'   # list-column input (as_vector = TRUE) is reduced element-wise
#'   v <- a5_read_raster(path, resolution = 14L, as_vector = TRUE)
#'   a5_aggregate(v, 10L)
#' }
a5_aggregate <- function(x,
                         to_resolution,
                         ...,
                         stat = c("mean", "sum", "count", "min", "max",
                                  "var", "sd")) {
  if (!is.data.frame(x)) {
    cli::cli_abort("{.arg x} must be a data.frame, not {.cls {class(x)[1]}}.")
  }
  stat <- rlang::arg_match(stat)
  to_resolution <- vctrs::vec_cast(to_resolution, integer(), x_arg = "to_resolution")
  vctrs::vec_assert(to_resolution, size = 1L)
  check_resolution(to_resolution)

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

  cur_res <- a5R::a5_get_resolution(cells[1])
  if (to_resolution >= cur_res) {
    cli::cli_abort(
      "{.arg to_resolution} ({to_resolution}) must be strictly less than the input resolution ({cur_res})."
    )
  }

  sel_names <- aggregate_select_columns(x, cell_name, rlang::enquos(...))
  if (length(sel_names) == 0L) {
    cli::cli_abort(
      "No columns to aggregate. Pass column names via {.arg ...} or include numeric / list-of-numeric columns in {.arg x}."
    )
  }

  parents <- a5R::a5_cell_to_parent(cells, resolution = to_resolution)
  gid <- vctrs::vec_group_id(parents)
  uniq_parents <- vctrs::vec_slice(parents, !duplicated(gid))

  out_cols <- list()
  out_cols[[cell_name]] <- uniq_parents
  for (nm in sel_names) {
    out_cols[[nm]] <- reduce_column(x[[nm]], gid, stat, nm)
  }
  tibble::tibble(!!!out_cols)
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
reduce_column <- function(col, gid, stat, name) {
  if (is.list(col)) {
    mat <- tryCatch(
      do.call(rbind, lapply(col, as.numeric)),
      error = function(e) cli::cli_abort(
        "Column {.field {name}} could not be stacked to a matrix: {e$message}"
      )
    )
    reduce_matrix_groupwise(mat, gid, stat)
  } else {
    reduce_numeric_groupwise(as.numeric(col), gid, stat)
  }
}

#' Reduce a numeric vector by integer group id.
#' Uses rowsum() for sum/mean/count; per-group apply for min/max.
#' @noRd
reduce_numeric_groupwise <- function(v, gid, stat) {
  if (stat %in% c("sum", "mean", "count")) {
    nz <- !is.na(v)
    sums <- as.numeric(rowsum(ifelse(nz, v, 0), gid, reorder = FALSE))
    counts <- as.numeric(rowsum(as.integer(nz), gid, reorder = FALSE))
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
reduce_matrix_groupwise <- function(mat, gid, stat) {
  n_bands <- ncol(mat)
  if (stat %in% c("sum", "mean", "count")) {
    nz <- !is.na(mat)
    sums   <- rowsum(ifelse(nz, mat, 0), gid, reorder = FALSE)
    counts <- rowsum(matrix(as.integer(nz), nrow = nrow(mat)), gid, reorder = FALSE)
    out <- switch(stat,
      sum = sums,
      count = counts,
      mean = {
        m <- sums / counts
        m[counts == 0L] <- NA_real_
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
