//! Chunked metadata fetch cache.
//!
//! async-tiff's `ReadaheadMetadataCache` caches an exponentially growing
//! prefix of the file, so a TIFF whose IFD sits at the end (GDAL `Create`
//! output, not a COG) makes it read the whole file to parse the header: a
//! 925 MB local file cost 0.6-1.1 s per call. This cache fetches aligned
//! `CHUNK`-byte pieces on demand and keeps them for the life of the open,
//! so a COG's header costs one chunk and a trailing IFD a couple more.

use std::collections::HashMap;
use std::ops::Range;

use async_tiff::error::AsyncTiffResult;
use async_tiff::metadata::MetadataFetch;
use async_tiff::reader::AsyncFileReader;
use bytes::{Bytes, BytesMut};
use tokio::sync::Mutex;

const CHUNK: u64 = 256 * 1024;

#[derive(Debug)]
pub(crate) struct ChunkedMetadataCache<F: AsyncFileReader> {
    inner: F,
    chunks: Mutex<HashMap<u64, Bytes>>,
}

impl<F: AsyncFileReader> ChunkedMetadataCache<F> {
    pub(crate) fn new(inner: F) -> Self {
        Self { inner, chunks: Mutex::new(HashMap::new()) }
    }
}

#[async_trait::async_trait]
impl<F: AsyncFileReader + Send + Sync + 'static> MetadataFetch for ChunkedMetadataCache<F> {
    async fn fetch(&self, range: Range<u64>) -> AsyncTiffResult<Bytes> {
        if range.is_empty() {
            return Ok(Bytes::new());
        }
        let first = range.start / CHUNK;
        let last = (range.end - 1) / CHUNK;
        let mut chunks = self.chunks.lock().await;
        for c in first..=last {
            let base = c * CHUNK;
            // bytes of this chunk the request needs
            let need = (range.end.min(base + CHUNK) - base) as usize;
            if chunks.get(&c).map(|b| b.len() >= need).unwrap_or(false) {
                continue;
            }
            // A full chunk can run past the end of the file, which stores
            // reject; then fetch only up to the requested end and keep
            // that (partial) chunk.
            let b = match self.inner.get_bytes(base..base + CHUNK).await {
                Ok(b) if b.len() >= need => b,
                _ => self.inner.get_bytes(base..base + need as u64).await?,
            };
            chunks.insert(c, b);
        }
        if first == last {
            let b = &chunks[&first];
            let s = (range.start - first * CHUNK) as usize;
            let e = (range.end - first * CHUNK) as usize;
            if e > b.len() {
                return Err(async_tiff::error::AsyncTiffError::General(format!(
                    "metadata range {}..{} beyond end of file",
                    range.start, range.end
                )));
            }
            return Ok(b.slice(s..e));
        }
        let mut out = BytesMut::with_capacity((range.end - range.start) as usize);
        for c in first..=last {
            let b = &chunks[&c];
            let lo = (range.start.max(c * CHUNK) - c * CHUNK) as usize;
            let hi = (range.end.min((c + 1) * CHUNK) - c * CHUNK) as usize;
            if hi > b.len() {
                return Err(async_tiff::error::AsyncTiffError::General(format!(
                    "metadata range {}..{} beyond end of file",
                    range.start, range.end
                )));
            }
            out.extend_from_slice(&b[lo..hi]);
        }
        Ok(out.freeze())
    }
}
