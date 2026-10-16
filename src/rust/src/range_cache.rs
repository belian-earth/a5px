//! Session cache of remote byte ranges.
//!
//! A read that is split into bbox chunks, or repeated, opens the same remote
//! files again and fetches the header and every block it touches each time:
//! over HTTPS a 1 km box read in 25 chunks took about 40 times as long as
//! one read. [`CachedReader`] keeps the byte ranges a remote source returns
//! in one cache shared by every read of the R session, so later reads take
//! header chunks and blocks from memory. Entries are keyed by source and
//! exact range, which repeats from read to read because block offsets come
//! from the file's header. The cache holds compressed bytes, evicts the least
//! recently used range past its budget, and is never consulted for local
//! files, which the operating system already caches.
//!
//! Remote files are taken to be immutable for the session; a source that
//! changes in place needs [`clear`].

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use async_tiff::error::AsyncTiffResult;
use async_tiff::reader::{AsyncFileReader, ObjectReader};
use bytes::Bytes;
use extendr_api::prelude::*;

/// Default budget in MiB, overridden by `A5PX_CACHE_MB` at first use, and
/// held to an eighth of a cgroup memory limit (a container or a systemd
/// scope), so a 2 GiB task caches at most 256 MiB.
const DEFAULT_BUDGET_MB: u64 = 512;

type Key = (Arc<str>, u64, u64);

struct RangeCache {
    map: HashMap<Key, (Bytes, u64)>,
    /// Recency order: last-use tick -> key.
    order: BTreeMap<u64, Key>,
    tick: u64,
    bytes: u64,
    budget: u64,
}

impl RangeCache {
    fn get(&mut self, key: &Key) -> Option<Bytes> {
        self.tick += 1;
        let tick = self.tick;
        let (b, t) = self.map.get_mut(key)?;
        let old = std::mem::replace(t, tick);
        let b = b.clone();
        if let Some(k) = self.order.remove(&old) {
            self.order.insert(tick, k);
        }
        Some(b)
    }

    fn insert(&mut self, key: Key, b: Bytes) {
        let n = b.len() as u64;
        if n == 0 || n > self.budget || self.map.contains_key(&key) {
            return;
        }
        self.tick += 1;
        self.order.insert(self.tick, key.clone());
        self.map.insert(key, (b, self.tick));
        self.bytes += n;
        self.evict();
    }

    fn evict(&mut self) {
        while self.bytes > self.budget {
            let Some((_, key)) = self.order.pop_first() else { break };
            if let Some((b, _)) = self.map.remove(&key) {
                self.bytes -= b.len() as u64;
            }
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.bytes = 0;
    }
}

static CACHE: LazyLock<Mutex<RangeCache>> = LazyLock::new(|| {
    let budget = match std::env::var("A5PX_CACHE_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(mb) => mb.saturating_mul(1 << 20),
        None => {
            let d = DEFAULT_BUDGET_MB << 20;
            cgroup_memory_limit().map_or(d, |lim| d.min(lim / 8))
        }
    };
    Mutex::new(RangeCache {
        map: HashMap::new(),
        order: BTreeMap::new(),
        tick: 0,
        bytes: 0,
        budget,
    })
});

/// The memory limit of this process's cgroup in bytes, the smallest set on
/// it or an ancestor (cgroup v2), or the v1 memory controller's; `None`
/// without one or off Linux.
fn cgroup_memory_limit() -> Option<u64> {
    let own = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let parse = |s: String| s.trim().parse::<u64>().ok();
    let mut best: Option<u64> = None;
    for line in own.lines() {
        let mut f = line.splitn(3, ':');
        let (_, ctrl, path) = (f.next()?, f.next()?, f.next()?);
        if ctrl.is_empty() {
            // v2: walk from this cgroup to the root
            let mut p = std::path::PathBuf::from("/sys/fs/cgroup").join(path.trim_start_matches('/'));
            loop {
                if let Some(v) = std::fs::read_to_string(p.join("memory.max")).ok().and_then(parse) {
                    best = Some(best.map_or(v, |b| b.min(v)));
                }
                if p == std::path::Path::new("/sys/fs/cgroup") || !p.pop() {
                    break;
                }
            }
        } else if ctrl.split(',').any(|c| c == "memory") {
            let p = std::path::PathBuf::from("/sys/fs/cgroup/memory")
                .join(path.trim_start_matches('/'))
                .join("memory.limit_in_bytes");
            // v1 reports "no limit" as a huge page-rounded value
            if let Some(v) = std::fs::read_to_string(p).ok().and_then(parse).filter(|&v| v < (1u64 << 60)) {
                best = Some(best.map_or(v, |b| b.min(v)));
            }
        }
    }
    best
}

/// CPUs this process may use: `std::thread::available_parallelism`, which
/// honours affinity masks and cgroup CPU quotas on Linux.
pub(crate) fn available_cpus() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);

fn cache() -> std::sync::MutexGuard<'static, RangeCache> {
    // a panic while holding the lock leaves the map consistent enough to
    // keep serving; recover rather than poison every later read
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Set the budget in bytes, evicting down to it; 0 disables the cache.
/// Returns the previous budget.
pub(crate) fn set_budget(bytes: u64) -> u64 {
    let mut c = cache();
    let prev = c.budget;
    c.budget = bytes;
    c.evict();
    prev
}

/// Drop every cached range and reset the hit and miss counts.
pub(crate) fn clear() {
    cache().clear();
    HITS.store(0, Ordering::Relaxed);
    MISSES.store(0, Ordering::Relaxed);
}

/// `(entries, bytes, budget, hits, misses)`.
pub(crate) fn stats() -> (u64, u64, u64, u64, u64) {
    let c = cache();
    (
        c.map.len() as u64,
        c.bytes,
        c.budget,
        HITS.load(Ordering::Relaxed),
        MISSES.load(Ordering::Relaxed),
    )
}

/// An [`ObjectReader`] whose ranges go through the session cache when it
/// has a key.
#[derive(Debug, Clone)]
pub(crate) struct CachedReader {
    inner: ObjectReader,
    key: Option<Arc<str>>,
}

impl CachedReader {
    /// `key` (see `store::cache_key`) is `None` for local files.
    pub(crate) fn new(inner: ObjectReader, key: Option<String>) -> Self {
        Self { inner, key: key.map(Arc::from) }
    }

    fn key_of(&self, range: &Range<u64>) -> Option<Key> {
        let k = self.key.as_ref()?;
        Some((Arc::clone(k), range.start, range.end))
    }
}

#[async_trait::async_trait]
impl AsyncFileReader for CachedReader {
    async fn get_bytes(&self, range: Range<u64>) -> AsyncTiffResult<Bytes> {
        let Some(key) = self.key_of(&range) else {
            return self.inner.get_bytes(range).await;
        };
        if let Some(b) = cache().get(&key) {
            HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(b);
        }
        MISSES.fetch_add(1, Ordering::Relaxed);
        let b = self.inner.get_bytes(range).await?;
        cache().insert(key, b.clone());
        Ok(b)
    }

    async fn get_byte_ranges(&self, ranges: Vec<Range<u64>>) -> AsyncTiffResult<Vec<Bytes>> {
        if self.key.is_none() {
            return self.inner.get_byte_ranges(ranges).await;
        }
        let keys: Vec<Key> = ranges.iter().filter_map(|r| self.key_of(r)).collect();
        let mut out: Vec<Option<Bytes>> = {
            let mut c = cache();
            keys.iter().map(|k| c.get(k)).collect()
        };
        let missing: Vec<usize> = (0..out.len()).filter(|&i| out[i].is_none()).collect();
        HITS.fetch_add((out.len() - missing.len()) as u64, Ordering::Relaxed);
        MISSES.fetch_add(missing.len() as u64, Ordering::Relaxed);
        if !missing.is_empty() {
            let fetched = self
                .inner
                .get_byte_ranges(missing.iter().map(|&i| ranges[i].clone()).collect())
                .await?;
            let mut c = cache();
            for (&i, b) in missing.iter().zip(fetched) {
                c.insert(keys[i].clone(), b.clone());
                out[i] = Some(b);
            }
        }
        Ok(out.into_iter().map(|b| b.expect("every range filled")).collect())
    }
}

// ---------------------------------------------------------------------------
// extendr bindings

/// Set the session range cache budget in MiB (0 disables it); returns the
/// previous budget in MiB.
/// @noRd
/// @keywords internal
#[extendr]
fn a5px_cache_set_rs(max_mb: f64) -> f64 {
    let bytes = (max_mb.max(0.0) * (1u64 << 20) as f64) as u64;
    set_budget(bytes) as f64 / (1u64 << 20) as f64
}

/// Drop every cached range.
/// @noRd
/// @keywords internal
#[extendr]
fn a5px_cache_clear_rs() {
    clear();
}

/// CPUs available to this process, for the default `cpu_workers`.
/// @noRd
/// @keywords internal
#[extendr]
fn a5px_available_cpus_rs() -> i32 {
    available_cpus() as i32
}

/// Return memory a finished read freed to the operating system.
/// @noRd
/// @keywords internal
#[extendr]
fn a5px_release_memory_rs() {
    crate::runtime::release_freed_memory();
}

/// Cache state: entries, size and budget in MiB, hits and misses.
/// @noRd
/// @keywords internal
#[extendr]
fn a5px_cache_info_rs() -> List {
    let (n, bytes, budget, hits, misses) = stats();
    let mb = |b: u64| b as f64 / (1u64 << 20) as f64;
    list!(
        entries = n as f64,
        size_mb = mb(bytes),
        max_mb = mb(budget),
        hits = hits as f64,
        misses = misses as f64
    )
}

extendr_module! {
    mod range_cache;
    fn a5px_cache_set_rs;
    fn a5px_cache_clear_rs;
    fn a5px_cache_info_rs;
    fn a5px_available_cpus_rs;
    fn a5px_release_memory_rs;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_of(budget: u64) -> RangeCache {
        RangeCache { map: HashMap::new(), order: BTreeMap::new(), tick: 0, bytes: 0, budget }
    }

    fn key(s: &str, a: u64) -> Key {
        (Arc::from(s), a, a + 10)
    }

    #[test]
    fn evicts_least_recently_used_past_budget() {
        let mut c = cache_of(25);
        c.insert(key("a", 0), Bytes::from(vec![0u8; 10]));
        c.insert(key("a", 10), Bytes::from(vec![1u8; 10]));
        // touch the first so the second is the oldest
        assert!(c.get(&key("a", 0)).is_some());
        c.insert(key("a", 20), Bytes::from(vec![2u8; 10]));
        assert_eq!(c.bytes, 20);
        assert!(c.get(&key("a", 10)).is_none());
        assert!(c.get(&key("a", 0)).is_some());
        assert!(c.get(&key("a", 20)).is_some());
    }

    #[test]
    fn skips_ranges_larger_than_the_budget_and_a_zero_budget() {
        let mut c = cache_of(5);
        c.insert(key("a", 0), Bytes::from(vec![0u8; 10]));
        assert!(c.map.is_empty());
        let mut z = cache_of(0);
        z.insert(key("a", 0), Bytes::from(vec![0u8; 1]));
        assert!(z.map.is_empty());
    }

    #[test]
    fn sources_do_not_share_entries() {
        let mut c = cache_of(100);
        c.insert(key("a", 0), Bytes::from_static(b"aaaaaaaaaa"));
        assert!(c.get(&key("b", 0)).is_none());
    }
}
