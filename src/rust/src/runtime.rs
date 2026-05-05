//! Shared tokio runtime.
//!
//! Every extendr entry point used to build a fresh `tokio::runtime::Runtime`
//! per call -- a few tens of milliseconds of overhead and an unbounded
//! thread-pool churn over many calls. We now build a single multi-thread
//! runtime once on first use and reuse it for the lifetime of the package.
//! `worker_threads` is set to the number of CPUs since the heavy CPU work
//! happens in `spawn_blocking` (separate pool) anyway -- the runtime threads
//! are mostly driving I/O futures that yield.

use std::sync::OnceLock;
use tokio::runtime::Runtime;

use crate::error::{A5CogError, Result};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub(crate) fn shared_runtime() -> Result<&'static Runtime> {
    if let Some(rt) = RUNTIME.get() {
        return Ok(rt);
    }
    // Race on init is fine: the loser of `set` discards its Runtime via Drop.
    let rt = Runtime::new()
        .map_err(|e| A5CogError::Internal(format!("tokio runtime build: {e}")))?;
    Ok(RUNTIME.get_or_init(|| rt))
}
