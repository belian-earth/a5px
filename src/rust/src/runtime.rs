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

extern "C" {
    fn R_ToplevelExec(fun: unsafe extern "C" fn(*mut std::ffi::c_void), data: *mut std::ffi::c_void) -> std::ffi::c_int;
    fn R_CheckUserInterrupt();
}

unsafe extern "C" fn check_interrupt(_: *mut std::ffi::c_void) {
    R_CheckUserInterrupt();
}

/// Whether the user has asked R to interrupt (Ctrl-C, or Esc in an IDE).
/// `R_CheckUserInterrupt()` jumps out when an interrupt is pending;
/// `R_ToplevelExec()` catches the jump and reports it as `FALSE`. Must run
/// on R's main thread.
fn interrupt_pending() -> bool {
    unsafe { R_ToplevelExec(check_interrupt, std::ptr::null_mut()) == 0 }
}

/// How often a running read checks for an interrupt.
const INTERRUPT_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Run a read to completion on the shared runtime, stopping it with
/// [`A5CogError::Interrupted`] when the user interrupts R. `block_on` polls
/// the read on the calling thread, R's main thread, so the timer branch
/// may call into R. Dropping the read closes its fetch queue; the CPU
/// workers finish the block in hand and exit.
pub(crate) fn block_on_interruptible<T>(fut: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    let rt = shared_runtime()?;
    rt.block_on(async {
        tokio::pin!(fut);
        let mut tick = tokio::time::interval(INTERRUPT_POLL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                r = &mut fut => return r,
                _ = tick.tick() => {
                    if interrupt_pending() {
                        return Err(A5CogError::Interrupted);
                    }
                }
            }
        }
    })
}

/// Hand memory freed by a read back to the operating system; the R
/// wrappers call it once a read's output has been copied into R. glibc keeps
/// freed heap in its per-thread arenas, so a long-lived R session that
/// makes many reads (a worker reading tile after tile) grew from 290 to
/// 895 MB over nine reads; `malloc_trim` returns what the arenas no longer
/// use. Elsewhere the allocator returns memory itself.
pub(crate) fn release_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}
