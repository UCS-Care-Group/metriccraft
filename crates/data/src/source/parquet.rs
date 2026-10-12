//! Parquet Source (spec S-1): streamed one row group at a time, [`BATCH_ROWS`] rows per batch.
//! Codecs: uncompressed, snappy, gzip, lz4, brotli, zstd (all builds, web included).
//!
//! Column types are normalised to the plain Source set:
//! - integers of any width/sign → `Int64`; a `UInt64` value above `i64::MAX` is an error naming
//!   the column (no silent float rounding);
//! - floats → `Float64`; decimals → `Float64` (≈15–17 significant digits, so very wide decimals
//!   lose precision — the DAX Decimal type is handled later in the model);
//! - `Utf8`/`LargeUtf8`/`Utf8View` and string dictionaries → `Utf8`;
//! - `Date32`/`Date64` → `Date32` (Date64 time-of-day dropped);
//! - timestamps of any unit → `Timestamp(Microsecond, None)`; zoned timestamps keep their UTC
//!   instant (shown as UTC wall-clock time), nanoseconds are truncated;
//! - `Boolean` as is. Anything else (nested, binary, time-of-day…) is an error naming the column.

use std::sync::Arc;

use ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
pub use ::parquet::file::reader::ChunkReader;
use arrow_array::{Array, ArrayRef, RecordBatch, TimestampMicrosecondArray};
use arrow_cast::{CastOptions, cast_with_options};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
pub use bytes::Bytes;

use super::{BATCH_ROWS, load};
use crate::{Error, Result, Table};

/// Reads a Parquet file (a `std::fs::File`, or [`Bytes`] already in memory) into a Table named
/// `name`, using at most `limit` bytes.
pub fn read<R: ChunkReader + 'static>(name: &str, reader: R, limit: usize) -> Result<Table> {
    // The parquet/arrow decoders still assert on some corrupt pages (found by the bit-flip
    // test); turn those panics into an error. Native only: wasm aborts on panic, so the web
    // build relies on the checks below and upstream fixes.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        read_unguarded(name, reader, limit)
    }))
    .unwrap_or_else(|_| {
        Err(Error::Source {
            source: name.into(),
            message: "corrupt file: the Parquet decoder failed".into(),
        })
    })
}

fn read_unguarded<R: ChunkReader + 'static>(name: &str, reader: R, limit: usize) -> Result<Table> {
    let err = |message: String| Error::Source {
        source: name.into(),
        message,
    };
    let len = reader.len();
    let builder = ParquetRecordBatchReaderBuilder::try_new(reader)
        .map_err(|e| err(e.to_string()))?
        .with_batch_size(BATCH_ROWS);
    // The parquet crate asserts (panics) on negative chunk ranges; reject them first.
    for chunk in builder
        .metadata()
        .row_groups()
        .iter()
        .flat_map(|g| g.columns())
    {
        let start = chunk
            .dictionary_page_offset()
            .unwrap_or(chunk.data_page_offset());
        let size = chunk.compressed_size();
        let end = u64::try_from(start).ok().zip(u64::try_from(size).ok());
        if end
            .and_then(|(s, n)| s.checked_add(n))
            .is_none_or(|e| e > len)
        {
            return Err(err("corrupt file: column chunk outside the file".into()));
        }
    }
    let fields = builder
        .schema()
        .fields()
        .iter()
        .map(|f| {
            plain(f.data_type())
                .map(|t| Field::new(f.name(), t, true))
                .ok_or_else(|| {
                    err(format!(
                        "column `{}` has unsupported type {}",
                        f.name(),
                        f.data_type()
                    ))
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let schema = Arc::new(Schema::new(fields));
    let reader = builder.build().map_err(|e| err(e.to_string()))?;
    let batches = reader.map(|batch| {
        let batch = batch.map_err(|e| err(e.to_string()))?;
        let columns = batch
            .columns()
            .iter()
            .zip(schema.fields())
            .map(|(col, f)| {
                normalise(col, f.data_type())
                    .map_err(|e| err(format!("column `{}`: {e}", f.name())))
            })
            .collect::<Result<Vec<_>>>()?;
        RecordBatch::try_new(schema.clone(), columns).map_err(|e| err(e.to_string()))
    });
    load(name, schema.clone(), batches, limit)
}

/// The plain type a Parquet column becomes, or `None` if unsupported.
fn plain(t: &DataType) -> Option<DataType> {
    use DataType::*;
    Some(match t {
        Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 => Int64,
        Float16 | Float32 | Float64 => Float64,
        Decimal32(..) | Decimal64(..) | Decimal128(..) | Decimal256(..) => Float64,
        Utf8 | LargeUtf8 | Utf8View => Utf8,
        Dictionary(_, v) if plain(v) == Some(Utf8) => Utf8,
        Date32 | Date64 => Date32,
        Timestamp(..) => Timestamp(TimeUnit::Microsecond, None),
        Boolean => Boolean,
        _ => return None,
    })
}

fn normalise(col: &ArrayRef, to: &DataType) -> std::result::Result<ArrayRef, String> {
    let strict = CastOptions {
        safe: false,
        ..Default::default()
    };
    let cast = |t: &DataType| cast_with_options(col, t, &strict).map_err(|e| e.to_string());
    match col.data_type() {
        // Change the unit keeping the zone, then drop the zone: the values are UTC instants.
        DataType::Timestamp(_, tz @ Some(_)) => {
            let micros = cast(&DataType::Timestamp(TimeUnit::Microsecond, tz.clone()))?;
            let micros = micros
                .as_any()
                .downcast_ref::<TimestampMicrosecondArray>()
                .ok_or("timestamp cast failed")?;
            Ok(Arc::new(micros.clone().with_timezone_opt(None::<String>)))
        }
        _ => cast(to),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::parquet::arrow::ArrowWriter;
    use ::parquet::basic::{BrotliLevel, Compression, GzipLevel, ZstdLevel};
    use ::parquet::file::properties::WriterProperties;
    use arrow_array::{
        BooleanArray, Date32Array, Date64Array, Decimal128Array, Float32Array, Float64Array,
        Int8Array, Int64Array, LargeStringArray, ListArray, StringArray, TimestampNanosecondArray,
        TimestampSecondArray, UInt32Array, UInt64Array, types::Int32Type,
    };

    const BIG: usize = 1 << 30;
    fn codecs() -> [Compression; 7] {
        [
            Compression::UNCOMPRESSED,
            Compression::SNAPPY,
            Compression::GZIP(GzipLevel::default()),
            Compression::LZ4_RAW,
            Compression::LZ4,
            Compression::BROTLI(BrotliLevel::default()),
            Compression::ZSTD(ZstdLevel::default()),
        ]
    }

    fn write(batch: &RecordBatch, compression: Compression, row_group: usize) -> Bytes {
        let props = WriterProperties::builder()
            .set_compression(compression)
            .set_max_row_group_row_count(Some(row_group))
            .build();
        let mut out = Vec::new();
        let mut w = ArrowWriter::try_new(&mut out, batch.schema(), Some(props)).unwrap();
        w.write(batch).unwrap();
        w.close().unwrap();
        Bytes::from(out)
    }

    fn batch(cols: Vec<(&str, ArrayRef)>) -> RecordBatch {
        RecordBatch::try_from_iter(cols).unwrap()
    }

    fn sample() -> RecordBatch {
        batch(vec![
            (
                "region",
                Arc::new(StringArray::from(vec![Some("North"), None, Some("South")])) as ArrayRef,
            ),
            ("units", Arc::new(Int64Array::from(vec![1, 2, 3]))),
            ("price", Arc::new(Float64Array::from(vec![1.5, 2.5, 3.5]))),
        ])
    }

    #[test]
    fn every_codec_round_trips() {
        for c in codecs() {
            let t = read("Sales", write(&sample(), c, 1024), BIG).unwrap();
            assert_eq!(t.rows(), 3, "{c:?}");
            let b = t.batch();
            let units = b
                .column(1)
                .as_any()
                .downcast_ref::<arrow_array::Int8Array>();
            assert_eq!(units.unwrap().values(), &[1, 2, 3], "{c:?}");
            let price = b.column(2).as_any().downcast_ref::<Float64Array>().unwrap();
            assert_eq!(price.values(), &[1.5, 2.5, 3.5], "{c:?}");
            assert!(b.column(0).is_null(1), "{c:?}");
        }
    }

    #[test]
    fn many_row_groups_and_batches_stream() {
        let n = BATCH_ROWS * 3 + 17;
        let ids = Int64Array::from_iter_values(0..n as i64);
        let b = batch(vec![("id", Arc::new(ids) as ArrayRef)]);
        let t = read("Big", write(&b, Compression::SNAPPY, 5000), BIG).unwrap();
        assert_eq!(t.rows(), n);
        let ids = arrow_cast::cast(t.batch().column(0), &DataType::Int64).unwrap();
        let sum: i64 = ids
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .values()
            .iter()
            .sum();
        assert_eq!(sum, (n as i64 - 1) * n as i64 / 2);
    }

    #[test]
    fn types_normalise_with_nulls() {
        let b = batch(vec![
            (
                "i8",
                Arc::new(Int8Array::from(vec![Some(-1), None])) as ArrayRef,
            ),
            ("u32", Arc::new(UInt32Array::from(vec![Some(7), None]))),
            ("u64", Arc::new(UInt64Array::from(vec![Some(9), None]))),
            ("f32", Arc::new(Float32Array::from(vec![Some(0.5), None]))),
            (
                "dec",
                Arc::new(
                    Decimal128Array::from(vec![Some(12345), None])
                        .with_precision_and_scale(10, 2)
                        .unwrap(),
                ),
            ),
            (
                "large",
                Arc::new(LargeStringArray::from(vec![Some("a"), None])),
            ),
            (
                "dict",
                Arc::new(
                    vec![Some("x"), None]
                        .into_iter()
                        .collect::<arrow_array::DictionaryArray<Int32Type>>(),
                ),
            ),
            (
                "d64",
                Arc::new(Date64Array::from(vec![Some(86_400_000), None])),
            ),
            ("d32", Arc::new(Date32Array::from(vec![Some(3), None]))),
            ("flag", Arc::new(BooleanArray::from(vec![Some(true), None]))),
            (
                "ts_s",
                Arc::new(TimestampSecondArray::from(vec![Some(2), None])),
            ),
            (
                "ts_ns_tz",
                Arc::new(
                    TimestampNanosecondArray::from(vec![Some(3_000_001_000), None])
                        .with_timezone("+05:00"),
                ),
            ),
        ]);
        let t = read("Types", write(&b, Compression::SNAPPY, 1024), BIG).unwrap();
        let schema = t.batch().schema();
        let types: Vec<String> = schema
            .fields()
            .iter()
            .map(|f| format!("{} {}", f.name(), f.data_type()))
            .collect();
        let micros = DataType::Timestamp(TimeUnit::Microsecond, None).to_string();
        let float = DataType::Float64.to_string();
        let utf8 = "Dictionary(Int8, Utf8)";
        for (i, want) in [
            "Int8",
            "Int8",
            "Int8",
            float.as_str(),
            float.as_str(),
            utf8,
            utf8,
            "Date32",
            "Date32",
            "Boolean",
            micros.as_str(),
            micros.as_str(),
        ]
        .iter()
        .enumerate()
        {
            assert!(types[i].ends_with(want), "{} vs {want}", types[i]);
            assert!(t.batch().column(i).is_null(1), "{}", types[i]);
        }
        let col = |i: usize| t.batch().column(i).clone();
        let dec = col(4);
        let dec = dec.as_any().downcast_ref::<Float64Array>().unwrap();
        assert_eq!(dec.value(0), 123.45);
        let d64 = col(7);
        assert_eq!(
            d64.as_any().downcast_ref::<Date32Array>().unwrap().value(0),
            1
        );
        let ts = col(10);
        let ts = ts
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        assert_eq!(ts.value(0), 2_000_000);
        let ts = col(11);
        let ts = ts
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        assert_eq!(ts.value(0), 3_000_001, "UTC instant kept, ns truncated");
    }

    #[test]
    fn uint64_overflow_names_the_column() {
        let b = batch(vec![(
            "huge",
            Arc::new(UInt64Array::from(vec![u64::MAX])) as ArrayRef,
        )]);
        let e = read("T", write(&b, Compression::SNAPPY, 1024), BIG).unwrap_err();
        assert!(e.to_string().contains("column `huge`"), "{e}");
    }

    #[test]
    fn unsupported_type_names_the_column() {
        let list = ListArray::from_iter_primitive::<Int32Type, _, _>(vec![Some(vec![Some(1)])]);
        let b = batch(vec![("tags", Arc::new(list) as ArrayRef)]);
        let e = read("T", write(&b, Compression::SNAPPY, 1024), BIG).unwrap_err();
        assert!(matches!(e, Error::Source { .. }));
        assert!(e.to_string().contains("column `tags`"), "{e}");
    }

    #[test]
    fn corrupt_or_truncated_bytes_error() {
        let good = write(&sample(), Compression::SNAPPY, 1024);
        let bad = [
            Bytes::new(),
            Bytes::from_static(b"PAR1 not really parquet PAR1"),
            good.slice(..good.len() / 2),
            good.slice(good.len() / 2..),
        ];
        for b in bad {
            assert!(read("Bad", b, BIG).is_err());
        }
        // Damage every byte of every codec's file: errors or garbage are fine, panics are not.
        for c in codecs() {
            let good = write(&sample(), c, 1024);
            for i in 0..good.len() {
                for flip in [0x01, 0x80, 0xFF] {
                    let mut v = good.to_vec();
                    v[i] ^= flip;
                    let _ = read("Flipped", Bytes::from(v), BIG);
                }
            }
        }
    }

    #[test]
    fn memory_cap_names_the_table() {
        let e = read("Sales", write(&sample(), Compression::SNAPPY, 1024), 16).unwrap_err();
        assert!(
            matches!(e, Error::MemoryCap { ref table, .. } if table == "Sales"),
            "{e}"
        );
    }
}
