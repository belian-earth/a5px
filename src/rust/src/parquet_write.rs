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
/// Each entry of `flat_values` is one stat's cell-major buffer: index
/// `i * n_bands + b` is band `b` of cell `i`. With a single stat the schema is
/// `cell:uint64 + value:FixedSizeList<float, n_bands>` (matching the
/// pre-multi-stat layout). With multiple stats the value column is replaced
/// by one FixedSizeList per stat, named `value` (single) or `<stat>` (multi).
pub(crate) fn write_arrow_parquet(
    dest: &str,
    cells: Vec<u64>,
    flat_values: Vec<Vec<f64>>,
    n_bands: usize,
    band_names: &[String],
    stats: &[String],
    resolution: i32,
    value_type: ValueType,
    compression: CompressionChoice,
) -> Result<()> {
    let n_cells = cells.len();
    if flat_values.len() != stats.len() {
        return Err(A5CogError::Invalid(format!(
            "flat_values outer len {} != stats {}",
            flat_values.len(),
            stats.len()
        )));
    }
    for v in &flat_values {
        if v.len() != n_cells * n_bands {
            return Err(A5CogError::Invalid(format!(
                "flat_values inner len {} != cells {} * bands {}",
                v.len(),
                n_cells,
                n_bands
            )));
        }
    }

    let cell_arr = UInt64Array::from(cells);

    let value_dtype = match value_type {
        ValueType::Float64 => DataType::Float64,
        ValueType::Float32 => DataType::Float32,
    };
    let item_field = Arc::new(Field::new("item", value_dtype.clone(), true));
    let fsl_dtype = DataType::FixedSizeList(item_field.clone(), n_bands as i32);

    let mut columns: Vec<ArrayRef> = Vec::with_capacity(1 + stats.len());
    let mut fields: Vec<Field> = Vec::with_capacity(1 + stats.len());
    columns.push(Arc::new(cell_arr));
    fields.push(Field::new("cell", DataType::UInt64, false));

    for (s_i, s_name) in stats.iter().enumerate() {
        let inner: ArrayRef = match value_type {
            ValueType::Float64 => {
                let buf = Buffer::from_vec(flat_values[s_i].clone());
                Arc::new(Float64Array::new(buf.into(), None)) as ArrayRef
            }
            ValueType::Float32 => {
                let casted: Vec<f32> = flat_values[s_i].iter().map(|v| *v as f32).collect();
                let buf = Buffer::from_vec(casted);
                Arc::new(Float32Array::new(buf.into(), None)) as ArrayRef
            }
        };
        let fsl = FixedSizeListArray::new(
            item_field.clone(),
            n_bands as i32,
            inner,
            None,
        );
        let col_name = if stats.len() == 1 { "value".to_string() } else { s_name.clone() };
        columns.push(Arc::new(fsl));
        fields.push(Field::new(col_name, fsl_dtype.clone(), false));
    }

    let metadata = file_metadata(band_names, resolution, stats);
    let schema = Schema::new(fields).with_metadata(metadata.clone());

    let batch = RecordBatch::try_new(Arc::new(schema.clone()), columns)
        .map_err(|e| A5CogError::Parquet(format!("RecordBatch build: {e}")))?;

    let file = File::create(dest)?;
    let kv: Vec<KeyValue> = metadata
        .into_iter()
        .map(|(k, v)| KeyValue::new(k, v))
        .collect();
    let props = WriterProperties::builder()
        .set_compression(compression.to_parquet())
        .set_key_value_metadata(Some(kv))
        .build();
    let mut writer = ArrowWriter::try_new(file, Arc::new(schema), Some(props))
        .map_err(|e| A5CogError::Parquet(format!("ArrowWriter: {e}")))?;
    writer
        .write(&batch)
        .map_err(|e| A5CogError::Parquet(format!("write batch: {e}")))?;
    writer
        .close()
        .map_err(|e| A5CogError::Parquet(format!("writer close: {e}")))?;
    Ok(())
}

fn file_metadata(
    band_names: &[String],
    resolution: i32,
    stats: &[String],
) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("a5px_band_names".to_string(), band_names.join("\n"));
    m.insert("a5px_resolution".to_string(), resolution.to_string());
    m.insert("a5px_stats".to_string(), stats.join("\n"));
    // backwards-compat: when single stat, also write the legacy a5px_stat key
    if stats.len() == 1 {
        m.insert("a5px_stat".to_string(), stats[0].clone());
    }
    m
}
