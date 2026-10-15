//! Shared tokio runtime.
//!
//! Every extendr entry point used to build a fresh `tokio::runtime::Runtime`
//! per call -- a few tens of milliseconds of overhead and an unbounded
//! thread-pool churn over many calls. We now build a single multi-thread
//! runtime once on first use and reuse it for the lifetime of the package.
//! `worker_threads` is set to the number of CPUs since the heavy CPU work
//! happens in `spawn_blocking` (separate pool) anyway -- the runtime threads
//! are mostly driving I/O futures that yield.
//!
//! The runtime's threads belong to the process that built it. A child
//! forked after a read (`parallel::mcparallel()`, `mclapply()`) inherits
//! the runtime but none of its threads, so it would wait forever on the
//! first read. Each process therefore keeps its own runtime: a child builds
//! a fresh one, and the inherited one is leaked, since dropping it would
//! try to join threads that do not exist.

use std::sync::Mutex;
use tokio::runtime::Runtime;

use crate::error::{A5CogError, Result};

/// The runtime and the process it was built in.
static RUNTIME: Mutex<Option<(u32, &'static Runtime)>> = Mutex::new(None);

pub(crate) fn shared_runtime() -> Result<&'static Runtime> {
    let pid = std::process::id();
    let mut slot = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((owner, rt)) = *slot {
        if owner == pid {
            return Ok(rt);
        }
    }
    let rt = Runtime::new()
        .map_err(|e| A5CogError::Internal(format!("tokio runtime build: {e}")))?;
    let rt: &'static Runtime = Box::leak(Box::new(rt));
    *slot = Some((pid, rt));
    Ok(rt)
}
