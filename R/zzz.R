# nocov start
.onLoad <- function(libname, pkgname) {
  n <- getOption("a5px.threads")
  if (is.null(n)) {
    env <- Sys.getenv("A5PX_NUM_THREADS", unset = "")
    if (nzchar(env)) {
      n <- as.integer(env)
    }
  }
  if (!is.null(n) && !is.na(n) && n >= 1L) {
    a5px_set_threads_rs(as.integer(n))
  }
}
# nocov end
