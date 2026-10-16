//! A deflate tile decoder that grows its buffer in few steps.
//!
//! async-tiff's decoder streams into an empty `Vec` with `read_to_end`,
//! which grows it in many small steps, copying the tile each time. Here the
//! buffer starts at four times the compressed size and doubles, and zlib
//! writes into it directly: with eight workers a float32 deflate read took
//! 12% less time, though one worker gained nothing, so the saving is in
//! allocator contention rather than the copies. (zstd, tried the same way
//! with the frame's recorded size, gained nothing.)

use async_tiff::decoder::{Decoder, DecoderRegistry};
use async_tiff::error::{AsyncTiffError, AsyncTiffResult};
use async_tiff::tags::{Compression, PhotometricInterpretation};
use bytes::Bytes;

/// async-tiff's decoders, with deflate replaced.
pub(crate) fn registry() -> DecoderRegistry {
    let mut r = DecoderRegistry::default();
    let m = r.as_mut();
    m.insert(Compression::Deflate, Box::new(SizedDeflate));
    m.insert(Compression::OldDeflate, Box::new(SizedDeflate));
    r
}

fn general(what: &str, e: impl std::fmt::Display) -> AsyncTiffError {
    AsyncTiffError::General(format!("{what}: {e}"))
}

#[derive(Debug)]
struct SizedDeflate;

impl Decoder for SizedDeflate {
    fn decode_tile(
        &self,
        buffer: Bytes,
        _photometric_interpretation: PhotometricInterpretation,
        _jpeg_tables: Option<&[u8]>,
        _samples_per_pixel: u16,
        _bits_per_sample: u16,
        _lerc_parameters: Option<&[u32]>,
    ) -> AsyncTiffResult<Vec<u8>> {
        inflate(&buffer)
    }
}

/// Decode a zlib stream (TIFF's deflate) into a growing buffer.
fn inflate(input: &[u8]) -> AsyncTiffResult<Vec<u8>> {
    use flate2::{Decompress, FlushDecompress, Status};
    let mut d = Decompress::new(true);
    let mut out: Vec<u8> = Vec::with_capacity((input.len() * 4).max(4096));
    loop {
        let (i0, o0) = (d.total_in(), d.total_out());
        let status = d
            .decompress_vec(&input[i0 as usize..], &mut out, FlushDecompress::Finish)
            .map_err(|e| general("deflate", e))?;
        if matches!(status, Status::StreamEnd) {
            return Ok(out);
        }
        if out.len() == out.capacity() {
            out.reserve(out.capacity());
        } else if d.total_in() as usize == input.len() || (d.total_in() == i0 && d.total_out() == o0) {
            return Err(general("deflate", "truncated stream"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn inflates_streams_larger_and_smaller_than_the_first_guess() {
        for data in [vec![7u8; 1 << 20], (0..5000u32).flat_map(|i| i.to_le_bytes()).collect()] {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&data).unwrap();
            let z = e.finish().unwrap();
            assert_eq!(inflate(&z).unwrap(), data);
            assert!(inflate(&z[..z.len() / 2]).is_err());
        }
    }
}
