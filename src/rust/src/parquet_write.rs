//! Rust-direct Parquet output.
//!
//! Columns are built straight from the cell slab (as float32 when asked,
//! so the f64 flatten copy of the R paths is never made) and encoded in
//! parallel, one column per task on the read's index pool, then appended
//! to the file as row groups of up to `ROW_GROUP_ROWS` cells.

use std::collections::HashMap;
use std::fs::File;
use std::sync::Arc;

use arrow_array::{ArrayRef, FixedSizeListArray, Float32Array, Float64Array, UInt64Array};
use arrow_buffer::Buffer;
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::arrow_writer::{compute_leaves, ArrowColumnChunk, ArrowLeafColumn, ArrowRowGroupWriterFactory};
use parquet::arrow::{add_encoded_arrow_schema_to_metadata, ArrowSchemaConverter};
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;

use crate::error::{A5CogError, Result};
use crate::read::{AccLayout, Aggregate};

/// Matches `ArrowWriter`'s default `max_row_group_size`.
const ROW_GROUP_ROWS: usize = 1024 * 1024;

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

    fn data_type(self) -> DataType {
        match self {
            Self::Float64 => DataType::Float64,
            Self::Float32 => DataType::Float32,
        }
    }

    /// Array of `n` values produced by `f(i)`, filled in parallel chunks
    /// of `per_row * 4096` elements on `pool` when there is one.
    fn array_par(
        self,
        pool: Option<&rayon::ThreadPool>,
        n: usize,
        per_row: usize,
        f: impl Fn(usize) -> f64 + Sync,
    ) -> ArrayRef {
        let chunk = (per_row * 4096).max(1);
        macro_rules! fill {
            ($t:ty, $arr:ident) => {{
                let mut v: Vec<$t> = vec![0 as $t; n];
                match pool {
                    Some(p) if n > chunk => p.install(|| {
                        use rayon::prelude::*;
                        v.par_chunks_mut(chunk).enumerate().for_each(|(k, out)| {
                            for (j, o) in out.iter_mut().enumerate() {
                                *o = f(k * chunk + j) as $t;
                            }
                        })
                    }),
                    _ => {
                        for (j, o) in v.iter_mut().enumerate() {
                            *o = f(j) as $t;
                        }
                    }
                }
                Arc::new($arr::new(Buffer::from_vec(v).into(), None)) as ArrayRef
            }};
        }
        match self {
            Self::Float64 => fill!(f64, Float64Array),
            Self::Float32 => fill!(f32, Float32Array),
        }
    }

    /// Array of `n` values produced by `f(i)`.
    fn array(self, n: usize, f: impl Fn(usize) -> f64) -> ArrayRef {
        match self {
            Self::Float64 => {
                let v: Vec<f64> = (0..n).map(f).collect();
                Arc::new(Float64Array::new(Buffer::from_vec(v).into(), None))
            }
            Self::Float32 => {
                let v: Vec<f32> = (0..n).map(|i| f(i) as f32).collect();
                Arc::new(Float32Array::new(Buffer::from_vec(v).into(), None))
            }
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

fn pq_err(what: &str, e: impl std::fmt::Display) -> A5CogError {
    A5CogError::Parquet(format!("{what}: {e}"))
}

/// Run `f` over `0..n` on the pool when there is one, else in order.
fn par_map<T: Send>(pool: Option<&rayon::ThreadPool>, n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    match pool {
        Some(p) if n > 1 => p.install(|| {
            use rayon::prelude::*;
            (0..n).into_par_iter().map(&f).collect()
        }),
        _ => (0..n).map(f).collect(),
    }
}

/// Output schema fields plus the column names, for `stats` x `band_names`.
fn fields_for(
    band_names: &[String],
    stats: &[String],
    value_type: ValueType,
    n_bands: usize,
    as_vector: bool,
) -> Vec<Field> {
    let mut fields = vec![Field::new("cell", DataType::UInt64, false)];
    if as_vector {
        let item_field = Arc::new(Field::new("item", value_type.data_type(), true));
        let fsl = DataType::FixedSizeList(item_field, n_bands as i32);
        for s_name in stats {
            let col_name = if stats.len() == 1 { "value".to_string() } else { format!("value_{s_name}") };
            fields.push(Field::new(col_name, fsl.clone(), false));
        }
    } else {
        for s_name in stats {
            for b_name in band_names {
                let col_name = if stats.len() == 1 { b_name.clone() } else { format!("{b_name}_{s_name}") };
                fields.push(Field::new(col_name, value_type.data_type(), false));
            }
        }
    }
    fields
}

/// Wrap a cell-major flat array as a FixedSizeList column.
fn fsl_column(value_type: ValueType, n_bands: usize, inner: ArrayRef) -> ArrayRef {
    let item_field = Arc::new(Field::new("item", value_type.data_type(), true));
    Arc::new(FixedSizeListArray::new(item_field, n_bands as i32, inner, None))
}

/// Write a finished read straight from its cell slab.
pub(crate) fn write_aggregate_parquet<L: AccLayout>(
    agg: &Aggregate<L>,
    dest: &str,
    resolution: i32,
    value_type: ValueType,
    compression: CompressionChoice,
    as_vector: bool,
) -> Result<()> {
    let n = agg.len();
    let n_bands = agg.n_out;
    let stats: Vec<String> = agg.stats.iter().map(|s| s.as_str().to_string()).collect();
    let pool = agg.pool.as_deref();

    let mut columns: Vec<ArrayRef> = vec![Arc::new(UInt64Array::from(agg.cells().to_vec()))];
    if as_vector {
        // one FixedSizeList per stat; the inner array is cell-major, filled
        // in cell chunks
        for &stat in &agg.stats {
            let inner = value_type.array_par(pool, n * n_bands, n_bands, |j| {
                agg.value(stat, j / n_bands, j % n_bands)
            });
            columns.push(fsl_column(value_type, n_bands, inner));
        }
    } else {
        let cols: Vec<ArrayRef> = par_map(pool, agg.stats.len() * n_bands, |k| {
            let stat = agg.stats[k / n_bands];
            let b = k % n_bands;
            value_type.array(n, |i| agg.value(stat, i, b))
        });
        columns.extend(cols);
    }
    let fields = fields_for(&agg.band_names, &stats, value_type, n_bands, as_vector);
    let metadata = file_metadata(&agg.band_names, resolution, &stats, as_vector);
    write_columns_parquet(dest, fields, columns, metadata, compression, pool)
}

/// Legacy entry from flat per-stat values (the centroid sampler).
#[allow(clippy::too_many_arguments)]
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
    as_vector: bool,
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
    let mut columns: Vec<ArrayRef> = vec![Arc::new(UInt64Array::from(cells))];
    if as_vector {
        for flat in &flat_values {
            let inner = value_type.array(flat.len(), |j| flat[j]);
            columns.push(fsl_column(value_type, n_bands, inner));
        }
    } else {
        for flat in &flat_values {
            for b in 0..n_bands {
                columns.push(value_type.array(n_cells, |i| flat[i * n_bands + b]));
            }
        }
    }
    let fields = fields_for(band_names, stats, value_type, n_bands, as_vector);
    let metadata = file_metadata(band_names, resolution, stats, as_vector);
    write_columns_parquet(dest, fields, columns, metadata, compression, None)
}

/// Encode `columns` to `dest`, one column per task on `pool`, in row
/// groups of `ROW_GROUP_ROWS`. Equivalent to `ArrowWriter` on one
/// RecordBatch (same schema metadata, same row-group size), minus its
/// serial encoding.
fn write_columns_parquet(
    dest: &str,
    fields: Vec<Field>,
    columns: Vec<ArrayRef>,
    metadata: HashMap<String, String>,
    compression: CompressionChoice,
    pool: Option<&rayon::ThreadPool>,
) -> Result<()> {
    let schema = Arc::new(Schema::new(fields).with_metadata(metadata.clone()));
    let n_rows = columns.first().map(|c| c.len()).unwrap_or(0);
    let kv: Vec<KeyValue> = metadata.into_iter().map(|(k, v)| KeyValue::new(k, v)).collect();
    let mut props = WriterProperties::builder()
        .set_compression(compression.to_parquet())
        .set_key_value_metadata(Some(kv))
        .build();
    add_encoded_arrow_schema_to_metadata(&schema, &mut props);
    let parquet_schema = ArrowSchemaConverter::new()
        .with_coerce_types(props.coerce_types())
        .convert(&schema)
        .map_err(|e| pq_err("schema", e))?;
    let props = Arc::new(props);
    let file = File::create(dest)?;
    let mut writer = SerializedFileWriter::new(file, parquet_schema.root_schema_ptr(), props)
        .map_err(|e| pq_err("open writer", e))?;
    let factory = ArrowRowGroupWriterFactory::new(&writer, Arc::clone(&schema));

    let mut start = 0usize;
    let mut rg = 0usize;
    // always at least one row group so an empty read still carries the schema
    loop {
        let len = ROW_GROUP_ROWS.min(n_rows - start);
        let col_writers = factory.create_column_writers(rg).map_err(|e| pq_err("column writers", e))?;
        let mut leaves: Vec<ArrowLeafColumn> = Vec::with_capacity(col_writers.len());
        for (arr, field) in columns.iter().zip(schema.fields()) {
            let sliced = arr.slice(start, len);
            leaves.extend(compute_leaves(field, &sliced).map_err(|e| pq_err("leaves", e))?);
        }
        if leaves.len() != col_writers.len() {
            return Err(A5CogError::Parquet(format!(
                "leaf count {} != column writers {}",
                leaves.len(),
                col_writers.len()
            )));
        }
        let jobs: Vec<(parquet::arrow::arrow_writer::ArrowColumnWriter, ArrowLeafColumn)> =
            col_writers.into_iter().zip(leaves).collect();
        let encode = |(mut w, leaf): (parquet::arrow::arrow_writer::ArrowColumnWriter, ArrowLeafColumn)| -> Result<ArrowColumnChunk> {
            w.write(&leaf).map_err(|e| pq_err("encode column", e))?;
            w.close().map_err(|e| pq_err("close column", e))
        };
        let chunks: Vec<ArrowColumnChunk> = match pool {
            Some(p) if jobs.len() > 1 => p.install(|| {
                use rayon::prelude::*;
                jobs.into_par_iter().map(encode).collect::<Result<Vec<_>>>()
            })?,
            _ => jobs.into_iter().map(encode).collect::<Result<Vec<_>>>()?,
        };
        let mut rg_writer = writer.next_row_group().map_err(|e| pq_err("row group", e))?;
        for c in chunks {
            c.append_to_row_group(&mut rg_writer).map_err(|e| pq_err("append column", e))?;
        }
        rg_writer.close().map_err(|e| pq_err("close row group", e))?;
        start += len;
        rg += 1;
        if start >= n_rows {
            break;
        }
    }
    writer.close().map_err(|e| pq_err("close file", e))?;
    Ok(())
}

fn file_metadata(
    band_names: &[String],
    resolution: i32,
    stats: &[String],
    as_vector: bool,
) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("a5px_band_names".to_string(), band_names.join("\n"));
    m.insert("a5px_resolution".to_string(), resolution.to_string());
    m.insert("a5px_stats".to_string(), stats.join("\n"));
    m.insert(
        "a5px_layout".to_string(),
        if as_vector { "fsl".into() } else { "wide".into() },
    );
    if stats.len() == 1 {
        m.insert("a5px_stat".to_string(), stats[0].clone());
    }
    m
}
