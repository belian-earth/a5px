test_that("a5px_concurrency() reports, sets and restores like options()", {
  withr::local_options(a5px.cpu_workers = NULL, a5px.io_concurrency = NULL)
  withr::local_envvar(A5PX_CPU_WORKERS = "", A5PX_IO_CONCURRENCY = "")
  now <- a5px_concurrency()
  expect_type(now$cpu_workers, "integer")
  expect_identical(now$io_concurrency, 32L)

  expect_invisible(old <- a5px_concurrency(cpu_workers = 2, io_concurrency = 64))
  expect_s3_class(old, "a5px_concurrency")
  expect_null(old$cpu_workers)
  expect_identical(a5px_concurrency(), list(cpu_workers = 2L, io_concurrency = 64L))
  expect_output(print(a5px_concurrency(io_concurrency = 8)), "io_concurrency: 64")
  expect_identical(a5px_concurrency()$io_concurrency, 8L)

  # passing the earlier result back restores the unset defaults
  a5px_concurrency(old)
  expect_null(getOption("a5px.cpu_workers"))
  expect_identical(a5px_concurrency(), now)
})

test_that("with() scopes a setting to one expression, even when it fails", {
  withr::local_options(a5px.cpu_workers = 3L, a5px.io_concurrency = NULL)
  expect_identical(with(a5px_concurrency(cpu_workers = 1), a5px_concurrency()$cpu_workers), 1L)
  expect_identical(a5px_concurrency()$cpu_workers, 3L)
  expect_error(with(a5px_concurrency(cpu_workers = 1), stop("boom")), "boom")
  expect_identical(getOption("a5px.cpu_workers"), 3L)
})

test_that("an invalid setting errors and changes nothing", {
  withr::local_options(a5px.cpu_workers = 3L, a5px.io_concurrency = 5L)
  expect_error(a5px_concurrency(cpu_workers = 2, io_concurrency = 0), "io_concurrency")
  expect_error(a5px_concurrency(cpu_workers = NA), "cpu_workers")
  expect_error(a5px_concurrency(cpu_workers = 1:2), "cpu_workers")
  expect_identical(a5px_concurrency(), list(cpu_workers = 3L, io_concurrency = 5L))
})
