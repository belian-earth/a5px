//! Direct Rust-side Parquet writer.
//!
//! Same schema as the R `a5_read_raster_arrow()` path (cell:uint64 +
//! value:FixedSizeList<float, n_bands>) but constructed in Rust from the
//! aggregator's flat cell-major buffer with no R intermediary. Saves the
//! Vec<f64> -> R numeric -> R list of vectors -> arrow Array round-trip and
//! the per-cell R object allocation.

use std::collections::HashMap;
use std::fs::File;
use std::sync::Arc;

use arrow_array::{
    ArrayRef, FixedSizeListArray, Float32Array, Float64Array, RecordBatch, UInt64Array,
};
use arrow_buffer::Buffer;
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;

use crate::error::{A5CogError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueType {
    Float64,
    Float32,
}

impl ValueType {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "float64" => Ok(Self::Float64),
            "float32" => Ok(Self::Float32),
            other => Err(A5CogError::Invalid(format!(
                "unknown value_type {other:?}; expected float32 or float64"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompressionChoice {
    Zstd,
    Snappy,
    None,
}

impl CompressionChoice {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "zstd" | "ZSTD" => Ok(Self::Zstd),
            "snappy" | "SNAPPY" => Ok(Self::Snappy),
            "none" | "uncompressed" => Ok(Self::None),
            other => Err(A5CogError::Invalid(format!(
                "unknown compression {other:?}; expected zstd | snappy | none"
            ))),
        }
    }

    fn to_parquet(self) -> Compression {
        match self {
            Self::Zstd => Compression::ZSTD(ZstdLevel::default()),
            Self::Snappy => Compression::SNAPPY,
            Self::None => Compression::UNCOMPRESSED,
        }
    }
}

/// Build the RecordBatch and write it to `dest` as a Parquet file.
///
/// `flat_values` is cell-major: row i band b at index `i * n_bands + b`.
pub(crate) fn write_arrow_parquet(
    dest: &str,
    cells: Vec<u64>,
    flat_values: Vec<f64>,
    n_bands: usize,
    band_names: &[String],
    resolution: i32,
    stat: &str,
    value_type: ValueType,
    compression: CompressionChoice,
) -> Result<()> {
    let n_cells = cells.len();
    if flat_values.len() != n_cells * n_bands {
        return Err(A5CogError::Invalid(format!(
            "flat_values len {} != cells {} * bands {}",
            flat_values.len(),
            n_cells,
            n_bands
        )));
    }

    // cell column: zero-copy from Vec<u64>
    let cell_arr = UInt64Array::from(cells);

    // value column: FixedSizeList<float, n_bands> with flat_values as the child
    let inner_array: ArrayRef = match value_type {
        ValueType::Float64 => {
            // zero-copy reuse of the Vec<f64>
            let buf = Buffer::from_vec(flat_values);
            let arr = Float64Array::new(buf.into(), None);
            Arc::new(arr) as ArrayRef
        }
        ValueType::Float32 => {
            // f64 -> f32: must copy / cast
            let casted: Vec<f32> = flat_values.into_iter().map(|v| v as f32).collect();
            let buf = Buffer::from_vec(casted);
            let arr = Float32Array::new(buf.into(), None);
            Arc::new(arr) as ArrayRef
        }
    };
    let item_field = Arc::new(Field::new(
        "item",
        inner_array.data_type().clone(),
        true,
    ));
    let value_arr = FixedSizeListArray::new(
        item_field.clone(),
        n_bands as i32,
        inner_array,
        None,
    );

    let schema = Schema::new(vec![
        Field::new("cell", DataType::UInt64, false),
        Field::new(
            "value",
            DataType::FixedSizeList(item_field, n_bands as i32),
            false,
        ),
    ])
    .with_metadata(file_metadata(band_names, resolution, stat));

    let batch = RecordBatch::try_new(
        Arc::new(schema.clone()),
        vec![Arc::new(cell_arr), Arc::new(value_arr)],
    )
    .map_err(|e| A5CogError::Invalid(format!("RecordBatch build: {e}")))?;

    let file = File::create(dest)?;
    let kv: Vec<KeyValue> = file_metadata(band_names, resolution, stat)
        .into_iter()
        .map(|(k, v)| KeyValue::new(k, v))
        .collect();
    let props = WriterProperties::builder()
        .set_compression(compression.to_parquet())
        .set_key_value_metadata(Some(kv))
        .build();
    let mut writer = ArrowWriter::try_new(file, Arc::new(schema), Some(props))
        .map_err(|e| A5CogError::Invalid(format!("ArrowWriter: {e}")))?;
    writer
        .write(&batch)
        .map_err(|e| A5CogError::Invalid(format!("write batch: {e}")))?;
    writer
        .close()
        .map_err(|e| A5CogError::Invalid(format!("writer close: {e}")))?;
    Ok(())
}

fn file_metadata(band_names: &[String], resolution: i32, stat: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("a5px_band_names".to_string(), band_names.join("\n"));
    m.insert("a5px_resolution".to_string(), resolution.to_string());
    m.insert("a5px_stat".to_string(), stat.to_string());
    m
}
